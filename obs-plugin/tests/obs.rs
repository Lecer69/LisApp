//! Loads the built plugin the way OBS does, through libobs with no window and no OBS settings. The
//! two calls it makes with the video are wrapped here, to record them before passing them on.

#[path = "common.rs"]
mod common;

use common::{make_video, FPS, HEIGHT, WIDTH};
use lis_camera::sys::*;
use lis_camera::{SOURCE_ID, SOURCE_NAME};
use lis_video::h264::split_access_units;
use lis_video::{encode, Packet, VideoFrame, VideoInfo, CODEC_H264};
use std::ffi::{c_char, c_long, c_void, CStr, CString};
use std::io::Write;
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::ptr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq)]
enum Call {
    Frame { width: u32, height: u32, format: i32, timestamp: u64, partial_range: bool, has_matrix: bool, luma_plane: bool, chroma_planes: bool },
    Clear,
    Rotation(i64),
}

static CALLS: Mutex<Vec<Call>> = Mutex::new(Vec::new());

extern "C" {
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

const RTLD_NEXT: *mut c_void = -1isize as *mut c_void;

#[no_mangle]
pub unsafe extern "C" fn obs_source_output_video2(source: *mut obs_source_t, frame: *const obs_source_frame2) {
    let call = if frame.is_null() {
        Call::Clear
    } else {
        let out = &*frame;
        Call::Frame {
            width: out.width,
            height: out.height,
            format: out.format as i32,
            timestamp: out.timestamp,
            partial_range: out.range == video_range_type_VIDEO_RANGE_PARTIAL,
            has_matrix: out.color_matrix.iter().any(|value| *value != 0.0),
            luma_plane: !out.data[0].is_null() && out.linesize[0] >= out.width,
            chroma_planes: !out.data[1].is_null() && !out.data[2].is_null() && out.linesize[1] >= out.width / 2,
        }
    };
    CALLS.lock().unwrap().push(call);

    let real = dlsym(RTLD_NEXT, c"obs_source_output_video2".as_ptr());
    assert!(!real.is_null(), "the real libobs function");
    let real: unsafe extern "C" fn(*mut obs_source_t, *const obs_source_frame2) = std::mem::transmute(real);
    real(source, frame);
}

#[no_mangle]
pub unsafe extern "C" fn obs_source_set_async_rotation(source: *mut obs_source_t, rotation: c_long) {
    CALLS.lock().unwrap().push(Call::Rotation(rotation as i64));
    let real = dlsym(RTLD_NEXT, c"obs_source_set_async_rotation".as_ptr());
    assert!(!real.is_null(), "the real libobs function");
    let real: unsafe extern "C" fn(*mut obs_source_t, c_long) = std::mem::transmute(real);
    real(source, rotation);
}

fn plugin_path() -> PathBuf {
    let tests = std::env::current_exe().unwrap();
    tests.parent().unwrap().parent().unwrap().join("liblis_camera.so")
}

fn wait_for(what: &str, limit: Duration, wanted: impl Fn(&[Call]) -> bool) {
    let start = Instant::now();
    loop {
        if wanted(&CALLS.lock().unwrap()) {
            return;
        }
        assert!(start.elapsed() < limit, "never saw {what}: {} calls", CALLS.lock().unwrap().len());
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn frames(calls: &[Call]) -> usize {
    calls.iter().filter(|call| matches!(call, Call::Frame { .. })).count()
}

#[test]
fn the_plugin_loads_and_a_source_shows_the_stream() {
    let dir = tempfile::tempdir().unwrap();
    std::env::set_var("XDG_RUNTIME_DIR", dir.path());
    let socket = dir.path().join("lis-camera.sock");
    let video = dir.path().join("test.h264");
    make_video(&video, 3);
    let data = std::fs::read(&video).unwrap();
    let units: Vec<(Vec<u8>, bool)> = split_access_units(&data).iter().map(|unit| (unit.data.to_vec(), unit.keyframe)).collect();
    assert_eq!(units.len() as u32, 3 * FPS);

    let path = plugin_path();
    assert!(path.exists(), "{path:?}");
    let path = CString::new(path.to_str().unwrap()).unwrap();

    unsafe {
        assert!(obs_startup(c"en-US".as_ptr(), ptr::null(), ptr::null_mut()), "libobs starts");

        let mut module = ptr::null_mut();
        let opened = obs_open_module(&mut module, path.as_ptr(), c"/nonexistent".as_ptr());
        assert_eq!(opened, MODULE_SUCCESS as i32, "OBS accepts the plugin");
        assert!(obs_init_module(module), "the plugin loads");

        let name = obs_source_get_display_name(SOURCE_ID.as_ptr());
        assert!(!name.is_null(), "the source is registered");
        assert_eq!(CStr::from_ptr(name), SOURCE_NAME);
        assert_ne!(obs_get_source_output_flags(SOURCE_ID.as_ptr()) & OBS_SOURCE_ASYNC_VIDEO, 0);

        // The source goes first: it has to keep retrying until the daemon is up
        let source = obs_source_create(SOURCE_ID.as_ptr(), c"test".as_ptr(), ptr::null_mut(), ptr::null_mut());
        assert!(!source.is_null(), "the source is created");
        std::thread::sleep(Duration::from_millis(300));
        assert!(CALLS.lock().unwrap().is_empty(), "nothing yet");

        let listener = UnixListener::bind(&socket).unwrap();
        let info = VideoInfo { codec: CODEC_H264.into(), width: WIDTH, height: HEIGHT, fps: FPS, bitrate: 16_000_000, rotation: 90, camera: "test".into() };
        let (stopped_tx, stopped_rx) = std::sync::mpsc::channel::<()>();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.write_all(&encode(&Packet::Info(info)).unwrap()).unwrap();
            for (index, (unit, keyframe)) in units.iter().enumerate() {
                let frame = VideoFrame { pts_us: index as i64 * 1_000_000 / FPS as i64, keyframe: *keyframe, data: unit.clone() };
                stream.write_all(&encode(&Packet::Frame(frame)).unwrap()).unwrap();
                std::thread::sleep(Duration::from_secs(1) / FPS);
            }
            stream.write_all(&encode(&Packet::End).unwrap()).unwrap();
            // Held open, so only the end packet can clear the picture
            let _ = stopped_rx.recv_timeout(Duration::from_secs(10));
        });

        // 90 clockwise in the stream is 270 to OBS. Hidden for the first half second, so nothing should be decoded
        wait_for("the rotation", Duration::from_secs(5), |calls| calls.contains(&Call::Rotation(270)));
        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(frames(&CALLS.lock().unwrap()), 0, "a hidden source doesn't decode");

        obs_source_inc_showing(source);
        wait_for("the picture clearing", Duration::from_secs(10), |calls| calls.last() == Some(&Call::Clear));

        let calls = CALLS.lock().unwrap().clone();
        let shown: Vec<&Call> = calls.iter().filter(|call| matches!(call, Call::Frame { .. })).collect();
        println!("{} frames reached libobs", shown.len());

        // Shown part way through a second, so it starts at the next keyframe and misses nothing after
        let Call::Frame { timestamp: first, .. } = shown[0] else { unreachable!() };
        assert_eq!(*first, 1_000_000_000, "starts at the 1 second keyframe");
        assert_eq!(shown.len(), 2 * FPS as usize, "every frame from there on");
        for call in &shown {
            let Call::Frame { width, height, format, partial_range, has_matrix, luma_plane, chroma_planes, .. } = call else { unreachable!() };
            assert_eq!((*width, *height), (WIDTH, HEIGHT));
            assert_eq!(*format, video_format_VIDEO_FORMAT_I420 as i32);
            assert!(*partial_range, "the test video is limited range");
            assert!(*has_matrix && *luma_plane && *chroma_planes);
        }
        let timestamps: Vec<u64> = shown.iter().map(|call| if let Call::Frame { timestamp, .. } = call { *timestamp } else { 0 }).collect();
        assert!(timestamps.windows(2).all(|pair| pair[0] < pair[1]), "in order");

        // Closing it mid connection must not hang OBS
        let begun = Instant::now();
        obs_source_dec_showing(source);
        obs_source_release(source);
        std::thread::sleep(Duration::from_millis(100));
        assert!(begun.elapsed() < Duration::from_secs(3), "the source closes promptly");
        let _ = stopped_tx.send(());
        server.join().unwrap();
    }
}
