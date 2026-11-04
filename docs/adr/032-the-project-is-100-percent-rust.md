# 032 — The project is 100% Rust

- **Status:** accepted (user directive, 2026-11-04)
- **Date:** 2026-11-04
- **Phase:** cross-cutting (tooling; no product behaviour changes)
- **Invariants touched:** 7 (reproducible builds) -- by naming, not changing,
  the exception ADR 023 already recorded. No other invariant.

## Context

On 2026-11-04 the user made "100% Rust, all in Rust" a project requirement.
Until now the product crates were Rust but everything around them was not:
about 27 shell scripts and 4 Python scripts (`ci/*.sh`, `ci/*.py`,
`ci/lib.sh`, `tests/brand/gate-brand-leak.sh`) carry the gate logic that
decides whether a phase is done. That logic is as security-relevant as the
product code: a gate that silently stops checking is worse than no gate. The
scripts also cost portability (Windows needs Git Bash, and `lib.sh` carries
several workarounds for the Microsoft Store `python3` stub, `cp1252` consoles
and `core.filemode`), and a second language means a second set of idioms for
reviewers to hold in their heads.

The scripts are not being deleted wholesale: each encodes hard-won behaviour
(its comments record bugs that shipped). They are being **ported**, gate by
gate, behaviourally identically, by later work on top of the `px-xtask` crate
this ADR introduces.

## Decision

**The project is 100% Rust.** First-party source in any other programming or
scripting language is not accepted, and a ratchet enforces it.

**What "all Rust" covers:** first-party product source, build logic, tests,
CI gate logic, and developer tooling. Gate logic lives in `crates/px-xtask`,
run as `cargo xtask <gate>` (alias for `cargo run -p px-xtask --`). `px-xtask`
depends on `std` only; the project wants as few dependencies as possible, so
argument parsing, file walking and the small amounts of TOML/JSON extraction
are written by hand.

**What it does not cover** (these are data, config, or someone else's code):

- YAML workflow configuration (`.github/workflows/*.yml`). A workflow may
  contain `run:` lines, but each must invoke `cargo`, a Rust binary, or the
  platform; logic belongs in `px-xtask`, not inline in YAML.
- Data files: `.toml`, `.json`, `.lock`, `.der`, `.har`, `.dat`, `.tsv`,
  `.txt`, requirement pins, and similar.
- Documentation (`.md`) and SVG/image assets.
- Vendored third-party test corpora: `tests/wpt/**`, `tests/html5lib/**`,
  and committed fuzz corpora. These contain `.js`, `.py` and `.html` that we
  do not author and must not edit.
- `build-python/requirements.txt`: it is a data file that pins the one
  exception below; it is not Python source.

**The one named exception.** `stylo`'s `build.rs` runs `properties/build.py`
through Python 3 and Mako (ADR 023). That is a *dependency's* build script, not
first-party code, and it is the single place where a non-Rust language is
required to build the product. Invariant 7 was already amended for it. The
exception **stays** until either `stylo` removes the Python step or ADR 023's
rejected alternative (vendoring/forking `stylo` with the generated output
checked in) is revisited. This ADR does not hide it and does not claim the
product build is Python-free: it claims the *project's own* code is Rust. The
Python interpreter setup (`ci/setup-build-python.sh`, `ci/check_build_python_pin.py`)
is first-party glue around that exception and is itself on the ratchet's
allowlist to be ported to Rust; the interpreter pin it manages remains.

**WPT vendoring.** The WPT corpus itself is vendored third-party data and is
excluded. The *tool* that selects, vendors and updates it, once it exists,
is a Rust tool (an `xtask` subcommand), not a script. The same applies to
the other corpus generators (`ci/make-fetch-corpus.py`, `ci/update-psl.sh`,
`ci/update-hsts.sh`).

**Enforcement: a shrinking allowlist.** `cargo xtask all-rust` lists tracked
files (`git ls-files`) and fails if any first-party non-Rust source file
(by extension or name: `.sh .bash .py .ps1 .js .mjs .ts .rb .pl .bat .cmd`,
`Makefile`, ...) exists outside the excluded paths and outside
`crates/px-xtask/all-rust-allowlist.txt`. The allowlist holds every script
awaiting port, one path per line with a reason. It may **only shrink**: an
entry whose file no longer exists fails the gate (delete the line in the same
commit that deletes the file), and a new non-Rust file not on the list fails
the gate. Adding to the allowlist requires amending this ADR.

## Alternatives rejected

- **Keep shell for CI glue, Rust for product.** This is the status quo and the
  thing the user ruled out. Gate logic is where the project's own history shows
  silent failure (the corpus gate that passed locally and failed in CI).
- **Use `clap`, `walkdir`, `serde_json`, `toml` in the xtask.** Each is a
  reasonable crate and each is a dependency, a supply-chain entry, and an ADR.
  The gates need little: flag parsing for one-word subcommands, a recursive
  directory read, and line-oriented extraction of a few values. Hand-written
  std code is cheaper than the dependency.
- **A policy with no ratchet (a documented intention).** Unenforced
  requirements decay; a shrinking list makes progress measurable and the end
  state (an empty list) checkable.
- **Fork `stylo` now to remove Python.** Rejected by ADR 023 on cost; not
  reopened here. The exception is recorded instead.

## Consequences

- Porting is real work: each script carries comments recording bugs; ports must
  preserve behaviour and, where a gate proves itself with a deliberate
  violation, preserve that proof.
- `cargo xtask` compiles a crate before a gate runs. Source-only gates formerly
  ran with no toolchain; they now need `cargo`. Every CI job already has one.
- The Python 3 + Mako build input remains until the exception is retired, and
  the "100% Rust" claim must always be quoted with it.
- `px-xtask` is a workspace member and so appears in the structure and
  unsafe-header gates, which is correct: it is first-party code.

## Verification

`cargo xtask all-rust` passes in CI on Linux and Windows. The decision was
wrong if the allowlist cannot shrink (a script proves un-portable without
losing a check), or if gate logic migrates into YAML `run:` blocks to dodge the
ratchet.
