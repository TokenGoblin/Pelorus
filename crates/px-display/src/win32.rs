//! The Win32 backend: `windows-sys`, one top-level window, `StretchDIBits`
//! presentation, per-monitor-v2 DPI, keyboard, mouse, IMM32 composition and the
//! Unicode text clipboard. This file holds every `unsafe` block in the crate;
//! each has a `// SAFETY:` comment, to be reviewed against the Win32
//! documentation (ADR 044, the same rule as `px-sandbox`).
//!
//! **Written on Linux and compile-checked for `x86_64-pc-windows-gnu`; never
//! run.** CI on Windows is the first execution (as ADR 048 says of the sandbox
//! profile).
//!
//! Design notes:
//! - The window procedure receives a pointer to [`State`], stored in
//!   `GWLP_USERDATA`. `State` is only ever borrowed shared, and all mutation goes
//!   through `Cell`/`RefCell`, because `ShowWindow`, `SetWindowPos` and
//!   `CreateWindowExW` send messages synchronously: the window procedure runs
//!   inside our own method calls. No `RefCell` borrow is held across a call that
//!   can dispatch a message.
//! - Sizes the chrome sees are logical (96 dpi) pixels. Mouse coordinates are
//!   divided by the scale, and a frame is stretched to the physical client area.
//!   That is blurry above 100%; rendering at device scale is a later change.
//! - While Windows runs its own modal move/size loop the window procedure keeps
//!   being called but `wait_events` is not, so events queue until it ends.
//! - AltGr arrives as Ctrl+Alt; both are cleared for key events so that the
//!   `WM_CHAR` text it produces is not also a shortcut.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::ptr::{null, null_mut};
use std::time::{Duration, Instant};

use px_paint::raster::Canvas;
use px_ui::chrome::PlatformConfig;
use px_ui::geom::{Rect, Size};
use px_ui::input::{
    Button, CursorShape, Event, Key, KeyEvent, Modifiers, PointerEvent, PointerKind, Preedit,
    WindowAction,
};
use px_ui::platform::{PlatformError, PlatformWindow};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BeginPaint, DIB_RGB_COLORS, EndPaint, GetDC, HALFTONE,
    PAINTSTRUCT, ReleaseDC, SRCCOPY, SetStretchBltMode, StretchDIBits,
};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use windows_sys::Win32::UI::HiDpi::{
    AdjustWindowRectExForDpi, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow,
    SetProcessDpiAwarenessContext,
};
use windows_sys::Win32::UI::Input::Ime::{
    CANDIDATEFORM, CFS_CANDIDATEPOS, CFS_POINT, COMPOSITIONFORM, GCS_COMPSTR, GCS_RESULTSTR,
    IACE_DEFAULT, ImmAssociateContextEx, ImmGetCompositionStringW, ImmGetContext,
    ImmReleaseContext, ImmSetCandidateWindow, ImmSetCompositionWindow,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, MapVirtualKeyW, ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT,
    TrackMouseEvent, VK_BACK, VK_CONTROL, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_F1, VK_F12,
    VK_HOME, VK_LEFT, VK_LWIN, VK_MENU, VK_NEXT, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_RWIN, VK_SHIFT,
    VK_TAB, VK_UP,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, CreateWindowExW, DefWindowProcW,
    DestroyWindow, DispatchMessageW, GWLP_USERDATA, GetClientRect, GetWindowLongPtrW, HTCLIENT,
    IDC_ARROW, IDC_HAND, IDC_IBEAM, IsZoomed, LoadCursorW, MSG, MsgWaitForMultipleObjects,
    PM_REMOVE, PeekMessageW, QS_ALLINPUT, RegisterClassExW, SIZE_MINIMIZED, SW_MAXIMIZE,
    SW_MINIMIZE, SW_RESTORE, SW_SHOW, SWP_NOACTIVATE, SWP_NOZORDER, SetCursor, SetWindowLongPtrW,
    SetWindowPos, ShowWindow, TranslateMessage, WM_CHAR, WM_CLOSE, WM_DPICHANGED, WM_ERASEBKGND,
    WM_IME_COMPOSITION, WM_IME_ENDCOMPOSITION, WM_IME_STARTCOMPOSITION, WM_KEYDOWN, WM_KILLFOCUS,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEMOVE, WM_NCCREATE,
    WM_NCDESTROY, WM_PAINT, WM_QUIT, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SETCURSOR, WM_SETFOCUS,
    WM_SIZE, WM_SYSKEYDOWN, WNDCLASSEXW, WS_OVERLAPPEDWINDOW, WS_POPUP,
};

use crate::{Display, DisplayError, DisplayEvent, WindowOptions};

const CF_UNICODETEXT: u32 = 13;
const MAX_CLIPBOARD_UNITS: usize = 1 << 20;
const MAPVK_VK_TO_CHAR: u32 = 2;
const INFINITE: u32 = u32::MAX;
const WM_MOUSELEAVE: u32 = 0x02A3;
const WAIT_TIMEOUT: u32 = 0x102;
const CLASS_NAME: &str = "px_display_window";

struct State {
    hwnd: Cell<HWND>,
    /// Physical client size.
    client: Cell<(i32, i32)>,
    dpi: Cell<u32>,
    queue: RefCell<VecDeque<DisplayEvent>>,
    /// Last presented frame as top-down BGRA, `frame_size` logical pixels.
    bgra: RefCell<Vec<u8>>,
    frame_size: Cell<(i32, i32)>,
    high_surrogate: Cell<Option<u16>>,
    tracking_leave: Cell<bool>,
    buttons_down: Cell<u32>,
    cursor: Cell<CursorShape>,
    composing: Cell<bool>,
    ime_area: Cell<Rect>,
}

impl State {
    fn push(&self, e: DisplayEvent) {
        if let Ok(mut q) = self.queue.try_borrow_mut() {
            q.push_back(e);
        }
    }

    fn scale(&self, v: i32) -> i32 {
        // Physical to logical.
        let dpi = i64::from(self.dpi.get().max(1));
        ((i64::from(v) * 96 + dpi / 2) / dpi) as i32
    }

    fn logical_size(&self) -> Size {
        let (w, h) = self.client.get();
        Size::new(self.scale(w), self.scale(h))
    }
}

pub struct Win32Window {
    state: Box<State>,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn lo(l: isize) -> i32 {
    i32::from((l & 0xffff) as u16 as i16)
}
fn hi(l: isize) -> i32 {
    i32::from(((l >> 16) & 0xffff) as u16 as i16)
}

fn key_down(vk: i32) -> bool {
    // SAFETY: GetKeyState takes a virtual-key code by value, reads thread
    // keyboard state and has no preconditions on its argument.
    unsafe { GetKeyState(vk) < 0 }
}

fn modifiers() -> Modifiers {
    let (mut ctrl, mut alt) = (
        key_down(i32::from(VK_CONTROL)),
        key_down(i32::from(VK_MENU)),
    );
    if ctrl && alt {
        // AltGr.
        ctrl = false;
        alt = false;
    }
    Modifiers {
        shift: key_down(i32::from(VK_SHIFT)),
        alt,
        ctrl,
        meta: key_down(i32::from(VK_LWIN)) || key_down(i32::from(VK_RWIN)),
    }
}

fn map_key(vk: u32) -> Option<Key> {
    let vk16 = u16::try_from(vk).ok()?;
    Some(match vk16 {
        VK_BACK => Key::Backspace,
        VK_TAB => Key::Tab,
        VK_RETURN => Key::Enter,
        VK_ESCAPE => Key::Escape,
        VK_DELETE => Key::Delete,
        VK_HOME => Key::Home,
        VK_END => Key::End,
        VK_LEFT => Key::Left,
        VK_RIGHT => Key::Right,
        VK_UP => Key::Up,
        VK_DOWN => Key::Down,
        VK_PRIOR => Key::PageUp,
        VK_NEXT => Key::PageDown,
        f @ VK_F1..=VK_F12 => Key::F((f - VK_F1 + 1) as u8),
        0x30..=0x39 | 0x41..=0x5a => {
            let c = char::from_u32(vk)?;
            Key::Char(c.to_ascii_lowercase())
        }
        _ => {
            // SAFETY: MapVirtualKeyW takes two integers and returns an
            // integer; any virtual-key code is a valid argument.
            let ch = unsafe { MapVirtualKeyW(vk, MAPVK_VK_TO_CHAR) };
            // The high bit marks a dead key: not a key the chrome can name.
            if ch == 0 || ch & 0x8000_0000 != 0 {
                return None;
            }
            let c = char::from_u32(ch & 0x7fff_ffff)?;
            if c.is_control() {
                return None;
            }
            Key::Char(c.to_lowercase().next().unwrap_or(c))
        }
    })
}

/// UTF-16 from the IME, via `ImmGetCompositionStringW`.
fn ime_string(hwnd: HWND, what: u32) -> Option<String> {
    // SAFETY: `hwnd` is our window; ImmGetContext returns a context handle or
    // null, which is checked, and it is released below on every path.
    let himc = unsafe { ImmGetContext(hwnd) };
    if himc.is_null() {
        return None;
    }
    // SAFETY: a null buffer with length 0 asks for the size in bytes.
    let bytes = unsafe { ImmGetCompositionStringW(himc, what, null_mut(), 0) };
    let out = if bytes >= 0 && (bytes as usize) <= 2 * MAX_CLIPBOARD_UNITS {
        let mut buf = vec![0u16; bytes as usize / 2];
        // SAFETY: `buf` is a live allocation of `bytes` bytes (rounded down to
        // whole units), and the call writes at most the length we pass.
        let got = unsafe {
            ImmGetCompositionStringW(himc, what, buf.as_mut_ptr().cast(), (buf.len() * 2) as u32)
        };
        if got >= 0 {
            buf.truncate(got as usize / 2);
            Some(String::from_utf16_lossy(&buf))
        } else {
            None
        }
    } else {
        None
    };
    // SAFETY: `himc` came from ImmGetContext for this `hwnd` just above.
    unsafe { ImmReleaseContext(hwnd, himc) };
    out
}

fn place_ime(hwnd: HWND, area: Rect, dpi: u32) {
    let phys = |v: i32| ((i64::from(v) * i64::from(dpi) + 48) / 96) as i32;
    // SAFETY: as in `ime_string`; the context is checked and released.
    let himc = unsafe { ImmGetContext(hwnd) };
    if himc.is_null() {
        return;
    }
    let pt = windows_sys::Win32::Foundation::POINT {
        x: phys(area.x),
        y: phys(area.y),
    };
    let comp = COMPOSITIONFORM {
        dwStyle: CFS_POINT,
        ptCurrentPos: pt,
        rcArea: RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        },
    };
    let cand = CANDIDATEFORM {
        dwIndex: 0,
        dwStyle: CFS_CANDIDATEPOS,
        ptCurrentPos: windows_sys::Win32::Foundation::POINT {
            x: pt.x,
            y: phys(area.y + area.h),
        },
        rcArea: RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        },
    };
    // SAFETY: `himc` is a live input context; both pointers are to fully
    // initialised stack structs that outlive the calls.
    unsafe {
        ImmSetCompositionWindow(himc, &comp);
        ImmSetCandidateWindow(himc, &cand);
        ImmReleaseContext(hwnd, himc);
    }
}

fn blit(st: &State, hdc: windows_sys::Win32::Graphics::Gdi::HDC) {
    let Ok(bits) = st.bgra.try_borrow() else {
        return;
    };
    let (fw, fh) = st.frame_size.get();
    let (cw, ch) = st.client.get();
    if fw <= 0 || fh <= 0 || cw <= 0 || ch <= 0 || bits.len() != fw as usize * fh as usize * 4 {
        return;
    }
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: fw,
            biHeight: -fh, // top-down
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            biSizeImage: 0,
            biXPelsPerMeter: 0,
            biYPelsPerMeter: 0,
            biClrUsed: 0,
            biClrImportant: 0,
        },
        bmiColors: [windows_sys::Win32::Graphics::Gdi::RGBQUAD {
            rgbBlue: 0,
            rgbGreen: 0,
            rgbRed: 0,
            rgbReserved: 0,
        }],
    };
    // SAFETY: `hdc` is a device context from BeginPaint/GetDC valid for this
    // call; `bits` holds exactly fw*fh*4 bytes (checked above) matching the
    // 32-bpp top-down header in `info`, and both outlive the call.
    unsafe {
        SetStretchBltMode(hdc, HALFTONE);
        StretchDIBits(
            hdc,
            0,
            0,
            cw,
            ch,
            0,
            0,
            fw,
            fh,
            bits.as_ptr().cast(),
            &info,
            DIB_RGB_COLORS,
            SRCCOPY,
        );
    }
}

fn client_size(hwnd: HWND) -> (i32, i32) {
    let mut r = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    // SAFETY: `r` is a valid RECT the call fills in; a stale `hwnd` makes the
    // call fail and leave `r` zeroed, which callers treat as an empty window.
    unsafe { GetClientRect(hwnd, &mut r) };
    (r.right - r.left, r.bottom - r.top)
}

/// The window procedure.
///
/// # Safety
/// Called only by the system, with the arguments of a message for a window of
/// our class.
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        // SAFETY: for WM_NCCREATE `lp` points to a CREATESTRUCTW; its
        // `lpCreateParams` is the `*const State` passed to CreateWindowExW.
        unsafe {
            let cs = lp as *const CREATESTRUCTW;
            if !cs.is_null() {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, (*cs).lpCreateParams as isize);
            }
            return DefWindowProcW(hwnd, msg, wp, lp);
        }
    }
    // SAFETY: GWLP_USERDATA holds either 0 or the pointer stored above, to the
    // boxed `State` that `Win32Window` owns until it destroys the window (the
    // pointer is cleared in WM_NCDESTROY). It is only turned into a shared
    // reference, and `State` mutates through Cell/RefCell.
    let st: Option<&State> =
        unsafe { (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const State).as_ref() };
    let Some(st) = st else {
        // SAFETY: forwarding the system's own arguments.
        return unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
    };
    match msg {
        WM_CLOSE => {
            st.push(DisplayEvent::Close);
            0
        }
        WM_NCDESTROY => {
            // SAFETY: detaching the pointer so no later message sees it.
            unsafe {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                DefWindowProcW(hwnd, msg, wp, lp)
            }
        }
        WM_ERASEBKGND => 1,
        WM_PAINT => {
            let mut ps = PAINTSTRUCT {
                hdc: null_mut(),
                fErase: 0,
                rcPaint: RECT {
                    left: 0,
                    top: 0,
                    right: 0,
                    bottom: 0,
                },
                fRestore: 0,
                fIncUpdate: 0,
                rgbReserved: [0; 32],
            };
            // SAFETY: `ps` is a valid PAINTSTRUCT; BeginPaint is paired with
            // EndPaint on the same window.
            let hdc = unsafe { BeginPaint(hwnd, &mut ps) };
            if !hdc.is_null() {
                blit(st, hdc);
            }
            // SAFETY: pairs with the BeginPaint above.
            unsafe { EndPaint(hwnd, &ps) };
            if st.bgra.try_borrow().map(|b| b.is_empty()).unwrap_or(false) {
                let s = st.logical_size();
                st.push(DisplayEvent::Redraw(Rect::new(0, 0, s.w, s.h)));
            }
            0
        }
        WM_SIZE => {
            if wp as u32 != SIZE_MINIMIZED {
                st.client.set((lo(lp).max(0), hi(lp).max(0)));
                let s = st.logical_size();
                st.push(DisplayEvent::Ui(Event::Resize(s)));
                st.push(DisplayEvent::Redraw(Rect::new(0, 0, s.w, s.h)));
            }
            0
        }
        WM_DPICHANGED => {
            st.dpi.set(((wp >> 16) & 0xffff) as u32);
            // SAFETY: for WM_DPICHANGED `lp` points to the suggested RECT.
            if let Some(r) = unsafe { (lp as *const RECT).as_ref() } {
                // SAFETY: resizing our own window to the suggested rectangle.
                unsafe {
                    SetWindowPos(
                        hwnd,
                        null_mut(),
                        r.left,
                        r.top,
                        r.right - r.left,
                        r.bottom - r.top,
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                }
            }
            0
        }
        WM_SETFOCUS => {
            st.push(DisplayEvent::Ui(Event::WindowFocus(true)));
            0
        }
        WM_KILLFOCUS => {
            st.push(DisplayEvent::Ui(Event::WindowFocus(false)));
            0
        }
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            if let Some(key) = map_key(wp as u32) {
                st.push(DisplayEvent::Ui(Event::Key(KeyEvent::with(
                    key,
                    modifiers(),
                ))));
            }
            if msg == WM_SYSKEYDOWN {
                // SAFETY: forwarding the system's own arguments (Alt+F4 etc.).
                return unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
            }
            0
        }
        WM_CHAR => {
            let unit = wp as u16;
            let ch = match (st.high_surrogate.take(), unit) {
                (Some(h), l @ 0xdc00..=0xdfff) => {
                    char::decode_utf16([h, l]).next().and_then(Result::ok)
                }
                (_, h @ 0xd800..=0xdbff) => {
                    st.high_surrogate.set(Some(h));
                    None
                }
                (_, u) => char::from_u32(u32::from(u)),
            };
            if let Some(c) = ch
                && !c.is_control()
            {
                st.push(DisplayEvent::Ui(Event::Text(c.to_string())));
            }
            0
        }
        WM_IME_STARTCOMPOSITION => {
            st.composing.set(true);
            place_ime(hwnd, st.ime_area.get(), st.dpi.get());
            0 // the chrome draws the preedit; no system composition window
        }
        WM_IME_COMPOSITION => {
            let flags = lp as u32;
            if flags & GCS_RESULTSTR != 0
                && let Some(s) = ime_string(hwnd, GCS_RESULTSTR)
                && !s.is_empty()
            {
                st.push(DisplayEvent::Ui(Event::Preedit(None)));
                st.push(DisplayEvent::Ui(Event::Text(s)));
            }
            if flags & GCS_COMPSTR != 0 {
                let text = ime_string(hwnd, GCS_COMPSTR).unwrap_or_default();
                st.push(DisplayEvent::Ui(Event::Preedit(
                    (!text.is_empty()).then_some(Preedit { text }),
                )));
            }
            0
        }
        WM_IME_ENDCOMPOSITION => {
            st.composing.set(false);
            st.push(DisplayEvent::Ui(Event::Preedit(None)));
            0
        }
        WM_MOUSEMOVE | WM_LBUTTONDOWN | WM_LBUTTONUP | WM_RBUTTONDOWN | WM_RBUTTONUP
        | WM_MBUTTONDOWN | WM_MBUTTONUP => {
            let kind = match msg {
                WM_LBUTTONDOWN => PointerKind::Down(Button::Primary),
                WM_LBUTTONUP => PointerKind::Up(Button::Primary),
                WM_RBUTTONDOWN => PointerKind::Down(Button::Secondary),
                WM_RBUTTONUP => PointerKind::Up(Button::Secondary),
                WM_MBUTTONDOWN => PointerKind::Down(Button::Middle),
                WM_MBUTTONUP => PointerKind::Up(Button::Middle),
                _ => PointerKind::Move,
            };
            match kind {
                PointerKind::Down(_) => {
                    if st.buttons_down.get() == 0 {
                        // SAFETY: capturing the mouse for our own window.
                        unsafe { SetCapture(hwnd) };
                    }
                    st.buttons_down.set(st.buttons_down.get() + 1);
                }
                PointerKind::Up(_) => {
                    st.buttons_down.set(st.buttons_down.get().saturating_sub(1));
                    if st.buttons_down.get() == 0 {
                        // SAFETY: releases the capture taken above.
                        unsafe { ReleaseCapture() };
                    }
                }
                _ => {
                    if !st.tracking_leave.get() {
                        let mut t = TRACKMOUSEEVENT {
                            cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                            dwFlags: TME_LEAVE,
                            hwndTrack: hwnd,
                            dwHoverTime: 0,
                        };
                        // SAFETY: `t` is fully initialised with its size set.
                        if unsafe { TrackMouseEvent(&mut t) } != 0 {
                            st.tracking_leave.set(true);
                        }
                    }
                }
            }
            let mut e = PointerEvent::new(st.scale(lo(lp)), st.scale(hi(lp)), kind);
            e.mods = modifiers();
            st.push(DisplayEvent::Ui(Event::Pointer(e)));
            0
        }
        WM_MOUSELEAVE => {
            st.tracking_leave.set(false);
            st.push(DisplayEvent::Ui(Event::Pointer(PointerEvent::new(
                -1,
                -1,
                PointerKind::Leave,
            ))));
            0
        }
        WM_SETCURSOR => {
            if (lp & 0xffff) as u32 == HTCLIENT {
                let id = match st.cursor.get() {
                    CursorShape::Default | CursorShape::Resize(_) => IDC_ARROW,
                    CursorShape::Text => IDC_IBEAM,
                    CursorShape::Pointer => IDC_HAND,
                };
                // SAFETY: loading a stock system cursor (no module) and
                // selecting it; both accept the values given.
                unsafe { SetCursor(LoadCursorW(null_mut(), id)) };
                return 1;
            }
            // SAFETY: forwarding the system's own arguments.
            unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
        }
        // SAFETY: forwarding the system's own arguments.
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

impl Win32Window {
    pub fn create(opts: &WindowOptions) -> Result<Win32Window, DisplayError> {
        // SAFETY: process-wide setting; failure (already set, e.g. by a
        // manifest) is fine and ignored.
        unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        let state = Box::new(State {
            hwnd: Cell::new(null_mut()),
            client: Cell::new((0, 0)),
            dpi: Cell::new(96),
            queue: RefCell::new(VecDeque::new()),
            bgra: RefCell::new(Vec::new()),
            frame_size: Cell::new((0, 0)),
            high_surrogate: Cell::new(None),
            tracking_leave: Cell::new(false),
            buttons_down: Cell::new(0),
            cursor: Cell::new(CursorShape::Default),
            composing: Cell::new(false),
            ime_area: Cell::new(Rect::default()),
        });
        let class = wide(CLASS_NAME);
        // SAFETY: a null module name asks for the current executable's handle.
        let hinst = unsafe { GetModuleHandleW(null()) };
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinst,
            hIcon: null_mut(),
            // SAFETY: stock arrow cursor, no module.
            hCursor: unsafe { LoadCursorW(null_mut(), IDC_ARROW) },
            hbrBackground: null_mut(),
            lpszMenuName: null(),
            lpszClassName: class.as_ptr(),
            hIconSm: null_mut(),
        };
        // SAFETY: `wc` is fully initialised and `class` outlives the call. A
        // zero return is "already registered" on a second window, which is fine.
        unsafe { RegisterClassExW(&wc) };

        let style = if opts.frameless {
            WS_POPUP
        } else {
            WS_OVERLAPPEDWINDOW
        };
        let mut r = RECT {
            left: 0,
            top: 0,
            right: i32::try_from(opts.width).unwrap_or(i32::MAX),
            bottom: i32::try_from(opts.height).unwrap_or(i32::MAX),
        };
        // SAFETY: `r` is a valid RECT adjusted in place; 96 dpi until the
        // window exists and WM_DPICHANGED/GetDpiForWindow say otherwise.
        unsafe { AdjustWindowRectExForDpi(&mut r, style, 0, 0, 96) };
        let title = wide(&opts.title);
        // SAFETY: class and title are NUL-terminated and outlive the call;
        // `lpParam` is the address of the boxed State, which stays put (the
        // Box is moved into `Win32Window` but its heap allocation does not move)
        // and outlives the window (Drop destroys the window first).
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                title.as_ptr(),
                style,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                r.right - r.left,
                r.bottom - r.top,
                null_mut(),
                null_mut(),
                hinst,
                (&*state as *const State).cast(),
            )
        };
        if hwnd.is_null() {
            return Err(DisplayError::Connect("CreateWindowExW failed".into()));
        }
        state.hwnd.set(hwnd);
        // SAFETY: `hwnd` is the window just created.
        let dpi = unsafe { GetDpiForWindow(hwnd) };
        state.dpi.set(if dpi == 0 { 96 } else { dpi });
        state.client.set(client_size(hwnd));
        // Start with the input method off; the chrome enables it for the
        // address field (set_ime_allowed).
        // SAFETY: disassociating the default context from our own window.
        unsafe { ImmAssociateContextEx(hwnd, null_mut(), 0) };
        // SAFETY: showing our own window.
        unsafe { ShowWindow(hwnd, SW_SHOW) };
        Ok(Win32Window { state })
    }

    fn hwnd(&self) -> HWND {
        self.state.hwnd.get()
    }
}

impl Drop for Win32Window {
    fn drop(&mut self) {
        // SAFETY: destroying the window we created, on its own thread. The
        // window procedure sees State until WM_NCDESTROY clears the pointer,
        // and `state` is dropped only after this returns.
        unsafe { DestroyWindow(self.hwnd()) };
    }
}

impl PlatformWindow for Win32Window {
    fn config(&self) -> PlatformConfig {
        PlatformConfig::desktop()
    }

    fn size(&self) -> Size {
        self.state.logical_size()
    }

    fn present(&mut self, frame: &Canvas, damage: Rect) -> Result<(), PlatformError> {
        let (fw, fh) = (
            i32::try_from(frame.width).unwrap_or(0),
            i32::try_from(frame.height).unwrap_or(0),
        );
        let need = fw as usize * fh as usize * 4;
        if frame.pixels.len() != need || need == 0 {
            return Err(PlatformError::Present(
                "frame buffer does not match its size".into(),
            ));
        }
        {
            let mut bgra = self
                .state
                .bgra
                .try_borrow_mut()
                .map_err(|_| PlatformError::Present("frame buffer is in use".into()))?;
            let whole = self.state.frame_size.get() != (fw, fh) || bgra.len() != need;
            if whole {
                bgra.clear();
                bgra.resize(need, 0);
            }
            let d = if whole {
                Rect::new(0, 0, fw, fh)
            } else {
                damage
            };
            let (x0, y0) = (d.x.max(0), d.y.max(0));
            let (x1, y1) = (d.right().min(fw), d.bottom().min(fh));
            for y in y0..y1.max(y0) {
                let a = (y as usize * fw as usize + x0 as usize) * 4;
                let b = (y as usize * fw as usize + x1.max(x0) as usize) * 4;
                let (Some(src), Some(dst)) = (frame.pixels.get(a..b), bgra.get_mut(a..b)) else {
                    continue;
                };
                let pairs = src
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .zip(dst.as_chunks_mut::<4>().0);
                for (&[r, g, bl, _], d) in pairs {
                    *d = [bl, g, r, 255];
                }
            }
        }
        self.state.frame_size.set((fw, fh));
        // SAFETY: GetDC/ReleaseDC are paired on our own window; the DC is used
        // only inside `blit` before release.
        unsafe {
            let hdc = GetDC(self.hwnd());
            if hdc.is_null() {
                return Err(PlatformError::Present("GetDC failed".into()));
            }
            blit(&self.state, hdc);
            ReleaseDC(self.hwnd(), hdc);
        }
        Ok(())
    }

    fn set_cursor(&mut self, shape: CursorShape) {
        self.state.cursor.set(shape);
        // SAFETY: stock cursor, as in WM_SETCURSOR.
        unsafe {
            let id = match shape {
                CursorShape::Default | CursorShape::Resize(_) => IDC_ARROW,
                CursorShape::Text => IDC_IBEAM,
                CursorShape::Pointer => IDC_HAND,
            };
            SetCursor(LoadCursorW(null_mut(), id));
        }
    }

    fn set_ime_allowed(&mut self, allowed: bool) {
        let flags = if allowed { IACE_DEFAULT } else { 0 };
        // SAFETY: (re)associating or removing the default input context of our
        // own window; a null HIMC with flags 0 disables IME.
        unsafe { ImmAssociateContextEx(self.hwnd(), null_mut(), flags) };
        if allowed {
            place_ime(self.hwnd(), self.state.ime_area.get(), self.state.dpi.get());
        }
    }

    fn set_ime_cursor_area(&mut self, area: Rect) {
        self.state.ime_area.set(area);
        place_ime(self.hwnd(), area, self.state.dpi.get());
    }

    fn clipboard_write(&mut self, text: &str) {
        let units: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let bytes = units.len() * 2;
        // SAFETY: the clipboard is opened for our window and closed on every
        // path. The moveable block is sized for `units`, locked before the copy
        // (the copy stays inside `bytes`) and unlocked before ownership passes
        // to the system with SetClipboardData; only if that fails would the
        // block still be ours, and it is then leaked rather than freed (a
        // failed set means the system may still hold it, so freeing is the
        // riskier mistake).
        unsafe {
            if OpenClipboard(self.hwnd()) == 0 {
                return;
            }
            EmptyClipboard();
            let h = GlobalAlloc(GMEM_MOVEABLE, bytes);
            if !h.is_null() {
                let p = GlobalLock(h).cast::<u16>();
                if !p.is_null() {
                    std::ptr::copy_nonoverlapping(units.as_ptr(), p, units.len());
                    GlobalUnlock(h);
                    SetClipboardData(CF_UNICODETEXT, h);
                }
            }
            CloseClipboard();
        }
    }

    fn clipboard_request_read(&mut self) {
        let mut out: Option<String> = None;
        // SAFETY: opened for our window and closed on every path. The handle
        // from GetClipboardData is owned by the clipboard and only read while
        // locked; the read is bounded by both GlobalSize and our own cap, and
        // stops at the first NUL.
        unsafe {
            if OpenClipboard(self.hwnd()) != 0 {
                let h = GetClipboardData(CF_UNICODETEXT);
                if !h.is_null() {
                    let size = GlobalSize(h) / 2;
                    let p = GlobalLock(h).cast::<u16>();
                    if !p.is_null() {
                        let n = size.min(MAX_CLIPBOARD_UNITS);
                        let all = std::slice::from_raw_parts(p, n);
                        let end = all.iter().position(|u| *u == 0).unwrap_or(n);
                        out = all.get(..end).map(String::from_utf16_lossy);
                        GlobalUnlock(h);
                    }
                }
                CloseClipboard();
            }
        }
        if let Some(t) = out {
            self.state.push(DisplayEvent::Ui(Event::Clipboard(t)));
        }
    }

    fn window_action(&mut self, action: WindowAction) {
        match action {
            WindowAction::Close => self.state.push(DisplayEvent::Close),
            // SAFETY (all three): ShowWindow on our own window with a valid
            // SW_* command. It may send messages synchronously; no borrow of
            // State is held here.
            WindowAction::Minimize => unsafe {
                ShowWindow(self.hwnd(), SW_MINIMIZE);
            },
            WindowAction::Maximize => unsafe {
                let zoomed = IsZoomed(self.hwnd()) != 0;
                ShowWindow(self.hwnd(), if zoomed { SW_RESTORE } else { SW_MAXIMIZE });
            },
        }
    }

    fn request_redraw(&mut self) {
        let s = self.size();
        self.state
            .push(DisplayEvent::Redraw(Rect::new(0, 0, s.w, s.h)));
    }
}

impl Display for Win32Window {
    fn wait_events(
        &mut self,
        timeout: Option<Duration>,
    ) -> Result<Vec<DisplayEvent>, DisplayError> {
        let deadline = timeout.map(|d| Instant::now() + d);
        loop {
            let mut msg = MSG {
                hwnd: null_mut(),
                message: 0,
                wParam: 0,
                lParam: 0,
                time: 0,
                pt: windows_sys::Win32::Foundation::POINT { x: 0, y: 0 },
            };
            // SAFETY: `msg` is a valid MSG; the loop is on the thread that owns
            // the window. DispatchMessageW runs `wndproc`, which only touches
            // State through shared references.
            unsafe {
                while PeekMessageW(&mut msg, null_mut(), 0, 0, PM_REMOVE) != 0 {
                    if msg.message == WM_QUIT {
                        self.state.push(DisplayEvent::Close);
                        continue;
                    }
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            let drained: Vec<DisplayEvent> = self
                .state
                .queue
                .try_borrow_mut()
                .map(|mut q| q.drain(..).collect())
                .unwrap_or_default();
            if !drained.is_empty() {
                return Ok(coalesce(drained));
            }
            let wait_ms = match deadline {
                None => INFINITE,
                Some(d) => {
                    let left = d.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        return Ok(Vec::new());
                    }
                    u32::try_from(left.as_millis())
                        .unwrap_or(INFINITE - 1)
                        .max(1)
                }
            };
            // SAFETY: no handles to wait on (count 0, null array), wake on any
            // queued input message of our thread.
            let r = unsafe { MsgWaitForMultipleObjects(0, null(), 0, wait_ms, QS_ALLINPUT) };
            if r == WAIT_TIMEOUT && deadline.is_some() {
                return Ok(Vec::new());
            }
        }
    }
}

/// Keep only the last of consecutive pointer moves.
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
