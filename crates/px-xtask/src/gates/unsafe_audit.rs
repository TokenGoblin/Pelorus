//! `cargo xtask unsafe-audit` -- unsafe-line accounting over the whole
//! dependency tree (build-spec §4.5). Port of `ci/unsafe-audit.sh` plus the
//! comparison against the committed baseline that `ci/gate-supply-chain.sh`
//! used to carry inline.
//!
//! Counts, per vendored crate, the `unsafe` tokens in its Rust sources, and
//! compares that map against `ci/unsafe-baseline.json`; any increase (or any
//! new crate with unsafe) fails and needs an ADR.
//!
//! `cargo xtask unsafe-audit --write` rewrites the baseline instead of
//! comparing (the old way was `ci/unsafe-audit.sh > ci/unsafe-baseline.json`;
//! a gate prints a verdict on stdout, so redirecting it is no longer possible).
//!
//! Why this counts text and not cargo-geiger: this is a security project, the
//! count is a gate, and the gate should not depend on a thinly-maintained
//! third-party tool that runs arbitrary build scripts to produce its number.
//! This counts tokens in text. It over-counts -- `unsafe` in a comment or a
//! string literal is counted -- and never under-counts, which is the correct
//! direction for a gate. The baseline absorbs the over-count; only movement
//! matters.
//!
//! The JSON is written with LF endings on every platform, sorted keys and
//! two-space indent: the output is compared byte-for-byte against a committed
//! baseline, and a CRLF would make every Windows run a false failure.

use crate::Gate;
use crate::proc;
use std::collections::BTreeMap;
use std::path::Path;

const BASELINE: &str = "ci/unsafe-baseline.json";

pub type Counts = BTreeMap<String, u64>;

fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Number of non-overlapping matches of the regex
/// `(?:^|[^\w])unsafe(?:[^\w]|$)` over `bytes` (ASCII `\w`). Each match
/// consumes its trailing delimiter, so `unsafe unsafe` counts once -- that
/// quirk is part of the committed baseline's meaning.
pub fn count_unsafe(b: &[u8]) -> u64 {
    const U: &[u8] = b"unsafe";
    let len = b.len();
    let at = |i: usize| b.get(i..).is_some_and(|r| r.starts_with(U));
    let mut n = 0;
    let mut p = 0;
    while p < len {
        // Alternatives in regex order: `^`, then one non-word byte.
        let mut matched = None;
        let starts = [
            (p == 0).then_some(0),
            b.get(p).filter(|c| !is_word(**c)).map(|_| p + 1),
        ];
        for u in starts.into_iter().flatten() {
            if !at(u) {
                continue;
            }
            let e = u + U.len();
            match b.get(e) {
                None => {
                    matched = Some(len);
                    break;
                }
                Some(c) if !is_word(*c) => {
                    matched = Some(e + 1);
                    break;
                }
                _ => {}
            }
        }
        match matched {
            Some(next) => {
                n += 1;
                p = next;
            }
            None => p += 1,
        }
    }
    n
}

fn count_crate(dir: &Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in rd.flatten() {
            let path = entry.path();
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_dir() {
                stack.push(path);
            } else if ft.is_symlink() && path.is_dir() {
                // os.walk lists a symlinked directory but does not descend.
            } else if path.extension().is_some_and(|e| e == "rs")
                && let Ok(bytes) = std::fs::read(&path)
            {
                total += count_unsafe(&bytes);
            }
        }
    }
    total
}

/// Count `unsafe` tokens per crate directory directly under `vendor`. Crates
/// with none are left out.
pub fn audit_dir(vendor: &Path) -> Counts {
    let mut counts = Counts::new();
    let Ok(rd) = std::fs::read_dir(vendor) else {
        return counts;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let n = count_crate(&path);
        if n > 0 {
            counts.insert(entry.file_name().to_string_lossy().into_owned(), n);
        }
    }
    counts
}

/// The baseline's exact byte form: `json.dump(indent=2, sort_keys=True)` plus a
/// trailing newline.
pub fn to_json(counts: &Counts) -> String {
    if counts.is_empty() {
        return "{}\n".to_string();
    }
    let body: Vec<String> = counts
        .iter()
        .map(|(k, v)| {
            let mut key = String::new();
            for c in k.chars() {
                match c {
                    '"' => key.push_str("\\\""),
                    '\\' => key.push_str("\\\\"),
                    c if (c as u32) < 0x20 || (c as u32) > 0x7e => {
                        let mut buf = [0u16; 2];
                        for u in c.encode_utf16(&mut buf) {
                            key.push_str(&format!("\\u{u:04x}"));
                        }
                    }
                    c => key.push(c),
                }
            }
            format!("  \"{key}\": {v}")
        })
        .collect();
    format!("{{\n{}\n}}\n", body.join(",\n"))
}

/// Parse a flat JSON object of non-negative integers (the baseline's shape).
pub fn parse_counts(text: &str) -> Result<Counts, String> {
    let c: Vec<char> = text.chars().collect();
    let mut i = 0;
    let ws = |i: &mut usize| {
        while c.get(*i).is_some_and(|x| x.is_whitespace()) {
            *i += 1;
        }
    };
    let mut out = Counts::new();
    ws(&mut i);
    if c.get(i) != Some(&'{') {
        return Err("expected a JSON object".into());
    }
    i += 1;
    ws(&mut i);
    if c.get(i) == Some(&'}') {
        i += 1;
    } else {
        loop {
            ws(&mut i);
            if c.get(i) != Some(&'"') {
                return Err("expected a string key".into());
            }
            i += 1;
            let mut key = String::new();
            loop {
                let Some(&ch) = c.get(i) else {
                    return Err("unterminated string".into());
                };
                i += 1;
                match ch {
                    '"' => break,
                    '\\' => {
                        let Some(&e) = c.get(i) else {
                            return Err("unterminated escape".into());
                        };
                        i += 1;
                        match e {
                            '"' | '\\' | '/' => key.push(e),
                            'u' => {
                                let hex: String = c.iter().skip(i).take(4).collect();
                                i += 4;
                                let cp = u32::from_str_radix(&hex, 16)
                                    .map_err(|_| "bad \\u escape".to_string())?;
                                key.push(char::from_u32(cp).unwrap_or('\u{fffd}'));
                            }
                            _ => return Err("unsupported escape".into()),
                        }
                    }
                    _ => key.push(ch),
                }
            }
            ws(&mut i);
            if c.get(i) != Some(&':') {
                return Err("expected `:`".into());
            }
            i += 1;
            ws(&mut i);
            let start = i;
            while c.get(i).is_some_and(|x| x.is_ascii_digit()) {
                i += 1;
            }
            let num: String = c[start..i].iter().collect();
            let n = num
                .parse::<u64>()
                .map_err(|_| format!("`{key}` is not a count"))?;
            out.insert(key, n);
            ws(&mut i);
            match c.get(i) {
                Some(',') => i += 1,
                Some('}') => {
                    i += 1;
                    break;
                }
                _ => return Err("expected `,` or `}`".into()),
            }
        }
    }
    ws(&mut i);
    if i != c.len() {
        return Err("trailing text after the JSON object".into());
    }
    Ok(out)
}

/// The decision, pure: `(failures, notes)`. Any increase, or a crate that
/// gained unsafe from nothing, fails. A decrease or a vanished crate is only a
/// note: it asks for the baseline to be regenerated.
pub fn compare(base: &Counts, cur: &Counts) -> (Vec<String>, Vec<String>) {
    let mut fails = Vec::new();
    let mut notes = Vec::new();
    for (krate, n) in cur {
        match base.get(krate) {
            None => fails.push(format!("new crate with unsafe: {krate} ({n})")),
            Some(was) if n > was => fails.push(format!("{krate} unsafe count {was} -> {n}")),
            Some(_) => {}
        }
    }
    for (krate, n) in base {
        match cur.get(krate) {
            None => notes.push(format!(
                "note baseline crate no longer present: {krate} ({n}) \u{2014} \
                 regenerate the baseline"
            )),
            Some(c) if c < n => notes.push(format!(
                "note {krate} unsafe count fell {n} -> {c} \u{2014} regenerate the baseline"
            )),
            Some(_) => {}
        }
    }
    (fails, notes)
}

/// `cargo vendor` the whole locked tree into a scratch directory and count it.
fn vendor_and_count(root: &Path) -> Result<Counts, String> {
    let scratch = std::env::temp_dir().join(format!("px-unsafe-audit-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).map_err(|e| format!("cannot create {scratch:?}: {e}"))?;
    let crates = scratch.join("crates");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let res = proc::capture(
        &cargo,
        &[
            "vendor",
            "--locked",
            "--versioned-dirs",
            &crates.to_string_lossy(),
        ],
        Some(root),
    );
    let out = match res {
        Ok(_) => Ok(audit_dir(&crates)),
        Err(e) => Err(format!("cargo vendor failed: {e}")),
    };
    let _ = std::fs::remove_dir_all(&scratch);
    out
}

pub fn run(g: &mut Gate, root: &Path, args: &[String]) {
    let write = match args {
        [] => false,
        [a] if a == "--write" => true,
        _ => {
            g.fail("usage: cargo xtask unsafe-audit [--write]");
            return;
        }
    };
    let current = match vendor_and_count(root) {
        Ok(c) => c,
        Err(e) => {
            g.fail(e);
            return;
        }
    };
    if write {
        match std::fs::write(root.join(BASELINE), to_json(&current)) {
            Ok(()) => g.ok(format!(
                "wrote {BASELINE} ({} crates with unsafe)",
                current.len()
            )),
            Err(e) => g.fail(format!("cannot write {BASELINE}: {e}")),
        }
        return;
    }
    let text = match std::fs::read_to_string(root.join(BASELINE)) {
        Ok(t) => t,
        Err(_) => {
            g.fail(format!(
                "{BASELINE} is missing \u{2014} there is no committed unsafe baseline to compare against"
            ));
            return;
        }
    };
    g.info(format!("unsafe audit against {BASELINE}"));
    let base = match parse_counts(&text) {
        Ok(b) => b,
        Err(e) => {
            g.fail(format!(
                "{BASELINE} is not a flat JSON object of counts: {e}"
            ));
            return;
        }
    };
    let (fails, notes) = compare(&base, &current);
    for f in &fails {
        g.fail(f);
    }
    for n in &notes {
        g.detail(n);
    }
    if fails.is_empty() {
        g.ok(format!(
            "no unsafe increase across {} crates with unsafe",
            current.len()
        ));
    } else {
        g.fail("dependency-tree unsafe count increased");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Fixture;

    #[test]
    fn token_counting_matches_the_regex() {
        assert_eq!(count_unsafe(b"unsafe"), 1);
        assert_eq!(count_unsafe(b"unsafe fn f() {}"), 1);
        assert_eq!(count_unsafe(b"x unsafe {"), 1);
        assert_eq!(count_unsafe(b"(unsafe)"), 1);
        assert_eq!(count_unsafe(b"unsafe_code"), 0);
        assert_eq!(count_unsafe(b"is_unsafe"), 0);
        assert_eq!(count_unsafe(b"unsafety"), 0);
        assert_eq!(count_unsafe(b"// unsafe in a comment"), 1);
        assert_eq!(count_unsafe(b""), 0);
        // Delimiters are consumed: adjacent tokens sharing one count once.
        assert_eq!(count_unsafe(b"unsafe unsafe"), 1);
        assert_eq!(count_unsafe(b"unsafe  unsafe"), 2);
        // Non-ASCII bytes are not word characters in a bytes pattern.
        assert_eq!(count_unsafe("\u{e9}unsafe\u{e9}".as_bytes()), 1);
        assert_eq!(count_unsafe(b"unsafe\nunsafe\n"), 1);
        assert_eq!(count_unsafe(b"unsafe\n\nunsafe\n"), 2);
    }

    #[test]
    fn json_is_byte_stable() {
        let mut c = Counts::new();
        c.insert("b-1.0.0".into(), 2);
        c.insert("a-0.1.0".into(), 10);
        assert_eq!(to_json(&c), "{\n  \"a-0.1.0\": 10,\n  \"b-1.0.0\": 2\n}\n");
        assert_eq!(to_json(&Counts::new()), "{}\n");
        assert_eq!(parse_counts(&to_json(&c)).expect("round trip"), c);
        assert_eq!(parse_counts("{}").expect("empty"), Counts::new());
        assert!(parse_counts("[1]").is_err());
        assert!(parse_counts("{\"a\": -1}").is_err());
    }

    fn counts(pairs: &[(&str, u64)]) -> Counts {
        pairs.iter().map(|(k, v)| ((*k).to_string(), *v)).collect()
    }

    #[test]
    fn compare_rules() {
        let base = counts(&[("a", 5), ("b", 3), ("gone", 1)]);
        let cur = counts(&[("a", 6), ("b", 2), ("new", 4)]);
        let (f, n) = compare(&base, &cur);
        assert_eq!(
            f,
            ["a unsafe count 5 -> 6", "new crate with unsafe: new (4)"]
        );
        assert_eq!(n.len(), 2);
        assert!(n[0].starts_with("note b unsafe count fell 3 -> 2"));
        assert!(n[1].starts_with("note baseline crate no longer present: gone (1)"));
        let (f, n) = compare(&base, &base);
        assert!(f.is_empty() && n.is_empty());
    }

    #[test]
    fn audit_dir_counts_rust_files_only() {
        let fx = Fixture::new("unsafe-audit");
        fx.write("v/a-1.0.0/src/lib.rs", "unsafe fn f() {}\nunsafe { }\n")
            .write("v/a-1.0.0/src/deep/x.rs", "unsafe impl X for Y {}")
            .write("v/a-1.0.0/README.md", "unsafe unsafe")
            .write("v/b-1.0.0/src/lib.rs", "fn safe() {}")
            .write("v/stray-file.rs", "unsafe");
        let c = audit_dir(&fx.path().join("v"));
        assert_eq!(c, counts(&[("a-1.0.0", 3)]));
        assert!(audit_dir(&fx.path().join("missing")).is_empty());
    }
}
