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

## 04:20 UTC — ADRs drafted; campaign 1h45m in

Campaign still running (started 02:35, six shards of four hours, so ~06:35).
Nothing to do on Phase 1 until it lands, so I drafted the two Phase 2 ADRs into
the scratchpad rather than committing Phase 2 content onto the Phase 1 branch.
They go in as the first commit on `phase/02-sandbox`.

**ADR 007 departs from §14.5, and the user should look at this specifically.**
§14.5 frames the Linux question as binary — SUID helper, or refuse to run when
the sandbox is unavailable. On Linux the sandbox is not one mechanism but four
(`no_new_privs`, seccomp-bpf, Landlock, user namespaces), and **only the last
is commonly restricted**. Treating its absence as "no sandbox" discards three
mechanisms that are still available and still real, and it refuses in exactly
the scenario §14.5 worries about — which is what drives a user to
`--no-sandbox`.

So ADR 007 takes §14.5's recommendation (refuse to run, never silently degrade)
and adds a **floor**: the ladder is applied rung by rung, and refusal happens
below a defined minimum rather than on the loss of any single rung. Floor on
Linux is `no_new_privs` + seccomp-bpf; on Windows a restricted token + job
object.

A second thing §14.5 could not have known: **there is no single sysctl to
name.** The restriction is `kernel.unprivileged_userns_clone` on Debian
derivatives, `kernel.apparmor_restrict_unprivileged_userns` on recent Ubuntu,
and `user.max_user_namespaces` on RHEL-family kernels. §14.5's recommendation
is to name it precisely, and naming the wrong one sends a user to edit a
setting that does not exist on their machine. The message has to be derived
from the condition actually detected.

The honest cost is in the ADR: on a kernel with user namespaces restricted,
this browser runs with a weaker sandbox than Chromium would, because we will
not ship a setuid binary. That is the trade.

ADR 008 answers what ADR 005 deferred — `px-sandbox` owns every unsafe OS
operation, not only sandboxing ones — and records the argument for the `px-os`
split that was rejected, because the naming argument for it is good and will
come back at Phase 17.

Next: waiting on the campaign. A monitor is armed on it, so the merge starts
the moment it reports.
