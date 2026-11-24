//! The relay server's command. Settings come from a TOML file; see deploy/config.toml.

use lis_relay::config::{Settings, EXAMPLE};
use std::env;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use tokio::net::TcpListener;

const DEFAULT_CONFIG: &str = "/etc/lis-relay/config.toml";

const USAGE: &str = "usage: lis-relay [--config <file>] [--check]
       lis-relay --print-config   prints an example config.toml
       lis-relay --version

Reads /etc/lis-relay/config.toml, or the file in --config or LIS_RELAY_CONFIG. --check only checks it.
Without a file, LIS_RELAY_PASSWORD (or LIS_RELAY_PASSWORD_FILE) and LIS_RELAY_PORT are enough.";

enum Run {
    Serve { config: Option<PathBuf>, check: bool },
    Print(&'static str),
}

fn parse_args() -> Result<Run, String> {
    let mut config = env::var_os("LIS_RELAY_CONFIG").map(PathBuf::from);
    let mut check = false;
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-c" | "--config" => config = Some(args.next().ok_or("--config needs a file")?.into()),
            "--check" => check = true,
            "--print-config" => return Ok(Run::Print(EXAMPLE)),
            "--version" => return Ok(Run::Print(concat!("lis-relay ", env!("CARGO_PKG_VERSION"), "\n"))),
            "-h" | "--help" => return Ok(Run::Print(USAGE)),
            other => return Err(format!("unknown option {other}\n{USAGE}")),
        }
    }
    Ok(Run::Serve { config, check })
}

/// The file when there's one, or the environment for a quick run without one.
fn settings(config: Option<PathBuf>) -> Result<Settings, String> {
    let path = config.unwrap_or_else(|| PathBuf::from(DEFAULT_CONFIG));
    if path.exists() {
        return Settings::load(&path);
    }
    let from_env = env::var_os("LIS_RELAY_PASSWORD").is_some() || env::var_os("LIS_RELAY_PASSWORD_FILE").is_some();
    if !from_env {
        return Err(format!("no {} (lis-relay --print-config makes one)", path.display()));
    }
    let mut config = lis_relay::config::Config::default();
    config.auth.password = env::var("LIS_RELAY_PASSWORD").unwrap_or_default();
    config.auth.password_file = env::var("LIS_RELAY_PASSWORD_FILE").ok();
    if let Some(port) = env::var("LIS_RELAY_PORT").ok().and_then(|port| port.parse().ok()) {
        config.server.port = port;
    }
    if let Some(kbps) = env::var("LIS_RELAY_KBPS").ok().and_then(|kbps| kbps.parse().ok()) {
        config.limits.tunnel_kbps = kbps;
    }
    Settings::from_config(config)
}

async fn serve(settings: Settings) -> Result<(), String> {
    let server = &settings.config.server;
    let address = SocketAddr::new(server.address, server.port);
    let listener = match TcpListener::bind(address).await {
        Ok(listener) => listener,
        // No IPv6 on this server
        Err(_) if server.address.is_unspecified() && server.address.is_ipv6() => {
            TcpListener::bind(SocketAddr::from(([0u8; 4], server.port))).await.map_err(|error| format!("can't listen on port {}: {error}", server.port))?
        }
        Err(error) => return Err(format!("can't listen on {address}: {error}")),
    };
    let bound = listener.local_addr().map_err(|error| error.to_string())?;
    eprintln!("lis-relay {}: listening on {bound}", env!("CARGO_PKG_VERSION"));

    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).map_err(|error| error.to_string())?;
    tokio::select! {
        _ = lis_relay::run(listener, settings) => {}
        _ = terminate.recv() => {}
        _ = tokio::signal::ctrl_c() => {}
    }
    eprintln!("lis-relay: stopped");
    Ok(())
}

#[tokio::main]
async fn main() -> ExitCode {
    let result = match parse_args() {
        Ok(Run::Print(text)) => {
            print!("{text}");
            Ok(())
        }
        Ok(Run::Serve { config, check }) => match settings(config) {
            Ok(_) if check => {
                println!("lis-relay: the config is fine");
                Ok(())
            }
            Ok(settings) => serve(settings).await,
            Err(error) => Err(error),
        },
        Err(error) => Err(error),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("lis-relay: {error}");
            ExitCode::FAILURE
        }
    }
}
