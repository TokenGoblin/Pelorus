# 046 — Phase 22 extension story: nothing in v1, three declarative substitutes, and a trigger

- **Status:** proposed. No code, no dependency, no manifest change.
- **Date:** 2026-11-04
- **Phase:** 22 (due before Phase 22 starts; nothing earlier depends on it)
- **Invariants touched:** 1, 2, 3, 4, 5, 6, 9 are each *bounded by* an answer here;
  none is changed by the recommendation. Appendix A "No WebExtensions" is not
  touched; this ADR answers the narrower question build-spec section 9 leaves open
  ("a narrow Rust plugin API for blocking and page transforms, or nothing").
- **Evidence:** the specs and code read for this ADR (cited inline), one measurement
  (section "A WASM runtime", same method as ADR 044), and claims from memory, which
  are labelled.

## Context

Phase 22 says: "ADR first: a narrow Rust plugin API for blocking and page
transforms, or nothing. WebExtensions is a second browser's worth of surface."
Appendix A settles no WebExtensions; ADR 001 lists "no extensions" among the
reasons the attack surface is smaller than the alternatives'.

Two corrections to the premise, both checkable:

- **`px-block` exists, and is narrower than "content blocking" suggests.** At the
  base of this branch (`33a92651`) it is about 4,600 lines of pure policy with no
  I/O: an Adblock Plus / uBlock Origin **network-filter** parser, a deterministic
  token-indexed matcher, query-parameter stripping, referrer tightening, CNAME
  cloaking and partition isolation, with conformance, hostile-input and fuzz tests
  (`crates/px-block/src/lib.rs`). Its own header lists what it does **not** do:
  **cosmetic filtering (element hiding), scriptlets, `$redirect`/`$rewrite`,
  `$removeparam`, `$csp`, top-level `$document` blocking.** Cosmetic filtering is
  the half of a blocker that is a "page transform", and it is exactly the part a
  plugin API would be asked to supply.
- **There is no store, no update path and no install flow**, and invariant 4 says the
  network is for pages and for the signed update manifest, nothing else. An extension
  ecosystem normally means a store, update checks and remote rule lists. Each is a
  second network endpoint the invariant forbids. Any design below is therefore
  *local-file install, manual update*, or it first amends invariant 4.

**What people use extensions for** (from memory; no usage data was consulted, and
the ranking is mine): content blocking; password and credential managers; page
restyling (user stylesheets, dark mode); userscripts and automation; tab, session
and bookmark tools; translation; developer tools; download helpers; privacy
cleaners (cookie auto-delete). Against this project:

| Need | Served without a plugin API? | By what |
|---|---|---|
| Content blocking | Network blocking yes; cosmetic hiding only if added as data | `px-block` (built) with the user's own local rule file |
| Cookie and storage cleanup | Yes | clear-on-exit and partitioning (invariant 2, Phase 19) |
| Dark mode, restyling | Mostly | a user stylesheet per site (below); forced colour scheme |
| Automation and "userscripts" | Mostly | MCP (Phase 21): a local agent drives the browser under explicit, expiring grants; it is the project's extensibility mechanism for automation |
| Password managers | **No** | a non-goal ("a password manager" is listed in section 1); the third-party need is real and no narrow API serves it, because autofill is privileged DOM access plus credential UI |
| Tab and session tools, bookmarks | Built-in scope, not extension scope | Phases 13 and 18 |
| Translation | No, by design | a network service the browser must not call (invariant 4) |
| Developer tools | Out of scope | no devtools phase exists |
| Video downloaders | No, by design | no download capability for untrusted tools (the MCP rule, section 7.3) |

So the honest picture is: **the thing a narrow API would be for (blocking) is
already a built-in; the things it cannot safely be for (passwords, arbitrary page
scripting) are the things people would want it for.** The narrow design that is safe
is mostly redundant, and the design that is not redundant is not narrow.

## Analysis of the shapes a plugin API could take

### A. In-process plugins (a Rust crate or dylib loaded into a browser process)

Rejected on the spot. The project's one structural claim is "memory-safe
application code over a small audited unsafe core, with process isolation for the
rest" (section 4). A plugin in the broker has the broker's authority (every OS
handle, section 3); a plugin in a content process shares memory with the page and
every document of its key. A dynamic library is native code the project did not
write and cannot fuzz, loaded where the sandbox is supposed to be the control.
`forbid(unsafe_code)` does not apply to it. No capability model rescues a
component that shares an address space with the thing granting capabilities.

### B. Out-of-process native plugins, typed IPC

A `px-plugin` host process, spawned like `px-mcp` and a content process: sandboxed
(no filesystem, no network, no clipboard), one process per plugin, postcard over
typed enums with size limits, **authority from the channel** (invariant 9), a grant
table in the broker keyed by the plugin's channel, restart drops every grant
(section 7.3's rule for MCP, for the same reason). This is consistent with
every invariant. What it costs:

- **The plugin is a native executable the user must obtain and trust**, written in
  Rust by a third party against an API we then have to keep stable. A sandbox
  escape in it is a full user compromise by a binary we did not build. Distribution
  without a store is a file the user downloads and runs, which is what the browser
  exists to stop people doing.
- **The blocking hook is a round trip per subresource request.** A page makes tens
  to hundreds. Batching and a decision cache are possible; the latency is real and
  unmeasured. A timeout forces a choice between fail-open (a dead blocker blocks
  nothing, silently) and fail-closed (a dead blocker breaks every site), and the
  user needs to see which.
- **"Page transforms" cannot be out of process.** A transform needs the DOM, which
  lives in the content process. Doing it over IPC means a remote DOM API (chatty,
  large, a second binding generator) or putting the plugin's code in the content
  process, which is shape A or shape D.

### C. Declarative extensions (no code)

Data files the browser interprets itself, validated on load, with no way to run
anything: blocking rule lists (the Phase 19 format), per-site user stylesheets,
per-site settings. Attack surface is the parser (fuzzable, which every parser here
already is) and the semantics. **A declarative list cannot encode browsing history
into its decisions; a code plugin with state can.** That is the sharpest difference
for a privacy browser (below).

### D. A WASM runtime (an interpreter running third-party modules)

The sandboxing story is good (memory-safe by construction, no ambient imports unless
we provide them) and portable (one artifact). Measured, same method as ADR 044
(scratch project, this repository's `Cargo.lock` copied in, the gate's own unsafe
counter): **`wasmi` 2.0.0, `std` only: 8 crates in the closure, 6 new
(`spin wasmi wasmi_collections wasmi_core wasmi_ir wasmparser`; `bitflags` and
`libm` are already locked), +482 unsafe tokens; with default features (the `wat`
text parser) 15 crates, 12 new, +720.** Small, interpreter-only (invariant 6 holds). But: ADR 040 recommends
**no WebAssembly in v1** for the page; a plugin runtime would be the project's
first WASM, with its host-function ABI, versioning, a second `wasmparser` fuzz
surface, and a stability promise. A JIT runtime (`wasmtime`) is out under
invariant 6. The cost is not the six crates, it is owning an ABI.

## What a narrow API would expose, and its attack surface

Whatever the shape, the narrowest honest API is **one hook**:
`on_request(top_level_site, request_url, resource_type) -> Allow | Block`, delivered
in batches, to a plugin the user has enabled for a stated set of sites. No page
transforms, no DOM, no `fetch`, no storage, no UI.

Its surface, each item real:

1. **It is a cross-site observer by construction.** To decide, it must see
   `(top-level site, request URL)` pairs: the user's browsing history as a stream.
   Invariant 2 is "state is partitioned by top-level site, structurally"; this hook
   is the one deliberate exception to "no component correlates across sites".
   Containing it means the plugin process has no network, no file, no clipboard and
   no channel but the broker.
2. **A sandboxed observer can still exfiltrate through its decisions.** Which
   requests it blocks is observable to the page and to the network: a malicious
   blocker can encode what it saw in the pattern of blocks to a domain the
   attacker controls. Narrow, slow, real. Declarative rules (shape C) have no such
   channel because the decision function is fixed in advance. **For a privacy
   product that is the strongest argument for rules over code.**
3. **Supply chain.** A plugin updated out of band is a code-execution update to a
   process that sees all browsing. Local-file install and no auto-update avoids a
   store and also avoids security fixes reaching users.
4. **Chrome pressure.** Extensions want toolbar buttons, badges and prompts.
   Invariant 3 freezes the chrome with one-pixel snapshot tests; invariant 5 forbids
   badges and nags. Any plugin UI is a standing exception to both.
5. **API permanence.** Every hook is a compatibility promise for a solo maintainer;
   the section 11 risk "solo maintenance of a security product" names extensions in
   its mitigation ("deliberately small surface").
6. **Fail mode.** Fail-open on a crash silently disables protection; fail-closed
   breaks browsing. Either is a UI decision the silent chrome has no place to show.

## The capability model, if one is ever built

No ambient authority (invariant 1). A plugin is a process with one channel; the
broker holds a per-plugin grant table `(hook, site-scope, expiry)`; **authority
from the channel** (invariant 9), so the plugin never states its name or the site it
is acting for; grants die with the process and a restart begins with zero
(the MCP rule); the audit log is written by the broker, append-only from the
plugin's side; plugin traffic is `postcard` with size limits, a fuzz target per
message; the sandbox profile is the content profile (no `open`, no `connect`, no
`exec`), plus the same probe test the other processes have (`testing`-only, scanned
for in release). Scope is **per site and chosen by the user**, never "all sites" by
default; a plugin cannot be granted a site the user has not visited-and-enabled.
Declared in a manifest whose every capability the user sees in plain language
(section 7.3's wording rule).

## Decision

**Nothing in v1: no plugin API of any shape, no WebExtensions, and no
extension-shaped surface in the chrome.** Phase 22 closes as "decided: not in v1"
with this ADR as its deliverable, and the spec's alternative ("a narrow Rust plugin
API") is recorded as considered and deferred, not rejected.

In place of an extension story, three **declarative, validated, local-file**
facilities, each small and each already within an existing phase's remit. They are
proposals here, not scope this ADR adds:

1. **Bring-your-own rule list** in `px-block` (Phase 19): a local file in the same
   declarative format as the built-in list, imported by the user; no remote fetch
   (the spec already requires consent for one). The parser already accepts uBlock
   and Adblock Plus network syntax, so this is a loader, not a format. **Cosmetic
   element hiding is the gap**: plain `##selector` lines are recognised and
   skipped today. Supporting only the declarative subset (a selector list becoming
   `display: none !important` in the user origin, validated by the CSS parser, no
   procedural selectors, no scriptlets) is the one extension-shaped capability
   that is both the most-wanted and expressible as data.
2. **A per-site user stylesheet** (Phase 18/19 preferences): author-level CSS the
   user writes, with `url()`, `@import`, `@font-face` sources and `image-set()`
   rejected at load, because CSS can exfiltrate through attribute selectors plus
   `url()` requests. Stylo has a user origin in its cascade (from memory; not checked against the
integration in `px-css`).
3. **MCP is the automation story** (Phase 21): documented as such, so "I want a
   userscript" has an answer that already carries grants and an audit log.

### Trigger for revisiting

Reopen this ADR, not before, when **any** of:

- the Phase 23 thirty-day daily-driver run produces a **friction log** (kept for that
  purpose, one line per task) with at least three distinct tasks that the three
  facilities above cannot do and that are not password management;
- the WASM question in ADR 040 flips to "yes" for pages, so the runtime cost is
  already paid and a plugin host adds a host ABI rather than a runtime;
- there is a second maintainer or a stated contributor who will own the API's
  stability;
- the Phase 21 grant model has run in daily use long enough to say whether
  users read and understand grant prompts, since a plugin permission prompt is the
  same act.

### If it is reopened: the narrowest viable design

One process per plugin; WASM (`wasmi`, interpreter only) rather than native code,
so there is one portable artifact and no third-party native binary; **one hook**
(`on_request`, batched, with a documented fail mode the chrome shows once, in
words); no DOM, no page transforms, no network, no storage; local-file install, no
store, no auto-update (so invariant 4 stands); per-site, per-plugin, user-chosen
grants; plugin UI limited to a preferences page the browser renders. Estimated at
five to six units (host process and sandbox profile, ABI and manifest, grant table
and audit, hook integration at the `px-net` boundary, fuzzing, adversarial review)
and one amendment to ADR 001's "no extensions" sentence. The estimate is mine.

## Alternatives rejected

- **A narrow Rust plugin API in v1.** Shape B or D: possible, consistent with the
  invariants, and not justified: it duplicates `px-block`, cannot serve the needs
  it would be wanted for, adds a cross-site observer, a stability promise and a
  chrome exception, to a solo-maintained product whose threat model sells a small
  surface.
- **WebExtensions, any level of compatibility.** Settled in Appendix A. The numbers
  here only add that the largest consumers of that ecosystem (blockers, password
  managers) are respectively a built-in and a non-goal.
- **In-process plugins.** Section "A".
- **Userscripts in an isolated world in the content process.** Needs Phase 11's
  bindings plus an isolated-world model in Boa; the script reads every matched page
  with that page's authority. It is the most useful code surface and the one this
  project's DOM work is least ready for. Revisit with the trigger, behind per-site
  grants.

## Consequences

- Phase 22 costs one ADR. The facilities in the Decision are small additions to
  Phases 18, 19 and 21, listed so they are not forgotten and not so large as to
  need their own phase.
- **Users who depend on a password manager extension cannot use the browser.** This
  is a real cost, not a footnote: it is likely the single largest reason a daily
  driver fails Phase 23's gate for some people. The project's answer is the OS
  keyring and the browser's own save/fill, which does not exist and is a non-goal;
  say so in the documentation.
- Users who depend on a specific blocker's lists get Phase 19's rules or their own
  imported file, not that product.
- ADR 001's "no extensions" and the section 1 non-goal "WebExtensions
  compatibility" stay true as written.

## Verification

Wrong if: the friction log shows the three facilities do not cover the maintainer's
own week; `px-block` (Phase 19) cannot express the rules the maintainer relies on,
in which case the missing power belongs in the rule format, not in code; or a
security-relevant need (a Phase 21 grant UX failure, say) turns out to require
user-installable code. Not falsifiable here: how many people would use an extension
API, and whether not offering one costs adoption that a solo project can afford to
lose.

## What the user must decide

1. **Accept "nothing in v1"** (recommended), or ask for the narrowest design now
   (one-hook out-of-process WASM blocker, five to six units, and a read of invariant
   4's no-store consequence).
2. **Whether the three declarative facilities are in scope** for Phases 18, 19 and
   21, or are backlog.
3. **Whether to adopt the friction log and the four triggers** as the documented
   condition for reopening, so the answer to "why no extensions" is a measurement
   rather than an opinion.
