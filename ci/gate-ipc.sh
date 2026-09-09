#!/usr/bin/env bash
#
# Phase 1 gate (build-spec §9).
#
#   IPC deserializers fuzz clean for 24h        -> ci/gate-fuzz-smoke.sh + campaign
#   broker rejects any message asserting        -> here
#     its own identity
#   killing the content process is recovered    -> here
#     from cleanly
#   message size limits enforced and tested     -> here
#     with a hostile length prefix
#
# The last three are Rust tests, so this script is thin on purpose: it names
# which test suites carry which gate item, so a suite that stops existing is
# visible rather than silently absent from a green run.

. "$(dirname "$0")/lib.sh"

if ! command -v cargo >/dev/null 2>&1; then
    fail "cargo is not on PATH"
    verdict "ipc"
fi

# Each entry is "gate item::test filter". The filter must match at least one
# test — a gate item whose tests were renamed away would otherwise pass by
# matching nothing, which is the failure mode this loop exists to prevent.
SUITES="
hostile-identity::hostile_identity
crash-restart::crash_restart
size-limits::size_limit
"

for entry in $SUITES; do
    item="${entry%%::*}"
    filter="${entry##*::}"
    count="$(cargo test --workspace --locked -- --list 2>/dev/null \
             | grep -c ": test$" || true)"
    matched="$(cargo test --workspace --locked -- --list "$filter" 2>/dev/null \
               | grep -c ": test$" || true)"
    if [ "${matched:-0}" -eq 0 ]; then
        fail "gate item '$item' has no tests matching '$filter' (of ${count:-0} total)"
    else
        ok "$item: $matched test(s)"
    fi
done

info "cargo test --workspace --locked"
cargo test --workspace --locked || fail "tests failed"

# §14.2 / ADR 004: the content process ships from a profile that aborts. A
# build gate that only exercises `release` would never compile it.
info "cargo build --locked --profile content-release -p px-content"
cargo build --locked --profile content-release -p px-content \
    || fail "content-release build failed"

verdict "ipc"
