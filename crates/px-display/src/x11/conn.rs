//! One X11 connection over a `UnixStream`: handshake, request sequencing,
//! reply matching, and a buffer of events that arrived while waiting for a
//! reply. Safe Rust throughout.

use std::collections::VecDeque;
use std::io::{ErrorKind, Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use super::discover::Auth;
use super::proto::{self, Message, Req, Setup, SetupReply};
use crate::DisplayError;

/// How long a synchronous round trip may take before the server is declared
/// dead. Fail closed: a hung server must not hang the browser process.
const REPLY_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct XError {
    pub code: u8,
    pub seq: u16,
    pub bad_value: u32,
    pub major: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RawEvent {
    pub code: u8,
    pub sent: bool,
    pub raw: [u8; 32],
}

pub struct Reply {
    pub detail: u8,
    /// Everything after the 8-byte header (so original offset N is index N-8).
    pub body: Vec<u8>,
}

impl Reply {
    pub fn u32_at(&self, orig_offset: usize) -> Option<u32> {
        proto::Reader::new(&self.body).u32_at(orig_offset.checked_sub(8)?)
    }
    pub fn data_from(&self, orig_offset: usize) -> &[u8] {
        self.body.get(orig_offset.saturating_sub(8)..).unwrap_or(&[])
    }
}

pub enum Wait {
    /// Return at once if nothing is buffered.
    Poll,
    Block,
    For(Duration),
}

pub struct Conn {
    stream: UnixStream,
    rbuf: Vec<u8>,
    seq: u16,
    big: bool,
    max_req_bytes: usize,
    pub setup: Setup,
    next_id: u32,
    id_shift: u32,
    nonblocking: bool,
    pub events: VecDeque<RawEvent>,
    pub errors: Vec<XError>,
}

impl Conn {
    pub fn connect(mut stream: UnixStream, auth: &Auth) -> Result<Conn, DisplayError> {
        stream.set_read_timeout(Some(REPLY_TIMEOUT))?;
        stream.write_all(&proto::setup_request(&auth.name, &auth.data))?;
        let mut buf = Vec::new();
        let mut tmp = [0u8; 4096];
        let setup = loop {
            match proto::parse_setup(&buf).map_err(DisplayError::Connect)? {
                SetupReply::Failed(why) => {
                    return Err(DisplayError::Connect(format!("server refused: {why}")));
                }
                SetupReply::Ok(s, used) => {
                    buf.drain(..used);
                    break *s;
                }
                SetupReply::Incomplete => {}
            }
            let n = read_retry(&mut stream, &mut tmp)?;
            if n == 0 {
                return Err(DisplayError::Closed);
            }
            buf.extend_from_slice(tmp.get(..n).unwrap_or(&[]));
            if buf.len() > 1 << 20 {
                return Err(DisplayError::Protocol("setup reply too large".into()));
            }
        };
        if setup.resource_id_mask == 0 {
            return Err(DisplayError::Protocol("empty resource-id mask".into()));
        }
        let id_shift = setup.resource_id_mask.trailing_zeros();
        let max_req_bytes = usize::from(setup.max_request_words) * 4;
        Ok(Conn {
            stream,
            rbuf: buf,
            seq: 1,
            big: false,
            max_req_bytes,
            next_id: 1,
            id_shift,
            setup,
            nonblocking: false,
            events: VecDeque::new(),
            errors: Vec::new(),
        })
    }

    /// Largest request this connection can send, in bytes.
    pub fn max_request_bytes(&self) -> usize {
        self.max_req_bytes
    }

    pub fn enable_big_requests(&mut self, max_words: u32) {
        let bytes = (max_words as usize).saturating_mul(4).min(proto::MAX_MESSAGE);
        if bytes > self.max_req_bytes {
            self.big = true;
            self.max_req_bytes = bytes;
        }
    }

    pub fn alloc_id(&mut self) -> Result<u32, DisplayError> {
        let n = self.next_id;
        let shifted = u64::from(n) << self.id_shift;
        if shifted > u64::from(self.setup.resource_id_mask) {
            return Err(DisplayError::Protocol("resource ids exhausted".into()));
        }
        self.next_id += 1;
        Ok(self.setup.resource_id_base | shifted as u32)
    }

    /// Send a request; returns its sequence number.
    pub fn send(&mut self, req: Req) -> Result<u16, DisplayError> {
        let bytes = req.finish(self.big).map_err(DisplayError::Protocol)?;
        if bytes.len() > self.max_req_bytes {
            return Err(DisplayError::Protocol(format!(
                "request of {} bytes exceeds the server limit {}",
                bytes.len(),
                self.max_req_bytes
            )));
        }
        if self.nonblocking {
            self.stream.set_nonblocking(false)?;
            self.nonblocking = false;
        }
        self.stream.write_all(&bytes)?;
        let s = self.seq;
        self.seq = self.seq.wrapping_add(1);
        Ok(s)
    }

    /// Pull bytes from the socket. `Ok(true)` if any arrived, `Ok(false)` on
    /// timeout; end of stream is `Err(Closed)`.
    pub fn fill(&mut self, wait: Wait) -> Result<bool, DisplayError> {
        let poll = match wait {
            Wait::Poll => true,
            Wait::For(d) if d.is_zero() => true,
            _ => false,
        };
        if poll != self.nonblocking {
            self.stream.set_nonblocking(poll)?;
            self.nonblocking = poll;
        }
        if !poll {
            let t = match wait {
                Wait::For(d) => Some(d),
                _ => None,
            };
            self.stream.set_read_timeout(t)?;
        }
        let mut tmp = [0u8; 16384];
        loop {
            match self.stream.read(&mut tmp) {
                Ok(0) => return Err(DisplayError::Closed),
                Ok(n) => {
                    self.rbuf.extend_from_slice(tmp.get(..n).unwrap_or(&[]));
                    if self.rbuf.len() > 2 * proto::MAX_MESSAGE {
                        return Err(DisplayError::Protocol("receive buffer overflow".into()));
                    }
                    return Ok(true);
                }
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                    return Ok(false);
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// The next complete message in the buffer, if any.
    pub fn next_message(&mut self) -> Result<Option<Message>, DisplayError> {
        match proto::parse_message(&self.rbuf).map_err(DisplayError::Protocol)? {
            None => Ok(None),
            Some((m, used)) => {
                self.rbuf.drain(..used);
                Ok(Some(m))
            }
        }
    }

    /// Move every complete buffered message into `events`/`errors`.
    pub fn drain_buffered(&mut self) -> Result<(), DisplayError> {
        while let Some(m) = self.next_message()? {
            self.file(m);
        }
        Ok(())
    }

    fn file(&mut self, m: Message) {
        match m {
            Message::Event { code, sent, raw } => self.events.push_back(RawEvent { code, sent, raw }),
            Message::Error {
                code,
                seq,
                bad_value,
                major,
            } => self.errors.push(XError {
                code,
                seq,
                bad_value,
                major,
            }),
            // A reply nobody is waiting for: ignore.
            Message::Reply { .. } => {}
        }
    }

    /// Block for the reply to request `seq`. Events seen meanwhile are kept.
    pub fn reply(&mut self, seq: u16) -> Result<Reply, DisplayError> {
        loop {
            while let Some(m) = self.next_message()? {
                match m {
                    Message::Reply { seq: s, detail, body } if s == seq => {
                        return Ok(Reply { detail, body });
                    }
                    Message::Error { seq: s, code, major, .. } if s == seq => {
                        return Err(DisplayError::Protocol(format!(
                            "X error {code} from request opcode {major}"
                        )));
                    }
                    other => self.file(other),
                }
            }
            if !self.fill(Wait::For(REPLY_TIMEOUT))? {
                return Err(DisplayError::Protocol("timed out waiting for the server".into()));
            }
        }
    }

    pub fn round_trip(&mut self, req: Req) -> Result<Reply, DisplayError> {
        let s = self.send(req)?;
        self.reply(s)
    }

    /// Round trip that also flushes the server's processing of earlier requests.
    pub fn sync(&mut self) -> Result<(), DisplayError> {
        self.round_trip(Req::new(43, 0)).map(|_| ())
    }
}

fn read_retry(s: &mut UnixStream, buf: &mut [u8]) -> Result<usize, DisplayError> {
    loop {
        match s.read(buf) {
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                return Err(DisplayError::Connect("the server did not answer the handshake".into()));
            }
            r => return r.map_err(Into::into),
        }
    }
}
