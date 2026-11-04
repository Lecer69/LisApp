//! What's playing on this PC, for the phone's lock screen. Polled with playerctl and wpctl, only
//! while a phone is connected.

use crate::daemon::Daemon;
use lis_core::protocol::{MediaAction, MediaState, Message};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::process::Command;
use tokio::sync::Notify;
use tokio::time::{timeout, Instant};

pub const EVERY: Duration = Duration::from_secs(2);

const FORMAT: &str = "{{playerName}}\u{1f}{{status}}\u{1f}{{title}}\u{1f}{{artist}}\u{1f}{{album}}\u{1f}{{position}}\u{1f}{{mpris:length}}\u{1f}{{mpris:artUrl}}";
const SINK: &str = "@DEFAULT_AUDIO_SINK@";
const TOOL_LIMIT: Duration = Duration::from_secs(5);
/// Its hex has to fit in one message.
const MAX_ART: u64 = 256 * 1024;
/// The phone keeps counting by itself, so the position only goes out when it's this far off.
const SEEK_SLACK_MS: u64 = 3000;
/// For the player to act on a command before asking it again.
const SETTLE: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, PartialEq, Default)]
struct Player {
    name: String,
    playing: bool,
    title: String,
    artist: String,
    album: String,
    position_ms: u64,
    length_ms: u64,
    art_url: String,
}

fn micros_to_ms(text: &str) -> u64 {
    text.trim().parse::<u64>().unwrap_or(0) / 1000
}

/// Players with nothing loaded have no title, and nothing to show.
fn parse_players(output: &str) -> Vec<Player> {
    output
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split('\u{1f}').collect();
            let [name, status, title, artist, album, position, length, art_url] = fields.as_slice() else { return None };
            (!title.trim().is_empty()).then(|| Player {
                name: (*name).to_string(),
                playing: *status == "Playing",
                title: (*title).to_string(),
                artist: (*artist).to_string(),
                album: (*album).to_string(),
                position_ms: micros_to_ms(position),
                length_ms: micros_to_ms(length),
                art_url: (*art_url).to_string(),
            })
        })
        .collect()
}

/// One that's playing, staying with the current one among several; otherwise the one last used, or
/// one with real music over something like a browser tab, which has no artist.
fn pick<'a>(players: &'a [Player], current: Option<&str>) -> Option<&'a Player> {
    let is_current = |player: &&Player| Some(player.name.as_str()) == current;
    players
        .iter()
        .filter(|player| player.playing)
        .find(is_current)
        .or_else(|| players.iter().find(|player| player.playing))
        .or_else(|| players.iter().find(is_current))
        .or_else(|| players.iter().find(|player| !player.artist.is_empty()))
        .or_else(|| players.first())
}

/// "Volume: 0.52" or "Volume: 0.52 [MUTED]".
fn parse_volume(output: &str) -> Option<(u8, bool)> {
    let level: f32 = output.strip_prefix("Volume:")?.split_whitespace().next()?.parse().ok()?;
    Some(((level * 100.0).round().clamp(0.0, 100.0) as u8, output.contains("[MUTED]")))
}

fn is_image(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0xff, 0xd8, 0xff])
        || bytes.starts_with(&[0x89, b'P', b'N', b'G'])
        || bytes.starts_with(b"GIF8")
        || (bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP".as_slice()))
}

fn percent_decode(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let escaped = (bytes[index] == b'%')
            .then(|| bytes.get(index + 1..index + 3))
            .flatten()
            .and_then(|pair| hex::decode(pair).ok());
        if let Some(pair) = escaped {
            decoded.push(pair[0]);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    decoded
}

/// Only ever an image, so a player can't send some other file to the phone by naming it as its art.
async fn load_art(url: &str) -> Option<Vec<u8>> {
    let bytes = if let Some(path) = url.strip_prefix("file://") {
        use std::os::unix::ffi::OsStringExt;
        let path = std::path::PathBuf::from(std::ffi::OsString::from_vec(percent_decode(path)));
        if tokio::fs::metadata(&path).await.ok()?.len() > MAX_ART {
            return None;
        }
        tokio::fs::read(&path).await.ok()?
    } else if url.starts_with("https://") {
        let limit = MAX_ART.to_string();
        let output = Command::new("curl")
            .args(["-fsL", "--proto", "=https", "--max-time", "5", "--max-filesize", &limit, "--url", url])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .output();
        let output = timeout(TOOL_LIMIT * 2, output).await.ok()?.ok()?;
        if !output.status.success() {
            return None;
        }
        output.stdout
    } else {
        return None;
    };
    (bytes.len() as u64 <= MAX_ART && is_image(&bytes)).then_some(bytes)
}

async fn tool_output(tool: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(tool).args(args).stdin(Stdio::null()).stderr(Stdio::null()).kill_on_drop(true).output();
    let output = timeout(TOOL_LIMIT, output).await.ok()?.ok()?;
    output.status.success().then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

#[derive(Default)]
struct Now {
    player: Option<String>,
    sent: Option<MediaState>,
    sent_at: Option<Instant>,
    art_url: String,
    art: String,
}

/// What a phone needs to hear about, leaving out the position while it runs as expected.
fn worth_sending(sent: Option<&MediaState>, elapsed_ms: u64, new: &MediaState) -> bool {
    let Some(sent) = sent else { return true };
    let mut same = new.clone();
    same.position_ms = sent.position_ms;
    if same != *sent {
        return true;
    }
    let expected = if sent.playing { sent.position_ms + elapsed_ms } else { sent.position_ms };
    expected.abs_diff(new.position_ms) > SEEK_SLACK_MS
}

#[derive(Default)]
pub struct Media {
    now: Mutex<Now>,
    wake: Arc<Notify>,
}

impl Media {
    /// For a phone that just connected.
    pub fn greeting(&self) -> Vec<Message> {
        let now = self.now.lock().unwrap();
        let Some(sent) = &now.sent else { return Vec::new() };
        vec![Message::MediaState(sent.clone()), Message::MediaArt { data: now.art.clone() }]
    }

    pub fn forget(&self) {
        *self.now.lock().unwrap() = Now::default();
    }

    pub fn wake(&self) -> Arc<Notify> {
        self.wake.clone()
    }

    /// Never waits for the tool, as this comes from the phone's connection.
    pub fn control(&self, action: MediaAction) {
        let player = self.now.lock().unwrap().player.clone();
        let wake = self.wake.clone();
        tokio::spawn(async move {
            let mut command = match &action {
                MediaAction::SetVolume { percent } => {
                    let mut command = Command::new("wpctl");
                    command.args(["set-volume", "-l", "1.0", SINK, &format!("{}%", (*percent).min(100))]);
                    command
                }
                MediaAction::PlayPause | MediaAction::Next | MediaAction::Previous => {
                    let Some(player) = player else { return };
                    let verb = match action {
                        MediaAction::Next => "next",
                        MediaAction::Previous => "previous",
                        _ => "play-pause",
                    };
                    let mut command = Command::new("playerctl");
                    command.args(["-p", &player, verb]);
                    command
                }
            };
            let done = command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).kill_on_drop(true).status();
            if let Err(error) = timeout(TOOL_LIMIT, done).await.unwrap_or_else(|_| Err(std::io::ErrorKind::TimedOut.into())) {
                eprintln!("lis-daemon: can't control the media: {error}");
            }
            tokio::time::sleep(SETTLE).await;
            wake.notify_one();
        });
    }

    pub async fn tick(&self, daemon: &Daemon) {
        if !daemon.has_phone() {
            self.forget();
            return;
        }
        // Stays quiet where these tools aren't installed
        let players = parse_players(&tool_output("playerctl", &["-a", "metadata", "--format", FORMAT]).await.unwrap_or_default());
        let volume = tool_output("wpctl", &["get-volume", SINK]).await.as_deref().and_then(parse_volume);

        let current = self.now.lock().unwrap().player.clone();
        let Some(player) = pick(&players, current.as_deref()).cloned() else {
            let was_playing = {
                let mut now = self.now.lock().unwrap();
                let had = now.sent.is_some();
                *now = Now::default();
                had
            };
            if was_playing {
                daemon.broadcast(Message::MediaGone);
            }
            return;
        };

        // A failed read keeps the last figure, rather than showing the volume as nothing
        let last = self.now.lock().unwrap().sent.as_ref().map(|sent| (sent.volume_percent, sent.muted));
        let (percent, muted) = volume.or(last).unwrap_or((0, false));
        let state = MediaState {
            player: player.name.clone(),
            title: player.title.clone(),
            artist: player.artist.clone(),
            album: player.album.clone(),
            playing: player.playing,
            position_ms: player.position_ms,
            length_ms: player.length_ms,
            volume_percent: percent,
            muted,
        };

        let (send, new_art) = {
            let mut now = self.now.lock().unwrap();
            now.player = Some(player.name.clone());
            let elapsed = now.sent_at.map_or(0, |at| at.elapsed().as_millis() as u64);
            let send = worth_sending(now.sent.as_ref(), elapsed, &state);
            if send {
                now.sent = Some(state.clone());
                now.sent_at = Some(Instant::now());
            }
            let new_art = now.art_url != player.art_url;
            if new_art {
                now.art_url = player.art_url.clone();
            }
            (send, new_art)
        };
        if send {
            daemon.broadcast(Message::MediaState(state));
        }
        if new_art {
            let data = load_art(&player.art_url).await.map(hex::encode).unwrap_or_default();
            self.now.lock().unwrap().art = data.clone();
            daemon.broadcast(Message::MediaArt { data });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(name: &str, status: &str, title: &str) -> String {
        [name, status, title, "Artist", "Album", "120650000", "336812000", "https://i.example/a"].join("\u{1f}")
    }

    fn players(lines: &[String]) -> Vec<Player> {
        parse_players(&lines.join("\n"))
    }

    #[test]
    fn reads_what_playerctl_prints() {
        let found = players(&[line("spotify", "Paused", "Song")]);
        assert_eq!(
            found,
            vec![Player {
                name: "spotify".into(),
                playing: false,
                title: "Song".into(),
                artist: "Artist".into(),
                album: "Album".into(),
                position_ms: 120_650,
                length_ms: 336_812,
                art_url: "https://i.example/a".into(),
            }]
        );
    }

    #[test]
    fn players_with_nothing_loaded_or_odd_lines_are_skipped() {
        assert!(players(&[line("firefox", "Stopped", ""), "No players found".to_string(), String::new()]).is_empty());
    }

    #[test]
    fn missing_times_are_zero() {
        let found = parse_players("vlc\u{1f}Playing\u{1f}Stream\u{1f}\u{1f}\u{1f}\u{1f}\u{1f}");
        assert_eq!((found[0].position_ms, found[0].length_ms), (0, 0));
        assert!(found[0].playing);
    }

    #[test]
    fn the_one_playing_comes_first_and_stays() {
        let found = players(&[line("a", "Paused", "1"), line("b", "Playing", "2"), line("c", "Playing", "3")]);
        assert_eq!(pick(&found, None).unwrap().name, "b");
        assert_eq!(pick(&found, Some("a")).unwrap().name, "b", "a paused one gives way");
        assert_eq!(pick(&found, Some("c")).unwrap().name, "c", "no jumping between two playing");
    }

    #[test]
    fn with_nothing_playing_it_stays_on_the_last_one() {
        let found = players(&[line("a", "Paused", "1"), line("b", "Paused", "2")]);
        assert_eq!(pick(&found, Some("b")).unwrap().name, "b");
        assert_eq!(pick(&found, Some("gone")).unwrap().name, "a");
        assert_eq!(pick(&[], Some("a")), None);
    }

    #[test]
    fn with_nothing_playing_real_music_comes_before_a_browser_tab() {
        let mut found = players(&[line("firefox", "Paused", "A page"), line("spotify", "Paused", "A song")]);
        found[0].artist.clear();
        assert_eq!(pick(&found, None).unwrap().name, "spotify");
        assert_eq!(pick(&found, Some("firefox")).unwrap().name, "firefox", "unless it's the one in use");
    }

    #[test]
    fn reads_the_volume() {
        assert_eq!(parse_volume("Volume: 0.52\n"), Some((52, false)));
        assert_eq!(parse_volume("Volume: 0.40 [MUTED]\n"), Some((40, true)));
        assert_eq!(parse_volume("Volume: 1.30\n"), Some((100, false)));
        assert_eq!(parse_volume("nonsense"), None);
    }

    #[test]
    fn only_images_count_as_art() {
        assert!(is_image(&[0xff, 0xd8, 0xff, 0xe0]));
        assert!(is_image(b"\x89PNG\r\n"));
        assert!(is_image(b"RIFF\0\0\0\0WEBPVP8 "));
        assert!(!is_image(b"-----BEGIN PRIVATE KEY-----"));
        assert!(!is_image(b""));
    }

    #[test]
    fn decodes_art_paths() {
        assert_eq!(percent_decode("/tmp/a%20b.png"), b"/tmp/a b.png");
        assert_eq!(percent_decode("/tmp/%D8%B3"), "/tmp/س".as_bytes());
        assert_eq!(percent_decode("/tmp/100%"), b"/tmp/100%");
        assert_eq!(percent_decode("/tmp/%zz"), b"/tmp/%zz");
    }

    #[test]
    fn the_position_only_goes_out_after_a_seek() {
        let sent = MediaState { title: "a".into(), playing: true, position_ms: 10_000, ..Default::default() };
        let later = |position_ms| MediaState { position_ms, ..sent.clone() };

        assert!(worth_sending(None, 0, &sent));
        assert!(!worth_sending(Some(&sent), 2000, &later(12_000)), "running as expected");
        assert!(worth_sending(Some(&sent), 2000, &later(60_000)), "a seek forward");
        assert!(worth_sending(Some(&sent), 2000, &later(1000)), "a seek back");

        let paused = MediaState { playing: false, ..sent.clone() };
        assert!(!worth_sending(Some(&paused), 60_000, &paused), "paused, so time doesn't count");
        assert!(worth_sending(Some(&sent), 0, &paused), "pausing itself");
        assert!(worth_sending(Some(&sent), 0, &MediaState { volume_percent: 9, ..sent.clone() }), "the volume");
        assert!(worth_sending(Some(&sent), 0, &MediaState { title: "b".into(), ..sent.clone() }), "a new track");
    }
}
