//! Enforcement: request, inline, eval, navigation and document-level checks.
//!
//! Every function here is an *enforcement point*, listed in
//! [`ENFORCEMENT_POINTS`]. The test module at the bottom drives each listed
//! point through a scenario and asserts the point was actually reached (a
//! test-only hit log is written by `mark`), so adding a directive check
//! without registering it, or registering one nothing calls, fails the build.
//!
//! The functions taking a single [`Policy`] are pure and ignore the policy's
//! disposition, except where the spec says report-only policies do not apply
//! a directive at all (`frame-ancestors`, `sandbox`, `upgrade-insecure-
//! requests`). [`PolicySet`] methods apply them to every policy of a global
//! and split the outcome into an enforced block and report-only violations.

use super::policy::{Disposition, Policy, PolicySet, PolicySource};
use super::sandbox::SandboxFlags;
use super::source::{HashValue, InlineType, Keyword, SourceList};
use super::url::{CspUrl, Origin, is_potentially_trustworthy};

#[cfg(test)]
thread_local! {
    static HITS: std::cell::RefCell<Vec<&'static str>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Record that an enforcement point ran (test builds only).
#[cfg(test)]
fn mark(name: &'static str) {
    HITS.with(|h| h.borrow_mut().push(name));
}
#[cfg(not(test))]
#[inline(always)]
fn mark(_name: &'static str) {}

/// What kind of enforcement point this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointKind {
    /// [`check_request`]: a fetch directive via the fallback chain.
    Fetch,
    /// [`check_inline`].
    Inline,
    /// [`check_eval`].
    Eval,
    /// A navigation directive.
    Navigation,
    /// A document-level directive.
    Document,
}

/// One enumerated enforcement point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnforcementPoint {
    /// Unique name; also the string `mark` records.
    pub name: &'static str,
    /// The entry-point function.
    pub entry: &'static str,
    /// The directive the point enforces (the effective directive for
    /// fetches).
    pub directive: &'static str,
    /// Category.
    pub kind: PointKind,
}

const fn ep(
    name: &'static str,
    entry: &'static str,
    directive: &'static str,
    kind: PointKind,
) -> EnforcementPoint {
    EnforcementPoint {
        name,
        entry,
        directive,
        kind,
    }
}

/// Every place CSP is enforced. Each has a dedicated test.
pub const ENFORCEMENT_POINTS: &[EnforcementPoint] = &[
    ep(
        "connect-src",
        "check_request",
        "connect-src",
        PointKind::Fetch,
    ),
    ep(
        "script-src-elem",
        "check_request",
        "script-src-elem",
        PointKind::Fetch,
    ),
    ep(
        "worker-src",
        "check_request",
        "worker-src",
        PointKind::Fetch,
    ),
    ep(
        "style-src-elem",
        "check_request",
        "style-src-elem",
        PointKind::Fetch,
    ),
    ep("img-src", "check_request", "img-src", PointKind::Fetch),
    ep("font-src", "check_request", "font-src", PointKind::Fetch),
    ep("media-src", "check_request", "media-src", PointKind::Fetch),
    ep(
        "object-src",
        "check_request",
        "object-src",
        PointKind::Fetch,
    ),
    ep("frame-src", "check_request", "frame-src", PointKind::Fetch),
    ep(
        "manifest-src",
        "check_request",
        "manifest-src",
        PointKind::Fetch,
    ),
    ep(
        "prefetch-src",
        "check_request",
        "prefetch-src",
        PointKind::Fetch,
    ),
    ep(
        "inline:script",
        "check_inline",
        "script-src-elem",
        PointKind::Inline,
    ),
    ep(
        "inline:script-attr",
        "check_inline",
        "script-src-attr",
        PointKind::Inline,
    ),
    ep(
        "inline:style",
        "check_inline",
        "style-src-elem",
        PointKind::Inline,
    ),
    ep(
        "inline:style-attr",
        "check_inline",
        "style-src-attr",
        PointKind::Inline,
    ),
    ep(
        "inline:navigation",
        "check_inline",
        "script-src-elem",
        PointKind::Inline,
    ),
    ep("unsafe-eval", "check_eval", "script-src", PointKind::Eval),
    ep(
        "wasm-unsafe-eval",
        "check_eval",
        "script-src",
        PointKind::Eval,
    ),
    ep(
        "form-action",
        "check_form_action",
        "form-action",
        PointKind::Navigation,
    ),
    ep(
        "frame-ancestors",
        "check_frame_ancestors",
        "frame-ancestors",
        PointKind::Navigation,
    ),
    ep(
        "base-uri",
        "check_base_uri",
        "base-uri",
        PointKind::Navigation,
    ),
    ep(
        "sandbox",
        "PolicySet::sandbox_flags",
        "sandbox",
        PointKind::Document,
    ),
    ep(
        "upgrade-insecure-requests",
        "upgrade_insecure_url",
        "upgrade-insecure-requests",
        PointKind::Document,
    ),
    ep(
        "block-all-mixed-content",
        "blocks_mixed_content",
        "block-all-mixed-content",
        PointKind::Document,
    ),
];

/// A CSP-governed kind of request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RequestKind {
    /// `fetch()`.
    Fetch,
    /// `XMLHttpRequest`.
    XmlHttpRequest,
    /// `navigator.sendBeacon`.
    Beacon,
    /// Hyperlink auditing (`<a ping>`).
    Ping,
    /// `EventSource`.
    EventSource,
    /// `WebSocket`.
    WebSocket,
    /// `<script src>`, `import()`, module scripts.
    Script,
    /// XSLT stylesheet.
    Xslt,
    /// Audio worklet module.
    AudioWorklet,
    /// Paint worklet module.
    PaintWorklet,
    /// Dedicated worker.
    Worker,
    /// Shared worker.
    SharedWorker,
    /// Service worker registration script.
    ServiceWorker,
    /// `<link rel=stylesheet>`, `@import`.
    Style,
    /// `<img>`, CSS images, favicons.
    Image,
    /// `@font-face`.
    Font,
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
    /// Nested navigation of a `<frame>`.
    Frame,
    /// Nested navigation of an `<iframe>`.
    Iframe,
    /// `<link rel=manifest>`.
    Manifest,
    /// `<link rel=prefetch>`.
    Prefetch,
}

impl RequestKind {
    /// Every variant, for exhaustive tests.
    pub const ALL: [RequestKind; 25] = [
        RequestKind::Fetch,
        RequestKind::XmlHttpRequest,
        RequestKind::Beacon,
        RequestKind::Ping,
        RequestKind::EventSource,
        RequestKind::WebSocket,
        RequestKind::Script,
        RequestKind::Xslt,
        RequestKind::AudioWorklet,
        RequestKind::PaintWorklet,
        RequestKind::Worker,
        RequestKind::SharedWorker,
        RequestKind::ServiceWorker,
        RequestKind::Style,
        RequestKind::Image,
        RequestKind::Font,
        RequestKind::Audio,
        RequestKind::Video,
        RequestKind::Track,
        RequestKind::Object,
        RequestKind::Embed,
        RequestKind::Frame,
        RequestKind::Iframe,
        RequestKind::Manifest,
        RequestKind::Prefetch,
    ];

    /// The effective directive (CSP3 §6.8.1).
    pub fn effective_directive(self) -> &'static str {
        use RequestKind::*;
        match self {
            Fetch | XmlHttpRequest | Beacon | Ping | EventSource | WebSocket => "connect-src",
            Script | Xslt | AudioWorklet | PaintWorklet => "script-src-elem",
            Worker | SharedWorker | ServiceWorker => "worker-src",
            Style => "style-src-elem",
            Image => "img-src",
            Font => "font-src",
            Audio | Video | Track => "media-src",
            Object | Embed => "object-src",
            Frame | Iframe => "frame-src",
            Manifest => "manifest-src",
            Prefetch => "prefetch-src",
        }
    }

    /// "Script-like" destinations (Fetch): scripts, workers, worklets, XSLT.
    pub fn is_script_like(self) -> bool {
        use RequestKind::*;
        matches!(
            self,
            Script | Xslt | AudioWorklet | PaintWorklet | Worker | SharedWorker | ServiceWorker
        )
    }
}

/// The directive fallback list (CSP3 §6.8.3) for an effective directive.
pub fn fallback_chain(effective: &str) -> &'static [&'static str] {
    match effective {
        "script-src-elem" => &["script-src-elem", "script-src", "default-src"],
        "script-src-attr" => &["script-src-attr", "script-src", "default-src"],
        "style-src-elem" => &["style-src-elem", "style-src", "default-src"],
        "style-src-attr" => &["style-src-attr", "style-src", "default-src"],
        "worker-src" => &["worker-src", "child-src", "script-src", "default-src"],
        "frame-src" => &["frame-src", "child-src", "default-src"],
        "connect-src" => &["connect-src", "default-src"],
        "img-src" => &["img-src", "default-src"],
        "font-src" => &["font-src", "default-src"],
        "media-src" => &["media-src", "default-src"],
        "object-src" => &["object-src", "default-src"],
        "manifest-src" => &["manifest-src", "default-src"],
        "prefetch-src" => &["prefetch-src", "default-src"],
        "script-src" => &["script-src", "default-src"],
        _ => &[],
    }
}

/// The directive that blocked, and the one the request mapped to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Blocked {
    /// Effective directive of the check (e.g. `script-src-elem`).
    pub effective_directive: &'static str,
    /// The directive actually present in the policy that did the blocking
    /// (e.g. `default-src`).
    pub violated_directive: &'static str,
}

/// Outcome of one check against one policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Not blocked by this policy.
    Allowed,
    /// Blocked, by this directive.
    Blocked(Blocked),
}

impl Decision {
    /// Whether this is a block.
    pub fn is_blocked(&self) -> bool {
        matches!(self, Decision::Blocked(_))
    }
}

/// Optional request metadata (Fetch "request" fields CSP reads).
#[derive(Debug, Clone, Copy, Default)]
pub struct RequestOptions<'a> {
    /// The request's cryptographic nonce metadata (the element's `nonce`).
    pub nonce: Option<&'a str>,
    /// Parsed integrity metadata (SRI) of the request.
    pub integrity: &'a [HashValue],
    /// Whether the request was initiated by the HTML parser (for
    /// `'strict-dynamic'`).
    pub parser_inserted: bool,
    /// Number of redirects followed; non-zero ignores path-parts.
    pub redirect_count: u32,
}

/// First directive of the fallback chain present in the policy, with its
/// parsed source list.
fn governing(policy: &Policy, effective: &str) -> Option<(&'static str, SourceList)> {
    for name in fallback_chain(effective) {
        if let Some(d) = policy.directive(name) {
            return Some((name, SourceList::parse(&d.values)));
        }
    }
    None
}

/// CSP3 §6.7.2 request check for the fetch directives, with the fallback
/// chain. A policy with no governing directive does not block.
pub fn check_request(
    policy: &Policy,
    kind: RequestKind,
    url: &CspUrl,
    origin: &Origin,
    opts: &RequestOptions<'_>,
) -> Decision {
    let eff = kind.effective_directive();
    mark(eff);
    let Some((name, list)) = governing(policy, eff) else {
        return Decision::Allowed;
    };
    let allowed = Decision::Allowed;
    let blocked = Decision::Blocked(Blocked {
        effective_directive: eff,
        violated_directive: name,
    });
    let script_directive = matches!(name, "script-src" | "script-src-elem");
    if script_directive && kind.is_script_like() {
        if opts.nonce.is_some_and(|n| list.nonce_matches(n)) {
            return allowed;
        }
        if list.has_hash()
            && !opts.integrity.is_empty()
            && opts.integrity.iter().all(|h| list.contains_hash(h))
        {
            return allowed;
        }
        if list.has_keyword(Keyword::StrictDynamic) {
            return if opts.parser_inserted {
                blocked
            } else {
                allowed
            };
        }
    }
    if kind == RequestKind::Style
        && matches!(name, "style-src" | "style-src-elem")
        && opts.nonce.is_some_and(|n| list.nonce_matches(n))
    {
        return allowed;
    }
    if list.matches_url(url, origin, opts.redirect_count) {
        allowed
    } else {
        blocked
    }
}

/// Convenience wrapper with the argument order of the phase brief:
/// `should_block_request(policy, request_kind, url, origin, options)`.
pub fn should_block_request(
    policy: &Policy,
    kind: RequestKind,
    url: &CspUrl,
    origin: &Origin,
    opts: &RequestOptions<'_>,
) -> Decision {
    check_request(policy, kind, url, origin, opts)
}

fn inline_names(ty: InlineType) -> (&'static str, &'static str) {
    match ty {
        InlineType::Script => ("inline:script", "script-src-elem"),
        InlineType::ScriptAttribute => ("inline:script-attr", "script-src-attr"),
        InlineType::Style => ("inline:style", "style-src-elem"),
        InlineType::StyleAttribute => ("inline:style-attr", "style-src-attr"),
        InlineType::Navigation => ("inline:navigation", "script-src-elem"),
    }
}

/// CSP3 §4.2.3 / §6.7.3.4 inline check: nonce, hash (for elements, or for
/// attributes and `javascript:` navigations when `'unsafe-hashes'` is
/// present), else `'unsafe-inline'` (disabled by any nonce/hash, and by
/// `'strict-dynamic'` for script-like types).
///
/// `source` is the text that is hashed: the element's text, the attribute
/// value, or the `javascript:` URL's script body.
pub fn check_inline(
    policy: &Policy,
    ty: InlineType,
    nonce: Option<&str>,
    source: &str,
) -> Decision {
    let (point, eff) = inline_names(ty);
    mark(point);
    let Some((name, list)) = governing(policy, eff) else {
        return Decision::Allowed;
    };
    let blocked = Decision::Blocked(Blocked {
        effective_directive: eff,
        violated_directive: name,
    });
    let element = matches!(ty, InlineType::Script | InlineType::Style);
    if element && nonce.is_some_and(|n| list.nonce_matches(n)) {
        return Decision::Allowed;
    }
    if (element || list.has_keyword(Keyword::UnsafeHashes)) && list.hash_matches_text(source) {
        return Decision::Allowed;
    }
    if list.allows_all_inline(ty) {
        Decision::Allowed
    } else {
        blocked
    }
}

/// What a string-to-code or Wasm compile request is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvalKind {
    /// `eval`, `new Function`, string timers.
    Eval,
    /// `WebAssembly.compile` and friends.
    Wasm,
}

/// CSP3 §4.2.4 `EnsureCSPDoesNotBlockStringCompilation` and the Wasm
/// variant. `'strict-dynamic'` does not allow eval. `'unsafe-hashes'` is not
/// consulted (string hashing for eval is not implemented; see crate docs).
pub fn check_eval(policy: &Policy, kind: EvalKind) -> Decision {
    mark(match kind {
        EvalKind::Eval => "unsafe-eval",
        EvalKind::Wasm => "wasm-unsafe-eval",
    });
    let Some((name, list)) = governing(policy, "script-src") else {
        return Decision::Allowed;
    };
    let ok = list.has_keyword(Keyword::UnsafeEval)
        || (kind == EvalKind::Wasm && list.has_keyword(Keyword::WasmUnsafeEval));
    if ok {
        Decision::Allowed
    } else {
        Decision::Blocked(Blocked {
            effective_directive: "script-src",
            violated_directive: name,
        })
    }
}

fn simple_directive_check(
    policy: &Policy,
    directive: &'static str,
    url: &CspUrl,
    origin: &Origin,
    redirect_count: u32,
) -> Decision {
    match policy.source_list(directive) {
        None => Decision::Allowed,
        Some(l) if l.matches_url(url, origin, redirect_count) => Decision::Allowed,
        Some(_) => Decision::Blocked(Blocked {
            effective_directive: directive,
            violated_directive: directive,
        }),
    }
}

/// `form-action` (CSP3 §6.3.3). No fallback to `default-src`.
pub fn check_form_action(
    policy: &Policy,
    url: &CspUrl,
    origin: &Origin,
    redirect_count: u32,
) -> Decision {
    mark("form-action");
    simple_directive_check(policy, "form-action", url, origin, redirect_count)
}

/// `base-uri` (CSP3 §6.2.1). No fallback to `default-src`; the check is
/// made with redirect count 0.
pub fn check_base_uri(policy: &Policy, url: &CspUrl, origin: &Origin) -> Decision {
    mark("base-uri");
    simple_directive_check(policy, "base-uri", url, origin, 0)
}

/// `frame-ancestors` (CSP3 §6.3.2): every ancestor, from the parent up to
/// the top, must match. `response_url` supplies `'self'`. The path-part of a
/// host-source is ignored (matches the redirect-count rule), so a path never
/// reveals or depends on an ancestor's URL. Not applied for report-only or
/// `<meta>` policies. An empty chain (top-level) is never a violation.
pub fn check_frame_ancestors(
    policy: &Policy,
    response_url: &CspUrl,
    ancestors: &[CspUrl],
) -> Decision {
    mark("frame-ancestors");
    if policy.disposition() == Disposition::Report || policy.source() == PolicySource::Meta {
        return Decision::Allowed;
    }
    let Some(list) = policy.source_list("frame-ancestors") else {
        return Decision::Allowed;
    };
    let origin = response_url.origin();
    let ok = ancestors.iter().all(|a| list.matches_url(a, &origin, 1));
    if ok {
        Decision::Allowed
    } else {
        Decision::Blocked(Blocked {
            effective_directive: "frame-ancestors",
            violated_directive: "frame-ancestors",
        })
    }
}

/// Context for `upgrade-insecure-requests`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpgradeContext {
    /// A subresource or nested-frame request: always upgraded.
    Subresource,
    /// A navigation to a URL that is same-origin with the navigating
    /// document: upgraded.
    SameOriginNavigation,
    /// A cross-origin navigation: **not** upgraded (UIR §4.1), the user is
    /// leaving the site.
    CrossOriginNavigation,
}

/// `upgrade-insecure-requests`: `http`→`https`, `ws`→`wss`, and port 80 →
/// 443 (an explicit non-80 port is kept). Returns the upgraded URL, or
/// `None` when no upgrade applies. Ignored for report-only policies.
pub fn upgrade_insecure_url(policy: &Policy, url: &CspUrl, ctx: UpgradeContext) -> Option<CspUrl> {
    mark("upgrade-insecure-requests");
    if policy.disposition() == Disposition::Report
        || !policy.has("upgrade-insecure-requests")
        || ctx == UpgradeContext::CrossOriginNavigation
    {
        return None;
    }
    let new_scheme = match url.scheme() {
        "http" => "https",
        "ws" => "wss",
        _ => return None,
    };
    let host = url.host()?;
    // The original port 80 is "default" and was normalised away to None.
    let port = url.port();
    CspUrl::from_parts(new_scheme, host, port, url.path())
}

/// `block-all-mixed-content` (deprecated, still parsed): blocks any request
/// to a non-potentially-trustworthy URL made from a secure document.
pub fn blocks_mixed_content(policy: &Policy, url: &CspUrl, document_is_secure: bool) -> Decision {
    mark("block-all-mixed-content");
    if policy.has("block-all-mixed-content")
        && document_is_secure
        && !is_potentially_trustworthy(url)
    {
        Decision::Blocked(Blocked {
            effective_directive: "block-all-mixed-content",
            violated_directive: "block-all-mixed-content",
        })
    } else {
        Decision::Allowed
    }
}

/// Result of checking one operation against every policy of a global.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Verdict {
    /// The first enforced policy that blocked: its index in the set and why.
    pub blocked: Option<(usize, Blocked)>,
    /// Report-only policies that would have blocked.
    pub reports: Vec<(usize, Blocked)>,
}

impl Verdict {
    /// Whether an enforced policy blocked.
    pub fn is_blocked(&self) -> bool {
        self.blocked.is_some()
    }
}

impl PolicySet {
    fn evaluate<F: Fn(&Policy) -> Decision>(&self, f: F) -> Verdict {
        let mut v = Verdict::default();
        for (i, p) in self.policies().iter().enumerate() {
            if let Decision::Blocked(b) = f(p) {
                match p.disposition() {
                    Disposition::Enforce => {
                        if v.blocked.is_none() {
                            v.blocked = Some((i, b));
                        }
                    }
                    Disposition::Report => v.reports.push((i, b)),
                }
            }
        }
        v
    }

    /// [`check_request`] against every policy.
    pub fn check_request(
        &self,
        kind: RequestKind,
        url: &CspUrl,
        origin: &Origin,
        opts: &RequestOptions<'_>,
    ) -> Verdict {
        self.evaluate(|p| check_request(p, kind, url, origin, opts))
    }

    /// [`check_inline`] against every policy.
    pub fn check_inline(&self, ty: InlineType, nonce: Option<&str>, source: &str) -> Verdict {
        self.evaluate(|p| check_inline(p, ty, nonce, source))
    }

    /// [`check_eval`] against every policy.
    pub fn check_eval(&self, kind: EvalKind) -> Verdict {
        self.evaluate(|p| check_eval(p, kind))
    }

    /// [`check_form_action`] against every policy.
    pub fn check_form_action(&self, url: &CspUrl, origin: &Origin, redirects: u32) -> Verdict {
        self.evaluate(|p| check_form_action(p, url, origin, redirects))
    }

    /// [`check_base_uri`] against every policy.
    pub fn check_base_uri(&self, url: &CspUrl, origin: &Origin) -> Verdict {
        self.evaluate(|p| check_base_uri(p, url, origin))
    }

    /// [`check_frame_ancestors`] against every policy.
    pub fn check_frame_ancestors(&self, response_url: &CspUrl, ancestors: &[CspUrl]) -> Verdict {
        self.evaluate(|p| check_frame_ancestors(p, response_url, ancestors))
    }

    /// [`blocks_mixed_content`] against every policy.
    pub fn check_mixed_content(&self, url: &CspUrl, document_is_secure: bool) -> Verdict {
        self.evaluate(|p| blocks_mixed_content(p, url, document_is_secure))
    }

    /// The sandbox flags the document must run under: the intersection of
    /// every enforced policy's `sandbox` directive. `None` means no policy
    /// sandboxes the document.
    pub fn sandbox_flags(&self) -> Option<SandboxFlags> {
        mark("sandbox");
        let mut acc: Option<SandboxFlags> = None;
        for p in self.policies() {
            if let Some(f) = p.sandbox_flags() {
                acc = Some(match acc {
                    Some(a) => a.intersect(f),
                    None => f,
                });
            }
        }
        acc
    }

    /// Upgrade `url` if any enforced policy asks for it.
    pub fn upgrade_insecure_url(&self, url: &CspUrl, ctx: UpgradeContext) -> Option<CspUrl> {
        self.policies()
            .iter()
            .find_map(|p| upgrade_insecure_url(p, url, ctx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pol(s: &str) -> Policy {
        Policy::parse(s.as_bytes(), Disposition::Enforce, PolicySource::Header).unwrap()
    }
    fn url(s: &str) -> CspUrl {
        CspUrl::parse(s).unwrap()
    }
    fn page() -> Origin {
        Origin::from_url_str("https://page.example/")
    }
    fn req(p: &str, kind: RequestKind, u: &str) -> Decision {
        check_request(&pol(p), kind, &url(u), &page(), &RequestOptions::default())
    }
    fn blocked_by(d: Decision) -> Option<&'static str> {
        match d {
            Decision::Blocked(b) => Some(b.violated_directive),
            Decision::Allowed => None,
        }
    }
    fn take_hits() -> Vec<&'static str> {
        HITS.with(|h| std::mem::take(&mut *h.borrow_mut()))
    }

    // ---- one scenario per enforcement point -------------------------------

    /// Returns true if the scenario's check blocked what it should and
    /// allowed what it should.
    fn scenario(name: &str) -> bool {
        match name {
            "connect-src" => fetch_pair("connect-src", RequestKind::Fetch),
            "script-src-elem" => fetch_pair("script-src-elem", RequestKind::Script),
            "worker-src" => fetch_pair("worker-src", RequestKind::Worker),
            "style-src-elem" => fetch_pair("style-src-elem", RequestKind::Style),
            "img-src" => fetch_pair("img-src", RequestKind::Image),
            "font-src" => fetch_pair("font-src", RequestKind::Font),
            "media-src" => fetch_pair("media-src", RequestKind::Video),
            "object-src" => fetch_pair("object-src", RequestKind::Object),
            "frame-src" => fetch_pair("frame-src", RequestKind::Iframe),
            "manifest-src" => fetch_pair("manifest-src", RequestKind::Manifest),
            "prefetch-src" => fetch_pair("prefetch-src", RequestKind::Prefetch),
            "inline:script" => inline_pair("script-src-elem", InlineType::Script),
            "inline:script-attr" => inline_pair("script-src-attr", InlineType::ScriptAttribute),
            "inline:style" => inline_pair("style-src-elem", InlineType::Style),
            "inline:style-attr" => inline_pair("style-src-attr", InlineType::StyleAttribute),
            "inline:navigation" => inline_pair("script-src-elem", InlineType::Navigation),
            "unsafe-eval" => {
                check_eval(&pol("script-src 'self'"), EvalKind::Eval).is_blocked()
                    && !check_eval(&pol("script-src 'unsafe-eval'"), EvalKind::Eval).is_blocked()
            }
            "wasm-unsafe-eval" => {
                check_eval(&pol("script-src 'self'"), EvalKind::Wasm).is_blocked()
                    && !check_eval(&pol("script-src 'wasm-unsafe-eval'"), EvalKind::Wasm)
                        .is_blocked()
            }
            "form-action" => {
                let p = pol("form-action 'self'");
                check_form_action(&p, &url("https://evil.test/"), &page(), 0).is_blocked()
                    && !check_form_action(&p, &url("https://page.example/s"), &page(), 0)
                        .is_blocked()
            }
            "frame-ancestors" => {
                let p = pol("frame-ancestors 'self'");
                let me = url("https://page.example/doc");
                check_frame_ancestors(&p, &me, &[url("https://evil.test/")]).is_blocked()
                    && !check_frame_ancestors(&p, &me, &[url("https://page.example/")]).is_blocked()
            }
            "base-uri" => {
                let p = pol("base-uri 'none'");
                check_base_uri(&p, &url("https://page.example/"), &page()).is_blocked()
                    && !check_base_uri(&pol("img-src a"), &url("https://x.test/"), &page())
                        .is_blocked()
            }
            "sandbox" => {
                let mut s = PolicySet::new();
                s.push(pol("sandbox allow-forms"));
                s.sandbox_flags().is_some() && PolicySet::new().sandbox_flags().is_none()
            }
            "upgrade-insecure-requests" => {
                let p = pol("upgrade-insecure-requests");
                check_upgrade(&p)
            }
            "block-all-mixed-content" => {
                let p = pol("block-all-mixed-content");
                blocks_mixed_content(&p, &url("http://a.test/"), true).is_blocked()
                    && !blocks_mixed_content(&p, &url("https://a.test/"), true).is_blocked()
            }
            _ => false,
        }
    }

    fn check_upgrade(p: &Policy) -> bool {
        upgrade_insecure_url(p, &url("http://a.test/x"), UpgradeContext::Subresource).is_some()
            && upgrade_insecure_url(p, &url("https://a.test/x"), UpgradeContext::Subresource)
                .is_none()
    }

    /// directive `d` set to 'none' blocks, absent directive does not.
    fn fetch_pair(d: &str, kind: RequestKind) -> bool {
        let blocked = check_request(
            &pol(&format!("{d} 'none'")),
            kind,
            &url("https://page.example/x"),
            &page(),
            &RequestOptions::default(),
        );
        let allowed = check_request(
            &pol(&format!("{d} 'self'")),
            kind,
            &url("https://page.example/x"),
            &page(),
            &RequestOptions::default(),
        );
        let unrelated = check_request(
            &pol("sandbox"),
            kind,
            &url("https://page.example/x"),
            &page(),
            &RequestOptions::default(),
        );
        blocked
            == Decision::Blocked(Blocked {
                effective_directive: kind.effective_directive(),
                violated_directive: static_name(d),
            })
            && allowed == Decision::Allowed
            && unrelated == Decision::Allowed
    }

    fn inline_pair(d: &str, ty: InlineType) -> bool {
        check_inline(&pol(&format!("{d} 'none'")), ty, None, "x").is_blocked()
            && !check_inline(&pol(&format!("{d} 'unsafe-inline'")), ty, None, "x").is_blocked()
            && !check_inline(&pol("sandbox"), ty, None, "x").is_blocked()
    }

    fn static_name(d: &str) -> &'static str {
        fallback_chain(d).first().copied().unwrap_or("")
    }

    #[test]
    fn every_enforcement_point_has_a_scenario_that_reaches_it() {
        let mut names = std::collections::HashSet::new();
        for point in ENFORCEMENT_POINTS {
            assert!(names.insert(point.name), "duplicate point {}", point.name);
            take_hits();
            assert!(scenario(point.name), "scenario failed for {}", point.name);
            assert!(
                take_hits().contains(&point.name),
                "scenario for {} never reached its enforcement point",
                point.name
            );
        }
        assert_eq!(names.len(), ENFORCEMENT_POINTS.len());
    }

    #[test]
    fn every_request_kind_is_covered_by_a_registered_fetch_point() {
        let fetch_points: Vec<&str> = ENFORCEMENT_POINTS
            .iter()
            .filter(|p| p.kind == PointKind::Fetch)
            .map(|p| p.directive)
            .collect();
        for k in RequestKind::ALL {
            assert!(
                fetch_points.contains(&k.effective_directive()),
                "{k:?} maps to unregistered directive {}",
                k.effective_directive()
            );
            assert!(!fallback_chain(k.effective_directive()).is_empty());
        }
        // and the table lists every variant exactly once
        let set: std::collections::HashSet<_> = RequestKind::ALL.iter().collect();
        assert_eq!(set.len(), RequestKind::ALL.len());
    }

    #[test]
    fn every_registered_directive_is_a_known_directive_name() {
        use crate::csp::policy::KNOWN_DIRECTIVES;
        for p in ENFORCEMENT_POINTS {
            assert!(KNOWN_DIRECTIVES.contains(&p.directive), "{}", p.directive);
        }
    }

    // ---- request kind -> directive, each kind individually -----------------

    #[test]
    fn each_request_kind_hits_its_own_directive() {
        use RequestKind::*;
        let table = [
            (Fetch, "connect-src"),
            (XmlHttpRequest, "connect-src"),
            (Beacon, "connect-src"),
            (Ping, "connect-src"),
            (EventSource, "connect-src"),
            (WebSocket, "connect-src"),
            (Script, "script-src-elem"),
            (Xslt, "script-src-elem"),
            (AudioWorklet, "script-src-elem"),
            (PaintWorklet, "script-src-elem"),
            (Worker, "worker-src"),
            (SharedWorker, "worker-src"),
            (ServiceWorker, "worker-src"),
            (Style, "style-src-elem"),
            (Image, "img-src"),
            (Font, "font-src"),
            (Audio, "media-src"),
            (Video, "media-src"),
            (Track, "media-src"),
            (Object, "object-src"),
            (Embed, "object-src"),
            (Frame, "frame-src"),
            (Iframe, "frame-src"),
            (Manifest, "manifest-src"),
            (Prefetch, "prefetch-src"),
        ];
        assert_eq!(table.len(), RequestKind::ALL.len());
        for (k, d) in table {
            assert_eq!(k.effective_directive(), d, "{k:?}");
            // blocked by exactly that directive and by default-src
            assert_eq!(
                blocked_by(req(&format!("{d} 'none'"), k, "https://page.example/")),
                Some(static_name(d)),
                "{k:?}"
            );
            assert_eq!(
                blocked_by(req("default-src 'none'", k, "https://page.example/")),
                Some("default-src"),
                "{k:?}"
            );
            // an unrelated directive does not
            assert_eq!(
                blocked_by(req("form-action 'none'", k, "https://x.test/")),
                None
            );
        }
    }

    // ---- fallback chains (CSP3 §6.8.3) -------------------------------------

    #[test]
    fn fallback_script_src_elem() {
        let u = "https://evil.test/a.js";
        assert_eq!(
            blocked_by(req(
                "script-src-elem 'none'; script-src *",
                RequestKind::Script,
                u
            )),
            Some("script-src-elem")
        );
        assert_eq!(
            blocked_by(req(
                "script-src 'none'; default-src *",
                RequestKind::Script,
                u
            )),
            Some("script-src")
        );
        assert_eq!(
            blocked_by(req("default-src 'none'", RequestKind::Script, u)),
            Some("default-src")
        );
        assert_eq!(
            blocked_by(req(
                "script-src-elem *; script-src 'none'",
                RequestKind::Script,
                u
            )),
            None
        );
        assert_eq!(
            blocked_by(req("img-src 'none'", RequestKind::Script, u)),
            None
        );
    }

    #[test]
    fn fallback_worker_src_chain() {
        let u = "https://evil.test/w.js";
        let w = RequestKind::Worker;
        assert_eq!(
            blocked_by(req("worker-src 'none'; child-src *; script-src *", w, u)),
            Some("worker-src")
        );
        assert_eq!(
            blocked_by(req("child-src 'none'; script-src *; default-src *", w, u)),
            Some("child-src")
        );
        assert_eq!(
            blocked_by(req("script-src 'none'; default-src *", w, u)),
            Some("script-src")
        );
        assert_eq!(
            blocked_by(req("default-src 'none'", w, u)),
            Some("default-src")
        );
        assert_eq!(
            blocked_by(req("worker-src *; default-src 'none'", w, u)),
            None
        );
        // script-src-elem is NOT in the worker chain
        assert_eq!(blocked_by(req("script-src-elem 'none'", w, u)), None);
    }

    #[test]
    fn fallback_frame_src_chain() {
        let u = "https://evil.test/";
        let f = RequestKind::Iframe;
        assert_eq!(
            blocked_by(req("frame-src 'none'; child-src *", f, u)),
            Some("frame-src")
        );
        assert_eq!(
            blocked_by(req("child-src 'none'; default-src *", f, u)),
            Some("child-src")
        );
        assert_eq!(
            blocked_by(req("default-src 'none'", f, u)),
            Some("default-src")
        );
        // script-src is not in the frame chain
        assert_eq!(blocked_by(req("script-src 'none'", f, u)), None);
    }

    #[test]
    fn fallback_style_chain_and_attr_chains() {
        let u = "https://evil.test/s.css";
        let s = RequestKind::Style;
        assert_eq!(
            blocked_by(req("style-src-elem 'none'; style-src *", s, u)),
            Some("style-src-elem")
        );
        assert_eq!(
            blocked_by(req("style-src 'none'; default-src *", s, u)),
            Some("style-src")
        );
        assert_eq!(
            blocked_by(req("default-src 'none'", s, u)),
            Some("default-src")
        );
        assert_eq!(
            fallback_chain("script-src-attr"),
            ["script-src-attr", "script-src", "default-src"]
        );
        assert_eq!(
            fallback_chain("style-src-attr"),
            ["style-src-attr", "style-src", "default-src"]
        );
        assert_eq!(fallback_chain("bogus"), [] as [&str; 0]);
    }

    #[test]
    fn empty_directive_value_blocks_it_does_not_fall_back() {
        // `img-src` with no tokens is an empty source list: matches nothing,
        // and does NOT fall through to default-src.
        assert_eq!(
            blocked_by(req(
                "img-src; default-src *",
                RequestKind::Image,
                "https://a.test/"
            )),
            Some("img-src")
        );
    }

    // ---- script requests: nonce, integrity, strict-dynamic ------------------

    fn script_req(p: &str, opts: RequestOptions<'_>, u: &str) -> Decision {
        check_request(&pol(p), RequestKind::Script, &url(u), &page(), &opts)
    }

    #[test]
    fn script_nonce_allows_regardless_of_host() {
        let o = RequestOptions {
            nonce: Some("abc"),
            ..Default::default()
        };
        assert_eq!(
            script_req("script-src 'nonce-abc'", o, "https://evil.test/x.js"),
            Decision::Allowed
        );
        let o = RequestOptions {
            nonce: Some("abd"),
            ..Default::default()
        };
        assert!(script_req("script-src 'nonce-abc'", o, "https://evil.test/x.js").is_blocked());
        let o = RequestOptions {
            nonce: Some(""),
            ..Default::default()
        };
        assert!(script_req("script-src 'nonce-abc'", o, "https://evil.test/x.js").is_blocked());
        assert!(
            script_req(
                "script-src 'nonce-abc'",
                RequestOptions::default(),
                "https://evil.test/x.js"
            )
            .is_blocked()
        );
        // nonce does not allow a *style* request under script-src... and
        // does not cover non-script requests at all
        let o = RequestOptions {
            nonce: Some("abc"),
            ..Default::default()
        };
        assert!(
            check_request(
                &pol("img-src 'nonce-abc'"),
                RequestKind::Image,
                &url("https://evil.test/i.png"),
                &page(),
                &o
            )
            .is_blocked()
        );
    }

    #[test]
    fn style_nonce_allows_stylesheet() {
        let o = RequestOptions {
            nonce: Some("abc"),
            ..Default::default()
        };
        let d = check_request(
            &pol("style-src 'nonce-abc'"),
            RequestKind::Style,
            &url("https://evil.test/s.css"),
            &page(),
            &o,
        );
        assert_eq!(d, Decision::Allowed);
        let d = check_request(
            &pol("style-src 'nonce-abd'"),
            RequestKind::Style,
            &url("https://evil.test/s.css"),
            &page(),
            &o,
        );
        assert!(d.is_blocked());
    }

    #[test]
    fn script_integrity_hash_allows_when_all_hashes_listed() {
        let abc = HashValue::parse("sha256-ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0=").unwrap();
        let other =
            HashValue::parse("sha256-47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=").unwrap();
        let p = "script-src 'sha256-ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0='";
        let o = RequestOptions {
            integrity: std::slice::from_ref(&abc),
            ..Default::default()
        };
        assert_eq!(
            script_req(p, o, "https://evil.test/x.js"),
            Decision::Allowed
        );
        let both = [abc.clone(), other.clone()];
        let o = RequestOptions {
            integrity: &both,
            ..Default::default()
        };
        assert!(
            script_req(p, o, "https://evil.test/x.js").is_blocked(),
            "every integrity hash must be listed"
        );
        let o = RequestOptions {
            integrity: std::slice::from_ref(&other),
            ..Default::default()
        };
        assert!(script_req(p, o, "https://evil.test/x.js").is_blocked());
        assert!(script_req(p, RequestOptions::default(), "https://evil.test/x.js").is_blocked());
    }

    #[test]
    fn strict_dynamic_parser_inserted_rule() {
        let p = "script-src 'strict-dynamic' 'nonce-abc' https://good.test";
        // parser-inserted without nonce: blocked even though host matches
        let o = RequestOptions {
            parser_inserted: true,
            ..Default::default()
        };
        assert!(script_req(p, o, "https://good.test/x.js").is_blocked());
        // non-parser-inserted (added by a trusted script): allowed, any host
        let o = RequestOptions {
            parser_inserted: false,
            ..Default::default()
        };
        assert_eq!(
            script_req(p, o, "https://anywhere.test/x.js"),
            Decision::Allowed
        );
        // parser-inserted with the nonce: allowed
        let o = RequestOptions {
            parser_inserted: true,
            nonce: Some("abc"),
            ..Default::default()
        };
        assert_eq!(
            script_req(p, o, "https://anywhere.test/x.js"),
            Decision::Allowed
        );
        // 'strict-dynamic' does not loosen non-script requests
        assert!(
            req(
                "script-src 'strict-dynamic'; img-src 'none'",
                RequestKind::Image,
                "https://a.test/"
            )
            .is_blocked()
        );
        // and a worker request falling back to script-src is treated as script
        let o = RequestOptions {
            parser_inserted: false,
            ..Default::default()
        };
        assert_eq!(
            check_request(
                &pol(p),
                RequestKind::Worker,
                &url("https://a.test/w.js"),
                &page(),
                &o
            ),
            Decision::Allowed
        );
        // but worker-src itself is a plain source list
        let q = "worker-src 'none'; script-src 'strict-dynamic'";
        assert!(
            check_request(
                &pol(q),
                RequestKind::Worker,
                &url("https://a.test/w.js"),
                &page(),
                &o
            )
            .is_blocked()
        );
    }

    #[test]
    fn redirects_ignore_path_in_request_check() {
        let p = pol("img-src https://a.test/only.png");
        let o0 = RequestOptions::default();
        let o1 = RequestOptions {
            redirect_count: 1,
            ..Default::default()
        };
        let u = url("https://a.test/other.png");
        assert!(check_request(&p, RequestKind::Image, &u, &page(), &o0).is_blocked());
        assert!(!check_request(&p, RequestKind::Image, &u, &page(), &o1).is_blocked());
    }

    #[test]
    fn should_block_request_is_check_request() {
        let p = pol("img-src 'none'");
        let u = url("https://a.test/");
        assert_eq!(
            should_block_request(
                &p,
                RequestKind::Image,
                &u,
                &page(),
                &RequestOptions::default()
            ),
            check_request(
                &p,
                RequestKind::Image,
                &u,
                &page(),
                &RequestOptions::default()
            )
        );
    }

    // ---- inline checks (CSP3 §6.7.3) ----------------------------------------

    const ABC_SHA256: &str = "'sha256-ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0='";

    #[test]
    fn inline_nonce_hash_unsafe_inline() {
        use InlineType::*;
        let ok =
            |p: &str, t, n: Option<&str>, s: &str| !check_inline(&pol(p), t, n, s).is_blocked();
        assert!(!ok("script-src 'self'", Script, None, "abc"));
        assert!(ok("script-src 'unsafe-inline'", Script, None, "abc"));
        assert!(ok("script-src 'nonce-n1'", Script, Some("n1"), "abc"));
        assert!(!ok("script-src 'nonce-n1'", Script, Some("n2"), "abc"));
        assert!(!ok("script-src 'nonce-n1'", Script, None, "abc"));
        assert!(!ok("script-src 'nonce-n1'", Script, Some(""), "abc"));
        assert!(ok(&format!("script-src {ABC_SHA256}"), Script, None, "abc"));
        assert!(
            !ok(&format!("script-src {ABC_SHA256}"), Script, None, "abc "),
            "whitespace matters"
        );
        assert!(!ok(
            &format!("script-src {ABC_SHA256}"),
            Script,
            None,
            "ABC"
        ));
        assert!(ok(&format!("style-src {ABC_SHA256}"), Style, None, "abc"));
        // hash/nonce disables 'unsafe-inline'
        assert!(!ok(
            &format!("script-src 'unsafe-inline' {ABC_SHA256}"),
            Script,
            None,
            "other"
        ));
        assert!(!ok(
            "style-src 'unsafe-inline' 'nonce-n1'",
            Style,
            None,
            "x"
        ));
        assert!(ok(
            "style-src 'unsafe-inline' 'nonce-n1'",
            Style,
            Some("n1"),
            "x"
        ));
        // strict-dynamic disables unsafe-inline for scripts, not styles
        assert!(!ok(
            "script-src 'unsafe-inline' 'strict-dynamic'",
            Script,
            None,
            "x"
        ));
        assert!(ok(
            "style-src 'unsafe-inline' 'strict-dynamic'",
            Style,
            None,
            "x"
        ));
        // a nonce does not apply to attributes
        assert!(!ok(
            "script-src 'nonce-n1' 'unsafe-inline'",
            ScriptAttribute,
            Some("n1"),
            "x"
        ));
    }

    #[test]
    fn inline_unsafe_hashes_for_attributes_and_navigation() {
        use InlineType::*;
        let hashed = format!("script-src {ABC_SHA256}");
        for t in [ScriptAttribute, Navigation] {
            assert!(
                check_inline(&pol(&hashed), t, None, "abc").is_blocked(),
                "{t:?}: no unsafe-hashes"
            );
            let with = format!("script-src 'unsafe-hashes' {ABC_SHA256}");
            assert!(
                !check_inline(&pol(&with), t, None, "abc").is_blocked(),
                "{t:?}"
            );
            assert!(
                check_inline(&pol(&with), t, None, "abd").is_blocked(),
                "{t:?}"
            );
        }
        let with = format!("style-src 'unsafe-hashes' {ABC_SHA256}");
        assert!(!check_inline(&pol(&with), StyleAttribute, None, "abc").is_blocked());
        assert!(
            check_inline(
                &pol(&format!("style-src {ABC_SHA256}")),
                StyleAttribute,
                None,
                "abc"
            )
            .is_blocked()
        );
    }

    #[test]
    fn inline_fallback_to_script_src_and_default_src() {
        use InlineType::*;
        assert_eq!(
            blocked_by(check_inline(
                &pol("script-src-elem 'none'; script-src 'unsafe-inline'"),
                Script,
                None,
                "x"
            )),
            Some("script-src-elem")
        );
        assert_eq!(
            blocked_by(check_inline(
                &pol("script-src 'none'; default-src 'unsafe-inline'"),
                Script,
                None,
                "x"
            )),
            Some("script-src")
        );
        assert_eq!(
            blocked_by(check_inline(
                &pol("default-src 'none'"),
                ScriptAttribute,
                None,
                "x"
            )),
            Some("default-src")
        );
        assert_eq!(
            blocked_by(check_inline(
                &pol("default-src 'none'"),
                StyleAttribute,
                None,
                "x"
            )),
            Some("default-src")
        );
        assert_eq!(
            blocked_by(check_inline(
                &pol("style-src-attr 'none'; style-src 'unsafe-inline'"),
                StyleAttribute,
                None,
                "x"
            )),
            Some("style-src-attr")
        );
        // attr falls back to script-src, not to script-src-elem
        assert_eq!(
            blocked_by(check_inline(
                &pol("script-src-elem 'unsafe-inline'; script-src 'none'"),
                ScriptAttribute,
                None,
                "x"
            )),
            Some("script-src")
        );
        // no relevant directive: allowed
        assert!(!check_inline(&pol("img-src 'none'"), Script, None, "x").is_blocked());
    }

    // ---- eval --------------------------------------------------------------

    #[test]
    fn eval_rules() {
        assert!(!check_eval(&pol("img-src 'none'"), EvalKind::Eval).is_blocked());
        assert!(check_eval(&pol("script-src 'self'"), EvalKind::Eval).is_blocked());
        assert!(check_eval(&pol("default-src 'self'"), EvalKind::Eval).is_blocked());
        assert_eq!(
            blocked_by(check_eval(&pol("default-src 'self'"), EvalKind::Eval)),
            Some("default-src")
        );
        assert!(!check_eval(&pol("default-src 'unsafe-eval'"), EvalKind::Eval).is_blocked());
        // script-src overrides default-src
        assert!(
            check_eval(
                &pol("default-src 'unsafe-eval'; script-src 'self'"),
                EvalKind::Eval
            )
            .is_blocked()
        );
        // strict-dynamic does not allow eval; wasm-unsafe-eval does not allow eval
        assert!(
            check_eval(
                &pol("script-src 'strict-dynamic' 'nonce-a'"),
                EvalKind::Eval
            )
            .is_blocked()
        );
        assert!(check_eval(&pol("script-src 'wasm-unsafe-eval'"), EvalKind::Eval).is_blocked());
        // but unsafe-eval allows wasm, and wasm-unsafe-eval allows wasm
        assert!(!check_eval(&pol("script-src 'unsafe-eval'"), EvalKind::Wasm).is_blocked());
        assert!(!check_eval(&pol("script-src 'wasm-unsafe-eval'"), EvalKind::Wasm).is_blocked());
        assert!(check_eval(&pol("script-src 'unsafe-inline'"), EvalKind::Wasm).is_blocked());
        // script-src-elem is irrelevant to eval
        assert!(!check_eval(&pol("script-src-elem 'none'"), EvalKind::Eval).is_blocked());
    }

    // ---- navigation directives ---------------------------------------------

    #[test]
    fn form_action_has_no_default_src_fallback() {
        let u = url("https://evil.test/post");
        assert!(!check_form_action(&pol("default-src 'none'"), &u, &page(), 0).is_blocked());
        assert!(check_form_action(&pol("form-action 'none'"), &u, &page(), 0).is_blocked());
        assert!(check_form_action(&pol("form-action 'self'"), &u, &page(), 0).is_blocked());
        assert!(
            !check_form_action(&pol("form-action https://evil.test"), &u, &page(), 0).is_blocked()
        );
        // redirects ignore the path
        let p = pol("form-action https://evil.test/only");
        assert!(check_form_action(&p, &u, &page(), 0).is_blocked());
        assert!(!check_form_action(&p, &u, &page(), 1).is_blocked());
        // javascript: form actions do not match a host-source
        let j = url("javascript:alert(1)");
        assert!(check_form_action(&pol("form-action 'self'"), &j, &page(), 0).is_blocked());
    }

    #[test]
    fn base_uri_has_no_default_src_fallback() {
        let u = url("https://evil.test/");
        assert!(!check_base_uri(&pol("default-src 'none'"), &u, &page()).is_blocked());
        assert!(check_base_uri(&pol("base-uri 'self'"), &u, &page()).is_blocked());
        assert!(
            !check_base_uri(
                &pol("base-uri 'self'"),
                &url("https://page.example/dir/"),
                &page()
            )
            .is_blocked()
        );
        assert!(check_base_uri(&pol("base-uri https://evil.test/a"), &u, &page()).is_blocked());
    }

    #[test]
    fn frame_ancestors_chain() {
        let me = url("https://page.example/doc");
        let a = url("https://page.example/parent");
        let b = url("https://other.test/top");
        let chk = |p: &str, anc: &[CspUrl]| check_frame_ancestors(&pol(p), &me, anc);
        // top-level document has no ancestors: never violates
        assert!(!chk("frame-ancestors 'none'", &[]).is_blocked());
        assert!(chk("frame-ancestors 'none'", std::slice::from_ref(&a)).is_blocked());
        assert!(!chk("frame-ancestors 'self'", std::slice::from_ref(&a)).is_blocked());
        // EVERY ancestor must match, not just the parent
        assert!(chk("frame-ancestors 'self'", &[a.clone(), b.clone()]).is_blocked());
        assert!(chk("frame-ancestors 'self'", &[b.clone(), a.clone()]).is_blocked());
        assert!(
            !chk(
                "frame-ancestors 'self' https://other.test",
                &[a.clone(), b.clone()]
            )
            .is_blocked()
        );
        assert!(
            chk(
                "frame-ancestors https://other.test",
                &[a.clone(), b.clone()]
            )
            .is_blocked()
        );
        // no fallback to default-src
        assert!(!chk("default-src 'none'", std::slice::from_ref(&b)).is_blocked());
        // paths in the policy are ignored
        assert!(
            !chk(
                "frame-ancestors https://other.test/unrelated/path",
                std::slice::from_ref(&b)
            )
            .is_blocked()
        );
        // scheme-source and wildcard
        assert!(!chk("frame-ancestors https:", std::slice::from_ref(&b)).is_blocked());
        assert!(chk("frame-ancestors https:", &[url("http://other.test/")]).is_blocked());
        assert!(
            !chk(
                "frame-ancestors *.other.test",
                &[url("https://x.other.test/")]
            )
            .is_blocked()
        );
        assert!(chk("frame-ancestors *.other.test", std::slice::from_ref(&b)).is_blocked());
        // a data: ancestor cannot satisfy 'self' or a host-source
        assert!(chk("frame-ancestors 'self'", &[url("data:text/html,x")]).is_blocked());
        // empty value list = 'none'
        assert!(chk("frame-ancestors", std::slice::from_ref(&a)).is_blocked());
    }

    #[test]
    fn frame_ancestors_ignored_for_report_only_and_meta() {
        let me = url("https://page.example/doc");
        let anc = [url("https://other.test/")];
        let ro = Policy::parse(
            b"frame-ancestors 'none'",
            Disposition::Report,
            PolicySource::Header,
        )
        .unwrap();
        assert!(!check_frame_ancestors(&ro, &me, &anc).is_blocked());
        let meta = Policy::from_directives(
            pol("frame-ancestors 'none'").directives().to_vec(),
            Disposition::Enforce,
            PolicySource::Meta,
        );
        assert!(!check_frame_ancestors(&meta, &me, &anc).is_blocked());
    }

    // ---- upgrade-insecure-requests / mixed content --------------------------

    #[test]
    fn upgrade_insecure_requests_rules() {
        let p = pol("upgrade-insecure-requests");
        let up = |s: &str, c| upgrade_insecure_url(&p, &url(s), c).map(|u| u.serialize_stripped());
        use UpgradeContext::*;
        assert_eq!(
            up("http://a.test/x?q=1", Subresource).as_deref(),
            Some("https://a.test/x")
        );
        assert_eq!(
            up("http://a.test:80/x", Subresource).as_deref(),
            Some("https://a.test/x")
        );
        assert_eq!(
            up("http://a.test:8080/x", Subresource).as_deref(),
            Some("https://a.test:8080/x")
        );
        assert_eq!(
            up("ws://a.test/s", Subresource).as_deref(),
            Some("wss://a.test/s")
        );
        assert_eq!(up("https://a.test/x", Subresource), None);
        assert_eq!(up("wss://a.test/x", Subresource), None);
        assert_eq!(up("ftp://a.test/x", Subresource), None);
        assert_eq!(up("data:text/plain,x", Subresource), None);
        assert_eq!(
            up("http://a.test/x", SameOriginNavigation).as_deref(),
            Some("https://a.test/x")
        );
        assert_eq!(up("http://a.test/x", CrossOriginNavigation), None);
        // not present / report-only: no upgrade
        let none = pol("img-src *");
        assert!(upgrade_insecure_url(&none, &url("http://a.test/"), Subresource).is_none());
        let ro = Policy::parse(
            b"upgrade-insecure-requests",
            Disposition::Report,
            PolicySource::Header,
        )
        .unwrap();
        assert!(upgrade_insecure_url(&ro, &url("http://a.test/"), Subresource).is_none());
        // loopback http is upgraded too (UIR does not exempt it)
        assert_eq!(
            up("http://localhost/x", Subresource).as_deref(),
            Some("https://localhost/x")
        );
    }

    #[test]
    fn block_all_mixed_content_rules() {
        let p = pol("block-all-mixed-content");
        assert!(blocks_mixed_content(&p, &url("http://a.test/"), true).is_blocked());
        assert!(blocks_mixed_content(&p, &url("ws://a.test/"), true).is_blocked());
        assert!(!blocks_mixed_content(&p, &url("https://a.test/"), true).is_blocked());
        assert!(!blocks_mixed_content(&p, &url("http://localhost/"), true).is_blocked());
        assert!(
            !blocks_mixed_content(&p, &url("http://a.test/"), false).is_blocked(),
            "insecure document"
        );
        assert!(
            !blocks_mixed_content(&pol("img-src *"), &url("http://a.test/"), true).is_blocked()
        );
    }

    // ---- policy sets: multiple policies, report-only -------------------------

    #[test]
    fn policy_set_all_policies_must_allow() {
        let set =
            PolicySet::from_headers(&[b"img-src https://a.test, img-src https://b.test"], &[])
                .unwrap();
        let o = RequestOptions::default();
        // each policy independently restricts: only the intersection passes
        assert!(
            set.check_request(RequestKind::Image, &url("https://a.test/"), &page(), &o)
                .is_blocked()
        );
        assert!(
            set.check_request(RequestKind::Image, &url("https://b.test/"), &page(), &o)
                .is_blocked()
        );
        let v = set.check_request(RequestKind::Image, &url("https://a.test/"), &page(), &o);
        assert_eq!(v.blocked.map(|(i, _)| i), Some(1));
    }

    #[test]
    fn policy_set_report_only_does_not_block_but_reports() {
        let set = PolicySet::from_headers(&[b"img-src *"], &[b"img-src 'none'"]).unwrap();
        let v = set.check_request(
            RequestKind::Image,
            &url("https://a.test/"),
            &page(),
            &RequestOptions::default(),
        );
        assert!(!v.is_blocked());
        assert_eq!(v.reports.len(), 1);
        assert_eq!(v.reports[0].0, 1);
        // enforce + report both violating: both visible
        let set = PolicySet::from_headers(&[b"img-src 'none'"], &[b"img-src 'none'"]).unwrap();
        let v = set.check_request(
            RequestKind::Image,
            &url("https://a.test/"),
            &page(),
            &RequestOptions::default(),
        );
        assert!(v.is_blocked());
        assert_eq!(v.reports.len(), 1);
    }

    #[test]
    fn policy_set_sandbox_is_intersection_and_skips_report_only() {
        let set = PolicySet::from_headers(
            &[b"sandbox allow-scripts allow-forms, sandbox allow-scripts allow-popups"],
            &[b"sandbox allow-same-origin"],
        )
        .unwrap();
        let f = set.sandbox_flags().unwrap();
        assert!(f.allows_scripts());
        assert!(!f.allows(crate::csp::sandbox::SandboxAllow::Forms));
        assert!(!f.allows(crate::csp::sandbox::SandboxAllow::Popups));
        assert!(
            f.forces_opaque_origin(),
            "report-only allow-same-origin ignored"
        );
    }

    #[test]
    fn policy_set_upgrade_and_misc_wrappers() {
        let set = PolicySet::from_headers(&[b"upgrade-insecure-requests"], &[]).unwrap();
        assert!(
            set.upgrade_insecure_url(&url("http://a.test/"), UpgradeContext::Subresource)
                .is_some()
        );
        let set = PolicySet::from_headers(
            &[b"form-action 'none'; base-uri 'none'; frame-ancestors 'none'; script-src 'none'; block-all-mixed-content"],
            &[],
        )
        .unwrap();
        let u = url("https://a.test/");
        assert!(set.check_form_action(&u, &page(), 0).is_blocked());
        assert!(set.check_base_uri(&u, &page()).is_blocked());
        assert!(
            set.check_frame_ancestors(&u, std::slice::from_ref(&u))
                .is_blocked()
        );
        assert!(set.check_eval(EvalKind::Eval).is_blocked());
        assert!(set.check_inline(InlineType::Script, None, "x").is_blocked());
        assert!(
            set.check_mixed_content(&url("http://x.test/"), true)
                .is_blocked()
        );
    }

    #[test]
    fn empty_set_blocks_nothing() {
        let set = PolicySet::new();
        assert!(
            !set.check_request(
                RequestKind::Script,
                &url("https://a.test/"),
                &page(),
                &RequestOptions::default()
            )
            .is_blocked()
        );
        assert!(set.sandbox_flags().is_none());
    }

    // ---- deny-all fallback actually denies --------------------------------

    #[test]
    fn fail_closed_policy_blocks_all_fetches_inline_eval_navigation() {
        let big = vec![b'x'; crate::csp::policy::MAX_POLICY_BYTES + 1];
        let set = PolicySet::from_headers_fail_closed(&[&big], &[]);
        let u = url("https://page.example/");
        for k in RequestKind::ALL {
            assert!(
                set.check_request(k, &u, &page(), &RequestOptions::default())
                    .is_blocked(),
                "{k:?}"
            );
        }
        assert!(
            set.check_inline(InlineType::Script, Some("n"), "x")
                .is_blocked()
        );
        assert!(set.check_eval(EvalKind::Eval).is_blocked());
        assert!(set.check_form_action(&u, &page(), 0).is_blocked());
        assert!(set.check_base_uri(&u, &page()).is_blocked());
        assert!(
            set.check_frame_ancestors(&u, std::slice::from_ref(&u))
                .is_blocked()
        );
        assert!(set.sandbox_flags().is_some_and(|f| !f.allows_scripts()));
    }

    // ---- ambiguities, recorded rather than guessed (root CLAUDE.md) ---------

    #[test]
    #[ignore = "CSP3 does not say whether ws:/wss: count as network schemes for the bare `*` source; this crate says yes; unverified against the spec text"]
    fn ambiguity_star_and_websocket() {
        assert!(!req("connect-src *", RequestKind::WebSocket, "wss://a.test/").is_blocked());
    }

    #[test]
    #[ignore = "CSP3 prose for 'self' scheme upgrades could not be re-read (w3.org unreachable from the build container); the http->ws/wss/https and https->wss table is reconstructed"]
    fn ambiguity_self_upgrade_table() {
        let http_page = Origin::from_url_str("http://page.example/");
        assert!(
            check_request(
                &pol("connect-src 'self'"),
                RequestKind::WebSocket,
                &url("ws://page.example/"),
                &http_page,
                &RequestOptions::default()
            ) == Decision::Allowed
        );
    }

    #[test]
    #[ignore = "unverified against the spec text: whether policy path `/dir/` matches the URL path `/dir` (this crate: no, by the size check before the trailing-empty-segment is dropped)"]
    fn ambiguity_trailing_slash_path() {
        assert!(
            !req(
                "img-src https://a.test/dir/",
                RequestKind::Image,
                "https://a.test/dir"
            )
            .is_blocked()
        );
    }
}
