# 030 — Trust anchors on Windows: ask the platform, do not enumerate it

*Status: **proposed**, awaiting a decision. Blocks nothing today and breaks real
sites today.*

*Amends ADR 012's implementation, not its decision.*

## Context

ADR 012 decided that **the platform store is the source of trust** and the
bundled Mozilla store is a floor for when it cannot be read. That decision is
right and this ADR does not reopen it. The implementation does not achieve it.

`crates/px-sandbox/src/roots.rs` reads the Windows `ROOT` store with
`CertOpenSystemStoreW` and `CertEnumCertificatesInStore`, and hands the DER blobs
to rustls as its anchor set. **Enumerating the store is not the same as asking the
platform**, because Windows populates that store *lazily*. The Microsoft Trusted
Root Program is not all present on disk; the OS fetches a root the first time a
chain needs one, and schannel is what triggers the fetch. Enumeration sees a
snapshot of whatever this machine has needed before.

## The reproduction

Found in Phase 6 by pointing `px-fetch` at sixteen major sites. Two independent
instances, `lite.cnn.com` and `www.intel.com`. The second was reproduced
deliberately:

```
roots in Cert:\LocalMachine\Root      46
px-fetch https://www.intel.com        UnknownIssuer
curl     https://www.intel.com        301, ssl_verify_result=0
roots in Cert:\LocalMachine\Root      47
newly installed                       CN=Sectigo Public Server Authentication
                                      Root E46, O=Sectigo Limited, C=GB
px-fetch https://www.intel.com        TLS handshake completes
```

intel.com's chain is `*.intel.com` ← `Sectigo Public Server Authentication CA OV
E36`. The root that arrived is that intermediate's. Nothing was rebuilt between
the failing and the succeeding run, and `curl` — which goes through schannel — is
what installed it.

`root_store()` reported `Platform` with 46 anchors, all 46 accepted by rustls,
none rejected. **The store was not broken; it was incomplete, and nothing
distinguishes those two from inside.** `ImplausiblyFew` will not catch this: 46 is
above the threshold and a machine can be one root short of any site.

## What this costs

A trust decision that depends on the machine's browsing history rather than on
policy. A fresh install rejects sites a browser accepts, the set varies per
machine, and the failure presents as a site problem rather than a client one.

Phase 23's gate is "compat suite green on all forty sites; thirty consecutive days
of self-hosted use with no fallback". This makes that suite flaky in a way that
looks like the sites, on a schedule nobody controls.

## The options

### A. Ask Windows to build and verify the chain

Implement `rustls::client::danger::ServerCertVerifier` delegating to
`CertGetCertificateChain` and `CertVerifyCertificateChainPolicy`
(`CERT_CHAIN_POLICY_SSL`), with the intermediates the server sent supplied as an
extra store and the server name in `SSL_EXTRA_CERT_CHAIN_POLICY_PARA`.

- **Achieves ADR 012 exactly**: the platform decides, an administrator's distrust
  decisions are honoured, and root auto-update comes for free. Revocation checking
  becomes available later as a flag rather than a rewrite.
- **Lives where ADR 008 put it.** `roots.rs`'s own module doc says it exists
  "precisely so that `CertOpenSystemStoreW` does not become a second crate with an
  `unsafe` exception", and this is the same API family in the same crate.
- **Costs** roughly 250 lines of `unsafe` Win32 with a `// SAFETY:` comment per
  block, plus a verifier impl in `px-net`. Linux keeps reading `/etc/ssl/certs`,
  which has no lazy-population problem, so this is a Windows-only path and the two
  platforms stop sharing an implementation.
- **The risk is the point.** `rustls::client::danger` is named that because the
  trait is how verification gets switched off by accident. A wrong
  `verify_tls12_signature` or a `TrustStatus` check that looks at the wrong bit is
  a silent TLS bypass, which is the worst defect this project could ship. It wants
  the adversarial review the sandbox got, and a test that proves a bad certificate
  is still rejected — not only that a good one is accepted.

### B. Take a dependency on `rustls-platform-verifier`

The crate that exists for exactly this, maintained alongside rustls, doing A on
every platform.

- **Costs** an ADR of its own under the no-new-dependency rule, plus crates and
  `cargo-vet` exemptions against a budget Phase 5 already moved from 1 to 77.
- **Buys** code reviewed by more people than will ever read ours, and Linux and
  macOS handled at the same time.
- Worth weighing honestly against A rather than dismissed: the argument for writing
  it ourselves is weaker here than anywhere else in this project, because the
  failure mode is silent and the crate's whole purpose is this bug.

### C. Use the bundled Mozilla floor as the source on Windows

- **Three lines**, deterministic, and no `unsafe`.
- **Gives up ADR 012's central property**: an administrator who removes a root, or
  an enterprise that adds an MDM one, is ignored. That is the thing ADR 012 chose
  the platform store *for*.
- Listed because it is the honest cheap option, not because it is recommended.

## Recommendation

**A, with B as the serious alternative**, and C only if neither is affordable.

A keeps every property ADR 012 argued for and adds nothing to the dependency
budget. B is the same behaviour with the review burden moved to people who have
already done it. The case against A is not that it is hard but that it is the one
piece of code in this project where being clever and being wrong look identical
from the outside.

Whichever is chosen, two things are not negotiable:

1. **A negative test.** An expired certificate, a name mismatch and an untrusted
   root must each be *rejected*, asserted in CI on Windows. Verification code that
   has only ever been seen to accept has not been tested.
2. **Do not union the platform store with the bundled one.** `tls.rs` explains at
   length why those are never merged and this finding does not touch that
   reasoning.

## Not decided here

Whether Linux should move to the same shape. `/etc/ssl/certs` is a directory read
with no lazy population, so it has no version of this bug, and keeping one
platform on a simple file read is worth something.

## Verification

Wrong if the reproduction does not hold on a second machine — the experiment above
is one machine, and "Windows populates roots lazily" is documented behaviour but
the count going 46 → 47 with exactly the needed root is the only direct evidence
here.

Wrong in the expensive direction if A ships and a negative test is not written,
because then the defect this ADR fixes is replaced by one nobody can see.
