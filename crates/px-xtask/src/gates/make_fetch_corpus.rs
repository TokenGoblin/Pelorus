//! `cargo xtask make-fetch-corpus` -- generate the synthetic response corpus
//! the Phase 3 gate fetches against. Port of `ci/make-fetch-corpus.py`.
//!
//! This is NOT the compat suite. `tests/compat/archives/` holds recorded
//! traffic from real sites and is deliberately uncommitted (ADR 002) because
//! those archives name every host they replay, which is the disclosure opaque
//! ids exist to avoid. This corpus is generated, every host in it is the
//! loopback address, and it discloses nothing. Conflating the two made the
//! Phase 3 gate require files that could never be committed, which is how the
//! distinction got noticed.
//!
//! Two hundred archives, written deterministically so the corpus is a *fixed
//! target*: "200 URLs fetched correctly" has to mean the same thing in a year,
//! and a corpus that drifts turns a gate into a mood. Output is byte-identical
//! to the Python script's (sorted keys, two-space indent, LF, trailing
//! newline), because the files are committed and a regeneration that reorders
//! keys would be a diff nobody can read.
//!
//! Why recorded rather than live. A gate that reaches the internet goes red
//! when somebody else has an outage, and a gate that is red for reasons nobody
//! here controls is one people learn to ignore. It also makes the awkward
//! cases reachable on demand: a response that is chunked with a trailer, a 304
//! with a Content-Length that must be ignored, a body that ends only at close.
//! Those are the ones worth testing and the ones you cannot summon from the
//! real web.
//!
//! The shapes here are deliberately the ones `crates/px-net/src/http1.rs`
//! makes decisions about. A corpus of two hundred plain 200s would exercise
//! one branch two hundred times.

use crate::Gate;
use crate::json::{self, Value};
use crate::sha256;
use std::path::Path;

const OUT: &str = "tests/net/corpus";
const COUNT: usize = 200;

/// Written into every archive. The name is the retired script's, kept verbatim
/// so the committed corpus regenerates byte for byte.
const CREATOR: &str = "ci/make-fetch-corpus.py";

/// Response shapes, cycled so every one of them appears many times and the
/// corpus stays balanced rather than ending with a tail of exotic cases.
const SHAPES: &[&str] = &[
    "ok_length",
    "ok_chunked",
    "ok_chunked_trailer",
    "ok_until_close",
    "ok_empty_body",
    "redirect_301",
    "redirect_302",
    "not_modified_304",
    "no_content_204",
    "not_found_404",
    "server_error_500",
    "ok_utf8_body",
    "ok_large_body",
    "ok_many_headers",
];

pub fn body_for(index: usize, shape: &str) -> Vec<u8> {
    match shape {
        "ok_utf8_body" => format!(
            "page {index} \u{2014} k\u{e6}rlighed, \u{65e5}\u{672c}\u{8a9e}, emoji \u{1f642}"
        )
        .into_bytes(),
        // Big enough to span several reads without being slow.
        "ok_large_body" => format!("line {index} {}\n", "x".repeat(120))
            .into_bytes()
            .repeat(400),
        "ok_empty_body" | "not_modified_304" | "no_content_204" => Vec::new(),
        _ => format!("<html><body>resource {index}</body></html>").into_bytes(),
    }
}

fn header(name: &str, value: &str) -> Value {
    Value::object(vec![
        ("name", Value::str(name)),
        ("value", Value::str(value)),
    ])
}

pub fn archive(index: usize) -> Value {
    let shape = SHAPES[index % SHAPES.len()];
    let path = format!("/r/{index:03}");
    let body = body_for(index, shape);

    let mut status = 200usize;
    let mut headers = vec![header("Content-Type", "text/html; charset=utf-8")];
    let mut framing = "length";

    match shape {
        "ok_chunked" => framing = "chunked",
        "ok_chunked_trailer" => framing = "chunked_trailer",
        "ok_until_close" => framing = "until_close",
        "redirect_301" => {
            status = 301;
            headers.push(header(
                "Location",
                &format!("/r/{:03}", (index + 1) % COUNT),
            ));
        }
        "redirect_302" => {
            status = 302;
            headers.push(header(
                "Location",
                &format!("/r/{:03}", (index + 7) % COUNT),
            ));
        }
        "not_modified_304" => {
            status = 304;
            framing = "none";
        }
        "no_content_204" => {
            status = 204;
            framing = "none";
        }
        "not_found_404" => status = 404,
        "server_error_500" => status = 500,
        "ok_many_headers" => {
            for n in 0..24 {
                headers.push(header(&format!("X-Custom-{n}"), &format!("value-{n}")));
            }
        }
        _ => {}
    }

    // A HAR-shaped subset: the fields this corpus needs, not the whole
    // specification. Named .har because that is what the gate counts and what
    // a reader will expect; the schema is documented by this file.
    Value::object(vec![(
        "log",
        Value::object(vec![
            ("version", Value::str("1.2")),
            (
                "creator",
                Value::object(vec![
                    ("name", Value::str(CREATOR)),
                    ("version", Value::str("1")),
                ]),
            ),
            (
                "entries",
                Value::Array(vec![Value::object(vec![
                    (
                        "request",
                        Value::object(vec![
                            ("method", Value::str("GET")),
                            ("url", Value::str(format!("http://localhost{path}"))),
                        ]),
                    ),
                    (
                        "response",
                        Value::object(vec![
                            ("status", Value::int(status)),
                            ("headers", Value::Array(headers)),
                            (
                                "content",
                                Value::object(vec![
                                    ("size", Value::int(body.len())),
                                    ("text", Value::str(String::from_utf8_lossy(&body))),
                                    ("encoding", Value::str("utf-8")),
                                ]),
                            ),
                        ]),
                    ),
                    (
                        "_replay",
                        Value::object(vec![
                            ("path", Value::str(path.clone())),
                            ("shape", Value::str(shape)),
                            ("framing", Value::str(framing)),
                            ("body_sha256", Value::str(sha256::hex_of(&body))),
                        ]),
                    ),
                ])]),
            ),
        ]),
    )])
}

/// The bytes of `NNN.har`.
pub fn file_text(index: usize) -> String {
    format!("{}\n", json::to_pretty(&archive(index)))
}

pub fn run(g: &mut Gate, root: &Path, args: &[String]) {
    if let Some(a) = args.first() {
        g.fail(format!(
            "unexpected argument `{a}` (make-fetch-corpus takes none)"
        ));
        return;
    }
    let out = root.join(OUT);
    if let Err(e) = std::fs::create_dir_all(&out) {
        g.fail(format!("cannot create {OUT}: {e}"));
        return;
    }
    match std::fs::read_dir(&out) {
        Ok(rd) => {
            for ent in rd.flatten() {
                let p = ent.path();
                if p.extension().is_some_and(|x| x == "har")
                    && let Err(e) = std::fs::remove_file(&p)
                {
                    g.fail(format!("cannot remove {}: {e}", p.display()));
                    return;
                }
            }
        }
        Err(e) => {
            g.fail(format!("cannot read {OUT}: {e}"));
            return;
        }
    }
    for index in 0..COUNT {
        let p = out.join(format!("{index:03}.har"));
        if let Err(e) = std::fs::write(&p, file_text(index)) {
            g.fail(format!("cannot write {}: {e}", p.display()));
            return;
        }
    }
    g.ok(format!("wrote {COUNT} archives to {OUT}"));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Fixture;

    #[test]
    fn shapes_cycle_and_redirects_wrap() {
        let a = file_text(5); // redirect_301
        assert!(a.contains("\"status\": 301"));
        assert!(a.contains("\"value\": \"/r/006\""));
        let b = file_text(COUNT - 1);
        assert!(b.contains(SHAPES[(COUNT - 1) % SHAPES.len()]));
        assert!(file_text(6).contains("\"value\": \"/r/013\""));
        assert!(file_text(3).contains("until_close"));
        assert!(file_text(7).contains("\"framing\": \"none\""));
    }

    #[test]
    fn bodies() {
        assert!(body_for(7, "not_modified_304").is_empty());
        assert_eq!(
            body_for(0, "ok_large_body").len(),
            ("line 0 ".len() + 120 + 1) * 400
        );
        assert_eq!(
            body_for(11, "ok_utf8_body"),
            "page 11 \u{2014} k\u{e6}rlighed, \u{65e5}\u{672c}\u{8a9e}, emoji \u{1f642}".as_bytes()
        );
    }

    #[test]
    fn writes_two_hundred_and_replaces_stale_files() {
        let f = Fixture::new("corpus");
        f.write("tests/net/corpus/stale.har", "x");
        f.write("tests/net/corpus/keep.txt", "x");
        let mut g = Gate::new("make-fetch-corpus");
        run(&mut g, f.path(), &[]);
        assert!(g.passed());
        let n = std::fs::read_dir(f.path().join(OUT))
            .unwrap()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "har"))
            .count();
        assert_eq!(n, COUNT);
        assert!(!f.path().join("tests/net/corpus/stale.har").exists());
        assert!(f.path().join("tests/net/corpus/keep.txt").exists());
    }

    /// The committed corpus is the script's output; regenerating must not move
    /// a byte. Skipped when the corpus is not checked out.
    #[test]
    fn matches_the_committed_corpus() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/net/corpus");
        if !dir.is_dir() {
            return;
        }
        for index in 0..COUNT {
            let got = std::fs::read(dir.join(format!("{index:03}.har"))).unwrap();
            assert_eq!(got, file_text(index).into_bytes(), "archive {index}");
        }
    }
}
