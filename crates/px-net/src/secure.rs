//! The fetch path with the web security model wired in (Phase 12).
//!
//! [`SecureFetcher::fetch`] is the one place where a request and its response
//! are *decided*: same-origin, CORS, mixed content, CSP, referrer, CORP/COEP,
//! SRI, framing, redirects. Every decision is made by calling `px-websec`;
//! nothing here re-derives a policy. `px-websec` supplies verdicts as pure
//! functions, and this module is what makes them reachable ("a check that
//! exists but is not reached is not a check").
//!
//! # What the caller supplies, and what it cannot
//!
//! [`FetchContext`] is **data from the caller**: the initiator's origin, the
//! top-level origin, the destination, the CSP of the requesting document and
//! so on. None of it is ever read from the response. In production the caller
//! is the content process's network client, and the broker fills the
//! identity-bearing fields from the connection (CLAUDE.md: authority comes
//! from the channel, never from the message); this module treats the context
//! as already authenticated and is not a way to authenticate it.
//!
//! # Order of enforcement
//!
//! For every hop (the first request and each redirect target) the request side
//! runs in this order, and any step may end the fetch:
//!
//! ```text
//!  0  syntax      scheme is http(s); author method/headers not forbidden
//!  1  HSTS        preload upgrade  http -> https           (this module)
//!  2  CSP         upgrade-insecure-requests upgrade        (csp)
//!  3  mixed       check_request: Allow | Upgrade | Block   (mixed)
//!  4  CSP         block-all-mixed-content                  (csp)
//!  5  CSP         check_request on the destination's directive;
//!                 report-only policies are recorded, never block
//!  6  CORS        taint(): basic | cors | opaque, or deny  (cors)
//!  7  CORS        classify(); preflight (cache keyed by partition) if needed
//!  8  headers     Origin, Referer (referrer), Cookie (SameSite via
//!                 cross_site_for_cookies)
//!  9  transport   one request, never following a redirect
//! ```
//!
//! HSTS runs before the mixed-content check on purpose: a host that must be
//! reached over https is upgraded to https, and an upgraded request is then
//! not mixed content. Running it after would block a request that is about to
//! become secure, and would let the *blocking* decision depend on whether the
//! host happened to be preloaded. Both orders are fail-safe (nothing plaintext
//! leaves for a preloaded host either way: the transport is also handed the
//! preload list); this one is the one browsers observably implement.
//! `tests/secure_fetch.rs` pins it.
//!
//! The response side then runs, on the *final* response only (a redirect
//! response is checked for CORS and then followed):
//!
//! ```text
//!  r0 mixed       check_response on the final URL
//!  r1 CORS        validate_response (tainting == cors)
//!  r2 CORP/COEP   coep_check_response (opaque) / corp_check (subframe)
//!  r3 SRI         parse + verify the body (fail closed on parse error)
//!  r4 framing     X-Frame-Options and csp frame-ancestors, combined
//!  r5 filter      filter_response: basic / cors / opaque, Expose-Headers
//! ```
//!
//! # Redirects
//!
//! `redirect::Chain` still owns following: hop cap, loop detection, the
//! https-to-http refusal and `Location` resolution. This module runs steps
//! 0 to 9 again for every hop, applies the Fetch rules for the *origin-tainted*
//! flag (a cross-origin redirect from a cross-origin response makes later hops
//! carry `Origin: null`), drops `Authorization` on a cross-origin hop, and
//! turns `POST` into `GET` where Fetch says so.
//!
//! # Fail closed
//!
//! Anything that cannot be completed denies: an unparsable URL, a header value
//! that is not UTF-8, a CSP header that does not parse (the document gets
//! `Policy::deny_all` from `PolicySet::from_headers_fail_closed`), an SRI
//! attribute over its bounds, a transport error. A denied fetch has no
//! [`FetchOutcome::response`].

use std::time::Instant;

use px_websec::cors::{self, CredentialsMode, RequestMode, Tainting};
use px_websec::csp::{self, CspUrl, PolicySet};
use px_websec::origin::{self as wsorigin, Origin as WOrigin, Url as WUrl};
use px_websec::referrer::{self, ReferrerPolicy};
use px_websec::{coop, frame, mixed, sri};

use crate::cookie::Jar;
use crate::fetch::{self, FetchError, RequestSpec};
use crate::hsts::PreloadList;
use crate::partition::{Origin as NetOrigin, PartitionKey};
use crate::psl::PublicSuffixList;
use crate::redirect::{Chain, RefusedRedirect};

/// Maximum bytes of `Referer`-bearing or otherwise caller-supplied header
/// value accepted by [`FetchContext::headers`]; larger is denied.
pub const MAX_AUTHOR_HEADER_BYTES: usize = 8 * 1024;

/// What the request is for: Fetch's *destination*, folded with the initiator
/// kind. It selects the mixed-content category and the CSP directive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Destination {
    /// Top-level navigation. No mixed-content or CSP request check (neither
    /// applies to a top-level navigation); referrer and cookies do.
    Navigation,
    /// `<iframe>`/`<frame>` navigation: `frame-src`, framing checks.
    Iframe,
    /// `<script>`.
    Script,
    /// Stylesheet.
    Style,
    /// Image.
    Image,
    /// Web font.
    Font,
    /// `fetch()`/XHR.
    Fetch,
    /// `EventSource`.
    EventSource,
    /// `sendBeacon`.
    Beacon,
    /// `<audio>`.
    Audio,
    /// `<video>`.
    Video,
    /// `<track>`.
    Track,
    /// `<object>`.
    Object,
    /// `<embed>`.
    Embed,
    /// Dedicated worker script.
    Worker,
    /// Shared worker script.
    SharedWorker,
    /// Service worker script.
    ServiceWorker,
    /// Web app manifest.
    Manifest,
    /// `<link rel=prefetch>`.
    Prefetch,
}

impl Destination {
    /// Every destination, for exhaustive tests.
    pub const ALL: [Destination; 19] = [
        Destination::Navigation,
        Destination::Iframe,
        Destination::Script,
        Destination::Style,
        Destination::Image,
        Destination::Font,
        Destination::Fetch,
        Destination::EventSource,
        Destination::Beacon,
        Destination::Audio,
        Destination::Video,
        Destination::Track,
        Destination::Object,
        Destination::Embed,
        Destination::Worker,
        Destination::SharedWorker,
        Destination::ServiceWorker,
        Destination::Manifest,
        Destination::Prefetch,
    ];

    /// The CSP request kind, or `None` where CSP does not govern the request.
    pub fn csp_kind(self) -> Option<csp::RequestKind> {
        use csp::RequestKind as K;
        Some(match self {
            Self::Navigation => return None,
            Self::Iframe => K::Iframe,
            Self::Script => K::Script,
            Self::Style => K::Style,
            Self::Image => K::Image,
            Self::Font => K::Font,
            Self::Fetch => K::Fetch,
            Self::EventSource => K::EventSource,
            Self::Beacon => K::Beacon,
            Self::Audio => K::Audio,
            Self::Video => K::Video,
            Self::Track => K::Track,
            Self::Object => K::Object,
            Self::Embed => K::Embed,
            Self::Worker => K::Worker,
            Self::SharedWorker => K::SharedWorker,
            Self::ServiceWorker => K::ServiceWorker,
            Self::Manifest => K::Manifest,
            Self::Prefetch => K::Prefetch,
        })
    }

    /// The mixed-content request kind, or `None` for a top-level navigation
    /// (MC2 leaves those to the UI).
    pub fn mixed_kind(self) -> Option<mixed::RequestKind> {
        use mixed::RequestKind as K;
        Some(match self {
            Self::Navigation => return None,
            Self::Iframe => K::Subframe,
            Self::Script => K::Script,
            Self::Style => K::Style,
            Self::Image => K::Image,
            Self::Font => K::Font,
            Self::Fetch | Self::EventSource => K::Fetch,
            Self::Beacon => K::Beacon,
            Self::Audio => K::Audio,
            Self::Video => K::Video,
            Self::Track => K::Track,
            Self::Object | Self::Embed => K::Object,
            Self::Worker | Self::SharedWorker => K::Worker,
            Self::ServiceWorker => K::ServiceWorker,
            Self::Manifest => K::Manifest,
            Self::Prefetch => K::Prefetch,
        })
    }

    fn is_navigation(self) -> bool {
        matches!(self, Self::Navigation | Self::Iframe)
    }
}

/// Everything the checks need, as data supplied by the caller. See the module
/// documentation for why none of it may come from a response.
#[derive(Debug, Clone)]
pub struct FetchContext {
    /// The requesting document's origin: the request's *origin* in Fetch
    /// terms, and what `Origin:` and CORS compare against.
    pub initiator: WOrigin,
    /// The top-level document's origin: the site-for-cookies, and the source
    /// of the partition key's top-level host.
    pub top_level: WOrigin,
    /// The requesting document's URL: the referrer source.
    pub document_url: WUrl,
    /// The requesting document and its ancestors, for mixed content,
    /// secure-context and framing checks.
    pub document: mixed::Context,
    /// What is being fetched.
    pub destination: Destination,
    /// Request mode.
    pub mode: RequestMode,
    /// Credentials mode.
    pub credentials: CredentialsMode,
    /// Method (normalised by the fetch).
    pub method: String,
    /// Author-supplied request headers (what script set).
    pub headers: Vec<cors::Header>,
    /// Request body.
    pub body: Vec<u8>,
    /// Fetch's use-CORS-preflight flag.
    pub use_cors_preflight: bool,
    /// The request's own referrer policy (`referrerpolicy` attribute).
    pub request_referrer_policy: Option<ReferrerPolicy>,
    /// The document's referrer policy.
    pub document_referrer_policy: Option<ReferrerPolicy>,
    /// The requesting document's CSP.
    pub csp: PolicySet,
    /// The requesting document's enforced COEP.
    pub embedder_policy: coop::Coep,
    /// The `integrity` attribute, verbatim.
    pub integrity: Option<String>,
    /// The element's `nonce`.
    pub nonce: Option<String>,
    /// Whether the HTML parser created the element (`'strict-dynamic'`).
    pub parser_inserted: bool,
    /// For an `<iframe>`: its `sandbox` attribute.
    pub iframe_sandbox: Option<String>,
}

impl FetchContext {
    /// A top-level document at `document_url` making a `destination` request
    /// with the defaults a plain element load would have: `no-cors` for
    /// subresources, `cors` for `fetch()`, `navigate` for navigations;
    /// credentials `same-origin`; no CSP.
    ///
    /// `None` if the URL is not http(s).
    pub fn top_level_document(document_url: &str, destination: Destination) -> Option<Self> {
        let url = WUrl::parse(document_url)?;
        if !matches!(url.scheme(), "http" | "https") {
            return None;
        }
        let origin = WOrigin::from_url(&url);
        let node = mixed::ContextNode {
            url: url.clone(),
            origin: origin.clone(),
        };
        let mode = match destination {
            Destination::Navigation | Destination::Iframe => RequestMode::Navigate,
            Destination::Fetch | Destination::EventSource | Destination::Beacon => {
                RequestMode::Cors
            }
            _ => RequestMode::NoCors,
        };
        Some(Self {
            initiator: origin.clone(),
            top_level: origin,
            document_url: url,
            document: mixed::Context {
                document: node,
                ancestors: Vec::new(),
            },
            destination,
            mode,
            credentials: CredentialsMode::SameOrigin,
            method: "GET".to_string(),
            headers: Vec::new(),
            body: Vec::new(),
            use_cors_preflight: false,
            request_referrer_policy: None,
            document_referrer_policy: None,
            csp: PolicySet::new(),
            embedder_policy: coop::Coep::UnsafeNone,
            integrity: None,
            nonce: None,
            parser_inserted: false,
            iframe_sandbox: None,
        })
    }
}

/// What kind of decision refused (or flagged) the fetch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViolationKind {
    /// The request itself was malformed or forbidden (URL, scheme, method,
    /// header, no partition key).
    Request,
    /// Mixed content.
    MixedContent,
    /// CSP, enforced or report-only.
    Csp,
    /// The CORS check of the actual response (or a redirect response).
    Cors(cors::CorsError),
    /// The CORS preflight.
    Preflight(cors::CorsError),
    /// `Cross-Origin-Resource-Policy` / COEP.
    Corp,
    /// Subresource Integrity.
    Sri(sri::SriError),
    /// `X-Frame-Options`.
    FrameOptions,
    /// CSP `frame-ancestors`.
    FrameAncestors,
    /// A redirect that was refused.
    Redirect(RefusedRedirect),
    /// The response was not something the checks could evaluate.
    MalformedResponse,
}

/// One refusal, or one report-only observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    /// The px-websec enforcement point that produced the verdict (or the
    /// px-net step, for `Request`/`Redirect`).
    pub point: &'static str,
    /// What kind.
    pub kind: ViolationKind,
    /// `false` for a report-only observation: recorded, did not block.
    pub enforced: bool,
    /// The URL concerned, serialised without its fragment.
    pub url: String,
    /// Free text for the log.
    pub detail: String,
}

/// A response body that script may or may not read.
#[derive(Debug, Clone)]
pub struct ResponseView {
    /// The final URL.
    pub url: WUrl,
    /// Status as script sees it: 0 for an opaque response.
    pub status: u16,
    /// Headers as script sees them (Fetch response filtering applied).
    pub headers: Vec<cors::Header>,
    /// The tainting the response carries.
    pub tainting: Tainting,
    body: Vec<u8>,
}

impl ResponseView {
    /// Whether the response is opaque.
    pub fn is_opaque(&self) -> bool {
        self.tainting == Tainting::Opaque
    }

    /// The body as script may read it: `None` when opaque.
    pub fn script_body(&self) -> Option<&[u8]> {
        if self.is_opaque() {
            None
        } else {
            Some(&self.body)
        }
    }

    /// The body as the embedder (image decoder, script executor, stylesheet
    /// parser) may use it. For an opaque response this is the only way to get
    /// the bytes, and it is not exposed to script.
    pub fn embedder_body(&self) -> &[u8] {
        &self.body
    }
}

/// What a navigation response establishes for the new document.
#[derive(Debug, Clone)]
pub struct DocumentFacts {
    /// The new document's origin (sandbox applied).
    pub origin: WOrigin,
    /// Its CSP (fail-closed parse of the response headers).
    pub csp: PolicySet,
    /// Its `Referrer-Policy`.
    pub referrer_policy: Option<ReferrerPolicy>,
    /// Its COEP.
    pub embedder_policy: coop::EmbedderPolicy,
    /// Its COOP.
    pub opener_policy: coop::OpenerPolicy,
    /// The sandbox flags it runs under (iframe attribute and CSP `sandbox`).
    pub sandbox: Option<csp::SandboxFlags>,
}

/// The result of a fetch.
#[derive(Debug)]
pub struct FetchOutcome {
    /// The response, if every check passed. `None` means denied (see
    /// `violations`) or failed (see `error`).
    pub response: Option<ResponseView>,
    /// Enforced refusals and report-only observations, in the order found.
    pub violations: Vec<Violation>,
    /// The URL as first requested, if mixed-content, HSTS or CSP upgraded it.
    pub upgraded_from: Option<WUrl>,
    /// Every URL requested, in order, ending with the last attempted.
    pub urls: Vec<WUrl>,
    /// Tainting of the last hop, if one was computed.
    pub tainting: Option<Tainting>,
    /// Preflight requests actually sent.
    pub preflights_sent: usize,
    /// Preflights skipped because the cache covered the request.
    pub preflight_cache_hits: usize,
    /// For a navigation: what the response establishes for the new document.
    pub document: Option<DocumentFacts>,
    /// A transport failure, if that is why there is no response.
    pub error: Option<FetchError>,
}

impl FetchOutcome {
    fn new() -> Self {
        Self {
            response: None,
            violations: Vec::new(),
            upgraded_from: None,
            urls: Vec::new(),
            tainting: None,
            preflights_sent: 0,
            preflight_cache_hits: 0,
            document: None,
            error: None,
        }
    }

    /// Whether a response is available.
    pub fn allowed(&self) -> bool {
        self.response.is_some()
    }

    /// The violations that blocked.
    pub fn enforced(&self) -> impl Iterator<Item = &Violation> {
        self.violations.iter().filter(|v| v.enforced)
    }

    fn deny(
        &mut self,
        point: &'static str,
        kind: ViolationKind,
        url: &WUrl,
        detail: impl Into<String>,
    ) {
        self.violations.push(Violation {
            point,
            kind,
            enforced: true,
            url: url.serialize(true),
            detail: detail.into(),
        });
    }
}

/// The fetch path with every policy wired in. Owns the state a policy needs
/// across requests: the preflight cache and the cookie jar.
pub struct SecureFetcher<'a> {
    psl: &'a PublicSuffixList,
    preload: Option<&'a PreloadList>,
    preflight_cache: cors::PreflightCache,
    /// The cookie jar. Requests attach cookies from it when credentials mode
    /// allows, and responses' `Set-Cookie` are stored into it.
    pub jar: Jar,
    started: Instant,
    /// Test-only: connect every request here instead of resolving its host.
    #[cfg(feature = "testing")]
    pub connect_to: Option<std::net::SocketAddr>,
}

/// State carried between hops of one fetch.
struct HopState {
    url: WUrl,
    method: String,
    headers: Vec<cors::Header>,
    body: Vec<u8>,
    /// `Some` once a cross-origin redirect tainted the origin.
    tainted_origin: Option<WOrigin>,
    redirect_count: u32,
    /// A `Referrer-Policy` set by a redirect response.
    redirect_referrer_policy: Option<ReferrerPolicy>,
}

enum HopResult {
    Denied,
    Redirect { to: WUrl, status: u16 },
    Final,
}

fn csp_origin_of(origin: &WOrigin) -> csp::Origin {
    csp::Origin::from_url_str(&origin.serialize())
}

fn net_origin(url: &WUrl) -> Option<NetOrigin> {
    let host = url.host()?.serialize();
    NetOrigin::new(url.scheme(), &host, url.effective_port()?)
}

fn is_redirect_status(status: u16) -> bool {
    matches!(status, 301 | 302 | 303 | 307 | 308)
}

fn header_lines<'h>(headers: &'h [cors::Header], name: &str) -> Vec<&'h [u8]> {
    headers
        .iter()
        .filter(|h| h.name.eq_ignore_ascii_case(name))
        .map(|h| h.value.as_slice())
        .collect()
}

fn header_strs<'h>(headers: &'h [cors::Header], name: &str) -> Option<Vec<&'h str>> {
    header_lines(headers, name)
        .into_iter()
        .map(|b| std::str::from_utf8(b).ok())
        .collect()
}

impl<'a> SecureFetcher<'a> {
    /// A fetcher using `psl` for sites and partitions and, optionally, an
    /// HSTS preload list.
    pub fn new(psl: &'a PublicSuffixList, preload: Option<&'a PreloadList>) -> Self {
        Self {
            psl,
            preload,
            preflight_cache: cors::PreflightCache::new(),
            jar: Jar::new(),
            started: Instant::now(),
            #[cfg(feature = "testing")]
            connect_to: None,
        }
    }

    fn now(&self) -> u64 {
        self.started.elapsed().as_secs()
    }

    /// Drop every cached preflight (e.g. when storage is cleared).
    pub fn forget_preflights_for(&mut self, key: &PartitionKey) {
        self.preflight_cache.forget_partition(&key.to_string());
    }

    /// Number of cached preflight entries.
    pub fn preflight_cache_len(&self) -> usize {
        self.preflight_cache.len()
    }

    /// Fetch `url` as described by `ctx`, running every check.
    pub fn fetch(&mut self, ctx: &FetchContext, url: &str) -> FetchOutcome {
        let mut out = FetchOutcome::new();
        let Some(start) = WUrl::parse(url) else {
            out.violations.push(Violation {
                point: "net::parse_url",
                kind: ViolationKind::Request,
                enforced: true,
                url: String::new(),
                detail: "the URL does not parse".to_string(),
            });
            return out;
        };

        let method = cors::normalize_method(&ctx.method);
        if cors::is_forbidden_method(&method) {
            out.deny(
                "cors::forbidden_method",
                ViolationKind::Request,
                &start,
                format!("method {method} is forbidden"),
            );
            return out;
        }
        for h in &ctx.headers {
            if h.value.len() > MAX_AUTHOR_HEADER_BYTES
                || cors::is_forbidden_request_header(&h.name, &h.value)
            {
                out.deny(
                    "cors::forbidden_request_header",
                    ViolationKind::Request,
                    &start,
                    format!("header {} is forbidden or oversized", h.name),
                );
                return out;
            }
        }

        let mut chain = Chain::new();
        let mut state = HopState {
            url: start,
            method,
            headers: ctx.headers.clone(),
            body: ctx.body.clone(),
            tainted_origin: None,
            redirect_count: 0,
            redirect_referrer_policy: None,
        };
        loop {
            out.urls.push(state.url.clone());
            match self.hop(ctx, &mut state, &mut chain, &mut out) {
                HopResult::Denied | HopResult::Final => return out,
                HopResult::Redirect { to, status } => {
                    Self::advance(ctx, &mut state, to, status);
                }
            }
        }
    }

    /// Apply the Fetch rules for following a redirect to `to`.
    fn advance(ctx: &FetchContext, state: &mut HopState, to: WUrl, status: u16) {
        let from_origin = WOrigin::from_url(&state.url);
        let to_origin = WOrigin::from_url(&to);
        // Fetch "HTTP-redirect fetch": tainted-origin flag.
        if !to_origin.same_origin(&from_origin)
            && !ctx.initiator.same_origin(&from_origin)
            && state.tainted_origin.is_none()
        {
            state.tainted_origin = Some(WOrigin::new_opaque());
        }
        // Method and body: 301/302 turn POST into GET, 303 turns everything
        // but GET/HEAD into GET.
        let to_get = (matches!(status, 301 | 302) && state.method == "POST")
            || (status == 303 && !matches!(state.method.as_str(), "GET" | "HEAD"));
        if to_get {
            state.method = "GET".to_string();
            state.body.clear();
            state.headers.retain(|h| {
                let n = h.name.to_ascii_lowercase();
                !(n.starts_with("content-") || n == "origin")
            });
        }
        // Credentials-like author headers do not cross origins.
        if !to_origin.same_origin(&from_origin) {
            state
                .headers
                .retain(|h| !h.name.eq_ignore_ascii_case("authorization"));
        }
        state.url = to;
        state.redirect_count = state.redirect_count.saturating_add(1);
    }

    /// One request/response, up to (not including) following a redirect.
    fn hop(
        &mut self,
        ctx: &FetchContext,
        state: &mut HopState,
        chain: &mut Chain,
        out: &mut FetchOutcome,
    ) -> HopResult {
        let original = state.url.clone();
        let mut url = state.url.clone();

        // 0. syntax.
        if !matches!(url.scheme(), "http" | "https") || url.has_credentials() {
            out.deny(
                "net::scheme",
                ViolationKind::Request,
                &url,
                "only credential-free http(s) URLs are fetched",
            );
            return HopResult::Denied;
        }
        let Some(top_host) = ctx.top_level.host().map(|h| h.serialize()) else {
            out.deny(
                "net::partition",
                ViolationKind::Request,
                &url,
                "the top-level origin has no host",
            );
            return HopResult::Denied;
        };

        let upgrade = |url: &mut WUrl, out: &mut FetchOutcome| {
            if let Some(u) = url.upgraded_to_https() {
                if out.upgraded_from.is_none() {
                    out.upgraded_from = Some(original.clone());
                }
                *url = u;
            }
        };

        // 1. HSTS preload.
        if url.scheme() == "http"
            && let Some(list) = self.preload
            && let Some(host) = url.host()
            && list.requires_https(&host.serialize())
        {
            upgrade(&mut url, out);
        }

        // 2. CSP upgrade-insecure-requests.
        if url.scheme() == "http"
            && let Some(cu) = CspUrl::parse(&url.serialize(true))
        {
            let uctx = if ctx.destination.is_navigation() {
                if WOrigin::from_url(&url).same_origin(&ctx.initiator) {
                    csp::UpgradeContext::SameOriginNavigation
                } else {
                    csp::UpgradeContext::CrossOriginNavigation
                }
            } else {
                csp::UpgradeContext::Subresource
            };
            if ctx.csp.upgrade_insecure_url(&cu, uctx).is_some() {
                upgrade(&mut url, out);
            }
        }

        // 3. mixed content.
        if let Some(kind) = ctx.destination.mixed_kind() {
            match mixed::check_request(&ctx.document, kind, &url) {
                mixed::Verdict::Allow => {}
                mixed::Verdict::Upgrade(u) => {
                    if out.upgraded_from.is_none() {
                        out.upgraded_from = Some(original.clone());
                    }
                    url = u;
                }
                mixed::Verdict::Block => {
                    out.deny(
                        "mixed::check_request",
                        ViolationKind::MixedContent,
                        &url,
                        "blocked as mixed content",
                    );
                    return HopResult::Denied;
                }
            }
        }

        // From here the URL is the one that will actually be requested.
        state.url = url.clone();
        if let Some(last) = out.urls.last_mut() {
            *last = url.clone();
        }

        // 4 and 5. CSP.
        if let Some(blocked) = self.check_csp(ctx, state, &url, out) {
            return blocked;
        }

        // Partition key for the (possibly upgraded) destination.
        let Some(dest_origin) = net_origin(&url) else {
            out.deny(
                "net::partition",
                ViolationKind::Request,
                &url,
                "the destination is not representable as a partition origin",
            );
            return HopResult::Denied;
        };
        let Some(key) = PartitionKey::new(self.psl, &top_host, dest_origin) else {
            out.deny(
                "net::partition",
                ViolationKind::Request,
                &url,
                "the top-level site has no registrable domain",
            );
            return HopResult::Denied;
        };

        // 6. CORS tainting.
        let request_origin = state
            .tainted_origin
            .clone()
            .unwrap_or_else(|| ctx.initiator.clone());
        let creq = cors::CorsRequest {
            method: state.method.clone(),
            url: url.clone(),
            origin: request_origin,
            headers: state.headers.clone(),
            credentials: ctx.credentials,
            mode: ctx.mode,
            use_cors_preflight_flag: ctx.use_cors_preflight,
            origin_tainted: state.tainted_origin.is_some(),
        };
        let taint = match cors::taint(&creq) {
            Ok(t) => t,
            Err(e) => {
                out.deny(
                    "cors::taint",
                    ViolationKind::Cors(e.clone()),
                    &url,
                    format!("{e:?}"),
                );
                return HopResult::Denied;
            }
        };
        out.tainting = Some(taint);

        // 7. preflight.
        if taint == Tainting::Cors
            && ctx.mode == RequestMode::Cors
            && matches!(cors::classify(&creq), cors::Classification::Preflighted(_))
        {
            let partition = key.to_string();
            if self.preflight_cache.covers(&partition, &creq, self.now()) {
                out.preflight_cache_hits += 1;
            } else if let Some(denied) = self.preflight(ctx, state, &creq, &key, &partition, out) {
                return denied;
            }
        }

        // 8. headers: author headers, Origin, Referer, Cookie.
        let mut wire: Vec<(String, String)> = Vec::new();
        for h in &state.headers {
            let Ok(value) = String::from_utf8(h.value.clone()) else {
                out.deny(
                    "net::header_encoding",
                    ViolationKind::Request,
                    &url,
                    "a request header value is not UTF-8",
                );
                return HopResult::Denied;
            };
            wire.push((h.name.clone(), value));
        }
        if cors::should_send_origin_header(&creq, taint) {
            wire.push(("Origin".to_string(), cors::serialize_request_origin(&creq)));
        }
        let policy = state.redirect_referrer_policy.unwrap_or_else(|| {
            referrer::effective_policy(ctx.request_referrer_policy, ctx.document_referrer_policy)
        });
        if let Some(r) = referrer::compute(Some(&ctx.document_url), policy, &url) {
            wire.push(("Referer".to_string(), r));
        }
        let send_credentials = ctx.credentials == CredentialsMode::Include
            || (ctx.credentials == CredentialsMode::SameOrigin && taint == Tainting::Basic);
        let target = target_of(&url);
        if send_credentials {
            let cross_site = wsorigin::cross_site_for_cookies(
                &ctx.top_level,
                &WOrigin::from_url(&url),
                self.psl,
            );
            let cookies = self.jar.cookies_for(&key, url.path(), cross_site);
            if !cookies.is_empty() {
                let line = cookies
                    .iter()
                    .map(|c| format!("{}={}", c.name, c.value))
                    .collect::<Vec<_>>()
                    .join("; ");
                wire.push(("Cookie".to_string(), line));
            }
        }

        // 9. transport.
        let spec = self.spec(state, &target, wire);
        let response = match fetch::fetch_request(&key, &spec, self.preload) {
            Ok(r) => r,
            Err(e) => {
                out.error = Some(e);
                return HopResult::Denied;
            }
        };
        let status = response.head.status;
        let headers: Vec<cors::Header> = response
            .head
            .headers
            .iter()
            .map(|h| cors::Header::new(&h.name, &h.value))
            .collect();
        if send_credentials {
            for line in header_strs(&headers, "set-cookie").unwrap_or_default() {
                // A rejected cookie is dropped, never an error for the fetch.
                let _ = self.jar.store(self.psl, &key, line);
            }
        }
        let cresp = cors::CorsResponse {
            status,
            headers: headers.clone(),
        };

        // A redirect: CORS-check the redirect response, then follow.
        if is_redirect_status(status)
            && let Some(location) = response.head.header("location")
        {
            if taint == Tainting::Cors
                && let Err(e) = cors::validate_response(&creq, taint, &cresp)
            {
                out.deny(
                    "cors::validate_response",
                    ViolationKind::Cors(e.clone()),
                    &url,
                    format!("redirect response: {e:?}"),
                );
                return HopResult::Denied;
            }
            if let Some(p) = header_strs(&headers, "referrer-policy")
                .and_then(|v| v.last().copied().and_then(referrer::parse_header))
            {
                state.redirect_referrer_policy = Some(p);
            }
            let hop = match chain.follow(self.psl, None, &top_host, key.origin(), location) {
                Ok(h) => h,
                Err(e) => {
                    out.deny(
                        "net::redirect",
                        ViolationKind::Redirect(e.clone()),
                        &url,
                        e.to_string(),
                    );
                    return HopResult::Denied;
                }
            };
            let next = format!(
                "{}://{}:{}{}",
                hop.key.origin().scheme(),
                hop.key.origin().host(),
                hop.key.origin().port(),
                hop.path
            );
            let Some(next) = WUrl::parse(&next) else {
                out.deny(
                    "net::redirect",
                    ViolationKind::Redirect(RefusedRedirect::Unresolvable("unparsable")),
                    &url,
                    "the redirect target does not parse",
                );
                return HopResult::Denied;
            };
            return HopResult::Redirect { to: next, status };
        }

        // Response-side checks on the final response.
        self.finish(ctx, &creq, taint, cresp, response.body, out)
    }

    fn spec(&self, state: &HopState, target: &str, headers: Vec<(String, String)>) -> RequestSpec {
        RequestSpec {
            method: state.method.clone(),
            path: target.to_string(),
            headers,
            body: state.body.clone(),
            #[cfg(feature = "testing")]
            connect_to: self.connect_to,
        }
    }

    /// CSP blocking and report-only recording, then block-all-mixed-content.
    /// `Some` means the fetch is over.
    fn check_csp(
        &self,
        ctx: &FetchContext,
        state: &HopState,
        url: &WUrl,
        out: &mut FetchOutcome,
    ) -> Option<HopResult> {
        let Some(cu) = CspUrl::parse(&url.serialize(true)) else {
            out.deny(
                "csp::parse_url",
                ViolationKind::Csp,
                url,
                "the URL cannot be expressed to the CSP engine",
            );
            return Some(HopResult::Denied);
        };
        let chain_nodes: Vec<mixed::ContextNode> = std::iter::once(ctx.document.document.clone())
            .chain(ctx.document.ancestors.iter().cloned())
            .collect();
        let secure = coop::is_secure_context(&chain_nodes);
        let verdict = ctx.csp.check_mixed_content(&cu, secure);
        if verdict.is_blocked() {
            out.deny(
                "csp::block-all-mixed-content",
                ViolationKind::Csp,
                url,
                "block-all-mixed-content",
            );
            return Some(HopResult::Denied);
        }
        let kind = ctx.destination.csp_kind()?;
        let origin = csp_origin_of(&ctx.initiator);
        let opts = csp::RequestOptions {
            nonce: ctx.nonce.as_deref(),
            integrity: &[],
            parser_inserted: ctx.parser_inserted,
            redirect_count: state.redirect_count,
        };
        let verdict = ctx.csp.check_request(kind, &cu, &origin, &opts);
        for (_, b) in &verdict.reports {
            out.violations.push(Violation {
                point: b.effective_directive,
                kind: ViolationKind::Csp,
                enforced: false,
                url: url.serialize(true),
                detail: format!("report-only: {}", b.violated_directive),
            });
        }
        if let Some((_, b)) = verdict.blocked {
            out.deny(
                b.effective_directive,
                ViolationKind::Csp,
                url,
                format!("blocked by {}", b.violated_directive),
            );
            return Some(HopResult::Denied);
        }
        None
    }

    /// Send the CORS-preflight through the same transport and validate it.
    fn preflight(
        &mut self,
        ctx: &FetchContext,
        state: &HopState,
        creq: &cors::CorsRequest,
        key: &PartitionKey,
        partition: &str,
        out: &mut FetchOutcome,
    ) -> Option<HopResult> {
        let pre = cors::preflight_request(creq);
        let mut wire: Vec<(String, String)> = Vec::new();
        for h in &pre.headers {
            wire.push((
                h.name.clone(),
                String::from_utf8_lossy(&h.value).into_owned(),
            ));
        }
        wire.push(("Origin".to_string(), pre.origin_header.clone()));
        let policy = state.redirect_referrer_policy.unwrap_or_else(|| {
            referrer::effective_policy(ctx.request_referrer_policy, ctx.document_referrer_policy)
        });
        if let Some(r) = referrer::compute(Some(&ctx.document_url), policy, &creq.url) {
            wire.push(("Referer".to_string(), r));
        }
        let probe = HopState {
            url: creq.url.clone(),
            method: pre.method.clone(),
            headers: Vec::new(),
            body: Vec::new(),
            tainted_origin: None,
            redirect_count: 0,
            redirect_referrer_policy: None,
        };
        let spec = self.spec(&probe, &target_of(&creq.url), wire);
        out.preflights_sent += 1;
        let response = match fetch::fetch_request(key, &spec, self.preload) {
            Ok(r) => r,
            Err(e) => {
                out.error = Some(e);
                return Some(HopResult::Denied);
            }
        };
        // A redirect to a preflight is a network error (Fetch), so a 3xx
        // reaches validate_preflight as a non-2xx status and fails there.
        let cresp = cors::CorsResponse {
            status: response.head.status,
            headers: response
                .head
                .headers
                .iter()
                .map(|h| cors::Header::new(&h.name, &h.value))
                .collect(),
        };
        match cors::validate_preflight(creq, &cresp) {
            Ok(grant) => {
                self.preflight_cache
                    .store(partition, creq, &grant, self.now());
                None
            }
            Err(e) => {
                out.deny(
                    "cors::validate_preflight",
                    ViolationKind::Preflight(e.clone()),
                    &creq.url,
                    format!("{e:?}"),
                );
                Some(HopResult::Denied)
            }
        }
    }

    fn finish(
        &self,
        ctx: &FetchContext,
        creq: &cors::CorsRequest,
        taint: Tainting,
        cresp: cors::CorsResponse,
        body: Vec<u8>,
        out: &mut FetchOutcome,
    ) -> HopResult {
        let url = &creq.url;
        // r0. mixed content on the final URL.
        if ctx.destination.mixed_kind().is_some()
            && mixed::check_response(&ctx.document, url) == mixed::Verdict::Block
        {
            out.deny(
                "mixed::check_response",
                ViolationKind::MixedContent,
                url,
                "the final response is mixed content",
            );
            return HopResult::Denied;
        }
        // r1. CORS.
        if let Err(e) = cors::validate_response(creq, taint, &cresp) {
            out.deny(
                "cors::validate_response",
                ViolationKind::Cors(e.clone()),
                url,
                format!("{e:?}"),
            );
            return HopResult::Denied;
        }
        // r2. CORP / COEP.
        let corp_lines = header_strs(&cresp.headers, "cross-origin-resource-policy");
        let Some(corp_lines) = corp_lines else {
            out.deny(
                "coop::parse_corp",
                ViolationKind::Corp,
                url,
                "Cross-Origin-Resource-Policy is not text",
            );
            return HopResult::Denied;
        };
        let corp = coop::parse_corp(&corp_lines);
        let cr = coop::CorpResponse {
            url,
            corp,
            request_includes_credentials: ctx.credentials == CredentialsMode::Include,
        };
        let verdict = if ctx.destination == Destination::Iframe {
            coop::corp_check(&ctx.initiator, ctx.embedder_policy, &cr, true, self.psl)
        } else {
            coop::coep_check_response(&ctx.initiator, ctx.embedder_policy, taint, &cr, self.psl)
        };
        if verdict == coop::CorpVerdict::Blocked {
            out.deny(
                "coop::corp_check",
                ViolationKind::Corp,
                url,
                "blocked by Cross-Origin-Resource-Policy or COEP",
            );
            return HopResult::Denied;
        }
        // r3. SRI.
        if let Some(attr) = &ctx.integrity {
            match sri::parse(attr) {
                Err(e) => {
                    out.deny(
                        "sri::parse",
                        ViolationKind::Sri(e),
                        url,
                        "integrity too large",
                    );
                    return HopResult::Denied;
                }
                Ok(md) if md.needs_check() => {
                    if let Err(e) = sri::verify(&md, taint, &body) {
                        out.deny("sri::verify", ViolationKind::Sri(e), url, format!("{e:?}"));
                        return HopResult::Denied;
                    }
                }
                Ok(_) => {}
            }
        }
        // r4. framing and the new document's facts.
        if ctx.destination.is_navigation() {
            match self.navigation(ctx, url, &cresp.headers, out) {
                Ok(facts) => out.document = Some(facts),
                Err(()) => return HopResult::Denied,
            }
        }
        // r5. filter.
        let filtered = cors::filter_response(creq, taint, &cresp);
        out.response = Some(ResponseView {
            url: url.clone(),
            status: filtered.status,
            headers: filtered.headers,
            tainting: taint,
            body,
        });
        HopResult::Final
    }

    /// Framing checks (subframes only) and the new document's policies.
    fn navigation(
        &self,
        ctx: &FetchContext,
        url: &WUrl,
        headers: &[cors::Header],
        out: &mut FetchOutcome,
    ) -> Result<DocumentFacts, ()> {
        let enforce = header_lines(headers, "content-security-policy");
        let report_only = header_lines(headers, "content-security-policy-report-only");
        let policies = PolicySet::from_headers_fail_closed(&enforce, &report_only);
        let resp_origin = WOrigin::from_url(url);

        if ctx.destination == Destination::Iframe {
            let ancestors: Vec<&mixed::ContextNode> = std::iter::once(&ctx.document.document)
                .chain(ctx.document.ancestors.iter())
                .collect();
            let Some(cu) = CspUrl::parse(&url.serialize(true)) else {
                out.deny(
                    "csp::frame-ancestors",
                    ViolationKind::FrameAncestors,
                    url,
                    "the response URL cannot be expressed to the CSP engine",
                );
                return Err(());
            };
            let mut anc_urls = Vec::new();
            for a in &ancestors {
                match CspUrl::parse(&a.url.serialize(true)) {
                    Some(u) => anc_urls.push(u),
                    None => {
                        out.deny(
                            "csp::frame-ancestors",
                            ViolationKind::FrameAncestors,
                            url,
                            "an ancestor URL cannot be expressed to the CSP engine",
                        );
                        return Err(());
                    }
                }
            }
            let present = policies.policies().iter().any(|p| {
                p.disposition() == csp::Disposition::Enforce
                    && p.source() == csp::PolicySource::Header
                    && p.has("frame-ancestors")
            });
            let fa = if !present {
                frame::FrameAncestors::Absent
            } else if policies.check_frame_ancestors(&cu, &anc_urls).is_blocked() {
                frame::FrameAncestors::Blocked
            } else {
                frame::FrameAncestors::Allowed
            };
            let Some(xfo_lines) = header_strs(headers, "x-frame-options") else {
                out.deny(
                    "frame::xfo",
                    ViolationKind::FrameOptions,
                    url,
                    "X-Frame-Options is not text",
                );
                return Err(());
            };
            let anc_origins: Vec<WOrigin> = ancestors.iter().map(|a| a.origin.clone()).collect();
            let xfo = frame::check_xfo(&xfo_lines, &resp_origin, &anc_origins);
            if !frame::embedding_allowed(fa, xfo) {
                let (point, kind) = if fa == frame::FrameAncestors::Blocked {
                    ("csp::frame-ancestors", ViolationKind::FrameAncestors)
                } else {
                    ("frame::xfo", ViolationKind::FrameOptions)
                };
                out.deny(point, kind, url, "embedding refused");
                return Err(());
            }
        }

        let referrer_policy = header_strs(headers, "referrer-policy")
            .and_then(|v| v.last().copied().and_then(referrer::parse_header));
        let coep_header = header_strs(headers, "cross-origin-embedder-policy");
        let coop_header = header_strs(headers, "cross-origin-opener-policy");
        // Duplicated COEP/COOP are not a Structured Field item: ignore them,
        // which is the spec's failure value (unsafe-none).
        let one = |v: Option<Vec<&str>>| match v {
            Some(l) if l.len() == 1 => l.first().copied().map(str::to_string),
            _ => None,
        };
        let embedder_policy = coop::parse_coep(one(coep_header).as_deref());
        let opener_policy = coop::parse_coop(one(coop_header).as_deref(), embedder_policy.value);

        let attr = frame::parse_sandbox_attribute(ctx.iframe_sandbox.as_deref());
        let from_csp = policies.sandbox_flags();
        let sandbox = match (attr, from_csp) {
            (Some(a), Some(b)) => Some(a.intersect(b)),
            (a, b) => a.or(b),
        };
        let base = WOrigin::for_document(url, Some(&ctx.initiator));
        let origin = frame::sandboxed_origin(sandbox, &base);
        Ok(DocumentFacts {
            origin,
            csp: policies,
            referrer_policy,
            embedder_policy,
            opener_policy,
            sandbox,
        })
    }
}

fn target_of(url: &WUrl) -> String {
    match url.query() {
        Some(q) => format!("{}?{q}", url.path()),
        None => url.path().to_string(),
    }
}
