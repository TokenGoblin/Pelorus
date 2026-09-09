# 002 — The repository is public from Phase 0

- **Status:** accepted
- **Date:** 2026-09-08
- **Phase:** 0
- **Invariants touched:** 10 (clarified, not changed)

## Context

Open decision 9 in build-spec §13 asked two things together: what licence, and
whether the repository is public before Phase 20. This ADR answers the second.
The first is still open, and §"Consequences" says what that means in the
meantime.

Invariant 10 forbids distributing a build to anyone before Phase 20, on the
grounds that shipping a security tool you cannot patch is the one unrecoverable
mistake available. It says nothing about source. A public repository with no
release artifacts distributes nothing that anyone will run, so the two are not
in conflict — but the distinction has to be written down, because "it's public,
so try it" is exactly the pressure invariant 10 exists to resist.

## Decision

The repository is public from Phase 0. No release, no binary, no build
instructions promoted anywhere, until Phase 20 completes.

Git metadata is treated as a disclosure surface from this point. All commit
identities use a GitHub noreply address, and `ci/gate-structure.sh` enforces it
on every commit in the history rather than trusting local configuration.

## Alternatives rejected

**Private until Phase 20.** The safer default, and what invariant 10 implies if
read loosely. Rejected because a security project that is unreadable until it is
finished cannot be reviewed while the decisions are still cheap to change, which
is the only period when review is worth much.

## Consequences

**The compat site list cannot live in this repository.** §8 rejects live CI
partly because it "leaks your test list", then specifies committing that list to
`tests/compat/sites.toml` along with recorded traffic archives. Those are only
consistent in a private repository.

The disclosure is not primarily a security one. Forty ordinary websites tell an
attacker very little. What they tell everyone else is which sites the maintainer
uses every day, which is personal information about a person, and git history
makes it permanent — deleting the file later does not unpublish it.

Opaque ids are necessary and not sufficient, which is worth stating because it
is easy to stop there and feel finished: the `url` field names the site
directly, and the archives contain every hostname they replay. Renaming entries
while committing archives hides nothing.

So, in this repository: ids are opaque, `url` is absent, `why` may describe the
shape of a site but never identify it, and archives are not committed.
`ci/check_compat_sites.py` fails on an entry carrying a url. The mapping and the
archives live outside, and the replay runs where they are.

The cost is real and belongs here rather than in a footnote: **the project's
main progress meter is not publicly verifiable.** A reader of this repository
has to take "thirty-eight of forty pass" on trust. Alternatives were to accept
the disclosure, or to keep the whole repository private — the first trades a
person's habits for a verifiable number, the second trades early review for
both.

**A public repository with no `LICENSE` is "all rights reserved".** Nobody may
legally fork, patch, or redistribute it, which is an odd posture for a project
whose reason to be public is early review. That is the licence half of decision
9 and it is still open — but it is now open *in public*, with a default that
nobody chose. It should be settled early rather than at Phase 20.

**Security reports arrive before there is anywhere to send them.** `SECURITY.md`
with a disclosure address is a Phase 20 deliverable (§9). A public repository can
receive a report from the first week. Either write `SECURITY.md` early or accept
that reports land in public issues, which is the worst option for both parties.

## Verification

Falsified if the repository being public produces a release-shaped artifact
before Phase 20 — a tagged version, a build in the README, an installer someone
can run. Invariant 10 is the thing being protected; publicity is only acceptable
while it does not erode it.

## Follow-up

- [ ] Settle the licence half of open decision 9
- [ ] Decide whether `SECURITY.md` comes forward from Phase 20
- [ ] Write `tests/compat/sites.toml` with opaque ids; keep the url mapping and
      the traffic archives outside this repository
- [ ] Decide where the compat replay actually runs, since it cannot run here
