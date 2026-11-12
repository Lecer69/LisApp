//! The state behind the connections, and what each event from a phone does on this PC.

use crate::actions::{self, Action, Actions};
use crate::camera::Camera;
use crate::media::Media;
use crate::snap;
use crate::stats::{hostname, Sampler};
use crate::sync::{Applied, SyncStore};
use lis_core::identity::{write_private, Identity};
use lis_core::names::safe_file_name;
use lis_core::protocol::{FileOffer, Message, PcStats, PhoneStats};
use lis_core::session::{Command, Event, Handle, Handler, Peer};
use lis_core::sync::{MAX_DOC_LEN, KINDS};
use lis_core::unlock;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::process::Command as Process;
use tokio::sync::{broadcast, mpsc, Notify};
use tokio::time::Instant;

const LOW_BATTERY: u8 = 20;
const VERY_LOW_BATTERY: u8 = 10;
const FULL_BATTERY: u8 = 100;
/// How long the biometric prompt can take.
const CHALLENGE_LIMIT: Duration = Duration::from_secs(60);
/// A prompt that was dismissed shouldn't come straight back with the next key pressed.
const PROMPT_EVERY: Duration = Duration::from_secs(20);
/// Locking from the phone is someone walking away, not someone wanting back in.
const QUIET_AFTER_PHONE_LOCK: Duration = Duration::from_secs(5);
/// Wrong signatures before only the password will do.
const MAX_UNLOCK_FAILURES: u32 = 3;
const ADB_LIMIT: Duration = Duration::from_secs(10);

pub struct Paths {
    pub data: PathBuf,
    pub socket: PathBuf,
    pub camera_socket: PathBuf,
    pub downloads: PathBuf,
    /// Photos from the Camera screen, unless the shell names another folder.
    pub snaps: PathBuf,
    /// The shell's own data, where its todo, goals and notes live.
    pub shell_data: PathBuf,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct TrustedPhone {
    fingerprint: String,
    name: String,
    paired_at_ms: i64,
    /// Set only after someone approves it on this PC.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    unlock_key: Option<String>,
}

struct Link {
    handle: Handle,
    generation: u64,
    paired: bool,
}

struct PairCode {
    fingerprint: String,
    name: String,
    code: String,
}

struct IncomingFile {
    part: PathBuf,
    done: PathBuf,
    snap: bool,
}

/// From the shell's settings.
struct SnapSettings {
    folder: Option<PathBuf>,
    clipboard: bool,
}

impl Default for SnapSettings {
    fn default() -> Self {
        Self { folder: None, clipboard: true }
    }
}

/// Each alert goes out once, until the battery has recovered or the phone has been unplugged.
#[derive(Default, Clone, Copy)]
struct BatteryAlerts {
    known: bool,
    low: bool,
    very_low: bool,
    full: bool,
}

/// Plugged in and already full when first heard from isn't news.
fn battery_alert(alerts: &mut BatteryAlerts, stats: &PhoneStats) -> Option<(&'static str, String)> {
    let percent = stats.battery_percent;
    let first = !alerts.known;
    alerts.known = true;

    if percent > LOW_BATTERY {
        alerts.low = false;
    }
    if percent > VERY_LOW_BATTERY {
        alerts.very_low = false;
    }
    if stats.charging {
        let newly_full = percent >= FULL_BATTERY && !alerts.full;
        alerts.full |= percent >= FULL_BATTERY;
        return (newly_full && !first).then(|| ("Phone charged", "It's at 100%".to_string()));
    }
    alerts.full = false;
    if percent <= VERY_LOW_BATTERY && !alerts.very_low {
        alerts.very_low = true;
        alerts.low = true;
        return Some(("Phone battery very low", format!("It's at {percent}%")));
    }
    if percent <= LOW_BATTERY && !alerts.low {
        alerts.low = true;
        return Some(("Phone battery low", format!("It's at {percent}%")));
    }
    None
}

#[derive(Default)]
struct LockState {
    locked: bool,
    /// The shell wants the password.
    password_required: bool,
    /// Too many wrong signatures, until the next password.
    blocked: bool,
    failures: u32,
    challenge: Option<Challenge>,
    last_prompt: Option<Instant>,
}

struct Challenge {
    fingerprint: String,
    nonce: String,
    at: Instant,
}

#[derive(Default)]
struct UsbState {
    enabled: bool,
    forwarding: bool,
}

#[derive(Default)]
struct State {
    trusted: Vec<TrustedPhone>,
    links: HashMap<String, Link>,
    next_generation: u64,
    pairing_until: Option<Instant>,
    pair_code: Option<PairCode>,
    stats: HashMap<String, PhoneStats>,
    incoming: HashMap<(String, u64), IncomingFile>,
    pc_stats: PcStats,
    sampler: Sampler,
    battery: HashMap<String, BatteryAlerts>,
    ringing: HashMap<String, bool>,
    /// Keys asked for and not yet approved.
    pending_unlock: HashMap<String, String>,
    lock: LockState,
    locked_from_phone: Option<Instant>,
    usb: UsbState,
    snap: SnapSettings,
}

pub struct Daemon {
    pub identity: Identity,
    pub name: String,
    pub port: u16,
    pub paths: Paths,
    pub shutdown: Notify,
    pub camera: Camera,
    pub media: Media,
    sync: SyncStore,
    /// Held from reading a document until it's queued. Otherwise an older copy could reach a phone
    /// after a newer one, and what's missing from it would look like a deletion.
    sync_order: Mutex<()>,
    actions: Actions,
    state: Mutex<State>,
    events: broadcast::Sender<String>,
    /// One queue, so messages to a phone arrive in the order they were made.
    outbox: mpsc::UnboundedSender<(Handle, Message)>,
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |time| time.as_millis() as i64)
}

/// A free name, reserved on disk along with its partial file.
fn reserve(dir: &Path, name: &str) -> std::io::Result<(fs::File, PathBuf, PathBuf)> {
    fs::create_dir_all(dir)?;
    let path = Path::new(name);
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or(name);
    let extension = path.extension().and_then(|s| s.to_str());

    for attempt in 0..10_000 {
        let candidate = match (attempt, extension) {
            (0, _) => name.to_string(),
            (n, Some(extension)) => format!("{stem} ({n}).{extension}"),
            (n, None) => format!("{stem} ({n})"),
        };
        let done = dir.join(&candidate);
        let part = dir.join(format!("{candidate}.part"));
        if done.exists() {
            continue;
        }
        // create_new also refuses a symlink waiting at that name
        match fs::OpenOptions::new().write(true).create_new(true).open(&part) {
            Ok(file) => return Ok((file, part, done)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::other("no free file name"))
}

impl Daemon {
    pub fn new(identity: Identity, port: u16, paths: Paths) -> Self {
        let trusted = fs::read(paths.data.join("trusted.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        let (events, _) = broadcast::channel(256);
        let sync = SyncStore::new(paths.shell_data.clone(), paths.data.join("sync-backups"));
        let actions = Actions::new(paths.shell_data.join("phone-actions.json"));
        let (outbox, mut queued) = mpsc::unbounded_channel::<(Handle, Message)>();
        tokio::spawn(async move {
            while let Some((handle, message)) = queued.recv().await {
                handle.send(Command::Send(message)).await;
            }
        });
        Self {
            identity,
            name: hostname(),
            port,
            paths,
            shutdown: Notify::new(),
            camera: Camera::new(),
            media: Media::default(),
            sync,
            sync_order: Mutex::new(()),
            actions,
            state: Mutex::new(State { trusted, ..Default::default() }),
            events,
            outbox,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<String> {
        self.events.subscribe()
    }

    fn publish(&self, event: &str, data: Value) {
        let mut message = json!({ "event": event });
        if let (Some(message), Value::Object(data)) = (message.as_object_mut(), data) {
            message.extend(data);
        }
        let _ = self.events.send(message.to_string());
    }

    pub fn has_phone(&self) -> bool {
        self.state.lock().unwrap().links.values().any(|link| link.paired)
    }

    /// To every connected phone.
    pub fn broadcast(&self, message: Message) {
        let state = self.state.lock().unwrap();
        for link in state.links.values().filter(|link| link.paired) {
            let _ = self.outbox.send((link.handle.clone(), message.clone()));
        }
    }

    fn send_to(&self, fingerprint: &str, message: Message) {
        let state = self.state.lock().unwrap();
        if let Some(link) = state.links.get(fingerprint).filter(|link| link.paired) {
            let _ = self.outbox.send((link.handle.clone(), message));
        }
    }

    /// A paired phone's name.
    pub fn phone_name(&self, fingerprint: &str) -> Option<String> {
        let state = self.state.lock().unwrap();
        state.trusted.iter().find(|phone| phone.fingerprint == fingerprint).map(|phone| phone.name.clone())
    }

    /// Updates the camera's figures; only reports while it's live, as nothing changes otherwise.
    pub fn camera_tick(&self) {
        if self.camera.measure() {
            self.publish_status();
        }
    }

    pub fn publish_status(&self) {
        self.publish("status", json!({ "status": self.status() }));
    }

    pub fn status(&self) -> Value {
        let state = self.state.lock().unwrap();
        let pairing = state.pairing_until.is_some_and(|until| Instant::now() < until);
        let phones: Vec<Value> = state
            .trusted
            .iter()
            .map(|phone| {
                json!({
                    "fingerprint": phone.fingerprint,
                    "name": phone.name,
                    "connected": state.links.get(&phone.fingerprint).is_some_and(|link| link.paired),
                    "stats": state.stats.get(&phone.fingerprint),
                    "ringing": state.ringing.get(&phone.fingerprint).copied().unwrap_or(false),
                    "unlock": if state.pending_unlock.contains_key(&phone.fingerprint) {
                        "pending"
                    } else if phone.unlock_key.is_some() {
                        "on"
                    } else {
                        "off"
                    },
                })
            })
            .collect();
        json!({
            "name": self.name,
            "fingerprint": self.identity.fingerprint(),
            "port": self.port,
            "pairing": pairing,
            "pair_code": state.pair_code.as_ref().map(|pair| json!({ "name": pair.name, "code": pair.code })),
            "phones": phones,
            "camera": self.camera.status(),
            "usb": { "enabled": state.usb.enabled, "forwarding": state.usb.forwarding },
            "snap": { "folder": self.snap_folder(&state).display().to_string(), "clipboard": state.snap.clipboard },
            "lock": { "locked": state.lock.locked, "password_required": state.lock.password_required, "blocked": state.lock.blocked },
            "actions": self.actions.list(),
        })
    }

    fn save_trusted(&self, state: &State) {
        match serde_json::to_vec_pretty(&state.trusted) {
            Ok(bytes) => {
                let _ = fs::create_dir_all(&self.paths.data);
                if let Err(error) = write_private(&self.paths.data.join("trusted.json"), &bytes) {
                    eprintln!("lis-daemon: can't save paired phones: {error}");
                }
            }
            Err(error) => eprintln!("lis-daemon: can't encode paired phones: {error}"),
        }
    }

    /// A phone's newest connection replaces its older one.
    pub fn register(&self, fingerprint: &str, handle: Handle) -> u64 {
        let mut state = self.state.lock().unwrap();
        state.next_generation += 1;
        let generation = state.next_generation;
        let old = state.links.insert(fingerprint.to_string(), Link { handle, generation, paired: false });
        drop(state);
        if let Some(old) = old {
            tokio::spawn(async move { old.handle.send(Command::Close).await });
        }
        generation
    }

    pub fn unregister(&self, fingerprint: &str, generation: u64) {
        let mut state = self.state.lock().unwrap();
        if state.links.get(fingerprint).is_some_and(|link| link.generation == generation) {
            state.links.remove(fingerprint);
            if state.pair_code.as_ref().is_some_and(|pair| pair.fingerprint == fingerprint) {
                state.pair_code = None;
            }
        }
        drop(state);
        self.publish_status();
    }

    pub fn open_pairing(&self, seconds: u64) {
        let mut state = self.state.lock().unwrap();
        state.pairing_until = Some(Instant::now() + Duration::from_secs(seconds));
        drop(state);
        self.publish_status();
    }

    pub fn close_pairing(&self) {
        let mut state = self.state.lock().unwrap();
        state.pairing_until = None;
        let pending = state.pair_code.take().and_then(|pair| state.links.get(&pair.fingerprint).map(|link| link.handle.clone()));
        drop(state);
        if let Some(handle) = pending {
            tokio::spawn(async move { handle.send(Command::ConfirmPairing(false)).await });
        }
        self.publish_status();
    }

    pub async fn confirm_pairing(&self, accept: bool) -> bool {
        let handle = {
            let state = self.state.lock().unwrap();
            state.pair_code.as_ref().and_then(|pair| state.links.get(&pair.fingerprint).map(|link| link.handle.clone()))
        };
        match handle {
            Some(handle) => handle.send(Command::ConfirmPairing(accept)).await,
            None => false,
        }
    }

    /// The only phone, when there's one and none was named.
    fn pick_phone(&self, state: &State, fingerprint: Option<&str>) -> Option<String> {
        match fingerprint {
            Some(fingerprint) => state.trusted.iter().find(|phone| phone.fingerprint == fingerprint).map(|phone| phone.fingerprint.clone()),
            None => match state.trusted.as_slice() {
                [only] => Some(only.fingerprint.clone()),
                _ => state.links.iter().find(|(_, link)| link.paired).map(|(fingerprint, _)| fingerprint.clone()),
            },
        }
    }

    pub async fn unpair(&self, fingerprint: &str) -> bool {
        let handle = {
            let state = self.state.lock().unwrap();
            if !state.trusted.iter().any(|phone| phone.fingerprint == fingerprint) {
                return false;
            }
            state.links.get(fingerprint).map(|link| link.handle.clone())
        };
        match handle {
            // The session tells the phone, then forgets it here
            Some(handle) => {
                handle.send(Command::Unpair).await;
            }
            None => {
                self.forget(fingerprint);
                self.publish_status();
            }
        }
        true
    }

    pub async fn send_file(&self, fingerprint: Option<&str>, path: &str) -> Result<(), String> {
        let (handle, name) = {
            let state = self.state.lock().unwrap();
            let phone = self.pick_phone(&state, fingerprint).ok_or("No paired phone")?;
            let link = state.links.get(&phone).filter(|link| link.paired).ok_or("The phone isn't connected")?;
            let name = Path::new(path).file_name().and_then(|name| name.to_str()).ok_or("Not a file")?;
            (link.handle.clone(), name.to_string())
        };
        let file = fs::File::open(path).map_err(|error| error.to_string())?;
        let metadata = file.metadata().map_err(|error| error.to_string())?;
        if !metadata.is_file() {
            return Err("Not a file".to_string());
        }
        handle.send(Command::SendFile { file, name, size: metadata.len(), snap: false }).await;
        Ok(())
    }

    fn snap_folder(&self, state: &State) -> PathBuf {
        state.snap.folder.clone().unwrap_or_else(|| self.paths.snaps.clone())
    }

    /// An empty folder is the default one.
    pub fn set_snap_settings(&self, folder: &str, clipboard: bool) -> Result<(), String> {
        let folder = match folder {
            "" => None,
            folder if Path::new(folder).is_absolute() => Some(PathBuf::from(folder)),
            _ => return Err("The folder needs a full path".to_string()),
        };
        let mut state = self.state.lock().unwrap();
        state.snap = SnapSettings { folder, clipboard };
        drop(state);
        self.publish_status();
        Ok(())
    }

    /// Only the running camera can take one, so it goes to the phone that's streaming.
    pub fn request_snap(&self) -> Result<(), String> {
        let phone = self.camera.live_phone().ok_or("The phone's camera isn't on")?;
        let state = self.state.lock().unwrap();
        let link = state.links.get(&phone).filter(|link| link.paired).ok_or("The phone isn't connected")?;
        let _ = self.outbox.send((link.handle.clone(), Message::SnapRequest));
        Ok(())
    }

    /// Rings until told otherwise, or until the phone gives up by itself.
    pub fn ring_phone(&self, fingerprint: Option<&str>, ring: bool) -> Result<(), String> {
        let state = self.state.lock().unwrap();
        let phone = self.pick_phone(&state, fingerprint).ok_or("No paired phone")?;
        let link = state.links.get(&phone).filter(|link| link.paired).ok_or("The phone isn't connected")?;
        let _ = self.outbox.send((link.handle.clone(), Message::RingPhone { ring }));
        Ok(())
    }

    fn doc_message(kind: &str, rev: String, doc: Value) -> Message {
        // A message over the limit would end the connection, and the phone would only try again
        if doc.to_string().len() > MAX_DOC_LEN {
            eprintln!("lis-daemon: {kind} has grown too large to sync");
            return Message::SyncRejected { kind: kind.to_string(), reason: format!("{kind} has grown too large to sync") };
        }
        Message::SyncDoc { kind: kind.to_string(), rev, doc }
    }

    /// Passes on what the shell saved. Also looks for hand edits to the action list.
    pub fn sync_tick(&self) {
        if !self.has_phone() {
            return;
        }
        {
            let _order = self.sync_order.lock().unwrap();
            for (kind, rev, doc) in self.sync.changed() {
                self.broadcast(Self::doc_message(kind, rev, doc));
            }
        }
        if self.actions.reload_if_changed() {
            self.broadcast(Message::Actions { actions: self.actions.buttons() });
            self.publish_status();
        }
    }

    fn on_sync_push(&self, fingerprint: &str, kind: String, based_on: &str, doc: Value) {
        let _order = self.sync_order.lock().unwrap();
        match self.sync.apply(&kind, based_on, &doc) {
            Ok(Applied::Written { rev }) => self.broadcast(Self::doc_message(&kind, rev, doc)),
            Ok(Applied::Stale { rev, doc }) => self.send_to(fingerprint, Self::doc_message(&kind, rev, doc)),
            Err(reason) => {
                eprintln!("lis-daemon: refused {kind} from a phone: {reason}");
                self.send_to(fingerprint, Message::SyncRejected { kind, reason });
            }
        }
    }

    pub fn set_actions(&self, actions: Vec<Action>) -> Result<(), String> {
        self.actions.set(actions)?;
        self.broadcast(Message::Actions { actions: self.actions.buttons() });
        self.publish_status();
        Ok(())
    }

    fn run_action(&self, fingerprint: &str, id: String) {
        let Some(command) = self.actions.command_of(&id) else {
            self.send_to(fingerprint, Message::ActionResult { id, ok: false });
            return;
        };
        let handle = self.state.lock().unwrap().links.get(fingerprint).map(|link| link.handle.clone());
        tokio::spawn(async move {
            let ok = actions::run(command).await;
            if let Some(handle) = handle {
                handle.send(Command::Send(Message::ActionResult { id, ok })).await;
            }
        });
    }

    pub async fn set_usb(&self, enabled: bool) {
        let was = {
            let mut state = self.state.lock().unwrap();
            let was = state.usb.enabled;
            state.usb.enabled = enabled;
            if !enabled {
                state.usb.forwarding = false;
            }
            was
        };
        if enabled {
            self.usb_tick().await;
        } else if was {
            let forward = format!("tcp:{}", self.port);
            self.adb(&["reverse", "--remove", &forward]).await;
        }
        self.publish_status();
    }

    /// A phone forgets its forward each time it's plugged in, so this keeps asking.
    pub async fn usb_tick(&self) {
        if !self.state.lock().unwrap().usb.enabled {
            return;
        }
        let forward = format!("tcp:{}", self.port);
        let forwarding = self.adb(&["reverse", &forward, &forward]).await;
        let changed = {
            let mut state = self.state.lock().unwrap();
            let changed = state.usb.enabled && state.usb.forwarding != forwarding;
            if state.usb.enabled {
                state.usb.forwarding = forwarding;
            }
            changed
        };
        if changed {
            self.publish_status();
        }
    }

    /// Only the outcome counts: its output can stay open as long as the adb server it starts.
    async fn adb(&self, args: &[&str]) -> bool {
        let status = Process::new("adb")
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .status();
        matches!(tokio::time::timeout(ADB_LIMIT, status).await, Ok(Ok(status)) if status.success())
    }

    fn unlock_message(&self, state: &State, fingerprint: &str) -> Message {
        let key = state.trusted.iter().find(|phone| phone.fingerprint == fingerprint).and_then(|phone| phone.unlock_key.as_ref());
        let lock = &state.lock;
        Message::UnlockState { locked: lock.locked, available: lock.locked && !lock.password_required && !lock.blocked && key.is_some() }
    }

    fn send_unlock_states(&self) {
        let state = self.state.lock().unwrap();
        for (fingerprint, link) in state.links.iter().filter(|(_, link)| link.paired) {
            let _ = self.outbox.send((link.handle.clone(), self.unlock_message(&state, fingerprint)));
        }
    }

    /// From the shell, which is what knows about the lock. Being unlocked clears the failures, and
    /// once they've blocked this, only the password gets there.
    pub fn set_lock_state(&self, locked: bool, password_required: bool) {
        {
            let mut state = self.state.lock().unwrap();
            let lock = &mut state.lock;
            lock.locked = locked;
            lock.password_required = password_required;
            if !locked {
                *lock = LockState { locked, password_required, ..Default::default() };
            }
        }
        self.send_unlock_states();
    }

    /// From the shell, when someone is at the lock screen. A prompt can't unlock anything by itself.
    pub fn prompt_unlock(&self) {
        let mut state = self.state.lock().unwrap();
        let quiet = state.locked_from_phone.is_some_and(|at| at.elapsed() < QUIET_AFTER_PHONE_LOCK);
        let lock = &state.lock;
        let recent = lock.last_prompt.is_some_and(|at| at.elapsed() < PROMPT_EVERY);
        if quiet || recent || !lock.locked || lock.password_required || lock.blocked {
            return;
        }
        let mut asked = false;
        for phone in state.trusted.iter().filter(|phone| phone.unlock_key.is_some()) {
            if let Some(link) = state.links.get(&phone.fingerprint).filter(|link| link.paired) {
                let _ = self.outbox.send((link.handle.clone(), Message::UnlockPrompt));
                asked = true;
            }
        }
        // After a boot the phone may still be connecting, and the next key press should reach it
        if asked {
            state.lock.last_prompt = Some(Instant::now());
        }
    }

    /// Approving can only be done from here, on a PC that's already unlocked.
    pub fn answer_unlock(&self, fingerprint: &str, accept: bool) -> Result<(), String> {
        {
            let mut state = self.state.lock().unwrap();
            let key = state.pending_unlock.remove(fingerprint).ok_or("That phone isn't asking")?;
            if accept {
                let phone = state.trusted.iter_mut().find(|phone| phone.fingerprint == fingerprint).ok_or("Not a paired phone")?;
                phone.unlock_key = Some(key);
                self.save_trusted(&state);
            }
        }
        if !accept {
            self.send_to(fingerprint, Message::UnlockResult { ok: false, reason: "Declined on the PC".to_string() });
        }
        self.send_unlock_states();
        self.publish_status();
        Ok(())
    }

    pub fn disable_unlock(&self, fingerprint: &str) -> Result<(), String> {
        {
            let mut state = self.state.lock().unwrap();
            state.pending_unlock.remove(fingerprint);
            let phone = state.trusted.iter_mut().find(|phone| phone.fingerprint == fingerprint).ok_or("Not a paired phone")?;
            phone.unlock_key = None;
            self.save_trusted(&state);
        }
        self.send_unlock_states();
        self.publish_status();
        Ok(())
    }

    fn on_unlock_key(&self, peer: &Peer, public_key: String) {
        if !unlock::is_valid_key(&public_key) {
            self.send_to(&peer.fingerprint, Message::UnlockResult { ok: false, reason: "That key can't be used".to_string() });
            return;
        }
        {
            let mut state = self.state.lock().unwrap();
            let current = state.trusted.iter().find(|phone| phone.fingerprint == peer.fingerprint).and_then(|phone| phone.unlock_key.clone());
            if current.as_deref() == Some(public_key.as_str()) {
                return;
            }
            state.pending_unlock.insert(peer.fingerprint.clone(), public_key);
        }
        notify("Phone unlock", &format!("{} asks to unlock this PC. Approve it in Settings, Phone Sync.", peer.name));
        self.publish_status();
    }

    fn on_unlock_request(&self, fingerprint: &str) {
        let answer = {
            let mut state = self.state.lock().unwrap();
            let has_key = state.trusted.iter().any(|phone| phone.fingerprint == fingerprint && phone.unlock_key.is_some());
            let lock = &mut state.lock;
            if !has_key {
                Message::UnlockResult { ok: false, reason: "Not approved on the PC".to_string() }
            } else if !lock.locked {
                Message::UnlockResult { ok: false, reason: "The PC isn't locked".to_string() }
            } else if lock.password_required || lock.blocked {
                Message::UnlockResult { ok: false, reason: "The PC wants its password this time".to_string() }
            } else {
                let nonce = lis_core::pairing::new_nonce();
                lock.challenge = Some(Challenge { fingerprint: fingerprint.to_string(), nonce: nonce.clone(), at: Instant::now() });
                Message::UnlockChallenge { nonce }
            }
        };
        self.send_to(fingerprint, answer);
    }

    /// A challenge is good for one try, from the phone it was given to, for a short while.
    fn on_unlock_response(&self, peer: &Peer, nonce: &str, signature: &str) {
        let granted = {
            let mut state = self.state.lock().unwrap();
            let key = state.trusted.iter().find(|phone| phone.fingerprint == peer.fingerprint).and_then(|phone| phone.unlock_key.clone());
            let lock = &mut state.lock;
            let challenge = lock.challenge.take();
            let current = challenge.is_some_and(|challenge| {
                challenge.fingerprint == peer.fingerprint && challenge.nonce == nonce && challenge.at.elapsed() < CHALLENGE_LIMIT
            });
            let usable = lock.locked && !lock.password_required && !lock.blocked;
            let message = unlock::message(nonce, &self.identity.fingerprint(), &peer.fingerprint);
            let granted = current && usable && key.is_some_and(|key| unlock::verify(&key, &message, signature));
            if granted {
                lock.failures = 0;
            } else {
                lock.failures += 1;
                lock.blocked = lock.failures >= MAX_UNLOCK_FAILURES;
            }
            granted
        };
        if granted {
            self.publish("unlock_granted", json!({ "by": peer.name }));
            notify("Lis App", &format!("Unlocked by {}", peer.name));
            self.send_to(&peer.fingerprint, Message::UnlockResult { ok: true, reason: String::new() });
        } else {
            self.send_to(&peer.fingerprint, Message::UnlockResult { ok: false, reason: "The PC didn't accept that".to_string() });
            self.send_unlock_states();
        }
    }

    /// Sampled here so every phone gets the same figures.
    pub async fn send_stats(&self) {
        let (stats, handles) = {
            let mut state = self.state.lock().unwrap();
            let stats = state.sampler.sample();
            state.pc_stats = stats.clone();
            let handles: Vec<Handle> = state.links.values().filter(|link| link.paired).map(|link| link.handle.clone()).collect();
            (stats, handles)
        };
        for handle in handles {
            handle.send(Command::Send(Message::PcStats(stats.clone()))).await;
        }
    }

    fn on_connected(&self, peer: Peer, paired: bool) {
        let mut state = self.state.lock().unwrap();
        let mut greeting = false;
        if paired {
            if let Some(phone) = state.trusted.iter_mut().find(|phone| phone.fingerprint == peer.fingerprint) {
                if phone.name != peer.name {
                    phone.name = peer.name.clone();
                }
            }
            if let Some(link) = state.links.get_mut(&peer.fingerprint) {
                link.paired = true;
                greeting = true;
            }
        }
        let stats = state.pc_stats.clone();
        let unlock_state = self.unlock_message(&state, &peer.fingerprint);
        drop(state);
        if greeting {
            self.send_to(&peer.fingerprint, Message::PcStats(stats));
            {
                let _order = self.sync_order.lock().unwrap();
                for kind in KINDS {
                    match self.sync.current(kind) {
                        Ok((rev, doc)) => self.send_to(&peer.fingerprint, Self::doc_message(kind, rev, doc)),
                        Err(error) => eprintln!("lis-daemon: not syncing {kind}: {error}"),
                    }
                }
            }
            self.send_to(&peer.fingerprint, Message::Actions { actions: self.actions.buttons() });
            for message in self.media.greeting() {
                self.send_to(&peer.fingerprint, message);
            }
            self.send_to(&peer.fingerprint, unlock_state);
            self.media.wake().notify_one();
        }
        self.publish_status();
    }

    fn on_file_finished(&self, file: lis_core::session::FileStatus, ok: bool) {
        let mut saved_as = None;
        let mut snapped = false;
        if file.incoming {
            let reserved = self.state.lock().unwrap().incoming.remove(&(file.peer.fingerprint.clone(), file.id));
            if let Some(reserved) = reserved {
                snapped = reserved.snap;
                if ok && fs::rename(&reserved.part, &reserved.done).is_ok() {
                    saved_as = Some(reserved.done);
                } else {
                    let _ = fs::remove_file(&reserved.part);
                }
            }
        }

        let delivered = if file.incoming { saved_as.is_some() } else { ok };
        if let (Some(path), true) = (&saved_as, snapped) {
            let clipboard = self.state.lock().unwrap().snap.clipboard;
            snap::deliver(file.peer.name.clone(), path.clone(), clipboard);
        } else if let Some(path) = &saved_as {
            notify(&format!("File from {}", file.peer.name), &path.display().to_string());
        } else if !delivered {
            notify("Lis App", &format!("Couldn't transfer {}", file.name));
        }
        self.publish(
            "file_finished",
            json!({
                "name": file.name,
                "incoming": file.incoming,
                "ok": delivered,
                "snap": snapped,
                "path": saved_as.map(|path| path.display().to_string()),
            }),
        );
    }

    fn on_message(&self, peer: Peer, message: Message) {
        match message {
            Message::LockPc => {
                self.state.lock().unwrap().locked_from_phone = Some(Instant::now());
                lock_screen();
                self.publish("locked", json!({ "by": peer.name }));
            }
            Message::PhoneStats(stats) => {
                let alert = {
                    let mut state = self.state.lock().unwrap();
                    let alert = battery_alert(state.battery.entry(peer.fingerprint.clone()).or_default(), &stats);
                    state.stats.insert(peer.fingerprint.clone(), stats);
                    alert
                };
                if let Some((title, body)) = alert {
                    notify(title, &format!("{}: {body}", peer.name));
                }
                self.publish_status();
            }
            Message::PhoneRinging { ringing } => {
                self.state.lock().unwrap().ringing.insert(peer.fingerprint.clone(), ringing);
                self.publish_status();
            }
            Message::SyncPush { kind, based_on, doc } => self.on_sync_push(&peer.fingerprint, kind, &based_on, doc),
            Message::MediaControl { action } => self.media.control(action),
            Message::RunAction { id } => self.run_action(&peer.fingerprint, id),
            Message::UnlockKey { public_key } => self.on_unlock_key(&peer, public_key),
            Message::UnlockRevoke => {
                let _ = self.disable_unlock(&peer.fingerprint);
            }
            Message::UnlockRequest => self.on_unlock_request(&peer.fingerprint),
            Message::UnlockResponse { nonce, signature } => self.on_unlock_response(&peer, &nonce, &signature),
            _ => {}
        }
    }
}

impl Handler for Daemon {
    fn is_trusted(&self, fingerprint: &str) -> bool {
        self.state.lock().unwrap().trusted.iter().any(|phone| phone.fingerprint == fingerprint)
    }

    /// One at a time, so a second phone can't swap the code on screen.
    fn may_pair(&self) -> bool {
        let state = self.state.lock().unwrap();
        state.pair_code.is_none() && state.pairing_until.is_some_and(|until| Instant::now() < until)
    }

    fn trust(&self, peer: &Peer) {
        let mut state = self.state.lock().unwrap();
        state.trusted.retain(|phone| phone.fingerprint != peer.fingerprint);
        state.trusted.push(TrustedPhone { fingerprint: peer.fingerprint.clone(), name: peer.name.clone(), paired_at_ms: now_ms(), unlock_key: None });
        state.pairing_until = None;
        state.pair_code = None;
        self.save_trusted(&state);
    }

    fn forget(&self, fingerprint: &str) {
        let mut state = self.state.lock().unwrap();
        state.trusted.retain(|phone| phone.fingerprint != fingerprint);
        state.stats.remove(fingerprint);
        state.battery.remove(fingerprint);
        state.ringing.remove(fingerprint);
        state.pending_unlock.remove(fingerprint);
        if state.lock.challenge.as_ref().is_some_and(|challenge| challenge.fingerprint == fingerprint) {
            state.lock.challenge = None;
        }
        if let Some(link) = state.links.get_mut(fingerprint) {
            link.paired = false;
        }
        self.save_trusted(&state);
        drop(state);
        self.camera.drop_phone(fingerprint);
    }

    fn open_incoming(&self, peer: &Peer, offer: &FileOffer) -> Option<fs::File> {
        let name = safe_file_name(&offer.name);
        let dir = if offer.snap { self.snap_folder(&self.state.lock().unwrap()) } else { self.paths.downloads.clone() };
        match reserve(&dir, &name) {
            Ok((file, part, done)) => {
                let incoming = IncomingFile { part, done, snap: offer.snap };
                self.state.lock().unwrap().incoming.insert((peer.fingerprint.clone(), offer.id), incoming);
                Some(file)
            }
            Err(error) => {
                eprintln!("lis-daemon: can't save an incoming file: {error}");
                None
            }
        }
    }

    fn event(&self, event: Event) {
        match event {
            Event::Connected { peer, paired } => self.on_connected(peer, paired),
            Event::PairCode { peer, code } => {
                self.state.lock().unwrap().pair_code =
                    Some(PairCode { fingerprint: peer.fingerprint, name: peer.name.clone(), code: code.clone() });
                notify("Pair a phone", &format!("{} shows code {}", peer.name, code));
                self.publish_status();
            }
            Event::PairResult { ok, reason, .. } => {
                self.state.lock().unwrap().pair_code = None;
                self.publish("pair_result", json!({ "ok": ok, "reason": reason }));
                self.publish_status();
            }
            Event::Unpaired { .. } => self.publish_status(),
            Event::Message { peer, message } => self.on_message(peer, message),
            Event::FileProgress { file, done, total } => {
                self.publish("file_progress", json!({ "name": file.name, "incoming": file.incoming, "done": done, "total": total }));
            }
            Event::FileFinished { file, ok } => self.on_file_finished(file, ok),
        }
    }
}

/// The same request the idle lock makes. By instance, as the daemon can outlive a Hyprland session.
fn lock_screen() {
    run_detached(Process::new("hyprctl").args(["-i", "0", "dispatch", r#"hl.dsp.global("quickshell:lock")"#]));
}

/// `--` keeps text from the phone from being read as options.
fn notify(title: &str, body: &str) {
    run_detached(Process::new("notify-send").args(["-a", "Lis App", "--", title, body]));
}

fn run_detached(command: &mut Process) {
    match command.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn() {
        Ok(mut child) => {
            tokio::spawn(async move {
                let _ = child.wait().await;
            });
        }
        Err(error) => eprintln!("lis-daemon: can't run a helper: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::{battery_alert, reserve, BatteryAlerts};
    use lis_core::protocol::PhoneStats;

    fn stats(percent: u8, charging: bool) -> PhoneStats {
        PhoneStats { battery_percent: percent, charging, ..Default::default() }
    }

    /// The titles of the alerts a run of readings sets off.
    fn alerts_for(readings: &[(u8, bool)]) -> Vec<&'static str> {
        let mut alerts = BatteryAlerts::default();
        readings.iter().filter_map(|(percent, charging)| battery_alert(&mut alerts, &stats(*percent, *charging)).map(|(title, _)| title)).collect()
    }

    #[test]
    fn a_falling_battery_warns_once_at_each_level() {
        let readings = [(50, false), (21, false), (20, false), (19, false), (11, false), (10, false), (9, false)];
        assert_eq!(alerts_for(&readings), ["Phone battery low", "Phone battery very low"]);
    }

    #[test]
    fn already_low_when_first_heard_from_still_warns() {
        assert_eq!(alerts_for(&[(15, false), (14, false)]), ["Phone battery low"]);
        assert_eq!(alerts_for(&[(5, false), (4, false)]), ["Phone battery very low"]);
    }

    #[test]
    fn charging_a_little_and_unplugging_does_not_warn_again() {
        assert_eq!(alerts_for(&[(18, false), (19, true), (19, false), (18, false)]), ["Phone battery low"]);
    }

    #[test]
    fn a_recovered_battery_can_warn_again() {
        let readings = [(18, false), (60, true), (60, false), (20, false)];
        assert_eq!(alerts_for(&readings), ["Phone battery low", "Phone battery low"]);
    }

    #[test]
    fn charged_is_said_once_per_charge() {
        let readings = [(99, true), (100, true), (99, true), (100, true), (100, false), (99, true), (100, true)];
        assert_eq!(alerts_for(&readings), ["Phone charged", "Phone charged"]);
    }

    #[test]
    fn plugged_in_and_full_when_first_heard_from_is_no_news() {
        assert!(alerts_for(&[(100, true), (100, true)]).is_empty());
    }

    #[test]
    fn reserving_never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"keep").unwrap();

        let (_file, part, done) = reserve(dir.path(), "a.txt").unwrap();
        assert_eq!(done.file_name().unwrap(), "a (1).txt");
        assert_eq!(part.file_name().unwrap(), "a (1).txt.part");

        let (_file, _part, done) = reserve(dir.path(), "a.txt").unwrap();
        assert_eq!(done.file_name().unwrap(), "a (2).txt");
        assert_eq!(std::fs::read(dir.path().join("a.txt")).unwrap(), b"keep");

        let (_file, _part, done) = reserve(dir.path(), "noext").unwrap();
        assert_eq!(done.file_name().unwrap(), "noext");
    }

    #[test]
    fn reserving_refuses_to_follow_a_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        std::os::unix::fs::symlink(&target, dir.path().join("x.part")).unwrap();
        let (_file, part, done) = reserve(dir.path(), "x").unwrap();
        assert_eq!(done.file_name().unwrap(), "x (1)");
        assert_eq!(part.file_name().unwrap(), "x (1).part");
        assert!(!target.exists());
    }
}
