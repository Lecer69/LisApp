//! The API the Android app calls, against the real daemon. Only Android's own parts are missing.

mod common;

use common::Pc;
use lis_core::identity::Identity;
use lis_core::tls;
use lis_core::video::{read_packet, write_packet_async, Packet, VideoFrame, VideoInfo, CODEC_H264};
use lis_ffi::{ActionButton, ClientListener, ConnectionState, LisClient, MediaCommand, MediaInfo, PcStats, PhoneStats, VideoConfig, VideoState};
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
    Ring(bool),
    SyncChanged(String),
    SyncError(String, String),
    Media(Option<MediaInfo>),
    Art(Vec<u8>),
    Actions(Vec<ActionButton>),
    ActionResult(String, bool),
    UnlockState(bool, bool),
    UnlockPrompt,
    UnlockChallenge(String),
    UnlockResult(bool, String),
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
    fn on_ring(&self, ring: bool) {
        self.heard.lock().unwrap().push(Heard::Ring(ring));
    }
    fn on_sync_changed(&self, kind: String) {
        self.heard.lock().unwrap().push(Heard::SyncChanged(kind));
    }
    fn on_sync_error(&self, kind: String, reason: String) {
        self.heard.lock().unwrap().push(Heard::SyncError(kind, reason));
    }
    fn on_media(&self, media: Option<MediaInfo>) {
        self.heard.lock().unwrap().push(Heard::Media(media));
    }
    fn on_media_art(&self, data: Vec<u8>) {
        self.heard.lock().unwrap().push(Heard::Art(data));
    }
    fn on_actions(&self, actions: Vec<ActionButton>) {
        self.heard.lock().unwrap().push(Heard::Actions(actions));
    }
    fn on_action_result(&self, id: String, ok: bool) {
        self.heard.lock().unwrap().push(Heard::ActionResult(id, ok));
    }
    fn on_unlock_state(&self, locked: bool, available: bool) {
        self.heard.lock().unwrap().push(Heard::UnlockState(locked, available));
    }
    fn on_unlock_prompt(&self) {
        self.heard.lock().unwrap().push(Heard::UnlockPrompt);
    }
    fn on_unlock_challenge(&self, nonce: String) {
        self.heard.lock().unwrap().push(Heard::UnlockChallenge(nonce));
    }
    fn on_unlock_result(&self, ok: bool, reason: String) {
        self.heard.lock().unwrap().push(Heard::UnlockResult(ok, reason));
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
        // The phone can get there just ahead of the PC
        rt.block_on(pc.until("the PC's side", |status| status["phones"][0]["connected"] == json!(true)));
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

    assert!(app.client.send_video_frame(frame_bytes(0), 0, false).need_keyframe, "nothing before the first keyframe");
    assert!(!app.client.send_video_frame(frame_bytes(1), 1, true).need_keyframe);
    assert!(!app.client.send_video_frame(frame_bytes(2), 2, false).need_keyframe);
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
    assert!(!app.client.send_video_frame(frame_bytes(1), 1, true).need_keyframe, "ignored once stopped");

    app.clear();
    app.client.start_video(camera_config()).unwrap();
    app.wait("live again", |heard| *heard == Heard::Video(VideoState::Live));
    assert!(matches!(read_packet(&mut stream).unwrap(), Packet::Info(_)));
    assert!(!app.client.send_video_frame(frame_bytes(5), 5, true).need_keyframe);
    assert_eq!(next_frame(&mut stream).pts_us, 5);
}

#[test]
fn a_late_viewer_starts_at_a_keyframe() {
    let (rt, pc) = start();
    let app = App::new();
    app.pair(&rt, &pc);
    app.client.start_video(camera_config()).unwrap();
    app.wait("the camera live", |heard| *heard == Heard::Video(VideoState::Live));
    rt.block_on(pc.until("the camera known", |s| !s["camera"].is_null()));

    // An earlier viewer sees the keyframe, so it's certainly past by the time the late one joins
    let mut earlier = viewer(&pc);
    assert!(matches!(read_packet(&mut earlier).unwrap(), Packet::Info(_)));
    app.wait("the earlier viewer", |heard| *heard == Heard::Viewers(1));
    app.client.send_video_frame(frame_bytes(1), 1, true);
    assert_eq!(next_frame(&mut earlier).pts_us, 1);
    app.clear();
    drop(earlier);
    app.wait("it leaving", |heard| *heard == Heard::Viewers(0));
    app.clear();

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
    // Made just after the socket `start` waits for, and locked down just after that
    let path = pc.home.path().join("run/lis-camera.sock");
    let began = Instant::now();
    let mode = loop {
        let mode = std::fs::metadata(&path).map(|metadata| metadata.permissions().mode() & 0o777);
        if matches!(mode, Ok(0o600)) || began.elapsed() > Duration::from_secs(5) {
            break mode.unwrap();
        }
        std::thread::sleep(Duration::from_millis(5));
    };
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
    assert!(!app.client.send_video_frame(frame_bytes(1), 1, true).need_keyframe);
    assert_eq!(next_frame(&mut stream).pts_us, 1);

    let fingerprint = rt.block_on(pc.status())["phones"][0]["fingerprint"].as_str().unwrap().to_string();
    rt.block_on(pc.ok(json!({ "cmd": "unpair", "fingerprint": fingerprint })));

    assert_eq!(read_packet(&mut stream).unwrap(), Packet::End, "the viewer sees it stop");
    assert!(rt.block_on(pc.status())["camera"].is_null());
    let told = app.wait("the phone being told", |heard| matches!(heard, Heard::Video(VideoState::Refused { .. })));
    assert!(format!("{told:?}").contains("no longer paired"), "{told:?}");

    // Nothing more gets through
    assert!(!app.client.send_video_frame(frame_bytes(2), 2, true).need_keyframe);
    stream.set_read_timeout(Some(Duration::from_millis(500))).unwrap();
    assert!(read_packet(&mut stream).is_err());
}

#[test]
fn the_camera_needs_pairing() {
    let app = App::new();
    assert!(app.client.start_video(camera_config()).is_err());
    assert!(!app.client.send_video_frame(vec![1], 1, true).need_keyframe);
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
    assert!(app.client.send_video_frame(vec![1], 1, false).need_keyframe, "asks for a keyframe");

    // A full queue drops frames rather than letting the delay grow
    let mut asked = false;
    for pts in 0..64 {
        asked |= app.client.send_video_frame(vec![1], pts, true).need_keyframe;
    }
    assert!(asked, "a full queue should ask for a keyframe");
    assert!(app.client.send_video_frame(vec![1], 99, false).need_keyframe);
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
    assert!(app.client.send_video_frame(frame_bytes(1), 1, false).need_keyframe);
    assert!(!app.client.send_video_frame(frame_bytes(2), 2, true).need_keyframe);
    assert_eq!(next_frame(&mut stream).pts_us, 2);
}

// Todos, notes and goals

fn note(id: i64, title: &str, body: &str, updated: i64) -> serde_json::Value {
    json!({ "id": id, "title": title, "body": body, "created": 1, "updated": updated })
}

fn doc(app: &App, kind: &str) -> serde_json::Value {
    serde_json::from_str(&app.client.sync_doc(kind.into())).unwrap()
}

impl App {
    /// Until the phone's copy of a document is this one.
    fn doc_becomes(&self, kind: &str, wanted: &serde_json::Value) {
        let start = Instant::now();
        while doc(self, kind) != *wanted {
            assert!(start.elapsed() < Duration::from_secs(15), "{kind} stayed {} and never became {wanted}", doc(self, kind));
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Like the app: read the document, change it, and say which copy the change was made on.
    fn change(&self, kind: &str, change: impl FnOnce(&mut serde_json::Value)) -> serde_json::Value {
        let based_on = self.client.sync_doc(kind.into());
        let mut changed: serde_json::Value = serde_json::from_str(&based_on).unwrap();
        change(&mut changed);
        self.client.sync_edit(kind.into(), based_on, changed.to_string()).unwrap();
        changed
    }

    fn edit(&self, kind: &str, doc: &serde_json::Value) {
        self.change(kind, |current| *current = doc.clone());
    }
}

fn file_becomes(pc: &Pc, name: &str, wanted: &serde_json::Value) {
    let start = Instant::now();
    loop {
        let found = std::fs::read(pc.shell_file(name)).ok().and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
        if found.as_ref() == Some(wanted) {
            return;
        }
        assert!(start.elapsed() < Duration::from_secs(15), "{name} is {found:?} and never became {wanted}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn the_pcs_documents_arrive_on_connecting() {
    let (rt, pc) = start();
    let notes = json!([note(1, "a", "x", 5)]);
    let goals = json!([{ "id": 2, "title": "g", "done": false, "created": 0, "subgoals": [] }]);
    pc.write_shell_file("notes.json", &notes);
    pc.write_shell_file("goals.json", &goals);

    let app = App::new();
    assert_eq!(doc(&app, "notes"), json!([]), "empty before the first sync");
    app.pair(&rt, &pc);
    app.doc_becomes("notes", &notes);
    app.doc_becomes("goals", &goals);
    app.wait("the change", |heard| *heard == Heard::SyncChanged("notes".into()));
}

#[test]
fn an_edit_on_the_phone_replaces_the_pcs_file_and_keeps_a_backup() {
    let (rt, pc) = start();
    let before = json!([note(1, "a", "x", 5)]);
    pc.write_shell_file("notes.json", &before);
    let app = App::new();
    app.pair(&rt, &pc);
    app.doc_becomes("notes", &before);

    let after = json!([note(1, "a", "from the phone", 9)]);
    app.edit("notes", &after);
    file_becomes(&pc, "notes.json", &after);

    let backups: Vec<_> = std::fs::read_dir(pc.data("sync-backups")).unwrap().map(|entry| entry.unwrap().path()).collect();
    assert_eq!(backups.len(), 1);
    assert_eq!(serde_json::from_slice::<serde_json::Value>(&std::fs::read(&backups[0]).unwrap()).unwrap(), before);
}

#[test]
fn a_save_in_the_shell_reaches_the_phone() {
    let (rt, pc) = start();
    let app = App::new();
    app.pair(&rt, &pc);
    app.doc_becomes("notes", &json!([]));

    let saved = json!([note(1, "a", "from the pc", 5)]);
    pc.write_shell_file("notes.json", &saved);
    app.doc_becomes("notes", &saved);
}

#[test]
fn edits_made_on_both_sides_apart_all_survive() {
    let (rt, pc) = start();
    let task = |id: i64, text: &str| json!({ "id": id, "text": text, "description": "", "done": false, "important": false, "list": -1 });
    let todo = |tasks: Vec<serde_json::Value>| json!({ "tasks": tasks, "lists": [{ "id": 1, "name": "Daily" }], "trash": [], "dailyDate": "2026-11-4" });
    let both = todo(vec![task(1, "keep"), task(2, "delete on the pc"), task(3, "delete on the phone")]);
    pc.write_shell_file("todo.json", &both);

    let app = App::new();
    app.pair(&rt, &pc);
    app.doc_becomes("todo", &both);
    app.client.disconnect();

    // Apart: each side deletes one and adds one
    let on_phone = todo(vec![task(1, "keep"), task(2, "delete on the pc"), task(4, "added on the phone")]);
    app.edit("todo", &on_phone);
    pc.write_shell_file("todo.json", &todo(vec![task(1, "keep"), task(3, "delete on the phone"), task(5, "added on the pc")]));

    app.client.connect("127.0.0.1".into(), pc.port);
    let merged = todo(vec![task(1, "keep"), task(5, "added on the pc"), task(4, "added on the phone")]);
    app.doc_becomes("todo", &merged);
    file_becomes(&pc, "todo.json", &merged);
}

#[test]
fn offline_edits_survive_a_relaunch_and_go_out_later() {
    let (rt, pc) = start();
    let mut app = App::new();
    app.pair(&rt, &pc);
    app.doc_becomes("notes", &json!([]));
    app.client.disconnect();

    let written = json!([note(1, "offline", "x", 5)]);
    app.edit("notes", &written);
    app.relaunch();
    assert_eq!(doc(&app, "notes"), written, "kept on the phone");

    app.client.connect("127.0.0.1".into(), pc.port);
    file_becomes(&pc, "notes.json", &written);
}

#[test]
fn quick_edits_end_with_the_last_one() {
    let (rt, pc) = start();
    let app = App::new();
    app.pair(&rt, &pc);
    app.doc_becomes("notes", &json!([]));

    // The same field again and again, faster than each can be answered
    let mut last = json!([]);
    for round in 0..40 {
        last = json!([note(1, "t", &format!("typed {round}"), round)]);
        app.edit("notes", &last);
    }
    file_becomes(&pc, "notes.json", &last);
    assert_eq!(doc(&app, "notes"), last, "and no conflict copies either");
}

#[test]
fn a_change_on_the_pc_during_a_push_is_merged_and_not_overwritten() {
    let (rt, pc) = start();
    let app = App::new();
    app.pair(&rt, &pc);
    app.doc_becomes("notes", &json!([]));

    for round in 0..15i64 {
        let phone_id = 1000 + round;
        app.change("notes", |notes| notes.as_array_mut().unwrap().insert(0, note(phone_id, "phone", "x", 100 + round)));

        // Written straight after, so it often lands between the phone's read and its push
        let mut saved: serde_json::Value = std::fs::read(pc.shell_file("notes.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_else(|| json!([]));
        saved.as_array_mut().unwrap().insert(0, note(2000 + round, "pc", "y", 100 + round));
        pc.write_shell_file("notes.json", &saved);
        std::thread::sleep(Duration::from_millis(30));
    }

    let start_time = Instant::now();
    loop {
        let found = doc(&app, "notes");
        let has = |id: i64| found.as_array().unwrap().iter().any(|note| note["id"] == json!(id));
        let on_pc = pc.read_shell_file("notes.json");
        if found == on_pc && (0..15).all(|round| has(1000 + round) || has(2000 + round)) {
            assert!((0..15).all(|round| has(2000 + round)), "a save on the PC went missing: {found}");
            break;
        }
        assert!(start_time.elapsed() < Duration::from_secs(20), "never settled: {found}\n{on_pc}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn unpairing_clears_the_documents() {
    let (rt, pc) = start();
    let notes = json!([note(1, "a", "x", 5)]);
    pc.write_shell_file("notes.json", &notes);
    let app = App::new();
    app.pair(&rt, &pc);
    app.doc_becomes("notes", &notes);

    app.client.unpair();
    assert_eq!(doc(&app, "notes"), json!([]));
    assert!(!app.data.path().join("sync/notes.json").exists());
    assert_eq!(pc.read_shell_file("notes.json"), notes, "the PC keeps its own");
}

#[test]
fn a_broken_file_on_the_pc_is_left_alone() {
    let (rt, pc) = start();
    std::fs::create_dir_all(pc.shell_file("x").parent().unwrap()).unwrap();
    std::fs::write(pc.shell_file("notes.json"), "{broken").unwrap();
    let app = App::new();
    app.pair(&rt, &pc);

    app.edit("notes", &json!([note(1, "a", "x", 5)]));
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(std::fs::read_to_string(pc.shell_file("notes.json")).unwrap(), "{broken");
}

#[test]
fn nonsense_never_leaves_the_phone() {
    let app = App::new();
    let edit = |kind: &str, doc: &str| app.client.sync_edit(kind.into(), "[]".into(), doc.into());
    assert!(edit("notes", "not json").is_err());
    assert!(edit("notes", &json!({ "not": "notes" }).to_string()).is_err());
    assert!(edit("passwords", "[]").is_err());
    assert_eq!(doc(&app, "notes"), json!([]));
}

#[test]
fn an_edit_of_the_current_copy_is_not_called_merged() {
    let app = App::new();
    let edited = app.client.sync_edit("notes".into(), "[]".into(), json!([note(1, "a", "x", 5)]).to_string()).unwrap();
    assert!(!edited.merged);
}

#[test]
fn an_edit_of_an_old_copy_keeps_what_arrived_since() {
    let (rt, pc) = start();
    let app = App::new();
    app.pair(&rt, &pc);
    app.doc_becomes("notes", &json!([]));

    // Read just before a save on the PC arrives, and saved just after
    let based_on = app.client.sync_doc("notes".into());
    let from_pc = note(1, "from the pc", "x", 5);
    pc.write_shell_file("notes.json", &json!([from_pc]));
    app.doc_becomes("notes", &json!([from_pc]));

    let from_phone = note(2, "from the phone", "y", 9);
    let edited = app.client.sync_edit("notes".into(), based_on, json!([from_phone]).to_string()).unwrap();

    let both = json!([from_phone, from_pc]);
    assert!(edited.merged);
    assert_eq!(serde_json::from_str::<serde_json::Value>(&edited.doc).unwrap(), both);
    file_becomes(&pc, "notes.json", &both);
}

// Finding the phone, and its battery

#[test]
fn ringing_goes_to_the_phone_and_it_answers() {
    let (rt, pc) = start();
    let app = App::new();

    let refused = rt.block_on(pc.request(json!({ "cmd": "ring_phone", "ring": true })));
    assert_eq!(refused["ok"], json!(false), "no phone yet");

    app.pair(&rt, &pc);
    rt.block_on(pc.ok(json!({ "cmd": "ring_phone", "ring": true })));
    app.wait("ringing", |heard| *heard == Heard::Ring(true));

    app.client.report_ringing(true);
    rt.block_on(pc.until("ringing shown", |status| status["phones"][0]["ringing"] == json!(true)));

    rt.block_on(pc.ok(json!({ "cmd": "ring_phone", "ring": false })));
    app.wait("the stop", |heard| *heard == Heard::Ring(false));
    app.client.report_ringing(false);
    rt.block_on(pc.until("ringing over", |status| status["phones"][0]["ringing"] == json!(false)));
}

#[test]
fn a_low_battery_and_a_full_one_show_on_the_pc() {
    let (rt, pc) = start();
    let app = App::new();
    app.pair(&rt, &pc);

    app.client.send_stats(PhoneStats { battery_percent: 18, charging: false, ..stats() });
    let log = rt.block_on(pc.log_until("notify-send", "Phone battery low"));
    assert!(log.contains("[Test Phone: It's at 18%]"), "{log}");

    app.client.send_stats(PhoneStats { battery_percent: 99, charging: true, ..stats() });
    app.client.send_stats(PhoneStats { battery_percent: 100, charging: true, ..stats() });
    rt.block_on(pc.log_until("notify-send", "Phone charged"));
}

// Action buttons

fn set_actions(rt: &Runtime, pc: &Pc, actions: serde_json::Value) {
    rt.block_on(pc.ok(json!({ "cmd": "actions_set", "actions": actions })));
}

#[test]
fn action_buttons_show_names_and_run_only_what_the_pc_defined() {
    let (rt, pc) = start();
    let marker = pc.home.path().join("ran");
    let app = App::new();
    app.pair(&rt, &pc);
    app.wait("no actions yet", |heard| *heard == Heard::Actions(vec![]));

    let command = format!("echo done > '{}'", marker.display());
    set_actions(&rt, &pc, json!([
        { "id": "good", "name": "Write a file", "command": command },
        { "id": "bad", "name": "Fail", "command": "exit 4" },
    ]));
    let Heard::Actions(buttons) = app.wait("the buttons", |heard| matches!(heard, Heard::Actions(list) if !list.is_empty())) else { unreachable!() };
    assert_eq!(
        buttons,
        vec![
            ActionButton { id: "good".into(), name: "Write a file".into() },
            ActionButton { id: "bad".into(), name: "Fail".into() },
        ]
    );

    app.client.run_action("good".into());
    app.wait("it worked", |heard| *heard == Heard::ActionResult("good".into(), true));
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "done\n");

    app.client.run_action("bad".into());
    app.wait("it failed", |heard| *heard == Heard::ActionResult("bad".into(), false));

    // Only ids are understood: a command in its place is just an unknown id
    std::fs::remove_file(&marker).unwrap();
    app.client.run_action(command.clone());
    app.wait("the refusal", |heard| *heard == Heard::ActionResult(command.clone(), false));
    assert!(!marker.exists());
}

#[test]
fn a_bad_action_list_changes_nothing() {
    let (rt, pc) = start();
    set_actions(&rt, &pc, json!([{ "id": "a", "name": "A", "command": "true" }]));
    let reply = rt.block_on(pc.request(json!({ "cmd": "actions_set", "actions": [{ "id": "b", "name": "", "command": "true" }] })));
    assert_eq!(reply["ok"], json!(false));
    assert_eq!(rt.block_on(pc.status())["actions"][0]["id"], json!("a"));
}

#[test]
fn a_hand_edited_action_file_reaches_the_phone() {
    let (rt, pc) = start();
    let app = App::new();
    app.pair(&rt, &pc);
    pc.write_shell_file("phone-actions.json", &json!([{ "id": "h", "name": "By hand", "command": "true" }]));
    app.wait("the button", |heard| matches!(heard, Heard::Actions(list) if list.iter().any(|action| action.id == "h")));
}

// What's playing

fn playerctl_line(player: &str, status: &str, title: &str, art: &str) -> String {
    [player, status, title, "Artist", "Album", "5000000", "60000000", art].join("\u{1f}") + "\n"
}

#[test]
fn whats_playing_reaches_the_phone_and_can_be_controlled() {
    let (rt, pc) = start();
    pc.set_tool("wpctl", "Volume: 0.52\n");
    pc.set_tool("playerctl", &playerctl_line("spotify", "Playing", "A Song", ""));
    let app = App::new();
    app.pair(&rt, &pc);

    let Heard::Media(Some(media)) = app.wait("the song", |heard| matches!(heard, Heard::Media(Some(_)))) else { unreachable!() };
    assert_eq!(
        media,
        MediaInfo {
            player: "spotify".into(),
            title: "A Song".into(),
            artist: "Artist".into(),
            album: "Album".into(),
            playing: true,
            position_ms: 5000,
            length_ms: 60000,
            volume_percent: 52,
            muted: false,
        }
    );

    app.client.media_command(MediaCommand::PlayPause);
    rt.block_on(pc.log_until("playerctl", "[-p] [spotify] [play-pause]"));
    app.client.media_command(MediaCommand::Next);
    rt.block_on(pc.log_until("playerctl", "[-p] [spotify] [next]"));
    app.client.media_command(MediaCommand::Previous);
    rt.block_on(pc.log_until("playerctl", "[-p] [spotify] [previous]"));
    app.client.media_set_volume(30);
    rt.block_on(pc.log_until("wpctl", "[set-volume] [-l] [1.0] [@DEFAULT_AUDIO_SINK@] [30%]"));

    // A change on the PC follows
    pc.set_tool("wpctl", "Volume: 0.30 [MUTED]\n");
    app.wait("the volume", |heard| matches!(heard, Heard::Media(Some(media)) if media.volume_percent == 30 && media.muted));
    pc.set_tool("playerctl", &playerctl_line("spotify", "Paused", "A Song", ""));
    app.wait("pausing", |heard| matches!(heard, Heard::Media(Some(media)) if !media.playing));

    // Nothing left to control
    pc.set_tool("playerctl", "");
    app.wait("the end", |heard| *heard == Heard::Media(None));
}

#[test]
fn cover_art_is_sent_only_when_it_is_an_image() {
    let (rt, pc) = start();
    let png = pc.home.path().join("cover art.png");
    std::fs::write(&png, b"\x89PNG\r\n\x1a\nimage").unwrap();
    let secret = pc.home.path().join("secret");
    std::fs::write(&secret, b"not an image").unwrap();

    pc.set_tool("wpctl", "Volume: 0.50\n");
    let url = format!("file://{}", png.display()).replace(' ', "%20");
    pc.set_tool("playerctl", &playerctl_line("mpv", "Playing", "One", &url));
    let app = App::new();
    app.pair(&rt, &pc);
    app.wait("the art", |heard| *heard == Heard::Art(b"\x89PNG\r\n\x1a\nimage".to_vec()));

    app.clear();
    pc.set_tool("playerctl", &playerctl_line("mpv", "Playing", "Two", &format!("file://{}", secret.display())));
    app.wait("no art", |heard| *heard == Heard::Art(vec![]));
}

// The USB cable

#[test]
fn usb_mode_keeps_the_tunnel_up_and_takes_it_down() {
    let (rt, pc) = start();
    let forward = format!("[reverse] [tcp:{0}] [tcp:{0}]", pc.port);

    assert!(pc.log("adb").is_empty(), "off until asked for");
    rt.block_on(pc.ok(json!({ "cmd": "usb_mode", "enabled": true })));
    rt.block_on(pc.log_until("adb", &forward));
    let status = rt.block_on(pc.status());
    assert_eq!(status["usb"], json!({ "enabled": true, "forwarding": true }));

    // No phone plugged in
    pc.set_tool_exit("adb", 1);
    rt.block_on(pc.ok(json!({ "cmd": "usb_mode", "enabled": true })));
    assert_eq!(rt.block_on(pc.status())["usb"]["forwarding"], json!(false));
    pc.set_tool_exit("adb", 0);

    rt.block_on(pc.ok(json!({ "cmd": "usb_mode", "enabled": false })));
    rt.block_on(pc.log_until("adb", &format!("[reverse] [--remove] [tcp:{}]", pc.port)));
    assert_eq!(rt.block_on(pc.status())["usb"], json!({ "enabled": false, "forwarding": false }));
}

#[test]
fn the_tunnel_address_is_not_remembered_over_a_real_one() {
    let (rt, pc) = start();
    let mut app = App::new();
    app.pair(&rt, &pc);

    // As saved after connecting over Wi-Fi
    let path = app.data.path().join("pc.json");
    let mut saved: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    saved["host"] = json!("192.168.1.10");
    std::fs::write(&path, saved.to_string()).unwrap();
    app.relaunch();

    app.client.connect("127.0.0.1".into(), pc.port);
    app.state(ConnectionState::Connected);
    assert_eq!(app.client.paired_pc().unwrap().host, "192.168.1.10");
}

// Unlocking with the phone's fingerprint

mod unlocking {
    use super::*;
    use ring::rand::SystemRandom;
    use ring::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_ASN1_SIGNING};

    const P256_PREFIX: &str = "3059301306072a8648ce3d020106082a8648ce3d030107034200";

    /// Stands in for the key in the phone's hardware.
    struct HardwareKey(EcdsaKeyPair);

    impl HardwareKey {
        fn new() -> Self {
            let random = SystemRandom::new();
            let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &random).unwrap();
            Self(EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8.as_ref(), &random).unwrap())
        }

        fn public(&self) -> Vec<u8> {
            [hex::decode(P256_PREFIX).unwrap().as_slice(), self.0.public_key().as_ref()].concat()
        }

        fn sign(&self, message: &[u8]) -> Vec<u8> {
            self.0.sign(&SystemRandom::new(), message).unwrap().as_ref().to_vec()
        }
    }

    fn fingerprint(app: &App) -> serde_json::Value {
        json!(app.client.fingerprint())
    }

    /// Paired, with a key approved on the PC.
    fn approved(rt: &Runtime, pc: &Pc) -> (App, HardwareKey) {
        let app = App::new();
        app.pair(rt, pc);
        let key = HardwareKey::new();
        app.client.unlock_enroll(key.public());
        rt.block_on(pc.until("the request", |status| status["phones"][0]["unlock"] == json!("pending")));
        rt.block_on(pc.ok(json!({ "cmd": "unlock_answer", "fingerprint": fingerprint(&app), "accept": true })));
        rt.block_on(pc.until("the approval", |status| status["phones"][0]["unlock"] == json!("on")));
        (app, key)
    }

    fn lock(rt: &Runtime, pc: &Pc, locked: bool, password_required: bool) {
        rt.block_on(pc.ok(json!({ "cmd": "lock_state", "locked": locked, "password_required": password_required })));
    }

    fn challenge(app: &App) -> String {
        app.clear();
        app.client.unlock_request();
        let Heard::UnlockChallenge(nonce) = app.wait("the challenge", |heard| matches!(heard, Heard::UnlockChallenge(_))) else { unreachable!() };
        nonce
    }

    fn result(app: &App) -> (bool, String) {
        let Heard::UnlockResult(ok, reason) = app.wait("the result", |heard| matches!(heard, Heard::UnlockResult(..))) else { unreachable!() };
        (ok, reason)
    }

    fn refused_with(app: &App, wanted: &str) {
        app.clear();
        app.client.unlock_request();
        let (ok, reason) = result(app);
        assert!(!ok);
        assert!(reason.contains(wanted), "{reason}");
    }

    #[test]
    fn a_signature_from_the_approved_key_unlocks() {
        let (rt, pc) = start();
        let (app, key) = approved(&rt, &pc);
        let mut events = rt.block_on(pc.events());

        lock(&rt, &pc, true, false);
        app.wait("the lock", |heard| *heard == Heard::UnlockState(true, true));

        let nonce = challenge(&app);
        let signature = key.sign(&app.client.unlock_message(nonce.clone()).unwrap());
        app.client.unlock_respond(nonce, signature);
        assert_eq!(result(&app), (true, String::new()));

        rt.block_on(async {
            tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    let line = events.next_line().await.unwrap().unwrap();
                    if line.contains("unlock_granted") {
                        assert!(line.contains("Test Phone"));
                        return;
                    }
                }
            })
            .await
            .expect("the shell was never told");
        });
        rt.block_on(pc.log_until("notify-send", "Unlocked by Test Phone"));
    }

    #[test]
    fn asking_is_not_enough_until_approved_on_the_pc() {
        let (rt, pc) = start();
        let app = App::new();
        app.pair(&rt, &pc);
        lock(&rt, &pc, true, false);

        refused_with(&app, "Not approved");
        app.client.unlock_enroll(HardwareKey::new().public());
        rt.block_on(pc.until("the request", |status| status["phones"][0]["unlock"] == json!("pending")));
        rt.block_on(pc.log_until("notify-send", "asks to unlock this PC"));
        refused_with(&app, "Not approved");

        rt.block_on(pc.ok(json!({ "cmd": "unlock_answer", "fingerprint": fingerprint(&app), "accept": false })));
        rt.block_on(pc.until("the refusal", |status| status["phones"][0]["unlock"] == json!("off")));
        app.wait("being told", |heard| matches!(heard, Heard::UnlockResult(false, reason) if reason.contains("Declined")));
        refused_with(&app, "Not approved");
    }

    #[test]
    fn nothing_on_the_socket_can_unlock_or_approve_without_asking() {
        let (rt, pc) = start();
        let app = App::new();
        app.pair(&rt, &pc);
        for command in ["unlock", "unlock_granted", "unlock_now"] {
            let reply = rt.block_on(pc.request(json!({ "cmd": command })));
            assert_eq!(reply["error"], json!("Unknown command"), "{command}");
        }
        let reply = rt.block_on(pc.request(json!({ "cmd": "unlock_answer", "fingerprint": fingerprint(&app), "accept": true })));
        assert_eq!(reply["ok"], json!(false), "nothing to approve");
    }

    #[test]
    fn another_key_a_replay_or_an_old_challenge_fails() {
        let (rt, pc) = start();
        let (app, key) = approved(&rt, &pc);
        lock(&rt, &pc, true, false);

        // Signed by something else
        let nonce = challenge(&app);
        let forged = HardwareKey::new().sign(&app.client.unlock_message(nonce.clone()).unwrap());
        app.client.unlock_respond(nonce, forged);
        assert!(!result(&app).0);

        // Right key, for a challenge that has already been used
        let nonce = challenge(&app);
        let signature = key.sign(&app.client.unlock_message(nonce.clone()).unwrap());
        app.client.unlock_respond(nonce.clone(), signature.clone());
        assert!(result(&app).0);
        app.clear();
        app.client.unlock_respond(nonce, signature);
        assert!(!result(&app).0, "a replay");
    }

    #[test]
    fn a_new_key_only_counts_once_approved() {
        let (rt, pc) = start();
        let (app, old_key) = approved(&rt, &pc);
        lock(&rt, &pc, true, false);

        let new_key = HardwareKey::new();
        app.client.unlock_enroll(new_key.public());
        rt.block_on(pc.until("the request", |status| status["phones"][0]["unlock"] == json!("pending")));

        let nonce = challenge(&app);
        let signature = new_key.sign(&app.client.unlock_message(nonce.clone()).unwrap());
        app.client.unlock_respond(nonce, signature);
        assert!(!result(&app).0, "not approved yet");

        let nonce = challenge(&app);
        let signature = old_key.sign(&app.client.unlock_message(nonce.clone()).unwrap());
        app.client.unlock_respond(nonce, signature);
        assert!(result(&app).0, "the approved one still works");
    }

    #[test]
    fn the_password_is_needed_after_booting_or_when_not_locked() {
        let (rt, pc) = start();
        let (app, _key) = approved(&rt, &pc);

        refused_with(&app, "isn't locked");
        lock(&rt, &pc, true, true);
        app.wait("no unlocking", |heard| *heard == Heard::UnlockState(true, false));
        refused_with(&app, "wants its password");
    }

    fn ask_for_prompt(rt: &Runtime, pc: &Pc) {
        rt.block_on(pc.ok(json!({ "cmd": "unlock_prompt" })));
    }

    /// How many prompts arrived. Messages reach the phone in order, so the ring's stop comes after
    /// anything that was going to.
    fn prompts(rt: &Runtime, pc: &Pc, app: &App) -> usize {
        app.heard.lock().unwrap().retain(|heard| *heard != Heard::Ring(false));
        rt.block_on(pc.ok(json!({ "cmd": "ring_phone", "ring": false })));
        app.wait("everything sent so far", |heard| *heard == Heard::Ring(false));
        app.heard.lock().unwrap().iter().filter(|heard| **heard == Heard::UnlockPrompt).count()
    }

    #[test]
    fn a_prompt_reaches_the_phone_but_not_too_often() {
        let (rt, pc) = start();
        let (app, _key) = approved(&rt, &pc);
        lock(&rt, &pc, true, false);

        ask_for_prompt(&rt, &pc);
        ask_for_prompt(&rt, &pc);
        ask_for_prompt(&rt, &pc);
        assert_eq!(prompts(&rt, &pc, &app), 1, "pressing more keys doesn't keep it coming");

        // A new lock is a new reason to ask
        lock(&rt, &pc, false, false);
        lock(&rt, &pc, true, false);
        ask_for_prompt(&rt, &pc);
        assert_eq!(prompts(&rt, &pc, &app), 2);
    }

    #[test]
    fn no_prompt_when_the_phone_could_not_unlock_anyway() {
        let (rt, pc) = start();
        let (app, _key) = approved(&rt, &pc);

        ask_for_prompt(&rt, &pc);
        assert_eq!(prompts(&rt, &pc, &app), 0, "not locked");

        lock(&rt, &pc, true, true);
        ask_for_prompt(&rt, &pc);
        assert_eq!(prompts(&rt, &pc, &app), 0, "wants the password");

        lock(&rt, &pc, true, false);
        rt.block_on(pc.ok(json!({ "cmd": "unlock_disable", "fingerprint": fingerprint(&app) })));
        ask_for_prompt(&rt, &pc);
        assert_eq!(prompts(&rt, &pc, &app), 0, "no longer allowed");
    }

    #[test]
    fn a_phone_that_never_asked_gets_no_prompt() {
        let (rt, pc) = start();
        let app = App::new();
        app.pair(&rt, &pc);
        lock(&rt, &pc, true, false);
        ask_for_prompt(&rt, &pc);
        assert_eq!(prompts(&rt, &pc, &app), 0);
    }

    #[test]
    fn locking_from_the_phone_does_not_prompt_it() {
        let (rt, pc) = start();
        let (app, _key) = approved(&rt, &pc);
        app.client.lock_pc();
        rt.block_on(pc.log_until("hyprctl", "quickshell:lock"));
        lock(&rt, &pc, true, false);
        ask_for_prompt(&rt, &pc);
        assert_eq!(prompts(&rt, &pc, &app), 0);
    }

    #[test]
    fn a_prompt_grants_nothing() {
        let (rt, pc) = start();
        let (app, _key) = approved(&rt, &pc);
        lock(&rt, &pc, true, false);
        ask_for_prompt(&rt, &pc);
        app.wait("the prompt", |heard| *heard == Heard::UnlockPrompt);
        let status = rt.block_on(pc.status());
        assert_eq!(status["lock"]["locked"], json!(true));
        assert!(!pc.log("notify-send").contains("Unlocked by"));
    }

    #[test]
    fn a_challenge_is_void_once_the_pc_is_unlocked() {
        let (rt, pc) = start();
        let (app, key) = approved(&rt, &pc);
        lock(&rt, &pc, true, false);
        let nonce = challenge(&app);
        lock(&rt, &pc, false, false);

        let signature = key.sign(&app.client.unlock_message(nonce.clone()).unwrap());
        app.clear();
        app.client.unlock_respond(nonce, signature);
        assert!(!result(&app).0);
    }

    #[test]
    fn too_many_wrong_signatures_leave_only_the_password() {
        let (rt, pc) = start();
        let (app, key) = approved(&rt, &pc);
        lock(&rt, &pc, true, false);

        for _ in 0..3 {
            let nonce = challenge(&app);
            app.client.unlock_respond(nonce, vec![1, 2, 3]);
            assert!(!result(&app).0);
        }
        app.wait("no more", |heard| *heard == Heard::UnlockState(true, false));
        refused_with(&app, "wants its password");

        // The password was typed, and it's locked again later
        lock(&rt, &pc, false, false);
        lock(&rt, &pc, true, false);
        let nonce = challenge(&app);
        let signature = key.sign(&app.client.unlock_message(nonce.clone()).unwrap());
        app.client.unlock_respond(nonce, signature);
        assert!(result(&app).0);
    }

    #[test]
    fn turning_it_off_and_unpairing_end_it() {
        let (rt, pc) = start();
        let (app, _key) = approved(&rt, &pc);
        lock(&rt, &pc, true, false);
        app.wait("available", |heard| *heard == Heard::UnlockState(true, true));

        rt.block_on(pc.ok(json!({ "cmd": "unlock_disable", "fingerprint": fingerprint(&app) })));
        app.wait("gone", |heard| *heard == Heard::UnlockState(true, false));
        refused_with(&app, "Not approved");
    }

    #[test]
    fn deleting_the_key_on_the_phone_ends_it_on_the_pc() {
        let (rt, pc) = start();
        let (app, _key) = approved(&rt, &pc);
        app.client.unlock_revoke();
        rt.block_on(pc.until("the key gone", |status| status["phones"][0]["unlock"] == json!("off")));
        assert!(!std::fs::read_to_string(pc.data("trusted.json")).unwrap().contains("unlock_key"));
    }

    #[test]
    fn approval_survives_a_restart() {
        let (rt, mut pc) = start();
        let (app, key) = approved(&rt, &pc);
        rt.block_on(pc.restart());
        rt.block_on(pc.until("the key", |status| status["phones"][0]["unlock"] == json!("on")));

        app.client.reconnect_now();
        app.wait("reconnecting", |heard| matches!(heard, Heard::State(ConnectionState::Disconnected, _)));
        app.clear();
        app.client.reconnect_now();
        app.state(ConnectionState::Connected);

        lock(&rt, &pc, true, false);
        let nonce = challenge(&app);
        let signature = key.sign(&app.client.unlock_message(nonce.clone()).unwrap());
        app.client.unlock_respond(nonce, signature);
        assert!(result(&app).0);
    }
}
