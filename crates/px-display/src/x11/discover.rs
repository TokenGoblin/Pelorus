//! Finding the display and its authorisation cookie.
//!
//! In the shipped browser this is the broker's job (it owns every OS handle,
//! ADR 044 decision 3): it reads the environment and `~/.Xauthority`, connects,
//! and hands [`super::X11Window::connect`] the stream and the cookie. This
//! module is that code, kept apart from the protocol so the move is a file
//! move. The parsers are pure; only [`open`] touches the OS.

use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use crate::DisplayError;

/// `MIT-MAGIC-COOKIE-1` and its 16 bytes, or nothing (the server allows us).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Auth {
    pub name: Vec<u8>,
    pub data: Vec<u8>,
}

/// Where a `DISPLAY` value points. Only local displays (`:N`, `:N.S`,
/// `unix:N`, `host/unix:N`) are supported: the transport is a Unix socket.
pub fn parse_display(display: &str) -> Option<(u32, u32)> {
    let rest = match display.split_once(':') {
        Some((host, rest)) if host.is_empty() || host == "unix" || host.ends_with("/unix") => rest,
        _ => return None,
    };
    let (num, screen) = match rest.split_once('.') {
        Some((n, s)) => (n, s),
        None => (rest, "0"),
    };
    Some((num.parse().ok()?, screen.parse().ok()?))
}

/// The cookie for display `number` on host `hostname` from the bytes of an
/// Xauthority file. Entries are family (u16, big-endian), address, display
/// number, name, data, each string with a big-endian u16 length prefix. A
/// `Local` (256) entry must match the host name; `Wild` (65535) matches any.
pub fn find_cookie(file: &[u8], number: u32, hostname: &str) -> Option<Auth> {
    let want = number.to_string();
    let mut p = 0usize;
    let mut best: Option<Auth> = None;
    loop {
        let family = be16(file, p)?;
        p += 2;
        let mut fields: [&[u8]; 4] = [&[]; 4];
        for f in fields.iter_mut() {
            let n = usize::from(be16(file, p)?);
            p += 2;
            *f = file.get(p..p.checked_add(n)?)?;
            p += n;
        }
        let [addr, num, name, data] = fields;
        let num_ok = num.is_empty() || num == want.as_bytes();
        let host_ok = match family {
            256 => addr == hostname.as_bytes(),
            65535 => true,
            _ => false,
        };
        if num_ok && host_ok && name == b"MIT-MAGIC-COOKIE-1" {
            let a = Auth {
                name: name.to_vec(),
                data: data.to_vec(),
            };
            // An exact host match beats a wildcard.
            if family == 256 {
                return Some(a);
            }
            best.get_or_insert(a);
        }
        if p >= file.len() {
            return best;
        }
    }
}

fn be16(b: &[u8], i: usize) -> Option<u16> {
    let s = b.get(i..i.checked_add(2)?)?;
    Some(u16::from_be_bytes([*s.first()?, *s.get(1)?]))
}

/// Connect to the display named by `$DISPLAY` and read its cookie from
/// `$XAUTHORITY` or `~/.Xauthority`. A missing cookie file is not an error
/// (many servers allow local connections); a missing socket is.
pub fn open() -> Result<(UnixStream, Auth), DisplayError> {
    let display = std::env::var("DISPLAY").map_err(|_| {
        DisplayError::Connect("DISPLAY is not set (no X server or XWayland?)".into())
    })?;
    open_display(&display)
}

pub fn open_display(display: &str) -> Result<(UnixStream, Auth), DisplayError> {
    let (number, _screen) = parse_display(display).ok_or_else(|| {
        DisplayError::Connect(format!(
            "DISPLAY={display:?} is not a local display; only :N is supported"
        ))
    })?;
    let sock = format!("/tmp/.X11-unix/X{number}");
    let stream = UnixStream::connect(&sock)
        .map_err(|e| DisplayError::Connect(format!("cannot connect to {sock}: {e}")))?;
    let path = std::env::var_os("XAUTHORITY")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".Xauthority")));
    let host = std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let auth = path
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|bytes| find_cookie(&bytes, number, &host))
        .unwrap_or_default();
    Ok((stream, auth))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(family: u16, addr: &str, num: &str, name: &str, data: &[u8]) -> Vec<u8> {
        let mut v = family.to_be_bytes().to_vec();
        for f in [addr.as_bytes(), num.as_bytes(), name.as_bytes(), data] {
            v.extend_from_slice(&(f.len() as u16).to_be_bytes());
            v.extend_from_slice(f);
        }
        v
    }

    #[test]
    fn display_strings() {
        assert_eq!(parse_display(":0"), Some((0, 0)));
        assert_eq!(parse_display(":12.1"), Some((12, 1)));
        assert_eq!(parse_display("unix:3"), Some((3, 0)));
        assert_eq!(parse_display("example.org:0"), None);
        assert_eq!(parse_display("garbage"), None);
        assert_eq!(parse_display(":x"), None);
    }

    #[test]
    fn cookie_matches_host_and_display() {
        let mut f = entry(256, "other", "0", "MIT-MAGIC-COOKIE-1", b"AAAA");
        f.extend(entry(256, "me", "1", "MIT-MAGIC-COOKIE-1", b"BBBB"));
        f.extend(entry(65535, "", "0", "MIT-MAGIC-COOKIE-1", b"WILD"));
        f.extend(entry(256, "me", "0", "MIT-MAGIC-COOKIE-1", b"CCCC"));
        let a = find_cookie(&f, 0, "me").unwrap();
        assert_eq!(a.data, b"CCCC");
        assert_eq!(find_cookie(&f, 0, "nobody").unwrap().data, b"WILD");
        assert_eq!(find_cookie(&f, 7, "me"), None);
    }

    #[test]
    fn truncated_file_is_no_cookie() {
        let f = entry(256, "me", "0", "MIT-MAGIC-COOKIE-1", b"CCCC");
        assert_eq!(find_cookie(f.get(..f.len() - 1).unwrap(), 0, "me"), None);
        assert_eq!(find_cookie(&[], 0, "me"), None);
    }
}
