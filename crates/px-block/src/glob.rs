//! Adblock-style URL patterns and their matcher.
//!
//! A pattern is a sequence of [`Elem`]s -- literal bytes, the separator class
//! `^`, and the wildcard `*` -- plus a start anchor (`|`, `||`) and an end
//! anchor (`|`).
//!
//! # Separator semantics
//!
//! `^` matches exactly one byte that is *not* an ASCII letter, an ASCII digit,
//! or one of `_ - . %`, or it matches the end of the address (consuming
//! nothing). The set is the ASCII punctuation/control range
//! `00-24 26-2C 2F 3A-40 5B-5E 60 7B-7F` (the class the Adblock Plus core
//! compiles `^` to). Bytes `>= 0x80` are *not* separators: a non-ASCII byte
//! is part of a word.
//!
//! # Host anchor
//!
//! `||` matches at the start of the hostname or immediately after any `.`
//! inside the hostname -- never inside the path, query or userinfo. So
//! `||example.com^` matches `https://sub.example.com/` and does not match
//! `https://evil.net/?u=https://example.com/` or `https://notexample.com/`.
//! (Without the closing `^`, `||example.com` does match
//! `https://example.com.evil.net/`; that is the documented Adblock Plus
//! behaviour and list authors write the `^`.)
//!
//! # Complexity
//!
//! Matching splits the pattern at `*` and finds each segment leftmost, left to
//! right, never revisiting earlier text. There is no backtracking into an
//! earlier segment (which is what makes naive glob matchers exponential or
//! quadratic in the number of stars). Each text position is tried at most
//! once per segment scan and scans cover disjoint ranges, so the cost is
//! `O(text_len * longest_segment)` with
//! `longest_segment <= MAX_PATTERN_BYTES`; allocation-free.

use std::fmt;

/// Longest accepted pattern body, in bytes, before wildcard collapsing.
pub const MAX_PATTERN_BYTES: usize = 512;

/// Shortest index token. One-byte tokens are too common to discriminate.
pub const MIN_TOKEN_BYTES: usize = 2;

/// Longest index token. URL runs longer than this are skipped on lookup and
/// pattern runs longer than this are not used as index keys.
pub const MAX_TOKEN_BYTES: usize = 64;

/// One element of a compiled pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Elem {
    /// A literal byte (already lower-cased unless the rule is `match-case`).
    Lit(u8),
    /// `^`.
    Sep,
    /// `*`.
    Star,
}

/// Where a pattern must begin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartAnchor {
    /// Anywhere in the URL.
    Anywhere,
    /// `|`: at the start of the URL.
    Start,
    /// `||`: at the start of the hostname or after a `.` inside it.
    Host,
}

/// Why a pattern text could not be compiled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatternError {
    /// Longer than [`MAX_PATTERN_BYTES`].
    TooLong,
    /// A control byte, a space, or DEL.
    InvalidCharacter,
}

impl fmt::Display for PatternError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLong => write!(f, "pattern longer than {MAX_PATTERN_BYTES} bytes"),
            Self::InvalidCharacter => write!(f, "pattern contains a control byte or space"),
        }
    }
}

impl std::error::Error for PatternError {}

/// A compiled pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern {
    elems: Box<[Elem]>,
    start: StartAnchor,
    end: bool,
}

/// Whether `b` is matched by `^` (the end of the address is handled by the
/// matcher, not here).
pub fn is_separator(b: u8) -> bool {
    matches!(
        b,
        0x00..=0x24 | 0x26..=0x2C | 0x2F | 0x3A..=0x40 | 0x5B..=0x5E | 0x60 | 0x7B..=0x7F
    )
}

/// Bytes that make up index tokens.
pub fn is_token_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'%'
}

fn is_star(e: &Elem) -> bool {
    matches!(e, Elem::Star)
}

impl Pattern {
    /// Compile the text of an Adblock pattern (anchors, `*`, `^`, literals).
    ///
    /// `match_case == false` lower-cases ASCII letters in the pattern; the
    /// caller then matches against a lower-cased URL.
    pub fn from_text(text: &str, match_case: bool) -> Result<Pattern, PatternError> {
        let mut body = text;
        let mut start = StartAnchor::Anywhere;
        if let Some(rest) = body.strip_prefix("||") {
            start = StartAnchor::Host;
            body = rest;
        } else if let Some(rest) = body.strip_prefix('|') {
            start = StartAnchor::Start;
            body = rest;
        }
        let mut end = false;
        if let Some(rest) = body.strip_suffix('|')
            && !rest.is_empty()
        {
            end = true;
            body = rest;
        }
        if body.len() > MAX_PATTERN_BYTES {
            return Err(PatternError::TooLong);
        }
        let mut elems = Vec::with_capacity(body.len());
        for &b in body.as_bytes() {
            if b <= 0x20 || b == 0x7F {
                return Err(PatternError::InvalidCharacter);
            }
            elems.push(match b {
                b'*' => Elem::Star,
                b'^' => Elem::Sep,
                _ if match_case => Elem::Lit(b),
                _ => Elem::Lit(b.to_ascii_lowercase()),
            });
        }
        Ok(Self::new(elems, start, end))
    }

    /// Build from elements, normalising: runs of `*` collapse, and a `*` at a
    /// free edge is dropped (an unanchored edge is already a wildcard).
    pub fn new(elems: Vec<Elem>, mut start: StartAnchor, mut end: bool) -> Pattern {
        let mut out: Vec<Elem> = Vec::with_capacity(elems.len());
        for e in elems {
            if is_star(&e) && out.last().is_some_and(is_star) {
                continue;
            }
            out.push(e);
        }
        if out.first().is_some_and(is_star) && start != StartAnchor::Host {
            out.remove(0);
            start = StartAnchor::Anywhere;
        }
        if out.last().is_some_and(is_star) {
            out.pop();
            end = false;
        }
        Pattern {
            elems: out.into_boxed_slice(),
            start,
            end,
        }
    }

    /// The compiled elements.
    pub fn elems(&self) -> &[Elem] {
        &self.elems
    }

    /// The start anchor.
    pub fn start_anchor(&self) -> StartAnchor {
        self.start
    }

    /// Whether the pattern must end at the end of the URL.
    pub fn end_anchored(&self) -> bool {
        self.end
    }

    /// Whether the pattern contains a literal byte. A pattern without one
    /// (empty, `*`, `^`, `||`) constrains nothing about the URL's text.
    pub fn has_literal(&self) -> bool {
        self.elems.iter().any(|e| matches!(e, Elem::Lit(_)))
    }

    /// Match `text`. `host` is the byte range of the hostname inside `text`
    /// (needed only for `||`; a `||` pattern against a text without a host
    /// never matches).
    pub fn matches(&self, text: &[u8], host: Option<(usize, usize)>) -> bool {
        let len = text.len();
        let mut segs = self.elems.split(is_star).peekable();
        let mut cur = 0usize;
        let mut first = true;
        while let Some(seg) = segs.next() {
            let last = segs.peek().is_none();
            if first && self.start != StartAnchor::Anywhere {
                let need_end = if last && self.end { Some(len) } else { None };
                let found = match self.start {
                    StartAnchor::Start => {
                        match_at(seg, text, 0).filter(|e| need_end.is_none_or(|n| *e == n))
                    }
                    StartAnchor::Host => match host {
                        Some((hs, he)) => match_at_host(seg, text, hs, he, need_end),
                        None => None,
                    },
                    StartAnchor::Anywhere => None,
                };
                match found {
                    Some(e) => cur = e,
                    None => return false,
                }
            } else if last && self.end {
                if !seg.is_empty() {
                    let lo = cur.max(len.saturating_sub(seg.len()));
                    if !(lo..=len).any(|p| match_at(seg, text, p) == Some(len)) {
                        return false;
                    }
                }
                cur = len;
            } else {
                let found = (cur..=len).find_map(|p| match_at(seg, text, p));
                match found {
                    Some(e) => cur = e,
                    None => return false,
                }
            }
            first = false;
        }
        true
    }

    /// Candidate index tokens: maximal runs of `[a-z0-9%]` literals of length
    /// `MIN_TOKEN_BYTES..=MAX_TOKEN_BYTES` that are *bounded* on both sides by
    /// something that cannot be part of a longer URL run -- a separator, a
    /// non-token literal, or an anchor that pins the run's edge. A run next to
    /// a `*` or at a free edge could be a fragment of a longer URL run, so it
    /// is not a valid key. Lower-cased, in pattern order, deduplicated.
    pub fn index_tokens(&self) -> Vec<String> {
        let n = self.elems.len();
        let tok = |i: usize| matches!(self.elems.get(i), Some(Elem::Lit(b)) if is_token_byte(*b));
        let mut out: Vec<String> = Vec::new();
        let mut i = 0;
        while i < n {
            if !tok(i) {
                i += 1;
                continue;
            }
            let mut j = i;
            while tok(j) {
                j += 1;
            }
            let left_ok = if i == 0 {
                self.start != StartAnchor::Anywhere
            } else {
                matches!(self.elems.get(i - 1), Some(Elem::Lit(_) | Elem::Sep))
            };
            let right_ok = if j == n {
                self.end
            } else {
                matches!(self.elems.get(j), Some(Elem::Lit(_) | Elem::Sep))
            };
            let run = j - i;
            if left_ok && right_ok && (MIN_TOKEN_BYTES..=MAX_TOKEN_BYTES).contains(&run) {
                let bytes: Vec<u8> = self
                    .elems
                    .get(i..j)
                    .unwrap_or(&[])
                    .iter()
                    .filter_map(|e| match e {
                        Elem::Lit(b) => Some(b.to_ascii_lowercase()),
                        _ => None,
                    })
                    .collect();
                if let Ok(s) = String::from_utf8(bytes)
                    && !out.contains(&s)
                {
                    out.push(s);
                }
            }
            i = j;
        }
        out
    }
}

/// Match a star-free segment at exactly `p`; returns the end position.
fn match_at(seg: &[Elem], text: &[u8], p: usize) -> Option<usize> {
    let mut pos = p;
    for e in seg {
        match e {
            Elem::Lit(b) => {
                if text.get(pos) == Some(b) {
                    pos += 1;
                } else {
                    return None;
                }
            }
            Elem::Sep => match text.get(pos) {
                Some(&c) if is_separator(c) => pos += 1,
                Some(_) => return None,
                // End of the address: `^` matches, consuming nothing.
                None => {}
            },
            Elem::Star => return None,
        }
    }
    Some(pos)
}

/// First host boundary (`hs`, or just after a `.` in `hs..he`) at which `seg`
/// matches (and ends at `need_end`, if given).
fn match_at_host(
    seg: &[Elem],
    text: &[u8],
    hs: usize,
    he: usize,
    need_end: Option<usize>,
) -> Option<usize> {
    let mut p = hs;
    loop {
        if let Some(e) = match_at(seg, text, p)
            && need_end.is_none_or(|n| e == n)
        {
            return Some(e);
        }
        let rel = text.get(p..he)?.iter().position(|&b| b == b'.')?;
        p = p + rel + 1;
    }
}

/// Distinct index tokens of a lower-cased URL, sorted. Runs outside
/// `MIN_TOKEN_BYTES..=MAX_TOKEN_BYTES` are skipped.
pub fn url_tokens(lower: &str) -> Vec<&str> {
    let bytes = lower.as_bytes();
    let mut out: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if !bytes.get(i).is_some_and(|b| is_token_byte(*b)) {
            i += 1;
            continue;
        }
        let mut j = i;
        while bytes.get(j).is_some_and(|b| is_token_byte(*b)) {
            j += 1;
        }
        if (MIN_TOKEN_BYTES..=MAX_TOKEN_BYTES).contains(&(j - i))
            && let Some(t) = lower.get(i..j)
        {
            out.push(t);
        }
        i = j;
    }
    out.sort_unstable();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Match `pat` (ABP text) against `url`, computing the host range the way
    /// the matcher does: after `://`, up to the first `/ ? #` or `:port`.
    fn m(pat: &str, url: &str) -> bool {
        let p = Pattern::from_text(pat, false).unwrap();
        let lower = url.to_ascii_lowercase();
        let host = lower.find("://").map(|i| {
            let hs = i + 3;
            let rest = &lower[hs..];
            let he = rest
                .find(['/', '?', '#', ':'])
                .map_or(lower.len(), |k| hs + k);
            (hs, he)
        });
        p.matches(lower.as_bytes(), host)
    }

    #[test]
    fn separator_class_is_exactly_the_abp_set() {
        // Hand-derived from "anything but a letter, digit, _ - . %".
        for b in 0u8..=255 {
            let word = b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b'%');
            let expect = b < 0x80 && !word;
            assert_eq!(is_separator(b), expect, "byte {b:#04x}");
        }
        for sep in *b"/:?=&#@~+" {
            assert!(is_separator(sep));
        }
        for non in [b'a', b'Z', b'7', b'_', b'-', b'.', b'%', 0x80, 0xC3] {
            assert!(!is_separator(non));
        }
    }

    #[test]
    fn caret_matches_separator_or_end() {
        assert!(m("foo^", "http://a.com/foo/bar"));
        assert!(m("foo^", "http://a.com/foo?x"));
        assert!(m("foo^", "http://a.com/foo")); // end of address
        assert!(!m("foo^", "http://a.com/foo.bar")); // '.' is not a separator
        assert!(!m("foo^", "http://a.com/foo-bar"));
        assert!(!m("foo^", "http://a.com/foo_bar"));
        assert!(!m("foo^", "http://a.com/foo%20"));
        assert!(!m("foo^", "http://a.com/foobar"));
        // Several carets at the end of the address all match nothing.
        assert!(m("foo^^", "http://a.com/foo"));
        // ... but a literal can not follow the end of the address.
        assert!(!m("foo^x", "http://a.com/foo"));
        // Non-ASCII is not a separator.
        assert!(!m("foo^", "http://a.com/foo\u{e9}"));
    }

    #[test]
    fn host_anchor_boundaries() {
        assert!(m("||example.com^", "https://example.com/"));
        assert!(m("||example.com^", "https://sub.example.com/x"));
        assert!(m("||example.com^", "https://a.b.example.com:8080/x"));
        assert!(m("||example.com^", "https://example.com"));
        assert!(!m("||example.com^", "https://notexample.com/"));
        assert!(!m("||example.com^", "https://example.com.evil.net/"));
        assert!(!m(
            "||example.com^",
            "https://evil.net/?u=https://example.com/"
        ));
        assert!(!m("||example.com^", "https://evil.net/example.com/"));
        // No closing caret: ABP matches the longer host as well.
        assert!(m("||example.com", "https://example.com.evil.net/"));
        // Host anchor with a path.
        assert!(m("||example.com/ads/", "https://www.example.com/ads/x"));
        assert!(!m(
            "||example.com/ads/",
            "https://www.example.com/news/ads/x"
        ));
    }

    #[test]
    fn start_and_end_anchors() {
        assert!(m("|https://", "https://a.com/"));
        assert!(!m("|https://", "http://a.com/?u=https://"));
        assert!(m("|http://a.com/x|", "http://a.com/x"));
        assert!(!m("|http://a.com/x|", "http://a.com/x/"));
        assert!(m(".swf|", "http://a.com/movie.swf"));
        assert!(!m(".swf|", "http://a.com/movie.swf?x=1"));
        assert!(m("|http://a.com/|", "http://a.com/"));
        assert!(!m("|http://a.com/|", "http://a.com/x"));
    }

    #[test]
    fn wildcards() {
        assert!(m("/ad*banner", "http://a.com/ad/big/banner.gif"));
        assert!(m("/ad*banner", "http://a.com/adbanner"));
        assert!(!m("/ad*banner", "http://a.com/ad/bann"));
        assert!(m("a*b*c", "xx-a--b--c-yy"));
        assert!(!m("a*b*c", "xx-a--c--b-yy"));
        // Leftmost segment placement must not hide a later match.
        assert!(m("ab*abc", "ababc"));
        assert!(m("a*a*a*b", "aaaaaaab"));
        assert!(!m("a*a*a*b", "aaaaaaa"));
        // Anchored on both ends with a star in the middle.
        assert!(m("|http*.gif|", "http://a.com/x.gif"));
        assert!(!m("|http*.gif|", "http://a.com/x.gif?1"));
        // Leading/trailing star are no-ops.
        assert!(m("*banner*", "http://a.com/banner"));
        assert!(m("||a.com*banner", "http://a.com/x/banner"));
    }

    #[test]
    fn case_folding() {
        assert!(m("/BANNER/", "http://a.com/banner/"));
        let p = Pattern::from_text("/Banner/", true).unwrap();
        assert!(p.matches(b"http://a.com/Banner/", None));
        assert!(!p.matches(b"http://a.com/banner/", None));
    }

    #[test]
    fn host_anchor_without_host_never_matches() {
        let p = Pattern::from_text("||a.com", false).unwrap();
        assert!(!p.matches(b"data:a.com", None));
    }

    #[test]
    fn normalisation() {
        let p = Pattern::from_text("***a***b***", false).unwrap();
        assert_eq!(
            p.elems(),
            &[Elem::Lit(b'a'), Elem::Star, Elem::Lit(b'b')][..]
        );
        assert_eq!(p.start_anchor(), StartAnchor::Anywhere);
        assert!(!p.end_anchored());
        // `|*x` is the same as `x`.
        let q = Pattern::from_text("|*x", false).unwrap();
        assert_eq!(q.start_anchor(), StartAnchor::Anywhere);
        // A lone `|` has a body of nothing: start anchor, no end anchor.
        let r = Pattern::from_text("|", false).unwrap();
        assert_eq!(r.start_anchor(), StartAnchor::Start);
        assert!(!r.end_anchored());
    }

    #[test]
    fn rejects_bad_text() {
        assert_eq!(
            Pattern::from_text("a b", false),
            Err(PatternError::InvalidCharacter)
        );
        assert_eq!(
            Pattern::from_text("a\0b", false),
            Err(PatternError::InvalidCharacter)
        );
        let long = "a".repeat(MAX_PATTERN_BYTES + 1);
        assert_eq!(Pattern::from_text(&long, false), Err(PatternError::TooLong));
        let ok = "a".repeat(MAX_PATTERN_BYTES);
        assert!(Pattern::from_text(&ok, false).is_ok());
    }

    #[test]
    fn tokens_are_bounded_runs_only() {
        let t = |s: &str| Pattern::from_text(s, false).unwrap().index_tokens();
        // Host-anchored, caret-closed: both labels are exact URL runs.
        assert_eq!(t("||ads.example.com^"), ["ads", "example", "com"]);
        // Right edge free: "ads" could be a prefix of "adserver".
        assert!(t("||ads").is_empty());
        // Left edge free.
        assert!(t("ads^").is_empty());
        // Next to a star (and the one-byte "x" is too weak).
        assert!(t("/ads*/x/").is_empty());
        assert_eq!(t("/ads*/banner/"), ["banner"]);
        // Interior of a path: bounded by '/'.
        assert_eq!(t("/ads/banner/"), ["ads", "banner"]);
        // Single characters are too weak.
        assert!(t("/a/b/").is_empty());
        // End anchor pins the right edge.
        assert_eq!(t("/track.js|"), ["track", "js"]);
        // Start anchor pins the left edge.
        assert_eq!(t("|https:"), ["https"]);
        // Pattern case does not matter for the key.
        assert_eq!(
            Pattern::from_text("/ADS/", true).unwrap().index_tokens(),
            ["ads"]
        );
    }

    #[test]
    fn url_tokenisation() {
        let toks = url_tokens("https://a.example.com/ads/ads?x=1&yy=%20");
        // Byte order: '%' sorts before letters. One-byte runs are dropped.
        assert_eq!(toks, ["%20", "ads", "com", "example", "https", "yy"]);
        assert!(url_tokens(&"a".repeat(MAX_TOKEN_BYTES + 1)).is_empty());
        assert_eq!(url_tokens(&"a".repeat(MAX_TOKEN_BYTES)).len(), 1);
    }

    #[test]
    fn million_stars_terminate_fast() {
        // Pattern layer: the length cap (the list parser caps line length
        // first, so this is the second line of defence).
        let stars = "*".repeat(1_000_000);
        assert_eq!(
            Pattern::from_text(&stars, false),
            Err(PatternError::TooLong)
        );
        let many = format!("a{}b", "*a".repeat(250)); // 502 bytes, 250 stars
        let p = Pattern::from_text(&many, false).unwrap();
        let text = vec![b'a'; 8192];
        assert!(!p.matches(&text, None)); // no 'b': must terminate
    }

    #[test]
    fn quadratic_shapes_are_bounded() {
        // `aaaa...ab` against `aaaa...`: the worst case for naive search.
        let pat = format!("{}b", "a".repeat(400));
        let p = Pattern::from_text(&pat, false).unwrap();
        let text = vec![b'a'; 8192];
        assert!(!p.matches(&text, None));
    }
}
