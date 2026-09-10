# 016 — rustls with the `ring` backend

- **Status:** accepted
- **Date:** 2026-09-09
- **Phase:** 3
- **Invariants touched:** none. Constrained by 7 (reproducible builds).

## Context

`rustls` is named in build-spec §3's "take from the ecosystem" list, so *that* it
is used is settled. What §3 does not name — and what actually carries the risk
— is the cryptographic backend underneath it. rustls 0.23 does not implement
its own primitives; it takes a `CryptoProvider`, and the two supported ones are
very different dependencies.

This ADR also records the dependency numbers §3 requires, because rustls is
where this project's dependency closure stops being small.

## Decision

**`rustls` with `default-features = false` and the `ring` provider**, plus
`rustls-pki-types` for the shared types and `webpki-roots` for ADR 012's
bundled floor.

Measured rather than assumed. Both backends were resolved and counted before
choosing:

| | crates in lock | extra build-time tooling |
|---|---:|---|
| `ring` | **63** | a C compiler, via `cc` |
| `aws-lc-rs` | 70 | a C compiler **plus `cmake` and `pkg-config`** |

`aws-lc-rs` is the rustls default and the better-resourced project — AWS
maintains it, and it has a FIPS story `ring` does not. It loses here on build
requirements: `cmake` and `pkg-config` are two more things that must exist,
match, and behave identically on Windows and Linux for invariant 7's "same
source + same toolchain = same binary hash" to hold. It also pulls
`aws-lc-sys`, a vendored fork of BoringSSL, which is a far larger C surface
than `ring`'s.

FIPS validation is worth nothing to this project; reproducibility is worth a
great deal. That asymmetry is the whole decision.

## Alternatives rejected

**`aws-lc-rs`.** Above. Revisit if `ring`'s maintenance becomes a problem —
see Consequences — or if the build-tooling gap closes.

**A pure-Rust backend** (`rustls-rustcrypto` or similar). Genuinely appealing
for a project whose thesis is memory safety: it would put the entire TLS stack
under the same guarantees as the rest. Rejected because it is not a supported
rustls provider, has no comparable review history, and is materially slower —
and a browser doing TLS in constant-time-questionable pure Rust is trading a
memory-safety win for a side-channel loss that is harder to reason about.

**Writing the TLS stack.** Not seriously considered. §3 names rustls precisely
so this is not on the table, and it is the correct call: TLS is where a subtle
error is both catastrophic and undetectable by testing.

## Consequences

**The unsafe baseline rises from 13,341 to 17,294 tokens.** `ring` contributes
230 and `windows-sys 0.52` — reached through `getrandom` — contributes 3,411.

**And that number does not measure the thing most worth measuring.**
`ci/unsafe-audit.sh` counts `unsafe` tokens in *Rust* source. `ring` is
120,164 lines of assembly and 5,413 lines of C, none of which the audit can
see. The gate will report `ring` as 230 and that is an enormous understatement
of what was accepted. Recorded here, and in `docs/backlog.md`, because a
metric that silently stops measuring the risk it was built for is worse than
no metric.

This is not an argument against `ring` — every option here has a large C or
assembly core, and hand-written crypto assembly is how constant-time
primitives are achieved. It is an argument against reading the unsafe baseline
as though it covers the dependency closure. It covers the Rust part of it.

**`ring` is maintained by one person.** Brian Smith has maintained it for a
decade and its BoringSSL-derived core is widely reviewed, but bus factor one
on the cryptographic core of a browser is a real risk and the mitigation is
"switch to `aws-lc-rs`", which this ADR is written to keep cheap: the provider
is a rustls feature, not an API this project codes against.

**A duplicate `windows-sys`.** `px-sandbox` uses 0.61; `ring` reaches 0.52
through `getrandom`. `cargo deny`'s `multiple-versions = "deny"` now carries
one skip, with the reason. Two copies mean two audit surfaces and two crates
to patch at a CVE, which is the cost the deny rule exists to make visible.
Unifying is not ours to do: the pin is inside a dependency's dependency.

**A new licence class.** `webpki-roots` is Mozilla's root store, a *data* set
under `CDLA-Permissive-2.0` rather than a code licence. Added to the allow
list for that crate and that reason.

**A C compiler is now required to build.** Both platforms already need one for
other reasons, but it moves from incidental to load-bearing, and it is a
reproducibility variable: `cc` invoking a different compiler version produces
different object code from the same source.

## Dependency record (build-spec §3, §5)

- **Crates and versions:** `rustls` 0.23.44, `ring` 0.17.14, `rustls-webpki`
  0.103.15, `rustls-pki-types` 1.15.1, `webpki-roots` 1.0.9, plus the closure
  below.
- **What they do:** TLS 1.2 and 1.3; the cryptographic primitives; certificate
  path validation; shared types; the bundled root store of ADR 012.
- **Why not write them:** TLS is the canonical example of code where a subtle
  error is catastrophic and invisible to testing. §3 names rustls for this
  reason and it is right.
- **Maintainer count:** rustls — a small team under the ISRG umbrella (`ctz`,
  `djc`, `cpu`). `ring` — effectively one (`briansmith`). `webpki-roots` —
  the rustls team, tracking Mozilla's store.
- **Audit status:** `cargo vet --locked` passes with 42 crates audited,
  resolved by publisher trust against the ISRG, Mozilla and Bytecode Alliance
  import sets — the same mechanism ADR 010 used and the workspace already used
  for dtolnay's crates. Publisher trust is broader than version audit and that
  weakening is restated here rather than assumed forgotten.
- **Unsafe-line count:** 3,953 added in Rust tokens; **plus roughly 125,000
  lines of C and assembly the audit does not count.**
- **Transitive dependencies added:** the lock goes from 15 external crates to
  47. The largest additions are `ring`'s closure (`getrandom`, `untrusted`,
  `cfg-if`, `wasi`, `windows-sys 0.52` and its nine `windows_*` target crates)
  and `cc` with `find-msvc-tools` and `shlex` as build dependencies.

## Verification

Wrong if reproducibility breaks — if two builds of the same source on the same
toolchain stop producing the same hash because a C compiler differed. The
`reproducible` gate already tests exactly this on both platforms and now has
something real to test.

Wrong if `ring` goes unmaintained and the migration to `aws-lc-rs` turns out
not to be the feature-flag change this ADR assumes. That assumption is
testable cheaply and should be tested before it is needed rather than after.

It is *not* falsified by the unsafe baseline being large. That was measured
before accepting, and the number understating the C and assembly is the point
the Consequences section makes rather than a surprise waiting to happen.
