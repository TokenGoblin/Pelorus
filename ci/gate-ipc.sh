#!/usr/bin/env bash
#
# Phase 1 gate (build-spec §9).
#
#   IPC deserializers fuzz clean for 24h        -> ci/gate-fuzz-smoke.sh
#                                                  + the scheduled campaign
#   broker rejects any message asserting        -> here
#     its own identity
#   killing the content process is recovered    -> here
#     from cleanly
#   message size limits enforced and tested     -> here
#     with a hostile length prefix
#
# Three of the four are Rust tests, so most of this script names which suites
# carry which gate item. A suite that stops existing should be visible, not
# quietly absent from a green run.

. "$(dirname "$0")/lib.sh"

if ! command -v cargo >/dev/null 2>&1; then
    fail "cargo is not on PATH"
    verdict "ipc"
fi

# Each entry is "gate item::test filter". The filter must match at least one
# test — a gate item whose tests were renamed away would otherwise pass by
# matching nothing, which is the quietest way for a gate to stop meaning
# anything.
SUITES="
hostile-identity::hostile_identity
crash-restart::crash_restart
size-limits::size_limit
"

for entry in $SUITES; do
    item="${entry%%::*}"
    filter="${entry##*::}"
    matched="$(cargo test --workspace --locked -- --list "$filter" 2>/dev/null \
               | grep -c ": test$" || true)"
    if [ "${matched:-0}" -eq 0 ]; then
        fail "gate item '$item' has no tests matching '$filter'"
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

# Invariant 9, checked in source as well as in tests.
#
# The tests prove the broker refuses a request naming somebody else's resource.
# They cannot prove that no FUTURE message type carries an identity field —
# and a message that said who it was from would be believed by whatever new
# code was written to read it. So the field names are forbidden outright in the
# wire vocabulary.
#
# Naming a RESOURCE is fine, and is how the capability model works: FrameHost
# carries the FrameId it asks about, and the broker decides whether the channel
# it arrived on is entitled to it. What is forbidden is a message describing
# its own sender.
IDENTITY_FIELDS='^[[:space:]]*(sender|from|identity|channel|channel_id|pid|process|process_id|tab|tab_id|origin|partition|partition_key|caller|principal)[[:space:]]*:'
MESSAGE_SRC="crates/px-ipc/src/message.rs"
BROKER_SRC="crates/px-broker/src/lib.rs"

if [ ! -f "$MESSAGE_SRC" ]; then
    fail "$MESSAGE_SRC does not exist"
elif grep -nE "$IDENTITY_FIELDS" "$MESSAGE_SRC" >/dev/null 2>&1; then
    fail "$MESSAGE_SRC declares a field naming the sender; authority comes from"
    fail "  the channel, never from the message (invariant 9)"
    grep -nE "$IDENTITY_FIELDS" "$MESSAGE_SRC" | head -n 5 | sed 's/^/       /' >&2
else
    ok "no message type names its own sender"
fi

# ChannelId is the broker's answer to "who sent this". If it were ever
# serialisable it could travel inside a message, and then it would be a claim
# rather than an observation.
# Search the whole crate, not one file, and prove the type was found before
# drawing a conclusion from its absence. The `derive(` filter is not decoration:
# without it the check matches the word "Serialize" in the type's own doc
# comment, which explains why it must never be serialisable. `grep -B3 X | grep -q Y` reports
# success when X matches nothing at all, so renaming or moving the type would
# have turned this into a check that passed having inspected nothing.
channel_id_decl="$(grep -rn 'struct ChannelId' crates/px-broker/src/ 2>/dev/null || true)"
if [ -z "$channel_id_decl" ]; then
    fail "no 'struct ChannelId' found under crates/px-broker/src/; this check"
    fail "  cannot pass by not finding what it is looking for"
elif grep -rn -B6 'struct ChannelId' crates/px-broker/src/ | grep 'derive(' | grep -q 'Serialize'     || grep -rn 'impl .*Serialize for ChannelId' crates/px-broker/src/ >/dev/null 2>&1; then
    fail "ChannelId is serialisable; it must never be able to reach the wire"
else
    ok "ChannelId cannot be serialised onto the wire"
fi

verdict "ipc"
