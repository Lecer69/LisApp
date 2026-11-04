//! Frames on the wire: a kind byte, a big-endian length, then JSON or a file chunk.

use serde::{Deserialize, Serialize};
use std::io;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const PROTOCOL_VERSION: u32 = 1;
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
        assert!(Message::Ping.allowed_unpaired());
    }
}
