# Brand guide

*Draft 1. The name is provisional (ADR 000); everything here is written so a
rename changes strings and not meaning. Nothing in this guide is for public use
before Phase 20 (invariant 10).*

## The idea

**Safety you can check, not safety you are asked to believe.**

Most security products ask for trust and answer with adjectives. This one is
built so the claims are structural: the network is used for pages and patches
and nothing else, state is partitioned by construction, and a control that
cannot be applied stops the browser instead of being skipped. The brand's job is
to say that plainly and then get out of the way.

The name carries the same argument. A pelorus is a sighting compass with no
magnet: it reads bearings relative to your own vessel, not to an outside
reference. Your browser should work the same way. It takes its bearings from
you.

The logo is that object: a bearing ring, a sight line, a vessel heading arrow,
and deliberately no needle (`packaging/logo/pelorus.svg`).

## What it should feel like

Calm, steady, unhurried. The feeling of a well-kept instrument, not of an alarm.
Safety is conveyed by what the product leaves out (no feed, no promos, no
nags: invariant 5) more than by shields and padlocks.

## Voice

- **Plain.** Short sentences, concrete nouns. "No telemetry is compiled in", not
  "industry-leading privacy".
- **Specific.** Name the mechanism. "Each site gets its own cookies, cache and
  connections" beats "advanced tracking protection".
- **Honest about limits.** The spec lists who it is *not* defending against
  (local admin, physical access, a state actor with a full zero-day chain).
  The brand says so too. Candour is the trust signal.
- **Quiet.** No exclamation marks, no fear, no "hackers are after you".

### Words we do not use

"Unhackable", "100% secure", "military-grade", "bulletproof", "impenetrable",
"guaranteed safe", "completely anonymous". Each is false or unfalsifiable, and
an unfalsifiable security claim is the thing this project exists to avoid.

## Claims we may make, and what backs each one

A claim may appear in public copy only if its backing exists and its gate is
green. If the gate is red or the phase is not done, the claim is not made.

| Claim | Backed by |
|---|---|
| Pages run in a sandbox, and the browser will not start unsandboxed | Invariants 1, 8; Phase 2 gate |
| Sites cannot follow you from one site to another through cookies, cache or connections | Invariant 2; `px-net` partitioning tests |
| No telemetry, crash reporting or call-home | Invariant 4; no such endpoint is compiled in |
| The only non-page network call is a signed update check with no identifiers | Invariant 4 |
| No JIT in the script engine | Invariant 6 |
| Memory-safe application code over a small, audited unsafe core | `forbid(unsafe_code)` policy, `ci/unsafe-baseline.json` |
| Reproducible builds | Invariant 7, **with its stated caveat** (a pinned Python is a build input, ADR 023). Say "reproducible given the pinned build tools", not the unqualified form |
| Silent by default | Invariant 5 |

Never: any claim about blocking ads or malware generally, "safe browsing", or
anything that implies the product protects against a threat outside
`docs/adr/001-threat-model.md`.

## Taglines (candidates, none chosen)

1. **Your bearings, not theirs.** (leans on the name)
2. **A browser that stays in its lane.** (sandbox, partitioning)
3. **Quiet by design.** (invariant 5)
4. **Checkable safety.** (the central idea, most literal)

Recommendation: lead with 1 for the name and use 4 as the explanatory line.

## Colour

Navy for steadiness, teal for clarity, amber used sparingly because it means
"look here", which is also what a warning is for.

| Role | Hex | Use |
|---|---|---|
| Deep navy | `#070D1F` to `#14264A` | Backgrounds, logo face |
| Bearing teal | `#5EEAD4` | Accent, secure/ok state, ring start |
| Chart blue | `#2563EB` | Links, ring end |
| Sight amber | `#FB923C` to `#FDE047` | The one thing to notice; warnings |
| Instrument white | `#E2E8F0` | Text on dark, vessel arrow |
| Slate | `#CBD5E1` | Secondary text, ticks |

Rules:

- **Never use red for a secure state, or green for a warning.** Teal means
  fine, amber means look, red is reserved for refused or blocked.
- Contrast: body text on navy must meet WCAG AA (4.5:1). Check before shipping a
  new pairing.
- Invariant 3 applies: browser chrome does not change for brand reasons. This
  palette governs the logo, installer, site and documents; any change to the
  shipped UI needs the ADR invariant 3 requires.

## Logo use

- Use `packaging/logo/pelorus.svg` as is. Do not rotate the sight line, add a
  needle, add a shield or padlock, or put text inside the ring.
- Clear space: at least one quarter of the diameter on every side.
- Minimum size: 24 px. Below that the ticks are noise; use the plain ring and
  arrow (to be drawn when a favicon is needed).
- On light backgrounds the navy face is the contrast; do not remove it.

## Where the brand string may appear

Only in `px-brand`, `docs/`, `packaging/`, the README and the workspace
`Cargo.toml` (invariant: `tests/brand/gate-brand-leak.sh`). The logo contains no
text for this reason. A wordmark, when one exists, is a `packaging/` asset.

## Open decisions

- **The name is not cleared** (no trademark search, domain or organisation yet;
  ADR 000). Do no public brand work, and register nothing, until it is.
- **Wordmark and typeface** are not chosen. Prefer a free, self-hosted face so
  the installer and site fetch nothing from third parties (invariant 4's spirit).
- **A small-size mark** for favicons and the taskbar.
- **Whether to name the threat model publicly**, as a trust signal, and what to
  say in it. Recommended: yes, near-verbatim from ADR 001.
