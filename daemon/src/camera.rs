//! Carries the phone's camera to OBS: one phone in over TLS, any number of viewers out over a Unix
//! socket only this user can open. Nothing here decodes video.

use crate::daemon::Daemon;
use lis_core::video::{encode, read_packet_async, write_packet_async, Packet, VideoInfo};
use serde_json::{json, Value};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, watch};
use tokio::time::{timeout, Instant};

pub const STATS_EVERY: Duration = Duration::from_secs(1);

/// About 130 ms at 60 fps. A viewer further behind skips to the next keyframe.
const VIEWER_QUEUE: usize = 8;
const VIEWER_WRITE_LIMIT: Duration = Duration::from_secs(2);
/// The phone sends constantly, so this much quiet means it's gone.
const PHONE_SILENCE_LIMIT: Duration = Duration::from_secs(5);

#[derive(Clone)]
enum Item {
    Info(Arc<Vec<u8>>),
    Frame { keyframe: bool, bytes: Arc<Vec<u8>> },
    End,
}

#[derive(Default)]
struct Live {
    phone: String,
    fingerprint: String,
    info: Option<VideoInfo>,
    window_frames: u32,
    window_bytes: u64,
    fps: f32,
    mbps: f32,
    window_start: Option<Instant>,
}

pub struct Camera {
    live: Mutex<Live>,
    items: broadcast::Sender<Item>,
    viewers: watch::Sender<u32>,
    /// The newest phone connection; an older one stops when this moves on.
    generation: AtomicU64,
    newest: watch::Sender<u64>,
}

impl Camera {
    pub fn new() -> Self {
        Self {
            live: Mutex::new(Live::default()),
            items: broadcast::channel(VIEWER_QUEUE).0,
            viewers: watch::channel(0).0,
            generation: AtomicU64::new(0),
            newest: watch::channel(0).0,
        }
    }

    pub fn status(&self) -> Value {
        let live = self.live.lock().unwrap();
        let Some(info) = &live.info else { return Value::Null };
        json!({
            "phone": live.phone,
            "camera": info.camera,
            "width": info.width,
            "height": info.height,
            "fps": info.fps,
            "rotation": info.rotation,
            "measured_fps": live.fps,
            "mbps": live.mbps,
            "viewers": *self.viewers.borrow(),
        })
    }

    /// Whether the camera is live.
    pub fn measure(&self) -> bool {
        let mut live = self.live.lock().unwrap();
        if live.info.is_none() {
            return false;
        }
        let now = Instant::now();
        if let Some(start) = live.window_start {
            let seconds = now.duration_since(start).as_secs_f32();
            if seconds > 0.0 {
                live.fps = live.window_frames as f32 / seconds;
                live.mbps = live.window_bytes as f32 * 8.0 / 1_000_000.0 / seconds;
            }
        }
        live.window_start = Some(now);
        live.window_frames = 0;
        live.window_bytes = 0;
        true
    }

    fn set_info(&self, info: VideoInfo) -> std::io::Result<()> {
        let bytes = encode(&Packet::Info(info.clone()))?;
        self.live.lock().unwrap().info = Some(info);
        let _ = self.items.send(Item::Info(Arc::new(bytes)));
        Ok(())
    }

    fn current_info(&self) -> Option<VideoInfo> {
        self.live.lock().unwrap().info.clone()
    }

    fn stop(&self) {
        *self.live.lock().unwrap() = Live::default();
        let _ = self.items.send(Item::End);
    }

    /// When a phone is unpaired: trust was only checked as its stream connected.
    pub fn drop_phone(&self, fingerprint: &str) {
        if self.live.lock().unwrap().fingerprint == fingerprint {
            let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
            let _ = self.newest.send(generation);
            self.stop();
        }
    }
}

async fn phone_loop<R: AsyncRead + Unpin>(reader: &mut R, daemon: &Daemon) -> std::io::Result<()> {
    let camera = &daemon.camera;
    loop {
        let packet = timeout(PHONE_SILENCE_LIMIT, read_packet_async(reader))
            .await
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "the camera went quiet"))??;
        match packet {
            Packet::Info(info) => {
                camera.set_info(info)?;
                daemon.publish_status();
            }
            Packet::Frame(frame) => {
                let keyframe = frame.keyframe;
                let bytes = encode(&Packet::Frame(frame))?;
                {
                    let mut live = camera.live.lock().unwrap();
                    live.window_frames += 1;
                    live.window_bytes += bytes.len() as u64;
                }
                let _ = camera.items.send(Item::Frame { keyframe, bytes: Arc::new(bytes) });
            }
            _ => return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "unexpected packet from the phone")),
        }
    }
}

/// Until the phone stops. A newer connection replaces this one.
pub async fn serve_phone<S>(stream: S, fingerprint: String, phone: String, daemon: Arc<Daemon>)
where
    S: AsyncRead + AsyncWrite + Send + 'static,
{
    let camera = &daemon.camera;
    let generation = camera.generation.fetch_add(1, Ordering::SeqCst) + 1;
    let mut newest = camera.newest.subscribe();
    let _ = camera.newest.send(generation);
    {
        let mut live = camera.live.lock().unwrap();
        live.phone = phone;
        live.fingerprint = fingerprint;
    }

    let (mut reader, mut writer) = tokio::io::split(stream);
    let mut viewers = camera.viewers.subscribe();
    viewers.mark_changed();

    let replaced = async {
        while newest.changed().await.is_ok() {
            if *newest.borrow() != generation {
                break;
            }
        }
    };
    let tell_viewers = async {
        while viewers.changed().await.is_ok() {
            let count = *viewers.borrow();
            if write_packet_async(&mut writer, &Packet::Viewers(count)).await.is_err() {
                break;
            }
        }
    };

    tokio::select! {
        _ = phone_loop(&mut reader, &daemon) => {}
        _ = replaced => {}
        _ = tell_viewers => {}
    }

    // A replacement has already taken over the stream
    if camera.generation.load(Ordering::SeqCst) == generation {
        camera.stop();
        daemon.publish_status();
    }
}

pub fn bind(path: &Path) -> std::io::Result<UnixListener> {
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

pub async fn serve_viewers(listener: UnixListener, daemon: Arc<Daemon>) {
    loop {
        let Ok((stream, _)) = listener.accept().await else { continue };
        tokio::spawn(viewer(stream, daemon.clone()));
    }
}

struct ViewerCount<'a>(&'a Camera);

impl Drop for ViewerCount<'_> {
    fn drop(&mut self) {
        self.0.viewers.send_modify(|count| *count = count.saturating_sub(1));
    }
}

async fn write_limited(writer: &mut tokio::net::unix::OwnedWriteHalf, bytes: &[u8]) -> bool {
    matches!(timeout(VIEWER_WRITE_LIMIT, writer.write_all(bytes)).await, Ok(Ok(())))
}

async fn viewer(stream: UnixStream, daemon: Arc<Daemon>) {
    let camera = &daemon.camera;
    // Subscribed before reading the info, so no change falls between them
    let mut items = camera.items.subscribe();
    camera.viewers.send_modify(|count| *count += 1);
    let _count = ViewerCount(camera);
    daemon.publish_status();

    let (mut reader, mut writer) = stream.into_split();
    if let Some(info) = camera.current_info() {
        let Ok(bytes) = encode(&Packet::Info(info)) else { return };
        if !write_limited(&mut writer, &bytes).await {
            return;
        }
    }

    // A viewer can only start decoding at a keyframe
    let mut need_keyframe = true;
    let mut scratch = [0u8; 16];
    loop {
        tokio::select! {
            item = items.recv() => {
                let bytes = match item {
                    Ok(Item::Info(bytes)) => bytes,
                    Ok(Item::Frame { keyframe, bytes }) => {
                        if need_keyframe && !keyframe {
                            continue;
                        }
                        need_keyframe = false;
                        bytes
                    }
                    Ok(Item::End) => {
                        need_keyframe = true;
                        let Ok(bytes) = encode(&Packet::End) else { return };
                        Arc::new(bytes)
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        need_keyframe = true;
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                };
                if !write_limited(&mut writer, &bytes).await {
                    break;
                }
            }
            // Viewers send nothing, so this only ends when they leave
            read = reader.read(&mut scratch) => {
                if !matches!(read, Ok(count) if count > 0) {
                    break;
                }
            }
        }
    }
    drop(_count);
    daemon.publish_status();
}
