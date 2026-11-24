//! Keeps this PC on the relay server, when one is set, and takes each call from a phone there.

use crate::daemon::Daemon;
use lis_core::relay::{self, read_line};
use lis_core::tls;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::timeout;

const MAX_RETRY_DELAY: Duration = Duration::from_secs(60);
/// The relay pings every 30 s.
const SILENCE_LIMIT: Duration = Duration::from_secs(75);

#[derive(Clone, Debug, PartialEq, Default)]
pub struct RelaySettings {
    pub host: String,
    pub port: u16,
    pub password: String,
}

impl RelaySettings {
    pub fn is_set(&self) -> bool {
        !self.host.is_empty() && self.port != 0 && !self.password.is_empty()
    }
}

pub async fn run(daemon: Arc<Daemon>) {
    let Ok(acceptor) = tls::acceptor(&daemon.identity) else { return };
    let fingerprint = daemon.identity.fingerprint();
    let changed = daemon.relay_changed();
    let mut delay = Duration::from_secs(1);
    loop {
        let settings = daemon.relay_settings();
        if !settings.is_set() {
            daemon.set_relay_state("off", "");
            changed.notified().await;
            continue;
        }
        daemon.set_relay_state("connecting", "");
        let listened = tokio::select! {
            result = listen(&daemon, &acceptor, &settings, &fingerprint) => result,
            _ = changed.notified() => {
                delay = Duration::from_secs(1);
                continue;
            }
        };
        match listened {
            // It was up for a while, so try again soon
            Ok(()) => delay = Duration::from_secs(1),
            Err(error) => daemon.set_relay_state("error", &error.to_string()),
        }
        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            _ = changed.notified() => delay = Duration::from_secs(1),
        }
        delay = (delay * 2).min(MAX_RETRY_DELAY);
    }
}

/// Until the relay goes away. An error if it never let this PC on.
async fn listen(daemon: &Arc<Daemon>, acceptor: &tokio_rustls::TlsAcceptor, settings: &RelaySettings, fingerprint: &str) -> std::io::Result<()> {
    let mut calls = relay::listen(&settings.host, settings.port, &settings.password, fingerprint).await?;
    daemon.set_relay_state("on", "");
    loop {
        let line = match timeout(SILENCE_LIMIT, read_line(&mut calls)).await {
            Ok(Ok(line)) => line,
            _ => {
                daemon.set_relay_state("connecting", "");
                return Ok(());
            }
        };
        let Some(token) = line.strip_prefix("call ") else { continue };
        let (daemon, acceptor, settings, token) = (daemon.clone(), acceptor.clone(), settings.clone(), token.to_string());
        tokio::spawn(async move {
            if let Ok(tcp) = relay::take(&settings.host, settings.port, &settings.password, &token).await {
                crate::serve_phone(tcp, &acceptor, daemon, true).await;
            }
        });
    }
}
