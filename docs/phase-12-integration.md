# Phase 12 integration: the web security model on the fetch path

`px-websec` supplies verdicts as pure functions. This note records where
`px-net` calls them (`crates/px-net/src/secure.rs`, `SecureFetcher::fetch`),
in what order, what is not wired yet, and what could not be verified.

## Who supplies what

`FetchContext` is data from the **caller**: initiator origin, top-level origin,
document URL and ancestor chain, destination, mode, credentials mode, referrer
policies, the document's CSP (`PolicySet`), COEP, integrity metadata, nonce.
Nothing in it is read from a response. Authority comes from the channel: in
production the broker fills the identity-bearing fields (initiator, top-level
origin) from the connection; `secure.rs` treats the context as already
authenticated and does not try to authenticate it.

`PublicSuffixList` implements `px_websec::origin::SiteResolver`, so sites and
partition keys come from one list. `px-websec` still does not depend on
`px-net` (no rustls/ring in its build, no cycle). `px-websec`'s `trace`
feature, switched on by `px-net/testing`, records which enforcement points ran
on the current thread; it exists only so a test can prove a point was reached
through a fetch.

## Enforcement order

Per hop (the first request, then each redirect target):

```
 0  syntax      http(s) only, no credentials in the URL; method not
                CONNECT/TRACE/TRACK; author headers not forbidden/oversized
 1  HSTS        preload upgrade http -> https                 (px-net)
 2  CSP         upgrade-insecure-requests upgrade             (csp)
 3  mixed       check_request: Allow | Upgrade(url) | Block   (mixed)
 4  CSP         block-all-mixed-content                       (csp)
 5  CSP         check_request on the destination's directive  (csp)
                enforced -> deny; report-only -> Violation{enforced:false}
 6  CORS        taint(): basic | cors | opaque, or deny       (cors)
 7  CORS        classify(); preflight via the same transport, cache keyed by
                (PartitionKey string, origin, URL, credentials)
 8  headers     Origin (cors), Referer (referrer), Cookie (SameSite through
                origin::cross_site_for_cookies)
 9  transport   one request; px-net never follows a redirect itself

 redirect       CORS-check the redirect response (cors tainting), then
                redirect::Chain decides (cap, loop, https->http, Location);
                tainted-origin rule, POST->GET, Authorization dropped on a
                cross-origin hop; Referrer-Policy on the redirect response
                applies to later hops; go to step 0 with the new URL.

 final response:
 r0 mixed       check_response on the final URL
 r1 CORS        validate_response (tainting == cors)
 r2 CORP/COEP   coep_check_response (opaque) / corp_check (subframe)
 r3 SRI         parse (over-bound = deny) then verify the body
 r4 framing     (subframes) X-Frame-Options + CSP frame-ancestors,
                combined; frame-ancestors overrides X-Frame-Options.
                (navigations) document facts: CSP (fail-closed parse),
                Referrer-Policy, COEP, COOP, iframe sandbox + CSP sandbox,
                resulting origin (origin::for_document, sandboxed_origin)
 r5 filter      filter_response: basic | cors (Expose-Headers) | opaque
```

HSTS runs before mixed content on purpose. A preloaded host is upgraded to
https and an upgraded request is not mixed content; the other order would block
a request that is about to become secure. Either order is safe for the wire
(the transport is also given the preload list and refuses plaintext to a
preloaded host); this one is what the mixed-content blocker and HSTS are
generally understood to do together. `hsts_runs_before_mixed_content` pins it.

`FetchOutcome` carries `response` (`None` when denied; opaque responses give
script no status, headers or body, and the embedder bytes only through
`embedder_body()`), `violations` (enforced and report-only, each naming the
enforcement point), `upgraded_from`, every URL requested, the tainting,
preflight counts, the document facts for navigations, and a transport `error`.

Fail closed: unparsable URL, non-UTF-8 header value, unrepresentable
partition key, CSP header that does not parse (`from_headers_fail_closed` gives
deny-all), `integrity` over its bound, X-Frame-Options over its bound,
duplicated or conflicting `Access-Control-Allow-Origin`, any transport or
framing error: all deny.

## Tests

`crates/px-net/tests/secure_fetch.rs` drives real fetches against a local
server that records every request it receives. Each scenario names the
enforcement point it must reach and the runner checks it. One test enumerates
every module's `ENFORCEMENT_POINTS` and requires each to be reached through
`px-net` or listed in `NOT_WIRED` with a reason (checked both ways).

## Not wired

- **Document, worker and navigation integration.** The content process
  (Phase 13) owns: building `FetchContext` from a live document, `<meta>` CSP,
  policy inheritance for `about:`/`blob:`/`data:`, applying `DocumentFacts`,
  COOP browsing-context-group switching (`coop::group_switch`), service
  workers, the `referrerpolicy` attribute (`referrer::parse_attribute`).
- **Points that never involve a request:** CSP inline script/style/attributes,
  `javascript:` navigations, eval and Wasm, `form-action`, `base-uri`. They are
  in `NOT_WIRED` in the test.
- **CSP hash-source for external scripts.** `RequestOptions::integrity` is
  passed empty, so `script-src 'sha256-...'` does not allow an external script
  by its `integrity` hash.
- **COEP report-only**, CORP report-only reporting, CSP/COEP report delivery.
- **WebSocket, `ftp:`, `data:`, `blob:`, `file:` fetches** (px-net fetches
  http(s) only).
- **Cache, service-worker and HTTP-authentication interaction**; the Fetch
  "credentials in redirect Location" error (px-net's `redirect` refuses
  userinfo already).
- **Mixed-content for top-level navigations** is left to the UI (as MC2 does).
- **Connection reuse:** every request still opens a connection
  (`Connection: close`); the pool is not used by this path.

## Honest limits

- The spec text for HTML-defined pieces (COOP/COEP parsing, iframe sandbox,
  origin of `about:`/`srcdoc`, secure contexts) was not read; `px-websec`
  already records this. Fetch/CORS/mixed-content behaviour was written from the
  algorithms as known and from `px-websec`'s own notes.
- WPT suites (`cors/`, `fetch/`, `mixed-content/`, `referrer-policy/`,
  `content-security-policy/`) were not run; the Phase 12 gate is not met by this
  change. These tests prove the wiring, not conformance.
- No TLS server is used in these tests, so an upgraded request is shown to
  speak TLS (the server sees a ClientHello) and fail without falling back to
  http; a successful https round trip is not exercised here.
- Hosts such as `app.test` reach the local server through the `testing`-only
  `connect_to`; the production path resolves normally.
- A duplicated or unparsable `Cross-Origin-Resource-Policy` is treated as no
  policy (what Fetch specifies), not as a refusal.
- The two `Origin` types in `px-websec` (`origin::Origin` and
  `csp::url::Origin`) are still separate; `secure.rs` converts at the CSP
  boundary (`CspUrl::parse` of the serialised URL). A URL the CSP engine cannot
  express is denied. See `docs/backlog.md`.

## Live web in the shell

`px-shell https://host/` is the first consumer of `SecureFetcher` outside its
tests. The door is `px_net::browse` (`WebSession`, `Loader`): `navigate` is the
browser-initiated top-level fetch (`Destination::Navigation`, no referrer) and
`stylesheet` is a `Destination::Style` subresource whose `FetchContext` carries
the navigation's CSP and Referrer-Policy (`DocumentFacts`). Every step of the
enforcement order above runs; redirects follow `redirect::Chain`. The shell only
decodes (BOM, `Content-Type` charset, `<meta>` prescan; UTF-8, UTF-16, Latin-1 /
windows-1252), finds `<link rel=stylesheet>`, vets inline `<style>` with the
document's CSP (`Loader::allows_inline_style`), lays out and paints. Scripts,
images, fonts, frames and forms are never requested. A denial or transport
failure becomes `px_pages::error_page(address, reason)`.

Process model: in-process on the UI thread. Moving the loader into the network
process and the layout into the content process is Phase 19.

Testing behind a TLS-terminating egress proxy: `--features testing` adds
`px_net::testing_proxy` (HTTP CONNECT through `HTTPS_PROXY`, extra anchors from
the PEM file in `PX_TEST_CA_BUNDLE`, added to the normal store; verification is
never disabled). `cargo xtask network` scans release artifacts for it.
