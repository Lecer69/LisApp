//! The PC side of Lis App: listens for the phone on the local network and serves the shell.

mod camera;
mod daemon;
mod ipc;
mod stats;

use anyhow::Context;
use daemon::{Daemon, Paths};
use lis_core::identity::Identity;
use lis_core::protocol::{DEFAULT_PORT, SERVICE_TYPE};
use lis_core::session::{self, Handle, Local, Role, Timing};
use lis_core::tls;
use std::env;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::Semaphore;

/// Enough for real phones; keeps strangers on the network from opening connections without limit.
const MAX_CONNECTIONS: usize = 32;
const STATS_EVERY: Duration = Duration::from_secs(30);

fn paths() -> anyhow::Result<Paths> {
    let home = env::var_os("HOME").map(PathBuf::from).context("HOME isn't set")?;
    let data = env::var_os("XDG_DATA_HOME").map_or_else(|| home.join(".local/share"), PathBuf::from).join("lis-app");
    let runtime = env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).context("XDG_RUNTIME_DIR isn't set")?;
    Ok(Paths {
        data,
        socket: runtime.join("lis-app.sock"),
        camera_socket: runtime.join("lis-camera.sock"),
        downloads: home.join("Downloads"),
    })
}

fn advertise(identity_fp: &str, name: &str, port: u16) -> Option<mdns_sd::ServiceDaemon> {
    let mdns = mdns_sd::ServiceDaemon::new().map_err(|error| eprintln!("lis-daemon: no discovery: {error}")).ok()?;
    let host: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' }).collect();
    let properties = [("fp", &identity_fp[..16]), ("name", name), ("v", "1")];
    let info = mdns_sd::ServiceInfo::new(SERVICE_TYPE, name, &format!("{host}.local."), "", port, &properties[..])
        .map_err(|error| eprintln!("lis-daemon: no discovery: {error}"))
        .ok()?
        .enable_addr_auto();
    mdns.register(info).map_err(|error| eprintln!("lis-daemon: no discovery: {error}")).ok()?;
    Some(mdns)
}

async fn listen(port: u16) -> std::io::Result<TcpListener> {
    // Dual stack where IPv6 exists
    match TcpListener::bind(SocketAddr::from(([0u16; 8], port))).await {
        Ok(listener) => Ok(listener),
        Err(_) => TcpListener::bind(SocketAddr::from(([0u8; 4], port))).await,
    }
}

async fn accept_phones(listener: TcpListener, daemon: Arc<Daemon>) -> anyhow::Result<()> {
    let acceptor = tls::acceptor(&daemon.identity)?;
    let limit = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    let fingerprint = daemon.identity.fingerprint();

    loop {
        let (tcp, _) = listener.accept().await?;
        let Ok(permit) = limit.clone().try_acquire_owned() else { continue };
        let (acceptor, daemon, fingerprint) = (acceptor.clone(), daemon.clone(), fingerprint.clone());

        tokio::spawn(async move {
            let _permit = permit;
            let Ok((stream, peer)) = tls::accept(&acceptor, tcp).await else { return };
            if tls::client_is_video(&stream) {
                // Never a way in: only a phone that's already paired can send video
                if let Some(name) = daemon.phone_name(&peer) {
                    camera::serve_phone(stream, peer, name, daemon.clone()).await;
                }
                return;
            }
            let (handle, commands) = Handle::channel();
            let generation = daemon.register(&peer, handle);
            let local = Local { name: daemon.name.clone(), fingerprint, role: Role::Server, timing: Timing::default() };
            let result = session::run(stream, local, peer.clone(), daemon.clone(), commands).await;
            if let Err(lis_core::session::SessionError::Protocol(what)) = &result {
                eprintln!("lis-daemon: dropped a connection: {what}");
            }
            daemon.unregister(&peer, generation);
        });
    }
}

async fn run() -> anyhow::Result<()> {
    let paths = paths()?;
    std::fs::create_dir_all(&paths.data)?;
    let socket = paths.socket.clone();
    let camera_socket = paths.camera_socket.clone();
    let ipc = ipc::bind(&socket).await?;
    let viewers = camera::bind(&camera_socket)?;

    let identity = Identity::load_or_create(&paths.data)?;
    let port = env::var("LIS_APP_PORT").ok().and_then(|port| port.parse().ok()).unwrap_or(DEFAULT_PORT);
    let listener = listen(port).await.with_context(|| format!("can't listen on port {port}"))?;
    let port = listener.local_addr()?.port();

    let daemon = Arc::new(Daemon::new(identity, port, paths));
    let mdns = if env::var_os("LIS_APP_NO_MDNS").is_some() {
        None
    } else {
        advertise(&daemon.identity.fingerprint(), &daemon.name, port)
    };

    tokio::spawn(ipc::serve(ipc, daemon.clone()));
    tokio::spawn(camera::serve_viewers(viewers, daemon.clone()));
    {
        let daemon = daemon.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(camera::STATS_EVERY);
            loop {
                ticker.tick().await;
                daemon.camera_tick();
            }
        });
    }
    let phones = tokio::spawn(accept_phones(listener, daemon.clone()));
    let stats = {
        let daemon = daemon.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(STATS_EVERY);
            loop {
                ticker.tick().await;
                daemon.send_stats().await;
            }
        })
    };

    let mut terminate = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    tokio::select! {
        _ = daemon.shutdown.notified() => {}
        _ = terminate.recv() => {}
        _ = interrupt.recv() => {}
        result = phones => {
            eprintln!("lis-daemon: stopped listening: {result:?}");
        }
    }

    stats.abort();
    if let Some(mdns) = mdns {
        let _ = mdns.shutdown();
    }
    let _ = std::fs::remove_file(&socket);
    let _ = std::fs::remove_file(&camera_socket);
    Ok(())
}

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();

    let result = match args.as_slice() {
        [] | ["run"] => run().await.map(|_| true),
        ["ctl", request] => match paths() {
            Ok(paths) => ipc::ctl(&paths.socket, request).await,
            Err(error) => Err(error),
        },
        ["--version"] => {
            println!("lis-daemon {}", env!("CARGO_PKG_VERSION"));
            Ok(true)
        }
        _ => {
            eprintln!("usage: lis-daemon [run] | lis-daemon ctl '<json>' | lis-daemon --version");
            Ok(false)
        }
    };

    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("lis-daemon: {error:#}");
            ExitCode::FAILURE
        }
    }
}

