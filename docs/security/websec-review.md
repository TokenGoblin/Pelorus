# px-websec adversarial review

Scope: CSP (parsing, source matching, fallback, nonce/hash, frame-ancestors,
report-only, reports), COOP/COEP/CORP, X-Frame-Options, iframe sandbox
attribute, CORS, mixed content, referrer policy, SRI. Method: read each
module against the specifications **from memory** (no network access in the
review environment, same limitation as the original author), write a failing
test for each deviation, fix, then run a std-only mutation fuzz over every
parser. Tests: `tests/spec_review.rs` (one test per finding),
`tests/spec_probe.rs` (parsing/matching edge cases that passed),
`tests/fuzz_headers.rs` (deterministic fuzz, fixed seeds).

Because the review was from memory, every item below marked *verify* should
be checked against the spec text by a reviewer who has it open.

## Fixed

| ID | Sev | Finding | Fix |
|---|---|---|---|
| F-SRI-1 | High | A token with a recognised algorithm but an undecodable or wrong-length digest (`sha256-AAAA`, or a typo) was dropped, leaving an empty set that **disabled the integrity check** and let the load through. SRI keeps it as metadata that cannot match. | Kept with an empty digest; load fails. `Metadata::needs_check` added; px-net switched to it. |
| F-SRI-2 | Medium | Unknown-algorithm-only metadata (`md5-...`) on an opaque (no-cors) response proceeded; SRI tests eligibility before the empty-set rule. | `Metadata::had_tokens`; `verify` orders the steps as the spec does. |
| F-CSP-1 | High | A hash-source of the wrong digest length (or undecodable) was `Unrecognized`, so `'unsafe-inline'` stayed in force next to it. CSP3 treats any grammar-valid hash-source as a hash, which disables `'unsafe-inline'`. | Grammar-valid hash-sources parse as `Hash` with an unmatchable digest. |
| F-CORS-1 | Low | `Range` safelisting accepted `BYTES=` and `bytes=0-+5` (Rust integer parsing accepts `+`), skipping a preflight the spec requires. | Case-sensitive `bytes=`, ASCII digits only. |
| F-CORS-2 | Medium (privacy) | No way to compute the `Origin` header value: a no-cors cross-origin POST (form) under `no-referrer`/`strict-origin*`/`same-origin` must send `Origin: null`; the crate only said whether to send one. | New `cors::origin_header_value(request, taint, policy)`. Integration (px-net) should adopt it. |
| F-REF-1 | Medium (privacy) | `<meta name=referrer content=never>` (and `always`, `origin-when-crossorigin`) was ignored, so the page fell back to the default policy and leaked its origin. | New `referrer::parse_meta_content`. Legacy `default` deliberately not mapped (ambiguous, see below). |
| F-COEP-1 | Medium | No check that a nested document under a `require-corp`/`credentialless` parent sends its own isolation-compatible COEP (HTML "check a navigation response's adherence to its embedder policy"). A COEP-less child could be framed. | New `coop::coep_navigation_check` and enforcement point `coop::coep_navigation`. Wired into px-net's iframe navigation path (reached by the `secure_fetch` enforcement-point audit). |
| F-COEP-2 | Low | Structured-Fields parameters: bare values were accepted loosely (`x=1x`), and a later non-string `report-to` did not replace an earlier string. | Exact bare-item grammar (boolean, number, token, byte sequence); override rule. Parse failure now yields `unsafe-none` as specified. |
| F-CSP-2 | Low | `frame-ancestors` was skipped for report-only policies, so violations could never be reported. | Evaluated; `PolicySet` turns the block into a report only. Still skipped for `<meta>`. |
| F-REPORT-1 | Info | `blocked-uri` after a cross-origin redirect always included the port (`https://a.test:443`); an origin serialisation omits the default port. | Fixed. |

## Reviewed, no deviation found

Directive-name and keyword case-insensitivity; first-directive-wins
(including across case); ASCII-whitespace-only splitting (VT, NBSP, NUL are
not separators); comma lists as independent policies; `'self'` including the
http to https/ws/wss upgrade and opaque origins; `*` not matching data:/blob:;
`*.host` excluding the apex, suffix tricks and IP literals; port rules
(`:*`, default port, none); scheme upgrade rules; path matching (exact vs
prefix, case, percent-decoding, dot segments, `%2f`); userinfo/backslash host
tricks; fallback lists incl. `worker-src`/`child-src`/`script-src`; empty
directive means `'none'`; nonce case-sensitivity and empty nonce; hash text
matching; `strict-dynamic` with `'unsafe-inline'`; XFO duplicates, lists,
`allow-from`, quoting, VT, top-level; sandbox attribute whitespace, case,
truncation (only ever loses permissions, no panic on a multibyte cut); CORS
check/preflight/cache/filter logic against Fetch; mixed content (blob:,
filesystem:, localhost lookalikes, nested insecure frame, imageset);
referrer computation (fragment, credentials, downgrade, 4096 limit, local
schemes). The mutation fuzz (about 20k mutated inputs plus noise and
100 kB extremes through every parser and enforcer) found no panic.

## Needs a decision or a spec check (not changed)

1. **Non-ASCII byte in a CSP directive drops the whole directive** (CSP3
   parse step "token is not an ASCII string"). `default-src 'self' caf\xe9.test`
   therefore has *no* `default-src`: fail-open on a header typo, and other
   engines keep the directive. The code follows the spec text as remembered.
   Decision: keep spec behaviour, or fail closed (keep the directive, treat
   the token as unrecognised)? Recommended: fail closed, then confirm no WPT
   expects the skip.
2. **Bare `*` and `ws:`/`wss:`** (existing `#[ignore]`): Fetch's "network
   scheme" is `ftp` and HTTP(S); this crate also counts `ws`/`wss`. *verify.*
3. **`'self'` upgrade table**, **trailing-slash paths** (existing
   `#[ignore]`s): not resolved; no evidence found against the current table.
4. **Nonce/hash under `default-src` for script requests**: nonce and hash
   are honoured for script requests only when the governing directive is
   `script-src`/`script-src-elem`. Other engines also honour them via
   `default-src`. Fails closed (breaks sites), not a bypass. *verify.*
5. **Legacy `<meta name=referrer content=default>`**: mapping not guessed.
6. **COOP `noopener-allow-popups` / `restrict-properties`** parse as
   `unsafe-none`: a site asking for them silently gets no isolation.
   Feature gap, not an exploit.
7. **Report URLs**: spec's "strip URL for use in reports" returns only the
   scheme for non-HTTP(S) URLs; this crate keeps `ws:`/`ftp:` URLs. Query
   strings are dropped (stricter). Low.
8. **Oversized `Referrer-Policy` (> 4096 bytes)** is ignored, which selects
   the default policy rather than the (unknown) intended stricter one.
   Bound is a DoS guard; consider scanning tokens instead. Low.
9. **Fail-closed amplification**: an oversized *report-only* header makes
   `PolicySet::from_headers_fail_closed` enforce deny-all. Safe, but a
   report-only header should arguably never become enforcing.

## Integration follow-ups (outside this crate)

- px-net should call `cors::origin_header_value` when it builds `Origin`
  (`coop::coep_navigation_check` is already wired).
- px-net should use `referrer::parse_meta_content` for `<meta>` delivery.
