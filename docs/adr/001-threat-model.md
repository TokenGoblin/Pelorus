# 001 — Threat model

- **Status:** accepted
- **Date:** 2026-09-08
- **Phase:** 0
- **Invariants touched:** all of them; this is the document they answer to

## Context

Every invariant in build-spec §1 costs something — performance, compatibility,
or work. A control with no named adversary is a control nobody can argue with
and nobody can retire, which is how security budgets get spent on the wrong
things. This document names who the controls are for, and — just as
load-bearing — who they are not for.

Written in Phase 0, before the code, so that later phases can be checked
against it rather than rationalised into it.

## Decision

### Adversaries we defend against

**1. A malicious page.** The default assumption for every byte of content. It
controls its own HTML, CSS, JS, images, fonts and headers, and it wants code
execution, another origin's data, or the user's filesystem.

*Primary controls:* the content sandbox (Phase 2, hardened Phase 17); no JIT,
which removes attacker-controlled gadget generation and with it the practical
Spectre path (invariant 6); memory-safe application code over an audited unsafe
core (§4); generational DOM handles, so a stale ID resolves to None rather than
to type confusion (§4.1); parser depth and memory limits (§4.4).

*Residual:* stylo, boa_gc, swash and the GPU drivers contain real unsafe. The
sandbox, not the type system, is what holds when one of them is wrong.

**2. A malicious advertisement or third-party script.** A malicious page that
the top-level site did not intend to host, and which the user did not choose to
visit at all.

*Primary controls:* per-site process isolation and out-of-process iframes
(Phase 14) — without which "one process per site" is a claim, not a property;
px-block (Phase 19); CSP and frame-ancestors enforcement (Phase 12).

**3. A network observer.** Sees traffic, may modify it. Includes the user's ISP,
their coffee shop, and their government.

*Primary controls:* TLS via rustls with HSTS preload; DNS over HTTPS with a
first-run choice of resolver, because a hardcoded default provider transfers the
observation rather than removing it (Phase 3); mixed-content blocking (Phase 12);
no plaintext DNS, asserted by packet capture in Phase 3's gate.

*Residual:* traffic analysis. SNI and packet timing still say where you went.
Not addressed, and not claimed to be.

**4. A cross-site tracker.** Not trying to break anything. Trying to correlate
you across origins using storage, cache, connection reuse, or fingerprint.

*Primary controls:* partitioning by (eTLD+1 of top-level, origin) as a
type-level property, so there is no unpartitioned code path to leave enabled
(invariant 2); a frozen generic user agent carrying no product identifier;
letterboxing, because window size is the largest single fingerprinting vector;
query-parameter stripping and bounce-tracking mitigation (Phase 19).

*Residual:* partitioning breaks federated login, and that is a real cost paid by
the user. Decided by ADR in Phase 13 rather than discovered in Phase 23.

**5. A malicious MCP server.** The user connected an agent; the server on the
other end is hostile, or has been compromised, or is being fed prompt injection
from a page the agent is reading.

*Primary controls:* px-mcp is off at compile time by default, off at runtime by
default, and ungranted even when on (§7.1); it is sandboxed like a content
process and has no filesystem, so the audit log is written by the broker and
cannot be truncated by the thing being audited; file:// and internal schemes are
unreachable under every grant combination, permanently; no tool returns cookies,
storage, saved passwords or the certificate store, so no bug in one can leak
them; no download capability, denied rather than merely unlisted.

*On prompt injection specifically:* page content reaches the agent inside an
explicit untrusted envelope and is not sanitised. Filtering prose is not a
security boundary. The grant model is.

**6. A compromised agent.** The agent itself — Claude Code, a local model, a
script — is doing what an attacker wants, whether through injection or because
the user ran something they should not have.

*Primary controls:* the same grant model, plus scope and expiry. Session grants
die with the session, tab grants with the tab. No "always allow" for
console_eval or input injection. A px-mcp restart drops every grant, so a crash
loop cannot accumulate authority. Agent tabs are a separate partition with their
own cookie jar; reaching a signed-in tab is a distinct, per-tab act.

**7. A local unprivileged process.** Malware running as the user, or another
user on a shared machine, without administrative rights.

*Primary controls:* no TCP listener on any interface, ever — an
attacker-reachable automation port is how remote-debugging bugs have repeatedly
bitten shipping browsers; Unix sockets at 0600 in the user runtime directory,
Windows named pipes with an explicit DACL; test-only capabilities (the local CA
trust anchor, the WebDriver/testdriver surface) behind
`#[cfg(feature = "testing")]` **and scanned for in release artifacts**, because
a feature flag can be switched on by accident and a symbol in a shipped binary
cannot (§14.4).

*Residual, stated honestly:* profile encryption at rest via DPAPI or libsecret
protects against another user on the machine. It does not protect against
malware running as you, which can ask the keyring just as legitimately as we
can. The Phase 19 ADR says this in those words.

### Adversaries we do not defend against

Naming these is what keeps the ones above honest.

**Local administrator or root.** Can read our memory, replace our binary, and
install a kernel driver. There is no control we can apply from user space that
survives this, and pretending otherwise would mean shipping controls that cost
users something and buy nothing.

**Physical access.** Cold-boot attacks, DMA, an evil-maid firmware implant. Out
of scope. Full-disk encryption is the operating system's job and is a better
answer than anything a browser can do.

**A hostile kernel.** We build on the OS sandbox primitives — AppContainer,
seccomp-bpf, Landlock, user namespaces. If the kernel enforcing them is
compromised, or the primitive itself has a bug, we lose. The mitigation is a
small attack surface and a working patch channel, not a second sandbox.

**A targeted state actor with a full 0-day chain.** A renderer bug plus a
sandbox escape plus a kernel bug, aimed at a specific person. Real, and out of
scope for a solo-maintained project. What we can honestly offer against this
adversary is a smaller surface than the alternatives — no JIT, no WebRTC, no
DRM, no extensions, no PDF viewer — and a patch that ships quickly. Not
immunity.

## Consequences

Three things follow that are easy to forget once the code starts.

**The MCP subsystem is the softest thing in this document.** It is the only
component that deliberately hands an external party a channel into the browser.
That is why it is Phase 21 and not Phase 3: built before the capability broker
exists, it necessarily has ambient authority, and constraint cannot be
retrofitted.

**A same-origin bypass outranks a sandbox escape.** It needs no memory
corruption at all. Phase 12 exists for it, and it is the highest-severity bug
class in the project.

**No crash telemetry means no field visibility.** Invariant 4 forbids it and
that is the right call for the user, but the cost is real: we will not learn
about failures on other people's machines. Local crash dumps the user may
voluntarily attach to a report are the whole mitigation (§14.7).

## Verification

The escape-attempt suite in Phase 17 tests adversary 1 directly, and runs in CI
forever after. The leak harness in Phase 19 tests adversary 4. The Phase 21 gate
tests adversaries 5 and 6 by attempting each denied action under every grant
combination. Adversary 3 is tested by packet capture in Phase 3 and again in
Phase 19: a fresh profile makes zero network requests until a URL is entered.

Adversaries 2 and 7 have no single gate and are checked across Phases 12, 14, 17
and 21. That is a weakness of this document, recorded rather than hidden.

## Review

Re-read at the start of every phase whose gate cites it, and rewritten — not
amended — if the architecture changes shape. The lesson from Appendix B applies
here too: audit the plan again after any structural change to it.
