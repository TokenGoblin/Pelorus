//! Loading a web page for the embedder: one navigation, then its stylesheets.
//!
//! This is the door the browser binary uses to reach the network. It owns no
//! sockets and no policy of its own: every request goes through
//! [`SecureFetcher::fetch`], so partitioning, HSTS, CSP, mixed content, CORS,
//! referrer policy, SRI and redirect rules are the ones already wired there.
//! `px-shell` calls this module and never opens a connection itself (the symbol
//! audit forbids network calls outside `px-net` and `px-update`).
//!
//! # State
//!
//! A [`Loader`] lives for one page load. It owns the fetcher, and so the
//! (partitioned) cookie jar: cookies the site sets during the load are kept
//! only for that load's own redirects and stylesheet requests, then dropped with
//! the loader. Nothing is persisted and nothing identifies the browser.
//!
//! # What is not here
//!
//! Scripts, frames and forms are not fetched (the engine cannot use them), so
//! the embedder never asks for them. Images are ([`Loader::image`]) and web
//! fonts are ([`Loader::font`]). HTTP/2 is not implemented
//! (ADR 015). The loader is synchronous and in-process; the content/network
//! process split is Phase 19.

use px_websec::cors::{CredentialsMode, RequestMode};
use px_websec::origin::{Origin as WOrigin, Url as WUrl};
use px_websec::referrer::ReferrerPolicy;

use crate::fetch::FetchError;
use crate::hsts::PreloadList;
use crate::psl::PublicSuffixList;
use crate::secure::{
    Destination, DocumentFacts, FetchContext, FetchOutcome, SecureFetcher, ViolationKind,
};

const PSL_TEXT: &str = include_str!("../../../data/public_suffix_list.dat");
const HSTS_TEXT: &str = include_str!("../../../data/hsts_preload.txt");

/// The largest stylesheet the loader hands back.
pub const MAX_STYLESHEET_BYTES: usize = 2 << 20;

/// The largest image body the loader hands back (compressed bytes; the decoder
/// has its own pixel and memory caps).
pub const MAX_IMAGE_BYTES: usize = 8 << 20;

/// The largest web font body the loader hands back (the container as sent,
/// compressed for WOFF/WOFF2; the font reader has its own caps on what it
/// unwraps).
pub const MAX_FONT_BYTES: usize = 8 << 20;

/// What kind of picture a body is, by its first bytes (never by its name or by
/// the server's say-so alone).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageKind {
    Png,
    Gif,
    Jpeg,
    Bmp,
    Ico,
    Webp,
}

/// Sniff an image signature. `None` for anything else, SVG and HTML included:
/// an unknown body is refused, not guessed at.
#[must_use]
pub fn sniff_image(body: &[u8]) -> Option<ImageKind> {
    if body.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some(ImageKind::Png)
    } else if body.starts_with(b"GIF87a") || body.starts_with(b"GIF89a") {
        Some(ImageKind::Gif)
    } else if body.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(ImageKind::Jpeg)
    } else if body.starts_with(b"BM") {
        Some(ImageKind::Bmp)
    } else if body.starts_with(&[0, 0, 1, 0]) {
        Some(ImageKind::Ico)
    } else if body.starts_with(b"RIFF") && body.get(8..12) == Some(b"WEBP".as_slice()) {
        Some(ImageKind::Webp)
    } else {
        None
    }
}

/// The lists a load needs, parsed once.
pub struct WebSession {
    psl: PublicSuffixList,
    preload: PreloadList,
}

/// Why a load did not produce a page. Every variant has calm wording in
/// [`LoadError::reason`]; none blames the visitor or the site.
#[derive(Debug)]
pub enum LoadError {
    /// The address is not an http(s) URL this browser can fetch.
    BadAddress,
    /// The bundled data (public suffix list, HSTS list) did not load.
    Setup,
    /// A security rule stopped the request or the response.
    Blocked {
        /// Which enforcement point stopped it.
        point: String,
        /// Its short description.
        detail: String,
        /// The class of rule, in plain words.
        kind: &'static str,
    },
    /// The network or the TLS session failed.
    Network(FetchError),
    /// No response and no recorded reason: treated as a failed fetch.
    NoResponse,
}

impl LoadError {
    /// A short, calm explanation for the error page.
    #[must_use]
    pub fn reason(&self) -> String {
        match self {
            Self::BadAddress => {
                "This address is not one the browser can open. Web addresses start with https://."
                    .to_string()
            }
            Self::Setup => "The browser's built-in security lists could not be read.".to_string(),
            Self::Blocked { kind, detail, .. } => format!(
                "A {kind} rule stopped this page from loading ({detail}). Nothing from the page was run."
            ),
            Self::Network(e) => match e {
                FetchError::Connect(_) => {
                    "The site could not be reached. It may be offline, or the address may have a typing slip."
                        .to_string()
                }
                FetchError::Tls(_) => {
                    "A private, verified connection to the site could not be set up, so the page was not opened."
                        .to_string()
                }
                FetchError::Protocol(_) => {
                    "The site replied in a way this browser does not accept.".to_string()
                }
                FetchError::Io(_) => "The connection ended before the page arrived.".to_string(),
                FetchError::BadRequest(_) => "The address could not be turned into a request."
                    .to_string(),
                FetchError::PlaintextToPreloadedHost => {
                    "This site only accepts secure connections.".to_string()
                }
            },
            Self::NoResponse => "The site did not return a page.".to_string(),
        }
    }
}

/// A fetched navigation target.
#[derive(Debug)]
pub struct Document {
    /// The URL after redirects and upgrades.
    pub url: String,
    /// HTTP status of the final response.
    pub status: u16,
    /// `Content-Type` header value, if any.
    pub content_type: Option<String>,
    /// The response body, undecoded.
    pub body: Vec<u8>,
}

/// An image body that passed the loader's checks.
#[derive(Debug)]
pub struct FetchedImage {
    /// The URL after redirects.
    pub url: String,
    /// What its first bytes say it is.
    pub kind: ImageKind,
    /// The undecoded bytes, at most [`MAX_IMAGE_BYTES`].
    pub body: Vec<u8>,
}

/// A font body that passed the loader's checks.
#[derive(Debug)]
pub struct FetchedFont {
    /// The URL after redirects.
    pub url: String,
    /// The undecoded container (sfnt, WOFF or WOFF2), at most
    /// [`MAX_FONT_BYTES`]. Whether it is a font is the font reader's decision.
    pub body: Vec<u8>,
}

impl WebSession {
    /// Parse the bundled public suffix and HSTS preload lists.
    pub fn new() -> Result<Self, LoadError> {
        let psl = PublicSuffixList::parse(PSL_TEXT).map_err(|_| LoadError::Setup)?;
        let preload = PreloadList::parse(HSTS_TEXT).map_err(|_| LoadError::Setup)?;
        Ok(Self { psl, preload })
    }

    /// A loader for one page.
    #[must_use]
    pub fn loader(&self) -> Loader<'_> {
        Loader {
            fetcher: SecureFetcher::new(&self.psl, Some(&self.preload)),
            document: None,
        }
    }
}

/// What a navigation established, kept for the subresource requests that follow.
struct Established {
    url: String,
    facts: Option<DocumentFacts>,
}

/// One page load: a navigation and the subresources it asks for.
pub struct Loader<'a> {
    fetcher: SecureFetcher<'a>,
    document: Option<Established>,
}

fn blocked(out: &FetchOutcome) -> Option<LoadError> {
    let v = out.enforced().next()?;
    let kind = match &v.kind {
        ViolationKind::Csp => "content security",
        ViolationKind::MixedContent => "mixed-content",
        ViolationKind::Cors(_) | ViolationKind::Preflight(_) => "cross-origin",
        ViolationKind::Redirect(_) => "redirect",
        ViolationKind::Sri(_) => "integrity",
        ViolationKind::Corp => "cross-origin resource",
        _ => "security",
    };
    Some(LoadError::Blocked {
        point: v.point.to_string(),
        detail: v.detail.clone(),
        kind,
    })
}

fn failure(out: FetchOutcome) -> LoadError {
    if let Some(b) = blocked(&out) {
        return b;
    }
    match out.error {
        Some(e) => LoadError::Network(e),
        None => LoadError::NoResponse,
    }
}

impl Loader<'_> {
    /// Test-only: send every connection of this loader to `addr`.
    #[cfg(feature = "testing")]
    pub fn connect_to(&mut self, addr: std::net::SocketAddr) {
        self.fetcher.connect_to = Some(addr);
    }

    /// Navigate to `url` (an absolute http(s) URL): the browser-initiated
    /// top-level fetch, no referrer.
    pub fn navigate(&mut self, url: &str) -> Result<Document, LoadError> {
        let mut ctx = FetchContext::top_level_document(url, Destination::Navigation)
            .ok_or(LoadError::BadAddress)?;
        ctx.document_referrer_policy = Some(ReferrerPolicy::NoReferrer);
        let mut out = self.fetcher.fetch(&ctx, url);
        let Some(resp) = out.response.take() else {
            return Err(failure(out));
        };
        let final_url = resp.url.serialize(true);
        let content_type = resp
            .headers
            .iter()
            .find(|h| h.name.eq_ignore_ascii_case("content-type"))
            .and_then(|h| String::from_utf8(h.value.clone()).ok());
        self.document = Some(Established {
            url: final_url.clone(),
            facts: out.document.take(),
        });
        Ok(Document {
            url: final_url,
            status: resp.status,
            content_type,
            body: resp.embedder_body().to_vec(),
        })
    }

    /// Whether the document's Content-Security-Policy allows an inline
    /// `<style>` with this text (fail closed when there is no document).
    #[must_use]
    pub fn allows_inline_style(&self, text: &str) -> bool {
        use px_websec::csp::InlineType;
        match self.document.as_ref().and_then(|d| d.facts.as_ref()) {
            Some(f) => !f
                .csp
                .check_inline(InlineType::Style, None, text)
                .is_blocked(),
            None => false,
        }
    }

    /// Fetch a stylesheet for the current document. `href` is resolved against
    /// the document's URL. The request carries the document's CSP and
    /// referrer policy; the result is the body bytes (at most
    /// [`MAX_STYLESHEET_BYTES`]).
    pub fn stylesheet(&mut self, href: &str) -> Result<Vec<u8>, LoadError> {
        let doc = self.document.as_ref().ok_or(LoadError::BadAddress)?;
        let target = resolve(&doc.url, href).ok_or(LoadError::BadAddress)?;
        let mut ctx = FetchContext::top_level_document(&doc.url, Destination::Style)
            .ok_or(LoadError::BadAddress)?;
        if let Some(f) = &doc.facts {
            ctx.csp = f.csp.clone();
            ctx.document_referrer_policy = f.referrer_policy;
        }
        let mut out = self.fetcher.fetch(&ctx, &target);
        let Some(resp) = out.response.take() else {
            return Err(failure(out));
        };
        if !(200..300).contains(&resp.status) {
            return Err(LoadError::NoResponse);
        }
        let body = resp.embedder_body();
        if body.len() > MAX_STYLESHEET_BYTES {
            return Err(LoadError::NoResponse);
        }
        Ok(body.to_vec())
    }

    /// Fetch a web font for the current document (`@font-face` `src: url()`).
    /// `src` is resolved against the document's URL.
    ///
    /// A font load is a **CORS-mode** request (CSS Fonts 4 section 4.3, Fetch's
    /// `font` destination): a cross-origin font is used only if the server
    /// answers with an `Access-Control-Allow-Origin` that admits this document,
    /// and the credentials mode is `same-origin`. It otherwise runs the same
    /// secure path as every subresource: the partitioned client state, HSTS,
    /// the document's CSP (`font-src`), mixed-content blocking, referrer policy
    /// and the redirect rules. Only a 2xx body of at most [`MAX_FONT_BYTES`] is
    /// returned.
    pub fn font(&mut self, src: &str) -> Result<FetchedFont, LoadError> {
        let doc = self.document.as_ref().ok_or(LoadError::BadAddress)?;
        let target = resolve(&doc.url, src).ok_or(LoadError::BadAddress)?;
        let mut ctx = FetchContext::top_level_document(&doc.url, Destination::Font)
            .ok_or(LoadError::BadAddress)?;
        ctx.mode = RequestMode::Cors;
        ctx.credentials = CredentialsMode::SameOrigin;
        if let Some(f) = &doc.facts {
            ctx.csp = f.csp.clone();
            ctx.document_referrer_policy = f.referrer_policy;
        }
        let mut out = self.fetcher.fetch(&ctx, &target);
        let Some(resp) = out.response.take() else {
            return Err(failure(out));
        };
        if !(200..300).contains(&resp.status) {
            return Err(LoadError::NoResponse);
        }
        let body = resp.embedder_body();
        if body.is_empty() || body.len() > MAX_FONT_BYTES {
            return Err(LoadError::NoResponse);
        }
        Ok(FetchedFont {
            url: resp.url.serialize(true),
            body: body.to_vec(),
        })
    }

    /// Fetch an image for the current document (`<img>`, `<source>`,
    /// `background-image`). `src` is resolved against the document's URL and the
    /// request is a [`Destination::Image`] one, so it runs the same secure path
    /// as every other subresource: the partitioned client state, HSTS, the
    /// document's CSP (`img-src`), mixed-content blocking (an `http:` image on
    /// an `https:` page is upgraded or refused by the fetcher), the document's
    /// referrer policy, and the redirect rules.
    ///
    /// A body is returned only when it is plausibly a picture: a 2xx response,
    /// at most [`MAX_IMAGE_BYTES`], a `Content-Type` that is absent,
    /// `image/*` or `application/octet-stream`, and bytes that start with a
    /// signature [`sniff_image`] knows. Anything else (HTML, JSON, SVG, an
    /// unknown signature) is refused with [`LoadError::NoResponse`], whatever
    /// the server called it.
    pub fn image(&mut self, src: &str) -> Result<FetchedImage, LoadError> {
        let doc = self.document.as_ref().ok_or(LoadError::BadAddress)?;
        let target = resolve(&doc.url, src).ok_or(LoadError::BadAddress)?;
        let mut ctx = FetchContext::top_level_document(&doc.url, Destination::Image)
            .ok_or(LoadError::BadAddress)?;
        if let Some(f) = &doc.facts {
            ctx.csp = f.csp.clone();
            ctx.document_referrer_policy = f.referrer_policy;
        }
        let mut out = self.fetcher.fetch(&ctx, &target);
        let Some(resp) = out.response.take() else {
            return Err(failure(out));
        };
        if !(200..300).contains(&resp.status) {
            return Err(LoadError::NoResponse);
        }
        let content_type = resp
            .headers
            .iter()
            .find(|h| h.name.eq_ignore_ascii_case("content-type"))
            .and_then(|h| String::from_utf8(h.value.clone()).ok());
        if let Some(ct) = content_type.as_deref() {
            let essence = ct
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase();
            let ok = essence.is_empty()
                || essence.starts_with("image/")
                || essence == "application/octet-stream";
            // SVG is a document, not a bitmap this loader hands to a decoder.
            if !ok || essence == "image/svg+xml" {
                return Err(LoadError::NoResponse);
            }
        }
        let body = resp.embedder_body();
        if body.len() > MAX_IMAGE_BYTES {
            return Err(LoadError::NoResponse);
        }
        let Some(kind) = sniff_image(body) else {
            return Err(LoadError::NoResponse);
        };
        Ok(FetchedImage {
            url: resp.url.serialize(true),
            kind,
            body: body.to_vec(),
        })
    }
}

// -- URL reference resolution ------------------------------------------------

fn encode_path_char(c: char, out: &mut String) {
    if c <= ' '
        || c == '"'
        || c == '<'
        || c == '>'
        || c == '`'
        || c == '{'
        || c == '}'
        || c == '\u{7f}'
    {
        let mut buf = [0u8; 4];
        for b in c.encode_utf8(&mut buf).bytes() {
            out.push_str(&format!("%{b:02X}"));
        }
    } else if c.is_ascii() {
        out.push(c);
    } else {
        let mut buf = [0u8; 4];
        for b in c.encode_utf8(&mut buf).bytes() {
            out.push_str(&format!("%{b:02X}"));
        }
    }
}

fn remove_dot_segments(path: &str) -> String {
    let mut stack: Vec<&str> = Vec::new();
    let mut trailing = false;
    for seg in path.split('/').skip(1) {
        trailing = false;
        match seg {
            "." => trailing = true,
            ".." => {
                stack.pop();
                trailing = true;
            }
            s => stack.push(s),
        }
    }
    let mut out = String::new();
    for s in &stack {
        out.push('/');
        out.push_str(s);
    }
    if out.is_empty() || trailing {
        out.push('/');
    }
    out
}

/// Resolve `reference` against the absolute http(s) URL `base` (RFC 3986 §5,
/// for the forms a page uses). `None` for anything that is not an http(s)
/// result. The fragment is kept so the caller can decide what to do with it.
#[must_use]
pub fn resolve(base: &str, reference: &str) -> Option<String> {
    let base = WUrl::parse(base)?;
    if !matches!(base.scheme(), "http" | "https") {
        return None;
    }
    let mut encoded = String::new();
    for c in reference.trim().chars() {
        if matches!(c, '\t' | '\n' | '\r') {
            continue;
        }
        encode_path_char(c, &mut encoded);
    }
    let r = encoded.as_str();
    let origin = WOrigin::from_url(&base).serialize();
    let (r_no_frag, frag) = match r.split_once('#') {
        Some((a, f)) => (a, Some(f)),
        None => (r, None),
    };
    let base_query = base.query().map(|q| format!("?{q}")).unwrap_or_default();
    let mut s = if r.is_empty() || r.starts_with('#') {
        format!("{origin}{}{base_query}", base.path())
    } else if let Some(rest) = r_no_frag.strip_prefix("//") {
        format!("{}://{rest}", base.scheme())
    } else if r_no_frag.starts_with('/') {
        let (p, q) = r_no_frag.split_once('?').unwrap_or((r_no_frag, ""));
        let q = if r_no_frag.contains('?') {
            format!("?{q}")
        } else {
            String::new()
        };
        format!("{origin}{}{q}", remove_dot_segments(p))
    } else if r_no_frag.starts_with('?') {
        format!("{origin}{}{r_no_frag}", base.path())
    } else if WUrl::parse(r_no_frag).is_some_and(|u| !u.scheme().is_empty())
        && starts_with_scheme(r_no_frag)
    {
        r_no_frag.to_string()
    } else {
        let (p, q) = match r_no_frag.split_once('?') {
            Some((p, q)) => (p, format!("?{q}")),
            None => (r_no_frag, String::new()),
        };
        let dir = match base.path().rfind('/') {
            Some(i) => base.path().get(..=i).unwrap_or("/"),
            None => "/",
        };
        format!("{origin}{}{q}", remove_dot_segments(&format!("{dir}{p}")))
    };
    if let Some(f) = frag {
        s.push('#');
        s.push_str(f);
    }
    let parsed = WUrl::parse(&s)?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    Some(parsed.serialize(false))
}

fn starts_with_scheme(s: &str) -> bool {
    let Some((head, _)) = s.split_once(':') else {
        return false;
    };
    !head.is_empty()
        && head.starts_with(|c: char| c.is_ascii_alphabetic())
        && head
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// An absolute http(s) URL from user-typed text: adds `https://` to a bare
/// host. `None` if the result is not a fetchable address.
#[must_use]
pub fn address_from_text(text: &str) -> Option<String> {
    let t = text.trim();
    let full = if starts_with_scheme(t) {
        t.to_string()
    } else {
        format!("https://{t}")
    };
    let u = WUrl::parse(&full)?;
    if !matches!(u.scheme(), "http" | "https") || u.has_credentials() {
        return None;
    }
    Some(u.serialize(false))
}

#[cfg(test)]
mod tests {
    use super::*;

    const B: &str = "https://example.com/a/b/c.html?x=1";

    #[test]
    fn references_resolve() {
        let r = |x: &str| resolve(B, x);
        assert_eq!(r("d.css").as_deref(), Some("https://example.com/a/b/d.css"));
        assert_eq!(
            r("../d.css").as_deref(),
            Some("https://example.com/a/d.css")
        );
        assert_eq!(
            r("../../../d.css").as_deref(),
            Some("https://example.com/d.css")
        );
        assert_eq!(r("/d.css").as_deref(), Some("https://example.com/d.css"));
        assert_eq!(r("//other.org/x").as_deref(), Some("https://other.org/x"));
        assert_eq!(
            r("http://other.org/x").as_deref(),
            Some("http://other.org/x")
        );
        assert_eq!(
            r("?y=2").as_deref(),
            Some("https://example.com/a/b/c.html?y=2")
        );
        assert_eq!(
            r("#top").as_deref(),
            Some("https://example.com/a/b/c.html?x=1#top")
        );
        assert_eq!(r("./").as_deref(), Some("https://example.com/a/b/"));
        assert_eq!(
            r("d e.css").as_deref(),
            Some("https://example.com/a/b/d%20e.css")
        );
        assert_eq!(r("javascript:alert(1)"), None);
        assert_eq!(r("mailto:a@b.c"), None);
        assert_eq!(r("data:text/css,a{}"), None);
    }

    #[test]
    fn typed_addresses() {
        assert_eq!(
            address_from_text("example.com").as_deref(),
            Some("https://example.com/")
        );
        assert_eq!(
            address_from_text(" http://example.org/x ").as_deref(),
            Some("http://example.org/x")
        );
        assert_eq!(address_from_text("ftp://example.org/x"), None);
        assert_eq!(address_from_text("https://u:p@example.org/"), None);
    }

    #[test]
    fn image_signatures_are_sniffed_and_everything_else_is_not() {
        let png = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0];
        assert_eq!(sniff_image(&png), Some(ImageKind::Png));
        assert_eq!(sniff_image(b"GIF89a\x01\x00"), Some(ImageKind::Gif));
        assert_eq!(sniff_image(b"GIF87a"), Some(ImageKind::Gif));
        assert_eq!(
            sniff_image(&[0xFF, 0xD8, 0xFF, 0xE0]),
            Some(ImageKind::Jpeg)
        );
        assert_eq!(sniff_image(b"BM\x00\x00"), Some(ImageKind::Bmp));
        assert_eq!(sniff_image(&[0, 0, 1, 0, 1, 0]), Some(ImageKind::Ico));
        assert_eq!(
            sniff_image(b"RIFF\x00\x00\x00\x00WEBPVP8 "),
            Some(ImageKind::Webp)
        );
        // Documents, scripts, SVG, truncations and the empty body are not pictures.
        for not in [
            &b"<html>"[..],
            b"<svg xmlns='http://www.w3.org/2000/svg'/>",
            b"<?xml version='1.0'?><svg/>",
            b"GIF8",
            b"\x89PNG",
            b"RIFF\x00\x00\x00\x00WAVEfmt ",
            b"",
        ] {
            assert_eq!(sniff_image(not), None, "{not:?}");
        }
    }

    #[test]
    fn lists_parse() {
        assert!(WebSession::new().is_ok());
    }
}
