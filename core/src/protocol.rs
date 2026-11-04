//! Frames on the wire: a kind byte, a big-endian length, then JSON or a file chunk.

use serde::{Deserialize, Serialize};
use std::io;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const PROTOCOL_VERSION: u32 = 2;
pub const DEFAULT_PORT: u16 = 47420;
pub const SERVICE_TYPE: &str = "_lisapp._tcp.local.";
pub const CHUNK_LEN: usize = 64 * 1024;

const MAX_MESSAGE_LEN: usize = 1024 * 1024;
const MAX_CHUNK_FRAME_LEN: usize = CHUNK_LEN + 8;
const KIND_MESSAGE: u8 = 0;
const KIND_CHUNK: u8 = 1;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct PhoneStats {
    pub battery_percent: u8,
    pub charging: bool,
    pub storage_used: u64,
    pub storage_total: u64,
    pub memory_used: u64,
    pub memory_total: u64,
    pub network: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct PcStats {
    pub hostname: String,
    pub cpu_percent: f32,
    pub memory_used: u64,
    pub memory_total: u64,
    pub uptime_secs: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct FileOffer {
    pub id: u64,
    pub name: String,
    pub size: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct MediaState {
    pub player: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub playing: bool,
    pub position_ms: u64,
    pub length_ms: u64,
    pub volume_percent: u8,
    pub muted: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum MediaAction {
    PlayPause,
    Next,
    Previous,
    SetVolume { percent: u8 },
}

/// A button on the phone. Its command stays on the PC.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ActionInfo {
    pub id: String,
    pub name: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Message {
    Hello { protocol: u32, name: String },
    Auth { trusted: bool },
    PairCommit { commitment: String },
    PairNonce { nonce: String },
    PairReveal { nonce: String },
    PairDenied { reason: String },
    PairConfirm { accept: bool },
    Ping,
    Pong,
    Unpair,
    PhoneStats(PhoneStats),
    PcStats(PcStats),
    LockPc,
    FileOffer(FileOffer),
    FileAccept { id: u64 },
    FileReject { id: u64 },
    FileDone { id: u64 },
    FileAbort { id: u64 },
    FileAck { id: u64, ok: bool },
    RingPhone { ring: bool },
    PhoneRinging { ringing: bool },
    /// The PC's copy of a document, whenever it changes.
    SyncDoc { kind: String, rev: String, doc: serde_json::Value },
    /// The phone's merged copy, taken only if the PC's is still `based_on`.
    SyncPush { kind: String, based_on: String, doc: serde_json::Value },
    SyncRejected { kind: String, reason: String },
    MediaState(MediaState),
    MediaGone,
    /// Hex of the cover image, empty for none.
    MediaArt { data: String },
    MediaControl { action: MediaAction },
    Actions { actions: Vec<ActionInfo> },
    RunAction { id: String },
    ActionResult { id: String, ok: bool },
    /// Hex of an X.509 P-256 key, which signs only after the phone's biometric check.
    UnlockKey { public_key: String },
    /// The phone deleted its key.
    UnlockRevoke,
    UnlockState { locked: bool, available: bool },
    /// Someone is at the locked PC: show the fingerprint prompt without being asked.
    UnlockPrompt,
    UnlockRequest,
    UnlockChallenge { nonce: String },
    UnlockResponse { nonce: String, signature: String },
    UnlockResult { ok: bool, reason: String },
    /// From a newer version of the other device. Ignored, so it doesn't end the connection.
    #[serde(other)]
    Unknown,
}

impl Message {
    /// Allowed before both devices trust each other.
    pub fn allowed_unpaired(&self) -> bool {
        matches!(
            self,
            Message::Hello { .. }
                | Message::Auth { .. }
                | Message::PairCommit { .. }
                | Message::PairNonce { .. }
                | Message::PairReveal { .. }
                | Message::PairDenied { .. }
                | Message::PairConfirm { .. }
                | Message::Ping
                | Message::Pong
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    Message(Message),
    Chunk { id: u64, data: Vec<u8> },
}

pub async fn write_frame<W: AsyncWrite + Unpin>(writer: &mut W, frame: &Frame) -> io::Result<()> {
    let (kind, payload) = match frame {
        Frame::Message(message) => (KIND_MESSAGE, serde_json::to_vec(message)?),
        Frame::Chunk { id, data } => {
            let mut payload = Vec::with_capacity(8 + data.len());
            payload.extend_from_slice(&id.to_be_bytes());
            payload.extend_from_slice(data);
            (KIND_CHUNK, payload)
        }
    };

    let limit = if kind == KIND_MESSAGE { MAX_MESSAGE_LEN } else { MAX_CHUNK_FRAME_LEN };
    if payload.len() > limit {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "frame too large"));
    }

    let mut bytes = Vec::with_capacity(5 + payload.len());
    bytes.push(kind);
    bytes.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    bytes.extend_from_slice(&payload);
    writer.write_all(&bytes).await?;
    writer.flush().await
}

/// The length is checked before anything is allocated.
pub async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> io::Result<Frame> {
    let mut header = [0u8; 5];
    reader.read_exact(&mut header).await?;
    let kind = header[0];
    let len = u32::from_be_bytes([header[1], header[2], header[3], header[4]]) as usize;

    let limit = match kind {
        KIND_MESSAGE => MAX_MESSAGE_LEN,
        KIND_CHUNK => MAX_CHUNK_FRAME_LEN,
        _ => return Err(io::Error::new(io::ErrorKind::InvalidData, "unknown frame kind")),
    };
    if len > limit || (kind == KIND_CHUNK && len < 8) {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "bad frame length"));
    }

    let mut payload = vec![0u8; len];
    reader.read_exact(&mut payload).await?;

    if kind == KIND_MESSAGE {
        Ok(Frame::Message(serde_json::from_slice(&payload)?))
    } else {
        let mut id = [0u8; 8];
        id.copy_from_slice(&payload[..8]);
        Ok(Frame::Chunk { id: u64::from_be_bytes(id), data: payload.split_off(8) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn round_trip(frame: Frame) -> Frame {
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &frame).await.unwrap();
        read_frame(&mut bytes.as_slice()).await.unwrap()
    }

    #[tokio::test]
    async fn frames_round_trip() {
        for frame in [
            Frame::Message(Message::Ping),
            Frame::Message(Message::Hello { protocol: 1, name: "Phone ✓".into() }),
            Frame::Message(Message::PhoneStats(PhoneStats { battery_percent: 80, ..Default::default() })),
            Frame::Message(Message::FileOffer(FileOffer { id: 3, name: "سلام.txt".into(), size: 5 })),
            Frame::Chunk { id: 7, data: vec![1, 2, 3] },
            Frame::Chunk { id: 7, data: vec![] },
        ] {
            assert_eq!(round_trip(frame.clone()).await, frame);
        }
    }

    #[tokio::test]
    async fn new_messages_round_trip() {
        let doc = serde_json::json!({ "tasks": [{ "id": 1_730_000_000_000u64, "text": "سلام" }] });
        for message in [
            Message::RingPhone { ring: true },
            Message::SyncDoc { kind: "todo".into(), rev: "ab".into(), doc: doc.clone() },
            Message::SyncPush { kind: "todo".into(), based_on: "ab".into(), doc },
            Message::MediaState(MediaState { title: "A \"song\"".into(), volume_percent: 52, ..Default::default() }),
            Message::MediaGone,
            Message::MediaControl { action: MediaAction::SetVolume { percent: 40 } },
            Message::MediaControl { action: MediaAction::PlayPause },
            Message::Actions { actions: vec![ActionInfo { id: "a".into(), name: "Main scene".into() }] },
            Message::UnlockRequest,
            Message::UnlockResponse { nonce: "n".into(), signature: "s".into() },
        ] {
            let frame = Frame::Message(message);
            assert_eq!(round_trip(frame.clone()).await, frame);
        }
    }

    #[tokio::test]
    async fn a_message_from_a_newer_version_is_only_ignored() {
        let payload = br#"{"t":"something_new","field":[1,2,3]}"#;
        let mut bytes = vec![KIND_MESSAGE];
        bytes.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        bytes.extend_from_slice(payload);
        assert_eq!(read_frame(&mut bytes.as_slice()).await.unwrap(), Frame::Message(Message::Unknown));
        assert!(!Message::Unknown.allowed_unpaired());
    }

    #[tokio::test]
    async fn oversized_lengths_are_refused_before_allocating() {
        let mut bytes = vec![KIND_MESSAGE];
        bytes.extend_from_slice(&u32::MAX.to_be_bytes());
        assert!(read_frame(&mut bytes.as_slice()).await.is_err());

        let mut bytes = vec![KIND_CHUNK];
        bytes.extend_from_slice(&((CHUNK_LEN + 9) as u32).to_be_bytes());
        assert!(read_frame(&mut bytes.as_slice()).await.is_err());
    }

    #[tokio::test]
    async fn malformed_frames_are_errors() {
        for bytes in [
            vec![9u8, 0, 0, 0, 0],
            vec![KIND_CHUNK, 0, 0, 0, 3, 1, 2, 3],
            vec![KIND_MESSAGE, 0, 0, 0, 2, b'{', b'}'],
            vec![KIND_MESSAGE, 0, 0],
        ] {
            assert!(read_frame(&mut bytes.as_slice()).await.is_err());
        }
    }

    #[test]
    fn data_needs_pairing() {
        assert!(!Message::LockPc.allowed_unpaired());
        assert!(!Message::Unpair.allowed_unpaired());
        assert!(!Message::PhoneStats(PhoneStats::default()).allowed_unpaired());
        assert!(!Message::FileAccept { id: 1 }.allowed_unpaired());
        assert!(!Message::UnlockRequest.allowed_unpaired());
        assert!(!Message::RunAction { id: "a".into() }.allowed_unpaired());
        assert!(!Message::SyncPush { kind: "todo".into(), based_on: String::new(), doc: serde_json::Value::Null }.allowed_unpaired());
        assert!(Message::Ping.allowed_unpaired());
    }
}
