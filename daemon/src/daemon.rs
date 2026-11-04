//! The state behind the connections, and what each event from a phone does on this PC.

use crate::stats::{hostname, Sampler};
use lis_core::identity::{write_private, Identity};
use lis_core::names::safe_file_name;
use lis_core::protocol::{FileOffer, Message, PcStats, PhoneStats};
use lis_core::session::{Command, Event, Handle, Handler, Peer};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::process::Command as Process;
use tokio::sync::{broadcast, Notify};
use tokio::time::Instant;

pub struct Paths {
    pub data: PathBuf,
    pub socket: PathBuf,
    pub downloads: PathBuf,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct TrustedPhone {
    fingerprint: String,
    name: String,
    paired_at_ms: i64,
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
}

pub struct Daemon {
    pub identity: Identity,
    pub name: String,
    pub port: u16,
    pub paths: Paths,
    pub shutdown: Notify,
    state: Mutex<State>,
    events: broadcast::Sender<String>,
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
        Self {
            identity,
            name: hostname(),
            port,
            paths,
            shutdown: Notify::new(),
            state: Mutex::new(State { trusted, ..Default::default() }),
            events,
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

    fn publish_status(&self) {
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
        handle.send(Command::SendFile { file, name, size: metadata.len() }).await;
        Ok(())
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
        let mut greeting = None;
        if paired {
            let stats = state.pc_stats.clone();
            if let Some(phone) = state.trusted.iter_mut().find(|phone| phone.fingerprint == peer.fingerprint) {
                if phone.name != peer.name {
                    phone.name = peer.name.clone();
                }
            }
            if let Some(link) = state.links.get_mut(&peer.fingerprint) {
                link.paired = true;
                greeting = Some((link.handle.clone(), stats));
            }
        }
        drop(state);
        if let Some((handle, stats)) = greeting {
            tokio::spawn(async move { handle.send(Command::Send(Message::PcStats(stats))).await });
        }
        self.publish_status();
    }

    fn on_file_finished(&self, file: lis_core::session::FileStatus, ok: bool) {
        let mut saved_as = None;
        if file.incoming {
            let reserved = self.state.lock().unwrap().incoming.remove(&(file.peer.fingerprint.clone(), file.id));
            if let Some(reserved) = reserved {
                if ok && fs::rename(&reserved.part, &reserved.done).is_ok() {
                    saved_as = Some(reserved.done);
                } else {
                    let _ = fs::remove_file(&reserved.part);
                }
            }
        }

        let delivered = if file.incoming { saved_as.is_some() } else { ok };
        if let Some(path) = &saved_as {
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
                "path": saved_as.map(|path| path.display().to_string()),
            }),
        );
    }

    fn on_message(&self, peer: Peer, message: Message) {
        match message {
            Message::LockPc => {
                lock_screen();
                self.publish("locked", json!({ "by": peer.name }));
            }
            Message::PhoneStats(stats) => {
                self.state.lock().unwrap().stats.insert(peer.fingerprint.clone(), stats);
                self.publish_status();
            }
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
        state.trusted.push(TrustedPhone { fingerprint: peer.fingerprint.clone(), name: peer.name.clone(), paired_at_ms: now_ms() });
        state.pairing_until = None;
        state.pair_code = None;
        self.save_trusted(&state);
    }

    fn forget(&self, fingerprint: &str) {
        let mut state = self.state.lock().unwrap();
        state.trusted.retain(|phone| phone.fingerprint != fingerprint);
        state.stats.remove(fingerprint);
        if let Some(link) = state.links.get_mut(fingerprint) {
            link.paired = false;
        }
        self.save_trusted(&state);
    }

    fn open_incoming(&self, peer: &Peer, offer: &FileOffer) -> Option<fs::File> {
        let name = safe_file_name(&offer.name);
        match reserve(&self.paths.downloads, &name) {
            Ok((file, part, done)) => {
                self.state.lock().unwrap().incoming.insert((peer.fingerprint.clone(), offer.id), IncomingFile { part, done });
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
    use super::reserve;

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
