//! `lis-daemon obs ...`: tells OBS what to do through its own WebSocket server, for action buttons.
//! The address and password come from OBS's settings, so nothing has to be set up here.

use anyhow::{bail, Context};
use base64::Engine;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::net::{Ipv4Addr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tungstenite::{Message, WebSocket};

const LIMIT: Duration = Duration::from_secs(5);
const OP_HELLO: u64 = 0;
const OP_IDENTIFY: u64 = 1;
const OP_IDENTIFIED: u64 = 2;
const OP_REQUEST: u64 = 6;
const OP_RESPONSE: u64 = 7;

pub const USAGE: &str = "lis-daemon obs scene <name> | record | stream | scenes";

fn settings_path() -> anyhow::Result<PathBuf> {
    let config = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from(std::env::var_os("HOME").context("HOME isn't set")?).join(".config"),
    };
    Ok(config.join("obs-studio/plugin_config/obs-websocket/config.json"))
}

struct Server {
    port: u16,
    password: Option<String>,
}

fn read_server(path: &Path) -> anyhow::Result<Server> {
    let bytes = std::fs::read(path).with_context(|| format!("can't read OBS's settings at {}", path.display()))?;
    let settings: Value = serde_json::from_slice(&bytes).context("OBS's WebSocket settings aren't valid")?;
    if settings["server_enabled"] != json!(true) {
        bail!("OBS's WebSocket server is off. Turn it on in OBS: Tools, WebSocket Server Settings");
    }
    let port = settings["server_port"].as_u64().and_then(|port| u16::try_from(port).ok()).unwrap_or(4455);
    let password = if settings["auth_required"] == json!(false) {
        None
    } else {
        settings["server_password"].as_str().map(str::to_string)
    };
    Ok(Server { port, password })
}

fn base64_sha256(text: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(Sha256::digest(text.as_bytes()))
}

/// As obs-websocket's protocol defines it.
fn authentication(password: &str, salt: &str, challenge: &str) -> String {
    let secret = base64_sha256(&format!("{password}{salt}"));
    base64_sha256(&format!("{secret}{challenge}"))
}

fn read_op(socket: &mut WebSocket<TcpStream>, wanted: u64) -> anyhow::Result<Value> {
    loop {
        match socket.read() {
            Ok(Message::Text(text)) => {
                let message: Value = serde_json::from_str(text.as_str())?;
                if message["op"].as_u64() == Some(wanted) {
                    return Ok(message["d"].clone());
                }
            }
            Ok(Message::Close(frame)) => {
                let reason = frame.map(|frame| frame.reason.to_string()).unwrap_or_default();
                bail!("OBS closed the connection: {reason}");
            }
            Ok(_) => {}
            Err(error) => bail!("lost OBS: {error}"),
        }
    }
}

fn send(socket: &mut WebSocket<TcpStream>, op: u64, data: Value) -> anyhow::Result<()> {
    socket.send(Message::text(json!({ "op": op, "d": data }).to_string())).context("can't reach OBS")
}

fn request(server: &Server, request_type: &str, request_data: Value) -> anyhow::Result<Value> {
    let address = (Ipv4Addr::LOCALHOST, server.port);
    let stream = TcpStream::connect(address).with_context(|| format!("can't reach OBS on port {}. Is it running?", server.port))?;
    stream.set_read_timeout(Some(LIMIT))?;
    stream.set_write_timeout(Some(LIMIT))?;
    let (mut socket, _) = tungstenite::client(format!("ws://127.0.0.1:{}", server.port), stream).context("can't talk to OBS")?;

    let hello = read_op(&mut socket, OP_HELLO)?;
    let mut identify = json!({ "rpcVersion": 1, "eventSubscriptions": 0 });
    if let (Some(challenge), Some(salt)) = (hello["authentication"]["challenge"].as_str(), hello["authentication"]["salt"].as_str()) {
        let password = server.password.as_deref().context("OBS wants a password, and its settings have none")?;
        identify["authentication"] = json!(authentication(password, salt, challenge));
    }
    send(&mut socket, OP_IDENTIFY, identify)?;
    read_op(&mut socket, OP_IDENTIFIED)?;

    send(&mut socket, OP_REQUEST, json!({ "requestType": request_type, "requestId": "lis", "requestData": request_data }))?;
    let response = read_op(&mut socket, OP_RESPONSE)?;
    let _ = socket.close(None);

    if response["requestStatus"]["result"] != json!(true) {
        let comment = response["requestStatus"]["comment"].as_str().unwrap_or("no reason given");
        bail!("OBS refused {request_type}: {comment}");
    }
    Ok(response["responseData"].clone())
}

pub fn run(args: &[&str]) -> anyhow::Result<bool> {
    let server = read_server(&settings_path()?)?;
    match args {
        ["scene", name] => {
            request(&server, "SetCurrentProgramScene", json!({ "sceneName": name }))?;
        }
        ["record"] => {
            request(&server, "ToggleRecord", json!({}))?;
        }
        ["stream"] => {
            request(&server, "ToggleStream", json!({}))?;
        }
        ["scenes"] => {
            let list = request(&server, "GetSceneList", json!({}))?;
            for scene in list["scenes"].as_array().into_iter().flatten().rev() {
                println!("{}", scene["sceneName"].as_str().unwrap_or_default());
            }
        }
        _ => {
            eprintln!("usage: {USAGE}");
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authentication_matches_the_documented_steps() {
        let secret = base64_sha256("supersecretsalt");
        assert_eq!(authentication("supersecret", "salt", "challenge"), base64_sha256(&format!("{secret}challenge")));
        // The well-known hash of nothing, in base64
        assert_eq!(base64_sha256(""), "47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=");
    }

    fn settings(dir: &tempfile::TempDir, text: Value) -> PathBuf {
        let path = dir.path().join("config.json");
        std::fs::write(&path, text.to_string()).unwrap();
        path
    }

    #[test]
    fn says_so_when_the_server_is_off() {
        let dir = tempfile::tempdir().unwrap();
        let path = settings(&dir, json!({ "server_enabled": false, "server_port": 4455 }));
        let error = read_server(&path).err().unwrap().to_string();
        assert!(error.contains("Tools, WebSocket Server Settings"), "{error}");
    }

    #[test]
    fn reads_the_server_settings() {
        let dir = tempfile::tempdir().unwrap();
        let path = settings(&dir, json!({ "server_enabled": true, "server_port": 4460, "auth_required": true, "server_password": "p" }));
        let server = read_server(&path).unwrap();
        assert_eq!((server.port, server.password.as_deref()), (4460, Some("p")));

        let path = settings(&dir, json!({ "server_enabled": true, "server_port": 4460, "auth_required": false, "server_password": "p" }));
        assert_eq!(read_server(&path).unwrap().password, None);
    }
}
