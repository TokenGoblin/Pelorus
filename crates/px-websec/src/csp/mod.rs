//! Content Security Policy Level 3 (<https://www.w3.org/TR/CSP3/>).
//!
//! No network, no DOM, no clock: the engine is a set of pure functions over
//! parsed policies, URLs and origins, so it can be tested exhaustively and
//! fuzzed.
//!
//! | module | CSP3 | what |
//! |---|---|---|
//! | [`policy`] | §2.2, §3 | serialized-policy parser/serializer, dispositions, `<meta>`, header lists, [`PolicySet`] |
//! | [`source`] | §2.3, §6.7.2, §6.7.3 | source expressions, host/scheme/path matching, `'self'`, nonce, hash, inline rules |
//! | [`check`]  | §4, §6 | enforcement points ([`ENFORCEMENT_POINTS`]) |
//! | [`sandbox`] | §6.3.2 | `sandbox` directive flags |
//! | [`report`] | §5 | violation record and report bodies |
//! | [`sha2`]   | FIPS 180-4 | SHA-256/384/512 for hash sources |
//! | [`url`]    | — | minimal URL/origin (why not `px-net`'s: see that module) |
//!
//! # Implemented
//!
//! * Parsing: `;` splitting, ASCII-whitespace trimming/splitting, ASCII-only
//!   tokens, lowercased names, first-directive-wins duplicates, comma
//!   separated header lists, multiple headers, enforce vs report-only,
//!   `<meta>` subset (no `report-uri`/`frame-ancestors`/`sandbox`, no comma
//!   splitting, never report-only), size and policy-count bounds.
//! * Sources: keywords, nonce, hash (SHA-256/384/512, base64 and base64url,
//!   padding optional), scheme-source, host-source (wildcard, port, `*` port,
//!   path with percent-decoding, redirect path relaxation), `'self'`,
//!   `'none'`, `'unsafe-inline'`, `'unsafe-eval'`, `'wasm-unsafe-eval'`,
//!   `'strict-dynamic'`, `'unsafe-hashes'`, `'report-sample'`.
//! * Checks: all fetch directives with the §6.8.3 fallback lists, inline
//!   script/style/attributes/`javascript:` navigations, eval and Wasm
//!   compilation, `form-action`, `base-uri`, `frame-ancestors` (ancestor
//!   chain), `sandbox`, `upgrade-insecure-requests`, `block-all-mixed-content`.
//! * Reports: violation record, `report-uri` and Reporting API JSON bodies.
//!
//! # Not implemented, or deviating (honest list)
//!
//! * **The spec text could not be re-read.** w3.org was unreachable from the
//!   build container, so the algorithms were reconstructed from the author's
//!   knowledge of CSP3 and are pinned by hand-derived tests. Three points are
//!   `#[ignore]`d tests in `check.rs` rather than guesses: `*` and `ws:`,
//!   the `'self'` upgrade table, trailing-slash paths. A reviewer with the
//!   spec open should settle them.
//! * `trusted-types` / `require-trusted-types-for`: parsed and kept, never
//!   enforced. `navigate-to` (removed from CSP3) and `plugin-types`
//!   (obsolete): parsed, never enforced. `referrer`, `reflected-xss`: unknown.
//! * `'unsafe-allow-redirects'`, `'inline-speculation-rules'`: parsed, no
//!   effect. `'unsafe-hashes'` for eval strings is not implemented.
//! * `<meta>` extraction and `Content-Security-Policy` header *delivery* to
//!   documents, workers and `blob:`/`data:`/`about:` policy inheritance are
//!   embedder concerns; [`PolicySet`] is the object they would carry.
//! * `blob:`/`filesystem:` URLs use an opaque origin; their embedded origin
//!   is not resolved, so `'self'` never matches them (fails closed).
//! * [`url::CspUrl::parse`] is not a WHATWG parser: only canonical, ASCII,
//!   absolute URLs; query and fragment are discarded, so reports omit
//!   queries (stricter than the spec). IDN hosts in *policies* are not
//!   accepted (the grammar is ASCII); write punycode.
//! * A wildcard host-source never matches an IP-address host (the spec's
//!   later wording); a trailing-dot host `example.com.` does not match
//!   `example.com` (fail closed).
//! * Over-size policies (> [`policy::MAX_POLICY_BYTES`]) and header lists with
//!   more than [`policy::MAX_POLICIES`] policies are errors, never
//!   truncations; [`PolicySet::from_headers_fail_closed`] maps them to a
//!   deny-all policy. Unknown directives past
//!   [`policy::MAX_UNKNOWN_DIRECTIVES`] are dropped (no effect on
//!   enforcement). Directive names that are not `[A-Za-z0-9-]+` are skipped.
//! * `frame-ancestors` ignores policy paths and is not applied for
//!   report-only or `<meta>` policies; `sandbox` is not applied for those
//!   either; `upgrade-insecure-requests` is not applied when report-only.

pub mod check;
pub mod policy;
pub mod report;
pub mod sandbox;
pub mod sha2;
pub mod source;
pub mod url;

pub use check::{
    Blocked, Decision, ENFORCEMENT_POINTS, EnforcementPoint, EvalKind, PointKind, RequestKind,
    RequestOptions, UpgradeContext, Verdict, blocks_mixed_content, check_base_uri, check_eval,
    check_form_action, check_frame_ancestors, check_inline, check_request, fallback_chain,
    should_block_request, upgrade_insecure_url,
};
pub use policy::{
    Directive, Disposition, ParseError, Policy, PolicySet, PolicySource, parse_policy_list,
    serialize_policy_list,
};
pub use report::{BlockedResource, SourcePosition, Violation, ViolationContext};
pub use sandbox::{SandboxAllow, SandboxFlags};
pub use source::{HashAlg, HashValue, InlineType, Keyword, SourceExpr, SourceList};
pub use url::{CspUrl, Origin};
