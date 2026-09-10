# 013 — Revocation by pushed CRL set; no OCSP, ever

- **Status:** accepted
- **Date:** 2026-09-09
- **Phase:** 3
- **Invariants touched:** 4 (this is what enforcing it looks like for
  revocation)

## Context

A certificate can be revoked before it expires, and a browser that ignores that
accepts certificates the issuer has disowned. Three mechanisms exist in
practice, and the choice is already made for us by invariant 4 — this ADR
records *why*, because "no OCSP" without the reasoning is a rule someone
reasonable will try to relax later.

**OCSP.** The client asks the CA, in real time, whether a specific certificate
is still valid. That request names the site being visited, and it goes to a
third party the user never chose, unencrypted in the classic deployment, on
every new connection. It is a browsing-history feed to the CA. Invariant 4 says
the network is used for pages the user requested and signed update manifests,
nothing else; an OCSP request is neither. `crates/px-net/CLAUDE.md` already
carries this as a local invariant.

It is also, famously, not a security control. OCSP fails *soft* in every
shipping browser: a responder that times out or errors is treated as "not
revoked", because failing hard would mean the CA's uptime gates the web. An
attacker positioned to serve a revoked certificate is positioned to drop the
OCSP request. So the classic deployment pays a permanent privacy cost for a
check the attacker can turn off.

**OCSP stapling** answers the privacy objection: the *server* fetches the
response and staples it to the handshake, so the client asks nobody. It does
not answer the security one — an attacker with a revoked certificate simply
staples nothing, and soft-fail accepts it. `must-staple` fixes that, and is
deployed on a rounding error of the web.

**Pushed CRL sets.** The vendor aggregates revocations out of band and ships a
compact set to clients. This is CRLSets (Chrome) and OneCRL (Firefox). It fails
*closed* for everything it covers, costs no per-connection traffic, and leaks
nothing — the client asks nobody anything. Its weakness is coverage: the set is
a curated subset, not every revocation ever issued.

## Decision

**Revocation is checked against a signed, versioned CRL set carried in
`data/`, updated through the Phase 20 channel. No OCSP request is ever made,
and no OCSP responder URL is ever contacted — not stapled-only, not
best-effort, not behind a preference.**

A stapled OCSP response that arrives in the handshake **is** honoured when it
says *revoked*, because that costs nothing and no request was made. It is never
required, and its absence is never an error.

The check fails closed within its coverage: a certificate in the set is
rejected, full stop, with no soft-fail path and no click-through.

## Alternatives rejected

**OCSP, live.** Rejected by invariant 4, and independently by being a
soft-fail check an attacker can suppress. Both reasons stand alone.

**OCSP stapling as a requirement.** Privacy-clean, and it would be the right
answer if the web deployed `must-staple`. It does not, so requiring a staple
breaks most of the internet and requiring nothing changes nothing.

**Full CRL fetching.** The honest maximal version — fetch every CRL named by
every certificate. Rejected: CRLs are large, the fetch leaks the same browsing
signal as OCSP to the same parties, and the latency is per-issuer rather than
cached.

**No revocation checking at all.** Not as absurd as it sounds — it is close to
what soft-fail OCSP achieves in practice, and it is honest about it. Rejected
because pushed sets genuinely do catch the cases that matter most, which are
the ones the vendor has been told about: a compromised intermediate, a
mis-issued certificate, a CA distrust event. Those are exactly the revocations
a curated set covers well.

## Consequences

**Coverage is partial, and this must not be described as "revocation
checking" without that qualifier.** A revoked certificate absent from the set
is accepted. Anyone reading a green padlock here is being told that nothing
*known to us* has revoked this certificate — which is weaker than it sounds and
is the same guarantee every other browser actually provides.

**Someone has to curate the set**, and at this project's size that is a
sourcing problem rather than an engineering one: the update channel exists at
Phase 20, and there is no obvious source of a maintained set that is not
Chrome's or Mozilla's. This is deferred to Phase 20 with the honest note that
"consume somebody else's set" and "there is no revocation coverage" are the
realistic options, and the second is what ships until the first is arranged.

**Nothing here is on the connection path**, which is the point: revocation
costs no round trip, no third-party contact, and no per-connection latency.

**A stale set is a silent loss of coverage**, in the same way a stale PSL is a
silent loss of partitioning. It is versioned and asserted, like the PSL, so
that "the set is old" is observable rather than inferred.

## Verification

Wrong if any OCSP request is ever observed leaving the process. The Phase 3
gate already captures packets and asserts zero connections outside the
requested set, which catches this directly rather than by inspection — an OCSP
responder is by definition not a site the user asked for.

Wrong if a certificate in the CRL set is ever accepted, under any preference or
fallback. There is no soft-fail path to test, which is the property: the test
asserts rejection, and there is no configuration under which it passes.

It is *not* falsified by a revoked certificate being accepted when it is absent
from the set. That is a coverage limit stated above, not a defect — and if it
is ever treated as one, the fix is a better set, never an OCSP request.
