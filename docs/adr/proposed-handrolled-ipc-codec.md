# NNN — Replace the postcard crate with a wire-compatible, bounded, hand-written codec in px-ipc

- **Status:** proposed
- **Date:** 2026-11-04
- **Phase:** 1 (the crate), decision needed before Phase 8
- **Invariants touched:** none of the build-spec section 1 invariants. It **does
  amend `/CLAUDE.md`'s hard rule** "IPC is postcard over typed enums with size
  limits" (to "postcard wire format"), and supersedes the second rejected
  alternative in ADR 003.

*Numbering: `proposed-<slug>.md`, no number, because numbers collide across
branches (`main` ends at 028, `origin/phase/06-layout` already has 029 and 030).
Number it at merge. Evidence: `docs/research/dependency-audit.md` section 4.3 and
Appendix B.*

> **Reviewer note, because a draft should say what it thinks.** The audit's
> recommendation is **KEEP postcard** (do not accept this ADR) unless crate count
> outranks the argument below. It is drafted in full because the trade is real,
> time-sensitive and the biggest reduction available outside the settled set, and
> a rejection should leave a record of the numbers. If you reject it, set the
> status to `rejected` and keep the file.

## Context

ADR 003 took `serde` and `postcard` for `px-ipc` and rejected hand-rolled
encoding in two sentences: "every message type would need a hand-written encoder
and decoder kept in sync with the type by discipline alone". It also noted that
postcard is not in Appendix A (it is in section 3's "take from the ecosystem"
list, which is not settled the way `html5ever`, `stylo`, `rustls` and `swash`
are), and declined to argue against the spec. This ADR argues the case with
measurements ADR 003 did not have.

What is true today (`origin/main`, `bd47ba9`):

* `px-ipc` has **7 message variants**: `Request` (`Ping`, `Echo{payload}`,
  `FrameHost{frame}`) and `Response` (`Pong`, `Echo{payload}`, `FrameHost{host}`,
  `Denied`), over `FrameId` (two `u32`) and `FrameHost` (two unit variants).
  `serde` appears in 2 files, `postcard` in 2 files (3 call sites:
  `to_allocvec`, `take_from_bytes`, one test).
* Framing, the 1 MiB `MAX_MESSAGE_BYTES` bound, per-direction tags, trailing-byte
  rejection and channel poisoning are all in `px-ipc`'s own code, not postcard's.
  postcard supplies only the byte format for the body.
* Removing `postcard` from `px-ipc` removes **4 crates from every target**:
  `postcard`, `cobs`, `thiserror`, `thiserror-impl`; plus `embedded-io` 0.4.0 and
  0.6.1, which are lock-only. 13 unsafe tokens (`postcard` 12, `thiserror-impl` 1).
  It removes none of `serde`, `serde_core`, `serde_derive`, `syn`, `proc-macro2`,
  `quote`, `unicode-ident`: `stylo` (`app_units`, `cssparser`, `euclid`,
  `servo_arc`) and `url` require them. Out of 153 Linux crates that is 2.6%.
  Both `postcard` 1.1.3 and `cobs` 0.3.0 are covered by Bytecode Alliance audits
  already in `supply-chain/imports.lock`; `thiserror` by publisher trust.
* A wire-compatible prototype of the **entire current vocabulary** is **102
  non-blank, non-comment lines**, safe Rust, no macros: a `Reader` with
  `byte()`, `varint_u32()` (LEB128, at most 5 bytes, rejects bits beyond 32) and
  `bytes()` (length checked against `rest.len()` before any allocation), a
  `put_varint`/`put_bytes` pair, and a `Wire { encode, decode }` trait implemented
  for the four types (Appendix B of the audit).
* Differential test against `postcard::take_from_bytes` over **20,000,000** random
  inputs of 0 to 11 bytes (biased toward small tags and varint continuation
  bytes), as `Request` and as `Response`: accept/reject agreed on all of them,
  decoded values and consumed lengths agreed on all of the accepted ones (1,010,242
  accepted, 18,989,758 rejected as `Request`): **0 disagreements**. Encoding is
  byte-identical on 9 samples including `u32::MAX` indices and a 70,000-byte
  payload.

## Decision

`px-ipc` stops depending on `postcard` (and with it `cobs`, `thiserror`,
`thiserror-impl`). It implements the postcard wire format for the types it
sends, by hand, behind a small `Wire` trait:

* The format stays exactly postcard's: varint enum discriminants, varint lengths,
  declaration-order fields. A peer built from either side interoperates, and
  golden byte vectors for each variant are committed as the specification.
  This is why the amendment to `/CLAUDE.md` is "postcard wire format" and not
  "a new format".
* The decoder is total and bounded: no length drives an allocation before it is
  compared with the bytes remaining; no recursion (all current types are flat; a
  recursive type needs its own ADR); unknown discriminants and non-canonical
  tails are `Malformed`.
* `px-ipc` keeps `serde` only if a type outside `px-ipc` still derives it. On
  `main` nothing does, so the `serde` direct dependency is also removed (it stays
  in the lockfile through `stylo`). `#[derive(Serialize, Deserialize)]` on the
  four types is deleted.
* Every new message type must ship with a `Wire` impl, a golden vector, a
  round-trip property test, and a fuzz target (build-spec 4.5 already requires
  the last).
* If the vocabulary reaches the point where hand-written impls are the
  maintenance burden, the planned response is a `macro_rules!` for plain
  structs/enums in `px-ipc`, **not** reintroducing `serde_derive` at the
  boundary, and that is a signal to revisit this ADR.

## Alternatives rejected

**Keep postcard (ADR 003).** The recommendation of the audit. Costs 4 crates,
13 unsafe tokens, and a `serde` visitor path between hostile bytes and our types.
Benefits: BA-audited, widely fuzzed, schema kept in sync by the compiler via
derive, and no migration. It is the safer choice for a vocabulary that will grow.
Listed first because "rejected" is the status this draft is most likely to get.

**Hand-roll against postcard's flavor API.** ADR 003's rejected alternative:
still carries `postcard`, `cobs` and `serde` for nothing.

**A different codec with no serde** (a custom non-postcard format). Gains
nothing over wire-compatible, loses the ability to differential-test against
the real thing, and is the bigger spec departure.

**`bincode` / `rkyv` / `zerocopy`.** More dependencies and, for `rkyv` and
`zerocopy`, `unsafe` in the decode path of attacker-controlled bytes. Rejected.

## Consequences

* Gains: 4 crates (6 lockfile entries), 13 unsafe tokens, one proc-macro-derived
  trust boundary, `cobs` framing we never used, and a decoder that fits on one
  screen and can be Miri'd and fuzzed as exactly the code that runs.
* **Loses audited code**: the replacement is ours alone, reviewed by the
  adversarial session `px-ipc/CLAUDE.md` already requires.
* **The cost scales with the vocabulary.** Today it is 102 lines. Phase 8
  (display lists across the GPU process), Phase 13 (storage), Phase 21 (MCP) will
  add types with `String`, `Vec<T>`, `Option`, floats and maps. Each is another
  `encode`/`decode` pair to keep in sync by test rather than by derive. This is
  the argument ADR 003 made and it is still the right objection. If the project
  expects dozens more types, the case for the dependency strengthens, and the
  window in which this change is cheap closes around Phase 8.
* `serde` and `syn` 3.x remain; the `syn` duplicate in `deny.toml` is not
  fixed by this.
* Violates the letter of `/CLAUDE.md` unless amended; an amendment is part of
  the decision.
* Must not weaken: the 1 MiB limit, the tag byte, trailing-byte rejection and
  poisoning stay in `Channel`, unchanged. The existing fuzz targets
  (`frame_request`, `frame_response`, `channel_stream`, `broker_sequence`) call
  `decode_frame`, so they exercise the new decoder with no change, and the
  corpus is valid input to both codecs.

## Verification

Wrong if, for any type in the vocabulary, a byte string is accepted by the new
decoder and rejected by `postcard`, or the reverse, in the differential test that
this ADR requires to remain in CI as a dev-only check for the first release after
the change (a dev-dependency on `postcard` for that one test is acceptable and
removed afterwards). Wrong if a 24-hour fuzz campaign finds a panic or an
allocation above the frame bound in the new decoder. Wrong, in the sense of
"should have stayed", if within two phases the `Wire` impls are more than 15% of
`px-ipc`'s source or a schema-drift bug (encoder and decoder disagreeing) ships.
Not falsifiable by the crate count: the saving is 4 crates and that was known.

---

## For a dependency ADR, additionally (build-spec section 3, section 5)

This is a removal, not an addition. The record that would have been required for
an addition, for what is being given up:

- **Crate and version:** `postcard` 1.1.3, `cobs` 0.3.0, `thiserror` 2.0.20,
  `thiserror-impl` 2.0.20 (and `serde` as a *direct* dependency of `px-ipc`).
- **What it does:** postcard is a compact serde data format; `cobs` is
  byte-stuffing framing; `thiserror` derives `Error`.
- **Why not write it:** the case ADR 003 made: "a binary codec is writable in a
  week; keeping it correct against an adversary is not". Measured now: 102 lines
  and 20 million agreeing inputs for a 7-variant vocabulary. The case gets weaker
  the larger the vocabulary becomes.
- **Maintainer count:** postcard primarily one maintainer (ADR 003). After this
  change: ours.
- **Audit status:** postcard and cobs covered by Bytecode Alliance audits
  (`supply-chain/imports.lock` lines 409, 415, 489, 498). The replacement has none.
- **Unsafe-line count:** 13 removed from `ci/unsafe-baseline.json` (`postcard`
  12, `thiserror-impl` 1; `cobs`, `thiserror` contribute 0 by the baseline);
  `embedded-io-0.4.0` 2 more disappears when the lockfile is regenerated. The new
  code contains 0 `unsafe`.
- **Transitive dependencies added:** 0. Removed: 4 built crates, 2 lock-only.
