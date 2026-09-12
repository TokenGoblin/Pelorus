# Pelorus — working agreement

Full specification: docs/build-spec.md. Read §9 for the current phase at the
start of a phase. Do not load the whole spec every session.

## Hard rules
- Every crate begins with `#![forbid(unsafe_code)]`. Two crates are excepted,
  with different rules — read the difference, it is the point:
  - `px-sandbox` (ADR 008): `#![deny(unsafe_op_in_unsafe_fn)]`, a `// SAFETY:`
    comment on every unsafe block, reviewed against the OS documentation.
  - `px-css` (ADR 024): may declare `unsafe fn` where a `stylo` trait signature
    requires it — `forbid` rejects *implementing* an unsafe method, which is
    why the lint cannot stay. It contains **zero** `unsafe` blocks and **zero**
    `unsafe impl`. An `unsafe fn` body needs neither, so nothing in the crate
    does anything the compiler is not checking. `ci/gate-style.sh` enforces
    both halves. This is narrower than an exemption; do not read it as one, and
    do not quote it as precedent for anything broader than a trait signature we
    do not control.
- Authority comes from the channel, never from the message. The broker
  identifies callers by connection. No process sends its own identity,
  origin, partition key, or TabId and expects it to be trusted.
- Fail closed. If a check cannot be completed, deny.
- DOM handles are generational and every accessor returns Option. Never add
  an infallible index API.
- No unwrap/expect/panic/indexing in px-content, px-net, px-mcp. Clippy
  denies these; do not add allow attributes to get around it.
- The product name appears ONLY in px-brand, docs/, packaging/, README, and
  the workspace Cargo.toml. Take the constant from px-brand everywhere else.
  The update URL and signing key are NOT branded and never change.
- No new crate dependency without an ADR. Propose; do not add and continue.
- No network calls outside px-net and px-update. No filesystem access
  outside px-broker and px-store. CI enforces by symbol audit.
- IPC is postcard over typed enums with size limits. No serde_json in IPC.
- Test-only capabilities (local CA trust anchors, WebDriver/automation) are
  behind `#[cfg(feature = "testing")]` and are scanned for in release
  artifacts. Never let one reach a shipping binary.
- Nothing is distributed to anyone before Phase 20 completes.

## Phase discipline
- Work only on the current phase. Out-of-phase defects go to docs/backlog.md.
- One phase = one branch. One coherent unit of work = one commit.
- A phase is done when its gate passes in CI on both Windows and Linux. A
  green build is not a gate.
- Write the phase's gate tests first, failing, before implementation.
- Decisions listed in docs/build-spec.md Appendix A are settled. Do not
  re-propose them; if you think one is wrong, say so explicitly and wait.

## When stuck
- Read the WHATWG/W3C spec. Do not infer behavior from another browser's
  observable output — that produces code that passes your tests and fails
  real sites.
- If a spec is ambiguous, encode the ambiguity as an #[ignore] test with a
  comment and raise it. Do not guess.
