//! Mixed Content (W3C Mixed Content Level 2) and the "potentially
//! trustworthy" definitions of Secure Contexts.
//!
//! # Deviations
//! - `file:` is potentially trustworthy as Secure Contexts says, so a `file:`
//!   page *does* prohibit mixed content (stricter than some browsers).
//! - Form submissions and top-level navigations are not requests this check
//!   covers (MC2 treats them as UI concerns); callers pass only subresource
//!   and subframe requests.
//! - The settings object's "prohibits mixed security contexts" test is
//!   implemented as: its own origin or any ancestor's origin is potentially
//!   trustworthy.
//! - An `Upgrade` verdict carries no fallback: if the https request fails the
//!   load fails, as MC2 requires.

use crate::origin::{Host, Origin, Url, trace};

/// Enforcement points of this module.
pub const ENFORCEMENT_POINTS: &[&str] = &[
    "mixed::trustworthy_origin",
    "mixed::trustworthy_url",
    "mixed::check_request",
    "mixed::upgrade",
    "mixed::check_response",
];

/// Whether `host` (already lowercase) is a loopback name or address.
fn host_is_loopback(host: &Host) -> bool {
    match host {
        Host::Ipv4(a) => a.first() == Some(&127),
        Host::Ipv6(p) => *p == [0, 0, 0, 0, 0, 0, 0, 1],
        Host::Domain(d) => {
            let d = d.trim_end_matches('.');
            d == "localhost" || d.ends_with(".localhost")
        }
        Host::Opaque(_) | Host::Empty => false,
    }
}

/// Potentially trustworthy origin (Secure Contexts §3.1). Opaque origins are
/// never trustworthy here; `file:` URLs are handled by
/// [`is_url_potentially_trustworthy`] and [`ContextNode`] because a `file:`
/// origin is opaque in this crate.
pub fn is_origin_potentially_trustworthy(origin: &Origin) -> bool {
    trace::hit("mixed::trustworthy_origin");
    match origin {
        Origin::Opaque(_) => false,
        Origin::Tuple { scheme, host, .. } => {
            matches!(scheme.as_str(), "https" | "wss") || host_is_loopback(host)
        }
    }
}

/// Potentially trustworthy URL (Secure Contexts §3.2): `about:blank`,
/// `about:srcdoc`, `data:`, `file:`, or a URL whose origin is trustworthy.
pub fn is_url_potentially_trustworthy(url: &Url) -> bool {
    trace::hit("mixed::trustworthy_url");
    match url.scheme() {
        "about" => matches!(url.path(), "blank" | "srcdoc"),
        "data" | "file" => true,
        _ => is_origin_potentially_trustworthy(&Origin::from_url(url)),
    }
}

/// A document in a frame chain: its URL and its (possibly inherited) origin.
#[derive(Debug, Clone)]
pub struct ContextNode {
    /// The document's URL.
    pub url: Url,
    /// The document's origin.
    pub origin: Origin,
}

impl ContextNode {
    /// Whether this document's own origin is potentially trustworthy
    /// (including `file:` documents).
    pub fn is_trustworthy(&self) -> bool {
        is_origin_potentially_trustworthy(&self.origin)
            || (self.origin.is_opaque() && self.url.scheme() == "file")
    }
}

/// Request categories (MC2 §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestKind {
    /// `<img>`, CSS images, favicons.
    Image,
    /// `<audio>`.
    Audio,
    /// `<video>`.
    Video,
    /// `<track>`.
    Track,
    /// `<script>`.
    Script,
    /// Stylesheets.
    Style,
    /// Web fonts.
    Font,
    /// `fetch()`/XHR/EventSource.
    Fetch,
    /// `<iframe>`/`<frame>` navigation.
    Subframe,
    /// `<object>`/`<embed>`.
    Object,
    /// Dedicated or shared worker script.
    Worker,
    /// Service worker script.
    ServiceWorker,
    /// WebSocket.
    WebSocket,
    /// Web app manifest.
    Manifest,
    /// `<link rel=prefetch>` and similar.
    Prefetch,
    /// Image loaded through `srcset`/`<picture>` (initiator `imageset`):
    /// MC2 deliberately does not upgrade it, so it is blockable.
    ImageSet,
    /// `sendBeacon`/`ping`.
    Beacon,
}

/// MC2 category.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    /// Must be blocked.
    Blockable,
    /// May be upgraded, otherwise blocked.
    OptionallyBlockable,
}

/// Categorise a request: images, audio and video are optionally-blockable
/// (MC2 §3.2); everything else — scripts, styles, fonts, fetches, frames,
/// workers, WebSockets, tracks, manifests — is blockable.
pub fn category(kind: RequestKind) -> Category {
    match kind {
        RequestKind::Image | RequestKind::Audio | RequestKind::Video => {
            Category::OptionallyBlockable
        }
        _ => Category::Blockable,
    }
}

/// The environment a request is made from.
#[derive(Debug, Clone)]
pub struct Context {
    /// The requesting document.
    pub document: ContextNode,
    /// Ancestor documents, nearest first. Empty for a top-level document.
    pub ancestors: Vec<ContextNode>,
}

impl Context {
    /// MC2 "does settings prohibit mixed security contexts?".
    pub fn prohibits_mixed(&self) -> bool {
        self.document.is_trustworthy() || self.ancestors.iter().any(ContextNode::is_trustworthy)
    }
}

/// Verdict of [`check_request`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Proceed unchanged.
    Allow,
    /// Rewrite to this https URL and proceed; no fallback to the original.
    Upgrade(Url),
    /// Refuse.
    Block,
}

/// Mixed-content check for a request of `kind` to `url` from `context`.
pub fn check_request(context: &Context, kind: RequestKind, url: &Url) -> Verdict {
    trace::hit("mixed::check_request");
    if !context.prohibits_mixed() || is_url_potentially_trustworthy(url) {
        return Verdict::Allow;
    }
    match category(kind) {
        Category::Blockable => Verdict::Block,
        Category::OptionallyBlockable => {
            trace::hit("mixed::upgrade");
            // MC2 upgrade algorithm: hosts that are IP addresses are not
            // upgraded, and then the blocking step refuses them.
            let ip_host = matches!(url.host(), Some(Host::Ipv4(_) | Host::Ipv6(_)));
            match url.upgraded_to_https() {
                Some(_) if ip_host => Verdict::Block,
                Some(u) => Verdict::Upgrade(u),
                // An insecure URL we cannot upgrade (ws:, ftp:): fail closed.
                None => Verdict::Block,
            }
        }
    }
}

/// "Should response to request be blocked as mixed content?": run on the
/// final response URL (after redirects and service workers), so a redirect
/// to an insecure URL cannot slip past the request-time check.
pub fn check_response(context: &Context, response_url: &Url) -> Verdict {
    trace::hit("mixed::check_response");
    if !context.prohibits_mixed() || is_url_potentially_trustworthy(response_url) {
        Verdict::Allow
    } else {
        Verdict::Block
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }
    fn node(s: &str) -> ContextNode {
        let url = u(s);
        ContextNode {
            origin: Origin::from_url(&url),
            url,
        }
    }
    fn ctx(top: &str) -> Context {
        Context {
            document: node(top),
            ancestors: vec![],
        }
    }

    #[test]
    fn trustworthy_origins() {
        for yes in [
            "https://a.example/",
            "wss://a.example/",
            "http://localhost/",
            "http://LOCALHOST:8080/",
            "http://foo.localhost/",
            "http://localhost./",
            "http://127.0.0.1/",
            "http://127.255.255.254:9/",
            "http://127.1/",
            "http://[::1]/",
            "ws://localhost/",
        ] {
            assert!(
                is_origin_potentially_trustworthy(&Origin::from_url(&u(yes))),
                "{yes}"
            );
        }
        for no in [
            "http://a.example/",
            "ws://a.example/",
            "ftp://a.example/",
            "http://128.0.0.1/",
            "http://10.0.0.1/",
            "http://[::2]/",
            "http://[::ffff:127.0.0.1]/",
            "http://notlocalhost/",
            "http://localhost.example/",
            "http://evil-localhost/",
        ] {
            assert!(
                !is_origin_potentially_trustworthy(&Origin::from_url(&u(no))),
                "{no}"
            );
        }
        assert!(!is_origin_potentially_trustworthy(&Origin::new_opaque()));
    }

    #[test]
    fn trustworthy_urls() {
        for yes in [
            "about:blank",
            "about:srcdoc",
            "data:text/html,x",
            "file:///tmp/x",
            "https://a.example/",
            "http://localhost/x",
            "blob:https://a.example/id",
        ] {
            assert!(is_url_potentially_trustworthy(&u(yes)), "{yes}");
        }
        for no in [
            "http://a.example/",
            "about:config",
            "blob:http://a.example/id",
            "custom://x/",
            "javascript:1",
            "blob:garbage",
        ] {
            assert!(!is_url_potentially_trustworthy(&u(no)), "{no}");
        }
    }

    #[test]
    fn categories() {
        for k in [RequestKind::Image, RequestKind::Audio, RequestKind::Video] {
            assert_eq!(category(k), Category::OptionallyBlockable);
        }
        for k in [
            RequestKind::Track,
            RequestKind::Script,
            RequestKind::Style,
            RequestKind::Font,
            RequestKind::Fetch,
            RequestKind::Subframe,
            RequestKind::Object,
            RequestKind::Worker,
            RequestKind::ServiceWorker,
            RequestKind::WebSocket,
            RequestKind::Manifest,
            RequestKind::Prefetch,
            RequestKind::ImageSet,
            RequestKind::Beacon,
        ] {
            assert_eq!(category(k), Category::Blockable, "{k:?}");
        }
    }

    #[test]
    fn secure_page_blocks_blockable_http() {
        let c = ctx("https://a.example/");
        for k in [
            RequestKind::Script,
            RequestKind::Style,
            RequestKind::Fetch,
            RequestKind::Subframe,
            RequestKind::Font,
            RequestKind::Worker,
            RequestKind::Track,
        ] {
            assert_eq!(
                check_request(&c, k, &u("http://x.example/a")),
                Verdict::Block,
                "{k:?}"
            );
        }
        assert_eq!(
            check_request(&c, RequestKind::WebSocket, &u("ws://x.example/")),
            Verdict::Block
        );
    }

    #[test]
    fn secure_page_upgrades_optionally_blockable() {
        let c = ctx("https://a.example/");
        assert_eq!(
            check_request(&c, RequestKind::Image, &u("http://x.example:80/i.png?q#f")),
            Verdict::Upgrade(u("https://x.example/i.png?q#f"))
        );
        assert_eq!(
            check_request(&c, RequestKind::Video, &u("http://x.example:8080/v")),
            Verdict::Upgrade(u("https://x.example:8080/v"))
        );
        // Un-upgradable insecure scheme: fail closed.
        assert_eq!(
            check_request(&c, RequestKind::Audio, &u("ftp://x.example/a")),
            Verdict::Block
        );
        // IP-address hosts are not upgraded (MC2), so they are blocked.
        assert_eq!(
            check_request(&c, RequestKind::Image, &u("http://203.0.113.5/i.png")),
            Verdict::Block
        );
        assert_eq!(
            check_request(&c, RequestKind::ImageSet, &u("http://x.example/i.png")),
            Verdict::Block
        );
        // Response check catches a redirect target.
        assert_eq!(check_response(&c, &u("http://x.example/")), Verdict::Block);
        assert_eq!(check_response(&c, &u("https://x.example/")), Verdict::Allow);
    }

    #[test]
    fn trustworthy_targets_allowed() {
        let c = ctx("https://a.example/");
        for (k, t) in [
            (RequestKind::Script, "https://x.example/s.js"),
            (RequestKind::Script, "http://localhost:3000/s.js"),
            (RequestKind::Fetch, "http://127.0.0.1/api"),
            (RequestKind::WebSocket, "wss://x.example/"),
            (RequestKind::WebSocket, "ws://localhost/"),
            (RequestKind::Image, "data:image/png;base64,AAAA"),
            (RequestKind::Script, "blob:https://a.example/id"),
        ] {
            assert_eq!(check_request(&c, k, &u(t)), Verdict::Allow, "{t}");
        }
    }

    #[test]
    fn insecure_page_is_not_mixed() {
        let c = ctx("http://a.example/");
        assert_eq!(
            check_request(&c, RequestKind::Script, &u("http://x.example/")),
            Verdict::Allow
        );
        assert_eq!(
            check_request(&c, RequestKind::Image, &u("http://x.example/")),
            Verdict::Allow
        );
    }

    #[test]
    fn insecure_frame_under_secure_ancestor_is_mixed() {
        let c = Context {
            document: node("http://frame.example/"),
            ancestors: vec![node("https://top.example/")],
        };
        assert!(c.prohibits_mixed());
        assert_eq!(
            check_request(&c, RequestKind::Script, &u("http://x.example/")),
            Verdict::Block
        );
        // Insecure all the way up: fine.
        let c2 = Context {
            document: node("http://frame.example/"),
            ancestors: vec![node("http://top.example/")],
        };
        assert_eq!(
            check_request(&c2, RequestKind::Script, &u("http://x.example/")),
            Verdict::Allow
        );
    }

    #[test]
    fn file_pages_prohibit_mixed() {
        let c = ctx("file:///tmp/a.html");
        assert_eq!(
            check_request(&c, RequestKind::Script, &u("http://x.example/")),
            Verdict::Block
        );
        assert_eq!(
            check_request(&c, RequestKind::Script, &u("file:///tmp/b.js")),
            Verdict::Allow
        );
    }

    #[test]
    fn opaque_data_page_is_not_secure() {
        let c = ctx("data:text/html,x");
        assert!(!c.prohibits_mixed());
    }

    #[test]
    fn every_enforcement_point_is_reached() {
        trace::reset();
        let c = ctx("https://a.example/");
        let _ = check_request(&c, RequestKind::Image, &u("http://x.example/"));
        let _ = check_response(&c, &u("http://x.example/"));
        let _ = check_request(&c, RequestKind::Script, &u("http://x.example/"));
        trace::assert_all_hit(ENFORCEMENT_POINTS);
    }

    #[test]
    fn hostile_urls_never_panic() {
        let mut x: u64 = 0xDEAD_BEEF_CAFE_F00D;
        let alphabet = b"htps:/.[]@%0123456789abcxX-\\";
        let c = ctx("https://a.example/");
        for _ in 0..10_000 {
            let mut s = String::new();
            for _ in 0..(x % 50) {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                s.push(alphabet[(x % alphabet.len() as u64) as usize] as char);
            }
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            if let Some(url) = Url::parse(&s) {
                let _ = check_request(&c, RequestKind::Image, &url);
                let _ = check_request(&c, RequestKind::Script, &url);
            }
        }
    }
}
