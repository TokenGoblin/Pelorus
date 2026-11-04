# Dependency audit

*Research and proposals only. No `Cargo.toml`, `Cargo.lock` or source file was
changed. Date: 2026-11-04. Measured against `origin/main` at `bd47ba9`, Rust
1.98.1. Nothing here is an accepted decision; the three drafts in `docs/adr/`
are `proposed`.*

The brief: zero dependencies where possible, as few as possible otherwise.
`/CLAUDE.md` says "no new crate dependency without an ADR", and build-spec
Appendix A settles that `html5ever`, `stylo`, `rustls` and `swash` are reused.
This audit does not argue with those four. Everything else was fair game, and
**the headline is that very little else is movable.**

## 0. Headline

| Measure | Number | Command |
|---|---|---|
| Package entries in `Cargo.lock` | 192 = 21 first-party + 171 third-party | `grep -c '^name = ' Cargo.lock` |
| Third-party crates built on at least one target | 169 | `cargo tree --workspace --target all --edges normal,build,dev` |
| ... built for x86_64 Linux | 153 | same, `--target x86_64-unknown-linux-gnu` |
| ... built for x86_64 Windows | 154 (+`winapi-util` when the Windows machine is also the build host, see 6) | same, `--target x86_64-pc-windows-msvc` |
| ... built on **neither** shipped target | **13** | set difference, section 6 |
| ... in the lockfile but built nowhere | 2 (`embedded-io` 0.4.0 and 0.6.1) | `cargo tree` vs lock |
| Direct third-party declarations | 19 lines, 17 distinct crates | section 1 |
| Distinct direct deps with **zero** marginal crates workspace-wide | 12 of 17 | section 1 |
| `ci/unsafe-baseline.json` total | 21,675 tokens over 109 entries | |
| ... of which `windows-sys` 0.61.2 + 0.52.0 | 15,941 (73.5%) | |
| ... of which not compiled on either shipped target | 3,655 (16.9%) | section 6 |

**Crates that could vanish from the Linux tree without touching a settled
decision: 5 of 153 (3.3%)** (`postcard`, `cobs`, `thiserror`, `thiserror-impl`,
`webpki-roots`). Every other dependency outside the settled set has a marginal
cost of exactly zero crates and zero unsafe tokens, because `stylo` already
needs it.

Why: `stylo`'s `servo` feature implies `url`, `serde`, `encoding_rs`, `mime`;
`rayon`, `rayon-core`, `num_cpus`, `itertools`, `uluru` are *unconditional*
(`~/.cargo/registry/src/*/stylo-0.21.0/Cargo.toml`, no `optional = true`). So
`app_units`, `euclid`, `url`, `web_atoms`, `selectors`, `stylo_dom`,
`stylo_static_prefs`, `serde`, `toml` (a build-dependency of `stylo_static_prefs`
at the same version), `libc` (also needed by `ring`, `getrandom`,
`parking_lot_core`, `num_cpus`) and `windows-sys` 0.61 (also needed by
`winapi-util`, a build-dependency of `stylo` through `walkdir`) are all
load-bearing for something settled. Removing them from our `Cargo.toml` files
changes the lockfile by nothing.

What *is* worth doing is smaller than "remove dependencies":

1. Delete direct declarations that name things `stylo`/`rustls`/`html5ever`
   already re-export (3 distinct crates, plus 1 redundant dev line). Zero crates
   leave the tree, but each one is a place where a version drift becomes a type
   error, and `px-net` declares one that **nothing uses**.
2. Make the gates count what ships: 13 crates and 3,655 unsafe tokens are never
   compiled for either CI target.
3. Decide, while the IPC vocabulary is still seven variants, whether `postcard`
   stays. It is 4 real crates; a wire-compatible hand-rolled codec for today's
   vocabulary is 102 lines (measured, section 4.3). My verdict is KEEP, with the
   reasoning recorded so that the decision is made rather than drifted into.

## 1. Direct third-party dependencies, per crate

Counts of call sites are lines outside comments that contain the crate path,
from `crates/**/*.rs` (script output in section 8). "Marginal" is the number of
crates that leave the tree if this one edge is removed: first column for that
crate alone (`cargo metadata` graph, Linux target), second for the whole
workspace, i.e. nothing else reaches them. "Own unsafe" is the crate's own entry
in `ci/unsafe-baseline.json`; it counts the token `unsafe` in text, including
comments, and over-counts by design.

Workspace crates with **no** third-party dependency at all (14 of 21): `px-a11y`,
`px-bindings`, `px-block`, `px-brand`, `px-gpu`, `px-layout`, `px-mcp`,
`px-paint`, `px-script`, `px-store`, `px-text`, `px-ui`, `px-update`,
`px-websec`. `px-broker` and `px-content` depend only on first-party crates.

### px-css (10 declarations; 136 third-party crates in its closure)

| Dep | Ver | Kind | Used for in this repo | Sites src/tests | Marginal crate / ws | Own unsafe | Verdict |
|---|---|---|---|---|---|---|---|
| `stylo` (`style::`) | 0.21.0 | normal | the whole style engine | 136 / 27 in 8 files | 39 / 39 (41 incl. other targets) | 481 | settled |
| `selectors` | 0.40.0 | normal | name `selectors::Element` supertrait of `TElement`; stylo does not re-export it | 37 / 0 | 0 / 0 | 10 | KEEP |
| `html5ever` | 0.39.0 | normal | `QualName`, `local_name!`, `ns!` | 29 / 3 | 0 / 3 (shared with px-dom) | 4 | settled; declaration reducible, section 3 |
| `web_atoms` | 0.2.6 | normal | spell `LocalName`, `Namespace`, `*StaticSet` in `TElement` signatures | 8 / 0 in 2 files | 0 / 0 | 0 | **REPLACE with re-exports** |
| `stylo_dom` | 0.21.0 | normal | `ElementState` | 2 / 0 | 0 / 0 | 0 | KEEP |
| `euclid` | 0.22.14 | normal | `Size2D`, `Scale` in `Device::new` and `query_container_size` | 6 / 0 | 0 / 0 | 44 | KEEP |
| `app_units` | 0.7.8 | normal | `Au` in `query_container_size` | 1 / 0 | 0 / 0 | 0 | **REPLACE with re-export** |
| `url` | 2.5.8 | normal | `UrlExtraData` wraps `Arc<url::Url>`; `Url::parse` | 3 / 1 | 0 / 0 (url's own subtree is 35 crates, all kept by stylo) | 1 | KEEP |
| `stylo_static_prefs` | 0.21.0 | normal | `set_pref!` for the CSS properties pref'd off in the servo build | 2 / 0 | 0 / 0 | 0 | KEEP |
| `toml` | 1.1.6 | **dev** | reads `tests/properties.toml` | 0 / 4 in 1 file | 0 / 0 | 3 | KEEP |
| `px-dom` | path | normal | first-party | | | | |

### px-dom (1)

| Dep | Ver | Kind | Used for | Sites src/tests | Marginal crate / ws | Own unsafe | Verdict |
|---|---|---|---|---|---|---|---|
| `html5ever` | 0.39.0 | normal | tokenizer and tree-construction driver; our tree sink | 15 / 22 in 14 files | 25 / 3 (22 of the 25 are also needed by stylo) | 4 | settled |

### px-ipc (2)

| Dep | Ver | Kind | Used for | Sites | Marginal crate / ws | Own unsafe | Verdict |
|---|---|---|---|---|---|---|---|
| `serde` (`alloc`, `derive`) | 1.0.229 | normal | `#[derive(Serialize, Deserialize)]` on 4 types, `Message` trait bound | 16 in 2 files | 0 / 0 (stylo needs serde) | 2 | KEEP, tied to postcard |
| `postcard` (`alloc`) | 1.1.3 | normal | `to_allocvec`, `take_from_bytes`, plus 1 test call | 3 in 2 files | 4 / 4: `postcard`, `cobs`, `thiserror`, `thiserror-impl` (+2 lock-only `embedded-io`) | 12 (13 with the closure) | KEEP (ADR draft recommends deciding now) |

### px-net (3 normal + 1 dev)

| Dep | Ver | Kind | Used for | Sites src/tests | Marginal crate / ws | Own unsafe | Verdict |
|---|---|---|---|---|---|---|---|
| `rustls` (`ring`,`std`,`tls12`,`logging`) | 0.23.44 | normal | client config, `StreamOwned`, `RootCertStore`, `ServerName` via `rustls::pki_types` | 5 / 2 in 3 files | 12 / 10 on Linux (21 across all targets) | 5 | settled |
| `rustls-pki-types` | 1.15.1 | normal | **nothing**: zero occurrences of `rustls_pki_types::`; every use is spelled `rustls::pki_types::` | 0 / 0 | 0 / 0 | 1 | **REMOVE the declaration** |
| `webpki-roots` | 1.0.9 | normal **and** dev | bundled Mozilla floor (`TLS_SERVER_ROOTS`, 121 anchors) | 1 / 1 | 1 / 1 | 0 | KEEP now, REPLACE at Phase 20, section 3 |

The dev-dependency line `webpki-roots = "1"` is redundant: a normal dependency
is already visible to the crate's tests. Delete it with the next `px-net` touch.

### px-sandbox (2, target-scoped)

| Dep | Ver | Kind | Used for | Sites | Marginal crate / ws | Own unsafe | Verdict |
|---|---|---|---|---|---|---|---|
| `libc` (Linux) | 0.2.189 | normal | four `prctl` calls (`NO_NEW_PRIVS` set/get, `SECCOMP` set/get); the BPF constants are already hand-written in `linux.rs` | 4 in 1 file | 1 / **0** | 676 | KEEP |
| `windows-sys` (Windows) | 0.61.2 (+`windows-link` 0.2.1) | normal | 42 imported items: restricted token, job object, pipes, `CreateProcessAsUserW`, `STARTUPINFOEXW`, cert store enumeration | 9 `use` blocks in 2 files | 2 / **0** | 12,530 | KEEP |

`fuzz/` is a separate workspace with its own toolchain (ADR 006): `libfuzzer-sys`
0.4, plus first-party crates. It has no committed lockfile, is never shipped and
is outside `Cargo.lock`, so it is not counted above. Its closure was not measured
(the crate is not in the local registry cache and the disk was full).

`px-content` has `px-broker` and `px-sandbox` as dev-dependencies: first-party.

### Per-crate exclusive closure

Crates reachable from exactly one workspace member: `px-css` 106 crates / 3,310
unsafe tokens; `px-net` 24 / 3,945 (3,473 of that is `windows-sys` 0.52 and `wasi`,
which no shipped target compiles, section 6). Everything else is shared.

## 2. What cannot be reduced from our side

* **`stylo` is 41 crates / 1,606 tokens** (39 on Linux): `arrayvec`,
  `atomic_refcell`, `byteorder`, `core_detect`, `core_maths`, the three
  `crossbeam-*`, `either`, `encoding_rs`, `heck`, `icu_segmenter` and its data,
  `itertools`, `libm`, `mime`, `multiversion*`, `num-*`, `num_cpus`, `rayon`,
  `rayon-core`, `rustversion`, `simdutf8`, `strum*`, `uluru`, `walkdir`, `same-file`.
  `rayon`, `rayon-core`, `num_cpus`, `itertools` and `uluru` are not optional in
  stylo's manifest, so ADR 023's "rayon available but unused" is not a feature
  we can turn off. The rayon family (`rayon`, `rayon-core`, `crossbeam-deque`,
  `crossbeam-epoch`, `crossbeam-utils`, `either`, `num_cpus`, `hermit-abi`) is 8
  crates and 500 unsafe tokens that exist so a thread pool can be handed
  `None`. The only lever is upstream (making them optional behind a feature),
  which is a contribution, not a dependency decision.
* **`url` is a 35-crate subtree** (`idna`, `idna_adapter`, `percent-encoding`,
  `form_urlencoded`, `icu_collections`, `icu_locale_core`, `icu_normalizer`,
  `icu_properties` + data, `icu_provider`, `zerovec`, `yoke`, `zerofrom`,
  `zerotrie`, `tinystr`, `litemap`, `potential_utf`, `writeable`, `displaydoc`,
  `syn` 2 and 3 ...). `url` is in stylo's `servo` feature list. It is 23% of the
  Linux tree and entirely stylo-pinned. build-spec section 3 also names `url`
  and `idna` as wanted.
* **`rustls` is 10 crates on Linux**, of which `ring` (230 tokens, a C and asm
  build via `cc`, `shlex`, `find-msvc-tools`) is the backend choice from ADR 016.
  Settled; noted only because `cc` means a C compiler is a build requirement.

## 3. Verdicts outside the settled set

### app_units: KEEP; do not write our own `Au`

* In-repo: `px-css` 1 site on `main`. On `origin/phase/06-layout`, `px-layout` and
  an optional `px-broker` diagnostic add direct `app_units = "0.7.8"` and
  `px-layout/src/geom.rs` already contains the argument (it reaches the same
  verdict independently, and finds the real hazard, below).
* Marginal cost of removing it everywhere: **0 crates, 0 unsafe tokens** (the
  crate has none; 499 lines; its only dependencies are optional `serde`,
  `num-traits`, `malloc_size_of`).
* A saturating `Au` of our own is ~130 lines (your estimate; a minimal one with
  tests is ~150-200). It saves nothing in the tree and costs a second `Au` type:
  `app_units::Au` is part of stylo's trait surface (`values::computed::Au`
  re-export; `Au` appears in 21 stylo source files; `px-css`'s
  `query_container_size` returns it), so every font-metrics and container-query
  boundary would need a conversion layer. Two types with possibly different clamp
  ranges (`app_units` clamps to +-(2^30-1), Gecko's) is a worse place to be than one.
* The real risk is not saturation, it is that `Au` is `pub struct Au(pub i32)`:
  `Au(i32::MAX)` is constructible and turns the next `+` into a trap under
  `overflow-checks = true`. `Add` is `Au(self.0 + other.0).clamp()`, correct only
  while inputs are in range. That is a lint to enforce (the phase-06 branch's
  `ci/gate-layout.sh` bans the tuple constructor), not a reason to fork a type.
  Also `Div`/`Rem` (both the `Au` and `i32` forms) are a raw `self.0 / other` and panic on zero: wrap them in layout.
* Suggested: `px-layout` and `px-broker` should obtain `Au` through a `px-css`
  re-export (`style::values::computed::Au`) rather than each declaring
  `app_units`. This is the in-phase action (Phase 6) and is in the ADR draft.

### euclid: KEEP

6 sites in `px-css` (`Size2D`, `Scale` demanded by `Device::new`). 0 marginal,
44 own tokens (already in the closure). `px-layout` on the phase branch
deliberately avoids it (`LogicalSize { inline, block }`), which is right: do not
let it spread past the stylo boundary.

### url: KEEP

4 sites in `px-css` (3 src, 1 test), all to build a `UrlExtraData`. No
alternative exists: stylo's type is `Arc<url::Url>`. Its subtree is stylo-pinned.
`px-broker` has no `url` dependency on `main`; the phase-06 branch adds it as an
optional test-only dependency of a diagnostic binary.

### web_atoms: REPLACE the declaration (0 lines of new logic)

8 sites in 2 files. `markup5ever` re-exports `web_atoms::{LocalName, Namespace,
Prefix, ...}` (`markup5ever-0.39.0/lib.rs:36`), `html5ever` re-exports them, and
`stylo` defines `style::LocalName`, `style::Namespace` and `style::Prefix` as
`GenericAtomIdent<web_atoms::...StaticSet>` (`stylo-0.21.0/lib.rs`). So
`&style::values::GenericAtomIdent<web_atoms::LocalNameStaticSet>` becomes
`&style::LocalName`, and `&web_atoms::LocalName` becomes `&html5ever::LocalName`.
Risk: low, compile-time only; a wrong substitution fails to type-check. Not
compiled here (stylo's build needs Python Mako and the disk was full), so treat
as "verified by reading the re-exports", not "verified by building".

### app_units, again: REPLACE the declaration in px-css

`style::values::computed::Au` is `pub use app_units::Au` (`stylo values/computed/mod.rs:137`).
1 site. Same caveat as above.

### rustls-pki-types: REMOVE the declaration

Zero uses of the crate by name. Every site writes `rustls::pki_types::...` (5
in `tls.rs`/`tests/tls.rs`), which is the `rustls` re-export. `rustls` and
`webpki-roots` depend on it regardless, so 0 crates leave; one direct
declaration and a place where its version could drift from `rustls`'s go away.
`cargo-vet`'s `trusted.rustls-pki-types` entries stay (still in the tree).

### toml (dev): KEEP

4 call sites in one test. Removing it changes nothing: `stylo_static_prefs` has
`toml` 1.1.6 as a build-dependency, so the crate and its 3+141+47 unsafe tokens
(`toml`, `toml_parser`, `winnow`) stay either way. The existing `Cargo.toml`
comment (a `toml = "0.9"` once resolved a second copy: +3 crates, +55 unsafe
lines) is the right lesson: pin to the closure's version. A hand parser for the
manifest would save nothing and add a place for the fixture table and gate to
disagree.

### libc and windows-sys (ADR 010): KEEP

* `libc`: 4 call sites, all `prctl`. Hand-declaring
  `unsafe extern "C" { fn prctl(option: c_int, ...) -> c_int; }` is ~10 lines and
  `std` already links libc, so the replacement is real and low risk. **The
  benefit is nil**: `ring`, `getrandom`, `parking_lot_core` and `num_cpus` all
  pull `libc` (workspace-wide marginal 0 crates, 0 tokens). It would delete one
  declaration and make the next syscall (Landlock, seccomp, cgroup in Phase 17)
  a hand-declared ABI transcription. Phase 17 will want many more entry points,
  which is when the dependency pays for itself. Not worth an ADR.
* `windows-sys`: 42 imported items, 36 text lines mentioning `unsafe` in
  `px-sandbox`. A hand declaration is on the order of 300 lines of `extern`
  blocks and `#[repr(C)]` structs, including `STARTUPINFOEXW`, which is exactly
  the transcription ADR 010 and ADR 007 call the unrecoverable error (a process
  that looks sandboxed and is not). And again **0 crates**: on a Windows host,
  `winapi-util` (build-dependency of `stylo`, via `walkdir`) already needs
  `windows-sys` 0.61.2 and `windows-link`. The 12,530 tokens stay in the baseline
  either way. ADR 010's accounting stands; its conclusion is reconfirmed by a
  stronger argument than the one it gave.
* On the 15,941 tokens: they are 73.5% of the baseline and the figure to remember
  when someone asks "why is the unsafe count so high". They are generated
  declarations (ADR 010), not logic.

### serde + postcard: KEEP now; see 4.3 for the hand-rolled analysis

### webpki-roots: KEEP until Phase 20, then replace with a `data/` file

* 1 call site in `tls.rs` (`store.extend(TLS_SERVER_ROOTS...)`) + 1 test. 1 crate,
  0 unsafe, 0 transitive. It is the *only* third-party crate whose licence needed
  an addition to `deny.toml` (`CDLA-Permissive-2.0`).
* Why a replacement is interesting at all: ADR 012 and build-spec section 9 say the
  bundled store is a `data/` file "updated through the Phase 20 channel", like the
  PSL and HSTS list. `webpki-roots` is instead pinned by `Cargo.lock`: its
  freshness is whatever the last `cargo update` was. `origin/fix/rustls-advisories`
  just moved it 1.0.9 to 1.0.11 inside a lockfile bump, which shows the shape of the
  coupling.
* The replacement: `data/mozilla_roots.der` (length-prefixed DER, ~150-200 KB for
  today's 121 anchors; estimate, not measured), `ci/update-roots.sh` modelled on
  `ci/update-psl.sh`, and ~40-60 lines in `tls.rs` calling the existing
  `RootCertStore::add`. `anchor_from_trusted_cert` keeps name constraints from the
  certificate extension (`rustls-webpki trust_anchor.rs:94`), so the one anchor in
  today's list that carries name constraints (1 of 121) survives.
* Risk: **medium to high** relative to the saving. We would own provenance of
  trust anchors: which Mozilla/CCADB export, how distrust-after dates and
  server-auth-only filtering are applied (webpki-roots' generator does this; ours
  would have to). A stale or wrongly filtered floor is "exactly the anchor an
  attacker wants" (ADR 012). The saving is one crate and one licence line. It only
  becomes worth it when `px-update` can sign and verify the data (Phase 20),
  because only then does the bundle gain what the crate cannot give it. Until
  then: KEEP.

## 4. Duplicate versions (Cargo.lock)

`awk` over `Cargo.lock`: three crate names appear twice.

| Crate | Versions | Who pulls each | Compiled where | Fixable by us? |
|---|---|---|---|---|
| `windows-sys` | 0.52.0, 0.61.2 | **0.52.0**: `ring` 0.17.14 only, under `cfg(all(all(target_arch = "aarch64", target_endian = "little"), target_os = "windows"))` (`ring-0.17.14/Cargo.toml:256`). Brings `windows-targets` 0.52.6 and 9 `windows_*` per-arch crates. **0.61.2**: `px-sandbox`, and `winapi-util` (stylo build-dependency, via `walkdir`/`same-file`) | 0.52: **aarch64 Windows only, which no CI job, packaging file or doc mentions** | No (inside ring). `deny.toml` skips it with a reason. Section 6 makes the skip unnecessary. |
| `syn` | 2.0.119, 3.0.5 | **2.0.119** (12 proc-macro parents): `darling_core`, `darling_macro`, `derive_more-impl`, `malloc_size_of_derive`, `num-derive`, `phf_macros`, `strum_macros`, `stylo_derive`, `synstructure`, `to_shmem_derive`, `yoke-derive`, `zerofrom-derive`. **3.0.5** (6): `cssparser-macros`, `displaydoc`, `multiversion-macros`, `zerovec-derive`, **`serde_derive`**, **`thiserror-impl`** | build-time only (proc macros) | No. Two of the 3.x parents are ours (`serde_derive` via `px-ipc`, `thiserror-impl` via `cobs`) but 4 stylo-closure parents keep 3.x alive regardless, so dropping `postcard` does **not** collapse the duplicate. |
| `embedded-io` | 0.4.0, 0.6.1 | `postcard` 1.1.3's optional `embedded-io-04`/`-06` features, disabled in our build | **nowhere**; `cargo tree` never shows them; they are in the lockfile and `cargo vendor` (baseline entry `embedded-io-0.4.0`: 2 tokens) | Only by dropping `postcard`. |

## 5. Ranked reduction plan

Phase discipline (CLAUDE.md): `main` is at the start of Phase 6 (`px-layout`).
Items touching `px-net`, `px-css`, `px-ipc` or CI config are out of phase and
go to `docs/backlog.md` (paste-ready entries in Appendix A); I did not edit the
backlog because the brief asked for a single research commit.

| # | Step | Crates removed | Unsafe tokens | Risk | Phase it fits | ADR draft |
|---|---|---|---|---|---|---|
| 1 | Scope `deny.toml` and the unsafe baseline to the two shipped targets; delete the `windows-sys` skip | 13 stop being audited; **0 ship-time change** | 3,655 stop being counted | Low. Honest failure mode: a future target is added and nobody adds it to the graph | backlog; next time CI config is touched (Phase 0 infra) | `proposed-gate-scope-shipped-targets` |
| 2 | Prune direct declarations: `rustls-pki-types` (unused), `app_units` and `web_atoms` in `px-css`, redundant dev `webpki-roots`; in Phase 6 take `Au` via `px-css`, not a new `app_units` line in `px-layout`/`px-broker` | 0 (direct deps 17 to 14 distinct, 19 to 15 lines) | 0 | Low, compile-time checked | `px-layout` part: **Phase 6, in phase**. `px-net`/`px-css` part: backlog | `proposed-direct-deps-via-reexports` |
| 3 | Decide on `postcard`: wire-compatible hand-rolled codec | 4 (+2 lock-only) | 13 | Medium-high for a trust-boundary decoder; see 4.3 | **Decide before the IPC vocabulary grows**, i.e. before Phase 8 (`px-gpu` is the first big message producer). Code change is Phase 1's crate: backlog | `proposed-handrolled-ipc-codec` |
| 4 | `webpki-roots` to `data/` bundle | 1 | 0 | Medium-high (trust-anchor provenance) | Phase 20 with `px-update` | not drafted; section 3 is the sketch |
| - | `libc`, `windows-sys`, `app_units`, `euclid`, `url`, `selectors`, `stylo_*`, `toml` | 0 | 0 | | | KEEP |

Cumulative if 1 to 4 are all done: Linux tree 153 to 148, audited surface
169 to 151 crates across targets (about 11%), baseline 21,675 to ~18,000
(3,655 off-target + 2 lock-only + 13 postcard closure, about 17%; item 1 alone
is 17% and items 2 to 4 add under 0.1%), direct declarations 19 lines / 17 distinct to 11 distinct (`rustls-pki-types`,
`app_units`, `web_atoms`, `postcard`, `serde`, `webpki-roots` gone). If you only do the zero-risk ones (1, 2) you get
the 13-crate audit-surface cut and 4 fewer declarations.

### 4.3 serde + postcard: is a hand-rolled bounded encoder safer or riskier?

I built and measured one, outside the repo (prototype source in Appendix B).
It is wire-compatible with postcard for the current vocabulary: `FrameId`,
`FrameHost`, `Request` (3 variants), `Response` (4 variants).

* **Size:** 102 non-blank, non-comment lines for decode + encode + a bounded
  `Reader`, no macros, no `unsafe`, no allocation before a length is checked
  against the bytes actually remaining. Roughly 12 lines per enum variant with
  payload.
* **Equivalence:** 20,000,000 random inputs (0 to 11 bytes, biased toward small
  tags and varint continuation bytes), decoding as `Request` and as `Response`,
  compared to `postcard::take_from_bytes` for accept/reject, decoded value, and
  consumed length: **0 disagreements** (1,010,242 accepted, 18,989,758 rejected
  as `Request`). Encoding is byte-identical on 9 samples including `u32::MAX`
  indices and a 70,000-byte payload. This is weak evidence for a different
  type set but strong evidence that the format is simple and that the
  prototype is equivalent on it.

For safer:

* No generated code at the boundary; the whole decoder is readable in one
  sitting, and Miri/fuzz coverage is by construction of the same code the
  production path uses.
* `serde::Deserialize` for `Vec<u8>` depends on serde's size-hint capping and on
  `postcard` checking per element. It is correct today (a declared 1 GiB length
  costs at most a bounded preallocation) but the guarantee lives in two crates'
  conventions. In the hand-rolled version it is one `if len > rest.len()`.
* Removes `cobs` (framing we do not use: we frame with a 4-byte length prefix and
  a tag), `thiserror` and its proc macro from the build.

For riskier:

* postcard and `cobs` have **Bytecode Alliance audits** imported into
  `supply-chain/imports.lock` (lines 409, 415, 489, 498). Ours would be audited by
  us alone, and this crate's own `CLAUDE.md` says it gets an adversarial-review
  session for that reason.
* The cost is per type and scales with the vocabulary. Seven variants is 102
  lines. Phase 8 (display lists to the GPU process), Phase 13 (storage), Phase 21
  (MCP) will add dozens of types with strings, `Vec<T>`, `Option`, floats, maps.
  Hand-written codecs for those are where schema drift (encoder changed, decoder
  not) lives, and the cure is a `macro_rules!` or derive, which is `serde_derive`
  by another name, or a round-trip property test per type.
* `CLAUDE.md` says "IPC is postcard over typed enums with size limits". A
  wire-compatible codec arguably meets the format; it does not meet "postcard"
  the crate, so the ADR must amend that line. `postcard` is **not** in Appendix A
  (only html5ever, stylo, rustls, swash are), so this is allowed to be
  proposed, but ADR 003 explicitly weighed and rejected it.
* Payoff: 4 crates of 153 (2.6%), 13 tokens. `serde` and `serde_derive` stay (stylo,
  url), and so does the `syn` duplicate.

**Verdict: KEEP.** It is genuinely feasible and at today's size not scarier
than the status quo, but it buys 2.6% of the tree and trades an audited,
widely-fuzzed decoder for an unaudited one with a cost that grows with every
message. If crate count is the priority above all, the moment to do it is now;
after Phase 8 the migration is no longer cheap. The ADR draft is written so
that accepting it is a clean decision and rejecting it leaves a record.

## 6. Gates count what does not ship

`cargo tree --workspace --target all` vs the union of the two CI targets:

| Not built on x86_64 Linux or Windows | Tokens |
|---|---|
| `windows-sys` 0.52.0 (aarch64-windows via `ring`) | 3,411 |
| `windows-targets` 0.52.6 + 8 `windows_*` per-arch crates (all aarch64/i686/gnu variants) | 0 |
| `redox_syscall` 0.5.18 (parking_lot_core on Redox) | 178 |
| `wasi` 0.11.1 (getrandom on wasm) | 62 |
| `hermit-abi` 0.5.3 (num_cpus on Hermit) | 4 |
| **13 crates** | **3,655** |
| plus `embedded-io` 0.4.0 (lock-only) | 2 |

(`winapi-util` 24 tokens only appears for a Windows *build host*; `cargo tree
--target x86_64-pc-windows-msvc` run on Linux omits it, but the Windows CI runner
compiles it, so it belongs to the shipped set.)

`deny.toml` has no `[graph] targets`, so `cargo deny` evaluates all of them,
which is why the `windows-sys` skip exists. `ci/unsafe-audit.sh` runs `cargo
vendor`, which vendors the full lockfile. Neither changes what ships; both
overstate it by 16.9% of the unsafe baseline. There is no `aarch64`/`arm64`
anywhere in `.github/`, `packaging/`, `ci/`, `docs/` or the README, so the
surface is not an oversight about a platform you are building. If aarch64 Windows
is ever a target, the right time to add it back is that day.

This is an accuracy fix, not a reduction in what an attacker can reach: do not
count it as one in a release note.

## 7. Things I found that you did not ask about

1. `rustls-pki-types` is declared and unused in `px-net` (section 3).
2. Everything you suspected might be removable (`app_units`, `euclid`, `url`,
   `web_atoms`, `serde`, `libc`, `windows-sys`, `toml`) is pinned by `stylo`
   to the last crate. The only meaningful reductions need `stylo` itself to change.
3. `origin/phase/06-layout` (and the phase 7 branches built on it) adds eight
   optional direct dependencies to `px-broker` for a test-only `px-fetch`
   diagnostic: `px-net`, `px-dom`, `px-css`, `px-layout`, `html5ever`, `url`,
   `app_units`, `stylo`. They are `optional` behind `testing`, so a default build
   links none of them, and they add no crate to the lock, but they are 5 more
   places that must track `stylo`'s versions. Routing `Au` and `Url` through
   `px-css` re-exports would shorten the list.
4. `origin/phase/06-layout` carries proposed ADR 030 (Windows chain
   verification, option B is `rustls-platform-verifier`). That is the one
   in-flight proposal that would **add** crates to this graph; it is the
   dependency decision most worth watching against this audit. This audit did
   not evaluate it.
5. cargo-vet: 79 `[[exemptions]]` and 56 `[[trusted]]` entries today (proposed ADR 030
   on the phase-06 branch says the exemption budget moved from 1 to 77 in Phase 5). None
   of the proposed removals shrinks that: `postcard`/`cobs` are covered by
   Bytecode Alliance audits and `webpki-roots` by publisher trust.
6. ADR numbers: `main` stops at 028 but `origin/phase/06-layout` already holds 029
   and 030. The three ADR drafts are therefore named `proposed-<slug>.md` with no
   number, and are to be numbered at merge time.

## 8. Reproduction

```
git fetch origin main && git checkout -B research/dependency-audit origin/main
grep -c '^name = ' Cargo.lock                                   # 192
cargo tree --workspace --locked --target all --edges normal,build,dev --prefix none \
  | sort -u | grep -v '^px-' | wc -l                            # 169 (versions counted as separate lines)
cargo tree -p px-ipc --locked --edges normal,build              # postcard subtree
cargo tree --workspace --locked -i windows-sys@0.52.0 --target all
cargo tree --workspace --locked -i syn@3.0.5 --target all --edges normal,build --depth 2
cargo tree --workspace --locked -i serde@1.0.229 --target all --depth 3
awk '/^name = /{n=$3} /^version = /{print n, $3}' Cargo.lock | tr -d '"' \
  | awk '{c[$1]++; v[$1]=v[$1]" "$2} END{for(k in c) if(c[k]>1) print k, v[k]}'
```

Marginal counts were computed from `cargo metadata` as: closure(workspace) minus
closure(workspace with that edge deleted), intersected with the set of crates
`cargo tree --target all` actually builds (this drops the two lock-only
`embedded-io` entries that `cargo metadata` includes). Call-site counts: a
regex per crate path over `crates/**/*.rs`, comment lines skipped; `src`
includes `#[cfg(test)]` modules inside `src/`.

Not verified by building: the `web_atoms`/`app_units` re-export substitutions in
`px-css` (stylo's build needs Python Mako, which this container lacks); the size
estimate of a bundled DER root file; the 130-to-200 line estimate for an own `Au`.

## Appendix A: backlog entries (paste-ready)

```
## Delete the unused rustls-pki-types dependency and the redundant dev webpki-roots in px-net
- **Found in:** dependency audit (docs/research/dependency-audit.md), crates/px-net/Cargo.toml
- **Belongs to:** unassigned; next touch of px-net
- **What:** `rustls-pki-types` is declared and never named (every use is
  `rustls::pki_types::`). `webpki-roots` is declared as both a normal and a dev
  dependency. Neither removal changes Cargo.lock.
- **Why deferred:** px-net is a Phase 3 crate.

## px-css should not declare app_units and web_atoms
- **Found in:** dependency audit, crates/px-css/Cargo.toml
- **Belongs to:** unassigned; next touch of px-css
- **What:** `style::values::computed::Au` and `style::LocalName`/`html5ever::LocalName`
  already name these types. Fewer direct pins to keep in lockstep with stylo.
- **Why deferred:** px-css is a Phase 5 crate.

## Scope cargo-deny and the unsafe baseline to the shipped targets
- **Found in:** dependency audit, deny.toml and ci/unsafe-audit.sh
- **Belongs to:** unassigned
- **What:** 13 crates / 3,655 unsafe tokens are compiled on neither CI target
  (aarch64-windows via ring, redox, wasi, hermit). See
  docs/adr/proposed-gate-scope-shipped-targets.md.
- **Why deferred:** CI configuration, not Phase 6 work.

## Decide the postcard question before the IPC vocabulary grows
- **Found in:** dependency audit, crates/px-ipc
- **Belongs to:** decide before Phase 8
- **What:** see docs/adr/proposed-handrolled-ipc-codec.md.
- **Why deferred:** a decision, not a Phase 6 defect.
```

## Appendix B: prototype codec

Not committed as code. Lives in the session scratchpad; the body is reproduced
here because the 102-line figure and the zero-disagreement result are
load-bearing for the ADR draft. `Reader` and the helpers are the whole decoder;
the `Wire` impls are per type.

```rust
pub struct Reader<'a> { rest: &'a [u8] }
impl<'a> Reader<'a> {
    fn byte(&mut self) -> Result<u8, Malformed> {
        let (b, rest) = self.rest.split_first().ok_or(Malformed)?;
        self.rest = rest; Ok(*b)
    }
    // LEB128, at most 5 bytes, no bits beyond 32 (postcard rejects the same)
    fn varint_u32(&mut self) -> Result<u32, Malformed> {
        let mut value: u32 = 0;
        for i in 0..5u32 {
            let b = self.byte()?;
            let low = u32::from(b & 0x7f);
            let shifted = low.checked_shl(7 * i).ok_or(Malformed)?;
            if i == 4 && (low >> 4) != 0 { return Err(Malformed); }
            value |= shifted;
            if b & 0x80 == 0 { return Ok(value); }
        }
        Err(Malformed)
    }
    // the length is checked against the bytes actually left BEFORE allocating
    fn bytes(&mut self) -> Result<Vec<u8>, Malformed> {
        let len = usize::try_from(self.varint_u32()?).map_err(|_| Malformed)?;
        if len > self.rest.len() { return Err(Malformed); }
        let (head, rest) = self.rest.split_at(len);
        self.rest = rest; Ok(head.to_vec())
    }
}
// enum Request: tag as varint (0 Ping, 1 Echo{payload}, 2 FrameHost{frame}),
// then fields in declaration order; unknown tag => Err(Malformed).
```

Note: that prototype `split_first`/`split_at` is indexing-free and passes
`clippy::indexing_slicing`; `checked_shl`/`try_from` avoid `as` casts that wrap.
The differential harness (xorshift generator, `take_from_bytes` oracle) is ~40
lines and is not reproduced here.
