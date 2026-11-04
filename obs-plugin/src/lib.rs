//! The "Lis Camera" source for OBS: shows the phone's camera, which lis-daemon passes along over a
//! local socket. It keeps trying to connect, so OBS and the camera can start in any order.

pub mod decoder;
#[doc(hidden)]
pub mod sys;

use decoder::Decoder;
use lis_video::{read_packet, Packet, CODEC_H264};
use std::ffi::{c_char, c_void, CStr};
use std::io::BufReader;
use std::mem;
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;
use sys::*;

pub const SOURCE_ID: &CStr = c"lis_camera";
pub const SOURCE_NAME: &CStr = c"Lis Camera";

const RETRY_EVERY: Duration = Duration::from_secs(1);
const READ_BUFFER: usize = 256 * 1024;

fn log(level: u32, message: &str) {
    if let Ok(message) = std::ffi::CString::new(format!("[lis-camera] {message}")) {
        unsafe { blog(level as i32, c"%s".as_ptr(), message.as_ptr()) };
    }
}

fn socket_path() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR").map(|dir| PathBuf::from(dir).join("lis-camera.sock"))
}

struct SourcePtr(*mut obs_source_t);

// libobs allows these calls from any thread, and the thread stops before the source is destroyed
unsafe impl Send for SourcePtr {}
unsafe impl Sync for SourcePtr {}

struct Shared {
    source: SourcePtr,
    stopping: AtomicBool,
    connection: Mutex<Option<UnixStream>>,
    wake: Condvar,
    idle: Mutex<()>,
}

struct LisCamera {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

fn output_frame(source: *mut obs_source_t, frame: &AVFrame) {
    let format = frame.format;
    if format != AVPixelFormat_AV_PIX_FMT_YUV420P && format != AVPixelFormat_AV_PIX_FMT_YUVJ420P {
        return;
    }
    let full_range = frame.color_range == AVColorRange_AVCOL_RANGE_JPEG || format == AVPixelFormat_AV_PIX_FMT_YUVJ420P;
    let range = if full_range { video_range_type_VIDEO_RANGE_FULL } else { video_range_type_VIDEO_RANGE_PARTIAL };
    let is_601 = frame.colorspace == AVColorSpace_AVCOL_SPC_SMPTE170M || frame.colorspace == AVColorSpace_AVCOL_SPC_BT470BG;
    let colorspace = if is_601 { video_colorspace_VIDEO_CS_601 } else { video_colorspace_VIDEO_CS_709 };

    unsafe {
        let mut out: obs_source_frame2 = mem::zeroed();
        for plane in 0..3 {
            out.data[plane] = frame.data[plane];
            out.linesize[plane] = frame.linesize[plane] as u32;
        }
        out.width = frame.width as u32;
        out.height = frame.height as u32;
        out.timestamp = (frame.pts.max(0) as u64).saturating_mul(1000);
        out.format = video_format_VIDEO_FORMAT_I420;
        out.range = range;
        video_format_get_parameters_for_format(
            colorspace,
            range,
            out.format,
            out.color_matrix.as_mut_ptr(),
            out.color_range_min.as_mut_ptr(),
            out.color_range_max.as_mut_ptr(),
        );
        obs_source_output_video2(source, &out);
    }
}

fn clear_frame(source: *mut obs_source_t) {
    unsafe { obs_source_output_video2(source, ptr::null()) };
}

/// One connection to the daemon, until it ends.
fn watch(shared: &Shared, stream: UnixStream) {
    let source = shared.source.0;
    let mut reader = BufReader::with_capacity(READ_BUFFER, stream);
    let mut decoder = Decoder::new();
    let mut need_keyframe = true;
    let mut warned = false;

    if decoder.is_none() {
        log(LOG_WARNING, "no H.264 decoder in FFmpeg");
    }

    while let Ok(packet) = read_packet(&mut reader) {
        match packet {
            Packet::Info(info) => {
                if info.codec != CODEC_H264 {
                    log(LOG_WARNING, &format!("unknown codec {}", info.codec));
                }
                unsafe { obs_source_set_async_rotation(source, info.rotation.into()) };
            }
            Packet::Frame(frame) => {
                // Nothing to show it on, so it saves decoding
                if !unsafe { obs_source_showing(source) } {
                    need_keyframe = true;
                    continue;
                }
                if need_keyframe && !frame.keyframe {
                    continue;
                }
                let Some(active) = decoder.as_mut() else { continue };
                need_keyframe = false;
                if let Err(error) = active.decode(&frame.data, frame.pts_us, |decoded| output_frame(source, decoded)) {
                    need_keyframe = true;
                    if !warned {
                        warned = true;
                        log(LOG_WARNING, &format!("decoding failed ({error}), waiting for a keyframe"));
                    }
                }
            }
            Packet::End => {
                clear_frame(source);
                need_keyframe = true;
                decoder = Decoder::new();
            }
            Packet::Viewers(_) => {}
        }
    }
    clear_frame(source);
}

fn run(shared: Arc<Shared>) {
    let Some(path) = socket_path() else {
        log(LOG_WARNING, "XDG_RUNTIME_DIR isn't set, so the camera can't be found");
        return;
    };

    while !shared.stopping.load(Ordering::SeqCst) {
        if let Ok(stream) = UnixStream::connect(&path) {
            if let Ok(clone) = stream.try_clone() {
                *shared.connection.lock().unwrap() = Some(clone);
                if !shared.stopping.load(Ordering::SeqCst) {
                    watch(&shared, stream);
                }
                *shared.connection.lock().unwrap() = None;
            }
        }
        let idle = shared.idle.lock().unwrap();
        if !shared.stopping.load(Ordering::SeqCst) {
            let _ = shared.wake.wait_timeout(idle, RETRY_EVERY);
        }
    }
}

unsafe extern "C" fn get_name(_type_data: *mut c_void) -> *const c_char {
    SOURCE_NAME.as_ptr()
}

unsafe extern "C" fn create(_settings: *mut obs_data_t, source: *mut obs_source_t) -> *mut c_void {
    // The newest frame at once, as this is live video
    obs_source_set_async_unbuffered(source, true);

    let shared = Arc::new(Shared {
        source: SourcePtr(source),
        stopping: AtomicBool::new(false),
        connection: Mutex::new(None),
        wake: Condvar::new(),
        idle: Mutex::new(()),
    });
    let thread_shared = shared.clone();
    let thread = std::thread::Builder::new().name("lis-camera".to_string()).spawn(move || run(thread_shared));
    match thread {
        Ok(thread) => Box::into_raw(Box::new(LisCamera { shared, thread: Some(thread) })).cast(),
        Err(_) => ptr::null_mut(),
    }
}

unsafe extern "C" fn destroy(data: *mut c_void) {
    if data.is_null() {
        return;
    }
    let mut camera = Box::from_raw(data.cast::<LisCamera>());
    camera.shared.stopping.store(true, Ordering::SeqCst);
    if let Some(connection) = camera.shared.connection.lock().unwrap().as_ref() {
        let _ = connection.shutdown(Shutdown::Both);
    }
    {
        let _idle = camera.shared.idle.lock().unwrap();
        camera.shared.wake.notify_all();
    }
    if let Some(thread) = camera.thread.take() {
        let _ = thread.join();
    }
}

unsafe extern "C" fn get_properties(_data: *mut c_void) -> *mut obs_properties_t {
    let properties = obs_properties_create();
    obs_properties_add_text(
        properties,
        c"info".as_ptr(),
        c"Open Lis App on your phone and press Start in its Camera screen.".as_ptr(),
        obs_text_type_OBS_TEXT_INFO,
    );
    properties
}

static MODULE: AtomicPtr<obs_module_t> = AtomicPtr::new(ptr::null_mut());

/// # Safety
/// Called by OBS when it loads the plugin.
#[no_mangle]
pub unsafe extern "C" fn obs_module_set_pointer(module: *mut obs_module_t) {
    MODULE.store(module, Ordering::SeqCst);
}

#[no_mangle]
pub extern "C" fn obs_current_module() -> *mut obs_module_t {
    MODULE.load(Ordering::SeqCst)
}

#[no_mangle]
pub extern "C" fn obs_module_ver() -> u32 {
    (LIBOBS_API_MAJOR_VER << 24) | (LIBOBS_API_MINOR_VER << 16) | LIBOBS_API_PATCH_VER
}

#[no_mangle]
pub extern "C" fn obs_module_name() -> *const c_char {
    SOURCE_NAME.as_ptr()
}

#[no_mangle]
pub extern "C" fn obs_module_description() -> *const c_char {
    c"Shows your phone's camera from Lis App".as_ptr()
}

#[no_mangle]
pub extern "C" fn obs_module_load() -> bool {
    unsafe {
        let mut info: obs_source_info = mem::zeroed();
        info.id = SOURCE_ID.as_ptr();
        info.type_ = obs_source_type_OBS_SOURCE_TYPE_INPUT;
        info.output_flags = OBS_SOURCE_ASYNC_VIDEO | OBS_SOURCE_DO_NOT_DUPLICATE;
        info.get_name = Some(get_name);
        info.create = Some(create);
        info.destroy = Some(destroy);
        info.get_properties = Some(get_properties);
        info.icon_type = obs_icon_type_OBS_ICON_TYPE_CAMERA;
        obs_register_source_s(&info, mem::size_of::<obs_source_info>());
    }
    true
}
