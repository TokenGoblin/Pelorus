# Overnight log

Running unattended. Newest entry last. This is a work journal, not a gate
report — `docs/phase-NN-gate-report.md` is where conclusions live.

## Scope authorised

The user chose to continue past the point where I recommended stopping, and
then said they trust my recommendations. I am reading that as: proceed with the
wide scope, and take the **recommended** option at each decision the spec calls
theirs. Two such decisions arise tonight and both are recorded as ADRs for the
user to confirm or overturn:

- **§14.5, the Linux sandbox fallback** — refuse to run, naming the exact
  sysctl, with a `--no-sandbox` flag behind interactive confirmation and a
  permanent warning banner. Never a silent fallback. (ADR 007)
- **Where unsafe OS operations live** — `px-sandbox`, whose remit widens from
  "sandboxing" to "the audited unsafe core". ADR 005 deferred this to Phase 2.
  (ADR 008)

Guardrails I am keeping regardless:

- **Phase 2 does not merge.** It stays on `phase/02-sandbox` for review.
- `px-sandbox` gets an adversarial review session once it has real code, per
  build-spec §10 and its own `CLAUDE.md`. Tonight's evidence for why: two
  reviews of Phase 1 produced 38 findings, and the second found that the
  product binary always exited failure.
- No dependency outside build-spec §3 without stopping to ask.
- Every unsafe block gets a `// SAFETY:` comment citing the OS documentation it
  rests on, not the fact that it happened to work.

---

## 03:4x UTC — waiting on the fuzz campaign

Campaign [34303798308](https://github.com/TokenGoblin/Pelorus/actions/runs/34303798308)
still running: six shards, four hours each, ~06:35 UTC. It is the last open
item on Phase 1's gate, so the merge waits for it.

Everything else is green on both operating systems. `compat-list` is red and
expected to be — the forty-site list is the user's to write.

Read `docs/build-spec.md` §9 Phase 2 and §14.5 to ground the ADRs. Phase 2's
gate is three things:

1. a content process launches under a policy on both OSes;
2. with the sandbox forced unavailable, the browser **refuses** to launch
   content processes and says why;
3. the Linux capability ladder is documented and exercised.

The second is the interesting one to build first, because it is the one that
fails closed and the one a user will actually hit.
