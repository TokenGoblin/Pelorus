# px-xtask

Repository gates and developer tooling. The home of every check that used to
be a shell or Python script under `ci/` and `tests/` (ADR 032: the project is
100% Rust). Run as `cargo xtask <gate>`; `cargo xtask list` names them.

Full specification: `docs/build-spec.md`. Root working agreement:
`/CLAUDE.md` — it applies here too; this file only adds what is local.

## Local invariants

- `std` only. No third-party dependency, ever, without an ADR. Hand-write the
  argument parsing, directory walking and TOML/JSON extraction.
- A gate is `fn(&mut Gate, &Path, &[String])` registered in
  `src/gates/mod.rs`. It reports through `Gate::ok / info / fail`; the caller
  prints the verdict. Exit code: 0 PASS, 1 FAIL, 2 usage or environment error.
- Every gate takes the repository root as a parameter and runs git with that
  root as cwd, so it can be unit-tested against a fixture tree in a temp
  directory. Put the decision in a pure function and test that.
- A gate that proves itself with a deliberate violation (brand-leak) keeps that
  proof as a unit test, not only a transcript.
- `all-rust-allowlist.txt` may only shrink. Do not add a line to it; port the
  script instead. Delete the line in the same commit that deletes the script.
- Never write the product name here. Tests use a synthetic name.
