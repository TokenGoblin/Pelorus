# fuzz/

`cargo-fuzz` targets for the IPC deserializers (build-spec §4.5, ADR 006).

Outside the workspace, on a different toolchain, and never shipped.

## Running

```
cd fuzz
cargo fuzz list
cargo fuzz run frame_request -- -max_total_time=60
```

The nightly in `rust-toolchain.toml` applies only inside this directory. If
`rustup show active-toolchain` reports nightly at the repository root, the
fence has leaked and `ci/gate-fuzz-smoke.sh` fails.

## Two cadences

| | When | Duration |
|---|---|---|
| Smoke | Every push | 60s per target |
| Campaign | Weekly, or on demand | 24h |

Phase 1's gate is satisfied by one recorded campaign, not by every push doing
the impossible.

## When a crash is found

1. The reproducer lands in `artifacts/<target>/`.
2. Fix the defect.
3. **Commit the reproducing input to `corpus/<target>/`.** This is the step
   that makes the 60-second smoke job worth running: without it, the next
   regression of the same bug takes another 24 hours to find.

`artifacts/` is gitignored; `corpus/` is not.
