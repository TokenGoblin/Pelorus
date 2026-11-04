# 041 — Signed updates: Ed25519 verification is hand-written in px-update, with a canonical text manifest, threshold-signed key rotation and mandatory freshness

- **Status:** proposed
- **Date:** 2026-11-04
- **Phase:** 20
- **Invariants touched:** 4 (the update check is the one permitted non-page
  traffic and must carry no identifier), 10 (no release before the update
  channel exists), 2 and 5 indirectly (the PSL, HSTS list and root store ride
  this channel)

## Context

Phase 20 needs a way for an installed copy to learn, over a hostile network
and possibly from a hostile mirror, that a newer build and newer data files
exist, and to refuse everything that is not authentically from the release
signer. build-spec §9 names "minisign or Sigstore" as the options for signed
releases; CLAUDE.md forbids a new third-party dependency without an ADR; the
project is 100% Rust (ADR 032) and prefers few dependencies; and the update
URL and signing key are brand-independent and permanent (§14.1), so whatever
is chosen now is chosen for the life of every installed copy.

What was true when this was decided:

* `ring` 0.17.14 is already in `Cargo.lock` (via rustls, ADR 016) and verifies
  Ed25519. px-update does not depend on it today; declaring it would add a
  direct edge, no new lockfile crate.
* The code needed to *verify* (not sign) Ed25519 is small: field arithmetic
  mod 2^255-19, the Edwards group law, point decoding, SHA-512 and a scalar
  reduction. About 600 lines including comments. The project already carries
  hand-written SHA-2 in two crates.
* Every input to verification is public, so constant-time execution is not
  required. That removes the main reason people are told never to write their
  own elliptic-curve code.
* Nothing is shipped before Phase 20 completes, so there is no installed base
  whose format we are constrained by.

## Decision

1. **Verification is hand-written in px-update, std only.** RFC 8032 §5.1.7,
   `crates/px-update/src/ed25519/`. Verification only; the shipped crate holds
   no signing code (a signer exists under `#[cfg(test)]`). It is STRICT: it
   requires `S < L`, canonical encodings of `A` and `R`, rejects small-order
   `A` and `R`, rejects an `A` with a torsion component, and checks the
   cofactorless equation `[S]B = R + [k]A`. This is deliberately narrower than
   RFC 8032 and than ZIP215 (see Alternatives). There is one honest signer, so
   the goal is "accept nothing but that signer's output", not agreement with a
   set of other verifiers.
2. **The manifest is a hand-written canonical text format**, not JSON
   (`crates/px-update/src/manifest.rs`). `key value` lines in fixed order,
   sorted entries, canonical decimal numbers, lowercase hex, 16 KiB cap, and a
   parser that accepts a byte string only if it equals the canonical encoding
   of what it parsed. The signed unit is the exact bytes; the signature is
   detached (`manifest.sig`, `sig <key id> <signature>` lines) and covers
   `"px-update/manifest/v1\n" || bytes`. The signature is verified over the raw
   bytes before the parser runs.
3. **Freshness and rollback protection are part of the format.** A release
   manifest carries `sequence` (monotonic, never lower than the highest seen),
   `issued` and `expires` (window at most 90 days; outside it the manifest is
   refused), a strict `MAJOR.MINOR.PATCH` version (never lower than installed)
   and per-data-file `YYYYMMDD` versions (never lower than installed).
4. **Trust is a threshold set with an explicit rotation protocol.** The client
   holds `{epoch, 1..=8 keys, threshold, revoked keys}`; the compiled-in root
   set is epoch 1. A manifest needs `threshold` distinct valid signatures.
   A *rotation manifest* names the complete next set and is signed by a
   threshold of the CURRENT set: epoch must be exactly current + 1 (no skip, no
   rollback), keys removed are revoked forever, a revoked key can never be
   re-introduced, and every release states the `key-epoch` it was signed under
   and is refused unless it equals the client's. The client catches up through
   `rotation-N.txt`/`.sig`, one epoch at a time, at most four per check.
5. **The request is built by px-update, byte for byte** (`request.rs`), and
   handed to a `Transport` (implemented by px-net later) to send verbatim over
   a fresh connection that shares nothing with browsing state: `GET`, fixed
   path, `Host`, a constant `User-Agent: update/1`, `Accept: */*`,
   `Connection: close`. No query string, cookie or conditional header. An
   `audit()` allowlist runs on every request before the transport sees it.
6. **Stale data forces a check.** A data file older than `max_data_age_secs`
   (default 30 days) makes the check due regardless of the user's cadence
   (default weekly, user-configured; 0 = manual only); a retry floor (default
   1 hour) still applies so a stuck client cannot hammer a mirror.

**This is cryptography written in-house. It REQUIRES external review before
it ships** (the adversarial review session of build-spec §10, by someone other
than the author, with the author's test inputs withheld). The evidence in
"Verification" shows it agrees with a mature implementation on thousands of
cases; it does not and cannot show the absence of a flaw on inputs nobody
generated. If that review does not happen, the fallback is option B below, not
shipping unreviewed.

## Alternatives rejected

**B. Declare `ring` in px-update and call its Ed25519 verifier.** Zero new
lockfile crates; the code is widely reviewed and fuzzed. Costs: a new *direct*
declaration (needing this ADR's approval anyway); ring is BoringSSL-derived C
and assembly with a large `unsafe` footprint, which sits uncomfortably next to
ADR 032 and the `unsafe` baseline even though the project already links it
through rustls; its Ed25519 verification is the RFC-style cofactorless check and, as far as
we know (not verified here), does not reject small-order keys or
non-canonical `y` encodings, so the strictness would have to be layered on top
anyway; and the project's stated preference is few
dependencies. **This remains the recommended fallback** and is a one-function
change (`ed25519::verify` has a stable signature and the differential fixture
already encodes the expected behaviour on honest and corrupted signatures).
The honest comparison: B has strictly lower cryptographic risk, A has lower
dependency and `unsafe` surface and a stricter acceptance rule. The decision
for the user, below, is how to weigh them.

**C. minisign compatibility.** Attractive because the release tool is then an
existing, audited CLI. But minisign's default (and the `ED` prehashed mode used
for files) signs a BLAKE2b-512 prehash of the file, requiring a hand-written
BLAKE2b (about 150 more lines to write and review), plus the minisign
public-key and signature file formats and its trusted-comment signature. It
has no notion of freshness, sequence, threshold or rotation, so all of items
2 to 4 above would still be needed as a layer on top. Not done; can be added
later as a second accepted signature format without changing the manifest.

**D. Sigstore (cosign/Fulcio/Rekor).** Keyless signing, short-lived
certificates, a transparency log. Needs X.509 and ASN.1 parsing, an OIDC
identity at signing time, an online log lookup or an embedded log proof
format, and trust roots of its own that themselves rotate. Large dependency
and protocol surface, and the verification path would itself make network
calls that must carry no identifiers. Wrong shape for a single-maintainer
project whose requirement is "verify one signer, offline, forever". No.

**E. Cofactored (ZIP215-style) or RFC-permissive verification.** These accept
signatures that strict verification rejects (small-order or torsion-laden
`A`/`R`, non-canonical encodings). That matters for consensus systems where
independent verifiers must agree on the accepted set. Here there is one signer
and no such set; accepting more can only widen the attack surface. Rejected.

**F. JSON (or TOML) for the manifest.** A signed document is only as
unambiguous as its least careful reader: duplicate keys, number forms,
escapes, unicode normalisation, trailing data. A one-spelling text format with
a round-trip canonicality check removes the class. The cost is a small
bespoke parser, kept panic-free by the crate's workspace lints and exercised
by a fuzz target.

## Consequences

Unpleasant ones, stated plainly:

* **Hand-written cryptography is a standing liability** until reviewed, and
  after review it is still ours to maintain. A bug here is a remote code
  execution primitive for whoever holds a mirror.
* **Rotation can strand an offline client.** Freshness applies to rotations
  too, so a client offline for longer than a rotation's validity (at most 90
  days) cannot catch up and needs a manual reinstall, unless the signers keep
  re-issuing the rotation chain with fresh windows. This is the price of not
  letting an attacker who steals a retired key mint a "valid" rotation for
  clients that never saw the revocation.
* **Loss of `threshold` keys at once is unrecoverable in-band.** The recovery
  path is an out-of-band new install carrying a new root set.
* **A release cannot be revoked, only superseded** by a higher `sequence`.
  `sequence` is monotonic by design, so a bad signed release is fixed forward.
* **Clock trust.** Freshness depends on the local clock. A client whose clock
  an attacker controls can be made to accept an expired manifest; the
  monotonic `sequence` bounds the damage to "the newest manifest ever seen",
  never an older one.
* **Downloads are in memory** in this unit (up to the 1 GiB artifact cap);
  streaming verification needs an incremental hasher and a sink, to be done
  with the px-net transport.
* The update URL, mirror list and root public keys are not in this unit: they
  are decisions for the user and must be permanent (§14.1).
* Variable-time field arithmetic means this code must never be reused for
  signing or key generation. The module documentation says so.
* The strict rules reject some inputs RFC 8032 allows. No honestly generated
  key or signature trips them; a differently built signer that does would be
  a release-tool bug, caught the first time it signs.

## Verification

How we would know this decision was wrong:

* An external review finds an accepted forgery, a rejected honest signature,
  or a panic reachable from signature, key or manifest bytes. Any one of these
  flips the recommendation to option B.
* The fuzz target `update_manifest` (compile-checked in CI; run in the
  campaign) finds a panic, an accepted non-canonical document, or an input the
  parser accepts but the encoder does not reproduce.
* The differential comparison against `ring` ever disagrees on an honest or
  randomly corrupted signature.

Evidence at the time of writing (all in `cargo test -p px-update`):

* All five RFC 8032 §7.1 vectors (empty, 1 byte, 2 bytes, the 1023-byte
  message, and the SHA-512 "abc" message) verify, and the test-only signer
  reproduces each public key and signature exactly.
* Every single-bit flip of a signature fails; so does every bit flip of the
  key; wrong message, wrong key, `S = L`, `S + L`, small-order `R`, small-order
  and torsion `A`, non-canonical `y` and sign-on-zero encodings, an all-zero
  signature and truncated or oversized inputs are all rejected.
* A one-off differential run against `ring` 0.17.14 (scratch project outside
  the repo, never in `Cargo.lock`): 4000 cases, 2000 honest and 2000 randomly
  corrupted (signature bit flips, message bit flips, key bit flips), agreement
  on all 4000. A 100-case sample is committed as a fixture.

What would NOT be falsified by anything we can run: the absence of a flaw on
inputs outside what the tests construct. That is exactly what the external
review is for.

## What the user must decide

1. **Hand-written verification (this ADR's recommendation) or `ring`
   (option B)?** Recommendation: hand-written, *conditional on* the external
   review passing; `ring` if the review cannot be arranged before the first
   release.
2. **The root key ceremony:** who generates the keys, on what hardware, how
   many (suggest 3), the threshold (suggest 2 of 3), where each is stored
   offline, and the written rotation drill. The compiled-in epoch-1 public keys
   are fixed by this and are permanent.
3. **The update domain and mirror list.** Permanent (§14.1); serve both for at
   least two years if it ever moves. This unit contains no real host.
4. **Release tool and cadence:** who signs, how often (the manifest window is
   at most 90 days, so a signing cadence of well under that is required), and
   whether the release tool is a Rust `cargo xtask` command (the project is
   100% Rust; the test-only signer here is not the release signer).
5. **Policy values:** the stale-data bound (default 30 days), whether a user
   who turns scheduled checks off should still be subject to the stale-data
   forced check (this unit says yes: PSL staleness is a security-boundary bug),
   and the default cadence (weekly per invariant 4).
6. **Windows code signing and `SECURITY.md`** are separate Phase 20 deliverables
   not covered here.
