//! A websocket client (RFC 6455) over any `embedded_io_async` stream, as the
//! agent's `/agi` socket needs it: text frames, pings answered, fragmented
//! messages joined, close frames reported. Buffers grow as frames need
//! (REGISTER carries the whole declaration, ASSIGN its arguments).
//!
//! [`WebSocket::next`] keeps every partial frame in its own buffer and only
//! awaits the stream's `read`: dropping it (a `select` that took another
//! branch) loses nothing as long as that `read` is cancel-safe, which
//! embassy-net's TCP socket and the host adapters are.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use base64::Engine;
use embedded_io_async::{Read, Write};
use sha1::{Digest, Sha1};

const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
/// Refuse messages larger than this (a misbehaving peer, not a real frame).
const MAX_MESSAGE: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub enum WsError {
    Io(String),
    Handshake(String),
    Protocol(String),
}

impl core::fmt::Display for WsError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "io: {e}"),
            Self::Handshake(e) => write!(f, "handshake: {e}"),
            Self::Protocol(e) => write!(f, "protocol: {e}"),
        }
    }
}

fn io<E: core::fmt::Debug>(e: E) -> WsError {
    WsError::Io(alloc::format!("{e:?}"))
}

/// What came in.
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    Text(String),
    /// The peer pinged: answer with [`WebSocket::pong`].
    Ping(Vec<u8>),
    Closed {
        code: u16,
        reason: String,
    },
}

pub struct WebSocket<S> {
    stream: S,
    frames: Frames,
    fill_random: fn(&mut [u8]),
}

/// Bytes in, whole messages out.
#[derive(Default)]
struct Frames {
    /// Bytes read but not yet framed.
    buffer: Vec<u8>,
    /// A fragmented message so far.
    message: Vec<u8>,
}

impl<S: Read + Write> WebSocket<S> {
    /// Upgrade `stream` (connected to `host`) to a websocket at `path`.
    pub async fn connect(
        mut stream: S,
        host: &str,
        path: &str,
        fill_random: fn(&mut [u8]),
    ) -> Result<Self, WsError> {
        let mut nonce = [0u8; 16];
        fill_random(&mut nonce);
        let key = base64::engine::general_purpose::STANDARD.encode(nonce);
        let request = alloc::format!(
            "GET {path} HTTP/1.1\r\nHost: {host}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
             Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
        );
        stream.write_all(request.as_bytes()).await.map_err(io)?;
        stream.flush().await.map_err(io)?;

        let mut buffer = vec![];
        let end = loop {
            if let Some(end) = find(&buffer, b"\r\n\r\n") {
                break end;
            }
            if buffer.len() > 16 * 1024 {
                return Err(WsError::Handshake("an oversized response".into()));
            }
            let mut chunk = [0u8; 512];
            let n = stream.read(&mut chunk).await.map_err(io)?;
            if n == 0 {
                return Err(WsError::Handshake(
                    "the server closed the connection".into(),
                ));
            }
            buffer.extend_from_slice(&chunk[..n]);
        };
        let head = core::str::from_utf8(&buffer[..end])
            .map_err(|_| WsError::Handshake("a response that is not text".into()))?;
        let status = head.lines().next().unwrap_or_default();
        if status.split_whitespace().nth(1) != Some("101") {
            return Err(WsError::Handshake(alloc::format!(
                "the server answered {status}"
            )));
        }
        let expected = accept_for(&key);
        let accepted = head.lines().any(|line| {
            line.split_once(':').is_some_and(|(name, value)| {
                name.trim().eq_ignore_ascii_case("sec-websocket-accept") && value.trim() == expected
            })
        });
        if !accepted {
            return Err(WsError::Handshake(alloc::format!(
                "the server did not accept our key (expected {expected}): {head}"
            )));
        }
        Ok(Self {
            stream,
            frames: Frames {
                buffer: buffer[end + 4..].to_vec(),
                message: vec![],
            },
            fill_random,
        })
    }

    pub async fn send_text(&mut self, text: &str) -> Result<(), WsError> {
        self.send(0x1, text.as_bytes()).await
    }

    pub async fn pong(&mut self, payload: &[u8]) -> Result<(), WsError> {
        self.send(0xA, payload).await
    }

    /// Send a close frame (normal closure) and stop.
    pub async fn close(&mut self) {
        let _ = self.send(0x8, &1000u16.to_be_bytes()).await;
    }

    /// One frame, masked as a client must.
    async fn send(&mut self, opcode: u8, payload: &[u8]) -> Result<(), WsError> {
        let mut frame = Vec::with_capacity(payload.len() + 14);
        frame.push(0x80 | opcode);
        match payload.len() {
            n if n < 126 => frame.push(0x80 | n as u8),
            n if n <= u16::MAX as usize => {
                frame.push(0x80 | 126);
                frame.extend_from_slice(&(n as u16).to_be_bytes());
            }
            n => {
                frame.push(0x80 | 127);
                frame.extend_from_slice(&(n as u64).to_be_bytes());
            }
        }
        let mut mask = [0u8; 4];
        (self.fill_random)(&mut mask);
        frame.extend_from_slice(&mask);
        frame.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
        self.stream.write_all(&frame).await.map_err(io)?;
        self.stream.flush().await.map_err(io)
    }

    /// The next text message, ping or close. See the module docs on cancelling.
    pub async fn next(&mut self) -> Result<Incoming, WsError> {
        loop {
            if let Some(incoming) = self.frames.take()? {
                return Ok(incoming);
            }
            let mut chunk = [0u8; 2048];
            let n = self.stream.read(&mut chunk).await.map_err(io)?;
            if n == 0 {
                return Ok(Incoming::Closed {
                    code: 1006,
                    reason: "the connection ended".into(),
                });
            }
            self.frames.buffer.extend_from_slice(&chunk[..n]);
        }
    }
}

impl Frames {
    /// A whole message (or control frame) from the buffer, if there is one.
    fn take(&mut self) -> Result<Option<Incoming>, WsError> {
        loop {
            let Some((fin, opcode, payload, used)) = parse(&self.buffer)? else {
                return Ok(None);
            };
            self.buffer.drain(..used);
            match opcode {
                0x0..=0x2 => {
                    if opcode != 0x0 && !self.message.is_empty() {
                        return Err(WsError::Protocol("a new message inside another".into()));
                    }
                    self.message.extend_from_slice(&payload);
                    if self.message.len() > MAX_MESSAGE {
                        return Err(WsError::Protocol("an oversized message".into()));
                    }
                    if fin {
                        let message = core::mem::take(&mut self.message);
                        return String::from_utf8(message)
                            .map(|text| Some(Incoming::Text(text)))
                            .map_err(|_| WsError::Protocol("a message that is not text".into()));
                    }
                }
                0x8 => {
                    let code = payload
                        .get(..2)
                        .map(|b| u16::from_be_bytes([b[0], b[1]]))
                        .unwrap_or(1005);
                    let reason =
                        String::from_utf8_lossy(payload.get(2..).unwrap_or_default()).into();
                    return Ok(Some(Incoming::Closed { code, reason }));
                }
                0x9 => return Ok(Some(Incoming::Ping(payload))),
                // Pongs answer pings we never send.
                0xA => {}
                other => return Err(WsError::Protocol(alloc::format!("unknown opcode {other}"))),
            }
        }
    }
}

/// The `Sec-WebSocket-Accept` a server answers `key` with.
fn accept_for(key: &str) -> String {
    base64::engine::general_purpose::STANDARD
        .encode(Sha1::digest(alloc::format!("{key}{GUID}").as_bytes()))
}

/// `(fin, opcode, payload, bytes used)` of the frame at the start of `bytes`.
#[allow(clippy::type_complexity)]
fn parse(bytes: &[u8]) -> Result<Option<(bool, u8, Vec<u8>, usize)>, WsError> {
    if bytes.len() < 2 {
        return Ok(None);
    }
    let fin = bytes[0] & 0x80 != 0;
    let opcode = bytes[0] & 0x0F;
    let masked = bytes[1] & 0x80 != 0;
    let (len, mut at) = match bytes[1] & 0x7F {
        126 if bytes.len() >= 4 => (u16::from_be_bytes([bytes[2], bytes[3]]) as usize, 4),
        127 if bytes.len() >= 10 => {
            let mut raw = [0u8; 8];
            raw.copy_from_slice(&bytes[2..10]);
            let len = u64::from_be_bytes(raw);
            if len > MAX_MESSAGE as u64 {
                return Err(WsError::Protocol("an oversized frame".into()));
            }
            (len as usize, 10)
        }
        126 | 127 => return Ok(None),
        n => (n as usize, 2),
    };
    let mask = if masked {
        if bytes.len() < at + 4 {
            return Ok(None);
        }
        let mask = [bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]];
        at += 4;
        Some(mask)
    } else {
        None
    };
    if bytes.len() < at + len {
        return Ok(None);
    }
    let mut payload = bytes[at..at + len].to_vec();
    if let Some(mask) = mask {
        for (i, b) in payload.iter_mut().enumerate() {
            *b ^= mask[i % 4];
        }
    }
    Ok(Some((fin, opcode, payload, at + len)))
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(fin: bool, opcode: u8, payload: &[u8]) -> Vec<u8> {
        let mut f = vec![(if fin { 0x80 } else { 0 }) | opcode];
        match payload.len() {
            n if n < 126 => f.push(n as u8),
            n => {
                f.push(126);
                f.extend_from_slice(&(n as u16).to_be_bytes());
            }
        }
        f.extend_from_slice(payload);
        f
    }

    #[test]
    fn the_accept_is_rfc_6455s() {
        assert_eq!(
            accept_for("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }

    #[test]
    fn frames_split_anywhere_and_fragments_join() {
        let mut bytes = frame(false, 0x1, b"hel");
        bytes.extend(frame(true, 0x0, b"lo"));
        bytes.extend(frame(true, 0x9, b"p"));
        let big = "x".repeat(300);
        bytes.extend(frame(true, 0x1, big.as_bytes()));
        bytes.extend(frame(true, 0x8, &[0x0f, 0xa3, b'b', b'y', b'e']));

        // Byte by byte, as a slow socket would deliver it.
        let mut frames = Frames::default();
        let mut seen = vec![];
        for b in bytes {
            frames.buffer.push(b);
            while let Some(incoming) = frames.take().unwrap() {
                seen.push(incoming);
            }
        }
        assert_eq!(
            seen,
            vec![
                Incoming::Text("hello".into()),
                Incoming::Ping(b"p".to_vec()),
                Incoming::Text(big),
                Incoming::Closed {
                    code: 4003,
                    reason: "bye".into()
                },
            ]
        );
    }
}
