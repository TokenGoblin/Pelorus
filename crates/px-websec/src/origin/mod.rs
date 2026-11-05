//! The same-origin policy's vocabulary: origins, sites, and how to derive them.
//!
//! Specifications: HTML "Origin" (§7.5), URL Standard "origin" of a URL, and
//! the "site" / "schemelessly same site" definitions in HTML.
//!
//! # Non-goal: `document.domain`
//!
//! build-spec §1 lists `document.domain` as a non-goal. There is therefore no
//! "same origin-domain" comparison here and no way to mutate an origin's
//! domain. An origin is immutable once derived, which removes a whole class
//! of "relaxed" same-origin bypasses.
//!
//! # Deviations
//! - `file:` URLs get an *opaque* origin (HTML allows this and it is the
//!   fail-closed choice: no two files are ever same-origin).
//! - Opaque origins are identified by a process-wide counter, not a random
//!   nonce. Within a process that is unique; they are never serialised across
//!   a process boundary (authority comes from the channel, CLAUDE.md).
//! - Hosts must be pre-IDNA'd ASCII; see [`url`].

pub mod url;

pub mod trace;

use std::sync::atomic::{AtomicU64, Ordering};

pub use url::{Host, Url};

/// Enforcement points of this module.
pub const ENFORCEMENT_POINTS: &[&str] = &[
    "origin::derive_from_url",
    "origin::derive_document",
    "origin::same_origin",
    "origin::same_site",
    "origin::schemelessly_same_site",
    "origin::cross_site_for_cookies",
];

static NEXT_OPAQUE: AtomicU64 = AtomicU64::new(1);

/// A web origin.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Origin {
    /// Unique, equal only to itself. Serialises as `null`.
    Opaque(u64),
    /// `(scheme, host, port)`; port is `None` when it is the scheme default.
    Tuple {
        /// Lowercase special scheme.
        scheme: String,
        /// Host.
        host: Host,
        /// Non-default port.
        port: Option<u16>,
    },
}

impl Origin {
    /// A fresh opaque origin, different from every other.
    pub fn new_opaque() -> Origin {
        // Wrapping is unreachable in practice (2^64 origins); on the off
        // chance, a repeat could only ever *merge* two opaque origins, so we
        // saturate to a reserved id rather than wrap silently.
        let id = NEXT_OPAQUE
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| v.checked_add(1))
            .unwrap_or(u64::MAX);
        Origin::Opaque(id)
    }

    /// Build a tuple origin; `None` unless `scheme` is special and not `file`
    /// and the host is a real host.
    pub fn tuple(scheme: &str, host: Host, port: Option<u16>) -> Option<Origin> {
        let scheme = scheme.to_ascii_lowercase();
        if !url::is_special_scheme(&scheme) || scheme == "file" {
            return None;
        }
        if matches!(host, Host::Empty | Host::Opaque(_)) {
            return None;
        }
        let port = match (port, url::default_port(&scheme)) {
            (Some(p), Some(d)) if p == d => None,
            (p, _) => p,
        };
        Some(Origin::Tuple { scheme, host, port })
    }

    /// The origin of a URL (URL Standard). `blob:` takes its inner URL's
    /// origin when that is http(s); everything else non-special is opaque.
    pub fn from_url(url: &Url) -> Origin {
        trace::hit("origin::derive_from_url");
        match url.scheme() {
            "blob" => {
                if let Some(inner) = Url::parse(url.path())
                    && matches!(inner.scheme(), "http" | "https")
                {
                    return Origin::from_url_inner(&inner);
                }
                Origin::new_opaque()
            }
            _ => Origin::from_url_inner(url),
        }
    }

    fn from_url_inner(url: &Url) -> Origin {
        if url.is_special()
            && url.scheme() != "file"
            && let Some(h) = url.host()
            && let Some(o) = Origin::tuple(url.scheme(), h.clone(), url.port())
        {
            return o;
        }
        Origin::new_opaque()
    }

    /// Origin for a document created at `url`, given the origin of the
    /// document that created/navigated it (`creator`), if any.
    ///
    /// `about:blank` and `about:srcdoc` inherit the creator's origin (HTML
    /// "initial about:blank"); with no creator they are opaque. `data:` is
    /// always opaque, even from a creator. `file:` is opaque (see module
    /// docs). `blob:` follows the blob URL's own origin.
    pub fn for_document(url: &Url, creator: Option<&Origin>) -> Origin {
        trace::hit("origin::derive_document");
        if url.scheme() == "about" && matches!(url.path(), "blank" | "srcdoc") {
            return creator.cloned().unwrap_or_else(Origin::new_opaque);
        }
        Origin::from_url(url)
    }

    /// Whether this is an opaque origin.
    pub fn is_opaque(&self) -> bool {
        matches!(self, Origin::Opaque(_))
    }

    /// The scheme of a tuple origin.
    pub fn scheme(&self) -> Option<&str> {
        match self {
            Origin::Tuple { scheme, .. } => Some(scheme),
            Origin::Opaque(_) => None,
        }
    }

    /// The host of a tuple origin.
    pub fn host(&self) -> Option<&Host> {
        match self {
            Origin::Tuple { host, .. } => Some(host),
            Origin::Opaque(_) => None,
        }
    }

    /// Serialise as `null` or `scheme://host[:port]`.
    pub fn serialize(&self) -> String {
        match self {
            Origin::Opaque(_) => "null".to_string(),
            Origin::Tuple { scheme, host, port } => match port {
                Some(p) => format!("{scheme}://{}:{p}", host.serialize()),
                None => format!("{scheme}://{}", host.serialize()),
            },
        }
    }

    /// Same origin: tuples compare componentwise, opaque only to themselves.
    pub fn same_origin(&self, other: &Origin) -> bool {
        trace::hit("origin::same_origin");
        self == other
    }
}

/// Supplies the registrable domain (eTLD+1) of a host, or `None` when the
/// host has none. px-net's `PublicSuffixList::registrable_domain` is the
/// production implementation; this crate does not depend on px-net.
pub trait SiteResolver {
    /// Registrable domain of `host` (lowercase ASCII), if any.
    fn registrable_domain(&self, host: &str) -> Option<String>;
}

impl<F: Fn(&str) -> Option<String>> SiteResolver for F {
    fn registrable_domain(&self, host: &str) -> Option<String> {
        self(host)
    }
}

/// An origin's site.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Site {
    /// The opaque origin itself.
    Opaque(u64),
    /// `(scheme, registrable domain or host)`.
    Tuple {
        /// Scheme.
        scheme: String,
        /// Registrable domain, or the serialised host when there is none.
        host: String,
    },
}

impl Site {
    /// The site of `origin`.
    pub fn of(origin: &Origin, resolver: &dyn SiteResolver) -> Site {
        match origin {
            Origin::Opaque(id) => Site::Opaque(*id),
            Origin::Tuple { scheme, host, .. } => {
                let ser = host.serialize();
                let h = match host {
                    Host::Domain(d) => resolver.registrable_domain(d).unwrap_or(ser),
                    _ => ser,
                };
                Site::Tuple {
                    scheme: scheme.clone(),
                    host: h,
                }
            }
        }
    }
}

/// Same site (scheme and registrable domain both equal).
pub fn same_site(a: &Origin, b: &Origin, resolver: &dyn SiteResolver) -> bool {
    trace::hit("origin::same_site");
    Site::of(a, resolver) == Site::of(b, resolver)
}

/// Schemelessly same site: as [`same_site`] but schemes may differ. Opaque
/// origins are same-site only with themselves.
pub fn schemelessly_same_site(a: &Origin, b: &Origin, resolver: &dyn SiteResolver) -> bool {
    trace::hit("origin::schemelessly_same_site");
    match (Site::of(a, resolver), Site::of(b, resolver)) {
        (Site::Opaque(x), Site::Opaque(y)) => x == y,
        (Site::Tuple { host: x, .. }, Site::Tuple { host: y, .. }) => x == y,
        _ => false,
    }
}

/// The `cross_site` argument of px-net's `Jar::cookies_for`: true unless the
/// request is same-site with the site-for-cookies. Uses the schemeful rule
/// (stricter), so an http page cannot ride an https site's SameSite cookies.
/// SameSite attribute parsing and filtering stay in px-net's cookie module.
pub fn cross_site_for_cookies(
    site_for_cookies: &Origin,
    request: &Origin,
    resolver: &dyn SiteResolver,
) -> bool {
    trace::hit("origin::cross_site_for_cookies");
    !same_site(site_for_cookies, request, resolver)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }
    fn o(s: &str) -> Origin {
        Origin::from_url(&u(s))
    }
    /// Toy PSL: last two labels, or three under co.uk.
    fn psl(host: &str) -> Option<String> {
        let labels: Vec<&str> = host.split('.').collect();
        let n = if host.ends_with(".co.uk") { 3 } else { 2 };
        if labels.len() < n {
            return None;
        }
        Some(labels[labels.len() - n..].join("."))
    }

    #[test]
    fn same_origin_components() {
        assert!(o("https://a.example/x").same_origin(&o("https://a.example/y?z#f")));
        assert!(!o("https://a.example/").same_origin(&o("http://a.example/")));
        assert!(!o("https://a.example/").same_origin(&o("https://b.example/")));
        assert!(!o("https://a.example/").same_origin(&o("https://a.example:8443/")));
        assert!(!o("https://a.example/").same_origin(&o("https://sub.a.example/")));
    }

    #[test]
    fn default_ports_and_case() {
        assert!(o("https://A.Example:443/").same_origin(&o("https://a.example/")));
        assert!(o("http://a.example:80/").same_origin(&o("http://a.example/")));
        assert!(o("wss://a.example:443/").same_origin(&o("wss://a.example/")));
        assert!(!o("http://a.example:443/").same_origin(&o("http://a.example/")));
        assert_eq!(o("http://a.example:80/").serialize(), "http://a.example");
        assert_eq!(o("http://a.example:81/").serialize(), "http://a.example:81");
    }

    #[test]
    fn ip_origins_compare_numerically() {
        assert!(o("http://127.1/").same_origin(&o("http://127.0.0.1/")));
        assert_eq!(o("http://[::1]:3000/").serialize(), "http://[::1]:3000");
    }

    #[test]
    fn opaque_origins_are_unique() {
        let a = o("data:text/html,x");
        let b = o("data:text/html,x");
        assert!(a.is_opaque() && b.is_opaque());
        assert!(!a.same_origin(&b), "two data: origins are different");
        assert!(a.same_origin(&a.clone()), "an opaque origin equals itself");
        assert_eq!(a.serialize(), "null");
        assert!(!Origin::new_opaque().same_origin(&Origin::new_opaque()));
    }

    #[test]
    fn about_blank_inherits_creator() {
        let creator = o("https://a.example/");
        let d = Origin::for_document(&u("about:blank"), Some(&creator));
        assert!(d.same_origin(&creator));
        let s = Origin::for_document(&u("about:srcdoc"), Some(&creator));
        assert!(s.same_origin(&creator));
        let none = Origin::for_document(&u("about:blank"), None);
        assert!(none.is_opaque());
        // Anything else about: is opaque.
        let other = Origin::for_document(&u("about:config"), Some(&creator));
        assert!(other.is_opaque());
    }

    #[test]
    fn data_is_opaque_even_with_creator() {
        let creator = o("https://a.example/");
        let d = Origin::for_document(&u("data:text/html,<p>"), Some(&creator));
        assert!(d.is_opaque());
        assert!(!d.same_origin(&creator));
    }

    #[test]
    fn blob_takes_inner_origin() {
        let b = o("blob:https://a.example/550e8400-e29b-41d4-a716-446655440000");
        assert!(b.same_origin(&o("https://a.example/")));
        assert!(o("blob:null/abc").is_opaque());
        assert!(o("blob:file:///x").is_opaque());
        assert!(o("blob:blob:https://a.example/x").is_opaque());
        assert!(o("blob:garbage").is_opaque());
    }

    #[test]
    fn file_is_opaque_and_never_same_origin() {
        let a = o("file:///tmp/a.html");
        let b = o("file:///tmp/a.html");
        assert!(a.is_opaque());
        assert!(!a.same_origin(&b));
    }

    #[test]
    fn non_special_schemes_are_opaque() {
        assert!(o("custom://host/").is_opaque());
        assert!(o("javascript:alert(1)").is_opaque());
        assert!(o("mailto:a@b.example").is_opaque());
    }

    #[test]
    fn tuple_constructor_refuses_the_unrepresentable() {
        assert!(Origin::tuple("file", Host::Empty, None).is_none());
        assert!(Origin::tuple("custom", Host::Domain("a".into()), None).is_none());
        assert!(Origin::tuple("https", Host::Empty, None).is_none());
        assert_eq!(
            Origin::tuple("HTTPS", Host::Domain("a.example".into()), Some(443)),
            Some(o("https://a.example/"))
        );
    }

    #[test]
    fn sites() {
        assert!(same_site(
            &o("https://a.example.com/"),
            &o("https://b.example.com/"),
            &psl
        ));
        assert!(!same_site(
            &o("https://a.example.com/"),
            &o("http://a.example.com/"),
            &psl
        ));
        assert!(schemelessly_same_site(
            &o("https://a.example.com/"),
            &o("http://b.example.com:8080/"),
            &psl
        ));
        assert!(!same_site(
            &o("https://a.example.com/"),
            &o("https://example.org/"),
            &psl
        ));
        // Shared-suffix hosts are different sites.
        assert!(!same_site(
            &o("https://a.co.uk/"),
            &o("https://b.co.uk/"),
            &psl
        ));
        assert!(same_site(
            &o("https://x.a.co.uk/"),
            &o("https://y.a.co.uk/"),
            &psl
        ));
        // IPs: site is the address itself.
        assert!(same_site(
            &o("http://127.0.0.1:1/"),
            &o("http://127.0.0.1:2/"),
            &psl
        ));
        assert!(!same_site(
            &o("http://127.0.0.1/"),
            &o("http://127.0.0.2/"),
            &psl
        ));
        // No registrable domain: falls back to the host.
        assert!(same_site(
            &o("https://localhost/"),
            &o("https://localhost:8/"),
            &psl
        ));
        assert!(!same_site(
            &o("https://localhost/"),
            &o("https://other/"),
            &psl
        ));
    }

    #[test]
    fn opaque_sites() {
        let a = Origin::new_opaque();
        let b = Origin::new_opaque();
        assert!(same_site(&a, &a, &psl));
        assert!(!same_site(&a, &b, &psl));
        assert!(!schemelessly_same_site(&a, &b, &psl));
        assert!(!schemelessly_same_site(&a, &o("https://a.example/"), &psl));
    }

    #[test]
    fn cookie_context() {
        let top = o("https://a.example.com/");
        assert!(!cross_site_for_cookies(
            &top,
            &o("https://api.example.com/"),
            &psl
        ));
        assert!(cross_site_for_cookies(
            &top,
            &o("https://evil.example/"),
            &psl
        ));
        assert!(cross_site_for_cookies(
            &top,
            &o("http://api.example.com/"),
            &psl
        ));
    }

    #[test]
    fn every_enforcement_point_is_reached() {
        trace::reset();
        let a = o("https://a.example.com/");
        let creator = a.clone();
        let _ = Origin::for_document(&u("about:blank"), Some(&creator));
        let _ = a.same_origin(&creator);
        let _ = same_site(&a, &creator, &psl);
        let _ = schemelessly_same_site(&a, &creator, &psl);
        let _ = cross_site_for_cookies(&a, &creator, &psl);
        trace::assert_all_hit(ENFORCEMENT_POINTS);
    }

    #[test]
    fn hostile_strings_never_panic() {
        let mut x: u64 = 0x1234_5678_9abc_def1;
        let alphabet = b"abc.:/@[]%\\?#0x1fFhtps ;=-_\t\n\xc3\xa9";
        for _ in 0..20_000 {
            let mut s = String::new();
            if x & 1 == 0 {
                s.push_str("http://");
            }
            for _ in 0..(x % 40) {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                let b = alphabet[(x % alphabet.len() as u64) as usize];
                s.push(b as char);
            }
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            if let Some(url) = Url::parse(&s) {
                let _ = Origin::from_url(&url).serialize();
                let _ = url.serialize(false);
            }
        }
    }
}
