# 017 — `html5ever` and the closure underneath it

- **Status:** accepted
- **Date:** 2026-09-09
- **Phase:** 4
- **Invariants touched:** none directly. Bears on 2 (state partitioned by
  top-level site) through a process-global string interner; see Consequences.

## Context

`html5ever` is named twice as settled: build-spec §3's "take from the
ecosystem" list, and Appendix A, whose entry — *"writing your own HTML
tokenizer or TLS stack buys bugs, not sovereignty"* — closes the argument.
§9 Phase 4 says the same thing operationally: *"`html5ever` into a
**generational** arena."*

So this ADR does not re-propose the decision. It exists because §3 requires a
dependency record, and because measuring the closure turned up something the
headline decision does not predict.

## Decision

**`html5ever` 0.39.0 with default features**, implementing its `TreeSink`
trait over `px-dom`'s own generational arena.

The division of labour is the part worth stating: html5ever supplies the
tokenizer and tree-construction *state machine* — the part of the HTML spec
that is thousands of lines of error recovery nobody should retype — and
`px-dom` supplies the tree. Handles, generations, arena layout, mutation
safety and depth limits stay ours. `TreeSink` is a trait we implement, not a
tree we adopt, so §4.1's rules survive contact: every accessor still returns
`Option` and there is still no infallible index API.

No feature reduction is available, which was checked rather than assumed.
`html5ever` has exactly two features, `serde` and `trace_tokenizer`, both off
by default. `tendril`'s `encoding_rs` is off, which matters — it would pull
Gecko's entire character-encoding library. There is no knob that drops
anything below.

## What it costs

Measured before accepting, on the lock file rather than from the manifests.

| | before | after | delta |
|---|---:|---:|---:|
| crates in lock | 63 | 84 | **+21** |
| unsafe tokens (audit) | 17,294 | 18,261 | **+967** |

The twenty-one: `bitflags`, `fastrand`, `html5ever`, `lock_api`,
`markup5ever`, `new_debug_unreachable`, `parking_lot`, `parking_lot_core`,
`phf`, `phf_codegen`, `phf_generator`, `phf_shared`, `precomputed-hash`,
`redox_syscall`, `scopeguard`, `siphasher`, `smallvec`, `string_cache`,
`string_cache_codegen`, `tendril`, `web_atoms`.

**The unsafe is not where the HTML parsing is.** This is the finding, and it
is the reason this ADR is longer than "§3 said so":

| crate | unsafe | what it is |
|---|---:|---|
| `lock_api` | 302 | a mutex |
| `redox_syscall` | 178 | syscalls for an OS this project does not target |
| `parking_lot_core` | 149 | a mutex |
| `tendril` | 129 | refcounted string buffers |
| `parking_lot` | 88 | a mutex |
| `smallvec` | 75 | inline-capacity vectors |
| `string_cache` | 19 | the interner |
| `siphasher` | 12 | hashing |
| **`html5ever`** | **4** | **the HTML parser** |
| **`markup5ever`** | **1** | **the shared tokenizer types** |

**539 of the 967 — more than half — is `parking_lot`**, reached only because
`string_cache` wants a process-global interner and therefore wants a lock.
The HTML parser this ADR is nominally about contributes five tokens between
its two crates.

Two of these do not ship, and the accounting should say so rather than let the
gate number stand unqualified:

- **`redox_syscall` (178) is target-gated to Redox** and is compiled for
  neither Windows nor Linux. `cargo tree -i redox_syscall` prints nothing
  without `--target all`. It is in the baseline because `ci/unsafe-audit.sh`
  counts vendored source across all targets, which is the correct direction
  for a gate — it never under-counts — but 178 tokens of it are for an
  operating system this project will never run on.
- **`phf_codegen`, `phf_generator` and `string_cache_codegen` are build
  dependencies**, not normal ones. They generate perfect-hash tables at build
  time and are absent from the shipped binary. They are *not* absent from the
  threat model: they execute on the build machine, which is the supply-chain
  shape invariant 7 cares about.

So the figure that reaches a user is **789 tokens, of which 539 is a mutex**.

## Alternatives rejected

**Writing the tokenizer and tree builder.** Foreclosed by Appendix A, and
correctly. HTML parsing is not an algorithm, it is a twenty-year accretion of
compatibility behaviour; the html5lib corpus this phase's gate grades against
at 99% exists precisely because nobody gets it right by reading the spec.

**Vendoring `html5ever` and cutting `string_cache` out.** This is the
tempting one, because it removes 539 unsafe tokens, the global interner, and
the lock — and `markup5ever`'s atom types are woven through the tokenizer's
public API, so it is a fork of the tree-builder interface, not a patch. Taking
it would mean maintaining a divergent copy of the one crate in this project
whose value is *being* the widely-tested version. Rejected for now and
recorded in `docs/backlog.md` as a thing to reconsider if the interner turns
out to matter at Phase 14.

**A different HTML parser.** There is no second production-grade
spec-conformant HTML5 parser in Rust. This is not a choice that exists.

## Consequences

**A process-global, lock-protected string interner now sits under the DOM.**
`string_cache`'s dynamic atom set is shared per-process and refcounted behind
sharded locks. Two things follow.

The first is ordinary: it is a lock in the parse hot path, and parsing is not
yet concurrent, so it costs little today and should be re-measured when it is.

The second is not ordinary, and is the reason invariant 2 is listed at the top
of this ADR. Interner occupancy is observable through timing — interning a
string that is already present is measurably cheaper than interning a new one.
That is a cross-document channel *if two documents from different sites share
a process*. **They can today.** One-process-per-site is a Phase 14
deliverable, not a current property; build-spec §9 says so in as many words —
*"without this, 'one process per site' is a claim rather than a property."*
Until Phase 14 lands, this interner is one of the things that makes it a claim.

This is not a reason to reject html5ever — the same channel exists in every
browser that interns strings, and the mitigation is process separation, which
is already planned and scheduled. It is a reason to write it down now, while
it is a known consequence of a decision, rather than discovering it during
Phase 14 as an unexplained cross-origin timing result. Added to
`docs/backlog.md` against Phase 14.

**`cargo vet` needed twenty-one new entries, and the imports covered none of
them.** These versions are recent enough that Mozilla's, Google's, ISRG's and
the Bytecode Alliance's published audit sets do not reach them, so every one
was resolved by publisher trust — the mechanism ADRs 010 and 016 used, with
the same weakening over version audit that those ADRs state and this one does
not re-argue.

Nine people, not an organisation, which is worth naming precisely because
"these are Servo's crates" is the comfortable answer and is not what the trust
entries actually say:

| publisher | crates |
|---|---|
| `jdm` (Josh Bowman-Matthews) | `html5ever`, `markup5ever`, `web_atoms`, `smallvec` |
| `Amanieu` (Amanieu d'Antras) | `parking_lot`, `parking_lot_core`, `lock_api`, `scopeguard` |
| `JohnTitor` (Yuki Okushi) | `phf`, `phf_codegen`, `phf_generator`, `phf_shared` |
| `mrobinson` (Martin Robinson) | `string_cache`, `string_cache_codegen`, `tendril` |
| `KodrAus` (Ashley Mannix) | `bitflags` |
| `taiki-e` (Taiki Endo) | `fastrand` |
| `mbrubeck` (Matt Brubeck) | `new_debug_unreachable` |
| `jedisct1` (Frank Denis) | `siphasher` |

**`redox_syscall` is exempted rather than trusted, and that is deliberate.**
It is the project's first `cargo vet` exemption. Trusting `4lDO2` would assert
that this project vouches for a maintainer whose code it never compiles; an
exemption asserts what is true, which is that the crate is unaudited and
accepted because it does not ship. The exemption is also version-pinned, so a
bump turns the gate red and somebody re-reads the note — the right friction for
a crate whose entire justification is "we do not build it." The reasoning is
in `supply-chain/config.toml` next to the entry, not only here.

**`smallvec` and `bitflags` are arriving through the back door.** Both are
crates this project would plausibly have wanted anyway and both are now in the
tree without an ADR of their own, because they came as transitive dependencies
rather than as choices. Noted so that a later `smallvec` in a direct
dependency list is recognised as a change in *status* — from transitive to
depended-upon — and gets the ADR it would otherwise skip on the grounds that
"it's already in the lock file."

## Dependency record (build-spec §3, §5)

- **Crates and versions:** `html5ever` 0.39.0, `markup5ever` 0.39.0,
  `web_atoms` 0.2.6, `tendril` 0.5.1, `string_cache` 0.9.0, plus the closure
  listed above.
- **What they do:** the WHATWG HTML tokenizer and tree-construction algorithm,
  its atom tables, and the refcounted string buffers it hands out.
- **Why not write them:** Appendix A. Twenty years of compatibility behaviour
  that cannot be derived from the specification text.
- **Maintainer count:** eight publishers of record across the twenty-one,
  tabulated under Consequences. The parsing crates are Servo's, published by
  `jdm` and `mrobinson`, under the Linux Foundation Europe umbrella rather
  than Mozilla directly. The lock — the largest unsafe contributor — is
  `Amanieu` alone, though he is a Rust library team member and `parking_lot`
  is among the most-depended-on crates in the ecosystem. Bus factor one on
  four separate crates here, same as `ring` in ADR 016.
- **Audit status:** `cargo vet --locked` passes: 62 fully audited, 1 exempted.
  Every one of the twenty-one resolved by publisher trust rather than by an
  imported audit — no import set reaches these versions. `redox_syscall` is
  the exemption.
- **Unsafe-line count:** +967 in the baseline; **+789 in code that compiles
  for a target this project ships; 539 of that is `parking_lot`; 5 of it is
  the HTML parser.**
- **Transitive dependencies added:** 21, listed above. Three are build-only.
  One is for Redox.

## Verification

Wrong if `TreeSink` turns out not to be implementable over a generational
arena without either an infallible accessor or interior mutability that
defeats the generation check. This is the real risk and it is a Phase 4
finding, not a speculative one — the gate's "no infallible node accessor"
source check is what will catch it, and it will catch it as a *gate failure
rather than a design discussion*, which is the intent.

Wrong if the interner's cross-document timing channel turns out to be
exploitable before Phase 14 rather than after it. Testable: the Phase 13 gate
already asks for a cross-site leak harness, and interner occupancy is a
reasonable thing to point it at.

It is **not** falsified by the unsafe baseline rising by 967. That was
measured before accepting and is recorded above with the part that ships
separated from the part that does not.
