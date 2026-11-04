//! The camera stream on async connections. The packets themselves are in `lis-video`.

pub use lis_video::*;

use std::io;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub async fn read_packet_async<R: AsyncRead + Unpin>(reader: &mut R) -> io::Result<Packet> {
    let mut header = [0u8; HEADER_LEN];
    reader.read_exact(&mut header).await?;
    let (kind, len) = parse_header(&header)?;
    let mut payload = vec![0u8; len];
    reader.read_exact(&mut payload).await?;
    parse_payload(kind, payload)
}

pub async fn write_packet_async<W: AsyncWrite + Unpin>(writer: &mut W, packet: &Packet) -> io::Result<()> {
    writer.write_all(&encode(packet)?).await?;
    writer.flush().await
}
