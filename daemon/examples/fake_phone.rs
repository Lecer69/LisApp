//! A stand-in for the Android app, for working on the PC side without a phone. It pairs as soon as
//! the PC lets it, accepts the code by itself, then sends figures. With a video file it also acts as
//! the camera, looping the file, which makes a test source for the OBS plugin:
//!
//! ffmpeg -f lavfi -i testsrc2=s=1920x1080:r=60 -t 10 -c:v libx264 -bf 0 -g 60 -x264-params aud=1 -f h264 test.h264
//! cargo run -p lis-daemon --example fake_phone -- [host] [port] [test.h264]

use lis_core::video::h264::split_access_units;
use lis_ffi::{ClientListener, ConnectionState, LisClient, PcStats, PhoneStats, VideoConfig, VideoState};
use std::sync::{Arc, Once, OnceLock};
use std::time::{Duration, Instant};

const CAMERA_FPS: u32 = 60;

static CLIENT: OnceLock<Arc<LisClient>> = OnceLock::new();
static CAMERA_FILE: OnceLock<String> = OnceLock::new();
static CAMERA: Once = Once::new();

fn client() -> &'static LisClient {
    CLIENT.get().expect("client is set before connecting")
}

struct Listener;

impl ClientListener for Listener {
    fn on_state(&self, state: ConnectionState, pc_name: Option<String>) {
        println!("state: {state:?} {}", pc_name.unwrap_or_default());
        if state == ConnectionState::Unpaired {
            client().start_pairing();
        }
        if state == ConnectionState::Connected {
            client().send_stats(PhoneStats {
                battery_percent: 87,
                charging: false,
                storage_used: 120 * 1024 * 1024 * 1024,
                storage_total: 256 * 1024 * 1024 * 1024,
                memory_used: 5 * 1024 * 1024 * 1024,
                memory_total: 8 * 1024 * 1024 * 1024,
                network: "Wi-Fi".to_string(),
            });
            if let Some(file) = CAMERA_FILE.get() {
                CAMERA.call_once(|| {
                    let file = file.clone();
                    std::thread::spawn(move || run_camera(&file));
                });
            }
        }
    }
    fn on_pair_code(&self, code: String) {
        println!("code: {code}");
        client().confirm_pairing(true);
    }
    fn on_pair_result(&self, ok: bool, reason: String) {
        println!("paired: {ok} {reason}");
        if !ok {
            std::thread::spawn(|| {
                std::thread::sleep(Duration::from_secs(2));
                client().start_pairing();
            });
        }
    }
    fn on_pc_stats(&self, stats: PcStats) {
        println!("pc: {} cpu {:.0}%", stats.hostname, stats.cpu_percent);
    }
    fn open_incoming_file(&self, _id: u64, name: String, size: u64) -> i32 {
        println!("refusing {name} ({size} bytes)");
        -1
    }
    fn on_file_progress(&self, _id: u64, _name: String, _incoming: bool, _done: u64, _total: u64) {}
    fn on_file_finished(&self, _id: u64, name: String, _incoming: bool, ok: bool) {
        println!("file {name}: {ok}");
    }
    fn on_video_state(&self, state: VideoState) {
        println!("camera: {state:?}");
    }
    fn on_video_viewers(&self, count: u32) {
        println!("camera viewers: {count}");
    }
}

fn run_camera(file: &str) {
    let data = std::fs::read(file).expect("the video file");
    let units = split_access_units(&data);
    assert!(!units.is_empty(), "no frames found; the file needs -x264-params aud=1");

    client()
        .start_video(VideoConfig {
            width: 1920,
            height: 1080,
            fps: CAMERA_FPS,
            bitrate: 16_000_000,
            rotation: 0,
            camera: "fake".to_string(),
        })
        .expect("start the camera");

    let start = Instant::now();
    let frame_time = Duration::from_secs(1) / CAMERA_FPS;
    for (index, unit) in units.iter().cycle().enumerate() {
        let due = frame_time * index as u32;
        std::thread::sleep(due.saturating_sub(start.elapsed()));
        client().send_video_frame(unit.data.to_vec(), due.as_micros() as i64, unit.keyframe);
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let host = args.first().cloned().unwrap_or_else(|| "127.0.0.1".to_string());
    let port = args.get(1).and_then(|port| port.parse().ok()).unwrap_or(lis_core::protocol::DEFAULT_PORT);

    if let Some(file) = args.get(2) {
        let _ = CAMERA_FILE.set(file.clone());
    }

    // The same folder each time, so it stays one paired phone
    let dir = std::env::temp_dir().join("lis-fake-phone");
    let client = LisClient::new(dir.display().to_string(), "Fake Phone".to_string(), Box::new(Listener)).expect("client");
    let _ = CLIENT.set(client);
    self::client().connect(host, port);
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}
