// Each integration-test binary compiles this module separately and uses a
// different part of it — fetch.rs reads the recorded digests, connections.rs
// counts accepts — so anything one of them does not touch is dead code in that
// binary. This allow is for that, and for nothing else: the panic lints §4.3
// specifies are untouched, and the crate's CLAUDE.md forbids waiving those.
#![allow(dead_code)]

//! A local server that replays the recorded corpus.
//!
//! Shared by `fetch.rs` and `connections.rs`. It exists so the Phase 3 gate
//! can fetch two hundred URLs without reaching the internet: a gate that
//! depends on somebody else's uptime is a gate that goes red for reasons
//! nobody here controls, and one that is red for such reasons is one people
//! learn to ignore.
//!
//! It also lets the awkward framings be summoned on demand. A chunked response
//! with a trailer, a 304 carrying a `Content-Length` that must be ignored, a
//! body delimited only by connection close — those are the cases
//! `px_net::http1` makes decisions about, and they are not reliably available
//! from the real web.
//!
//! The server writes responses **byte by byte from the archive**, rather than
//! through any of the code under test. A replay built on the parser would
//! agree with the parser by construction.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// One recorded exchange, as the server needs it.
#[derive(Debug, Clone)]
pub struct Recorded {
    pub path: String,
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub framing: String,
    pub body_sha256: String,
}

fn archives_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("compat")
        .join("archives")
}

/// Read a quoted JSON string value for `key` out of `text`, starting at
/// `from`.
///
/// A hand-rolled reader rather than a JSON dependency: this crate's budget is
/// spent on rustls (ADR 016), and the corpus is written by
/// `ci/make-compat-corpus.py` in a fixed shape that this only has to read
/// back. It is not a general JSON parser and does not pretend to be.
fn json_string(text: &str, key: &str, from: usize) -> Option<String> {
    let needle = format!("\"{key}\":");
    let start = text.get(from..)?.find(&needle)? + from + needle.len();
    let rest = text.get(start..)?;
    let quote = rest.find('"')? + 1;
    let mut out = String::new();
    let mut chars = rest.get(quote..)?.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                't' => out.push('\t'),
                '"' => out.push('"'),
                '\\' => out.push('\\'),
                'u' => {
                    let hex: String = chars.by_ref().take(4).collect();
                    let code = u32::from_str_radix(&hex, 16).ok()?;
                    out.push(char::from_u32(code)?);
                }
                other => out.push(other),
            },
            _ => out.push(ch),
        }
    }
    None
}

fn json_number(text: &str, key: &str) -> Option<u64> {
    let needle = format!("\"{key}\":");
    let start = text.find(&needle)? + needle.len();
    let rest = text.get(start..)?.trim_start();
    let end = rest.find(|c: char| !c.is_ascii_digit())?;
    rest.get(..end)?.parse().ok()
}

/// Load every archive.
pub fn load_corpus() -> Vec<Recorded> {
    let mut out = Vec::new();
    let dir = archives_dir();
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "har"))
        .collect();
    paths.sort();

    for path in paths {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Some(request_path) = json_string(&text, "path", 0) else {
            continue;
        };
        let Some(status) = json_number(&text, "status") else {
            continue;
        };
        let body = json_string(&text, "text", 0).unwrap_or_default();
        let framing = json_string(&text, "framing", 0).unwrap_or_default();
        let body_sha256 = json_string(&text, "body_sha256", 0).unwrap_or_default();

        // Headers are name/value pairs in an array; walk them in order.
        let mut headers = Vec::new();
        if let Some(block_start) = text.find("\"headers\":") {
            let mut cursor = block_start;
            while let Some(name) = json_string(&text, "name", cursor) {
                let name_at = text.get(cursor..).and_then(|r| r.find("\"name\":"));
                let Some(offset) = name_at else { break };
                cursor += offset + 7;
                let Some(value) = json_string(&text, "value", cursor) else {
                    break;
                };
                headers.push((name, value));
                if text.get(cursor..).is_none_or(|r| !r.contains("\"name\":")) {
                    break;
                }
            }
        }

        out.push(Recorded {
            path: request_path,
            status: status as u16,
            headers,
            body: body.into_bytes(),
            framing,
            body_sha256,
        });
    }
    out
}

/// A running replay server.
pub struct ReplayServer {
    pub port: u16,
    /// How many TCP connections have been accepted.
    ///
    /// The Phase 3 gate asserts there are no connections outside the requested
    /// set, and this is the observation that makes that testable: a fetch that
    /// opened a second connection, preconnected, or contacted a responder
    /// would show up here as a count the test did not expect.
    pub accepted: Arc<AtomicUsize>,
}

/// Start a server replaying `corpus` on an ephemeral localhost port.
pub fn start(corpus: Vec<Recorded>) -> std::io::Result<ReplayServer> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let accepted = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&accepted);

    let by_path: HashMap<String, Recorded> = corpus
        .into_iter()
        .map(|entry| (entry.path.clone(), entry))
        .collect();

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            counter.fetch_add(1, Ordering::SeqCst);
            let table = by_path.clone();
            std::thread::spawn(move || {
                let _ = serve_one(stream, &table);
            });
        }
    });

    Ok(ReplayServer { port, accepted })
}

fn serve_one(mut stream: TcpStream, table: &HashMap<String, Recorded>) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;

    // Drain the header block so the client's write completes.
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 || line.trim().is_empty() {
            break;
        }
    }

    let path = request_line.split_whitespace().nth(1).unwrap_or("/");
    let Some(entry) = table.get(path) else {
        stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n")?;
        return Ok(());
    };

    // Built by hand, not through px_net::http1. A replay that used the code
    // under test would agree with it by construction and prove nothing.
    let mut out: Vec<u8> = Vec::new();
    out.extend_from_slice(format!("HTTP/1.1 {} X\r\n", entry.status).as_bytes());
    for (name, value) in &entry.headers {
        out.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
    }

    match entry.framing.as_str() {
        "chunked" | "chunked_trailer" => {
            out.extend_from_slice(b"Transfer-Encoding: chunked\r\n\r\n");
            // Split across two chunks so the decoder's loop actually loops.
            let mid = entry.body.len() / 2;
            for part in [
                entry.body.get(..mid).unwrap_or_default(),
                entry.body.get(mid..).unwrap_or_default(),
            ] {
                if part.is_empty() {
                    continue;
                }
                out.extend_from_slice(format!("{:x}\r\n", part.len()).as_bytes());
                out.extend_from_slice(part);
                out.extend_from_slice(b"\r\n");
            }
            out.extend_from_slice(b"0\r\n");
            if entry.framing == "chunked_trailer" {
                out.extend_from_slice(b"X-Trailer: recorded\r\n");
            }
            out.extend_from_slice(b"\r\n");
        }
        "until_close" => {
            // No length and no encoding: the body ends when the socket does.
            out.extend_from_slice(b"\r\n");
            out.extend_from_slice(&entry.body);
        }
        "none" => {
            // A 204 or 304. A Content-Length is written deliberately, because
            // the parser must ignore it — an upstream that honours it and a
            // client that does not have desynchronised.
            out.extend_from_slice(b"Content-Length: 99\r\n\r\n");
        }
        _ => {
            out.extend_from_slice(
                format!("Content-Length: {}\r\n\r\n", entry.body.len()).as_bytes(),
            );
            out.extend_from_slice(&entry.body);
        }
    }

    stream.write_all(&out)?;
    stream.flush()?;
    Ok(())
}
