//! The API the Android app calls, against the real daemon. Only Android's own parts are missing.

mod common;

use common::Pc;
use lis_core::identity::Identity;
use lis_core::tls;
use lis_core::video::{read_packet, write_packet_async, Packet, VideoFrame, VideoInfo, CODEC_H264};
use lis_ffi::{ClientListener, ConnectionState, LisClient, PcStats, PhoneStats, VideoConfig, VideoState};
use serde_json::json;
use std::os::fd::IntoRawFd;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::runtime::Runtime;

#[derive(Debug, Clone, PartialEq)]
enum Heard {
    State(ConnectionState, Option<String>),
    Code(String),
    PairResult(bool, String),
    Stats(PcStats),
    Finished(String, bool, bool),
    Video(VideoState),
    Viewers(u32),
}

#[derive(Clone)]
struct Listener {
    heard: Arc<Mutex<Vec<Heard>>>,
    dir: PathBuf,
}

impl ClientListener for Listener {
    fn on_state(&self, state: ConnectionState, pc_name: Option<String>) {
        self.heard.lock().unwrap().push(Heard::State(state, pc_name));
    }
    fn on_pair_code(&self, code: String) {
        self.heard.lock().unwrap().push(Heard::Code(code));
    }
    fn on_pair_result(&self, ok: bool, reason: String) {
        self.heard.lock().unwrap().push(Heard::PairResult(ok, reason));
    }
    fn on_pc_stats(&self, stats: PcStats) {
        self.heard.lock().unwrap().push(Heard::Stats(stats));
    }
    fn open_incoming_file(&self, _id: u64, name: String, _size: u64) -> i32 {
        std::fs::File::create(self.dir.join(name)).unwrap().into_raw_fd()
    }
    fn on_file_progress(&self, _id: u64, _name: String, _incoming: bool, _done: u64, _total: u64) {}
    fn on_file_finished(&self, _id: u64, name: String, incoming: bool, ok: bool) {
        self.heard.lock().unwrap().push(Heard::Finished(name, incoming, ok));
    }
    fn on_video_state(&self, state: VideoState) {
        self.heard.lock().unwrap().push(Heard::Video(state));
    }
    fn on_video_viewers(&self, count: u32) {
        self.heard.lock().unwrap().push(Heard::Viewers(count));
    }
}

struct App {
    client: Arc<LisClient>,
    heard: Arc<Mutex<Vec<Heard>>>,
    data: tempfile::TempDir,
    received: tempfile::TempDir,
}

impl App {
    fn new() -> Self {
        let data = tempfile::tempdir().unwrap();
        let received = tempfile::tempdir().unwrap();
        let (client, heard) = Self::open(&data, &received);
        Self { client, heard, data, received }
    }

    fn open(data: &tempfile::TempDir, received: &tempfile::TempDir) -> (Arc<LisClient>, Arc<Mutex<Vec<Heard>>>) {
        let heard = Arc::new(Mutex::new(Vec::new()));
        let listener = Listener { heard: heard.clone(), dir: received.path().to_path_buf() };
        let client = LisClient::new(data.path().to_str().unwrap().to_string(), "Test Phone".into(), Box::new(listener)).unwrap();
        (client, heard)
    }

    /// As when Android restarts the app: a new client from the same files.
    fn relaunch(&mut self) {
        self.client.disconnect();
        let (client, heard) = Self::open(&self.data, &self.received);
        self.client = client;
        self.heard = heard;
    }

    fn wait(&self, what: &str, wanted: impl Fn(&Heard) -> bool) -> Heard {
        let start = Instant::now();
        loop {
            if let Some(found) = self.heard.lock().unwrap().iter().rev().find(|heard| wanted(heard)) {
                return found.clone();
            }
            assert!(start.elapsed() < Duration::from_secs(15), "never heard {what}: {:?}", self.heard.lock().unwrap());
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn clear(&self) {
        self.heard.lock().unwrap().clear();
    }

    fn state(&self, state: ConnectionState) {
        self.wait(&format!("{state:?}"), |heard| matches!(heard, Heard::State(s, _) if *s == state));
    }

    fn pair(&self, rt: &Runtime, pc: &Pc) {
        self.client.connect("127.0.0.1".into(), pc.port);
        self.state(ConnectionState::Unpaired);
        rt.block_on(pc.ok(json!({ "cmd": "pair_open" })));
        self.client.start_pairing();
        let Heard::Code(code) = self.wait("the code", |heard| matches!(heard, Heard::Code(_))) else { unreachable!() };
        let status = rt.block_on(pc.until("the code", |s| !s["pair_code"].is_null()));
        assert_eq!(status["pair_code"]["code"], json!(code));
        self.client.confirm_pairing(true);
        rt.block_on(pc.ok(json!({ "cmd": "pair_confirm", "accept": true })));
        self.wait("pairing", |heard| matches!(heard, Heard::PairResult(true, _)));
        self.state(ConnectionState::Connected);
    }
}

fn stats() -> PhoneStats {
    PhoneStats { battery_percent: 55, charging: false, storage_used: 1, storage_total: 2, memory_used: 3, memory_total: 4, network: "Wi-Fi".into() }
}

fn start() -> (Runtime, Pc) {
    let rt = Runtime::new().unwrap();
    let pc = Pc::start();
    rt.block_on(pc.wait_for_socket());
    (rt, pc)
}

#[test]
fn pairing_then_everything_the_app_does() {
    let (rt, pc) = start();
    let app = App::new();
    assert_eq!(app.client.paired_pc(), None);
    app.pair(&rt, &pc);

    let paired = app.client.paired_pc().expect("paired");
    assert_eq!((paired.host.as_str(), paired.port), ("127.0.0.1", pc.port));
    assert_eq!(paired.fingerprint, rt.block_on(pc.status())["fingerprint"].as_str().unwrap());

    // The greeting from the PC
    app.wait("stats", |heard| matches!(heard, Heard::Stats(_)));

    app.client.send_stats(stats());
    let status = rt.block_on(pc.until("phone stats", |s| !s["phones"][0]["stats"].is_null()));
    assert_eq!(status["phones"][0]["stats"]["battery_percent"], json!(55));
    assert_eq!(status["phones"][0]["stats"]["network"], json!("Wi-Fi"));

    app.client.lock_pc();
    rt.block_on(pc.log_until("hyprctl", "quickshell:lock"));

    let source = app.received.path().join("from-phone.txt");
    std::fs::write(&source, b"to the pc").unwrap();
    let fd = std::fs::File::open(&source).unwrap().into_raw_fd();
    app.client.send_file(fd, "from-phone.txt".into(), 0).unwrap();
    app.wait("sent", |heard| *heard == Heard::Finished("from-phone.txt".into(), false, true));
    assert_eq!(std::fs::read(pc.downloads().join("from-phone.txt")).unwrap(), b"to the pc");

    let source = pc.home.path().join("from-pc.txt");
    std::fs::write(&source, b"to the phone").unwrap();
    rt.block_on(pc.ok(json!({ "cmd": "send_file", "path": source.to_str().unwrap() })));
    app.wait("received", |heard| *heard == Heard::Finished("from-pc.txt".into(), true, true));
    assert_eq!(std::fs::read(app.received.path().join("from-pc.txt")).unwrap(), b"to the phone");
}

#[test]
fn pairing_survives_a_relaunch() {
    use std::os::unix::fs::PermissionsExt;
    let (rt, pc) = start();
    let mut app = App::new();
    app.pair(&rt, &pc);

    let mode = std::fs::metadata(app.data.path().join("pc.json")).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    let fingerprint = app.client.fingerprint();

    app.relaunch();
    assert_eq!(app.client.fingerprint(), fingerprint, "the phone's identity is kept");
    let paired = app.client.paired_pc().expect("still paired");
    app.client.connect(paired.host, paired.port);
    app.state(ConnectionState::Connected);
}

#[test]
fn the_app_reconnects_on_its_own_when_the_pc_comes_back() {
    let (rt, mut pc) = start();
    let app = App::new();
    app.pair(&rt, &pc);
    app.clear();

    rt.block_on(pc.restart());
    app.state(ConnectionState::Disconnected);
    app.client.reconnect_now();
    app.state(ConnectionState::Connected);
}

#[test]
fn a_different_pc_at_the_address_hears_nothing() {
    let (rt, pc) = start();
    let app = App::new();
    app.pair(&rt, &pc);
    app.clear();

    let (rt2, impostor) = start();
    rt2.block_on(impostor.ok(json!({ "cmd": "pair_open" })));
    app.client.connect("127.0.0.1".into(), impostor.port);
    app.state(ConnectionState::WrongPc);

    app.client.lock_pc();
    app.client.send_stats(stats());
    app.client.start_pairing();
    std::thread::sleep(Duration::from_millis(400));

    let status = rt2.block_on(impostor.status());
    assert!(status["phones"].as_array().unwrap().is_empty());
    assert!(status["pair_code"].is_null());
    assert_eq!(impostor.log("hyprctl"), "");

    let paired = app.client.paired_pc().expect("still paired with the real PC");
    assert_eq!(paired.port, pc.port, "an address that didn't prove itself isn't saved");
}

#[test]
fn a_new_address_is_kept_once_the_pc_proves_itself() {
    let (rt, mut pc) = start();
    let mut app = App::new();
    app.pair(&rt, &pc);
    app.clear();

    pc.port = common::free_port();
    rt.block_on(pc.restart());
    app.client.connect("127.0.0.1".into(), pc.port);
    app.state(ConnectionState::Connected);
    assert_eq!(app.client.paired_pc().unwrap().port, pc.port);

    app.relaunch();
    assert_eq!(app.client.paired_pc().unwrap().port, pc.port, "saved, not just in memory");
}

#[test]
fn nothing_moves_before_pairing() {
    let (rt, pc) = start();
    let app = App::new();
    app.client.connect("127.0.0.1".into(), pc.port);
    app.state(ConnectionState::Unpaired);

    app.client.lock_pc();
    app.client.send_stats(stats());
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(pc.log("hyprctl"), "");
    assert_eq!(rt.block_on(pc.status())["phones"].as_array().unwrap().len(), 0);
}

#[test]
fn declining_on_the_pc_leaves_the_phone_unpaired() {
    let (rt, pc) = start();
    let app = App::new();
    app.client.connect("127.0.0.1".into(), pc.port);
    app.state(ConnectionState::Unpaired);
    rt.block_on(pc.ok(json!({ "cmd": "pair_open" })));
    app.client.start_pairing();
    app.wait("the code", |heard| matches!(heard, Heard::Code(_)));
    rt.block_on(pc.until("the code", |s| !s["pair_code"].is_null()));

    rt.block_on(pc.ok(json!({ "cmd": "pair_confirm", "accept": false })));
    app.wait("the refusal", |heard| matches!(heard, Heard::PairResult(false, _)));
    assert_eq!(app.client.paired_pc(), None);
    assert!(!app.data.path().join("pc.json").exists());
}

#[test]
fn unpairing_from_the_phone_clears_both_sides() {
    let (rt, pc) = start();
    let app = App::new();
    app.pair(&rt, &pc);
    app.clear();

    app.client.unpair();
    assert_eq!(app.client.paired_pc(), None, "forgotten at once, so the screen can change");
    app.state(ConnectionState::Unpaired);
    rt.block_on(pc.until("no phones", |s| s["phones"].as_array().unwrap().is_empty()));
    assert_eq!(app.client.paired_pc(), None);
    assert!(!app.data.path().join("pc.json").exists());
}

#[test]
fn unpairing_while_offline_still_forgets_the_pc() {
    let (rt, pc) = start();
    let app = App::new();
    app.pair(&rt, &pc);
    app.client.disconnect();
    app.client.unpair();
    assert_eq!(app.client.paired_pc(), None);
    assert!(!app.data.path().join("pc.json").exists());
}

#[test]
fn sending_a_file_while_disconnected_is_an_error() {
    let app = App::new();
    let source = app.received.path().join("x");
    std::fs::write(&source, b"x").unwrap();
    let fd = std::fs::File::open(&source).unwrap().into_raw_fd();
    assert!(app.client.send_file(fd, "x".into(), 1).is_err());
    assert!(app.client.send_file(-1, "x".into(), 1).is_err());
}

#[test]
fn an_unreachable_pc_reports_disconnected_and_keeps_trying() {
    let app = App::new();
    let port = common::free_port();
    app.client.connect("127.0.0.1".into(), port);
    app.state(ConnectionState::Disconnected);
    app.clear();
    app.client.reconnect_now();
    app.state(ConnectionState::Connecting);
}

fn camera_config() -> VideoConfig {
    VideoConfig { width: 1920, height: 1080, fps: 60, bitrate: 16_000_000, rotation: 90, camera: "back".into() }
}

fn viewer(pc: &Pc) -> std::os::unix::net::UnixStream {
    let stream = std::os::unix::net::UnixStream::connect(pc.home.path().join("run/lis-camera.sock")).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    stream
}

fn frame_bytes(pts: i64) -> Vec<u8> {
    pts.to_be_bytes().repeat(4)
}

fn next_frame(stream: &mut std::os::unix::net::UnixStream) -> VideoFrame {
    loop {
        match read_packet(stream).expect("a packet") {
            Packet::Frame(frame) => return frame,
            Packet::Info(_) => {}
            other => panic!("expected a frame, got {other:?}"),
        }
    }
}

/// Paired, with the camera live and a viewer watching.
fn camera_live(rt: &Runtime, pc: &Pc) -> (App, std::os::unix::net::UnixStream) {
    let app = App::new();
    app.pair(rt, pc);
    let stream = viewer(pc);
    app.client.start_video(camera_config()).unwrap();
    app.wait("the camera live", |heard| *heard == Heard::Video(VideoState::Live));
    app.wait("one viewer", |heard| *heard == Heard::Viewers(1));
    (app, stream)
}

#[test]
fn camera_frames_reach_a_viewer_intact() {
    let (rt, pc) = start();
    let (app, mut stream) = camera_live(&rt, &pc);

    match read_packet(&mut stream).unwrap() {
        Packet::Info(info) => {
            assert_eq!(info, VideoInfo::from(camera_config()));
            assert_eq!(info.codec, CODEC_H264);
        }
        other => panic!("expected the camera's info, got {other:?}"),
    }

    assert!(app.client.send_video_frame(frame_bytes(0), 0, false), "nothing before the first keyframe");
    assert!(!app.client.send_video_frame(frame_bytes(1), 1, true));
    assert!(!app.client.send_video_frame(frame_bytes(2), 2, false));
    assert_eq!(
        next_frame(&mut stream),
        VideoFrame { pts_us: 1, keyframe: true, data: frame_bytes(1) }
    );
    assert_eq!(
        next_frame(&mut stream),
        VideoFrame { pts_us: 2, keyframe: false, data: frame_bytes(2) }
    );

    let status = rt.block_on(pc.status());
    assert_eq!(status["camera"]["camera"], json!("back"));
    assert_eq!(status["camera"]["width"], json!(1920));
    assert_eq!(status["camera"]["fps"], json!(60));
    assert_eq!(status["camera"]["viewers"], json!(1));
    assert_eq!(status["camera"]["phone"], json!("Test Phone"));
}

#[test]
fn turning_the_phone_tells_the_viewer() {
    let (rt, pc) = start();
    let (app, mut stream) = camera_live(&rt, &pc);
    assert!(matches!(read_packet(&mut stream).unwrap(), Packet::Info(info) if info.rotation == 90));

    app.client.update_video(VideoConfig { rotation: 180, ..camera_config() });
    assert!(matches!(read_packet(&mut stream).unwrap(), Packet::Info(info) if info.rotation == 180));
    rt.block_on(pc.until("the new rotation", |s| s["camera"]["rotation"] == json!(180)));
}

#[test]
fn stopping_the_camera_tells_the_viewer_and_can_restart() {
    let (rt, pc) = start();
    let (app, mut stream) = camera_live(&rt, &pc);
    assert!(matches!(read_packet(&mut stream).unwrap(), Packet::Info(_)));

    app.client.stop_video();
    assert_eq!(read_packet(&mut stream).unwrap(), Packet::End);
    rt.block_on(pc.until("the camera gone", |s| s["camera"].is_null()));
    assert!(!app.client.send_video_frame(frame_bytes(1), 1, true), "ignored once stopped");

    app.clear();
    app.client.start_video(camera_config()).unwrap();
    app.wait("live again", |heard| *heard == Heard::Video(VideoState::Live));
    assert!(matches!(read_packet(&mut stream).unwrap(), Packet::Info(_)));
    assert!(!app.client.send_video_frame(frame_bytes(5), 5, true));
    assert_eq!(next_frame(&mut stream).pts_us, 5);
}

#[test]
fn a_late_viewer_starts_at_a_keyframe() {
    let (rt, pc) = start();
    let app = App::new();
    app.pair(&rt, &pc);
    app.client.start_video(camera_config()).unwrap();
    app.wait("the camera live", |heard| *heard == Heard::Video(VideoState::Live));
    app.client.send_video_frame(frame_bytes(1), 1, true);
    rt.block_on(pc.until("the camera known", |s| !s["camera"].is_null()));

    let mut stream = viewer(&pc);
    assert!(matches!(read_packet(&mut stream).unwrap(), Packet::Info(_)), "told what's playing at once");
    app.wait("one viewer", |heard| *heard == Heard::Viewers(1));

    app.client.send_video_frame(frame_bytes(2), 2, false);
    app.client.send_video_frame(frame_bytes(3), 3, false);
    std::thread::sleep(Duration::from_millis(100));
    app.client.send_video_frame(frame_bytes(4), 4, true);
    let first = next_frame(&mut stream);
    assert_eq!((first.pts_us, first.keyframe), (4, true));
}

#[test]
fn a_slow_viewer_only_ever_resumes_at_a_keyframe() {
    let (rt, pc) = start();
    let (app, mut stream) = camera_live(&rt, &pc);
    assert!(matches!(read_packet(&mut stream).unwrap(), Packet::Info(_)));

    // Big enough to fill the socket while the viewer isn't reading
    let big = |pts: i64| vec![pts as u8; 150_000];
    let mut pts = 0;
    for _ in 0..4 {
        for offset in 0..30 {
            app.client.send_video_frame(big(pts), pts, offset == 0);
            pts += 1;
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    let burst_end = pts;

    // Small keyframes from here on, so the viewer always has one to resume from
    let client = app.client.clone();
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let sender = {
        let done = done.clone();
        std::thread::spawn(move || {
            let mut pts = burst_end;
            while !done.load(std::sync::atomic::Ordering::SeqCst) {
                client.send_video_frame(frame_bytes(pts), pts, true);
                pts += 1;
                std::thread::sleep(Duration::from_millis(20));
            }
        })
    };

    let mut previous: Option<i64> = None;
    let mut gaps = 0;
    loop {
        let frame = next_frame(&mut stream);
        match previous {
            Some(previous) if frame.pts_us != previous + 1 => {
                gaps += 1;
                assert!(frame.keyframe, "resumed at {} after {previous} without a keyframe", frame.pts_us);
            }
            None => assert!(frame.keyframe, "started without a keyframe"),
            _ => {}
        }
        previous = Some(frame.pts_us);
        if frame.pts_us >= burst_end {
            break;
        }
    }
    done.store(true, std::sync::atomic::Ordering::SeqCst);
    sender.join().unwrap();
    assert!(gaps > 0, "the test should have made the viewer fall behind");
}

#[test]
fn a_stranger_cannot_send_video() {
    let (rt, pc) = start();
    let mut stream = viewer(&pc);
    stream.set_read_timeout(Some(Duration::from_millis(600))).unwrap();

    let stranger = Identity::generate().unwrap();
    let connector = tls::video_connector(&stranger).unwrap();
    let info = VideoInfo::from(camera_config());
    let frame = VideoFrame { pts_us: 1, keyframe: true, data: vec![1, 2, 3] };
    rt.block_on(async {
        let (mut tls, _) = tls::connect(&connector, "127.0.0.1", pc.port).await.unwrap();
        let _ = write_packet_async(&mut tls, &Packet::Info(info)).await;
        let _ = write_packet_async(&mut tls, &Packet::Frame(frame)).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
    });

    assert!(read_packet(&mut stream).is_err(), "the viewer must see nothing");
    assert!(rt.block_on(pc.status())["camera"].is_null());
}

#[test]
fn a_viewer_cannot_be_reached_by_other_users() {
    use std::os::unix::fs::PermissionsExt;
    let (_rt, pc) = start();
    let mode = std::fs::metadata(pc.home.path().join("run/lis-camera.sock")).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
fn the_camera_sends_nothing_to_a_different_pc() {
    let (rt, pc) = start();
    let app = App::new();
    app.pair(&rt, &pc);

    let (rt2, impostor) = start();
    let mut stream = viewer(&impostor);
    stream.set_read_timeout(Some(Duration::from_millis(600))).unwrap();
    app.client.connect("127.0.0.1".into(), impostor.port);
    app.state(ConnectionState::WrongPc);

    app.client.start_video(camera_config()).unwrap();
    let refused = app.wait("a refusal", |heard| matches!(heard, Heard::Video(VideoState::Refused { .. })));
    assert!(format!("{refused:?}").contains("Another device"), "{refused:?}");

    app.client.send_video_frame(frame_bytes(1), 1, true);
    assert!(read_packet(&mut stream).is_err(), "the other PC must see nothing");
    assert!(rt2.block_on(impostor.status())["camera"].is_null());
}

#[test]
fn unpairing_on_the_pc_stops_a_live_camera() {
    let (rt, pc) = start();
    let (app, mut stream) = camera_live(&rt, &pc);
    assert!(matches!(read_packet(&mut stream).unwrap(), Packet::Info(_)));
    assert!(!app.client.send_video_frame(frame_bytes(1), 1, true));
    assert_eq!(next_frame(&mut stream).pts_us, 1);

    let fingerprint = rt.block_on(pc.status())["phones"][0]["fingerprint"].as_str().unwrap().to_string();
    rt.block_on(pc.ok(json!({ "cmd": "unpair", "fingerprint": fingerprint })));

    assert_eq!(read_packet(&mut stream).unwrap(), Packet::End, "the viewer sees it stop");
    assert!(rt.block_on(pc.status())["camera"].is_null());
    let told = app.wait("the phone being told", |heard| matches!(heard, Heard::Video(VideoState::Refused { .. })));
    assert!(format!("{told:?}").contains("no longer paired"), "{told:?}");

    // Nothing more gets through
    assert!(!app.client.send_video_frame(frame_bytes(2), 2, true));
    stream.set_read_timeout(Some(Duration::from_millis(500))).unwrap();
    assert!(read_packet(&mut stream).is_err());
}

#[test]
fn the_camera_needs_pairing() {
    let app = App::new();
    assert!(app.client.start_video(camera_config()).is_err());
    assert!(!app.client.send_video_frame(vec![1], 1, true));
}

#[test]
fn while_the_pc_is_away_frames_wait_for_a_keyframe() {
    let (rt, mut pc) = start();
    let app = App::new();
    app.pair(&rt, &pc);
    pc.child.kill().unwrap();
    pc.child.wait().unwrap();
    app.state(ConnectionState::Disconnected);

    app.client.start_video(camera_config()).unwrap();
    assert!(app.client.send_video_frame(vec![1], 1, false), "asks for a keyframe");

    // A full queue drops frames rather than letting the delay grow
    let mut asked = false;
    for pts in 0..64 {
        asked |= app.client.send_video_frame(vec![1], pts, true);
    }
    assert!(asked, "a full queue should ask for a keyframe");
    assert!(app.client.send_video_frame(vec![1], 99, false));
}

#[test]
fn the_camera_comes_back_when_the_pc_does() {
    let (rt, mut pc) = start();
    let (app, _stream) = camera_live(&rt, &pc);
    app.clear();

    rt.block_on(pc.restart());
    app.client.reconnect_now();
    let mut stream = viewer(&pc);
    stream.set_read_timeout(Some(Duration::from_secs(15))).unwrap();
    app.wait("live again", |heard| *heard == Heard::Video(VideoState::Live));
    assert!(matches!(read_packet(&mut stream).unwrap(), Packet::Info(_)));

    // Waits for a keyframe, as the old stream is gone
    assert!(app.client.send_video_frame(frame_bytes(1), 1, false));
    assert!(!app.client.send_video_frame(frame_bytes(2), 2, true));
    assert_eq!(next_frame(&mut stream).pts_us, 2);
}
