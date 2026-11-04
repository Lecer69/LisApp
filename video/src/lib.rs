//! The camera stream's packets, shared by the phone, the daemon and the OBS plugin: a kind byte, a
//! big-endian length, then the payload. It has no async code, so the plugin stays small.

pub mod h264;

use serde::{Deserialize, Serialize};
use std::io::{self, Read};

pub const HEADER_LEN: usize = 5;
pub const CODEC_H264: &str = "h264";

const MAX_FRAME_LEN: usize = 8 * 1024 * 1024;
const MAX_INFO_LEN: usize = 64 * 1024;
const FRAME_HEADER_LEN: usize = 9;

const KIND_INFO: u8 = 0;
const KIND_FRAME: u8 = 1;
const KIND_VIEWERS: u8 = 2;
const KIND_END: u8 = 3;

const FLAG_KEYFRAME: u8 = 1;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct VideoInfo {
    pub codec: String,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate: u32,
    /// Degrees clockwise to show the frames upright.
    pub rotation: u32,
    pub camera: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VideoFrame {
    pub pts_us: i64,
    pub keyframe: bool,
    /// Annex B, with SPS and PPS before every keyframe.
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Packet {
    Info(VideoInfo),
    Frame(VideoFrame),
    /// From the daemon to the phone: how many are watching.
    Viewers(u32),
    /// From the daemon to a viewer: the camera stopped.
    End,
}

fn bad(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what)
}

pub fn encode(packet: &Packet) -> io::Result<Vec<u8>> {
    let (kind, mut bytes) = match packet {
        Packet::Info(info) => (KIND_INFO, serde_json::to_vec(info)?),
        Packet::Frame(frame) => {
            let mut bytes = Vec::with_capacity(HEADER_LEN + FRAME_HEADER_LEN + frame.data.len());
            bytes.resize(HEADER_LEN, 0);
            bytes.extend_from_slice(&frame.pts_us.to_be_bytes());
            bytes.push(if frame.keyframe { FLAG_KEYFRAME } else { 0 });
            bytes.extend_from_slice(&frame.data);
            return finish(KIND_FRAME, bytes);
        }
        Packet::Viewers(count) => (KIND_VIEWERS, count.to_be_bytes().to_vec()),
        Packet::End => (KIND_END, Vec::new()),
    };
    let mut framed = vec![0; HEADER_LEN];
    framed.append(&mut bytes);
    finish(kind, framed)
}

fn finish(kind: u8, mut bytes: Vec<u8>) -> io::Result<Vec<u8>> {
    let len = bytes.len() - HEADER_LEN;
    check_len(kind, len)?;
    bytes[0] = kind;
    bytes[1..HEADER_LEN].copy_from_slice(&(len as u32).to_be_bytes());
    Ok(bytes)
}

fn check_len(kind: u8, len: usize) -> io::Result<()> {
    let ok = match kind {
        KIND_INFO => len <= MAX_INFO_LEN,
        KIND_FRAME => (FRAME_HEADER_LEN..=MAX_FRAME_LEN + FRAME_HEADER_LEN).contains(&len),
        KIND_VIEWERS => len == 4,
        KIND_END => len == 0,
        _ => return Err(bad("unknown video packet")),
    };
    if ok { Ok(()) } else { Err(bad("bad video packet length")) }
}

/// The kind and payload length. Checked here so nothing huge gets allocated.
pub fn parse_header(header: &[u8; HEADER_LEN]) -> io::Result<(u8, usize)> {
    let len = u32::from_be_bytes([header[1], header[2], header[3], header[4]]) as usize;
    check_len(header[0], len)?;
    Ok((header[0], len))
}

pub fn parse_payload(kind: u8, mut payload: Vec<u8>) -> io::Result<Packet> {
    Ok(match kind {
        KIND_INFO => Packet::Info(serde_json::from_slice(&payload)?),
        KIND_FRAME => {
            let mut pts = [0u8; 8];
            pts.copy_from_slice(&payload[..8]);
            let keyframe = payload[8] & FLAG_KEYFRAME != 0;
            Packet::Frame(VideoFrame { pts_us: i64::from_be_bytes(pts), keyframe, data: payload.split_off(FRAME_HEADER_LEN) })
        }
        KIND_VIEWERS => Packet::Viewers(u32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]])),
        KIND_END => Packet::End,
        _ => return Err(bad("unknown video packet")),
    })
}

pub fn read_packet<R: Read>(reader: &mut R) -> io::Result<Packet> {
    let mut header = [0u8; HEADER_LEN];
    reader.read_exact(&mut header)?;
    let (kind, len) = parse_header(&header)?;
    let mut payload = vec![0u8; len];
    reader.read_exact(&mut payload)?;
    parse_payload(kind, payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info() -> VideoInfo {
        VideoInfo { codec: CODEC_H264.into(), width: 1920, height: 1080, fps: 60, bitrate: 16_000_000, rotation: 90, camera: "back".into() }
    }

    #[test]
    fn packets_round_trip() {
        for packet in [
            Packet::Info(info()),
            Packet::Frame(VideoFrame { pts_us: 123_456_789, keyframe: true, data: vec![0, 0, 0, 1, 0x65] }),
            Packet::Frame(VideoFrame { pts_us: -1, keyframe: false, data: vec![] }),
            Packet::Viewers(2),
            Packet::End,
        ] {
            let bytes = encode(&packet).unwrap();
            assert_eq!(read_packet(&mut bytes.as_slice()).unwrap(), packet);
        }
    }

    #[test]
    fn several_packets_read_back_in_order() {
        let mut bytes = encode(&Packet::Viewers(1)).unwrap();
        bytes.extend(encode(&Packet::End).unwrap());
        let mut reader = bytes.as_slice();
        assert_eq!(read_packet(&mut reader).unwrap(), Packet::Viewers(1));
        assert_eq!(read_packet(&mut reader).unwrap(), Packet::End);
        assert!(read_packet(&mut reader).is_err());
    }

    #[test]
    fn bad_lengths_are_refused_before_allocating() {
        for header in [
            [KIND_FRAME, 0xff, 0xff, 0xff, 0xff],
            [KIND_FRAME, 0, 0, 0, 3],
            [KIND_INFO, 0xff, 0xff, 0xff, 0xff],
            [KIND_VIEWERS, 0, 0, 0, 3],
            [KIND_END, 0, 0, 0, 1],
            [9, 0, 0, 0, 0],
        ] {
            assert!(parse_header(&header).is_err(), "{header:?}");
        }
    }

    #[test]
    fn a_frame_over_the_limit_cannot_be_encoded() {
        let frame = Packet::Frame(VideoFrame { pts_us: 0, keyframe: false, data: vec![0; MAX_FRAME_LEN + 1] });
        assert!(encode(&frame).is_err());
    }

    #[test]
    fn malformed_payloads_are_errors() {
        assert!(parse_payload(KIND_INFO, b"not json".to_vec()).is_err());
        assert!(read_packet(&mut [KIND_FRAME, 0, 0, 0, 9, 1].as_slice()).is_err());
    }
}
