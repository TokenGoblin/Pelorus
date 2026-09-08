# Phase 0 gate report

*Skeleton and policy. Branch `phase/00-skeleton`.*

Written to the rule in `CLAUDE.md`: a green build is not a gate. This says what
each check actually proved, and — more usefully — what it did not.

## The five gate checks (build-spec §9)

| # | Check | Job | Local result |
|---|---|---|---|
| 1 | Builds on both OSes | `build` | **Pass on Windows.** Linux unverified — see below. |
| 2 | Hashes reproducible across two machines, different paths and usernames | `reproducible` | **Pass, partially.** Paths and HOME/CARGO_HOME differ. Usernames do not. |
| 3 | Supply-chain checks green | `supply-chain` | **Pass, and vacuous.** See below. |
| 4 | `forbid(unsafe_code)` everywhere | `unsafe-headers` | **Pass.** 21 crates asserted from source text. |
| 5 | Brand-leak gate proven by a deliberate violation | `brand-leak` | **Pass, proven.** |

Plus one job that is a Phase 0 deliverable but not one of §9's gate checks:

| Check | Job | Result |
|---|---|---|
| Forty-site compat list | `compat-list` | **Red. Outstanding.** |

## What has not happened

**CI has never run.** There is no git remote, so `.github/workflows/gate.yml`
has not executed once. Every result above is a local run of the same scripts the
workflow invokes — which is why the gate is scripts rather than YAML — but a
local run is not the gate.

A static audit of the never-executed parts found three defects that would each
have failed the first run on their own, and they are worth recording because
they were invisible to every passing check above:

1. **Every script was committed mode 0644.** `core.filemode` is false on a
   Windows checkout, so `chmod +x` never reached git. The workflow invokes the
   scripts directly, so the whole gate would have died with "Permission denied"
   before running one check. `ci/gate-structure.sh` now asserts that a shebang
   means executable in the index, which catches the next script written here.
2. **All three build steps moved `HOME` without setting `RUSTUP_HOME`**, which
   resolves the cargo shim to a toolchain directory that does not exist.
3. `gate.yml` had never been parsed. It does parse, all seven jobs are
   well-formed, and every script path a `run:` step names exists.

Specifically still unverified until a remote exists:

- everything on Linux, including check 1
- the second-user half of check 2
- `cargo install --locked cargo-deny cargo-vet cargo-auditable` on a clean
  runner, and how long it adds to every supply-chain run

**The deliberate violation was proven locally, not in CI.** Commit
`Phase 0: deliberate brand-leak violation` is a knowingly-red commit kept in
history precisely so that its CI run becomes the durable evidence the moment a
remote exists. Local output is in `tests/brand/evidence/`, with the product name
redacted, because that directory is not on the allowlist and an unredacted
transcript would itself be the leak.

## What each passing check does and does not prove

**Check 3 is vacuous today, and it matters that nobody forgets it.** The
workspace has zero third-party dependencies. `cargo-deny` has nothing to
license-check, `cargo-vet` reports success *in those words* — "Vetting Succeeded
(because you have no third-party dependencies)" — and `ci/unsafe-baseline.json`
is `{}`. What is proven: the tools run, the policy files parse, the store is
initialised, `imports.lock` is pinned and committed, and the SBOM really does
reach the binary. What is not proven: anything whatsoever about any dependency.
The first real test of this gate is Phase 3.

**Check 2 is missing one variable.** The two builds differ in checkout path,
`HOME` and `CARGO_HOME`, and both were verified byte-identical locally on
Windows. They do not differ in username. The workflow creates a second user on
Linux and runs the second build as them; on a hosted Windows runner a second
interactive account is not practical, so the Windows half of check 2 remains
path-and-environment only. If a username ever leaks into a binary it will be
through a path, which is remapped — but that is an argument, not a test, and it
is recorded here as an argument.

**And check 2 proves less than it looks like, for a reason worth writing down.**
The path remapping it depends on has never actually been exercised. All
twenty-one crates are empty, so no source path is embedded in either binary in
the first place — the hashes would match with the remapping switched off
entirely. The `--remap-path-prefix` logic gets its first real test when there is
code with panic locations in it, which is Phase 1. Until then, a green check 2
says the build is deterministic, not that the build is path-independent.

Note that reproducibility on Windows required `/Brepro`: MSVC stamps the PE
header with the wall-clock time of the link, so identical source cannot produce
identical bytes without it. Cargo's `trim-paths` profile option is still
unstable in 1.98, so path remapping is done with `--remap-path-prefix` in
`ci/build-and-hash.sh`, where the actual paths are known.

**Check 4 asserts source text, not behaviour.** It reads the attribute out of
every crate's entry point rather than trusting that it is there. It also fails a
local `allow(unsafe_code)`, and — for `px-content`, `px-net` and `px-mcp` — a
local waiver of any of §4.3's panic lints, because a rule that can be waived at
the call site is a preference rather than a rule.

**Check 5's gate contains no occurrence of the product name.** It reads
`PRODUCT_NAME` out of `px-brand` at runtime. It therefore cannot exempt itself by
accident, and a rename does not touch it. It scans `git ls-files`, so untracked
work is invisible to it locally; it says so when there is any.

## Decisions taken where the spec did not decide

Each is reversible, and each is stated in the commit that made it.

1. **The shipped binary is `px-browser`.** §2.1 wants the binary named from "one
   workspace variable", but Cargo requires `[[bin]] name` to be a literal — no
   interpolation, and no build script can rename a target. A neutral artifact
   renamed by `packaging/` keeps the entire `crates/` tree brand-free, which is
   what §2.1 was protecting, and keeps a rename inside one directory.
2. **`CLAUDE.md` and `crates/*/CLAUDE.md` are on the brand allowlist.** §2.2
   names five permitted paths and the working agreement is not among them, so
   without this the gate failed on its own heading from the first run — which
   would have made the deliberate-violation proof meaningless.
3. **`px-broker` and `px-content` are binaries, not lib-only skeletons**, so
   check 2 has linked artifacts to hash. §3 already describes both as processes.
4. **The unsafe-count gate is a committed script, not `cargo-geiger`.** It counts
   `unsafe` tokens over `cargo vendor` output. It over-counts — the word in a
   comment counts — and never under-counts, which is the correct direction for a
   gate; the baseline absorbs the over-count and only movement fails. This keeps
   a thinly-maintained tool that runs arbitrary build scripts out of the trust
   path of a security gate.
5. **Audit tools are installed with `cargo install --locked`**, not a
   marketplace action, for the same reason.
6. **Text-only checks run on Linux only.** Running a grep on two operating
   systems buys nothing and obscures which checks are genuinely
   platform-sensitive.
7. **`[workspace.lints.clippy]` is wired now**, with `px-content`, `px-net` and
   `px-mcp` opting in. §9 does not list it as a Phase 0 deliverable, but Phase 0
   is when those crates are created, and a deny that arrives after the first
   parser is written is a deny that gets waived.
8. **Our crates carry no `license` field**, and `cargo-deny` is told to ignore
   private crates. The licence is open decision 9; typing a default into a
   manifest would settle it by accident.

## Outstanding at the end of Phase 0

**The forty compat sites.** `tests/compat/sites.toml` is schema and reasoning
with zero entries. Only the person who will daily-drive the browser can write
it. Its job is red and deliberately separate from the phase gate.

**Open decision 9, sooner than §13 implies.** §8 rejects live CI partly because
it leaks the test list — and `sites.toml` then commits that list. Those are
consistent only while the repository is private. Once it is public with that
file in it, git history keeps it public. Settle it as "private through Phase 20"
(which invariant 10 already implies) or hold opaque ids with the mapping outside
the repository.

**The name reservations** in `docs/adr/000-name.md`: crates.io prefix, GitHub
org, domain, USPTO classes 9 and 42. Due before Phase 3, and none of them can be
done from inside this repository.

**`deny.toml`'s licence allowlist is speculative.** It permits what §3's named
dependencies are expected to carry. It has never matched a real crate, and
cargo-deny warns about every unused allowance until Phase 3.

## Not in Phase 0, deliberately

- The `content-release` profile with `panic = "abort"`. Cargo sets panic per
  profile, not per crate (§14.2), so it needs two build invocations and the
  Phase 1 ADR that costs them out.
- The §14.4 release-artifact scan for the test CA and automation symbols —
  nothing to scan for until Phases 8 and 12. In `docs/backlog.md`.
- The symbol audit enforcing network and filesystem confinement. `CLAUDE.md`
  states CI enforces it; CI does not yet, and the crates it would police are
  empty. In `docs/backlog.md`.
- Miri, ASAN, TSAN, loom and fuzz jobs. Each arrives with the crate it covers.
  In `docs/backlog.md`.
- Anything in `data/`. Phase 3.

## Verdict

Five of five gate checks pass locally on Windows, with check 2 short one
variable and check 3 vacuous by construction. Phase 0 is **not closeable** until
CI runs green on both operating systems, which requires a remote — and until the
compat list exists, without which Phase 23 has no meter.
