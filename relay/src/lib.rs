//! lis-relay: joins a phone and its PC when they can't reach each other directly. It runs on a server
//! both can reach, and only passes bytes along: the devices' TLS runs inside, end to end. See
//! `lis_core::relay` for the requests, and `config` for what can be set.

pub mod config;

use config::{Log, Settings};
use lis_core::relay::{mac_matches, read_line, write_line, GREETING};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot, Semaphore};
use tokio::time::{sleep, sleep_until, timeout, Instant};

const BUFFER: usize = 16 * 1024;
const CLEANUP_EVERY: Duration = Duration::from_secs(60);

macro_rules! log {
    ($relay:expr, $level:expr, $($arg:tt)*) => {
        if $relay.settings.config.server.log >= $level {
            eprintln!($($arg)*);
        }
    };
}

/// Spaces out writes so they average `bytes_per_sec`. Shared by every tunnel it's given to.
struct Pacer {
    bytes_per_sec: u64,
    next: Mutex<Option<Instant>>,
}

impl Pacer {
    fn new(kbps: u64) -> Option<Arc<Self>> {
        (kbps > 0).then(|| Arc::new(Self { bytes_per_sec: kbps * 1024, next: Mutex::new(None) }))
    }

    async fn pass(&self, bytes: usize) {
        let due = {
            let mut next = self.next.lock().unwrap();
            let now = Instant::now();
            // Unused time isn't saved up, so a quiet tunnel can't burst past the limit later
            let start = next.filter(|at| *at > now).unwrap_or(now);
            let due = start + Duration::from_secs_f64(bytes as f64 / self.bytes_per_sec as f64);
            *next = Some(due);
            start
        };
        sleep_until(due).await;
    }
}

#[derive(Default)]
struct Usage {
    day: u64,
    bytes: u64,
}

#[derive(Default)]
struct State {
    /// A listening PC, by fingerprint, and where to tell it about calls.
    listeners: HashMap<String, (u64, mpsc::Sender<String>)>,
    /// Calls waiting for the PC to take them.
    pending: HashMap<String, oneshot::Sender<TcpStream>>,
    tunnels: HashMap<String, usize>,
    tunnels_total: usize,
    next_listener: u64,
    per_ip: HashMap<IpAddr, usize>,
    failures: HashMap<IpAddr, Vec<Instant>>,
    bans: HashMap<IpAddr, Instant>,
    usage: HashMap<String, Usage>,
}

pub struct Relay {
    settings: Settings,
    state: Mutex<State>,
    /// Phone to PC, and PC to phone, for all tunnels together.
    total_up: Option<Arc<Pacer>>,
    total_down: Option<Arc<Pacer>>,
    moved: AtomicU64,
}

fn today() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |time| time.as_secs() / 86_400)
}

fn short(pc: &str) -> &str {
    &pc[..pc.len().min(16)]
}

fn is_fingerprint(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Frees the address's connection slot when the connection ends.
struct IpSlot {
    relay: Arc<Relay>,
    ip: IpAddr,
}

impl Drop for IpSlot {
    fn drop(&mut self) {
        let mut state = self.relay.state.lock().unwrap();
        if let Some(count) = state.per_ip.get_mut(&self.ip) {
            *count -= 1;
            if *count == 0 {
                state.per_ip.remove(&self.ip);
            }
        }
    }
}

/// One phone's tunnel, counted until it ends.
struct TunnelSlot {
    relay: Arc<Relay>,
    pc: String,
}

impl Drop for TunnelSlot {
    fn drop(&mut self) {
        let mut state = self.relay.state.lock().unwrap();
        state.tunnels_total -= 1;
        if let Some(count) = state.tunnels.get_mut(&self.pc) {
            *count -= 1;
            if *count == 0 {
                state.tunnels.remove(&self.pc);
            }
        }
    }
}

impl Relay {
    pub fn new(settings: Settings) -> Arc<Self> {
        let kbps = settings.config.limits.total_kbps;
        Arc::new(Self { total_up: Pacer::new(kbps), total_down: Pacer::new(kbps), settings, state: Mutex::default(), moved: AtomicU64::new(0) })
    }

    fn banned(&self, ip: IpAddr) -> bool {
        let mut state = self.state.lock().unwrap();
        match state.bans.get(&ip) {
            Some(until) if *until > Instant::now() => true,
            Some(_) => {
                state.bans.remove(&ip);
                false
            }
            None => false,
        }
    }

    fn take_ip_slot(self: &Arc<Self>, ip: IpAddr) -> Option<IpSlot> {
        let mut state = self.state.lock().unwrap();
        let count = state.per_ip.entry(ip).or_default();
        if *count >= self.settings.config.limits.max_connections_per_ip {
            return None;
        }
        *count += 1;
        Some(IpSlot { relay: self.clone(), ip })
    }

    /// Counts towards a ban, and slows the answer down.
    async fn fail(&self, mut tcp: TcpStream, ip: IpAddr, reason: &str) {
        let auth = &self.settings.config.auth;
        if auth.max_failures > 0 {
            let window = Duration::from_secs(auth.failure_window_minutes * 60);
            let mut state = self.state.lock().unwrap();
            let now = Instant::now();
            let list = state.failures.entry(ip).or_default();
            list.retain(|at| now.duration_since(*at) < window);
            list.push(now);
            if list.len() >= auth.max_failures as usize {
                state.failures.remove(&ip);
                state.bans.insert(ip, now + Duration::from_secs(auth.ban_minutes * 60));
                drop(state);
                log!(self, Log::Info, "banned {ip} for {} minutes after {} failures", auth.ban_minutes, auth.max_failures);
            }
        }
        log!(self, Log::Debug, "refused {ip}: {reason}");
        sleep(self.settings.failure_delay()).await;
        let _ = write_line(&mut tcp, &format!("err {reason}")).await;
    }

    /// Phones are turned away once a PC has used its share for the day.
    fn over_quota(&self, pc: &str) -> bool {
        let Some(limit) = self.settings.daily_bytes_per_pc() else { return false };
        let state = self.state.lock().unwrap();
        state.usage.get(pc).is_some_and(|usage| usage.day == today() && usage.bytes >= limit)
    }

    /// False once the PC is over its share.
    fn count(&self, pc: &str, bytes: u64) -> bool {
        self.moved.fetch_add(bytes, Ordering::Relaxed);
        let Some(limit) = self.settings.daily_bytes_per_pc() else { return true };
        let mut state = self.state.lock().unwrap();
        let usage = state.usage.entry(pc.to_string()).or_default();
        let day = today();
        if usage.day != day {
            *usage = Usage { day, bytes: 0 };
        }
        usage.bytes += bytes;
        usage.bytes <= limit
    }

    /// Keeps the tables from growing with addresses that have gone.
    fn clean_up(&self) {
        let now = Instant::now();
        let window = Duration::from_secs(self.settings.config.auth.failure_window_minutes * 60);
        let day = today();
        let mut state = self.state.lock().unwrap();
        state.bans.retain(|_, until| *until > now);
        state.failures.retain(|_, list| {
            list.retain(|at| now.duration_since(*at) < window);
            !list.is_empty()
        });
        state.usage.retain(|_, usage| usage.day == day);
    }

    fn summary(&self) -> String {
        let state = self.state.lock().unwrap();
        let mb = self.moved.load(Ordering::Relaxed) as f64 / (1024.0 * 1024.0);
        format!("{} PCs waiting, {} tunnels, {} banned, {mb:.1} MiB moved", state.listeners.len(), state.tunnels_total, state.bans.len())
    }
}

async fn serve(relay: Arc<Relay>, mut tcp: TcpStream, ip: IpAddr) {
    let _ = tcp.set_nodelay(true);
    let nonce = hex::encode(rand::random::<[u8; 16]>());
    let request = timeout(relay.settings.request_limit(), async {
        write_line(&mut tcp, &format!("{GREETING} {nonce}")).await?;
        read_line(&mut tcp).await
    })
    .await;
    let Ok(Ok(line)) = request else { return };
    let parts: Vec<&str> = line.split(' ').collect();
    let [verb, arg, mac] = parts.as_slice() else { return relay.fail(tcp, ip, "bad request").await };
    if !mac_matches(&relay.settings.password, &nonce, verb, arg, mac) {
        return relay.fail(tcp, ip, "wrong relay password").await;
    }
    match *verb {
        "listen" | "dial" if is_fingerprint(arg) => {
            let pc = arg.to_lowercase();
            if !relay.settings.pc_allowed(&pc) {
                return relay.fail(tcp, ip, "that PC isn't allowed on this relay").await;
            }
            if *verb == "listen" {
                listen(relay, tcp, pc, ip).await;
            } else {
                dial(relay, tcp, pc, ip).await;
            }
        }
        "take" => take(relay, tcp, arg, ip).await,
        _ => relay.fail(tcp, ip, "bad request").await,
    }
}

/// A newer listener for the same PC replaces the older, as after the PC's network changed.
async fn listen(relay: Arc<Relay>, mut tcp: TcpStream, pc: String, ip: IpAddr) {
    let (calls, mut heard) = mpsc::channel::<String>(8);
    let id = {
        let mut state = relay.state.lock().unwrap();
        if !state.listeners.contains_key(&pc) && state.listeners.len() >= relay.settings.config.limits.max_pcs {
            None
        } else {
            state.next_listener += 1;
            let id = state.next_listener;
            state.listeners.insert(pc.clone(), (id, calls));
            Some(id)
        }
    };
    let Some(id) = id else {
        log!(relay, Log::Info, "turned away PC {} from {ip}: the relay has max_pcs", short(&pc));
        let _ = write_line(&mut tcp, "err the relay is full").await;
        return;
    };
    log!(relay, Log::Info, "PC {} is waiting, from {ip}", short(&pc));

    if write_line(&mut tcp, "ok").await.is_ok() {
        let (mut reader, mut writer) = tcp.into_split();
        let mut ping = tokio::time::interval(relay.settings.listener_ping());
        let mut byte = [0u8; 1];
        loop {
            let line = tokio::select! {
                call = heard.recv() => match call {
                    Some(call) => call,
                    None => break,
                },
                _ = ping.tick() => "ping".to_string(),
                // The PC says nothing on this connection, so a read only ends with it
                _ = reader.read(&mut byte) => break,
            };
            if timeout(relay.settings.request_limit(), write_line(&mut writer, &line)).await.map_or(true, |written| written.is_err()) {
                break;
            }
        }
    }
    let mut state = relay.state.lock().unwrap();
    if state.listeners.get(&pc).is_some_and(|(current, _)| *current == id) {
        state.listeners.remove(&pc);
        drop(state);
        log!(relay, Log::Info, "PC {} left", short(&pc));
    }
}

async fn dial(relay: Arc<Relay>, mut phone: TcpStream, pc: String, ip: IpAddr) {
    let token = hex::encode(rand::random::<[u8; 16]>());
    let (answer, answered) = oneshot::channel();
    let found = if relay.over_quota(&pc) {
        Err("that PC has used its data for today")
    } else {
        let limits = &relay.settings.config.limits;
        let mut state = relay.state.lock().unwrap();
        match state.listeners.get(&pc).cloned() {
            None => Err("the PC isn't on the relay"),
            Some(_) if state.tunnels_total >= limits.max_tunnels => Err("the relay is full"),
            Some(_) if state.tunnels.get(&pc).copied().unwrap_or(0) >= limits.max_tunnels_per_pc => Err("too many connections to that PC"),
            Some((_, calls)) => {
                state.pending.insert(token.clone(), answer);
                Ok(calls)
            }
        }
    };
    let calls = match found {
        Ok(calls) => calls,
        Err(reason) => {
            log!(relay, Log::Debug, "phone at {ip} can't reach PC {}: {reason}", short(&pc));
            let _ = write_line(&mut phone, &format!("err {reason}")).await;
            return;
        }
    };
    let sent = calls.send(format!("call {token}")).await.is_ok();
    let pc_side = if sent { timeout(relay.settings.answer_limit(), answered).await.ok().and_then(Result::ok) } else { None };
    relay.state.lock().unwrap().pending.remove(&token);
    let Some(mut pc_side) = pc_side else {
        let _ = write_line(&mut phone, "err the PC didn't answer").await;
        return;
    };

    // Checked again, as others may have connected while the PC answered
    let slot = {
        let limits = &relay.settings.config.limits;
        let mut state = relay.state.lock().unwrap();
        let per_pc = state.tunnels.get(&pc).copied().unwrap_or(0);
        if state.tunnels_total >= limits.max_tunnels || per_pc >= limits.max_tunnels_per_pc {
            None
        } else {
            state.tunnels_total += 1;
            *state.tunnels.entry(pc.clone()).or_default() += 1;
            Some(TunnelSlot { relay: relay.clone(), pc: pc.clone() })
        }
    };
    let Some(_slot) = slot else {
        let _ = write_line(&mut phone, "err the relay is full").await;
        return;
    };
    if write_line(&mut pc_side, "ok").await.is_err() || write_line(&mut phone, "ok").await.is_err() {
        return;
    }
    log!(relay, Log::Info, "phone at {ip} connected to PC {}", short(&pc));

    let started = Instant::now();
    let tunnel_kbps = relay.settings.config.limits.tunnel_kbps;
    let (phone_read, phone_write) = phone.into_split();
    let (pc_read, pc_write) = pc_side.into_split();
    let up = pump(&relay, &pc, phone_read, pc_write, Pacer::new(tunnel_kbps), relay.total_up.clone());
    let down = pump(&relay, &pc, pc_read, phone_write, Pacer::new(tunnel_kbps), relay.total_down.clone());
    let ended = tokio::select! {
        reason = up => reason,
        reason = down => reason,
        _ = sleep(relay.settings.tunnel_length().unwrap_or(Duration::MAX / 4)), if relay.settings.tunnel_length().is_some() => "it reached max_tunnel_hours",
    };
    log!(relay, Log::Info, "phone at {ip} left PC {} after {} s: {ended}", short(&pc), started.elapsed().as_secs());
}

/// The dial answers the PC once both sides are there.
async fn take(relay: Arc<Relay>, tcp: TcpStream, token: &str, ip: IpAddr) {
    let waiting = relay.state.lock().unwrap().pending.remove(token);
    match waiting {
        Some(waiting) => {
            let _ = waiting.send(tcp);
        }
        None => relay.fail(tcp, ip, "no such call").await,
    }
}

/// Copies one way until either side ends, goes quiet or runs out of quota. Says why it stopped.
async fn pump(
    relay: &Relay,
    pc: &str,
    mut reader: OwnedReadHalf,
    mut writer: OwnedWriteHalf,
    own: Option<Arc<Pacer>>,
    shared: Option<Arc<Pacer>>,
) -> &'static str {
    let mut buffer = vec![0u8; BUFFER];
    let reason = loop {
        let count = match timeout(relay.settings.idle_limit(), reader.read(&mut buffer)).await {
            Ok(Ok(0)) => break "closed",
            Ok(Err(_)) => break "connection lost",
            Err(_) => break "silent for idle_secs",
            Ok(Ok(count)) => count,
        };
        for pacer in [&own, &shared].into_iter().flatten() {
            pacer.pass(count).await;
        }
        if writer.write_all(&buffer[..count]).await.is_err() {
            break "connection lost";
        }
        if !relay.count(pc, count as u64) {
            break "the PC used its data for today";
        }
    };
    let _ = writer.shutdown().await;
    reason
}

/// Serves until the listener fails.
pub async fn run(listener: TcpListener, settings: Settings) {
    let relay = Relay::new(settings);
    let limit = Arc::new(Semaphore::new(relay.settings.config.limits.max_connections));
    {
        let relay = relay.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(CLEANUP_EVERY);
            let mut ticks = 0u64;
            loop {
                ticker.tick().await;
                relay.clean_up();
                ticks += 1;
                if ticks % 60 == 0 {
                    log!(relay, Log::Info, "{}", relay.summary());
                }
            }
        });
    }
    loop {
        let Ok((tcp, address)) = listener.accept().await else { continue };
        let ip = SocketAddr::ip(&address).to_canonical();
        if !relay.settings.ip_allowed(ip) || relay.banned(ip) {
            log!(relay, Log::Debug, "dropped {ip}: not allowed or banned");
            continue;
        }
        let Ok(permit) = limit.clone().try_acquire_owned() else {
            log!(relay, Log::Debug, "dropped {ip}: max_connections");
            continue;
        };
        let Some(slot) = relay.take_ip_slot(ip) else {
            log!(relay, Log::Debug, "dropped {ip}: max_connections_per_ip");
            continue;
        };
        let relay = relay.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let _slot = slot;
            serve(relay, tcp, ip).await;
        });
    }
}
