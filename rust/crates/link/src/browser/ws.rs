//! WebSocket framing (RFC 6455), just what a browser sends (browser_server.py's ws_* and _WsConnection).

use base64::Engine;
use std::io::ErrorKind;
use std::time::{Duration, Instant};
use telescope_core::web::Socket;

const WS_GUID: &[u8] = b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
/// An offer with every codec and address is ~10 KB; anything bigger ends the connection.
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;
/// Nothing at all for this long, not even a pong (a phone that went to sleep), ends the connection.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(10);

pub const TEXT: u8 = 0x1;
pub const BINARY: u8 = 0x2;
pub const CLOSE: u8 = 0x8;
pub const PING: u8 = 0x9;
pub const PONG: u8 = 0xA;

/// Why the connection ended, cleanly or not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Closed(pub String);

pub fn accept_key(key: &str) -> String {
    let mut ctx = ring::digest::Context::new(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY);
    ctx.update(key.as_bytes());
    ctx.update(WS_GUID);
    base64::engine::general_purpose::STANDARD.encode(ctx.finish().as_ref())
}

/// A server-to-client frame: final, unmasked.
pub fn frame(opcode: u8, payload: &[u8]) -> Vec<u8> {
    let n = payload.len();
    let mut out = vec![0x80 | opcode];
    if n < 126 {
        out.push(n as u8);
    } else if n < 1 << 16 {
        out.push(126);
        out.extend_from_slice(&(n as u16).to_be_bytes());
    } else {
        out.push(127);
        out.extend_from_slice(&(n as u64).to_be_bytes());
    }
    out.extend_from_slice(payload);
    out
}

pub fn close_frame(code: u16, reason: &str) -> Vec<u8> {
    let mut payload = code.to_be_bytes().to_vec();
    let mut cut = reason.len().min(120);
    while !reason.is_char_boundary(cut) {
        cut -= 1;
    }
    payload.extend_from_slice(&reason.as_bytes()[..cut]);
    frame(CLOSE, &payload)
}

pub fn unmask(payload: &[u8], mask: [u8; 4]) -> Vec<u8> {
    payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]).collect()
}

/// Reads a browser's frames off the socket with its own buffer, so a read timeout (used to send pending messages and
/// notice a quiet peer) never loses half a frame. Everything is sent from the reading thread too.
pub struct WsConnection<'a> {
    sock: Box<dyn Socket>,
    buf: Vec<u8>,
    on_tick: Box<dyn FnMut(&mut dyn FnMut(&[u8]) -> Result<(), Closed>) -> Result<(), Closed> + 'a>,
    last_heard: Instant,
}

impl<'a> WsConnection<'a> {
    /// on_tick runs each time a read times out with nothing in, and may send through what it's handed.
    pub fn new(sock: Box<dyn Socket>, on_tick: impl FnMut(&mut dyn FnMut(&[u8]) -> Result<(), Closed>) -> Result<(), Closed> + 'a)
               -> WsConnection<'a> {
        WsConnection { sock, buf: Vec::new(), on_tick: Box::new(on_tick), last_heard: Instant::now() }
    }

    pub fn send(&mut self, data: &[u8]) -> Result<(), Closed> {
        send_on(&mut *self.sock, data)
    }

    /// Runs the tick now, as a read timing out would.
    pub fn tick(&mut self) -> Result<(), Closed> {
        let sock = &mut *self.sock;
        (self.on_tick)(&mut |data| send_on(sock, data))
    }

    fn fill(&mut self, n: usize) -> Result<(), Closed> {
        let mut chunk = [0u8; 65536];
        while self.buf.len() < n {
            match self.sock.read(&mut chunk) {
                Ok(0) => return Err(Closed("eof".into())),
                Ok(got) => {
                    self.last_heard = Instant::now();
                    self.buf.extend_from_slice(&chunk[..got]);
                }
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                    if self.last_heard.elapsed() > IDLE_TIMEOUT {
                        return Err(Closed("idle".into()));
                    }
                    self.tick()?;
                }
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) => return Err(Closed(e.to_string())),
            }
        }
        Ok(())
    }

    fn take(&mut self, n: usize) -> Result<Vec<u8>, Closed> {
        self.fill(n)?;
        Ok(self.buf.drain(..n).collect())
    }

    /// (final, opcode, unmasked payload) of the next frame.
    pub fn read_frame(&mut self) -> Result<(bool, u8, Vec<u8>), Closed> {
        let head = self.take(2)?;
        let (b0, b1) = (head[0], head[1]);
        let (fin, opcode) = (b0 & 0x80 != 0, b0 & 0x0F);
        if b0 & 0x70 != 0 {
            return Err(Closed("reserved bits".into()));
        }
        if b1 & 0x80 == 0 {
            return Err(Closed("unmasked frame".into())); // browsers always mask
        }
        let mut n = (b1 & 0x7F) as u64;
        if n == 126 {
            n = u16::from_be_bytes(self.take(2)?.try_into().unwrap()) as u64;
        } else if n == 127 {
            n = u64::from_be_bytes(self.take(8)?.try_into().unwrap());
        }
        if n > MAX_MESSAGE_BYTES as u64 {
            return Err(Closed("frame too big".into()));
        }
        let mask: [u8; 4] = self.take(4)?.try_into().unwrap();
        let payload = self.take(n as usize)?;
        Ok((fin, opcode, unmask(&payload, mask)))
    }

    /// (opcode, payload) of the next text or binary message, answering pings and assembling fragments.
    pub fn read_message(&mut self) -> Result<(u8, Vec<u8>), Closed> {
        let (mut opcode, mut parts) = (None, Vec::new());
        loop {
            let (fin, op, payload) = self.read_frame()?;
            match op {
                CLOSE => {
                    let _ = self.send(&close_frame(1000, ""));
                    return Err(Closed("closed by browser".into()));
                }
                PING => {
                    self.send(&frame(PONG, &payload[..payload.len().min(125)]))?;
                    continue;
                }
                PONG => continue,
                TEXT | BINARY => {
                    if opcode.is_some() {
                        return Err(Closed("new message inside a fragmented one".into()));
                    }
                    opcode = Some(op);
                }
                0x0 if opcode.is_some() => {}
                _ => return Err(Closed(format!("unexpected opcode {op}"))),
            }
            parts.extend_from_slice(&payload);
            if parts.len() > MAX_MESSAGE_BYTES {
                return Err(Closed("message too big".into()));
            }
            if fin {
                return Ok((opcode.unwrap_or(TEXT), parts));
            }
        }
    }

    /// Sends and forgets: the connection is ending anyway.
    pub fn send_quietly(&mut self, data: &[u8]) {
        let _ = self.send(data);
    }

    pub fn set_read_timeout(&mut self, timeout: Option<Duration>) {
        let _ = self.sock.set_read_timeout(timeout);
    }
}

fn send_on(sock: &mut dyn Socket, data: &[u8]) -> Result<(), Closed> {
    sock.write_all(data).and_then(|_| sock.flush()).map_err(|e| Closed(e.to_string()))
}
