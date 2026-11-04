//! `lis-daemon obs`, against a stand-in for OBS that checks the password the way OBS does.

use base64::Engine;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::net::TcpListener;
use std::process::Command;
use std::sync::mpsc;
use tungstenite::{accept, Message};

const PASSWORD: &str = "supersecret";

fn hashed(text: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(Sha256::digest(text.as_bytes()))
}

fn read(socket: &mut tungstenite::WebSocket<std::net::TcpStream>) -> Value {
    loop {
        if let Message::Text(text) = socket.read().unwrap() {
            return serde_json::from_str(text.as_str()).unwrap();
        }
    }
}

/// Answers one connection; hands back the request it was given.
fn fake_obs(result: bool) -> (u16, mpsc::Receiver<Value>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (requests, received) = mpsc::channel();
    std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut socket = accept(stream).unwrap();
        let hello = json!({ "op": 0, "d": { "rpcVersion": 1, "authentication": { "challenge": "ch", "salt": "sa" } } });
        socket.send(Message::text(hello.to_string())).unwrap();

        let identify = read(&mut socket);
        let wanted = hashed(&format!("{}ch", hashed(&format!("{PASSWORD}sa"))));
        if identify["d"]["authentication"] != json!(wanted) {
            let _ = socket.close(None);
            let _ = socket.flush();
            return;
        }
        socket.send(Message::text(json!({ "op": 2, "d": { "negotiatedRpcVersion": 1 } }).to_string())).unwrap();

        let request = read(&mut socket);
        let answer = json!({ "op": 7, "d": {
            "requestType": request["d"]["requestType"],
            "requestId": request["d"]["requestId"],
            "requestStatus": { "result": result, "code": 100, "comment": "No such scene" },
            "responseData": { "scenes": [{ "sceneName": "Second" }, { "sceneName": "First" }] },
        } });
        socket.send(Message::text(answer.to_string())).unwrap();
        requests.send(request).unwrap();
        let _ = socket.read();
    });
    (port, received)
}

fn settings(dir: &tempfile::TempDir, port: u16, enabled: bool, password: &str) {
    let folder = dir.path().join(".config/obs-studio/plugin_config/obs-websocket");
    std::fs::create_dir_all(&folder).unwrap();
    let text = json!({ "server_enabled": enabled, "server_port": port, "auth_required": true, "server_password": password });
    std::fs::write(folder.join("config.json"), text.to_string()).unwrap();
}

fn obs(dir: &tempfile::TempDir, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_lis-daemon"))
        .env_clear()
        .env("HOME", dir.path())
        .arg("obs")
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn switches_scene_with_the_password_from_obss_settings() {
    let dir = tempfile::tempdir().unwrap();
    let (port, requests) = fake_obs(true);
    settings(&dir, port, true, PASSWORD);

    let output = obs(&dir, &["scene", "My Scene"]);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let request = requests.recv().unwrap();
    assert_eq!(request["d"]["requestType"], json!("SetCurrentProgramScene"));
    assert_eq!(request["d"]["requestData"]["sceneName"], json!("My Scene"));
}

#[test]
fn lists_scenes_top_first() {
    let dir = tempfile::tempdir().unwrap();
    let (port, _requests) = fake_obs(true);
    settings(&dir, port, true, PASSWORD);
    let output = obs(&dir, &["scenes"]);
    assert_eq!(String::from_utf8_lossy(&output.stdout), "First\nSecond\n");
}

#[test]
fn a_wrong_password_or_refused_request_fails_and_says_why() {
    let dir = tempfile::tempdir().unwrap();
    let (port, _requests) = fake_obs(true);
    settings(&dir, port, true, "wrong");
    assert!(!obs(&dir, &["record"]).status.success());

    let (port, _requests) = fake_obs(false);
    settings(&dir, port, true, PASSWORD);
    let output = obs(&dir, &["scene", "Missing"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("No such scene"));
}

#[test]
fn says_what_to_do_when_the_server_is_off_or_obs_is_not_running() {
    let dir = tempfile::tempdir().unwrap();
    settings(&dir, 1, false, PASSWORD);
    let output = obs(&dir, &["record"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("WebSocket Server Settings"));

    let free = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    settings(&dir, free, true, PASSWORD);
    let output = obs(&dir, &["record"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Is it running?"));
}
