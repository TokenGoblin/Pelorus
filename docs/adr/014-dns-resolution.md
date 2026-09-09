# 014 — The system resolver by default, DoH by choice, no bundled provider

- **Status:** accepted
- **Date:** 2026-09-09
- **Phase:** 3
- **Invariants touched:** none. Constrained by 2 (partitioning), 4 (what the
  network is for) and 5 (silence by default).
- **Settles:** build-spec §13 open decision 5, due this phase.

## Context

Every page load starts with a name lookup, and whoever answers it learns every
site the user visits, in order, with timestamps. There is no way to load a page
without telling *somebody*; the only question is who, and whether the user
chose them.

§9 Phase 3 fixes the shape of the answer — a first-run free choice among the
system resolver, a named provider, and custom — and gives the reason a default
provider is not acceptable:

> A hardcoded default provider transfers the tracking rather than eliminating
> it.

That is the whole argument against the industry's usual move. Shipping with
DoH pointed at one company by default encrypts the lookup, hides it from the
ISP, and hands the complete browsing history of every user to a party none of
them selected. It is a privacy *improvement* in the threat model where the ISP
is the adversary and a privacy *regression* in the one where an aggregator is,
and picking which of those the user faces is not our decision to make silently.

The remaining tension is invariant 5: silence by default, no onboarding. A
first-run screen that blocks until the user picks a resolver is an onboarding
step, and this project has committed to not having those.

## Decision

**The system resolver is used by default. It is preselected, not forced, and
nothing blocks on the choice.**

The alternatives are offered in settings, and the first-run state is a
preselected default rather than a modal: the browser starts and works without
the user answering anything, which is what invariant 5 requires.

Three options, and they are the three §9 names:

1. **System resolver** — default. Whatever the OS is configured to use.
2. **A named DoH provider** — a short curated list, each shown with its
   jurisdiction and what its published policy says it retains. Inclusion is not
   endorsement, and the list says so.
3. **Custom** — the user supplies a DoH URL. No validation beyond it being a
   well-formed HTTPS endpoint; a user who types their own resolver has already
   decided.

**The reasoning for the default, stated because it is not the obvious pick:**
the system resolver adds no party the user is not already trusting. Their DNS
already goes there — today, before this browser existed, for every other
program on the machine. Choosing it changes nothing about who sees their
lookups, which makes it the only option that cannot be a downgrade for
somebody. Every other default improves matters for users on a hostile ISP and
worsens them for users on a trusted network, and we cannot tell which a given
user is on.

It is not the most private option and this ADR does not claim it is. It is the
one that does not make the choice on the user's behalf.

**No provider is compiled in as a default endpoint.** The curated list is
data, in `data/`, updated through the Phase 20 channel — so adding or removing
a provider is not a code change, and a provider that turns out to be
untrustworthy can be dropped without a release.

**The DNS cache is keyed by partition key** (invariant 2), whichever resolver
is selected. There is no unpartitioned cache path, and no "shared DNS cache
because it is just a name" exception — a shared cache is a cross-site oracle
whichever resolver filled it.

## Alternatives rejected

**Preselect a named encrypted provider.** Encrypts DNS out of the box and hides
it from the ISP, which is a real gain for users on hostile networks. Rejected
by §9's own reasoning: it hands every lookup to one party the user never chose,
which transfers the tracking rather than eliminating it. It also makes this
project a source of traffic to a company we have no relationship with, on
behalf of users who did not ask.

**Force a choice at first run.** The most honest option, and genuinely
tempting: it guarantees an informed decision rather than an inherited one.
Rejected because it is an onboarding step, and invariant 5 rules those out
without qualification. A user who does not understand the question — most
users, and this is not a criticism — would be forced to answer it anyway, and
would pick whatever is first.

**DoH with automatic upgrade**, probing whether the system resolver supports
DoH and using it if so. Rejected because the probe is itself traffic to
something the user did not request, which invariant 4 does not permit, and
because "automatic" here means the resolver changes underneath the user based
on network conditions they cannot see.

**Do our own recursive resolution.** No third party at all, which is the
maximally private answer. Rejected as out of scope by a wide margin: it means
implementing a recursive resolver, talking to root servers directly, and
handling DNSSEC — and it is *more* identifying, not less, because the
authoritative servers then see the user's address directly instead of a
resolver's.

## Consequences

**The default leaks DNS to the ISP in plaintext, and that is a real cost.**
Anyone who expected a privacy-focused browser to encrypt DNS out of the box
will find this surprising, and the answer is in the reasoning above rather than
in an apology: encrypting it would mean choosing their new observer for them.
The setting is one screen away and the list explains the tradeoff.

**A curated provider list is an editorial responsibility**, and a small
project is not well placed to audit a resolver's retention claims. The list
therefore reports what each provider *publishes* and says that it is a claim
rather than a verified fact. Anything stronger would be a guarantee this
project cannot make.

**DoH goes through `px-net`'s ordinary HTTPS path**, so it is subject to the
same root store (ADR 012) and the same revocation set (ADR 013) as any other
connection, and it is partitioned like any other. There is no side channel that
skips the stack because it is "just DNS".

**The Phase 3 gate asserts zero plaintext DNS**, and under the default
resolver, the OS makes those queries and the browser makes none. The gate
therefore measures the browser's own sockets rather than the machine's, and
this ADR is where that distinction is written down before someone reads a green
gate as "no plaintext DNS occurred anywhere".

## Verification

Wrong if the browser ever contacts a resolver the user did not select — a
hardcoded fallback endpoint reached when the selected one fails would be the
likely form. The gate's packet capture asserts zero connections outside the
requested set, which catches it.

Wrong if the DNS cache is ever shared across partitions, which would make it a
cross-site oracle. Tested directly rather than inferred: the same name resolved
under two partition keys must not produce a cache hit across them.

It is *not* falsified by the default being less private than DoH. That is the
stated tradeoff, made deliberately, and reversing it would need this ADR
superseded with an argument about who the user's adversary is.
