# data/

Versioned, updatable data files bundled at build time and refreshed through the
Phase 20 update channel.

| File | What | Added in |
|---|---|---|
| Public Suffix List | eTLD+1 boundaries | 3 |
| HSTS preload list | Known HTTPS-only hosts | 3 |
| Root certificate store | Trust anchors, pending the Phase 3 ADR | 3 |

Empty until Phase 3.

The PSL is the one to worry about. Partitioning is keyed by eTLD+1, so a stale
PSL does not degrade privacy gently — it draws the security boundary in the
wrong place. Its version is asserted by Phase 3's gate and a stale-PSL test
fails deliberately.
