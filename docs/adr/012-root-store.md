# 012 — Platform root store, with a bundled store as the floor

- **Status:** accepted
- **Date:** 2026-09-09
- **Phase:** 3
- **Invariants touched:** none. Invariant 4 constrains what the network is used
  for; this constrains who is trusted to vouch for the other end of it.
- **Settles:** build-spec §13 open decision 4, due this phase.

## Context

`px-net` needs a set of trust anchors before it can verify a single
certificate. Two sources exist, and the choice is usually framed as one or the
other:

- **The platform store.** On Windows, `ROOT` in the system certificate stores,
  which is where enterprise MDM roots and user-added roots land. On Linux,
  whatever the distribution assembles under `/etc/ssl/certs`.
- **A bundled store**, shipped in `data/` and updated through the Phase 20
  channel, as §9 Phase 3 already requires the directory to carry.

The framing is where the trouble is. Chrome and Firefox have both moved toward
bundled stores for consistency, and both kept a platform escape hatch, because
a browser that ignores the platform store is a browser that cannot be used
inside a company that terminates TLS at its own gateway. That is not a fringe
case; it is most corporate networks, and the user's response to "this browser
cannot open our intranet" is to stop using the browser.

What the spec says, and it is right: *recommend platform on Windows so
enterprise and user roots work*.

## Decision

**Both, with the platform store as the source of trust and the bundled store as
a floor.**

At startup `px-net` builds its anchor set from the platform store. If the
platform store cannot be read, or yields implausibly few anchors, the bundled
store in `data/` is used instead — and the fact is surfaced, not swallowed.

The bundled store is **not** merged into the platform set by default. Union
semantics would mean a root the platform administrator deliberately *removed*
is silently restored by us, which turns a deliberate distrust decision into a
no-op. Whoever administers the machine outranks whatever we shipped.

**Implemented in `crates/px-sandbox/src/roots.rs`.** Reading the platform store is unsafe OS work, so it lives in `px-sandbox`
(ADR 008): `CertOpenSystemStoreW` and `CertEnumCertificatesInStore` on Windows
behind a safe wrapper, `px-net` receiving DER blobs it never had to call an OS
API for. On Linux the same wrapper reads `/etc/ssl/certs`, which needs no FFI
and no `unsafe`, and is a file read like the sandbox probes.

## Alternatives rejected

**Bundled only.** Consistent across platforms, reproducible, and exactly what a
browser with this project's threat model would pick if enterprises did not
exist. Rejected because it breaks corporate TLS interception, and the failure
mode is not "the user sees a warning" — it is "every internal site is broken,
and the user switches back to Chrome". A security posture nobody can use
protects nobody. Kept as the floor, which is where its consistency is worth
having.

**Platform only.** Simplest, and it is what most non-browser TLS clients do.
Rejected because a machine with a broken, empty or unreadable store would then
fail every connection with no way to distinguish "this site is untrustworthy"
from "this machine has no anchors" — and because Linux distributions vary
enough that "the platform store" is not one thing.

**Union of both, always.** Rejected above: it silently reverses an
administrator's removal of a root. That is the one behaviour a trust store must
not have.

**`rustls-native-certs`.** Does exactly this job and is maintained by the rustls
authors. Rejected for now because it is not in build-spec §3's list and the
work it saves is one `CertOpenSystemStoreW` loop that `px-sandbox` is already
the right home for — the crate that exists to own precisely this kind of call.
Revisit if the platform-specific edge cases turn out to be deeper than they
look; that would be an ADR amending this one, with the edge cases named.

## Consequences

**A corporate TLS-interception root is trusted, by design.** This project's
threat model does not include defending the user against the administrator of
the machine they are using. Stating it plainly because it is the kind of thing
that reads as a bug later: if the machine trusts a middlebox, so do we.

**Two code paths that must agree**, and the bundled one will be exercised far
less. The fallback is therefore tested explicitly rather than left to a machine
that happens to have a broken store.

**"Implausibly few anchors" needs a number, and any number is arbitrary.** A
store that reads successfully and returns three certificates is more likely
broken than minimal, but a threshold is a guess. The gate asserts the fallback
fires when the store is empty or unreadable; the low-count heuristic is
recorded as a decision to revisit with real data rather than defended as
principled.

**`data/` gains a root store that must be updated**, and a stale one is a real
hazard — a revoked or distrusted root left in the floor is exactly the anchor
an attacker wants. Same shape as the PSL problem this phase's gate already
covers, and it rides the same Phase 20 channel.

## Verification

Wrong if a machine with a working platform store ever verifies against the
bundled floor without saying so; the fallback is observable, and a silent
downgrade of trust anchors is the failure this ADR most wants to avoid.

Wrong if a root removed from the platform store is still accepted — that would
mean the union semantics rejected above crept in through an implementation
detail.

The gate tests both directions: an empty platform store falls back and reports
it, and a root present in the bundle but absent from the platform store is
**not** accepted when the platform store is readable.
