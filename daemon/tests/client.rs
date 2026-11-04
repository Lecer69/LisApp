//! The API the Android app calls, against the real daemon. Only Android's own parts are missing.

mod common;

use common::Pc;
use lis_ffi::{ClientListener, ConnectionState, LisClient, PcStats, PhoneStats};
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
