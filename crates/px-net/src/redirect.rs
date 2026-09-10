//! Redirect policy: whether to follow, and where to.
//!
//! # Why this is a policy module and not a loop inside `fetch`
//!
//! Following a redirect means making a request the caller did not ask for, to
//! a place the caller has not seen. Every interesting decision lives in that
//! gap: whether the target is in the same partition, whether a downgrade to
//! plaintext is acceptable, whether the chain has become a loop, whether
//! anything attached to the first request should survive to the second.
//!
//! A fetch primitive that quietly followed would make all of those silently
//! and identically. So [`crate::fetch`] returns the `Location` and this module
//! decides, and a caller has to hold both to move.
//!
//! # What it refuses
//!
//! - **A downgrade from `https` to `http`.** There is no legitimate redirect
//!   that does this and it is the sslstrip move: send the user to a secure
//!   page, redirect them off it. Refused outright rather than warned about.
//! - **A target that is not representable** — a scheme this crate does not
//!   fetch, a Unicode host, a malformed authority. Refusing is failing closed;
//!   guessing what was meant is how a request lands on the wrong host.
//! - **A chain longer than [`MAX_HOPS`]**, and any repeat of a URL already
//!   visited. Length alone does not catch a two-URL ping-pong.
//!
//! # What it deliberately does not do yet
//!
//! It does not parse general URLs. It resolves a `Location` — absolute, absolute
//! path, or relative path — and nothing else. `url` and `idna` are named in
//! build-spec §3 and would do this properly, but they cost 29 crates, almost
//! all of it ICU4X machinery reached through `idna`. That is the right price
//! for a browser that parses hrefs out of a DOM, and the wrong one for
//! resolving a header field. It comes with its first real consumer, which is
//! the argument ADR 015 made for HTTP/2.
//!
//! The consequence is stated rather than hidden: a `Location` carrying a
//! Unicode host is **refused**, not converted. That fails closed — a redirect
//! that does not happen — and it will be wrong for real sites until the proper
//! parser arrives.

use crate::hsts::PreloadList;
use crate::partition::{Origin, PartitionKey};
use crate::psl::PublicSuffixList;

/// How many redirects to follow before giving up.
///
/// Browsers converge on twenty; this is lower because nothing here yet needs a
/// long chain and a smaller number is easier to defend. It is a bound on work
/// an attacker can make us do, not a compatibility knob.
pub const MAX_HOPS: usize = 10;

/// Why a redirect was not followed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefusedRedirect {
    /// `https` to `http`. The sslstrip move.
    Downgrade,
    /// The `Location` could not be resolved to an origin this crate fetches.
    Unresolvable(&'static str),
    /// The target is on the HSTS preload list and the redirect is plaintext.
    PreloadedHostOverPlaintext,
    /// The chain exceeded [`MAX_HOPS`].
    TooManyHops,
    /// The chain revisited a URL it had already been to.
    Loop,
    /// The top-level site has no registrable domain, so no partition key
    /// exists for the target.
    NotPartitionable,
}

impl std::fmt::Display for RefusedRedirect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Downgrade => write!(f, "refusing a redirect from https to http"),
            Self::Unresolvable(why) => write!(f, "the Location could not be resolved: {why}"),
            Self::PreloadedHostOverPlaintext => {
                write!(
                    f,
                    "the target is HSTS-preloaded and the redirect is plaintext"
                )
            }
            Self::TooManyHops => write!(f, "more than {MAX_HOPS} redirects"),
            Self::Loop => write!(f, "the redirect chain revisited a URL"),
            Self::NotPartitionable => {
                write!(
                    f,
                    "the target has no registrable domain and cannot be partitioned"
                )
            }
        }
    }
}

impl std::error::Error for RefusedRedirect {}

/// Where a followed redirect goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hop {
    /// The key for the next request. Built through the public suffix list, in
    /// the **same top-level site** as the chain started in — a subresource
    /// redirect does not change which page the user is looking at.
    pub key: PartitionKey,
    /// The origin-relative request target.
    pub path: String,
}

/// A redirect chain in progress.
///
/// Holds the history, so a loop is detectable. Created per navigation or per
/// subresource, not shared: two chains sharing a history would refuse each
/// other's legitimate redirects.
#[derive(Debug, Default)]
pub struct Chain {
    visited: Vec<(String, String)>,
}

impl Chain {
    /// A fresh chain.
    pub fn new() -> Self {
        Self::default()
    }

    /// How many hops have been taken.
    pub fn hops(&self) -> usize {
        self.visited.len()
    }

    /// Decide where a redirect goes, or why it does not.
    ///
    /// `top_level_host` is the site the user is looking at, and it does not
    /// change across the chain. `from` is the origin the redirect came from,
    /// and `location` is the header value verbatim.
    pub fn follow(
        &mut self,
        psl: &PublicSuffixList,
        preload: Option<&PreloadList>,
        top_level_host: &str,
        from: &Origin,
        location: &str,
    ) -> Result<Hop, RefusedRedirect> {
        if self.visited.len() >= MAX_HOPS {
            return Err(RefusedRedirect::TooManyHops);
        }

        let (origin, path) = resolve(from, location)?;

        // https -> http, in any form. Checked on the resolved origin rather
        // than by looking for "http://" in the header, because a relative
        // Location inherits the scheme and an absolute one states it.
        if from.is_secure() && !origin.is_secure() {
            return Err(RefusedRedirect::Downgrade);
        }

        if let Some(list) = preload
            && !origin.is_secure()
            && list.requires_https(origin.host())
        {
            return Err(RefusedRedirect::PreloadedHostOverPlaintext);
        }

        // A repeat of somewhere the chain has already been. Length alone does
        // not catch a two-URL ping-pong, which is the common shape.
        let step = (origin.to_string(), path.clone());
        if self.visited.contains(&step) {
            return Err(RefusedRedirect::Loop);
        }

        let key = PartitionKey::new(psl, top_level_host, origin)
            .ok_or(RefusedRedirect::NotPartitionable)?;

        self.visited.push(step);
        Ok(Hop { key, path })
    }
}

/// Resolve a `Location` against the origin it came from.
///
/// Three forms, and nothing else:
///
/// - absolute — `https://host[:port]/path`
/// - absolute path — `/path`
/// - relative path — `path`, resolved against the origin root
///
/// A relative form inherits the scheme, host and port it came from, which is
/// why `from` is required rather than optional.
fn resolve(from: &Origin, location: &str) -> Result<(Origin, String), RefusedRedirect> {
    let location = location.trim();
    if location.is_empty() {
        return Err(RefusedRedirect::Unresolvable("empty"));
    }
    // A control character in a Location is response splitting reaching one
    // step further; the header parser already refuses them, and this refuses
    // again rather than assuming the layer below stayed strict.
    if location.bytes().any(|byte| byte < 0x20 || byte == 0x7f) {
        return Err(RefusedRedirect::Unresolvable("control character"));
    }

    if let Some(rest) = location.strip_prefix("//") {
        // Scheme-relative: inherits the scheme, replaces the authority.
        return resolve_absolute(from.scheme(), rest);
    }

    if let Some((scheme, rest)) = split_scheme(location) {
        let rest = rest
            .strip_prefix("//")
            .ok_or(RefusedRedirect::Unresolvable("authority missing"))?;
        return resolve_absolute(&scheme, rest);
    }

    if let Some(path) = location.strip_prefix('/') {
        return Ok((from.clone(), format!("/{path}")));
    }

    // A bare relative path. Resolved against the root rather than against the
    // request path, because this crate does not carry the request path — a
    // caller that needs true relative resolution has a URL parser by then.
    Ok((from.clone(), format!("/{location}")))
}

/// Split `scheme:` off the front, if the prefix is a valid scheme.
fn split_scheme(location: &str) -> Option<(String, &str)> {
    let colon = location.find(':')?;
    let scheme = location.get(..colon)?;
    if scheme.is_empty() {
        return None;
    }
    // RFC 3986: ALPHA *( ALPHA / DIGIT / "+" / "-" / "." ). A leading digit is
    // not a scheme, which is what stops `1.2.3.4:80/x` being read as one.
    let mut bytes = scheme.bytes();
    if !bytes.next()?.is_ascii_alphabetic() {
        return None;
    }
    if !bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.')) {
        return None;
    }
    Some((scheme.to_ascii_lowercase(), location.get(colon + 1..)?))
}

/// Resolve `authority[/path]` under a known scheme.
fn resolve_absolute(scheme: &str, rest: &str) -> Result<(Origin, String), RefusedRedirect> {
    let (authority, path) = match rest.find('/') {
        Some(at) => (
            rest.get(..at).unwrap_or_default(),
            rest.get(at..).unwrap_or("/").to_owned(),
        ),
        None => (rest, "/".to_owned()),
    };

    // Userinfo is refused rather than stripped. `https://evil@good.example/`
    // is the classic confusion, and a browser that silently drops the userinfo
    // has resolved an ambiguity the user could not see.
    if authority.contains('@') {
        return Err(RefusedRedirect::Unresolvable("userinfo in the authority"));
    }

    let (host, port) = match authority.rfind(':') {
        Some(at) => {
            let host = authority.get(..at).unwrap_or_default();
            let port_text = authority.get(at + 1..).unwrap_or_default();
            let port: u16 = port_text
                .parse()
                .map_err(|_| RefusedRedirect::Unresolvable("port is not a number"))?;
            (host, port)
        }
        None => (
            authority,
            match scheme {
                "https" => 443u16,
                "http" => 80u16,
                _ => return Err(RefusedRedirect::Unresolvable("scheme is not http(s)")),
            },
        ),
    };

    let origin = Origin::new(scheme, host, port).ok_or(RefusedRedirect::Unresolvable(
        "not an origin this crate fetches",
    ))?;
    Ok((origin, path))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn psl() -> Option<PublicSuffixList> {
        PublicSuffixList::parse("com\nnet\n").ok()
    }

    fn secure(host: &str) -> Option<Origin> {
        Origin::new("https", host, 443)
    }

    #[test]
    fn an_absolute_location_moves_origin() {
        let psl = psl().expect("list");
        let from = secure("a.example.com").expect("origin");
        let mut chain = Chain::new();
        let hop = chain
            .follow(
                &psl,
                None,
                "a.example.com",
                &from,
                "https://b.example.net/next",
            )
            .expect("a legitimate redirect");
        assert_eq!(hop.key.origin().host(), "b.example.net");
        assert_eq!(hop.path, "/next");
        assert_eq!(
            hop.key.top_level_site(),
            "example.com",
            "a subresource redirect does not change which page the user is on"
        );
    }

    #[test]
    fn an_absolute_path_keeps_the_origin() {
        let psl = psl().expect("list");
        let from = secure("a.example.com").expect("origin");
        let mut chain = Chain::new();
        let hop = chain
            .follow(&psl, None, "a.example.com", &from, "/elsewhere")
            .expect("valid");
        assert_eq!(hop.key.origin().host(), "a.example.com");
        assert_eq!(hop.path, "/elsewhere");
    }

    #[test]
    fn a_scheme_relative_location_inherits_the_scheme() {
        let psl = psl().expect("list");
        let from = secure("a.example.com").expect("origin");
        let mut chain = Chain::new();
        let hop = chain
            .follow(&psl, None, "a.example.com", &from, "//b.example.net/x")
            .expect("valid");
        assert!(hop.key.origin().is_secure(), "https must be inherited");
    }

    /// The sslstrip move, refused outright.
    #[test]
    fn a_downgrade_to_plaintext_is_refused() {
        let psl = psl().expect("list");
        let from = secure("a.example.com").expect("origin");
        let mut chain = Chain::new();
        assert_eq!(
            chain.follow(&psl, None, "a.example.com", &from, "http://a.example.com/x"),
            Err(RefusedRedirect::Downgrade)
        );
    }

    #[test]
    fn a_preloaded_target_over_plaintext_is_refused() {
        let psl = psl().expect("list");
        let list = PreloadList::parse(".secure.example.net").expect("list");
        // From plaintext, so the downgrade rule does not fire first.
        let from = Origin::new("http", "a.example.com", 80).expect("origin");
        let mut chain = Chain::new();
        assert_eq!(
            chain.follow(
                &psl,
                Some(&list),
                "a.example.com",
                &from,
                "http://x.secure.example.net/y"
            ),
            Err(RefusedRedirect::PreloadedHostOverPlaintext)
        );
    }

    #[test]
    fn a_chain_that_repeats_a_url_is_a_loop() {
        let psl = psl().expect("list");
        let from = secure("a.example.com").expect("origin");
        let mut chain = Chain::new();
        chain
            .follow(&psl, None, "a.example.com", &from, "/one")
            .expect("first hop");
        assert_eq!(
            chain.follow(&psl, None, "a.example.com", &from, "/one"),
            Err(RefusedRedirect::Loop),
            "length alone does not catch a two-URL ping-pong"
        );
    }

    #[test]
    fn a_chain_is_bounded_in_length() {
        let psl = psl().expect("list");
        let from = secure("a.example.com").expect("origin");
        let mut chain = Chain::new();
        for index in 0..MAX_HOPS {
            chain
                .follow(&psl, None, "a.example.com", &from, &format!("/{index}"))
                .expect("within the bound");
        }
        assert_eq!(
            chain.follow(&psl, None, "a.example.com", &from, "/last"),
            Err(RefusedRedirect::TooManyHops)
        );
    }

    /// `https://evil@good.example/` — the classic confusion. Refused rather
    /// than silently stripped.
    #[test]
    fn userinfo_in_the_authority_is_refused() {
        let psl = psl().expect("list");
        let from = secure("a.example.com").expect("origin");
        let mut chain = Chain::new();
        assert_eq!(
            chain.follow(
                &psl,
                None,
                "a.example.com",
                &from,
                "https://evil@good.example.net/"
            ),
            Err(RefusedRedirect::Unresolvable("userinfo in the authority"))
        );
    }

    #[test]
    fn a_scheme_this_crate_does_not_fetch_is_refused() {
        let psl = psl().expect("list");
        let from = secure("a.example.com").expect("origin");
        let mut chain = Chain::new();
        for location in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "data:text/html,x",
            "ftp://example.net/",
        ] {
            assert!(
                chain
                    .follow(&psl, None, "a.example.com", &from, location)
                    .is_err(),
                "{location} must not be followed"
            );
        }
    }

    /// Stated in the module docs and asserted here so it cannot change
    /// silently: a Unicode host is refused, not converted.
    #[test]
    fn a_unicode_host_is_refused_until_there_is_a_real_url_parser() {
        let psl = psl().expect("list");
        let from = secure("a.example.com").expect("origin");
        let mut chain = Chain::new();
        assert!(
            chain
                .follow(&psl, None, "a.example.com", &from, "https://exämple.net/")
                .is_err(),
            "refusing fails closed; converting needs idna, which is not here yet"
        );
    }

    #[test]
    fn a_location_with_a_control_character_is_refused() {
        let psl = psl().expect("list");
        let from = secure("a.example.com").expect("origin");
        let mut chain = Chain::new();
        assert!(
            chain
                .follow(&psl, None, "a.example.com", &from, "/a\rb")
                .is_err()
        );
    }

    /// A port-looking prefix must not be read as a scheme.
    #[test]
    fn a_numeric_prefix_is_not_a_scheme() {
        let (origin, path) = resolve(&secure("a.example.com").expect("origin"), "1.2.3.4:80/x")
            .expect("treated as a relative path");
        assert_eq!(
            origin.host(),
            "a.example.com",
            "a leading digit is not a scheme, so this is a relative path"
        );
        assert!(path.contains("1.2.3.4"));
    }
}
