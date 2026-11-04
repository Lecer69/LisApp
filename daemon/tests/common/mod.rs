#![allow(dead_code)]
//! The real daemon in a throwaway home. `hyprctl` and `notify-send` are stubs that write down how
//! they were called.

use serde_json::{json, Value};
use std::net::TcpListener as StdListener;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::time::{sleep, timeout};

pub const WAIT: Duration = Duration::from_secs(10);

pub struct Pc {
    pub child: Child,
    pub home: tempfile::TempDir,
    pub port: u16,
}

impl Pc {
    pub fn start() -> Self {
        let home = tempfile::tempdir().unwrap();
        for dir in ["bin", "run"] {
            std::fs::create_dir(home.path().join(dir)).unwrap();
        }
        for tool in ["hyprctl", "notify-send"] {
            let path = home.path().join("bin").join(tool);
            let script = format!(
                "#!/bin/sh\nline=''\nfor a in \"$@\"; do line=\"$line[$a] \"; done\nprintf '%s\\n' \"$line\" >> '{}'\n",
                home.path().join(format!("{tool}.log")).display(),
            );
            std::fs::write(&path, script).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let port = free_port();
        let child = Self::spawn(home.path(), port);
        Self { child, home, port }
    }

    pub fn spawn(home: &Path, port: u16) -> Child {
        std::process::Command::new(env!("CARGO_BIN_EXE_lis-daemon"))
            .env_clear()
            .env("HOME", home)
            .env("PATH", home.join("bin"))
            .env("XDG_RUNTIME_DIR", home.join("run"))
            .env("LIS_APP_PORT", port.to_string())
            .env("LIS_APP_NO_MDNS", "1")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    }

    pub async fn restart(&mut self) {
        self.request(json!({ "cmd": "quit" })).await;
        let _ = self.child.wait();
        self.child = Self::spawn(self.home.path(), self.port);
        self.wait_for_socket().await;
    }

    pub fn socket(&self) -> PathBuf {
        self.home.path().join("run/lis-app.sock")
    }

    pub fn data(&self, name: &str) -> PathBuf {
        self.home.path().join(".local/share/lis-app").join(name)
    }

    pub fn downloads(&self) -> PathBuf {
        self.home.path().join("Downloads")
    }

    pub fn log(&self, tool: &str) -> String {
        std::fs::read_to_string(self.home.path().join(format!("{tool}.log"))).unwrap_or_default()
    }

    pub async fn wait_for_socket(&self) {
        for _ in 0..200 {
            if UnixStream::connect(self.socket()).await.is_ok() {
                return;
            }
            sleep(Duration::from_millis(25)).await;
        }
        panic!("the daemon never opened its socket");
    }

    pub async fn request(&self, mut request: Value) -> Value {
        request["id"] = json!(7);
        let stream = UnixStream::connect(self.socket()).await.unwrap();
        let (reader, mut writer) = stream.into_split();
        writer.write_all(format!("{request}\n").as_bytes()).await.unwrap();
        let mut lines = BufReader::new(reader).lines();
        timeout(WAIT, async {
            loop {
                let line = lines.next_line().await.unwrap().unwrap();
                let reply: Value = serde_json::from_str(&line).unwrap();
                if reply["id"] == json!(7) {
                    return reply;
                }
            }
        })
        .await
        .expect("no reply from the daemon")
    }

    pub async fn ok(&self, request: Value) -> Value {
        let reply = self.request(request.clone()).await;
        assert_eq!(reply["ok"], json!(true), "{request} -> {reply}");
        reply["result"].clone()
    }

    pub async fn status(&self) -> Value {
        self.ok(json!({ "cmd": "status" })).await
    }

    pub async fn until(&self, what: &str, mut wanted: impl FnMut(&Value) -> bool) -> Value {
        for _ in 0..400 {
            let status = self.status().await;
            if wanted(&status) {
                return status;
            }
            sleep(Duration::from_millis(25)).await;
        }
        panic!("never saw: {what}\n{}", self.status().await);
    }

    pub async fn log_until(&self, tool: &str, wanted: &str) -> String {
        for _ in 0..400 {
            let log = self.log(tool);
            if log.contains(wanted) {
                return log;
            }
            sleep(Duration::from_millis(25)).await;
        }
        panic!("{tool} never got {wanted}: {}", self.log(tool));
    }
}

impl Drop for Pc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn free_port() -> u16 {
    StdListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

