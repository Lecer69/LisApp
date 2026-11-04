//! Runs the real daemon binary, with a phone built on the same core as the Android app.

mod common;

use common::{Pc, WAIT};

use lis_core::identity::Identity;
use lis_core::protocol::{FileOffer, Message, PhoneStats};
use lis_core::session::{run, Command, Event, Handle, Handler, Local, Peer, Role, Timing};
use lis_core::tls;
use serde_json::json;
use std::collections::HashSet;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout};

struct PhoneHandler {
    trusted: Mutex<HashSet<String>>,
    dir: PathBuf,
    events: mpsc::UnboundedSender<Event>,
}

impl Handler for PhoneHandler {
    fn is_trusted(&self, fingerprint: &str) -> bool {
        self.trusted.lock().unwrap().contains(fingerprint)
    }
    fn may_pair(&self) -> bool {
        false
    }
    fn trust(&self, peer: &Peer) {
        self.trusted.lock().unwrap().insert(peer.fingerprint.clone());
    }
    fn forget(&self, fingerprint: &str) {
        self.trusted.lock().unwrap().remove(fingerprint);
    }
    fn open_incoming(&self, _peer: &Peer, offer: &FileOffer) -> Option<std::fs::File> {
        Some(std::fs::File::create(self.dir.join(&offer.name)).unwrap())
    }
    fn event(&self, event: Event) {
        let _ = self.events.send(event);
    }
}

struct Phone {
    identity: Identity,
    handler: Arc<PhoneHandler>,
    events: mpsc::UnboundedReceiver<Event>,
    handle: Handle,
    task: JoinHandle<()>,
    _dir: tempfile::TempDir,
}

impl Phone {
    async fn connect(pc: &Pc) -> Self {
        let identity = Identity::generate().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (events_tx, events) = mpsc::unbounded_channel();
        let handler = Arc::new(PhoneHandler { trusted: Mutex::new(HashSet::new()), dir: dir.path().to_path_buf(), events: events_tx });
        let (handle, task) = Self::dial(&identity, &handler, pc).await;
        Self { identity, handler, events, handle, task, _dir: dir }
    }

    async fn dial(identity: &Identity, handler: &Arc<PhoneHandler>, pc: &Pc) -> (Handle, JoinHandle<()>) {
        let connector = tls::connector(identity).unwrap();
        let (stream, peer) = tls::connect(&connector, "127.0.0.1", pc.port).await.unwrap();
        let (handle, commands) = Handle::channel();
        let local = Local { name: "Test Phone".into(), fingerprint: identity.fingerprint(), role: Role::Client, timing: Timing::default() };
        let handler: Arc<dyn Handler> = handler.clone();
        let task = tokio::spawn(async move {
            let _ = run(stream, local, peer, handler, commands).await;
        });
        (handle, task)
    }

    async fn reconnect(&mut self, pc: &Pc) {
        self.handle.send(Command::Close).await;
        self.task.abort();
        let (handle, task) = Self::dial(&self.identity, &self.handler, pc).await;
        self.handle = handle;
        self.task = task;
    }

    async fn until(&mut self, mut wanted: impl FnMut(&Event) -> bool) -> Event {
        timeout(WAIT, async {
            loop {
                let event = self.events.recv().await.expect("phone events closed");
                if wanted(&event) {
                    return event;
                }
            }
        })
        .await
        .expect("timed out waiting for a phone event")
    }

    async fn send(&self, message: Message) {
        self.handle.send(Command::Send(message)).await;
    }

    async fn send_file(&self, contents: &[u8], name: &str) -> tempfile::NamedTempFile {
        let mut source = tempfile::NamedTempFile::new().unwrap();
        source.write_all(contents).unwrap();
        let file = std::fs::File::open(source.path()).unwrap();
        self.handle.send(Command::SendFile { file, name: name.to_string(), size: contents.len() as u64 }).await;
        source
    }

    async fn file_finished(&mut self) -> bool {
        match self.until(|e| matches!(e, Event::FileFinished { .. })).await {
            Event::FileFinished { ok, .. } => ok,
            _ => unreachable!(),
        }
    }

    /// Pairs the way a person would, comparing the codes.
    async fn pair(&mut self, pc: &Pc) {
        pc.ok(json!({ "cmd": "pair_open" })).await;
        self.until(|e| matches!(e, Event::Connected { .. })).await;
        self.handle.send(Command::StartPairing).await;

        let code = match self.until(|e| matches!(e, Event::PairCode { .. })).await {
            Event::PairCode { code, .. } => code,
            _ => unreachable!(),
        };
        let status = pc.until("the code on the PC", |s| !s["pair_code"].is_null()).await;
        assert_eq!(status["pair_code"]["code"], json!(code));
        assert_eq!(status["pair_code"]["name"], json!("Test Phone"));

        self.handle.send(Command::ConfirmPairing(true)).await;
        pc.ok(json!({ "cmd": "pair_confirm", "accept": true })).await;
        match self.until(|e| matches!(e, Event::PairResult { .. })).await {
            Event::PairResult { ok, reason, .. } => assert!(ok, "{reason}"),
            _ => unreachable!(),
        }
        pc.until("the phone listed as connected", |s| s["phones"][0]["connected"] == json!(true)).await;
    }

    fn fingerprint(&self) -> String {
        self.identity.fingerprint()
    }
}

async fn start() -> Pc {
    let pc = Pc::start();
    pc.wait_for_socket().await;
    pc
}

async fn paired() -> (Pc, Phone) {
    let pc = start().await;
    let mut phone = Phone::connect(&pc).await;
    phone.pair(&pc).await;
    (pc, phone)
}

#[tokio::test]
async fn pairing_is_refused_until_opened_on_the_pc() {
    let pc = start().await;
    let mut phone = Phone::connect(&pc).await;
    phone.until(|e| matches!(e, Event::Connected { paired: false, .. })).await;

    phone.handle.send(Command::StartPairing).await;
    match phone.until(|e| matches!(e, Event::PairResult { .. })).await {
        Event::PairResult { ok, reason, .. } => assert!(!ok && reason.contains("Open pairing"), "{reason}"),
        _ => unreachable!(),
    }
    assert!(pc.status().await["phones"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn a_second_phone_cannot_replace_the_code_on_screen() {
    let pc = start().await;
    let mut first = Phone::connect(&pc).await;
    first.until(|e| matches!(e, Event::Connected { .. })).await;
    pc.ok(json!({ "cmd": "pair_open" })).await;
    first.handle.send(Command::StartPairing).await;
    first.until(|e| matches!(e, Event::PairCode { .. })).await;
    let shown = pc.until("first code", |s| !s["pair_code"].is_null()).await["pair_code"]["code"].clone();

    let mut second = Phone::connect(&pc).await;
    second.until(|e| matches!(e, Event::Connected { .. })).await;
    second.handle.send(Command::StartPairing).await;
    match second.until(|e| matches!(e, Event::PairResult { .. })).await {
        Event::PairResult { ok, .. } => assert!(!ok),
        _ => unreachable!(),
    }
    assert_eq!(pc.status().await["pair_code"]["code"], shown);
}

#[tokio::test]
async fn pairing_works_and_survives_a_restart() {
    let (mut pc, mut phone) = paired().await;
    let status = pc.status().await;
    assert_eq!(status["phones"][0]["fingerprint"], json!(phone.fingerprint()));
    assert_eq!(status["phones"][0]["name"], json!("Test Phone"));
    assert_eq!(status["pairing"], json!(false), "pairing closes once a phone is paired");

    pc.restart().await;
    phone.reconnect(&pc).await;
    phone.until(|e| matches!(e, Event::Connected { paired: true, .. })).await;
    pc.until("the phone back", |s| s["phones"][0]["connected"] == json!(true)).await;
}

#[tokio::test]
async fn saved_secrets_are_private() {
    let (pc, _phone) = paired().await;
    for name in ["identity.key", "identity.cert", "trusted.json"] {
        let mode = std::fs::metadata(pc.data(name)).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "{name}");
    }
    let mode = std::fs::metadata(pc.socket()).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "socket");
}

#[tokio::test]
async fn a_paired_phone_locks_the_pc() {
    let (pc, phone) = paired().await;
    assert_eq!(pc.log("hyprctl"), "");
    phone.send(Message::LockPc).await;
    let log = pc.log_until("hyprctl", "quickshell:lock").await;
    assert_eq!(log.trim(), r#"[-i] [0] [dispatch] [hl.dsp.global("quickshell:lock")]"#);
}

#[tokio::test]
async fn an_unpaired_phone_cannot_lock_the_pc() {
    let pc = start().await;
    let mut phone = Phone::connect(&pc).await;
    phone.until(|e| matches!(e, Event::Connected { paired: false, .. })).await;
    phone.send(Message::LockPc).await;
    sleep(Duration::from_millis(300)).await;
    assert_eq!(pc.log("hyprctl"), "");
}

#[tokio::test]
async fn stats_reach_the_shell_and_the_phone() {
    let (pc, mut phone) = paired().await;
    phone.send(Message::PhoneStats(PhoneStats { battery_percent: 73, charging: true, network: "Wi-Fi".into(), ..Default::default() })).await;
    let status = pc.until("phone stats", |s| !s["phones"][0]["stats"].is_null()).await;
    assert_eq!(status["phones"][0]["stats"]["battery_percent"], json!(73));
    assert_eq!(status["phones"][0]["stats"]["charging"], json!(true));

    let got = phone.until(|e| matches!(e, Event::Message { message: Message::PcStats(_), .. })).await;
    assert!(matches!(got, Event::Message { message: Message::PcStats(_), .. }));
}

#[tokio::test]
async fn files_from_the_phone_land_safely_in_downloads() {
    let (pc, mut phone) = paired().await;

    let _keep = phone.send_file(b"hello", "../../../.bashrc").await;
    assert!(phone.file_finished().await);
    assert_eq!(std::fs::read(pc.downloads().join("bashrc")).unwrap(), b"hello");
    assert!(!pc.home.path().join(".bashrc").exists());

    let _keep = phone.send_file(b"second", "bashrc").await;
    assert!(phone.file_finished().await);
    assert_eq!(std::fs::read(pc.downloads().join("bashrc (1)")).unwrap(), b"second");
    assert_eq!(std::fs::read(pc.downloads().join("bashrc")).unwrap(), b"hello");

    pc.log_until("notify-send", "bashrc").await;
}

#[tokio::test]
async fn a_cut_transfer_leaves_no_partial_file() {
    let (pc, phone) = paired().await;
    let contents = vec![9u8; 80_000_000];
    let _keep = phone.send_file(&contents, "big.bin").await;

    for _ in 0..400 {
        if pc.downloads().join("big.bin.part").exists() {
            break;
        }
        sleep(Duration::from_millis(5)).await;
    }
    phone.task.abort();

    for _ in 0..400 {
        let names: Vec<_> = std::fs::read_dir(pc.downloads()).unwrap().flatten().collect();
        if names.is_empty() {
            return;
        }
        sleep(Duration::from_millis(25)).await;
    }
    panic!("files left behind: {:?}", std::fs::read_dir(pc.downloads()).unwrap().flatten().map(|e| e.file_name()).collect::<Vec<_>>());
}

#[tokio::test]
async fn files_go_from_the_pc_to_the_phone() {
    let (pc, mut phone) = paired().await;
    let source = pc.home.path().join("photo.jpg");
    std::fs::write(&source, b"jpeg bytes").unwrap();

    pc.ok(json!({ "cmd": "send_file", "path": source.to_str().unwrap() })).await;
    assert!(phone.file_finished().await);
    assert_eq!(std::fs::read(phone.handler.dir.join("photo.jpg")).unwrap(), b"jpeg bytes");

    let missing = pc.request(json!({ "cmd": "send_file", "path": "/no/such/file" })).await;
    assert_eq!(missing["ok"], json!(false));
    let dir = pc.request(json!({ "cmd": "send_file", "path": pc.home.path().to_str().unwrap() })).await;
    assert_eq!(dir["ok"], json!(false));
}

#[tokio::test]
async fn unpairing_tells_the_phone() {
    let (pc, mut phone) = paired().await;

    pc.ok(json!({ "cmd": "unpair", "fingerprint": phone.fingerprint() })).await;
    phone.until(|e| matches!(e, Event::Unpaired { .. })).await;
    pc.until("no phones", |s| s["phones"].as_array().unwrap().is_empty()).await;
    assert!(!std::fs::read_to_string(pc.data("trusted.json")).unwrap().contains(&phone.fingerprint()));

    phone.send(Message::LockPc).await;
    sleep(Duration::from_millis(300)).await;
    assert_eq!(pc.log("hyprctl"), "");
}

/// A phone that ignores being unpaired and keeps sending, as a modified app could.
#[tokio::test]
async fn unpairing_cuts_off_a_phone_that_keeps_streaming() {
    use lis_core::video::{read_packet, write_packet_async, Packet, VideoFrame, VideoInfo, CODEC_H264};

    let (pc, phone) = paired().await;
    let info = VideoInfo { codec: CODEC_H264.into(), width: 1920, height: 1080, fps: 60, bitrate: 1, rotation: 0, camera: "back".into() };

    let viewer_path = pc.home.path().join("run/lis-camera.sock");
    let mut viewer = std::os::unix::net::UnixStream::connect(&viewer_path).unwrap();
    viewer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();

    let connector = tls::video_connector(&phone.identity).unwrap();
    let (mut stream, _) = tls::connect(&connector, "127.0.0.1", pc.port).await.unwrap();
    write_packet_async(&mut stream, &Packet::Info(info)).await.unwrap();

    // Keeps going whatever happens, and stops only at the end of the test
    let sender = tokio::spawn(async move {
        let mut pts = 0;
        loop {
            let frame = Packet::Frame(VideoFrame { pts_us: pts, keyframe: true, data: vec![1; 64] });
            if write_packet_async(&mut stream, &frame).await.is_err() {
                return pts;
            }
            pts += 1;
            sleep(Duration::from_millis(5)).await;
        }
    });

    let (reader, first) = tokio::task::spawn_blocking(move || {
        loop {
            match read_packet(&mut viewer).expect("a packet") {
                Packet::Frame(frame) => return (viewer, frame),
                Packet::Info(_) => {}
                other => panic!("{other:?}"),
            }
        }
    })
    .await
    .unwrap();
    assert!(first.keyframe, "its frames reach the viewer while it's paired");

    pc.ok(json!({ "cmd": "unpair", "fingerprint": phone.fingerprint() })).await;

    let ended = tokio::task::spawn_blocking(move || {
        let mut viewer = reader;
        let limit = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < limit {
            match read_packet(&mut viewer) {
                Ok(Packet::End) => return true,
                Ok(_) => {}
                Err(_) => return false,
            }
        }
        false
    })
    .await
    .unwrap();
    assert!(ended, "the viewer sees the camera stop");
    assert!(pc.status().await["camera"].is_null());

    let stopped = timeout(WAIT, sender).await.expect("the daemon hangs up on it");
    assert!(stopped.is_ok());

    // And it can't come back
    let (mut again, _) = tls::connect(&connector, "127.0.0.1", pc.port).await.unwrap();
    let _ = write_packet_async(&mut again, &Packet::Info(VideoInfo { codec: CODEC_H264.into(), width: 1, height: 1, fps: 1, bitrate: 1, rotation: 0, camera: "x".into() })).await;
    sleep(Duration::from_millis(300)).await;
    assert!(pc.status().await["camera"].is_null());
}

#[tokio::test]
async fn only_one_daemon_runs_and_bad_requests_are_answered() {
    let pc = start().await;
    let second = std::process::Command::new(env!("CARGO_BIN_EXE_lis-daemon"))
        .env_clear()
        .env("HOME", pc.home.path())
        .env("XDG_RUNTIME_DIR", pc.home.path().join("run"))
        .env("LIS_APP_NO_MDNS", "1")
        .output()
        .unwrap();
    assert!(!second.status.success());
    assert!(String::from_utf8_lossy(&second.stderr).contains("already running"));
    pc.status().await;

    let stream = UnixStream::connect(pc.socket()).await.unwrap();
    let (reader, mut writer) = stream.into_split();
    writer.write_all(b"not json\n").await.unwrap();
    let line = BufReader::new(reader).lines().next_line().await.unwrap().unwrap();
    assert!(line.contains(r#""ok":false"#));
    assert_eq!(pc.request(json!({ "cmd": "nonsense" })).await["ok"], json!(false));
    assert_eq!(pc.request(json!({ "cmd": "pair_confirm", "accept": true })).await["ok"], json!(false));
    assert_eq!(pc.request(json!({ "cmd": "unpair", "fingerprint": "nope" })).await["ok"], json!(false));
}

#[tokio::test]
async fn a_message_from_a_newer_version_does_not_end_the_connection() {
    let pc = Pc::start();
    pc.wait_for_socket().await;
    let mut phone = Phone::connect(&pc).await;
    phone.pair(&pc).await;

    phone.send(Message::Unknown).await;
    phone.send(Message::PhoneStats(PhoneStats { battery_percent: 77, ..Default::default() })).await;
    pc.until("the stats after it", |status| status["phones"][0]["stats"]["battery_percent"] == json!(77)).await;
    assert!(pc.status().await["phones"][0]["connected"] == json!(true));
}

#[tokio::test]
async fn a_bad_document_from_a_phone_changes_nothing_and_it_is_told() {
    let pc = Pc::start();
    pc.wait_for_socket().await;
    let notes = json!([{ "id": 1, "title": "a", "body": "x", "created": 1, "updated": 1 }]);
    pc.write_shell_file("notes.json", &notes);
    let mut phone = Phone::connect(&pc).await;
    phone.pair(&pc).await;

    let rev = match phone.until(|event| matches!(event, Event::Message { message: Message::SyncDoc { kind, .. }, .. } if kind == "notes")).await {
        Event::Message { message: Message::SyncDoc { rev, doc, .. }, .. } => {
            assert_eq!(doc, notes);
            rev
        }
        _ => unreachable!(),
    };
    phone.send(Message::SyncPush { kind: "notes".into(), based_on: rev, doc: json!({ "not": "notes" }) }).await;
    phone.until(|event| matches!(event, Event::Message { message: Message::SyncRejected { kind, .. }, .. } if kind == "notes")).await;
    assert_eq!(pc.read_shell_file("notes.json"), notes);

    // Not a file that syncs, wherever it might point
    phone.send(Message::SyncPush { kind: "../../.ssh/config".into(), based_on: String::new(), doc: json!([]) }).await;
    phone.until(|event| matches!(event, Event::Message { message: Message::SyncRejected { kind, .. }, .. } if kind.contains("ssh"))).await;
    assert!(!pc.home.path().join(".ssh").exists());
}
