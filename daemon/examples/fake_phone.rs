//! A stand-in for the Android app, for working on the PC side without a phone. It pairs as soon as
//! the PC lets it, accepts the code by itself, then sends figures.
//!
//! cargo run -p lis-daemon --example fake_phone -- [host] [port]

use lis_ffi::{ClientListener, ConnectionState, LisClient, PcStats, PhoneStats};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

static CLIENT: OnceLock<Arc<LisClient>> = OnceLock::new();

fn client() -> &'static LisClient {
    CLIENT.get().expect("client is set before connecting")
}

struct Listener;

impl ClientListener for Listener {
    fn on_state(&self, state: ConnectionState, pc_name: Option<String>) {
        println!("state: {state:?} {}", pc_name.unwrap_or_default());
        if state == ConnectionState::Unpaired {
            client().start_pairing();
        }
        if state == ConnectionState::Connected {
            client().send_stats(PhoneStats {
                battery_percent: 87,
                charging: false,
                storage_used: 120 * 1024 * 1024 * 1024,
                storage_total: 256 * 1024 * 1024 * 1024,
                memory_used: 5 * 1024 * 1024 * 1024,
                memory_total: 8 * 1024 * 1024 * 1024,
                network: "Wi-Fi".to_string(),
            });
        }
    }
    fn on_pair_code(&self, code: String) {
        println!("code: {code}");
        client().confirm_pairing(true);
    }
    fn on_pair_result(&self, ok: bool, reason: String) {
        println!("paired: {ok} {reason}");
        if !ok {
            std::thread::spawn(|| {
                std::thread::sleep(Duration::from_secs(2));
                client().start_pairing();
            });
        }
    }
    fn on_pc_stats(&self, stats: PcStats) {
        println!("pc: {} cpu {:.0}%", stats.hostname, stats.cpu_percent);
    }
    fn open_incoming_file(&self, _id: u64, name: String, size: u64) -> i32 {
        println!("refusing {name} ({size} bytes)");
        -1
    }
    fn on_file_progress(&self, _id: u64, _name: String, _incoming: bool, _done: u64, _total: u64) {}
    fn on_file_finished(&self, _id: u64, name: String, _incoming: bool, ok: bool) {
        println!("file {name}: {ok}");
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let host = args.first().cloned().unwrap_or_else(|| "127.0.0.1".to_string());
    let port = args.get(1).and_then(|port| port.parse().ok()).unwrap_or(lis_core::protocol::DEFAULT_PORT);

    let dir = std::env::temp_dir().join(format!("lis-fake-phone-{}", std::process::id()));
    let client = LisClient::new(dir.display().to_string(), "Fake Phone".to_string(), Box::new(Listener)).expect("client");
    let _ = CLIENT.set(client);
    self::client().connect(host, port);
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}
