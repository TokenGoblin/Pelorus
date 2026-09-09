# Backlog

Out-of-phase defects land here, not in the current phase. Phases that absorb
every defect they encounter never close.

Format: one entry per defect.

```
## <short title>
- **Found in:** phase NN, <crate or file>
- **Belongs to:** phase NN (or: unassigned)
- **What:** <the defect, concretely>
- **Why deferred:** <why it is not this phase's work>
```

---

## Release-artifact scan for test-only capabilities

- **Found in:** phase 0, `.github/workflows/gate.yml`
- **Belongs to:** phase 8 (local CA anchor) and phase 12 (testdriver shim), and
  must exist before either ships
- **What:** §14.4 requires CI to scan built release binaries for the test CA's
  fingerprint and the WebDriver/automation entry symbols, and fail if either is
  present. Symbol absence in the artifact, not a feature flag being off — flags
  get enabled by accident. No such job exists.
- **Why deferred:** neither capability exists yet, so the scan would assert the
  absence of something that was never there and would pass for the wrong
  reason. It must land in the same phase as the capability it polices, not
  after it.

## Symbol audit for network and filesystem confinement

- **Found in:** phase 0, `CLAUDE.md`
- **Belongs to:** phase 3 (network side), phase 13 (filesystem side)
- **What:** the working agreement states "No network calls outside px-net and
  px-update. No filesystem access outside px-broker and px-store. CI enforces
  by symbol audit." CI does not. The rule is currently a convention.
- **Why deferred:** every crate is empty, so the audit has nothing to inspect
  and no baseline to hold. It needs to arrive with the first crate that could
  violate it, which is px-net in phase 3.

## Miri, ASAN, TSAN, loom and fuzz jobs

- **Found in:** phase 0, `.github/workflows/gate.yml`
- **Belongs to:** phase 1 (fuzz + loom, px-ipc), phase 2 (ASAN/TSAN,
  px-sandbox), phase 4 (Miri, px-dom)
- **What:** §4.5 requires Miri on px-dom, px-ipc and px-store unit tests, ASAN
  and TSAN builds of px-sandbox, loom for lock-free code in px-ipc, and
  cargo-fuzz targets for every parser and every IPC deserializer. None exist.
- **Why deferred:** each covers a crate that is currently an empty skeleton.
  Adding them now would mean six green jobs that inspect nothing, which is
  worse than no job — a passing check nobody has read is a check people stop
  reading.

## Windows half of the reproducibility gate does not vary the username

- **Found in:** phase 0, `.github/workflows/gate.yml`
- **Belongs to:** unassigned; revisit if a username ever reaches a binary
- **What:** gate check 2 asks for hashes reproducible across machines with
  different paths *and usernames*. The Linux job creates a second user and
  builds as them. On a hosted Windows runner a second interactive account is
  not practical, so the Windows job varies path, `HOME` and `CARGO_HOME` only.
- **Why deferred:** the residual risk is narrow — a username reaches a binary
  through a path, and paths are remapped — but that is an argument rather than
  a test, and it is recorded as one in `docs/phase-00-gate-report.md`.

## GitHub runner kernel features for the sandbox suite

- **Found in:** phase 0, while writing the CI matrix
- **Belongs to:** phase 2, confirmed by phase 17
- **What:** the sandbox gates need Landlock, seccomp-bpf and unprivileged user
  namespaces on the Linux runner. Hosted runner kernels may not offer all
  three, and §14.5 already flags that several distributions restrict
  unprivileged user namespaces by default.
- **Why deferred:** phase 2's ADR on the Linux fallback ladder is where this
  gets decided. Worth knowing before that ADR is written that CI itself may be
  one of the environments where the sandbox is unavailable.

## deny.toml licence allowlist is speculative

- **Found in:** phase 0, `deny.toml`
- **Belongs to:** phase 3
- **What:** the allowlist permits what §3's named dependencies are expected to
  carry. It has never matched a real crate, and cargo-deny warns about every
  unused allowance.
- **Why deferred:** correcting it against reality requires having the
  dependencies. Prune it when phase 3 lands the first ones.
