use lis_core::identity::Identity;
use lis_core::protocol::{read_frame, write_frame, FileOffer, Frame, Message, PcStats, PhoneStats, PROTOCOL_VERSION};
use lis_core::session::{run, Command, Event, Handle, Handler, Local, Peer, Role, SessionError, Timing};
use lis_core::tls;
use std::collections::HashSet;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::timeout;

const WAIT: Duration = Duration::from_secs(10);

struct TestHandler {
    trusted: Mutex<HashSet<String>>,
    may_pair: bool,
    accept_files: bool,
    dir: PathBuf,
    events: mpsc::UnboundedSender<Event>,
}

impl Handler for TestHandler {
    fn is_trusted(&self, fingerprint: &str) -> bool {
        self.trusted.lock().unwrap().contains(fingerprint)
    }
    fn may_pair(&self) -> bool {
        self.may_pair
    }
    fn trust(&self, peer: &Peer) {
        self.trusted.lock().unwrap().insert(peer.fingerprint.clone());
    }
    fn forget(&self, fingerprint: &str) {
        self.trusted.lock().unwrap().remove(fingerprint);
    }
    fn open_incoming(&self, _peer: &Peer, offer: &FileOffer) -> Option<std::fs::File> {
        self.accept_files.then(|| std::fs::File::create(self.dir.join(&offer.name)).unwrap())
    }
    fn event(&self, event: Event) {
        let _ = self.events.send(event);
    }
}

struct Side {
    handle: Handle,
    events: mpsc::UnboundedReceiver<Event>,
    handler: Arc<TestHandler>,
    task: JoinHandle<Result<(), SessionError>>,
    dir: tempfile::TempDir,
}

impl Side {
    async fn next(&mut self) -> Event {
        timeout(WAIT, self.events.recv()).await.expect("timed out waiting for an event").expect("events closed")
    }

    async fn until(&mut self, mut wanted: impl FnMut(&Event) -> bool) -> Event {
        loop {
            let event = self.next().await;
            if wanted(&event) {
                return event;
            }
        }
    }

    async fn nothing_for(&mut self, duration: Duration) {
        assert!(timeout(duration, self.events.recv()).await.is_err(), "expected no more events");
    }
}

struct Options {
    trust_each_other: bool,
    may_pair: bool,
    accept_files: bool,
    timing: Timing,
}

impl Default for Options {
    fn default() -> Self {
        Self { trust_each_other: false, may_pair: true, accept_files: true, timing: Timing::default() }
    }
}

fn side_handler(trusted: Option<String>, options: &Options) -> (Arc<TestHandler>, mpsc::UnboundedReceiver<Event>, tempfile::TempDir) {
    let (events, rx) = mpsc::unbounded_channel();
    let dir = tempfile::tempdir().unwrap();
    let handler = Arc::new(TestHandler {
        trusted: Mutex::new(trusted.into_iter().collect()),
        may_pair: options.may_pair,
        accept_files: options.accept_files,
        dir: dir.path().to_path_buf(),
        events,
    });
    (handler, rx, dir)
}

/// A phone and a PC over real TLS on localhost.
async fn connect(options: Options) -> (Side, Side) {
    let phone_id = Identity::generate().unwrap();
    let pc_id = Identity::generate().unwrap();
    let (phone_fp, pc_fp) = (phone_id.fingerprint(), pc_id.fingerprint());

    let acceptor = tls::acceptor(&pc_id).unwrap();
    let connector = tls::connector(&phone_id).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let accepting = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        tls::accept(&acceptor, tcp).await.unwrap()
    });
    let (client, seen_pc) = tls::connect(&connector, "127.0.0.1", port).await.unwrap();
    let (server, seen_phone) = accepting.await.unwrap();
    assert_eq!(seen_pc, pc_fp);
    assert_eq!(seen_phone, phone_fp);

    let (phone_trust, pc_trust) =
        if options.trust_each_other { (Some(pc_fp.clone()), Some(phone_fp.clone())) } else { (None, None) };

    let (phone_handler, phone_events, phone_dir) = side_handler(phone_trust, &options);
    let (pc_handler, pc_events, pc_dir) = side_handler(pc_trust, &options);
    let (phone_handle, phone_commands) = Handle::channel();
    let (pc_handle, pc_commands) = Handle::channel();

    let local = |name: &str, fingerprint: &str, role| Local {
        name: name.to_string(),
        fingerprint: fingerprint.to_string(),
        role,
        timing: options.timing,
    };
    let phone_task = tokio::spawn(run(client, local("Phone", &phone_fp, Role::Client), seen_pc, phone_handler.clone(), phone_commands));
    let pc_task = tokio::spawn(run(server, local("PC", &pc_fp, Role::Server), seen_phone, pc_handler.clone(), pc_commands));

    (
        Side { handle: phone_handle, events: phone_events, handler: phone_handler, task: phone_task, dir: phone_dir },
        Side { handle: pc_handle, events: pc_events, handler: pc_handler, task: pc_task, dir: pc_dir },
    )
}

async fn connected(side: &mut Side) -> bool {
    match side.until(|e| matches!(e, Event::Connected { .. })).await {
        Event::Connected { paired, .. } => paired,
        _ => unreachable!(),
    }
}

async fn code(side: &mut Side) -> String {
    match side.until(|e| matches!(e, Event::PairCode { .. })).await {
        Event::PairCode { code, .. } => code,
        _ => unreachable!(),
    }
}

async fn pair_result(side: &mut Side) -> (bool, String) {
    match side.until(|e| matches!(e, Event::PairResult { .. })).await {
        Event::PairResult { ok, reason, .. } => (ok, reason),
        _ => unreachable!(),
    }
}

async fn paired_sides() -> (Side, Side) {
    let (mut phone, mut pc) = connect(Options { trust_each_other: true, ..Default::default() }).await;
    assert!(connected(&mut phone).await);
    assert!(connected(&mut pc).await);
    (phone, pc)
}

#[tokio::test]
async fn pairing_needs_the_same_code_and_both_people() {
    let (mut phone, mut pc) = connect(Options::default()).await;
    assert!(!connected(&mut phone).await);
    assert!(!connected(&mut pc).await);

    phone.handle.send(Command::StartPairing).await;
    let (phone_code, pc_code) = (code(&mut phone).await, code(&mut pc).await);
    assert_eq!(phone_code, pc_code);
    assert_eq!(phone_code.len(), 6);

    phone.handle.send(Command::ConfirmPairing(true)).await;
    pc.nothing_for(Duration::from_millis(200)).await;
    assert!(phone.handler.trusted.lock().unwrap().is_empty(), "one person's yes isn't enough");

    pc.handle.send(Command::ConfirmPairing(true)).await;
    assert!(pair_result(&mut phone).await.0);
    assert!(pair_result(&mut pc).await.0);
    assert_eq!(phone.handler.trusted.lock().unwrap().len(), 1);
    assert_eq!(pc.handler.trusted.lock().unwrap().len(), 1);

    phone.handle.send(Command::Send(Message::LockPc)).await;
    let got = pc.until(|e| matches!(e, Event::Message { .. })).await;
    assert!(matches!(got, Event::Message { message: Message::LockPc, .. }));
}

#[tokio::test]
async fn declining_trusts_nobody_and_data_stays_blocked() {
    let (mut phone, mut pc) = connect(Options::default()).await;
    connected(&mut phone).await;
    connected(&mut pc).await;

    phone.handle.send(Command::StartPairing).await;
    code(&mut phone).await;
    code(&mut pc).await;
    phone.handle.send(Command::ConfirmPairing(true)).await;
    pc.handle.send(Command::ConfirmPairing(false)).await;

    let (ok, reason) = pair_result(&mut phone).await;
    assert!(!ok && reason.contains("declined"));
    assert!(!pair_result(&mut pc).await.0);
    assert!(phone.handler.trusted.lock().unwrap().is_empty());
    assert!(pc.handler.trusted.lock().unwrap().is_empty());

    phone.handle.send(Command::Send(Message::LockPc)).await;
    pc.nothing_for(Duration::from_millis(200)).await;
}

#[tokio::test]
async fn the_pc_only_pairs_while_pairing_is_open() {
    let (mut phone, mut pc) = connect(Options { may_pair: false, ..Default::default() }).await;
    connected(&mut phone).await;
    connected(&mut pc).await;

    phone.handle.send(Command::StartPairing).await;
    let (ok, reason) = pair_result(&mut phone).await;
    assert!(!ok && reason.contains("Open pairing"));
    pc.nothing_for(Duration::from_millis(200)).await;
    assert!(pc.handler.trusted.lock().unwrap().is_empty());
}

#[tokio::test]
async fn pairing_times_out() {
    let timing = Timing { pair_limit: Duration::from_millis(300), ..Default::default() };
    let (mut phone, mut pc) = connect(Options { timing, ..Default::default() }).await;
    connected(&mut phone).await;
    connected(&mut pc).await;

    phone.handle.send(Command::StartPairing).await;
    code(&mut phone).await;
    code(&mut pc).await;
    assert!(!pair_result(&mut phone).await.0);
    assert!(!pair_result(&mut pc).await.0);
    assert!(phone.handler.trusted.lock().unwrap().is_empty());
}

#[tokio::test]
async fn an_unpaired_side_sends_no_data_at_all() {
    let (mut phone, mut pc) = connect(Options::default()).await;
    connected(&mut phone).await;
    connected(&mut pc).await;

    phone.handle.send(Command::Send(Message::PhoneStats(PhoneStats { battery_percent: 50, ..Default::default() }))).await;
    phone.handle.send(Command::Send(Message::LockPc)).await;
    pc.nothing_for(Duration::from_millis(300)).await;
    assert!(!pc.task.is_finished(), "dropped locally, so the connection stays up");
}

/// A client that ignores the rules, as a stranger on the network could.
async fn raw_stranger(first_messages: Vec<Frame>) -> (Result<(), SessionError>, Vec<Event>) {
    let pc_id = Identity::generate().unwrap();
    let stranger_id = Identity::generate().unwrap();
    let acceptor = tls::acceptor(&pc_id).unwrap();
    let connector = tls::connector(&stranger_id).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let accepting = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        tls::accept(&acceptor, tcp).await.unwrap()
    });
    let (mut client, _) = tls::connect(&connector, "127.0.0.1", port).await.unwrap();
    let (server, seen) = accepting.await.unwrap();

    let options = Options { may_pair: false, ..Default::default() };
    let (handler, mut events, _dir) = side_handler(None, &options);
    let (_handle, commands) = Handle::channel();
    let local = Local { name: "PC".into(), fingerprint: pc_id.fingerprint(), role: Role::Server, timing: Timing::default() };
    let task = tokio::spawn(run(server, local, seen, handler.clone(), commands));

    for frame in &first_messages {
        write_frame(&mut client, frame).await.unwrap();
    }

    let result = timeout(WAIT, task).await.expect("the PC should have hung up").unwrap();
    assert!(handler.trusted.lock().unwrap().is_empty());
    let mut seen_events = Vec::new();
    while let Ok(event) = events.try_recv() {
        seen_events.push(event);
    }
    drop(client);
    (result, seen_events)
}

fn hello() -> Frame {
    Frame::Message(Message::Hello { protocol: PROTOCOL_VERSION, name: "Stranger".into() })
}

fn no_data_reached(events: &[Event]) {
    assert!(events.iter().all(|e| matches!(e, Event::Connected { paired: false, .. })), "{events:?}");
}

#[tokio::test]
async fn a_stranger_cannot_lock_the_pc() {
    let (result, events) = raw_stranger(vec![
        hello(),
        Frame::Message(Message::Auth { trusted: true }),
        Frame::Message(Message::LockPc),
    ])
    .await;
    assert!(matches!(result, Err(SessionError::Protocol(_))), "{result:?}");
    no_data_reached(&events);
}

#[tokio::test]
async fn a_stranger_claiming_trust_cannot_send_files() {
    let (result, events) = raw_stranger(vec![
        hello(),
        Frame::Message(Message::Auth { trusted: true }),
        Frame::Message(Message::FileOffer(FileOffer { id: 1, name: "x".into(), size: 1 })),
    ])
    .await;
    assert!(matches!(result, Err(SessionError::Protocol(_))), "{result:?}");
    no_data_reached(&events);
}

#[tokio::test]
async fn a_stranger_cannot_send_file_chunks() {
    let (result, events) = raw_stranger(vec![hello(), Frame::Chunk { id: 1, data: vec![1] }]).await;
    assert!(matches!(result, Err(SessionError::Protocol(_))), "{result:?}");
    no_data_reached(&events);
}

#[tokio::test]
async fn data_before_hello_and_wrong_versions_are_refused() {
    let (result, _) = raw_stranger(vec![Frame::Message(Message::LockPc)]).await;
    assert!(matches!(result, Err(SessionError::Protocol(_))));

    let (result, _) = raw_stranger(vec![Frame::Message(Message::Hello { protocol: 99, name: "x".into() })]).await;
    assert!(matches!(result, Err(SessionError::Protocol(_))));

    let (result, _) = raw_stranger(vec![hello(), Frame::Message(Message::PairReveal { nonce: "x".into() })]).await;
    assert!(matches!(result, Err(SessionError::Protocol(_))));
}

#[tokio::test]
async fn a_tampered_pair_reveal_is_caught() {
    let pc_id = Identity::generate().unwrap();
    let stranger_id = Identity::generate().unwrap();
    let acceptor = tls::acceptor(&pc_id).unwrap();
    let connector = tls::connector(&stranger_id).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let accepting = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        tls::accept(&acceptor, tcp).await.unwrap()
    });
    let (mut client, _) = tls::connect(&connector, "127.0.0.1", port).await.unwrap();
    let (server, seen) = accepting.await.unwrap();

    let (handler, mut events, _dir) = side_handler(None, &Options::default());
    let (_handle, commands) = Handle::channel();
    let local = Local { name: "PC".into(), fingerprint: pc_id.fingerprint(), role: Role::Server, timing: Timing::default() };
    let task = tokio::spawn(run(server, local, seen, handler, commands));

    write_frame(&mut client, &hello()).await.unwrap();
    write_frame(&mut client, &Frame::Message(Message::PairCommit { commitment: lis_core::pairing::commitment("real") })).await.unwrap();
    loop {
        if let Frame::Message(Message::PairNonce { .. }) = read_frame(&mut client).await.unwrap() {
            break;
        }
    }
    write_frame(&mut client, &Frame::Message(Message::PairReveal { nonce: "different".into() })).await.unwrap();

    let result = timeout(WAIT, task).await.unwrap().unwrap();
    assert!(matches!(result, Err(SessionError::Protocol(_))), "{result:?}");
    while let Ok(event) = events.try_recv() {
        assert!(!matches!(event, Event::PairCode { .. }), "no code may be shown");
    }
}

#[tokio::test]
async fn messages_flow_both_ways_once_paired() {
    let (mut phone, mut pc) = paired_sides().await;

    let pc_stats = PcStats { hostname: "desk".into(), ..Default::default() };
    pc.handle.send(Command::Send(Message::PcStats(pc_stats.clone()))).await;
    match phone.until(|e| matches!(e, Event::Message { .. })).await {
        Event::Message { message: Message::PcStats(got), .. } => assert_eq!(got, pc_stats),
        other => panic!("{other:?}"),
    }

    let phone_stats = PhoneStats { battery_percent: 61, network: "Wi-Fi".into(), ..Default::default() };
    phone.handle.send(Command::Send(Message::PhoneStats(phone_stats.clone()))).await;
    match pc.until(|e| matches!(e, Event::Message { .. })).await {
        Event::Message { message: Message::PhoneStats(got), peer } => {
            assert_eq!(got, phone_stats);
            assert_eq!(peer.name, "Phone");
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn unpairing_on_one_side_removes_trust_on_both() {
    let (mut phone, mut pc) = paired_sides().await;

    pc.handle.send(Command::Unpair).await;
    phone.until(|e| matches!(e, Event::Unpaired { .. })).await;
    pc.until(|e| matches!(e, Event::Unpaired { .. })).await;
    assert!(phone.handler.trusted.lock().unwrap().is_empty());
    assert!(pc.handler.trusted.lock().unwrap().is_empty());

    phone.handle.send(Command::Send(Message::LockPc)).await;
    pc.nothing_for(Duration::from_millis(200)).await;
}

fn temp_file(contents: &[u8]) -> (std::fs::File, tempfile::NamedTempFile) {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(contents).unwrap();
    (std::fs::File::open(file.path()).unwrap(), file)
}

async fn finished(side: &mut Side) -> bool {
    match side.until(|e| matches!(e, Event::FileFinished { .. })).await {
        Event::FileFinished { ok, .. } => ok,
        _ => unreachable!(),
    }
}

#[tokio::test]
async fn a_large_file_arrives_intact() {
    let (mut phone, mut pc) = paired_sides().await;
    let contents: Vec<u8> = (0..20_000_000u32).map(|i| (i % 251) as u8).collect();
    let (file, _keep) = temp_file(&contents);

    phone.handle.send(Command::SendFile { file, name: "big.bin".into(), size: contents.len() as u64 }).await;
    let (sent, received) = tokio::join!(
        timeout(Duration::from_secs(30), finished(&mut phone)),
        timeout(Duration::from_secs(30), finished(&mut pc)),
    );
    assert!(sent.unwrap());
    assert!(received.unwrap());
    assert_eq!(std::fs::read(pc.dir.path().join("big.bin")).unwrap(), contents);
}

#[tokio::test]
async fn control_messages_overtake_a_transfer() {
    let (phone, mut pc) = paired_sides().await;
    let contents = vec![7u8; 50_000_000];
    let (file, _keep) = temp_file(&contents);

    phone.handle.send(Command::SendFile { file, name: "big.bin".into(), size: contents.len() as u64 }).await;
    phone.handle.send(Command::Send(Message::LockPc)).await;

    loop {
        match pc.next().await {
            Event::Message { message: Message::LockPc, .. } => break,
            Event::FileFinished { .. } => panic!("the lock waited for the whole file"),
            _ => {}
        }
    }
    assert!(timeout(Duration::from_secs(30), finished(&mut pc)).await.unwrap());
}

#[tokio::test]
async fn a_refused_file_is_reported() {
    let (mut phone, mut pc) = connect(Options { trust_each_other: true, accept_files: false, ..Default::default() }).await;
    connected(&mut phone).await;
    connected(&mut pc).await;

    let (file, _keep) = temp_file(b"hello");
    phone.handle.send(Command::SendFile { file, name: "x".into(), size: 5 }).await;
    assert!(!finished(&mut phone).await);
}

#[tokio::test]
async fn a_file_shorter_than_offered_fails() {
    let (mut phone, mut pc) = paired_sides().await;
    let (file, _keep) = temp_file(b"short");

    phone.handle.send(Command::SendFile { file, name: "x".into(), size: 1000 }).await;
    assert!(!finished(&mut pc).await);
    assert!(!finished(&mut phone).await);
}

#[tokio::test]
async fn a_dropped_connection_fails_transfers_on_both_sides() {
    let (phone, mut pc) = paired_sides().await;
    let contents = vec![1u8; 200_000_000];
    let (file, _keep) = temp_file(&contents);

    phone.handle.send(Command::SendFile { file, name: "big.bin".into(), size: contents.len() as u64 }).await;
    pc.until(|e| matches!(e, Event::FileProgress { .. })).await;
    phone.task.abort();

    assert!(!timeout(Duration::from_secs(30), finished(&mut pc)).await.unwrap());
    let result = timeout(Duration::from_secs(5), pc.task).await.unwrap().unwrap();
    assert!(result.is_err());
}

#[tokio::test]
async fn silence_ends_the_session() {
    let timing = Timing {
        ping_every: Duration::from_secs(100),
        silence_limit: Duration::from_millis(300),
        ..Default::default()
    };
    let (phone, pc) = connect(Options { timing, ..Default::default() }).await;
    let result = timeout(Duration::from_secs(5), pc.task).await.unwrap().unwrap();
    assert!(matches!(result, Err(SessionError::Silent)), "{result:?}");
    drop(phone);
}

#[tokio::test]
async fn pings_keep_a_quiet_connection_alive() {
    let timing = Timing {
        ping_every: Duration::from_millis(100),
        silence_limit: Duration::from_millis(400),
        ..Default::default()
    };
    let (phone, pc) = connect(Options { timing, ..Default::default() }).await;
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert!(!pc.task.is_finished() && !phone.task.is_finished());
}

#[tokio::test]
async fn closing_is_clean() {
    let (phone, pc) = paired_sides().await;
    phone.handle.send(Command::Close).await;
    assert!(timeout(Duration::from_secs(5), phone.task).await.unwrap().unwrap().is_ok());
    assert!(timeout(Duration::from_secs(5), pc.task).await.unwrap().unwrap().is_err());
}
