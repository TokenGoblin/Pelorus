# 007 — The Linux sandbox is a ladder with a floor, and below the floor we refuse

- **Status:** accepted, pending the user's confirmation — see "How this was decided"
- **Date:** 2026-09-09
- **Phase:** 2
- **Invariants touched:** 8 (fail closed), clarified rather than weakened

## How this was decided

build-spec §14.5 marks this HIGH and calls for an ADR in Phase 2 choosing
between a SUID helper and refuse-to-run. It is a product judgement about what
happens to a user whose distribution ships a kernel we cannot sandbox on, and
it is the user's to make.

It was taken here, unattended, because the user asked for overnight progress
and then said they trust the recommendation. **They should confirm or overturn
it.** The recommendation is §14.5's own — refuse to run — and this ADR takes it
with one substantive addition the section does not consider, described below.

## Context

Invariant 8 says: if a security control cannot be applied, the operation does
not proceed. §14.5 observes that a strict reading makes the browser refuse to
start on distributions that restrict unprivileged user namespaces, and that
users respond to that by disabling security rather than by fixing their kernel.

The framing in §14.5 is binary — sandbox available or not — and that is the
part worth revisiting before choosing between its two options. On Linux the
sandbox is not one mechanism. It is at least four, with independent
availability:

| Mechanism | Gives us | Typically needs |
|---|---|---|
| `no_new_privs` | No privilege gain through exec | Nothing; any modern kernel |
| seccomp-bpf | Syscall filtering | Nothing; any modern kernel |
| Landlock | Filesystem restriction | Kernel 5.13+, ABI version varies |
| User namespaces | Mount/PID/network isolation | Often restricted by policy |

**Only the last one is commonly restricted**, and it is the one §14.5's
scenario is about. Treating its absence as "no sandbox" throws away three
mechanisms that are still available and still real.

There is also no single sysctl to name, which matters because §14.5's
recommendation is to name it precisely. The restriction is spelled differently
depending on the distribution:

- `kernel.unprivileged_userns_clone=0` — Debian and derivatives, historically
- `kernel.apparmor_restrict_unprivileged_userns=1` — recent Ubuntu
- `user.max_user_namespaces=0` — RHEL-family and hardened kernels

A message naming the wrong one is worse than a generic message, because it
sends the user to edit a setting that does not exist on their system.

## Decision

**A ladder with a floor.**

Each rung is applied if available, and the set actually applied is recorded and
surfaced. The floor is the minimum that must be present for a content process
to launch at all:

- **Floor (Linux):** `no_new_privs` **and** seccomp-bpf. Below this, refuse.
- **Rung:** Landlock, when the kernel offers it, at the best ABI available.
- **Rung:** user namespaces, when unrestricted.
- **Floor (Windows):** a restricted token **and** a job object. Below this,
  refuse. AppContainer and the Win32k lockdown are Phase 17's tightening.

**Refusing means refusing.** No content process launches, and the message says
which mechanism was missing, what the platform-specific remedy is *for the
condition actually detected*, and what the consequence of overriding is. It
does not name a sysctl we did not check.

`--no-sandbox` exists, and:

- it requires an interactive confirmation — it cannot be set once and forgotten
  in a desktop file or a wrapper script;
- it displays a permanent warning banner in the chrome for the whole session,
  not a dismissible dialog;
- it is never selected automatically, at any point, for any reason.

**There is no silent fallback anywhere on the ladder.** Falling from a rung is
logged and reflected in the browser's own reporting of its state.

## Alternatives rejected

**A SUID helper (Chromium's approach).** It makes the sandbox available on
kernels that restrict user namespaces, which is a real benefit for real users.
Rejected because a setuid binary is a permanent, privileged attack surface
shipped to every user in order to serve the subset whose kernel is restricted —
and for a solo-maintained project, "we ship a setuid root binary" is a sentence
that should be very hard to write. §14.5 says the same, more briefly.

Worth stating plainly: this decision costs those users something. On a kernel
with user namespaces restricted, this browser runs with a weaker sandbox than
Chromium would. That is the trade, and it is not free.

**Binary refuse-to-run, as §14.5 frames it.** Rejected because it refuses in
cases where three of four mechanisms are available, which is the scenario §14.5
is worried about and the one most likely to drive a user to `--no-sandbox`. A
control that gets disabled is worth less than a weaker control that stays on.

**Silent degradation.** Never. A browser that quietly runs unsandboxed because
a syscall failed is the thing invariant 8 exists to prevent.

## Consequences

**Invariant 8 is not weakened, but it is now specific.** "If a security control
cannot be applied, the operation does not proceed" needs a definition of *the*
control. The floor is that definition, and it lives in one place in
`px-sandbox` so it can be raised in one edit.

**Phase 17 will want to raise the floor**, and that re-opens this decision for
exactly the users it protects today. Whoever does it must decide again whether
a user on a restricted kernel gets a working browser. Do not raise it silently.

**The applied set has to be observable.** A user, and a bug report, must be
able to say which rungs were in force. `--no-sandbox` and a missing rung must
be distinguishable in the UI and in any diagnostic output.

**The detection is itself security-relevant code.** A capability probe that
wrongly reports success is worse than one that wrongly reports failure: the
first launches an unsandboxed process believing it is sandboxed. Probes fail
closed — an inconclusive probe counts as unavailable.

## Verification

Falsified if the floor turns out to be unreachable on a mainstream
distribution, in which case the refuse-to-run path is what users meet and the
SUID question comes back. Phase 2's gate exercises the refusal directly: with
the sandbox forced unavailable, no content process launches and the message
names the missing mechanism.

## Follow-up

- [ ] User confirms or overturns this ADR
- [ ] Phase 17: decide whether to raise the floor, and re-take this decision
      for the users it would exclude
