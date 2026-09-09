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

# Each entry is "gate item::test filter::file that must carry it".
#
# The filter must match at least one test AND the named file must contain a
# test matching it. Filter alone was not enough: an adversarial review deleted
# crates/px-content/tests/crash_restart.rs — the only tests in the phase that
# cross a real process boundary — and both gate items still matched, because
# similarly-named in-memory tests exist elsewhere in the workspace. The gate
# reported `ok crash-restart: 1 test(s)` for a phase whose process boundary had
# no tests at all.
SUITES="
hostile-identity::hostile_identity::crates/px-broker/src/lib.rs
crash-restart::crash_restart::crates/px-content/tests/crash_restart.rs
size-limits::size_limit::crates/px-ipc/src/lib.rs
"

for entry in $SUITES; do
    item="${entry%%::*}"
    rest="${entry#*::}"
    filter="${rest%%::*}"
    home="${rest##*::}"

    matched="$(cargo test --workspace --locked -- --list "$filter" 2>/dev/null \
               | grep -c ": test$" || true)"
    if [ "${matched:-0}" -eq 0 ]; then
        fail "gate item '$item' has no tests matching '$filter'"
        continue
    fi

    if [ ! -f "$home" ]; then
        fail "gate item '$item' expects tests in $home, which does not exist"
        continue
    fi
    in_home="$(grep -c "fn $filter" "$home" || true)"
    if [ "${in_home:-0}" -eq 0 ]; then
        fail "gate item '$item' has no '$filter' test in $home;"
        fail "  matching tests elsewhere do not substitute for that file"
        continue
    fi

    ok "$item: $matched test(s), $in_home in $home"
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
#
# `(pub )?` is load-bearing, and its absence made this check close to inverted.
# The regex used to anchor the field name straight after leading whitespace, so
# `pub sender: ChannelId` did not match — and a struct field must be `pub` to
# be readable from px-broker at all. It caught exactly the declarations that
# are unusable across a crate boundary and missed every one that is.
IDENTITY_FIELDS='^[[:space:]]*(pub[[:space:]]+)?(sender|from|identity|channel|channel_id|pid|process|process_id|tab|tab_id|origin|partition|partition_key|caller|principal)[[:space:]]*:'

# Every source file in the crate, not one file: a new wire type in
# crates/px-ipc/src/frame.rs would never have been scanned.
wire_sources="$(git ls-files 'crates/px-ipc/src/*.rs' || true)"
if [ -z "$wire_sources" ]; then
    fail "no px-ipc sources found to scan; this check cannot pass by finding nothing"
else
    leaks=0
    for src in $wire_sources; do
        if grep -nE "$IDENTITY_FIELDS" "$src" >/dev/null 2>&1; then
            fail "$src declares a field naming the sender; authority comes from"
            fail "  the channel, never from the message (invariant 9)"
            grep -nE "$IDENTITY_FIELDS" "$src" | head -n 5 | sed 's/^/       /' >&2
            leaks=1
        fi
    done
    [ "$leaks" -eq 0 ] && ok "no message type names its own sender"
fi

# ChannelId is the broker's answer to "who sent this". If it were ever
# serialisable it could travel inside a message, and then it would be a claim
# rather than an observation.
#
# Parsed rather than grepped in a fixed window. The previous version required
# `derive(` and `Serialize` to appear on the SAME line, which a multi-line
# derive silently defeats — and a multi-line derive is exactly what rustfmt
# emits once the list is long, in a project that runs `cargo fmt --check`.
channel_id_home="$(git ls-files 'crates/px-broker/src/*.rs' \
                   | xargs grep -ln 'struct ChannelId' 2>/dev/null || true)"
if [ -z "$channel_id_home" ]; then
    fail "no 'struct ChannelId' found under crates/px-broker/src/; this check"
    fail "  cannot pass by not finding what it is looking for"
elif [ -z "$PY_BIN" ]; then
    fail "no python >= 3.11 on PATH; cannot inspect ChannelId's derives"
elif "$PY_BIN" ci/check_not_serializable.py ChannelId $channel_id_home; then
    ok "ChannelId cannot be serialised onto the wire"
else
    fail "ChannelId is serialisable; it must never be able to reach the wire"
fi

verdict "ipc"
