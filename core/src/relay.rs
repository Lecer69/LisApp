//! The way through a relay server, for when the phone can't reach the PC on the network or the cable.
//! The relay only joins two TCP connections: the devices still run TLS with each other inside, so it
//! sees nothing but the PC's fingerprint, which it routes by. Each request carries an HMAC of the
//! relay's password over a fresh nonce, so the password itself never crosses the network.
//!
//! The relay greets with `lis-relay 1 <nonce>`, the client asks with `<verb> <arg> <mac>`, and the relay
//! answers `ok` or `err <reason>`. Verbs: `listen <pc>` (the PC, which then hears `call <token>`),
//! `dial <pc>` (the phone) and `take <token>` (the PC answering a call). After `ok` to a dial or a take,
//! the connection is the tunnel.

use ring::hmac;
use std::io;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

pub const DEFAULT_RELAY_PORT: u16 = 47421;
pub const GREETING: &str = "lis-relay 1";
pub const MAX_LINE: usize = 512;
const LIMIT: Duration = Duration::from_secs(10);

pub fn mac(password: &str, nonce: &str, verb: &str, arg: &str) -> String {
    let key = hmac::Key::new(hmac::HMAC_SHA256, password.as_bytes());
    let message = format!("lis-relay\0{nonce}\0{verb}\0{arg}");
    hex::encode(hmac::sign(&key, message.as_bytes()).as_ref())
}

pub fn mac_matches(password: &str, nonce: &str, verb: &str, arg: &str, given: &str) -> bool {
    let Ok(given) = hex::decode(given) else { return false };
    let key = hmac::Key::new(hmac::HMAC_SHA256, password.as_bytes());
    let message = format!("lis-relay\0{nonce}\0{verb}\0{arg}");
    hmac::verify(&key, message.as_bytes(), &given).is_ok()
}

/// One byte at a time, so nothing after the line is read: past `ok`, the bytes are the tunnel's.
pub async fn read_line<R: AsyncRead + Unpin>(reader: &mut R) -> io::Result<String> {
    let mut line = Vec::new();
    loop {
        let byte = reader.read_u8().await?;
        if byte == b'\n' {
            break;
        }
        if line.len() >= MAX_LINE {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "line too long"));
        }
        line.push(byte);
    }
    String::from_utf8(line).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "not text"))
}

pub async fn write_line<W: AsyncWrite + Unpin>(writer: &mut W, line: &str) -> io::Result<()> {
    writer.write_all(format!("{line}\n").as_bytes()).await?;
    writer.flush().await
}

fn refused(reason: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, reason.to_string())
}

/// Connects and makes one request. The stream is ready for what the verb leads to.
async fn request(host: &str, port: u16, password: &str, verb: &str, arg: &str) -> io::Result<TcpStream> {
    timeout(LIMIT, async {
        let mut tcp = TcpStream::connect((host, port)).await?;
        tcp.set_nodelay(true)?;
        let greeting = read_line(&mut tcp).await?;
        let nonce = greeting
            .strip_prefix(GREETING)
            .and_then(|rest| rest.strip_prefix(' '))
            .ok_or_else(|| refused("not a Lis relay"))?
            .to_string();
        write_line(&mut tcp, &format!("{verb} {arg} {}", mac(password, &nonce, verb, arg))).await?;
        Ok(tcp)
    })
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "the relay didn't answer"))?
}

/// The relay's answer to a request.
async fn answer(tcp: &mut TcpStream, limit: Duration) -> io::Result<()> {
    let line = timeout(limit, read_line(tcp))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "the relay didn't answer"))??;
    match line.as_str() {
        "ok" => Ok(()),
        other => Err(refused(other.strip_prefix("err ").unwrap_or("the relay refused"))),
    }
}

/// The phone: a tunnel to the PC with this fingerprint, if it's listening there.
pub async fn dial(host: &str, port: u16, password: &str, pc: &str) -> io::Result<TcpStream> {
    let mut tcp = request(host, port, password, "dial", pc).await?;
    // The PC has to open its side first
    answer(&mut tcp, LIMIT * 2).await?;
    Ok(tcp)
}

/// The PC: a connection that hears calls, one `call <token>` line each, and `ping` now and then.
pub async fn listen(host: &str, port: u16, password: &str, pc: &str) -> io::Result<TcpStream> {
    let mut tcp = request(host, port, password, "listen", pc).await?;
    answer(&mut tcp, LIMIT).await?;
    Ok(tcp)
}

/// The PC's side of the tunnel for a call.
pub async fn take(host: &str, port: u16, password: &str, token: &str) -> io::Result<TcpStream> {
    let mut tcp = request(host, port, password, "take", token).await?;
    answer(&mut tcp, LIMIT).await?;
    Ok(tcp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mac_binds_everything() {
        let good = mac("pw", "n", "dial", "fp");
        assert!(mac_matches("pw", "n", "dial", "fp", &good));
        assert!(!mac_matches("other", "n", "dial", "fp", &good));
        assert!(!mac_matches("pw", "m", "dial", "fp", &good));
        assert!(!mac_matches("pw", "n", "listen", "fp", &good));
        assert!(!mac_matches("pw", "n", "dial", "fq", &good));
        assert!(!mac_matches("pw", "n", "dial", "fp", "zz"));
    }

    #[tokio::test]
    async fn a_line_stops_at_its_end() {
        let mut bytes: &[u8] = b"ok\nrest";
        assert_eq!(read_line(&mut bytes).await.unwrap(), "ok");
        assert_eq!(bytes, b"rest");
    }
}
