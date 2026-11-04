//! What the Kotlin app calls. It owns one connection to one PC: Kotlin supplies the Android parts
//! (addresses from discovery, phone data, file descriptors) and hears back through the listener.

use lis_core::identity::{write_private, Identity};
use lis_core::names::safe_file_name;
use lis_core::protocol::{self, FileOffer, Message};
use lis_core::session::{self, Command, Event, Handle, Handler, Local, Peer, Role, Timing};
use lis_core::tls;
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::os::fd::FromRawFd;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::runtime::Runtime;
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio_rustls::TlsConnector;

uniffi::setup_scaffolding!();

const MAX_RETRY_DELAY: Duration = Duration::from_secs(60);

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
}

#[derive(Default)]
struct State {
    pc: Option<PairedPc>,
    target: Option<(String, u16)>,
    handle: Option<Handle>,
    task: Option<JoinHandle<()>>,
    connection_state: Option<ConnectionState>,
}

struct Inner {
    identity: Identity,
    connector: TlsConnector,
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
        let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().map_err(failed)?;
        let pc_path = dir.join("pc.json");
        let state = State { pc: load_pc(&pc_path), ..Default::default() };
        let inner = Arc::new(Inner {
            identity,
            connector,
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
