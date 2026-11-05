use std::collections::VecDeque;
use std::os::unix::net::UnixStream;
use std::time::Duration;

use px_paint::raster::Canvas;
use px_ui::chrome::PlatformConfig;
use px_ui::geom::{Rect, Size};
use px_ui::input::{Button, CursorShape, Event, KeyEvent, PointerEvent, PointerKind, WindowAction};
use px_ui::platform::{PlatformError, PlatformWindow};

use super::conn::{Conn, RawEvent, Wait};
use super::discover::Auth;
use super::keys::{self, KeyMap};
use super::proto::{self, Reader};
use crate::{Display, DisplayError, DisplayEvent, Mask, WindowKind, WindowOptions, WindowShape};

/// Largest clipboard payload accepted from another client, in bytes. A larger
/// one (or an INCR transfer) is dropped rather than truncated.
const MAX_CLIPBOARD: u32 = 1 << 20;

const ATOM_NAMES: [&str; 21] = [
    "WM_PROTOCOLS",
    "WM_DELETE_WINDOW",
    "_NET_WM_NAME",
    "UTF8_STRING",
    "CLIPBOARD",
    "TARGETS",
    "_MOTIF_WM_HINTS",
    "WM_CHANGE_STATE",
    "_NET_WM_STATE",
    "_NET_WM_STATE_MAXIMIZED_VERT",
    "_NET_WM_STATE_MAXIMIZED_HORZ",
    "PX_SELECTION",
    "INCR",
    "TEXT",
    // Compositing manager selection of screen 0 (EWMH): owned = a compositor runs.
    "_NET_WM_CM_S0",
    "_NET_WM_WINDOW_TYPE",
    "_NET_WM_WINDOW_TYPE_DIALOG",
    "_NET_WM_WINDOW_TYPE_POPUP_MENU",
    "_NET_WM_WINDOW_TYPE_DROPDOWN_MENU",
    "_NET_WM_WINDOW_TYPE_TOOLTIP",
    "_NET_WM_WINDOW_TYPE_NORMAL",
];

/// How the window outline is realised on this display. Chosen once, at creation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShapeMode {
    /// No outline requested (or no way to realise one): a plain rectangle.
    Rect,
    /// A compositor is running: a 32-bit ARGB visual with premultiplied
    /// per-pixel alpha. Anti-aliased.
    Argb,
    /// No compositor: the core-protocol SHAPE extension clips the window to the
    /// squircle thresholded at half coverage. Aliased edges (the documented
    /// fallback).
    ShapeExt,
    /// No compositor and no SHAPE: the window stays a rectangle; input in the
    /// transparent corners is still discarded in software.
    Software,
}

impl ShapeMode {
    /// Only `Argb` has anti-aliased edges; `ShapeExt` is aliased and `Software`
    /// and `Rect` are rectangles (docs/backlog.md tracks closing those gaps).
    pub fn is_antialiased(self) -> bool {
        self == ShapeMode::Argb
    }
}

#[derive(Clone, Copy)]
struct Atoms {
    wm_protocols: u32,
    delete: u32,
    net_wm_name: u32,
    utf8: u32,
    clipboard: u32,
    targets: u32,
    motif: u32,
    change_state: u32,
    net_wm_state: u32,
    max_vert: u32,
    max_horz: u32,
    prop: u32,
    incr: u32,
    text: u32,
    cm_s0: u32,
    wtype: u32,
    wt_dialog: u32,
    wt_popup_menu: u32,
    wt_dropdown: u32,
    wt_tooltip: u32,
    wt_normal: u32,
}

pub struct X11Window {
    conn: Conn,
    wid: u32,
    gc: u32,
    depth: u8,
    kind: WindowKind,
    mode: ShapeMode,
    shape: Option<WindowShape>,
    /// Outline coverage at the current size.
    mask: Mask,
    /// Major opcode of the SHAPE extension, when the server has it.
    shape_major: Option<u8>,
    /// The last pointer position was inside the outline.
    pointer_inside: bool,
    size: Size,
    atoms: Atoms,
    keymap: KeyMap,
    /// Default, text, pointer.
    cursors: [u32; 3],
    out: VecDeque<DisplayEvent>,
    clip: Option<String>,
    tried_string: bool,
    failed: Option<DisplayError>,
}

impl X11Window {
    /// Open a window on a connected stream. The stream and the cookie come from
    /// the broker (or [`super::discover::open`] in tools and the demo).
    pub fn connect(
        stream: UnixStream,
        auth: &Auth,
        opts: &WindowOptions,
    ) -> Result<X11Window, DisplayError> {
        let mut conn = Conn::connect(stream, auth)?;
        let s = conn.setup.clone();
        let fmt_ok = s.formats.iter().any(|&(d, bpp, _)| d == 24 && bpp == 32);
        if !s.image_byte_order_lsb || s.root_depth != 24 || !fmt_ok {
            return Err(DisplayError::Connect(format!(
                "unsupported display: need depth 24, 32 bits per pixel, LSB first \
                 (root depth {}, lsb {})",
                s.root_depth, s.image_byte_order_lsb
            )));
        }
        let (w, h) = match (u16::try_from(opts.width), u16::try_from(opts.height)) {
            (Ok(w), Ok(h)) if w > 0 && h > 0 => (w, h),
            _ => return Err(DisplayError::Connect("window size out of range".into())),
        };

        let atoms = intern_all(&mut conn)?;
        let keymap = fetch_keymap(&mut conn)?;
        enable_big_requests(&mut conn);

        let wid = conn.alloc_id()?;
        let gc = conn.alloc_id()?;
        let mask = proto::MASK_KEY_PRESS
            | proto::MASK_BUTTON_PRESS
            | proto::MASK_BUTTON_RELEASE
            | proto::MASK_POINTER_MOTION
            | proto::MASK_LEAVE
            | proto::MASK_EXPOSURE
            | proto::MASK_STRUCTURE_NOTIFY
            | proto::MASK_FOCUS_CHANGE
            | proto::MASK_PROPERTY_CHANGE;

        // Pick how the outline is realised (see `ShapeMode`).
        let shape_major = if opts.shape.is_some() {
            query_major(&mut conn, "SHAPE")
        } else {
            None
        };
        let compositor = opts.shape.is_some() && compositor_running(&mut conn, atoms.cm_s0);
        let argb = if compositor { s.argb_visual() } else { None };
        let mode = match (opts.shape, argb, shape_major) {
            (None, ..) => ShapeMode::Rect,
            (Some(_), Some(_), _) => ShapeMode::Argb,
            (Some(_), None, Some(_)) => ShapeMode::ShapeExt,
            (Some(_), None, None) => ShapeMode::Software,
        };
        // A rectangular window is never chosen silently: say so once.
        if mode == ShapeMode::Software {
            eprintln!(
                "px-display: no compositor and no SHAPE extension: this window stays a \
                 rectangle (corner input is still discarded)"
            );
        }
        let (depth, visual, colormap) = match argb {
            Some(v) if mode == ShapeMode::Argb => {
                let cmap = conn.alloc_id()?;
                conn.send(proto::create_colormap(cmap, s.root, v.id))?;
                (32u8, v.id, Some(cmap))
            }
            _ => (24u8, s.root_visual, None),
        };
        let (px, py) = opts.position.map_or((0, 0), |(x, y)| {
            (i16::try_from(x).unwrap_or(0), i16::try_from(y).unwrap_or(0))
        });
        conn.send(proto::create_window(&proto::NewWindow {
            wid,
            parent: s.root,
            depth,
            visual,
            pos: (px, py),
            size: (w, h),
            event_mask: mask,
            override_redirect: opts.kind.is_transient(),
            colormap,
        }))?;
        conn.send(proto::create_gc(gc, wid))?;

        // Title, class, close protocol.
        let ascii: String = opts
            .title
            .chars()
            .map(|c| if c.is_ascii() { c } else { '?' })
            .collect();
        conn.send(proto::change_property(
            wid,
            proto::ATOM_WM_NAME,
            proto::ATOM_STRING,
            8,
            ascii.len() as u32,
            ascii.as_bytes(),
        ))?;
        conn.send(proto::change_property(
            wid,
            atoms.net_wm_name,
            atoms.utf8,
            8,
            opts.title.len() as u32,
            opts.title.as_bytes(),
        ))?;
        conn.send(proto::change_property(
            wid,
            proto::ATOM_WM_CLASS,
            proto::ATOM_STRING,
            8,
            6,
            b"px\0Px\0",
        ))?;
        conn.send(proto::change_property(
            wid,
            atoms.wm_protocols,
            proto::ATOM_ATOM,
            32,
            1,
            &proto::words_le(&[atoms.delete]),
        ))?;
        // EWMH window type, for managed windows and compositors alike.
        let wtype = match opts.kind {
            WindowKind::Main => atoms.wt_normal,
            WindowKind::Dialog => atoms.wt_dialog,
            WindowKind::Popup => atoms.wt_dropdown,
            WindowKind::Menu => atoms.wt_popup_menu,
            WindowKind::Tooltip => atoms.wt_tooltip,
        };
        if wtype != 0 {
            conn.send(proto::change_property(
                wid,
                atoms.wtype,
                proto::ATOM_ATOM,
                32,
                1,
                &proto::words_le(&[wtype]),
            ))?;
        }
        // A shaped window draws its own outline: the window manager's frame
        // would be a rectangle around a squircle.
        if opts.frameless || opts.shape.is_some() {
            // _MOTIF_WM_HINTS: flags = decorations, decorations = none.
            conn.send(proto::change_property(
                wid,
                atoms.motif,
                atoms.motif,
                32,
                5,
                &proto::words_le(&[2, 0, 0, 0, 0]),
            ))?;
        }

        // Font cursors: left_ptr, xterm, hand2.
        let font = conn.alloc_id()?;
        conn.send(proto::open_font(font, "cursor"))?;
        let mut cursors = [0u32; 3];
        for (slot, glyph) in cursors.iter_mut().zip([68u16, 152, 60]) {
            let id = conn.alloc_id()?;
            conn.send(proto::create_glyph_cursor(id, font, glyph))?;
            *slot = id;
        }
        let mask = match opts.shape {
            Some(sh) => sh.mask(u32::from(w), u32::from(h)),
            None => Mask::full(u32::from(w), u32::from(h)),
        };
        apply_shape(&mut conn, wid, mode, shape_major, opts.kind, &mask)?;
        conn.send(proto::map_window(wid))?;
        conn.sync()?;

        Ok(X11Window {
            conn,
            wid,
            gc,
            depth,
            kind: opts.kind,
            mode,
            shape: opts.shape,
            mask,
            shape_major,
            pointer_inside: true,
            size: Size::new(i32::from(w), i32::from(h)),
            atoms,
            keymap,
            cursors,
            out: VecDeque::new(),
            clip: None,
            tried_string: false,
            failed: None,
        })
    }

    /// X errors the server reported for requests nobody waited on. Draining
    /// them is how a test (or a log line) learns a `PutImage` was rejected.
    pub fn take_errors(&mut self) -> Vec<super::conn::XError> {
        std::mem::take(&mut self.conn.errors)
    }

    /// Wait until the server has processed everything sent so far.
    pub fn sync(&mut self) -> Result<(), DisplayError> {
        self.conn.sync()
    }

    /// Read back window pixels as B,G,R,X rows (tests, screenshots).
    pub fn get_image(&mut self, rect: Rect) -> Result<Vec<u8>, DisplayError> {
        self.get_drawable_image(self.wid, rect)
    }

    /// Read back root-window pixels (B,G,R,X rows): a screenshot of what is on
    /// screen, composited if a compositor runs (tests, docs/brand screenshots).
    pub fn get_root_image(&mut self, rect: Rect) -> Result<Vec<u8>, DisplayError> {
        let root = self.conn.setup.root;
        self.get_drawable_image(root, rect)
    }

    fn get_drawable_image(&mut self, drawable: u32, rect: Rect) -> Result<Vec<u8>, DisplayError> {
        let (Ok(x), Ok(y), Ok(w), Ok(h)) = (
            i16::try_from(rect.x),
            i16::try_from(rect.y),
            u16::try_from(rect.w),
            u16::try_from(rect.h),
        ) else {
            return Err(DisplayError::Protocol("rectangle out of range".into()));
        };
        let r = self.conn.round_trip(
            proto::Req::new(73, 2)
                .u32(drawable)
                .i16(x)
                .i16(y)
                .u16(w)
                .u16(h)
                .u32(u32::MAX),
        )?;
        Ok(r.data_from(32).to_vec())
    }

    pub fn window_id(&self) -> u32 {
        self.wid
    }

    fn note(&mut self, r: Result<(), DisplayError>) {
        if let Err(e) = r
            && self.failed.is_none()
        {
            self.failed = Some(e);
        }
    }

    // -- events ----------------------------------------------------------------------

    fn handle(&mut self, ev: RawEvent) -> Result<(), DisplayError> {
        let raw = &ev.raw;
        let r = Reader::new(raw);
        match ev.code {
            proto::EV_KEY_PRESS => {
                let e = proto::input_event(raw);
                if let (Some((l0, _)), Some(sym)) = (
                    self.keymap.lookup(e.detail),
                    self.keymap.keysym(e.detail, e.state),
                ) {
                    if let Some(key) = keys::logical_key(l0, sym) {
                        self.out
                            .push_back(DisplayEvent::Ui(Event::Key(KeyEvent::with(
                                key,
                                keys::modifiers(e.state),
                            ))));
                    }
                    if let Some(t) = keys::text_of(sym, e.state) {
                        self.out.push_back(DisplayEvent::Ui(Event::Text(t)));
                    }
                }
            }
            proto::EV_BUTTON_PRESS | proto::EV_BUTTON_RELEASE => {
                let e = proto::input_event(raw);
                let button = match e.detail {
                    1 => Button::Primary,
                    2 => Button::Middle,
                    3 => Button::Secondary,
                    _ => return Ok(()), // wheel and extra buttons: no chrome event
                };
                let kind = if ev.code == proto::EV_BUTTON_PRESS {
                    PointerKind::Down(button)
                } else {
                    PointerKind::Up(button)
                };
                self.pointer(e.x, e.y, kind, e.state);
            }
            proto::EV_MOTION => {
                let e = proto::input_event(raw);
                self.pointer(e.x, e.y, PointerKind::Move, e.state);
            }
            proto::EV_LEAVE => {
                let e = proto::input_event(raw);
                if e.mode == 0 {
                    self.pointer(e.x, e.y, PointerKind::Leave, e.state);
                }
            }
            proto::EV_FOCUS_IN | proto::EV_FOCUS_OUT => {
                let mode = r.u8_at(8).unwrap_or(0);
                let detail = r.u8_at(1).unwrap_or(0);
                // Normal or WhileGrabbed, and not a pointer-only notification.
                if matches!(mode, 0 | 3) && !matches!(detail, 4..=6) {
                    self.out.push_back(DisplayEvent::Ui(Event::WindowFocus(
                        ev.code == proto::EV_FOCUS_IN,
                    )));
                }
            }
            proto::EV_EXPOSE => {
                let rect = Rect::new(
                    i32::from(r.u16_at(8).unwrap_or(0)),
                    i32::from(r.u16_at(10).unwrap_or(0)),
                    i32::from(r.u16_at(12).unwrap_or(0)),
                    i32::from(r.u16_at(14).unwrap_or(0)),
                );
                self.out.push_back(DisplayEvent::Redraw(rect));
            }
            proto::EV_CONFIGURE => {
                let size = Size::new(
                    i32::from(r.u16_at(20).unwrap_or(0)),
                    i32::from(r.u16_at(22).unwrap_or(0)),
                );
                if size != self.size && size.w > 0 && size.h > 0 {
                    self.size = size;
                    self.reshape()?;
                    self.out.push_back(DisplayEvent::Ui(Event::Resize(size)));
                    self.out
                        .push_back(DisplayEvent::Redraw(Rect::new(0, 0, size.w, size.h)));
                }
            }
            proto::EV_CLIENT_MESSAGE => {
                let (ty, w) = proto::client_message_words(raw);
                if ty == self.atoms.wm_protocols && w.first() == Some(&self.atoms.delete) {
                    self.out.push_back(DisplayEvent::Close);
                }
            }
            proto::EV_MAPPING_NOTIFY => {
                if r.u8_at(4) == Some(1) {
                    self.keymap = fetch_keymap(&mut self.conn)?;
                }
            }
            proto::EV_SELECTION_CLEAR => {
                if r.u32_at(12) == Some(self.atoms.clipboard) {
                    self.clip = None;
                }
            }
            proto::EV_SELECTION_REQUEST => self.serve_selection(raw)?,
            proto::EV_SELECTION_NOTIFY => self.selection_arrived(raw)?,
            _ => {}
        }
        Ok(())
    }

    /// Recompute the outline for the current size and re-apply it.
    fn reshape(&mut self) -> Result<(), DisplayError> {
        let (w, h) = (
            u32::try_from(self.size.w).unwrap_or(0),
            u32::try_from(self.size.h).unwrap_or(0),
        );
        self.mask = match self.shape {
            Some(sh) => sh.mask(w, h),
            None => Mask::full(w, h),
        };
        apply_shape(
            &mut self.conn,
            self.wid,
            self.mode,
            self.shape_major,
            self.kind,
            &self.mask,
        )
    }

    /// How the outline is realised (tests, diagnostics). Callers that care about
    /// anti-aliasing check [`ShapeMode::is_antialiased`].
    pub fn shape_mode(&self) -> ShapeMode {
        self.mode
    }

    /// Is window pixel `(x, y)` inside the outline? Transparent corners are not.
    pub fn hit(&self, x: i32, y: i32) -> bool {
        self.mask.hit(x, y)
    }

    fn pointer(&mut self, x: i16, y: i16, kind: PointerKind, state: u16) {
        // Transparent corners are outside the window. The server already
        // withholds them when the input region is set; this covers the rest
        // (no SHAPE extension). A press there is dropped, a move there becomes
        // one Leave, a release always passes (a drag may end anywhere).
        if self.shape.is_some() && kind != PointerKind::Leave {
            let in_rect =
                x >= 0 && y >= 0 && i32::from(x) < self.size.w && i32::from(y) < self.size.h;
            let hit = self.mask.hit(i32::from(x), i32::from(y));
            if in_rect && !hit {
                match kind {
                    PointerKind::Move if self.pointer_inside => {
                        self.pointer_inside = false;
                        let mut e =
                            PointerEvent::new(i32::from(x), i32::from(y), PointerKind::Leave);
                        e.mods = keys::modifiers(state);
                        self.out.push_back(DisplayEvent::Ui(Event::Pointer(e)));
                        return;
                    }
                    PointerKind::Move | PointerKind::Down(_) => return,
                    _ => {}
                }
            } else if hit {
                self.pointer_inside = true;
            }
        }
        let mut e = PointerEvent::new(i32::from(x), i32::from(y), kind);
        e.mods = keys::modifiers(state);
        self.out.push_back(DisplayEvent::Ui(Event::Pointer(e)));
    }

    // -- clipboard ---------------------------------------------------------------------

    fn serve_selection(&mut self, raw: &[u8; 32]) -> Result<(), DisplayError> {
        let r = Reader::new(raw);
        let time = r.u32_at(4).unwrap_or(0);
        let requestor = r.u32_at(12).unwrap_or(0);
        let selection = r.u32_at(16).unwrap_or(0);
        let target = r.u32_at(20).unwrap_or(0);
        let mut prop = r.u32_at(24).unwrap_or(0);
        if prop == 0 {
            prop = target; // obsolete clients
        }
        let a = self.atoms;
        let mut answer = 0;
        if selection == a.clipboard
            && let Some(text) = self.clip.clone()
        {
            if target == a.targets {
                let list = proto::words_le(&[a.targets, a.utf8, proto::ATOM_STRING, a.text]);
                self.conn.send(proto::change_property(
                    requestor,
                    prop,
                    proto::ATOM_ATOM,
                    32,
                    4,
                    &list,
                ))?;
                answer = prop;
            } else if target == a.utf8 || target == a.text || target == proto::ATOM_STRING {
                let bytes: Vec<u8> = if target == a.utf8 {
                    text.into_bytes()
                } else {
                    text.chars()
                        .map(|c| u8::try_from(u32::from(c)).unwrap_or(b'?'))
                        .collect()
                };
                // The header and padding come out of the request budget.
                if bytes.len() + 64 <= self.conn.max_request_bytes() {
                    let ty = if target == a.utf8 {
                        a.utf8
                    } else {
                        proto::ATOM_STRING
                    };
                    self.conn.send(proto::change_property(
                        requestor,
                        prop,
                        ty,
                        8,
                        bytes.len() as u32,
                        &bytes,
                    ))?;
                    answer = prop;
                }
            }
        }
        let ev = proto::selection_notify_event(time, requestor, selection, target, answer);
        self.conn.send(proto::send_event(requestor, 0, &ev))?;
        Ok(())
    }

    fn selection_arrived(&mut self, raw: &[u8; 32]) -> Result<(), DisplayError> {
        let r = Reader::new(raw);
        let target = r.u32_at(16).unwrap_or(0);
        let prop = r.u32_at(20).unwrap_or(0);
        let a = self.atoms;
        if prop == 0 {
            // Refused. Try the old Latin-1 target once before giving up.
            if target == a.utf8 && !self.tried_string {
                self.tried_string = true;
                self.conn.send(proto::convert_selection(
                    self.wid,
                    a.clipboard,
                    proto::ATOM_STRING,
                    a.prop,
                ))?;
            }
            return Ok(());
        }
        let rep =
            self.conn
                .round_trip(proto::get_property(self.wid, prop, true, MAX_CLIPBOARD / 4))?;
        let ty = rep.u32_at(8).unwrap_or(0);
        let after = rep.u32_at(12).unwrap_or(0);
        let len = rep.u32_at(16).unwrap_or(0) as usize;
        if ty == a.incr || after != 0 || rep.detail != 8 {
            return Ok(()); // INCR, oversize, or not bytes: dropped, not truncated
        }
        let data = rep.data_from(32).get(..len).unwrap_or(&[]);
        let text = if ty == a.utf8 {
            match std::str::from_utf8(data) {
                Ok(s) => s.to_string(),
                Err(_) => return Ok(()),
            }
        } else if ty == proto::ATOM_STRING {
            data.iter().map(|b| char::from(*b)).collect()
        } else {
            return Ok(());
        };
        self.out.push_back(DisplayEvent::Ui(Event::Clipboard(text)));
        Ok(())
    }

    // -- presentation --------------------------------------------------------------------

    fn put_frame(&mut self, frame: &Canvas, damage: Rect) -> Result<(), DisplayError> {
        let fw = i32::try_from(frame.width).unwrap_or(0);
        let fh = i32::try_from(frame.height).unwrap_or(0);
        let x0 = damage.x.max(0);
        let y0 = damage.y.max(0);
        let x1 = damage.right().min(fw);
        let y1 = damage.bottom().min(fh);
        if x1 <= x0 || y1 <= y0 {
            return Ok(());
        }
        let (w, rows) = (
            usize::try_from(x1 - x0).unwrap_or(0),
            usize::try_from(y1 - y0).unwrap_or(0),
        );
        let (Ok(wx), Ok(sx), Ok(_)) = (u16::try_from(w), i16::try_from(x0), i16::try_from(y0))
        else {
            return Err(DisplayError::Protocol("damage out of range".into()));
        };
        let stride = frame.width as usize * 4;
        let row_bytes = w * 4;
        // A request carries 24 bytes of header besides the pixels.
        let budget = self.conn.max_request_bytes().saturating_sub(32);
        let per = (budget / row_bytes.max(1)).clamp(1, rows);
        let mut y = y0 as usize;
        let mut left = rows;
        while left > 0 {
            let n = per.min(left);
            let mut buf = Vec::with_capacity(n * row_bytes);
            for row in 0..n {
                let start = (y + row) * stride + x0 as usize * 4;
                let src = frame.pixels.get(start..start + row_bytes).unwrap_or(&[]);
                if self.mode == ShapeMode::Argb {
                    // RGBA -> premultiplied B,G,R,A under the outline.
                    self.mask.premultiply_row(
                        (y + row) as i32,
                        x0,
                        src.as_chunks::<4>().0,
                        &mut buf,
                    );
                } else {
                    for &[r, g, b, _] in src.as_chunks::<4>().0 {
                        // RGBA -> B,G,R,X
                        buf.extend_from_slice(&[b, g, r, 0]);
                    }
                }
            }
            if buf.len() != n * row_bytes {
                return Err(DisplayError::Protocol(
                    "frame buffer is shorter than its size".into(),
                ));
            }
            let (Ok(hn), Ok(dy)) = (u16::try_from(n), i16::try_from(y)) else {
                return Err(DisplayError::Protocol("damage out of range".into()));
            };
            self.conn.send(proto::put_image(
                self.wid,
                self.gc,
                (wx, hn),
                (sx, dy),
                self.depth,
                &buf,
            ))?;
            y += n;
            left -= n;
        }
        Ok(())
    }

    fn root_client_message(&mut self, ty: u32, data: [u32; 5]) -> Result<(), DisplayError> {
        let ev = proto::client_message_event(self.wid, ty, data);
        let root = self.conn.setup.root;
        self.conn
            .send(proto::send_event(root, proto::MASK_ROOT_WM, &ev))?;
        Ok(())
    }
}

fn intern_all(conn: &mut Conn) -> Result<Atoms, DisplayError> {
    // Pipelined: all requests out, then all replies in order.
    let mut seqs = Vec::new();
    for n in ATOM_NAMES {
        seqs.push(conn.send(proto::intern_atom(n))?);
    }
    let mut ids = Vec::new();
    for s in seqs {
        let r = conn.reply(s)?;
        ids.push(r.u32_at(8).unwrap_or(0));
    }
    let g = |i: usize| ids.get(i).copied().unwrap_or(0);
    let a = Atoms {
        wm_protocols: g(0),
        delete: g(1),
        net_wm_name: g(2),
        utf8: g(3),
        clipboard: g(4),
        targets: g(5),
        motif: g(6),
        change_state: g(7),
        net_wm_state: g(8),
        max_vert: g(9),
        max_horz: g(10),
        prop: g(11),
        incr: g(12),
        text: g(13),
        cm_s0: g(14),
        wtype: g(15),
        wt_dialog: g(16),
        wt_popup_menu: g(17),
        wt_dropdown: g(18),
        wt_tooltip: g(19),
        wt_normal: g(20),
    };
    if ids.len() != ATOM_NAMES.len() || [a.wm_protocols, a.delete, a.utf8, a.clipboard].contains(&0)
    {
        return Err(DisplayError::Protocol(
            "server would not intern atoms".into(),
        ));
    }
    Ok(a)
}

/// Major opcode of an extension, if the server has it.
fn query_major(conn: &mut Conn, name: &str) -> Option<u8> {
    let q = conn.round_trip(proto::query_extension(name)).ok()?;
    let present = q.body.first().copied().unwrap_or(0) != 0;
    let major = q.body.get(1).copied().unwrap_or(0);
    (present && major != 0).then_some(major)
}

/// Is a compositing manager running on screen 0? EWMH: it owns the selection
/// `_NET_WM_CM_S0`. Fail closed: any error counts as "none", which picks the
/// SHAPE fallback (always correct, merely aliased).
fn compositor_running(conn: &mut Conn, atom: u32) -> bool {
    if atom == 0 {
        return false;
    }
    conn.round_trip(proto::get_selection_owner(atom))
        .ok()
        .and_then(|r| r.u32_at(8))
        .is_some_and(|owner| owner != 0)
}

/// Clip the window (and its input) to the outline, as the mode allows.
fn apply_shape(
    conn: &mut Conn,
    wid: u32,
    mode: ShapeMode,
    major: Option<u8>,
    kind: WindowKind,
    mask: &Mask,
) -> Result<(), DisplayError> {
    let Some(major) = major else {
        return Ok(());
    };
    if matches!(mode, ShapeMode::Rect | ShapeMode::Software) {
        return Ok(());
    }
    // A tooltip never takes the pointer: an empty input region.
    let rects: Vec<(i16, i16, u16, u16)> = if kind == WindowKind::Tooltip {
        Vec::new()
    } else {
        mask.rects()
            .iter()
            .filter_map(|r| {
                Some((
                    i16::try_from(r.x).ok()?,
                    i16::try_from(r.y).ok()?,
                    u16::try_from(r.w).ok()?,
                    u16::try_from(r.h).ok()?,
                ))
            })
            .collect()
    };
    if mode == ShapeMode::ShapeExt {
        // Bounding region: the visible, clipped shape. Aliased.
        let all: Vec<_> = mask
            .rects()
            .iter()
            .filter_map(|r| {
                Some((
                    i16::try_from(r.x).ok()?,
                    i16::try_from(r.y).ok()?,
                    u16::try_from(r.w).ok()?,
                    u16::try_from(r.h).ok()?,
                ))
            })
            .collect();
        conn.send(proto::shape_rectangles(major, wid, 0, &all))?;
    }
    // Input region: transparent corners belong to the window below.
    conn.send(proto::shape_rectangles(major, wid, 2, &rects))?;
    Ok(())
}

fn fetch_keymap(conn: &mut Conn) -> Result<KeyMap, DisplayError> {
    let min = conn.setup.min_keycode;
    let max = conn.setup.max_keycode;
    if max < min {
        return Err(DisplayError::Protocol("bad keycode range".into()));
    }
    let count = max - min + 1;
    let r = conn.round_trip(proto::get_keyboard_mapping(min, count))?;
    let per = usize::from(r.detail);
    let syms: Vec<u32> = r
        .data_from(32)
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| u32::from_le_bytes(*c))
        .collect();
    Ok(KeyMap::new(min, per, syms))
}

fn enable_big_requests(conn: &mut Conn) {
    // Optional: any failure leaves the normal limit in force.
    let Ok(q) = conn.round_trip(proto::query_extension("BIG-REQUESTS")) else {
        return;
    };
    let present = q.body.first().copied().unwrap_or(0) != 0;
    let major = q.body.get(1).copied().unwrap_or(0);
    if !present || major == 0 {
        return;
    }
    if let Ok(r) = conn.round_trip(proto::Req::new(major, 0))
        && let Some(words) = r.u32_at(8)
    {
        conn.enable_big_requests(words);
    }
}

impl PlatformWindow for X11Window {
    fn config(&self) -> PlatformConfig {
        PlatformConfig::desktop()
    }

    fn size(&self) -> Size {
        self.size
    }

    fn present(&mut self, frame: &Canvas, damage: Rect) -> Result<(), PlatformError> {
        self.put_frame(frame, damage)
            .map_err(|e| PlatformError::Present(e.to_string()))
    }

    fn set_cursor(&mut self, shape: CursorShape) {
        let [default, text, pointer] = self.cursors;
        let c = match shape {
            CursorShape::Default => default,
            CursorShape::Text => text,
            CursorShape::Pointer => pointer,
        };
        let r = self
            .conn
            .send(proto::set_window_cursor(self.wid, c))
            .map(|_| ());
        self.note(r);
    }

    /// No input method on X11 yet (XIM or a D-Bus client is its own ADR).
    fn set_ime_allowed(&mut self, _allowed: bool) {}

    fn set_ime_cursor_area(&mut self, _area: Rect) {}

    fn clipboard_write(&mut self, text: &str) {
        self.clip = Some(text.to_string());
        let r = self
            .conn
            .send(proto::set_selection_owner(self.wid, self.atoms.clipboard))
            .map(|_| ());
        self.note(r);
    }

    fn clipboard_request_read(&mut self) {
        if let Some(t) = self.clip.clone() {
            // We own the selection: no round trip through the server.
            self.out.push_back(DisplayEvent::Ui(Event::Clipboard(t)));
            return;
        }
        self.tried_string = false;
        let a = self.atoms;
        let r = self
            .conn
            .send(proto::convert_selection(
                self.wid,
                a.clipboard,
                a.utf8,
                a.prop,
            ))
            .map(|_| ());
        self.note(r);
    }

    fn window_action(&mut self, action: WindowAction) {
        let a = self.atoms;
        let r = match action {
            WindowAction::Close => {
                self.out.push_back(DisplayEvent::Close);
                Ok(())
            }
            // ICCCM: WM_CHANGE_STATE to the root with IconicState (3).
            WindowAction::Minimize => self.root_client_message(a.change_state, [3, 0, 0, 0, 0]),
            // EWMH: toggle both maximised axes.
            WindowAction::Maximize => {
                self.root_client_message(a.net_wm_state, [2, a.max_vert, a.max_horz, 1, 0])
            }
        };
        self.note(r);
    }

    fn request_redraw(&mut self) {
        self.out.push_back(DisplayEvent::Redraw(Rect::new(
            0,
            0,
            self.size.w,
            self.size.h,
        )));
    }
}

impl Display for X11Window {
    fn wait_events(
        &mut self,
        timeout: Option<Duration>,
    ) -> Result<Vec<DisplayEvent>, DisplayError> {
        loop {
            if let Some(e) = self.failed.take() {
                return Err(e);
            }
            self.conn.drain_buffered()?;
            while let Some(ev) = self.conn.events.pop_front() {
                self.handle(ev)?;
            }
            if let Some(e) = self.failed.take() {
                return Err(e);
            }
            if !self.out.is_empty() {
                return Ok(coalesce(self.out.drain(..).collect()));
            }
            let wait = match timeout {
                None => Wait::Block,
                Some(d) => Wait::For(d),
            };
            if !self.conn.fill(wait)? {
                return Ok(Vec::new());
            }
        }
    }
}

impl Drop for X11Window {
    fn drop(&mut self) {
        let _ = self.conn.send(proto::destroy_window(self.wid));
    }
}

/// Keep only the last of consecutive pointer moves and merge redraws.
fn coalesce(v: Vec<DisplayEvent>) -> Vec<DisplayEvent> {
    let mut out: Vec<DisplayEvent> = Vec::with_capacity(v.len());
    for e in v {
        if let (
            Some(DisplayEvent::Ui(Event::Pointer(prev))),
            DisplayEvent::Ui(Event::Pointer(next)),
        ) = (out.last(), &e)
            && prev.kind == PointerKind::Move
            && next.kind == PointerKind::Move
        {
            out.pop();
        }
        out.push(e);
    }
    out
}
