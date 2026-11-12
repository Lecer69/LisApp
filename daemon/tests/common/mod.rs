#![allow(dead_code)]
//! The real daemon in a throwaway home. The tools it runs are stubs that write down how they were
//! called, and answer with what a test leaves in `<tool>.out` and `<tool>.exit`.

use serde_json::{json, Value};
use std::io::{Read, Write};
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
        for tool in ["hyprctl", "notify-send", "playerctl", "wpctl", "adb", "curl", "wl-copy", "xdg-open"] {
            let path = home.path().join("bin").join(tool);
            let base = home.path().join(tool);
            let base = base.display();
            let script = format!(
                "#!/bin/sh\nline=''\nfor a in \"$@\"; do line=\"$line[$a] \"; done\nprintf '%s\\n' \"$line\" >> '{base}.log'\n\
                 [ '{tool}' = wl-copy ] && /bin/cat > '{base}.in'\n\
                 [ -f '{base}.out' ] && /bin/cat '{base}.out'\n\
                 [ -f '{base}.exit' ] && exit \"$(/bin/cat '{base}.exit')\"\n\
                 exit 0\n",
            );
            std::fs::write(&path, script).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        loop {
            let port = free_port();
            let mut child = Self::spawn(home.path(), port);
            // Another test's daemon can take the port between finding it free and binding it
            if Self::settled(&mut child, home.path()) {
                return Self { child, home, port };
            }
            eprintln!("the daemon couldn't use port {port}, trying another");
        }
    }

    /// Up and serving, which it only does once it has its port. False if it gave up instead.
    fn settled(child: &mut Child, home: &Path) -> bool {
        let socket = home.join("run/lis-app.sock");
        for _ in 0..4000 {
            if !matches!(child.try_wait(), Ok(None)) {
                return false;
            }
            if let Ok(mut stream) = std::os::unix::net::UnixStream::connect(&socket) {
                let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
                let mut reply = [0u8; 1];
                let answered = stream.write_all(b"{\"cmd\":\"status\"}\n").is_ok() && matches!(stream.read(&mut reply), Ok(1));
                if answered {
                    return true;
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("the daemon neither started nor gave up");
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

    pub fn set_tool(&self, tool: &str, output: &str) {
        std::fs::write(self.home.path().join(format!("{tool}.out")), output).unwrap();
    }

    pub fn set_tool_exit(&self, tool: &str, code: i32) {
        std::fs::write(self.home.path().join(format!("{tool}.exit")), code.to_string()).unwrap();
    }

    pub fn shell_file(&self, name: &str) -> PathBuf {
        self.home.path().join(".config/lis").join(name)
    }

    pub fn read_shell_file(&self, name: &str) -> Value {
        serde_json::from_slice(&std::fs::read(self.shell_file(name)).unwrap()).unwrap()
    }

    /// The way the shell saves: complete, or not at all.
    pub fn write_shell_file(&self, name: &str, doc: &Value) {
        let path = self.shell_file(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let tmp = path.with_extension("test-tmp");
        std::fs::write(&tmp, doc.to_string()).unwrap();
        std::fs::rename(&tmp, &path).unwrap();
    }

    pub fn downloads(&self) -> PathBuf {
        self.home.path().join("Downloads")
    }

    pub fn log(&self, tool: &str) -> String {
        std::fs::read_to_string(self.home.path().join(format!("{tool}.log"))).unwrap_or_default()
    }

    pub async fn wait_for_socket(&self) {
        for _ in 0..800 {
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

    /// Hears events from now on, as the shell does.
    pub async fn events(&self) -> tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>> {
        let stream = UnixStream::connect(self.socket()).await.unwrap();
        // The writing half has to stay open, or the daemon sees the client leave
        let (reader, writer) = stream.into_split();
        std::mem::forget(writer);
        BufReader::new(reader).lines()
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

