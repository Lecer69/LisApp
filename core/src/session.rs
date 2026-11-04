//! One connection between two devices. Nothing but the hello and pairing messages moves until both
//! sides trust each other, in either direction: incoming data closes the connection, and outgoing
//! data is dropped.

use crate::pairing;
use crate::protocol::{read_frame, write_frame, FileOffer, Frame, Message, CHUNK_LEN, PROTOCOL_VERSION};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{sleep_until, Instant};

const PROGRESS_STEP: u64 = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Role {
    /// Connects, and starts pairing: the phone.
    Client,
    /// Listens, and pairs only while allowed to: the PC.
    Server,
}

#[derive(Clone, Copy, Debug)]
pub struct Timing {
    pub ping_every: Duration,
    pub silence_limit: Duration,
    pub pair_limit: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            ping_every: Duration::from_secs(25),
            silence_limit: Duration::from_secs(75),
            pair_limit: Duration::from_secs(120),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Local {
    pub name: String,
    pub fingerprint: String,
    pub role: Role,
    pub timing: Timing,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Peer {
    pub name: String,
    pub fingerprint: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FileStatus {
    pub peer: Peer,
    pub id: u64,
    pub name: String,
    pub incoming: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Connected { peer: Peer, paired: bool },
    PairCode { peer: Peer, code: String },
    PairResult { peer: Peer, ok: bool, reason: String },
    Unpaired { peer: Peer },
    Message { peer: Peer, message: Message },
    FileProgress { file: FileStatus, done: u64, total: u64 },
    FileFinished { file: FileStatus, ok: bool },
}

pub trait Handler: Send + Sync + 'static {
    fn is_trusted(&self, fingerprint: &str) -> bool;
    /// Asked on the server when a phone starts pairing.
    fn may_pair(&self) -> bool;
    fn trust(&self, peer: &Peer);
    fn forget(&self, fingerprint: &str);
    /// Where an offered file goes; `None` refuses it.
    fn open_incoming(&self, peer: &Peer, offer: &FileOffer) -> Option<std::fs::File>;
    fn event(&self, event: Event);
}

#[derive(Debug, Error)]
pub enum SessionError {
    #[error("connection lost: {0}")]
    Io(#[from] std::io::Error),
    #[error("protocol violation: {0}")]
    Protocol(String),
    #[error("nothing heard from the other device")]
    Silent,
}

pub enum Command {
    Send(Message),
    StartPairing,
    ConfirmPairing(bool),
    Unpair,
    SendFile { file: std::fs::File, name: String, size: u64 },
    Close,
}

#[derive(Clone)]
pub struct Handle(mpsc::Sender<Command>);

impl Handle {
    pub fn channel() -> (Self, mpsc::Receiver<Command>) {
        let (tx, rx) = mpsc::channel(64);
        (Self(tx), rx)
    }

    pub async fn send(&self, command: Command) -> bool {
        self.0.send(command).await.is_ok()
    }

    pub fn is_closed(&self) -> bool {
        self.0.is_closed()
    }
}

enum Pairing {
    Idle,
    Committed { nonce: String },
    Answered { commitment: String, nonce: String },
    Comparing { local: Option<bool>, remote: Option<bool> },
}

struct Incoming {
    offer: FileOffer,
    file: tokio::fs::File,
    received: u64,
    reported: u64,
}

struct Outgoing {
    name: String,
    size: u64,
    file: Option<std::fs::File>,
    task: Option<JoinHandle<()>>,
}

struct Session {
    local: Local,
    peer_fingerprint: String,
    handler: Arc<dyn Handler>,
    control: mpsc::Sender<Frame>,
    data: mpsc::Sender<Frame>,
    peer: Option<Peer>,
    local_trust: bool,
    peer_trust: Option<bool>,
    pairing: Pairing,
    pair_deadline: Option<Instant>,
    incoming: HashMap<u64, Incoming>,
    outgoing: HashMap<u64, Outgoing>,
    next_file_id: u64,
}

impl Session {
    fn active(&self) -> bool {
        self.local_trust && self.peer_trust == Some(true)
    }

    fn peer(&self) -> Result<Peer, SessionError> {
        self.peer.clone().ok_or_else(|| violation("message before hello"))
    }

    async fn send(&self, message: Message) -> Result<(), SessionError> {
        self.control
            .send(Frame::Message(message))
            .await
            .map_err(|_| SessionError::Io(std::io::ErrorKind::BrokenPipe.into()))
    }

    fn fail_pairing(&mut self, reason: &str) {
        self.pairing = Pairing::Idle;
        self.pair_deadline = None;
        if let Some(peer) = self.peer.clone() {
            self.handler.event(Event::PairResult { peer, ok: false, reason: reason.to_string() });
        }
    }

    fn fingerprints(&self) -> (&str, &str) {
        match self.local.role {
            Role::Client => (&self.local.fingerprint, &self.peer_fingerprint),
            Role::Server => (&self.peer_fingerprint, &self.local.fingerprint),
        }
    }

    fn show_code(&mut self, client_nonce: &str, server_nonce: &str) -> Result<(), SessionError> {
        let (client_fp, server_fp) = self.fingerprints();
        let code = pairing::code(client_fp, server_fp, client_nonce, server_nonce);
        self.pairing = Pairing::Comparing { local: None, remote: None };
        self.handler.event(Event::PairCode { peer: self.peer()?, code });
        Ok(())
    }

    /// Trust needs both people to have accepted the same code.
    fn settle_pairing(&mut self) -> Result<(), SessionError> {
        let Pairing::Comparing { local, remote } = self.pairing else {
            return Ok(());
        };
        if local == Some(false) || remote == Some(false) {
            self.fail_pairing("Pairing was declined");
        } else if local == Some(true) && remote == Some(true) {
            let peer = self.peer()?;
            self.handler.trust(&peer);
            self.local_trust = true;
            self.peer_trust = Some(true);
            self.pairing = Pairing::Idle;
            self.pair_deadline = None;
            self.handler.event(Event::PairResult { peer: peer.clone(), ok: true, reason: String::new() });
            self.handler.event(Event::Connected { peer, paired: true });
        }
        Ok(())
    }

    async fn on_command(&mut self, command: Command) -> Result<bool, SessionError> {
        match command {
            Command::Close => return Ok(false),
            Command::Send(message) => {
                // Data never leaves for a device that isn't trusted
                if self.active() || message.allowed_unpaired() {
                    self.send(message).await?;
                }
            }
            Command::StartPairing => self.start_pairing().await?,
            Command::ConfirmPairing(accept) => {
                if let Pairing::Comparing { local, .. } = &mut self.pairing {
                    if local.is_none() {
                        *local = Some(accept);
                        self.send(Message::PairConfirm { accept }).await?;
                        self.settle_pairing()?;
                    }
                }
            }
            Command::Unpair => {
                if self.active() {
                    self.send(Message::Unpair).await?;
                }
                self.handler.forget(&self.peer_fingerprint);
                self.local_trust = false;
                if let Some(peer) = self.peer.clone() {
                    self.handler.event(Event::Unpaired { peer });
                }
            }
            Command::SendFile { file, name, size } => {
                if self.active() {
                    let id = self.next_file_id;
                    self.next_file_id += 1;
                    self.outgoing.insert(id, Outgoing { name: name.clone(), size, file: Some(file), task: None });
                    self.send(Message::FileOffer(FileOffer { id, name, size })).await?;
                } else if let Some(peer) = self.peer.clone() {
                    let file = FileStatus { peer, id: 0, name, incoming: false };
                    self.handler.event(Event::FileFinished { file, ok: false });
                }
            }
        }
        Ok(true)
    }

    async fn start_pairing(&mut self) -> Result<(), SessionError> {
        if self.local.role != Role::Client || self.peer_trust.is_none() || self.active() {
            return Ok(());
        }
        let nonce = pairing::new_nonce();
        let commitment = pairing::commitment(&nonce);
        self.pairing = Pairing::Committed { nonce };
        self.pair_deadline = Some(Instant::now() + self.local.timing.pair_limit);
        self.send(Message::PairCommit { commitment }).await
    }

    async fn on_frame(&mut self, frame: Frame) -> Result<(), SessionError> {
        match frame {
            Frame::Message(message) => self.on_message(message).await,
            Frame::Chunk { id, data } => self.on_chunk(id, data).await,
        }
    }

    async fn on_message(&mut self, message: Message) -> Result<(), SessionError> {
        if !message.allowed_unpaired() && !self.active() {
            return Err(violation("data before pairing"));
        }

        match message {
            Message::Hello { protocol, name } => {
                if protocol != PROTOCOL_VERSION {
                    return Err(violation(&format!("protocol {protocol}, this device speaks {PROTOCOL_VERSION}")));
                }
                if self.peer.is_some() {
                    return Err(violation("second hello"));
                }
                self.peer = Some(Peer { name, fingerprint: self.peer_fingerprint.clone() });
                self.local_trust = self.handler.is_trusted(&self.peer_fingerprint);
                self.send(Message::Auth { trusted: self.local_trust }).await?;
            }
            Message::Auth { trusted } => {
                let peer = self.peer()?;
                self.peer_trust = Some(trusted);
                self.handler.event(Event::Connected { peer, paired: self.active() });
            }
            Message::Ping => self.send(Message::Pong).await?,
            Message::Pong | Message::Unknown => {}
            Message::PairCommit { commitment } => {
                self.peer()?;
                if self.local.role != Role::Server {
                    return Err(violation("pair commit sent to a client"));
                }
                if self.active() {
                    return Ok(());
                }
                if !self.handler.may_pair() {
                    return self.send(Message::PairDenied { reason: "Open pairing on the PC first".into() }).await;
                }
                let nonce = pairing::new_nonce();
                self.pairing = Pairing::Answered { commitment, nonce: nonce.clone() };
                self.pair_deadline = Some(Instant::now() + self.local.timing.pair_limit);
                self.send(Message::PairNonce { nonce }).await?;
            }
            Message::PairNonce { nonce: server_nonce } => {
                let Pairing::Committed { nonce } = &self.pairing else {
                    return Err(violation("unexpected pair nonce"));
                };
                let nonce = nonce.clone();
                self.send(Message::PairReveal { nonce: nonce.clone() }).await?;
                self.show_code(&nonce, &server_nonce)?;
            }
            Message::PairReveal { nonce: client_nonce } => {
                let Pairing::Answered { commitment, nonce } = &self.pairing else {
                    return Err(violation("unexpected pair reveal"));
                };
                if pairing::commitment(&client_nonce) != *commitment {
                    return Err(violation("pair reveal doesn't match the commitment"));
                }
                let nonce = nonce.clone();
                self.show_code(&client_nonce, &nonce)?;
            }
            Message::PairDenied { reason } => self.fail_pairing(&reason),
            Message::PairConfirm { accept } => {
                match &mut self.pairing {
                    Pairing::Comparing { remote, .. } => {
                        *remote = Some(accept);
                        self.settle_pairing()?;
                    }
                    Pairing::Idle => {}
                    _ if !accept => self.fail_pairing("Pairing was declined"),
                    _ => return Err(violation("pair confirm before the code")),
                }
            }
            Message::Unpair => {
                let peer = self.peer()?;
                self.handler.forget(&self.peer_fingerprint);
                self.local_trust = false;
                self.handler.event(Event::Unpaired { peer });
            }
            Message::FileOffer(offer) => self.on_offer(offer).await?,
            Message::FileAccept { id } => self.on_accept(id)?,
            Message::FileReject { id } => self.finish_outgoing(id, false)?,
            Message::FileDone { id } => self.finish_incoming(id, true).await?,
            Message::FileAbort { id } => self.finish_incoming(id, false).await?,
            Message::FileAck { id, ok } => self.finish_outgoing(id, ok)?,
            message => self.handler.event(Event::Message { peer: self.peer()?, message }),
        }
        Ok(())
    }

    async fn on_offer(&mut self, offer: FileOffer) -> Result<(), SessionError> {
        if self.incoming.contains_key(&offer.id) {
            return Err(violation("file id in use"));
        }
        let id = offer.id;
        let peer = self.peer()?;
        match self.handler.open_incoming(&peer, &offer) {
            Some(file) => {
                let file = tokio::fs::File::from_std(file);
                self.incoming.insert(id, Incoming { offer, file, received: 0, reported: 0 });
                self.send(Message::FileAccept { id }).await
            }
            None => self.send(Message::FileReject { id }).await,
        }
    }

    fn on_accept(&mut self, id: u64) -> Result<(), SessionError> {
        let peer = self.peer()?;
        let Some(outgoing) = self.outgoing.get_mut(&id) else {
            return Err(violation("accept for an unknown file"));
        };
        let Some(file) = outgoing.file.take() else {
            return Err(violation("file accepted twice"));
        };
        let status = FileStatus { peer, id, name: outgoing.name.clone(), incoming: false };
        outgoing.task = Some(tokio::spawn(send_file(
            file,
            outgoing.size,
            status,
            self.data.clone(),
            self.handler.clone(),
        )));
        Ok(())
    }

    fn finish_outgoing(&mut self, id: u64, ok: bool) -> Result<(), SessionError> {
        let peer = self.peer()?;
        let Some(outgoing) = self.outgoing.remove(&id) else {
            return Err(violation("answer for an unknown file"));
        };
        let file = FileStatus { peer, id, name: outgoing.name, incoming: false };
        self.handler.event(Event::FileFinished { file, ok });
        Ok(())
    }

    async fn on_chunk(&mut self, id: u64, data: Vec<u8>) -> Result<(), SessionError> {
        if !self.active() {
            return Err(violation("file data before pairing"));
        }
        let peer = self.peer()?;
        let Some(incoming) = self.incoming.get_mut(&id) else {
            return Err(violation("chunk for an unknown file"));
        };
        incoming.received += data.len() as u64;
        if incoming.received > incoming.offer.size {
            return Err(violation("more file data than offered"));
        }
        incoming.file.write_all(&data).await?;

        if incoming.received - incoming.reported >= PROGRESS_STEP {
            incoming.reported = incoming.received;
            let file = FileStatus { peer, id, name: incoming.offer.name.clone(), incoming: true };
            let (done, total) = (incoming.received, incoming.offer.size);
            self.handler.event(Event::FileProgress { file, done, total });
        }
        Ok(())
    }

    async fn finish_incoming(&mut self, id: u64, done: bool) -> Result<(), SessionError> {
        let peer = self.peer()?;
        let Some(mut incoming) = self.incoming.remove(&id) else {
            return Err(violation("end of an unknown file"));
        };
        let ok = done && incoming.received == incoming.offer.size && incoming.file.flush().await.is_ok();
        drop(incoming.file);
        self.send(Message::FileAck { id, ok }).await?;
        let file = FileStatus { peer, id, name: incoming.offer.name, incoming: true };
        self.handler.event(Event::FileFinished { file, ok });
        Ok(())
    }

    /// Every unfinished transfer is reported, so partial files can be cleaned up.
    fn finish_all(&mut self) {
        let Some(peer) = self.peer.clone() else { return };
        for (id, incoming) in self.incoming.drain() {
            let file = FileStatus { peer: peer.clone(), id, name: incoming.offer.name, incoming: true };
            self.handler.event(Event::FileFinished { file, ok: false });
        }
        for (id, outgoing) in self.outgoing.drain() {
            if let Some(task) = outgoing.task {
                task.abort();
            }
            let file = FileStatus { peer: peer.clone(), id, name: outgoing.name, incoming: false };
            self.handler.event(Event::FileFinished { file, ok: false });
        }
    }
}

fn violation(what: &str) -> SessionError {
    SessionError::Protocol(what.to_string())
}

async fn send_file(
    file: std::fs::File,
    size: u64,
    status: FileStatus,
    data: mpsc::Sender<Frame>,
    handler: Arc<dyn Handler>,
) {
    let id = status.id;
    let mut file = tokio::fs::File::from_std(file);
    let mut buffer = vec![0u8; CHUNK_LEN];
    let (mut sent, mut reported) = (0u64, 0u64);

    let intact = loop {
        let count = match file.read(&mut buffer).await {
            Ok(0) => break sent == size,
            Ok(count) => count,
            Err(_) => break false,
        };
        if data.send(Frame::Chunk { id, data: buffer[..count].to_vec() }).await.is_err() {
            return;
        }
        sent += count as u64;
        if sent - reported >= PROGRESS_STEP {
            reported = sent;
            handler.event(Event::FileProgress { file: status.clone(), done: sent, total: size });
        }
    };

    let end = if intact { Message::FileDone { id } } else { Message::FileAbort { id } };
    let _ = data.send(Frame::Message(end)).await;
}

async fn write_frames<W: AsyncWrite + Unpin>(
    mut writer: W,
    mut control: mpsc::Receiver<Frame>,
    mut data: mpsc::Receiver<Frame>,
) {
    loop {
        // Control first, so a big transfer doesn't hold up pings or a lock
        let frame = tokio::select! {
            biased;
            frame = control.recv() => match frame {
                Some(frame) => frame,
                None => break,
            },
            Some(frame) = data.recv() => frame,
        };
        if write_frame(&mut writer, &frame).await.is_err() {
            return;
        }
    }
    let _ = writer.shutdown().await;
}

/// Runs until the connection ends. `peer_fingerprint` comes from the TLS handshake.
pub async fn run<S>(
    stream: S,
    local: Local,
    peer_fingerprint: String,
    handler: Arc<dyn Handler>,
    mut commands: mpsc::Receiver<Command>,
) -> Result<(), SessionError>
where
    S: AsyncRead + AsyncWrite + Send + 'static,
{
    let (mut reader, writer) = tokio::io::split(stream);
    let (control_tx, control_rx) = mpsc::channel(256);
    let (data_tx, data_rx) = mpsc::channel(16);
    let mut writer_task = tokio::spawn(write_frames(writer, control_rx, data_rx));

    // In its own task, as a read can't be cancelled midway through a frame
    let (frames_tx, mut frames_rx) = mpsc::channel(16);
    let reader_task = tokio::spawn(async move {
        loop {
            let frame = read_frame(&mut reader).await;
            let stop = frame.is_err();
            if frames_tx.send(frame).await.is_err() || stop {
                break;
            }
        }
    });

    let timing = local.timing;
    let name = local.name.clone();
    let mut session = Session {
        local,
        peer_fingerprint,
        handler,
        control: control_tx,
        data: data_tx,
        peer: None,
        local_trust: false,
        peer_trust: None,
        pairing: Pairing::Idle,
        pair_deadline: None,
        incoming: HashMap::new(),
        outgoing: HashMap::new(),
        next_file_id: 1,
    };

    let mut ping = tokio::time::interval_at(Instant::now() + timing.ping_every, timing.ping_every);
    let mut last_heard = Instant::now();

    let result = async {
        session.send(Message::Hello { protocol: PROTOCOL_VERSION, name }).await?;
        loop {
            let pair_deadline = session.pair_deadline;
            tokio::select! {
                frame = frames_rx.recv() => {
                    let frame = frame.ok_or_else(|| SessionError::Io(std::io::ErrorKind::BrokenPipe.into()))??;
                    last_heard = Instant::now();
                    session.on_frame(frame).await?;
                }
                command = commands.recv() => {
                    match command {
                        Some(command) => {
                            if !session.on_command(command).await? {
                                return Ok(());
                            }
                        }
                        None => return Ok(()),
                    }
                }
                _ = ping.tick() => session.send(Message::Ping).await?,
                _ = sleep_until(last_heard + timing.silence_limit) => return Err(SessionError::Silent),
                _ = sleep_until(pair_deadline.unwrap_or_else(Instant::now)), if pair_deadline.is_some() => {
                    session.send(Message::PairConfirm { accept: false }).await?;
                    session.fail_pairing("Pairing timed out");
                }
            }
        }
    }
    .await;

    session.finish_all();
    reader_task.abort();
    let Session { control, data, .. } = session;
    drop((control, data));
    // Lets what's queued go out, such as a last unpair
    if tokio::time::timeout(Duration::from_secs(1), &mut writer_task).await.is_err() {
        writer_task.abort();
    }
    result
}
