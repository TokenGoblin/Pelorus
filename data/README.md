# data/

Versioned, updatable data files bundled at build time and refreshed through the
Phase 20 update channel.

| File | What | Added in |
|---|---|---|
| Public Suffix List | eTLD+1 boundaries | 3 |
| HSTS preload list | Known HTTPS-only hosts | 3 |
| Root certificate store | Trust anchors, pending the Phase 3 ADR | 3 |

As of Phase 3: the PSL and the HSTS preload list are here, each with a version
file recording a SHA-256 and a fetch date, and each with a refresh script that
rewrites both together. The root store is **not** here — ADR 012 decided the
platform store is the source of trust, with webpki-roots as the bundled floor,
so there is no root-store file to version. §9 lists one; ADR 012 supersedes
that, and this note is where the discrepancy is recorded rather than left to
look like an omission.

The PSL is the one to worry about. Partitioning is keyed by eTLD+1, so a stale
PSL does not degrade privacy gently — it draws the security boundary in the
wrong place. Its version is asserted by Phase 3's gate and a stale-PSL test
fails deliberately.
