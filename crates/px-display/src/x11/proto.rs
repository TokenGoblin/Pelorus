//! X11 core protocol: request encoders and the message decoder. Pure functions
//! over byte slices, no I/O, so the decoder can be fed a recorded socket or a
//! fuzzer. Byte order is little-endian (the client announces `l`).
//!
//! Every read is bounds-checked and returns `None`/`Err` on short input; nothing
//! here indexes, unwraps or panics (the crate carries the workspace panic lints).

pub const OP_CREATE_WINDOW: u8 = 1;
pub const OP_CHANGE_WINDOW_ATTRIBUTES: u8 = 2;
pub const OP_DESTROY_WINDOW: u8 = 4;
pub const OP_MAP_WINDOW: u8 = 8;
pub const OP_CONFIGURE_WINDOW: u8 = 12;
pub const OP_INTERN_ATOM: u8 = 16;
pub const OP_CHANGE_PROPERTY: u8 = 18;
pub const OP_GET_PROPERTY: u8 = 20;
pub const OP_SET_SELECTION_OWNER: u8 = 22;
pub const OP_CONVERT_SELECTION: u8 = 24;
pub const OP_SEND_EVENT: u8 = 25;
pub const OP_OPEN_FONT: u8 = 45;
pub const OP_CREATE_GC: u8 = 55;
pub const OP_PUT_IMAGE: u8 = 72;
pub const OP_QUERY_EXTENSION: u8 = 98;
pub const OP_GET_KEYBOARD_MAPPING: u8 = 101;
pub const OP_CREATE_GLYPH_CURSOR: u8 = 94;

// Event codes.
pub const EV_KEY_PRESS: u8 = 2;
pub const EV_BUTTON_PRESS: u8 = 4;
pub const EV_BUTTON_RELEASE: u8 = 5;
pub const EV_MOTION: u8 = 6;
pub const EV_LEAVE: u8 = 8;
pub const EV_FOCUS_IN: u8 = 9;
pub const EV_FOCUS_OUT: u8 = 10;
pub const EV_EXPOSE: u8 = 12;
pub const EV_CONFIGURE: u8 = 22;
pub const EV_SELECTION_CLEAR: u8 = 29;
pub const EV_SELECTION_REQUEST: u8 = 30;
pub const EV_SELECTION_NOTIFY: u8 = 31;
pub const EV_CLIENT_MESSAGE: u8 = 33;
pub const EV_MAPPING_NOTIFY: u8 = 34;

// Event masks.
pub const MASK_KEY_PRESS: u32 = 0x1;
pub const MASK_BUTTON_PRESS: u32 = 0x4;
pub const MASK_BUTTON_RELEASE: u32 = 0x8;
pub const MASK_LEAVE: u32 = 0x20;
pub const MASK_POINTER_MOTION: u32 = 0x40;
pub const MASK_EXPOSURE: u32 = 0x8000;
pub const MASK_STRUCTURE_NOTIFY: u32 = 0x2_0000;
pub const MASK_FOCUS_CHANGE: u32 = 0x20_0000;
pub const MASK_PROPERTY_CHANGE: u32 = 0x40_0000;
/// SubstructureNotify | SubstructureRedirect: the mask root-directed client
/// messages to the window manager carry.
pub const MASK_ROOT_WM: u32 = 0x10_0000 | 0x8_0000;

/// Largest reply or event body the decoder will buffer, in bytes. A hostile
/// server (or a corrupt stream) cannot make the client allocate more.
pub const MAX_MESSAGE: usize = 16 << 20;

pub const ATOM_ATOM: u32 = 4;
pub const ATOM_CARDINAL: u32 = 6;
pub const ATOM_STRING: u32 = 31;
pub const ATOM_WM_CLASS: u32 = 67;
pub const ATOM_WM_NAME: u32 = 39;

/// A bounds-checked cursor over received bytes.
#[derive(Clone, Copy)]
pub struct Reader<'a> {
    b: &'a [u8],
}

impl<'a> Reader<'a> {
    pub fn new(b: &'a [u8]) -> Reader<'a> {
        Reader { b }
    }
    pub fn u8_at(&self, i: usize) -> Option<u8> {
        self.b.get(i).copied()
    }
    pub fn u16_at(&self, i: usize) -> Option<u16> {
        let s = self.b.get(i..i.checked_add(2)?)?;
        Some(u16::from_le_bytes([*s.first()?, *s.get(1)?]))
    }
    pub fn i16_at(&self, i: usize) -> Option<i16> {
        self.u16_at(i).map(|v| v as i16)
    }
    pub fn u32_at(&self, i: usize) -> Option<u32> {
        let s = self.b.get(i..i.checked_add(4)?)?;
        Some(u32::from_le_bytes([
            *s.first()?,
            *s.get(1)?,
            *s.get(2)?,
            *s.get(3)?,
        ]))
    }
    pub fn slice(&self, i: usize, n: usize) -> Option<&'a [u8]> {
        self.b.get(i..i.checked_add(n)?)
    }
    pub fn len(&self) -> usize {
        self.b.len()
    }
    pub fn is_empty(&self) -> bool {
        self.b.is_empty()
    }
}

pub fn pad4(n: usize) -> usize {
    (4 - n % 4) % 4
}

// -- encoding ------------------------------------------------------------------

/// Builds one request. `data` is the byte after the opcode. The length field is
/// filled in by [`Req::finish`]; with `big` set, a request over 65,535 words uses
/// the BIG-REQUESTS extended header (length field 0, then a 32-bit length).
pub struct Req {
    opcode: u8,
    data: u8,
    body: Vec<u8>,
}

impl Req {
    pub fn new(opcode: u8, data: u8) -> Req {
        Req {
            opcode,
            data,
            body: Vec::new(),
        }
    }
    pub fn u8(mut self, v: u8) -> Req {
        self.body.push(v);
        self
    }
    pub fn u16(mut self, v: u16) -> Req {
        self.body.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn i16(self, v: i16) -> Req {
        self.u16(v as u16)
    }
    pub fn u32(mut self, v: u32) -> Req {
        self.body.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn zeros(mut self, n: usize) -> Req {
        self.body.resize(self.body.len() + n, 0);
        self
    }
    /// Bytes, padded to a multiple of four.
    pub fn padded(mut self, bytes: &[u8]) -> Req {
        self.body.extend_from_slice(bytes);
        self.body.resize(self.body.len() + pad4(bytes.len()), 0);
        self
    }
    /// Total length in bytes once finished with the normal header.
    pub fn wire_len(&self) -> usize {
        4 + self.body.len() + pad4(self.body.len())
    }
    pub fn finish(mut self, big: bool) -> Result<Vec<u8>, String> {
        self.body.resize(self.body.len() + pad4(self.body.len()), 0);
        let words = (4 + self.body.len()) / 4;
        let mut out = Vec::with_capacity(self.body.len() + 8);
        out.push(self.opcode);
        out.push(self.data);
        if let Ok(w) = u16::try_from(words)
            && w != 0
        {
            out.extend_from_slice(&w.to_le_bytes());
        } else if big {
            out.extend_from_slice(&0u16.to_le_bytes());
            let w = u32::try_from(words + 1).map_err(|_| "request too large".to_string())?;
            out.extend_from_slice(&w.to_le_bytes());
        } else {
            return Err(format!("request of {words} words needs BIG-REQUESTS"));
        }
        out.extend_from_slice(&self.body);
        Ok(out)
    }
}

/// The connection setup request: byte order, protocol 11.0, authorisation.
pub fn setup_request(auth_name: &[u8], auth_data: &[u8]) -> Vec<u8> {
    let mut v = vec![b'l', 0];
    v.extend_from_slice(&11u16.to_le_bytes());
    v.extend_from_slice(&0u16.to_le_bytes());
    v.extend_from_slice(&(auth_name.len() as u16).to_le_bytes());
    v.extend_from_slice(&(auth_data.len() as u16).to_le_bytes());
    v.extend_from_slice(&[0, 0]);
    v.extend_from_slice(auth_name);
    v.resize(v.len() + pad4(auth_name.len()), 0);
    v.extend_from_slice(auth_data);
    v.resize(v.len() + pad4(auth_data.len()), 0);
    v
}

// -- setup reply -------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Setup {
    pub resource_id_base: u32,
    pub resource_id_mask: u32,
    /// Maximum request length in 4-byte units (without BIG-REQUESTS).
    pub max_request_words: u16,
    pub image_byte_order_lsb: bool,
    pub min_keycode: u8,
    pub max_keycode: u8,
    /// `(depth, bits_per_pixel, scanline_pad)` for each pixmap format.
    pub formats: Vec<(u8, u8, u8)>,
    pub root: u32,
    pub root_visual: u32,
    pub root_depth: u8,
    pub width_px: u16,
    pub height_px: u16,
}

pub enum SetupReply {
    /// The server needs more bytes (the number needed in total, if known).
    Incomplete,
    Failed(String),
    Ok(Box<Setup>, usize),
}

pub fn parse_setup(b: &[u8]) -> Result<SetupReply, String> {
    let r = Reader::new(b);
    let (Some(status), Some(extra)) = (r.u8_at(0), r.u16_at(6)) else {
        return Ok(SetupReply::Incomplete);
    };
    let total = 8usize + usize::from(extra) * 4;
    if b.len() < total {
        return Ok(SetupReply::Incomplete);
    }
    match status {
        0 => {
            let n = usize::from(r.u8_at(1).unwrap_or(0));
            let reason = r.slice(8, n).unwrap_or(&[]);
            Ok(SetupReply::Failed(
                String::from_utf8_lossy(reason).into_owned(),
            ))
        }
        1 => {
            let bad = || "truncated setup".to_string();
            let vendor_len = usize::from(r.u16_at(24).ok_or_else(bad)?);
            let n_screens = r.u8_at(28).ok_or_else(bad)?;
            let n_formats = usize::from(r.u8_at(29).ok_or_else(bad)?);
            let mut setup = Setup {
                resource_id_base: r.u32_at(12).ok_or_else(bad)?,
                resource_id_mask: r.u32_at(16).ok_or_else(bad)?,
                max_request_words: r.u16_at(26).ok_or_else(bad)?,
                image_byte_order_lsb: r.u8_at(30).ok_or_else(bad)? == 0,
                min_keycode: r.u8_at(34).ok_or_else(bad)?,
                max_keycode: r.u8_at(35).ok_or_else(bad)?,
                formats: Vec::new(),
                root: 0,
                root_visual: 0,
                root_depth: 0,
                width_px: 0,
                height_px: 0,
            };
            if n_screens == 0 {
                return Err("server has no screen".into());
            }
            let mut p = 40 + vendor_len + pad4(vendor_len);
            for _ in 0..n_formats {
                setup.formats.push((
                    r.u8_at(p).ok_or_else(bad)?,
                    r.u8_at(p + 1).ok_or_else(bad)?,
                    r.u8_at(p + 2).ok_or_else(bad)?,
                ));
                p += 8;
            }
            // First screen only.
            setup.root = r.u32_at(p).ok_or_else(bad)?;
            setup.width_px = r.u16_at(p + 20).ok_or_else(bad)?;
            setup.height_px = r.u16_at(p + 22).ok_or_else(bad)?;
            setup.root_visual = r.u32_at(p + 32).ok_or_else(bad)?;
            setup.root_depth = r.u8_at(p + 38).ok_or_else(bad)?;
            Ok(SetupReply::Ok(Box::new(setup), total))
        }
        2 => Err("server requires further authentication (not supported)".into()),
        s => Err(format!("unknown setup status {s}")),
    }
}

// -- messages ------------------------------------------------------------------------

/// A decoded server-to-client message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Message {
    Error {
        code: u8,
        seq: u16,
        bad_value: u32,
        major: u8,
    },
    Reply {
        seq: u16,
        /// Byte 1 of the reply header.
        detail: u8,
        /// The 24 bytes after the length field, then any extra data.
        body: Vec<u8>,
    },
    /// The 32-byte event, high "sent" bit cleared in `code`.
    Event {
        code: u8,
        sent: bool,
        raw: [u8; 32],
    },
}

/// Decode one message from the front of `buf`. `Ok(None)` means more bytes are
/// needed. `Err` is a protocol violation (the connection must be dropped).
pub fn parse_message(buf: &[u8]) -> Result<Option<(Message, usize)>, String> {
    let r = Reader::new(buf);
    let Some(kind) = r.u8_at(0) else {
        return Ok(None);
    };
    if buf.len() < 32 {
        return Ok(None);
    }
    match kind {
        0 => {
            let m = Message::Error {
                code: r.u8_at(1).unwrap_or(0),
                seq: r.u16_at(2).unwrap_or(0),
                bad_value: r.u32_at(4).unwrap_or(0),
                major: r.u8_at(10).unwrap_or(0),
            };
            Ok(Some((m, 32)))
        }
        1 => {
            let extra = r.u32_at(4).unwrap_or(0) as usize;
            let total = 32usize
                .checked_add(extra.checked_mul(4).ok_or("reply length overflow")?)
                .ok_or("reply length overflow")?;
            if total > MAX_MESSAGE {
                return Err(format!("reply of {total} bytes exceeds the limit"));
            }
            if buf.len() < total {
                return Ok(None);
            }
            let body = r.slice(8, total - 8).unwrap_or(&[]).to_vec();
            let m = Message::Reply {
                seq: r.u16_at(2).unwrap_or(0),
                detail: r.u8_at(1).unwrap_or(0),
                body,
            };
            Ok(Some((m, total)))
        }
        code => {
            let mut raw = [0u8; 32];
            for (d, s) in raw.iter_mut().zip(buf.iter()) {
                *d = *s;
            }
            let mut size = 32;
            // GenericEvent (35) carries extra words.
            if code & 0x7f == 35 {
                let extra = r.u32_at(4).unwrap_or(0) as usize;
                size = 32usize
                    .checked_add(extra.checked_mul(4).ok_or("event length overflow")?)
                    .ok_or("event length overflow")?;
                if size > MAX_MESSAGE {
                    return Err("generic event exceeds the limit".into());
                }
                if buf.len() < size {
                    return Ok(None);
                }
            }
            Ok(Some((
                Message::Event {
                    code: code & 0x7f,
                    sent: code & 0x80 != 0,
                    raw,
                },
                size,
            )))
        }
    }
}

// -- events ----------------------------------------------------------------------------

/// The pointer/key event layout shared by KeyPress, ButtonPress, Motion, Leave.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InputEv {
    pub detail: u8,
    pub time: u32,
    pub x: i16,
    pub y: i16,
    pub state: u16,
    /// Enter/Leave mode (byte 30); 0 is Normal.
    pub mode: u8,
}

pub fn input_event(raw: &[u8; 32]) -> InputEv {
    let r = Reader::new(raw);
    InputEv {
        detail: r.u8_at(1).unwrap_or(0),
        time: r.u32_at(4).unwrap_or(0),
        x: r.i16_at(24).unwrap_or(0),
        y: r.i16_at(26).unwrap_or(0),
        state: r.u16_at(28).unwrap_or(0),
        mode: r.u8_at(30).unwrap_or(0),
    }
}

/// A 20-byte ClientMessage payload as five words.
pub fn client_message_words(raw: &[u8; 32]) -> (u32, [u32; 5]) {
    let r = Reader::new(raw);
    let mut w = [0u32; 5];
    for (i, slot) in w.iter_mut().enumerate() {
        *slot = r.u32_at(12 + 4 * i).unwrap_or(0);
    }
    (r.u32_at(8).unwrap_or(0), w)
}

// -- request builders ----------------------------------------------------------------------

pub fn create_window(
    wid: u32,
    parent: u32,
    depth: u8,
    visual: u32,
    (w, h): (u16, u16),
    event_mask: u32,
) -> Req {
    // value-mask: BackingStore? no. BackPixel (0x2) and EventMask (0x800).
    Req::new(OP_CREATE_WINDOW, depth)
        .u32(wid)
        .u32(parent)
        .i16(0)
        .i16(0)
        .u16(w)
        .u16(h)
        .u16(0)
        .u16(1) // InputOutput
        .u32(visual)
        .u32(0x2 | 0x800)
        .u32(0) // BackPixel: black
        .u32(event_mask)
}

pub fn map_window(wid: u32) -> Req {
    Req::new(OP_MAP_WINDOW, 0).u32(wid)
}

pub fn destroy_window(wid: u32) -> Req {
    Req::new(OP_DESTROY_WINDOW, 0).u32(wid)
}

pub fn intern_atom(name: &str) -> Req {
    Req::new(OP_INTERN_ATOM, 0)
        .u16(name.len() as u16)
        .zeros(2)
        .padded(name.as_bytes())
}

/// `format` is 8, 16 or 32; `units` is the number of format-sized items.
pub fn change_property(wid: u32, prop: u32, ty: u32, format: u8, units: u32, data: &[u8]) -> Req {
    Req::new(OP_CHANGE_PROPERTY, 0)
        .u32(wid)
        .u32(prop)
        .u32(ty)
        .u8(format)
        .zeros(3)
        .u32(units)
        .padded(data)
}

pub fn words_le(words: &[u32]) -> Vec<u8> {
    words.iter().flat_map(|w| w.to_le_bytes()).collect()
}

pub fn get_property(wid: u32, prop: u32, delete: bool, long_len: u32) -> Req {
    Req::new(OP_GET_PROPERTY, u8::from(delete))
        .u32(wid)
        .u32(prop)
        .u32(0) // AnyPropertyType
        .u32(0)
        .u32(long_len)
}

pub fn create_gc(gc: u32, drawable: u32) -> Req {
    Req::new(OP_CREATE_GC, 0).u32(gc).u32(drawable).u32(0)
}

/// ZPixmap `PutImage`. `data` is `w*h*4` bytes of B,G,R,X.
pub fn put_image(drawable: u32, gc: u32, (w, h): (u16, u16), (x, y): (i16, i16), depth: u8, data: &[u8]) -> Req {
    Req::new(OP_PUT_IMAGE, 2)
        .u32(drawable)
        .u32(gc)
        .u16(w)
        .u16(h)
        .i16(x)
        .i16(y)
        .u8(0)
        .u8(depth)
        .zeros(2)
        .padded(data)
}

pub fn get_keyboard_mapping(first: u8, count: u8) -> Req {
    Req::new(OP_GET_KEYBOARD_MAPPING, 0).u8(first).u8(count).zeros(2)
}

pub fn set_selection_owner(owner: u32, selection: u32) -> Req {
    Req::new(OP_SET_SELECTION_OWNER, 0)
        .u32(owner)
        .u32(selection)
        .u32(0) // CurrentTime
}

pub fn convert_selection(requestor: u32, selection: u32, target: u32, prop: u32) -> Req {
    Req::new(OP_CONVERT_SELECTION, 0)
        .u32(requestor)
        .u32(selection)
        .u32(target)
        .u32(prop)
        .u32(0)
}

/// SendEvent with a 32-byte event body.
pub fn send_event(dest: u32, mask: u32, event: &[u8; 32]) -> Req {
    let mut r = Req::new(OP_SEND_EVENT, 0).u32(dest).u32(mask);
    r.body.extend_from_slice(event);
    r
}

/// A SelectionNotify event (code 31) to send to a requestor.
pub fn selection_notify_event(
    time: u32,
    requestor: u32,
    selection: u32,
    target: u32,
    prop: u32,
) -> [u8; 32] {
    let mut e = [0u8; 32];
    let put = |e: &mut [u8; 32], at: usize, v: u32| {
        for (d, s) in e.iter_mut().skip(at).zip(v.to_le_bytes()) {
            *d = s;
        }
    };
    if let Some(b) = e.first_mut() {
        *b = EV_SELECTION_NOTIFY;
    }
    put(&mut e, 4, time);
    put(&mut e, 8, requestor);
    put(&mut e, 12, selection);
    put(&mut e, 16, target);
    put(&mut e, 20, prop);
    e
}

/// A format-32 ClientMessage event.
pub fn client_message_event(window: u32, ty: u32, data: [u32; 5]) -> [u8; 32] {
    let mut e = [0u8; 32];
    for (d, s) in e.iter_mut().zip([EV_CLIENT_MESSAGE, 32]) {
        *d = s;
    }
    let mut at = 4;
    for v in [window, ty, data[0], data[1], data[2], data[3], data[4]] {
        for (d, s) in e.iter_mut().skip(at).zip(v.to_le_bytes()) {
            *d = s;
        }
        at += 4;
    }
    e
}

pub fn open_font(fid: u32, name: &str) -> Req {
    Req::new(OP_OPEN_FONT, 0)
        .u32(fid)
        .u16(name.len() as u16)
        .zeros(2)
        .padded(name.as_bytes())
}

pub fn create_glyph_cursor(cid: u32, font: u32, glyph: u16) -> Req {
    Req::new(OP_CREATE_GLYPH_CURSOR, 0)
        .u32(cid)
        .u32(font)
        .u32(font)
        .u16(glyph)
        .u16(glyph + 1)
        .u16(0)
        .u16(0)
        .u16(0)
        .u16(0xffff)
        .u16(0xffff)
        .u16(0xffff)
}

pub fn set_window_cursor(wid: u32, cursor: u32) -> Req {
    Req::new(OP_CHANGE_WINDOW_ATTRIBUTES, 0)
        .u32(wid)
        .u32(0x4000)
        .u32(cursor)
}

pub fn query_extension(name: &str) -> Req {
    Req::new(OP_QUERY_EXTENSION, 0)
        .u16(name.len() as u16)
        .zeros(2)
        .padded(name.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_have_word_lengths() {
        let b = intern_atom("WM_PROTOCOLS").finish(false).unwrap();
        assert_eq!(b.len() % 4, 0);
        assert_eq!(u16::from_le_bytes([b[2], b[3]]) as usize * 4, b.len());
        let b = put_image(1, 2, (3, 1), (0, 0), 24, &[0; 12]).finish(false).unwrap();
        assert_eq!(u16::from_le_bytes([b[2], b[3]]) as usize * 4, b.len());
        assert_eq!(b[0], OP_PUT_IMAGE);
        assert_eq!(b[1], 2);
    }

    #[test]
    fn big_requests_use_the_extended_header() {
        let data = vec![0u8; 300_000];
        assert!(change_property(1, 2, 3, 8, 300_000, &data).finish(false).is_err());
        let b = change_property(1, 2, 3, 8, 300_000, &data).finish(true).unwrap();
        assert_eq!(&b[2..4], &[0, 0]);
        let words = u32::from_le_bytes([b[4], b[5], b[6], b[7]]) as usize;
        assert_eq!(words * 4, b.len());
    }

    #[test]
    fn messages_split_and_wait_for_more() {
        let mut ev = [0u8; 32];
        ev[0] = 0x80 | EV_KEY_PRESS;
        ev[1] = 38;
        assert_eq!(parse_message(&ev[..31]), Ok(None));
        let (m, n) = parse_message(&ev).unwrap().unwrap();
        assert_eq!(n, 32);
        assert!(matches!(m, Message::Event { code: 2, sent: true, .. }));

        let mut rep = vec![0u8; 36];
        rep[0] = 1;
        rep[2] = 7;
        rep[4] = 1; // one extra word
        assert_eq!(parse_message(&rep[..35]), Ok(None));
        let (m, n) = parse_message(&rep).unwrap().unwrap();
        assert_eq!(n, 36);
        assert!(matches!(m, Message::Reply { seq: 7, .. }));
    }

    #[test]
    fn oversized_reply_is_a_protocol_error() {
        let mut rep = vec![0u8; 32];
        rep[0] = 1;
        rep[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(parse_message(&rep).is_err());
    }

    #[test]
    fn decoder_survives_arbitrary_bytes() {
        let mut s = 0x1234_5678u32;
        for _ in 0..2000 {
            let mut v = Vec::new();
            for _ in 0..(s % 90) {
                s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                v.push((s >> 24) as u8);
            }
            let _ = parse_message(&v);
            let _ = parse_setup(&v);
        }
    }

    #[test]
    fn setup_failure_carries_the_reason() {
        let mut b = vec![0u8; 8];
        b[1] = 4;
        b[6] = 1; // 1 extra word
        b.extend_from_slice(b"nope");
        match parse_setup(&b).unwrap() {
            SetupReply::Failed(m) => assert_eq!(m, "nope"),
            _ => panic!("expected failure"),
        }
        assert!(matches!(parse_setup(&b[..9]).unwrap(), SetupReply::Incomplete));
    }
}
