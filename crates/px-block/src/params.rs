//! Tracking query-parameter stripping.
//!
//! A pure function from a URL and a configurable name list to a (possibly)
//! cleaned URL. Applied by the integration to navigations; this crate does no
//! I/O.
//!
//! # Contract
//!
//! - Only `http` and `https` URLs are touched.
//! - Only whole `name[=value]` pairs separated by `&` are removed. Every
//!   remaining pair is preserved byte-for-byte and in order; the scheme,
//!   host, port, path and fragment are untouched. If no pair remains the `?`
//!   is dropped too. So the result is *the original minus those parameters*,
//!   and a navigation to it differs from the original only by their absence.
//! - The result is re-parsed; if it does not parse, or any component other
//!   than the query differs, `None` (leave the URL alone) is returned.
//! - Idempotent: stripping a stripped URL changes nothing.
//! - `;`-separated queries are not split (the `&` form is what trackers use).
//! - Names are compared after percent-decoding, ASCII case-insensitively.
//!
//! # The built-in list is deliberately conservative
//!
//! Only parameters whose sole purpose is cross-site attribution: the `utm_`
//! family and the click identifiers of the large ad and mail platforms.
//! Generic names that sites also use functionally (`ref`, `source`, `id`,
//! `s`, `campaign`) are NOT in it, because removing them breaks pages.

use px_websec::origin::Url;
use std::collections::BTreeSet;

/// Most entries (exact + prefix) a configuration may hold.
pub const MAX_ENTRIES: usize = 1024;
/// Longest entry.
pub const MAX_ENTRY_BYTES: usize = 128;

/// Built-in exact names (lower case).
pub const DEFAULT_EXACT: &[&str] = &[
    "fbclid",
    "gclid",
    "gclsrc",
    "dclid",
    "gbraid",
    "wbraid",
    "msclkid",
    "twclid",
    "ttclid",
    "yclid",
    "igshid",
    "mc_eid",
    "mc_cid",
    "_hsenc",
    "_hsmi",
    "mkt_tok",
    "oly_anon_id",
    "oly_enc_id",
    "vero_id",
    "vero_conv",
    "wickedid",
    "srsltid",
];

/// Built-in prefixes (lower case).
pub const DEFAULT_PREFIXES: &[&str] = &["utm_"];

/// The set of parameter names to strip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackingParams {
    exact: BTreeSet<Box<str>>,
    prefixes: BTreeSet<Box<str>>,
}

impl Default for TrackingParams {
    fn default() -> Self {
        TrackingParams {
            exact: DEFAULT_EXACT.iter().map(|s| (*s).into()).collect(),
            prefixes: DEFAULT_PREFIXES.iter().map(|s| (*s).into()).collect(),
        }
    }
}

impl TrackingParams {
    /// A configuration that strips nothing.
    pub fn empty() -> Self {
        TrackingParams {
            exact: BTreeSet::new(),
            prefixes: BTreeSet::new(),
        }
    }

    fn admit(&self, name: &str) -> Option<String> {
        let n = name.trim().to_ascii_lowercase();
        let ok = !n.is_empty()
            && n.len() <= MAX_ENTRY_BYTES
            && !n
                .bytes()
                .any(|b| b <= 0x20 || b == b'&' || b == b'=' || b == b'#' || b == 0x7f)
            && self.exact.len() + self.prefixes.len() < MAX_ENTRIES;
        ok.then_some(n)
    }

    /// Strip parameters named exactly `name`. `false` if refused (empty,
    /// contains `& = #` or whitespace, too long, or the table is full).
    pub fn add_exact(&mut self, name: &str) -> bool {
        match self.admit(name) {
            Some(n) => {
                self.exact.insert(n.into());
                true
            }
            None => false,
        }
    }

    /// Strip parameters whose name starts with `prefix`.
    pub fn add_prefix(&mut self, prefix: &str) -> bool {
        match self.admit(prefix) {
            Some(n) => {
                self.prefixes.insert(n.into());
                true
            }
            None => false,
        }
    }

    /// Stop stripping `name` (exact entries only).
    pub fn remove_exact(&mut self, name: &str) -> bool {
        self.exact.remove(name.to_ascii_lowercase().as_str())
    }

    /// Stop stripping a prefix.
    pub fn remove_prefix(&mut self, prefix: &str) -> bool {
        self.prefixes.remove(prefix.to_ascii_lowercase().as_str())
    }

    /// Whether a (decoded) parameter name is stripped.
    pub fn is_tracking(&self, name: &str) -> bool {
        let n = name.to_ascii_lowercase();
        self.exact.contains(n.as_str()) || self.prefixes.iter().any(|p| n.starts_with(&**p))
    }
}

/// A successful strip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stripped {
    /// The cleaned URL.
    pub url: Url,
    /// The removed parameter names, as written, in order.
    pub removed: Vec<String>,
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn decode_name(raw: &str) -> String {
    let b = raw.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while let Some(&c) = b.get(i) {
        if c == b'%'
            && let (Some(h), Some(l)) = (
                b.get(i + 1).copied().and_then(hex),
                b.get(i + 2).copied().and_then(hex),
            )
        {
            out.push(h * 16 + l);
            i += 3;
            continue;
        }
        out.push(if c == b'+' { b' ' } else { c });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Strip tracking parameters from `url`. `None` means "leave it unchanged"
/// (nothing to strip, not http(s), or the result would not be a faithful
/// valid URL).
pub fn strip_tracking_params(url: &Url, params: &TrackingParams) -> Option<Stripped> {
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    let query = url.query()?;
    let mut kept: Vec<&str> = Vec::new();
    let mut removed: Vec<String> = Vec::new();
    for pair in query.split('&') {
        let name = pair.split_once('=').map_or(pair, |(n, _)| n);
        if !name.is_empty() && params.is_tracking(&decode_name(name)) {
            removed.push(name.to_string());
        } else {
            kept.push(pair);
        }
    }
    if removed.is_empty() {
        return None;
    }
    let full = url.serialize(false);
    let (before_frag, frag) = match full.split_once('#') {
        Some((a, f)) => (a, Some(f)),
        None => (full.as_str(), None),
    };
    let base = before_frag.split_once('?').map_or(before_frag, |(b, _)| b);
    let mut out = String::with_capacity(full.len());
    out.push_str(base);
    if !kept.is_empty() {
        out.push('?');
        out.push_str(&kept.join("&"));
    }
    if let Some(f) = frag {
        out.push('#');
        out.push_str(f);
    }
    let cleaned = Url::parse(&out)?;
    let faithful = cleaned.scheme() == url.scheme()
        && cleaned.host() == url.host()
        && cleaned.port() == url.port()
        && cleaned.path() == url.path()
        && cleaned.fragment() == url.fragment()
        && cleaned.username() == url.username()
        && cleaned.password() == url.password();
    faithful.then_some(Stripped {
        url: cleaned,
        removed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip(s: &str) -> Option<String> {
        let u = Url::parse(s).unwrap();
        strip_tracking_params(&u, &TrackingParams::default()).map(|r| r.url.serialize(false))
    }

    #[test]
    fn removes_utm_and_click_ids() {
        assert_eq!(
            strip("https://a.com/p?utm_source=x&id=7&fbclid=abc&utm_medium=m").as_deref(),
            Some("https://a.com/p?id=7")
        );
        assert_eq!(
            strip("https://a.com/p?gclid=1").as_deref(),
            Some("https://a.com/p")
        );
        assert_eq!(
            strip("https://a.com/?mc_eid=1&mc_cid=2#top").as_deref(),
            Some("https://a.com/#top")
        );
    }

    #[test]
    fn leaves_other_parameters_byte_for_byte() {
        assert_eq!(
            strip("https://a.com/p?b=%41%20c&utm_id=1&a=1+2&&z").as_deref(),
            Some("https://a.com/p?b=%41%20c&a=1+2&&z")
        );
        assert_eq!(strip("https://a.com/p?ref=home&source=x&id=3"), None);
        assert_eq!(strip("https://a.com/p"), None);
        assert_eq!(strip("https://a.com/p?"), None);
    }

    #[test]
    fn case_and_percent_encoding_of_names() {
        assert_eq!(
            strip("https://a.com/?GCLID=1&UTM_Source=2&%75tm_x=3&utm%5Fy=4&keep=1").as_deref(),
            Some("https://a.com/?keep=1")
        );
        // `utm` alone and `xutm_a` are not tracking names.
        assert_eq!(strip("https://a.com/?utm=1&xutm_a=1"), None);
    }

    #[test]
    fn value_containing_tracker_name_is_not_a_param() {
        assert_eq!(strip("https://a.com/?next=%2F%3Futm_source%3Dx&k=1"), None);
        assert_eq!(strip("https://a.com/?q=fbclid"), None);
    }

    #[test]
    fn only_http_and_https() {
        assert_eq!(strip("ftp://a.com/?utm_source=1"), None);
        assert_eq!(strip("mailto:x@y.com?utm_source=1"), None);
    }

    #[test]
    fn rest_of_url_is_untouched() {
        let u = Url::parse("https://u:p@a.com:8443/a/b;c?utm_source=1&k=v#f?utm_x=1").unwrap();
        let r = strip_tracking_params(&u, &TrackingParams::default()).unwrap();
        assert_eq!(
            r.url.serialize(false),
            "https://u:p@a.com:8443/a/b;c?k=v#f?utm_x=1"
        );
        assert_eq!(r.removed, ["utm_source"]);
        assert_eq!(r.url.path(), u.path());
        assert_eq!(r.url.fragment(), u.fragment());
    }

    #[test]
    fn idempotent() {
        for s in [
            "https://a.com/?utm_a=1&b=2&gclid=3",
            "https://a.com/?utm_a=1",
            "https://a.com/?b=2",
            "http://a.com/x?&&utm_a=1&&",
        ] {
            let u = Url::parse(s).unwrap();
            if let Some(r) = strip_tracking_params(&u, &TrackingParams::default()) {
                assert_eq!(
                    strip_tracking_params(&r.url, &TrackingParams::default()),
                    None,
                    "{s}"
                );
            }
        }
    }

    #[test]
    fn empty_segments_survive() {
        assert_eq!(
            strip("http://a.com/x?&&utm_a=1&&").as_deref(),
            Some("http://a.com/x?&&&")
        );
    }

    #[test]
    fn configuration() {
        let mut p = TrackingParams::empty();
        let u = Url::parse("https://a.com/?utm_source=1&x_trk=2&sid=3").unwrap();
        assert_eq!(strip_tracking_params(&u, &p), None);
        assert!(p.add_prefix("x_"));
        assert!(p.add_exact("SID"));
        let r = strip_tracking_params(&u, &p).unwrap();
        assert_eq!(r.url.serialize(false), "https://a.com/?utm_source=1");
        assert!(p.remove_exact("sid"));
        assert!(p.remove_prefix("x_"));
        assert_eq!(strip_tracking_params(&u, &p), None);
        // Refusals.
        assert!(!p.add_exact(""));
        assert!(!p.add_exact("a&b"));
        assert!(!p.add_exact("a=b"));
        assert!(!p.add_exact(&"a".repeat(MAX_ENTRY_BYTES + 1)));
        let mut full = TrackingParams::empty();
        for i in 0..MAX_ENTRIES {
            assert!(full.add_exact(&format!("p{i}")));
        }
        assert!(!full.add_exact("one-more"));
    }

    #[test]
    fn default_list_is_conservative() {
        let p = TrackingParams::default();
        for functional in [
            "ref", "source", "id", "s", "campaign", "q", "page", "token", "utm",
        ] {
            assert!(!p.is_tracking(functional), "{functional}");
        }
        for t in ["utm_source", "fbclid", "gclid", "mc_eid", "msclkid"] {
            assert!(p.is_tracking(t));
        }
    }

    #[test]
    fn hostile_queries_do_not_panic() {
        let p = TrackingParams::default();
        for q in [
            "%",
            "%zz",
            "%7",
            "&",
            "=",
            "==&=&",
            "utm_%",
            "\u{1F600}=1&utm_a=%F0%9F",
            "a=1&utm_a=1&utm_b=2&utm_c=3&utm_d=4",
        ] {
            if let Some(u) = Url::parse(&format!("https://a.com/?{q}")) {
                let _ = strip_tracking_params(&u, &p);
            }
        }
    }
}
