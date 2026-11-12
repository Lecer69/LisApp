//! What the Kotlin app calls. It owns one connection to one PC: Kotlin supplies the Android parts
//! (addresses from discovery, phone data, file descriptors) and hears back through the listener.

mod bitrate;

use bitrate::BitrateGovernor;
use lis_core::identity::{write_private, Identity};
use lis_core::names::safe_file_name;
use lis_core::protocol::{self, FileOffer, MediaAction, Message};
use lis_core::session::{self, Command, Event, Handle, Handler, Local, Peer, Role, Timing};
use lis_core::sync;
use lis_core::tls;
use lis_core::unlock;
use lis_core::video::{read_packet_async, write_packet_async, Packet, VideoFrame, VideoInfo, CODEC_H264};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::fs::{self, File};
use std::os::fd::FromRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::runtime::Runtime;
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{mpsc, Notify};
use tokio::task::JoinHandle;
use tokio_rustls::TlsConnector;

uniffi::setup_scaffolding!();

const MAX_RETRY_DELAY: Duration = Duration::from_secs(60);
const MAX_VIDEO_RETRY_DELAY: Duration = Duration::from_secs(8);
/// About 130 ms at 60 fps. Beyond that, frames are dropped rather than the delay growing.
const VIDEO_QUEUE: usize = 8;
const VIDEO_WRITE_LIMIT: Duration = Duration::from_secs(3);

#[derive(uniffi::Error, thiserror::Error, Debug)]
#[uniffi(flat_error)]
pub enum LisError {
    #[error("{0}")]
    Failed(String),
}

fn failed(error: impl std::fmt::Display) -> LisError {
    LisError::Failed(error.to_string())
}

#[derive(uniffi::Enum, Debug, Clone, Copy, PartialEq)]
pub enum ConnectionState {
    Disconnected,
    Connecting,
    /// Connected, and the PC isn't paired yet.
    Unpaired,
    Connected,
    /// Something else answers at this address, not the paired PC.
    WrongPc,
}

#[derive(uniffi::Enum, Debug, Clone, PartialEq)]
pub enum VideoState {
    Connecting,
    Live,
    /// It stopped trying, as retrying won't help.
    Refused { reason: String },
}

#[derive(uniffi::Record, Debug, Clone, PartialEq)]
pub struct VideoConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate: u32,
    /// Degrees clockwise to show the frames upright.
    pub rotation: u32,
    pub camera: String,
}

impl From<VideoConfig> for VideoInfo {
    fn from(config: VideoConfig) -> Self {
        Self {
            codec: CODEC_H264.to_string(),
            width: config.width,
            height: config.height,
            fps: config.fps,
            bitrate: config.bitrate,
            rotation: config.rotation,
            camera: config.camera,
        }
    }
}

#[derive(uniffi::Record, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PairedPc {
    pub fingerprint: String,
    pub name: String,
    pub host: String,
    pub port: u16,
}

#[derive(uniffi::Record, Debug, Clone, PartialEq)]
pub struct PhoneStats {
    pub battery_percent: u8,
    pub charging: bool,
    pub storage_used: u64,
    pub storage_total: u64,
    pub memory_used: u64,
    pub memory_total: u64,
    pub network: String,
}

#[derive(uniffi::Record, Debug, Clone, PartialEq)]
pub struct PcStats {
    pub hostname: String,
    pub cpu_percent: f32,
    pub memory_used: u64,
    pub memory_total: u64,
    pub uptime_secs: u64,
}

#[derive(uniffi::Record, Debug, Clone, PartialEq)]
pub struct MediaInfo {
    pub player: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub playing: bool,
    pub position_ms: u64,
    pub length_ms: u64,
    pub volume_percent: u8,
    pub muted: bool,
}

#[derive(uniffi::Enum, Debug, Clone, Copy, PartialEq)]
pub enum MediaCommand {
    PlayPause,
    Next,
    Previous,
}

#[derive(uniffi::Record, Debug, Clone, PartialEq)]
pub struct ActionButton {
    pub id: String,
    pub name: String,
}

/// A document after an edit.
#[derive(uniffi::Record, Debug, Clone, PartialEq)]
pub struct SyncEdited {
    pub doc: String,
    /// Differs from the edit, as something had arrived in the meantime.
    pub merged: bool,
}

/// What the encoder should do after a frame.
#[derive(uniffi::Record, Debug, Clone, PartialEq)]
pub struct VideoFeedback {
    pub need_keyframe: bool,
    pub bitrate: Option<u32>,
}

/// Called from Rust's threads. None of these may block for long.
#[uniffi::export(callback_interface)]
pub trait ClientListener: Send + Sync {
    fn on_state(&self, state: ConnectionState, pc_name: Option<String>);
    fn on_pair_code(&self, code: String);
    fn on_pair_result(&self, ok: bool, reason: String);
    fn on_pc_stats(&self, stats: PcStats);
    /// A writable file descriptor the caller gives up, or -1 to refuse.
    fn open_incoming_file(&self, id: u64, name: String, size: u64) -> i32;
    fn on_file_progress(&self, id: u64, name: String, incoming: bool, done: u64, total: u64);
    fn on_file_finished(&self, id: u64, name: String, incoming: bool, ok: bool);
    fn on_video_state(&self, state: VideoState);
    /// How many viewers on the PC are watching the camera.
    fn on_video_viewers(&self, count: u32);
    fn on_ring(&self, ring: bool);
    /// The document changed: read it again with `sync_doc`.
    fn on_sync_changed(&self, kind: String);
    fn on_sync_error(&self, kind: String, reason: String);
    fn on_media(&self, media: Option<MediaInfo>);
    /// The cover image, empty for none.
    fn on_media_art(&self, data: Vec<u8>);
    fn on_actions(&self, actions: Vec<ActionButton>);
    fn on_action_result(&self, id: String, ok: bool);
    fn on_unlock_state(&self, locked: bool, available: bool);
    /// Someone is at the locked PC, so show the fingerprint prompt.
    fn on_unlock_prompt(&self);
    /// Sign `unlock_message` of this, then answer with `unlock_respond`.
    fn on_unlock_challenge(&self, nonce: String);
    fn on_unlock_result(&self, ok: bool, reason: String);
    /// The PC asks for a photo, which only the running camera can take.
    fn on_snap_request(&self);
}

struct VideoLink {
    frames: mpsc::Sender<VideoFrame>,
    /// Frames are dropped until the next keyframe, which is all a decoder can restart from.
    skip: Arc<AtomicBool>,
    info: Arc<Mutex<VideoInfo>>,
    info_changed: Arc<Notify>,
    governor: Arc<Mutex<BitrateGovernor>>,
    task: JoinHandle<()>,
}

/// One document as this phone has it. `base` is the PC's copy as last heard, which merging compares
/// both sides to.
#[derive(Serialize, Deserialize)]
struct SyncSide {
    base: Value,
    local: Value,
    #[serde(skip)]
    rev: Option<String>,
    /// Sent and not yet answered. Only one goes at a time, as a second would be based on a copy the
    /// PC has already moved on from.
    #[serde(skip)]
    pushed: Option<Value>,
}

impl SyncSide {
    fn new(kind: &str) -> Self {
        Self { base: sync::empty(kind), local: sync::empty(kind), rev: None, pushed: None }
    }
}

enum VideoEnd {
    Retry,
    Refused(String),
}

#[derive(Default)]
struct State {
    pc: Option<PairedPc>,
    target: Option<(String, u16)>,
    handle: Option<Handle>,
    task: Option<JoinHandle<()>>,
    connection_state: Option<ConnectionState>,
    video: Option<VideoLink>,
}

struct Inner {
    identity: Identity,
    connector: TlsConnector,
    video_connector: TlsConnector,
    name: String,
    pc_path: PathBuf,
    sync_dir: PathBuf,
    listener: Box<dyn ClientListener>,
    state: Mutex<State>,
    sync: Mutex<HashMap<String, SyncSide>>,
    runtime: tokio::runtime::Handle,
    wake: Notify,
}

impl Inner {
    fn set_state(&self, new: ConnectionState, pc_name: Option<String>) {
        self.state.lock().unwrap().connection_state = Some(new);
        self.listener.on_state(new, pc_name);
    }

    /// Reconnects for as long as the target stands, backing off while the PC can't be reached.
    async fn connection_loop(self: Arc<Self>, host: String, port: u16) {
        let mut delay = Duration::from_secs(1);
        loop {
            self.set_state(ConnectionState::Connecting, None);
            match tls::connect(&self.connector, &host, port).await {
                Ok((stream, peer)) => {
                    let paired_elsewhere = self.state.lock().unwrap().pc.as_ref().is_some_and(|pc| pc.fingerprint != peer);
                    if paired_elsewhere {
                        // Not the paired PC, whatever it calls itself: nothing is said to it
                        self.set_state(ConnectionState::WrongPc, None);
                    } else {
                        let (handle, commands) = Handle::channel();
                        self.state.lock().unwrap().handle = Some(handle);
                        let local = Local {
                            name: self.name.clone(),
                            fingerprint: self.identity.fingerprint(),
                            role: Role::Client,
                            timing: Timing::default(),
                        };
                        let handler: Arc<dyn Handler> = self.clone();
                        let _ = session::run(stream, local, peer, handler, commands).await;
                        self.state.lock().unwrap().handle = None;
                        delay = Duration::from_secs(1);
                        self.set_state(ConnectionState::Disconnected, None);
                    }
                }
                Err(_) => self.set_state(ConnectionState::Disconnected, None),
            }

            tokio::select! {
                _ = tokio::time::sleep(delay) => {}
                _ = self.wake.notified() => delay = Duration::from_secs(1),
            }
            delay = (delay * 2).min(MAX_RETRY_DELAY);
        }
    }

    /// Reconnects while the camera runs, until it's stopped or refused.
    async fn video_loop(
        self: Arc<Self>,
        mut frames: mpsc::Receiver<VideoFrame>,
        info: Arc<Mutex<VideoInfo>>,
        info_changed: Arc<Notify>,
        skip: Arc<AtomicBool>,
    ) {
        let mut delay = Duration::from_secs(1);
        loop {
            self.listener.on_video_state(VideoState::Connecting);
            match self.video_session(&mut frames, &info, &info_changed, &mut delay).await {
                VideoEnd::Refused(reason) => {
                    self.listener.on_video_state(VideoState::Refused { reason });
                    return;
                }
                VideoEnd::Retry => {
                    // The decoder missed frames, so it has to restart from a keyframe
                    skip.store(true, Ordering::SeqCst);
                    while frames.try_recv().is_ok() {}
                    tokio::time::sleep(delay).await;
                    delay = (delay * 2).min(MAX_VIDEO_RETRY_DELAY);
                }
            }
        }
    }

    async fn video_session(
        self: &Arc<Self>,
        frames: &mut mpsc::Receiver<VideoFrame>,
        info: &Mutex<VideoInfo>,
        info_changed: &Notify,
        delay: &mut Duration,
    ) -> VideoEnd {
        let (target, paired) = {
            let state = self.state.lock().unwrap();
            (state.target.clone(), state.pc.as_ref().map(|pc| pc.fingerprint.clone()))
        };
        let Some(paired) = paired else {
            return VideoEnd::Refused("Not paired with a PC".to_string());
        };
        let Some((host, port)) = target else { return VideoEnd::Retry };

        let Ok((stream, peer)) = tls::connect(&self.video_connector, &host, port).await else {
            return VideoEnd::Retry;
        };
        if peer != paired {
            return VideoEnd::Refused("Another device answered instead of your PC".to_string());
        }
        if !tls::server_is_video(&stream) {
            return VideoEnd::Refused("Update lis-daemon on the PC".to_string());
        }

        let (mut reader, mut writer) = tokio::io::split(stream);
        let first = info.lock().unwrap().clone();
        if write_packet_async(&mut writer, &Packet::Info(first)).await.is_err() {
            return VideoEnd::Retry;
        }
        *delay = Duration::from_secs(1);
        self.listener.on_video_state(VideoState::Live);

        let inner = self.clone();
        let mut viewers = tokio::spawn(async move {
            while let Ok(Packet::Viewers(count)) = read_packet_async(&mut reader).await {
                inner.listener.on_video_viewers(count);
            }
        });

        loop {
            let packet = tokio::select! {
                frame = frames.recv() => match frame {
                    Some(frame) => Packet::Frame(frame),
                    None => break,
                },
                _ = info_changed.notified() => {
                    let current = info.lock().unwrap().clone();
                    Packet::Info(current)
                },
                _ = &mut viewers => break,
            };
            let written = tokio::time::timeout(VIDEO_WRITE_LIMIT, write_packet_async(&mut writer, &packet)).await;
            if !matches!(written, Ok(Ok(()))) {
                break;
            }
        }
        viewers.abort();
        VideoEnd::Retry
    }

    /// Only once the PC has proved itself, as anyone on the network can announce an address. Not the
    /// USB tunnel's, which is no use for finding the PC again without the cable.
    fn remember_address(&self) {
        let mut state = self.state.lock().unwrap();
        let Some((host, port)) = state.target.clone() else { return };
        if let Some(pc) = &mut state.pc {
            if is_loopback(&host) && !is_loopback(&pc.host) {
                return;
            }
            if pc.host != host || pc.port != port {
                pc.host = host;
                pc.port = port;
                let pc = pc.clone();
                self.save_pc(&pc);
            }
        }
    }

    fn save_pc(&self, pc: &PairedPc) {
        match serde_json::to_vec(pc) {
            Ok(bytes) => {
                if let Err(error) = write_private(&self.pc_path, &bytes) {
                    eprintln!("lis: can't save the paired PC: {error}");
                }
            }
            Err(error) => eprintln!("lis: can't encode the paired PC: {error}"),
        }
    }

    fn send(&self, command: Command) {
        let handle = self.state.lock().unwrap().handle.clone();
        if let Some(handle) = handle {
            self.runtime.spawn(async move { handle.send(command).await });
        }
    }

    fn send_message(&self, message: Message) {
        self.send(Command::Send(message));
    }

    fn sync_path(&self, kind: &str) -> PathBuf {
        self.sync_dir.join(format!("{kind}.json"))
    }

    fn save_side(&self, kind: &str, side: &SyncSide) {
        let saved = serde_json::to_vec(side).map_err(std::io::Error::other).and_then(|bytes| {
            fs::create_dir_all(&self.sync_dir)?;
            write_private(&self.sync_path(kind), &bytes)
        });
        if let Err(error) = saved {
            eprintln!("lis: can't save {kind}: {error}");
        }
    }

    /// Sends the document if nothing's waiting for an answer. Otherwise the answer sends it.
    fn push_if_idle(&self, kind: &str, side: &mut SyncSide) {
        let Some(rev) = side.rev.clone() else { return };
        if side.pushed.is_some() || side.local == side.base {
            return;
        }
        if let Err(reason) = sync::check(kind, &side.local) {
            self.listener.on_sync_error(kind.to_string(), reason);
            return;
        }
        side.pushed = Some(side.local.clone());
        self.send_message(Message::SyncPush { kind: kind.to_string(), based_on: rev, doc: side.local.clone() });
    }

    /// Merges the PC's copy into this one, and sends back anything it's missing.
    fn on_sync_doc(&self, kind: String, rev: String, remote: Value) {
        if sync::file_name(&kind).is_none() {
            return;
        }
        let changed = {
            let mut docs = self.sync.lock().unwrap();
            let side = docs.entry(kind.clone()).or_insert_with(|| SyncSide::new(&kind));
            // The PC took what was sent, so that's now common ground, and only newer edits differ
            let base = if side.pushed.as_ref() == Some(&remote) { &remote } else { &side.base };
            let merged = sync::merge(&kind, base, &side.local, &remote);
            let changed = merged != side.local;
            side.local = merged;
            side.base = remote;
            side.rev = Some(rev);
            side.pushed = None;
            self.save_side(&kind, side);
            self.push_if_idle(&kind, side);
            changed
        };
        if changed {
            self.listener.on_sync_changed(kind);
        }
    }

    /// Stays as it is until the PC's copy changes or the connection starts over, rather than sending
    /// the same refused document again and again.
    fn on_sync_rejected(&self, kind: String, reason: String) {
        self.listener.on_sync_error(kind, reason);
    }

    fn forget_sync(&self) {
        self.sync.lock().unwrap().clear();
        let _ = fs::remove_dir_all(&self.sync_dir);
    }
}

fn is_loopback(host: &str) -> bool {
    host.parse::<std::net::IpAddr>().is_ok_and(|address| address.is_loopback()) || host == "localhost"
}

impl Handler for Inner {
    fn is_trusted(&self, fingerprint: &str) -> bool {
        self.state.lock().unwrap().pc.as_ref().is_some_and(|pc| pc.fingerprint == fingerprint)
    }

    fn may_pair(&self) -> bool {
        false
    }

    fn trust(&self, peer: &Peer) {
        let mut state = self.state.lock().unwrap();
        let (host, port) = state.target.clone().unwrap_or_default();
        let pc = PairedPc { fingerprint: peer.fingerprint.clone(), name: peer.name.clone(), host, port };
        self.save_pc(&pc);
        state.pc = Some(pc);
    }

    fn forget(&self, fingerprint: &str) {
        let mut state = self.state.lock().unwrap();
        if state.pc.as_ref().is_some_and(|pc| pc.fingerprint == fingerprint) {
            state.pc = None;
            let _ = fs::remove_file(&self.pc_path);
        }
        let video = state.video.take();
        drop(state);
        // Another PC's documents would otherwise merge into these
        self.forget_sync();
        if let Some(video) = video {
            video.task.abort();
            self.listener.on_video_state(VideoState::Refused { reason: "This phone is no longer paired".to_string() });
        }
    }

    fn open_incoming(&self, _peer: &Peer, offer: &FileOffer) -> Option<File> {
        let fd = self.listener.open_incoming_file(offer.id, safe_file_name(&offer.name), offer.size);
        // The listener handed this descriptor over, so it's ours to close
        (fd >= 0).then(|| unsafe { File::from_raw_fd(fd) })
    }

    fn event(&self, event: Event) {
        match event {
            Event::Connected { peer, paired } => {
                if paired {
                    self.remember_address();
                }
                let state = if paired { ConnectionState::Connected } else { ConnectionState::Unpaired };
                self.set_state(state, Some(peer.name));
            }
            Event::PairCode { code, .. } => self.listener.on_pair_code(code),
            Event::PairResult { ok, reason, .. } => self.listener.on_pair_result(ok, reason),
            Event::Unpaired { peer } => self.set_state(ConnectionState::Unpaired, Some(peer.name)),
            Event::Message { message: Message::PcStats(stats), .. } => self.listener.on_pc_stats(PcStats {
                hostname: stats.hostname,
                cpu_percent: stats.cpu_percent,
                memory_used: stats.memory_used,
                memory_total: stats.memory_total,
                uptime_secs: stats.uptime_secs,
            }),
            Event::Message { message, .. } => self.on_message(message),
            Event::FileProgress { file, done, total } => {
                self.listener.on_file_progress(file.id, file.name, file.incoming, done, total);
            }
            Event::FileFinished { file, ok } => self.listener.on_file_finished(file.id, file.name, file.incoming, ok),
        }
    }
}

impl Inner {
    fn on_message(&self, message: Message) {
        match message {
            Message::RingPhone { ring } => self.listener.on_ring(ring),
            Message::SyncDoc { kind, rev, doc } => self.on_sync_doc(kind, rev, doc),
            Message::SyncRejected { kind, reason } => self.on_sync_rejected(kind, reason),
            Message::MediaState(media) => self.listener.on_media(Some(MediaInfo {
                player: media.player,
                title: media.title,
                artist: media.artist,
                album: media.album,
                playing: media.playing,
                position_ms: media.position_ms,
                length_ms: media.length_ms,
                volume_percent: media.volume_percent,
                muted: media.muted,
            })),
            Message::MediaGone => self.listener.on_media(None),
            Message::MediaArt { data } => self.listener.on_media_art(hex::decode(data).unwrap_or_default()),
            Message::Actions { actions } => {
                self.listener.on_actions(actions.into_iter().map(|action| ActionButton { id: action.id, name: action.name }).collect());
            }
            Message::ActionResult { id, ok } => self.listener.on_action_result(id, ok),
            Message::UnlockState { locked, available } => self.listener.on_unlock_state(locked, available),
            Message::UnlockPrompt => self.listener.on_unlock_prompt(),
            Message::UnlockChallenge { nonce } => self.listener.on_unlock_challenge(nonce),
            Message::UnlockResult { ok, reason } => self.listener.on_unlock_result(ok, reason),
            Message::SnapRequest => self.listener.on_snap_request(),
            _ => {}
        }
    }
}

/// True means a keyframe is needed. Once one frame is lost, later ones can't decode without it.
fn offer_frame(frames: &mpsc::Sender<VideoFrame>, skip: &AtomicBool, frame: VideoFrame) -> bool {
    let keyframe = frame.keyframe;
    if !keyframe && skip.load(Ordering::SeqCst) {
        return true;
    }
    match frames.try_send(frame) {
        Ok(()) => {
            if keyframe {
                skip.store(false, Ordering::SeqCst);
            }
            false
        }
        Err(TrySendError::Full(_)) => {
            skip.store(true, Ordering::SeqCst);
            true
        }
        Err(TrySendError::Closed(_)) => false,
    }
}

fn load_pc(path: &Path) -> Option<PairedPc> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

#[derive(uniffi::Object)]
pub struct LisClient {
    runtime: Runtime,
    inner: Arc<Inner>,
}

#[uniffi::export]
impl LisClient {
    #[uniffi::constructor]
    pub fn new(data_dir: String, device_name: String, listener: Box<dyn ClientListener>) -> Result<Arc<Self>, LisError> {
        let dir = PathBuf::from(data_dir);
        let identity = Identity::load_or_create(&dir).map_err(failed)?;
        let connector = tls::connector(&identity).map_err(failed)?;
        let video_connector = tls::video_connector(&identity).map_err(failed)?;
        let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().map_err(failed)?;
        let pc_path = dir.join("pc.json");
        let sync_dir = dir.join("sync");
        let docs = sync::KINDS
            .iter()
            .filter_map(|kind| {
                let bytes = fs::read(sync_dir.join(format!("{kind}.json"))).ok()?;
                Some(((*kind).to_string(), serde_json::from_slice(&bytes).ok()?))
            })
            .collect();
        let state = State { pc: load_pc(&pc_path), ..Default::default() };
        let inner = Arc::new(Inner {
            identity,
            connector,
            video_connector,
            name: device_name,
            pc_path,
            sync_dir,
            listener,
            state: Mutex::new(state),
            sync: Mutex::new(docs),
            runtime: runtime.handle().clone(),
            wake: Notify::new(),
        });
        Ok(Arc::new(Self { runtime, inner }))
    }

    pub fn paired_pc(&self) -> Option<PairedPc> {
        self.inner.state.lock().unwrap().pc.clone()
    }

    pub fn fingerprint(&self) -> String {
        self.inner.identity.fingerprint()
    }

    /// Replaces any current target. Connecting to the same address again only retries sooner.
    pub fn connect(&self, host: String, port: u16) {
        let mut state = self.inner.state.lock().unwrap();
        if state.target.as_ref() == Some(&(host.clone(), port)) && state.task.as_ref().is_some_and(|task| !task.is_finished()) {
            drop(state);
            self.inner.wake.notify_one();
            return;
        }
        if let Some(task) = state.task.take() {
            task.abort();
        }
        state.handle = None;
        state.target = Some((host.clone(), port));
        let inner = self.inner.clone();
        state.task = Some(self.runtime.spawn(inner.connection_loop(host, port)));
    }

    /// Retries now, after the network changed.
    pub fn reconnect_now(&self) {
        self.inner.wake.notify_one();
    }

    pub fn disconnect(&self) {
        let mut state = self.inner.state.lock().unwrap();
        if let Some(task) = state.task.take() {
            task.abort();
        }
        if let Some(video) = state.video.take() {
            video.task.abort();
        }
        state.handle = None;
        state.target = None;
        state.connection_state = Some(ConnectionState::Disconnected);
        drop(state);
        self.inner.listener.on_state(ConnectionState::Disconnected, None);
    }

    pub fn start_pairing(&self) {
        self.command(Command::StartPairing);
    }

    pub fn confirm_pairing(&self, accept: bool) {
        self.command(Command::ConfirmPairing(accept));
    }

    /// Forgets the PC here at once, and tells it if connected. The connection stays up, unpaired.
    pub fn unpair(&self) {
        self.stop_video();
        let (fingerprint, handle) = {
            let state = self.inner.state.lock().unwrap();
            (state.pc.as_ref().map(|pc| pc.fingerprint.clone()), state.handle.clone())
        };
        if let Some(handle) = handle {
            self.runtime.spawn(async move { handle.send(Command::Unpair).await });
        }
        if let Some(fingerprint) = fingerprint {
            self.inner.forget(&fingerprint);
        }
    }

    pub fn lock_pc(&self) {
        self.command(Command::Send(Message::LockPc));
    }

    pub fn send_stats(&self, stats: PhoneStats) {
        self.command(Command::Send(Message::PhoneStats(protocol::PhoneStats {
            battery_percent: stats.battery_percent,
            charging: stats.charging,
            storage_used: stats.storage_used,
            storage_total: stats.storage_total,
            memory_used: stats.memory_used,
            memory_total: stats.memory_total,
            network: stats.network,
        })));
    }

    /// Opens the camera's own connection to the paired PC. Replaces one already running.
    pub fn start_video(&self, config: VideoConfig) -> Result<(), LisError> {
        let mut state = self.inner.state.lock().unwrap();
        if state.pc.is_none() {
            return Err(failed("Not paired with a PC"));
        }
        if let Some(video) = state.video.take() {
            video.task.abort();
        }
        let (frames, receiver) = mpsc::channel(VIDEO_QUEUE);
        let skip = Arc::new(AtomicBool::new(true));
        let config_bitrate = config.bitrate;
        let info = Arc::new(Mutex::new(VideoInfo::from(config)));
        let info_changed = Arc::new(Notify::new());
        let governor = Arc::new(Mutex::new(BitrateGovernor::new(config_bitrate)));
        let task = self.runtime.spawn(self.inner.clone().video_loop(receiver, info.clone(), info_changed.clone(), skip.clone()));
        state.video = Some(VideoLink { frames, skip, info, info_changed, governor, task });
        Ok(())
    }

    /// When the rotation or the camera changes while it runs.
    pub fn update_video(&self, config: VideoConfig) {
        let state = self.inner.state.lock().unwrap();
        if let Some(video) = &state.video {
            *video.info.lock().unwrap() = VideoInfo::from(config);
            video.info_changed.notify_one();
        }
    }

    /// Never waits, as this runs on the encoder's thread. Asks for a keyframe soon when frames are
    /// being dropped until one arrives, and for a new bitrate when the connection calls for one.
    pub fn send_video_frame(&self, data: Vec<u8>, pts_us: i64, keyframe: bool) -> VideoFeedback {
        let (frames, skip, governor) = {
            let state = self.inner.state.lock().unwrap();
            let Some(video) = &state.video else { return VideoFeedback { need_keyframe: false, bitrate: None } };
            (video.frames.clone(), video.skip.clone(), video.governor.clone())
        };
        let capacity = frames.max_capacity();
        let queued = capacity - frames.capacity();
        let need_keyframe = offer_frame(&frames, &skip, VideoFrame { pts_us, keyframe, data });
        let bitrate = governor.lock().unwrap().on_frame(pts_us, queued, capacity);
        VideoFeedback { need_keyframe, bitrate }
    }

    pub fn stop_video(&self) {
        if let Some(video) = self.inner.state.lock().unwrap().video.take() {
            video.task.abort();
        }
    }

    pub fn report_ringing(&self, ringing: bool) {
        self.command(Command::Send(Message::PhoneRinging { ringing }));
    }

    /// The document as JSON: the phone's copy, which works without the PC.
    pub fn sync_doc(&self, kind: String) -> String {
        let docs = self.inner.sync.lock().unwrap();
        docs.get(&kind).map_or_else(|| sync::empty(&kind), |side| side.local.clone()).to_string()
    }

    /// An edit of the copy read earlier as `based_on`, passed on when connected. Something from the
    /// PC may have been merged in since, so this merges too, rather than undoing that.
    pub fn sync_edit(&self, kind: String, based_on: String, doc: String) -> Result<SyncEdited, LisError> {
        let based_on: Value = serde_json::from_str(&based_on).map_err(failed)?;
        let doc: Value = serde_json::from_str(&doc).map_err(failed)?;
        sync::check(&kind, &doc).map_err(failed)?;
        let mut docs = self.inner.sync.lock().unwrap();
        let side = docs.entry(kind.clone()).or_insert_with(|| SyncSide::new(&kind));
        let merged = sync::merge(&kind, &based_on, &doc, &side.local);
        sync::check(&kind, &merged).map_err(failed)?;
        let differs = merged != doc;
        side.local = merged;
        self.inner.save_side(&kind, side);
        self.inner.push_if_idle(&kind, side);
        Ok(SyncEdited { doc: side.local.to_string(), merged: differs })
    }

    pub fn media_command(&self, command: MediaCommand) {
        let action = match command {
            MediaCommand::PlayPause => MediaAction::PlayPause,
            MediaCommand::Next => MediaAction::Next,
            MediaCommand::Previous => MediaAction::Previous,
        };
        self.command(Command::Send(Message::MediaControl { action }));
    }

    pub fn media_set_volume(&self, percent: u8) {
        self.command(Command::Send(Message::MediaControl { action: MediaAction::SetVolume { percent: percent.min(100) } }));
    }

    pub fn run_action(&self, id: String) {
        self.command(Command::Send(Message::RunAction { id }));
    }

    /// Asks the PC to accept this key, an X.509 P-256 public key. Someone has to approve it there.
    pub fn unlock_enroll(&self, public_key: Vec<u8>) {
        self.command(Command::Send(Message::UnlockKey { public_key: hex::encode(public_key) }));
    }

    /// After deleting the key, so the PC stops offering this.
    pub fn unlock_revoke(&self) {
        self.command(Command::Send(Message::UnlockRevoke));
    }

    pub fn unlock_request(&self) {
        self.command(Command::Send(Message::UnlockRequest));
    }

    /// What to sign for a challenge, which names both devices.
    pub fn unlock_message(&self, nonce: String) -> Option<Vec<u8>> {
        let pc = self.inner.state.lock().unwrap().pc.as_ref()?.fingerprint.clone();
        Some(unlock::message(&nonce, &pc, &self.inner.identity.fingerprint()))
    }

    pub fn unlock_respond(&self, nonce: String, signature: Vec<u8>) {
        self.command(Command::Send(Message::UnlockResponse { nonce, signature: hex::encode(signature) }));
    }

    /// Takes ownership of `fd`, which must be readable.
    pub fn send_file(&self, fd: i32, name: String, size: u64) -> Result<(), LisError> {
        self.send_fd(fd, name, size, false)
    }

    /// A photo from the Camera screen, which the PC also puts on its clipboard. Takes ownership of `fd`.
    pub fn send_snap(&self, fd: i32, name: String, size: u64) -> Result<(), LisError> {
        self.send_fd(fd, name, size, true)
    }
}

impl LisClient {
    fn send_fd(&self, fd: i32, name: String, size: u64, snap: bool) -> Result<(), LisError> {
        if fd < 0 {
            return Err(failed("Can't read that file"));
        }
        let file = unsafe { File::from_raw_fd(fd) };
        let size = if size > 0 { size } else { file.metadata().map_err(failed)?.len() };
        let handle = self.handle().ok_or_else(|| failed("Not connected to the PC"))?;
        self.runtime.spawn(async move { handle.send(Command::SendFile { file, name, size, snap }).await });
        Ok(())
    }

    fn handle(&self) -> Option<Handle> {
        self.inner.state.lock().unwrap().handle.clone()
    }

    fn command(&self, command: Command) {
        self.inner.send(command);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(pts_us: i64, keyframe: bool) -> VideoFrame {
        VideoFrame { pts_us, keyframe, data: vec![] }
    }

    #[test]
    fn after_a_lost_frame_nothing_goes_until_a_keyframe() {
        let (frames, mut queue) = mpsc::channel(2);
        let skip = AtomicBool::new(true);

        assert!(offer_frame(&frames, &skip, frame(0, false)), "nothing before the first keyframe");
        assert!(queue.try_recv().is_err());

        assert!(!offer_frame(&frames, &skip, frame(1, true)));
        assert!(!offer_frame(&frames, &skip, frame(2, false)));
        assert!(offer_frame(&frames, &skip, frame(3, false)), "the queue is full, so this one is lost");

        // Room again, but the decoder has missed a frame
        assert_eq!(queue.try_recv().unwrap().pts_us, 1);
        assert_eq!(queue.try_recv().unwrap().pts_us, 2);
        assert!(offer_frame(&frames, &skip, frame(4, false)), "must wait for a keyframe");
        assert!(queue.try_recv().is_err(), "and must not be sent");

        assert!(!offer_frame(&frames, &skip, frame(5, true)));
        assert!(!offer_frame(&frames, &skip, frame(6, false)));
        assert_eq!(queue.try_recv().unwrap().pts_us, 5);
        assert_eq!(queue.try_recv().unwrap().pts_us, 6);
    }

    #[test]
    fn a_keyframe_that_does_not_fit_keeps_waiting() {
        let (frames, mut queue) = mpsc::channel(1);
        let skip = AtomicBool::new(true);
        assert!(!offer_frame(&frames, &skip, frame(1, true)));
        assert!(offer_frame(&frames, &skip, frame(2, true)), "no room");
        assert_eq!(queue.try_recv().unwrap().pts_us, 1);
        assert!(offer_frame(&frames, &skip, frame(3, false)));
        assert!(queue.try_recv().is_err());
    }

    #[test]
    fn only_the_usb_tunnel_counts_as_loopback() {
        for host in ["127.0.0.1", "::1", "localhost"] {
            assert!(is_loopback(host), "{host}");
        }
        for host in ["192.168.1.10", "pc.local"] {
            assert!(!is_loopback(host), "{host}");
        }
    }

    #[test]
    fn a_stopped_camera_asks_for_nothing() {
        let (frames, queue) = mpsc::channel(1);
        drop(queue);
        let skip = AtomicBool::new(false);
        assert!(!offer_frame(&frames, &skip, frame(1, true)));
    }
}
