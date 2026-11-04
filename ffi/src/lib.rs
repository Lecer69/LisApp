//! What the Kotlin app calls. It owns one connection to one PC: Kotlin supplies the Android parts
//! (addresses from discovery, phone data, file descriptors) and hears back through the listener.

use lis_core::identity::{write_private, Identity};
use lis_core::names::safe_file_name;
use lis_core::protocol::{self, FileOffer, Message};
use lis_core::session::{self, Command, Event, Handle, Handler, Local, Peer, Role, Timing};
use lis_core::tls;
use lis_core::video::{read_packet_async, write_packet_async, Packet, VideoFrame, VideoInfo, CODEC_H264};
use serde::{Deserialize, Serialize};
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
}

struct VideoLink {
    frames: mpsc::Sender<VideoFrame>,
    /// Frames are dropped until the next keyframe, which is all a decoder can restart from.
    skip: Arc<AtomicBool>,
    info: Arc<Mutex<VideoInfo>>,
    info_changed: Arc<Notify>,
    task: JoinHandle<()>,
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
    listener: Box<dyn ClientListener>,
    state: Mutex<State>,
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

    /// Only once the PC has proved itself, as anyone on the network can announce an address.
    fn remember_address(&self) {
        let mut state = self.state.lock().unwrap();
        let Some((host, port)) = state.target.clone() else { return };
        if let Some(pc) = &mut state.pc {
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
            Event::Message { .. } => {}
            Event::FileProgress { file, done, total } => {
                self.listener.on_file_progress(file.id, file.name, file.incoming, done, total);
            }
            Event::FileFinished { file, ok } => self.listener.on_file_finished(file.id, file.name, file.incoming, ok),
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
        let state = State { pc: load_pc(&pc_path), ..Default::default() };
        let inner = Arc::new(Inner {
            identity,
            connector,
            video_connector,
            name: device_name,
            pc_path,
            listener,
            state: Mutex::new(state),
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
        let info = Arc::new(Mutex::new(VideoInfo::from(config)));
        let info_changed = Arc::new(Notify::new());
        let task = self.runtime.spawn(self.inner.clone().video_loop(receiver, info.clone(), info_changed.clone(), skip.clone()));
        state.video = Some(VideoLink { frames, skip, info, info_changed, task });
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

    /// Never waits, as this runs on the encoder's thread. True means the encoder should send a
    /// keyframe soon, because frames are being dropped until one arrives.
    pub fn send_video_frame(&self, data: Vec<u8>, pts_us: i64, keyframe: bool) -> bool {
        let (frames, skip) = {
            let state = self.inner.state.lock().unwrap();
            let Some(video) = &state.video else { return false };
            (video.frames.clone(), video.skip.clone())
        };
        offer_frame(&frames, &skip, VideoFrame { pts_us, keyframe, data })
    }

    pub fn stop_video(&self) {
        if let Some(video) = self.inner.state.lock().unwrap().video.take() {
            video.task.abort();
        }
    }

    /// Takes ownership of `fd`, which must be readable.
    pub fn send_file(&self, fd: i32, name: String, size: u64) -> Result<(), LisError> {
        if fd < 0 {
            return Err(failed("Can't read that file"));
        }
        let file = unsafe { File::from_raw_fd(fd) };
        let size = if size > 0 { size } else { file.metadata().map_err(failed)?.len() };
        let handle = self.handle().ok_or_else(|| failed("Not connected to the PC"))?;
        self.runtime.spawn(async move { handle.send(Command::SendFile { file, name, size }).await });
        Ok(())
    }
}

impl LisClient {
    fn handle(&self) -> Option<Handle> {
        self.inner.state.lock().unwrap().handle.clone()
    }

    fn command(&self, command: Command) {
        if let Some(handle) = self.handle() {
            self.runtime.spawn(async move { handle.send(command).await });
        }
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
    fn a_stopped_camera_asks_for_nothing() {
        let (frames, queue) = mpsc::channel(1);
        drop(queue);
        let skip = AtomicBool::new(false);
        assert!(!offer_frame(&frames, &skip, frame(1, true)));
    }
}
