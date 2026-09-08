# 000 — The product name is Pelorus, provisionally

- **Status:** accepted
- **Date:** 2026-09-08
- **Phase:** 0
- **Invariants touched:** none

## Context

The project needs a name before it needs a repository, and picking one badly is
expensive in a way that is easy to underestimate: a name appears in the binary,
the installer, the config directory, the internal URL scheme, the UI chrome, and
the update endpoint. Changing it later touches all of them.

Nothing about the name has been cleared. No trademark search has been run, no
domain registered, no crates.io prefix held, no GitHub organisation claimed.

## Decision

The product is called **Pelorus**, provisionally, and the codebase is built so
the name can be replaced in a day rather than a quarter.

A pelorus is a sighting compass with no magnet: it reads bearings relative to
your own vessel rather than to any external reference. That is the product's
argument in one object.

"Provisionally" is load-bearing, not hedging. It is why build-spec §2 exists,
why the internal crate prefix is the neutral `px-`, why the shipped binary is
called `px-browser` and gets its user-facing name from packaging, and why CI
fails the build if the literal appears outside the places that are allowed to
know it.

## Alternatives rejected

Not rejected — **held**. These survive as alternates and are the pool a rename
draws from: Quindar, Palfrey, Azimuth, Binnacle, Lethe, Temenos.

Rejected: naming it later. A codebase without a name accumulates a hundred
informal ones, and every one of them is a leak the gate cannot catch because
nobody knows to look for it.

## Consequences

Renaming costs, in full (Phase 24):

1. Change the constants in `px-brand`.
2. Run the brand-leak gate; it finds every hardcoded occurrence.
3. Regenerate the UI snapshot baselines, or rely on the masked name region
   (§14.6).
4. Append the outgoing `CONFIG_DIR` to `LEGACY_CONFIG_DIRS` — append, never
   replace, or the profile of anyone who skipped a release is orphaned.
5. Register the legacy internal scheme alongside the new one for one release
   cycle.
6. `git mv` the repository directory, which is cosmetic.

And the exception that makes the rest safe: **the update URL and the release
signing key do not change.** They are brand-independent and permanent (§14.1),
they live in `px-update`, and a rename does not touch them. If the domain ever
must change, both are served for a minimum of two years. Renaming the update
endpoint would silently end security updates for every installed copy — the one
failure mode in this list that cannot be recovered from, because the users it
affects are exactly the ones who can no longer be reached.

## Verification

Falsifiable by the trademark search. If Pelorus is taken in USPTO classes 9 or
42, or the domain is unobtainable, the decision was wrong and Phase 24 executes
the rename against an alternate.

## Reservation work — due before Phase 3, and not startable by Claude

These require an account, a card, or a legal search, and none of them can be
done from inside this repository. They are listed here so that "provisional"
has a deadline rather than becoming permanent by inattention.

- [ ] crates.io: publish `px-brand` as a stub to hold the `px-` prefix
- [ ] GitHub organisation
- [ ] Domain — noting that the *update* domain must be one intended to be held
      for a decade, since §14.1 forbids changing it
- [ ] USPTO search, classes 9 and 42
