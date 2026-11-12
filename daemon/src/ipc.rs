//! The shell talks to the daemon over a Unix socket only this user can open: one JSON object per
//! line each way. Every client also gets the events as they happen.

use crate::daemon::Daemon;
use serde_json::{json, Value};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

const DEFAULT_PAIRING_SECONDS: u64 = 120;

/// Fails when another daemon already answers on the socket.
pub async fn bind(path: &Path) -> anyhow::Result<UnixListener> {
    if UnixStream::connect(path).await.is_ok() {
        anyhow::bail!("already running");
    }
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

pub async fn serve(listener: UnixListener, daemon: Arc<Daemon>) {
    loop {
        let Ok((stream, _)) = listener.accept().await else { continue };
        let daemon = daemon.clone();
        tokio::spawn(async move { client(stream, daemon).await });
    }
}

async fn client(stream: UnixStream, daemon: Arc<Daemon>) {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    let mut events = daemon.subscribe();

    loop {
        tokio::select! {
            line = lines.next_line() => {
                let Ok(Some(line)) = line else { return };
                let reply = handle(&line, &daemon).await;
                if write_line(&mut writer, &reply.to_string()).await.is_err() {
                    return;
                }
            }
            event = events.recv() => match event {
                Ok(event) => {
                    if write_line(&mut writer, &event).await.is_err() {
                        return;
                    }
                }
                // A slow client skips events; the next status catches it up
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            },
        }
    }
}

async fn write_line(writer: &mut tokio::net::unix::OwnedWriteHalf, line: &str) -> std::io::Result<()> {
    writer.write_all(line.as_bytes()).await?;
    writer.write_all(b"\n").await
}

async fn handle(line: &str, daemon: &Daemon) -> Value {
    let request: Value = match serde_json::from_str(line) {
        Ok(request) => request,
        Err(_) => return json!({ "ok": false, "error": "Not JSON" }),
    };
    let result = run(&request, daemon).await;
    let mut reply = match result {
        Ok(result) => json!({ "ok": true, "result": result }),
        Err(error) => json!({ "ok": false, "error": error }),
    };
    if let Some(id) = request.get("id") {
        reply["id"] = id.clone();
    }
    reply
}

async fn run(request: &Value, daemon: &Daemon) -> Result<Value, String> {
    let text = |key: &str| request.get(key).and_then(Value::as_str);
    let fingerprint = text("fingerprint");

    match text("cmd").ok_or("No command")? {
        "status" => Ok(daemon.status()),
        "pair_open" => {
            daemon.open_pairing(request.get("seconds").and_then(Value::as_u64).unwrap_or(DEFAULT_PAIRING_SECONDS));
            Ok(Value::Null)
        }
        "pair_close" => {
            daemon.close_pairing();
            Ok(Value::Null)
        }
        "pair_confirm" => {
            let accept = request.get("accept").and_then(Value::as_bool).ok_or("No answer")?;
            if daemon.confirm_pairing(accept).await {
                Ok(Value::Null)
            } else {
                Err("Nothing is waiting to pair".to_string())
            }
        }
        "unpair" => {
            if daemon.unpair(fingerprint.ok_or("No phone")?).await {
                Ok(Value::Null)
            } else {
                Err("Not a paired phone".to_string())
            }
        }
        "send_file" => {
            daemon.send_file(fingerprint, text("path").ok_or("No path")?).await?;
            Ok(Value::Null)
        }
        "ring_phone" => {
            daemon.ring_phone(fingerprint, request.get("ring").and_then(Value::as_bool).unwrap_or(true))?;
            Ok(Value::Null)
        }
        "snap" => {
            daemon.request_snap()?;
            Ok(Value::Null)
        }
        "snap_settings" => {
            let clipboard = request.get("clipboard").and_then(Value::as_bool).ok_or("No answer")?;
            daemon.set_snap_settings(text("folder").unwrap_or(""), clipboard)?;
            Ok(Value::Null)
        }
        "usb_mode" => {
            daemon.set_usb(request.get("enabled").and_then(Value::as_bool).ok_or("No answer")?).await;
            Ok(Value::Null)
        }
        "actions_set" => {
            let actions = request.get("actions").cloned().ok_or("No actions")?;
            let actions = serde_json::from_value(actions).map_err(|_| "Not a list of actions".to_string())?;
            daemon.set_actions(actions)?;
            Ok(Value::Null)
        }
        // There's no command that unlocks: only a signature from the phone's hardware key can
        "lock_state" => {
            let locked = request.get("locked").and_then(Value::as_bool).ok_or("No answer")?;
            let password_required = request.get("password_required").and_then(Value::as_bool).unwrap_or(true);
            daemon.set_lock_state(locked, password_required);
            Ok(Value::Null)
        }
        "unlock_prompt" => {
            daemon.prompt_unlock();
            Ok(Value::Null)
        }
        "unlock_answer" => {
            let accept = request.get("accept").and_then(Value::as_bool).ok_or("No answer")?;
            daemon.answer_unlock(fingerprint.ok_or("No phone")?, accept)?;
            Ok(Value::Null)
        }
        "unlock_disable" => {
            daemon.disable_unlock(fingerprint.ok_or("No phone")?)?;
            Ok(Value::Null)
        }
        "quit" => {
            daemon.shutdown.notify_one();
            Ok(Value::Null)
        }
        _ => Err("Unknown command".to_string()),
    }
}

/// For scripts: sends one request and prints its reply.
pub async fn ctl(path: &Path, request: &str) -> anyhow::Result<bool> {
    let mut request: Value = serde_json::from_str(request)?;
    let id = 1;
    request["id"] = json!(id);

    let stream = UnixStream::connect(path).await?;
    let (reader, mut writer) = stream.into_split();
    write_line(&mut writer, &request.to_string()).await?;

    let mut lines = BufReader::new(reader).lines();
    while let Some(line) = lines.next_line().await? {
        let reply: Value = serde_json::from_str(&line)?;
        if reply.get("id") == Some(&json!(id)) {
            println!("{reply}");
            return Ok(reply["ok"] == json!(true));
        }
    }
    anyhow::bail!("the daemon closed the connection")
}
