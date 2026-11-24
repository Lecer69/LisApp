//! The relay's limits, against a real one on localhost.

use lis_core::relay::{dial, listen, read_line, take};
use lis_relay::config::Settings;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;

const PASSWORD: &str = "limits-test-password";

fn pc(n: u8) -> String {
    format!("{n:02x}").repeat(32)
}

async fn start(change: impl FnOnce(&mut lis_relay::config::Config)) -> u16 {
    let mut config = lis_relay::config::Config::default();
    config.auth.password = PASSWORD.into();
    config.auth.failure_delay_ms = 0;
    change(&mut config);
    let settings = Settings::from_config(config).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(lis_relay::run(listener, settings));
    port
}

/// A PC that answers every call with a tunnel, which echoes what it hears. Stays until the test ends.
async fn echo_pc(port: u16, fingerprint: String) {
    let mut calls = listen("127.0.0.1", port, PASSWORD, &fingerprint).await.unwrap();
    tokio::spawn(async move {
        loop {
            let Ok(line) = read_line(&mut calls).await else { return };
            let Some(token) = line.strip_prefix("call ") else { continue };
            let token = token.to_string();
            tokio::spawn(async move {
                let Ok(mut tunnel) = take("127.0.0.1", port, PASSWORD, &token).await else { return };
                let mut buffer = [0u8; 4096];
                while let Ok(count) = tunnel.read(&mut buffer).await {
                    if count == 0 || tunnel.write_all(&buffer[..count]).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
}

async fn echoes(tunnel: &mut TcpStream) -> bool {
    if tunnel.write_all(b"hello").await.is_err() {
        return false;
    }
    let mut reply = [0u8; 5];
    matches!(timeout(Duration::from_secs(5), tunnel.read_exact(&mut reply)).await, Ok(Ok(_))) && &reply == b"hello"
}

fn reason(result: std::io::Result<TcpStream>) -> String {
    result.err().map(|error| error.to_string()).unwrap_or_default()
}

#[tokio::test]
async fn a_phone_reaches_its_pc() {
    let port = start(|_| {}).await;
    echo_pc(port, pc(1)).await;
    let mut tunnel = dial("127.0.0.1", port, PASSWORD, &pc(1)).await.unwrap();
    assert!(echoes(&mut tunnel).await);
    assert_eq!(reason(dial("127.0.0.1", port, PASSWORD, &pc(2)).await), "the PC isn't on the relay");
}

#[tokio::test]
async fn each_pc_gets_only_so_many_tunnels() {
    let port = start(|config| config.limits.max_tunnels_per_pc = 2).await;
    echo_pc(port, pc(1)).await;
    let mut first = dial("127.0.0.1", port, PASSWORD, &pc(1)).await.unwrap();
    let mut second = dial("127.0.0.1", port, PASSWORD, &pc(1)).await.unwrap();
    assert!(echoes(&mut first).await && echoes(&mut second).await);
    assert_eq!(reason(dial("127.0.0.1", port, PASSWORD, &pc(1)).await), "too many connections to that PC");

    drop(first);
    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut again = dial("127.0.0.1", port, PASSWORD, &pc(1)).await.unwrap();
    assert!(echoes(&mut again).await, "a closed tunnel frees its place");
}

#[tokio::test]
async fn only_allowed_pcs_get_on() {
    let port = start(|config| config.access.allow_pcs = vec![pc(1)]).await;
    assert!(listen("127.0.0.1", port, PASSWORD, &pc(1)).await.is_ok());
    assert_eq!(reason(listen("127.0.0.1", port, PASSWORD, &pc(2)).await), "that PC isn't allowed on this relay");
}

#[tokio::test]
async fn the_relay_holds_only_so_many_pcs() {
    let port = start(|config| config.limits.max_pcs = 1).await;
    let _first = listen("127.0.0.1", port, PASSWORD, &pc(1)).await.unwrap();
    assert_eq!(reason(listen("127.0.0.1", port, PASSWORD, &pc(2)).await), "the relay is full");
    assert!(listen("127.0.0.1", port, PASSWORD, &pc(1)).await.is_ok(), "the same PC can replace its own place");
}

#[tokio::test]
async fn guessing_the_password_gets_an_address_banned() {
    let port = start(|config| config.auth.max_failures = 3).await;
    for _ in 0..3 {
        assert_eq!(reason(listen("127.0.0.1", port, "wrong-password", &pc(1)).await), "wrong relay password");
    }
    // Now even the right password is turned away, without an answer
    assert!(listen("127.0.0.1", port, PASSWORD, &pc(1)).await.is_err());
}

#[tokio::test]
async fn a_pc_over_its_daily_data_turns_phones_away() {
    let port = start(|config| config.limits.daily_mb_per_pc = 1).await;
    echo_pc(port, pc(1)).await;
    let mut tunnel = dial("127.0.0.1", port, PASSWORD, &pc(1)).await.unwrap();
    // Past 1 MiB, counting both ways
    let block = vec![7u8; 64 * 1024];
    let mut buffer = vec![0u8; 64 * 1024];
    let mut ended = false;
    for _ in 0..16 {
        if tunnel.write_all(&block).await.is_err() {
            ended = true;
            break;
        }
        match timeout(Duration::from_secs(5), tunnel.read_exact(&mut buffer)).await {
            Ok(Ok(_)) => {}
            _ => {
                ended = true;
                break;
            }
        }
    }
    assert!(ended, "the tunnel ended once over the limit");
    assert_eq!(reason(dial("127.0.0.1", port, PASSWORD, &pc(1)).await), "that PC has used its data for today");
}

#[tokio::test]
async fn a_tunnel_is_held_to_its_speed() {
    let port = start(|config| config.limits.tunnel_kbps = 64).await;
    echo_pc(port, pc(1)).await;
    let mut tunnel = dial("127.0.0.1", port, PASSWORD, &pc(1)).await.unwrap();
    let block = vec![1u8; 64 * 1024];
    let mut buffer = vec![0u8; 64 * 1024];
    let start = std::time::Instant::now();
    tunnel.write_all(&block).await.unwrap();
    tunnel.write_all(&block).await.unwrap();
    timeout(Duration::from_secs(10), tunnel.read_exact(&mut buffer)).await.unwrap().unwrap();
    timeout(Duration::from_secs(10), tunnel.read_exact(&mut buffer)).await.unwrap().unwrap();
    // 128 KiB at 64 KiB/s takes about 2 s, give or take the first block
    assert!(start.elapsed() >= Duration::from_millis(900), "{:?}", start.elapsed());
}
