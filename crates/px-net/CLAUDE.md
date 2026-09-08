# px-net

Rustls, DoH, HTTP/1.1 and HTTP/2, partitioned pools. Implemented in phase 3.

Full specification: `docs/build-spec.md`. Root working agreement:
`/CLAUDE.md` — it applies here too; this file only adds what is local.

## Local invariants

- One of only two crates permitted to touch the network (px-update is the
  other). CI enforces by symbol audit.
- Connection pools, DNS cache and TLS session cache are keyed by partition
  key. There is no unpartitioned path (invariant 2).
- No unwrap/expect/panic/indexing: this crate parses hostile bytes.
  `[lints] workspace = true` denies them; do not waive them locally.
- No OCSP. It leaks browsing to the CA and violates invariant 4 (§9 Phase 3).
