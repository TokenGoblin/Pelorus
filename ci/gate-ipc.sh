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
# carry which gate item. A suite that stops existing — or stops running —
# should be visible, not quietly absent from a green run.

. "$(dirname "$0")/lib.sh"

if ! command -v cargo >/dev/null 2>&1; then
    fail "cargo is not on PATH"
    verdict "ipc"
fi

# Each entry is "gate item::test filter::file that must carry it".
#
# Three conditions, each closing a way the previous version could pass while
# testing nothing:
#
#   1. the filter matches at least one test        (it was renamed away)
#   2. the named file contains #[test] fns for it  (deleting the only tests
#      that cross a process boundary still matched similarly-named in-memory
#      tests elsewhere in the workspace)
#   3. none of the matching tests is #[ignore]d    (measured: marking every
#      gate test ignored left this reporting PASS for a phase in which none of
#      them ran)
SUITES="
hostile-identity::hostile_identity::crates/px-broker/src/lib.rs
crash-restart::crash_restart::crates/px-content/tests/crash_restart.rs
size-limits::size_limit::crates/px-ipc/src/lib.rs
"

ignored_tests="$(cargo test --workspace --locked -- --list --ignored 2>/dev/null \
                 | grep ": test$" || true)"

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

    # `grep -c "fn $filter"` alone was satisfied by a comment or a doc line, so
    # require the #[test] attribute to be near the function.
    in_home="$(grep -B2 "fn $filter" "$home" | grep -c '#\[test\]' || true)"
    if [ "${in_home:-0}" -eq 0 ]; then
        fail "gate item '$item' has no #[test] fn matching '$filter' in $home;"
        fail "  matching tests elsewhere do not substitute for that file"
        continue
    fi

    skipped="$(printf '%s\n' "$ignored_tests" | grep -c "$filter" || true)"
    if [ "${skipped:-0}" -gt 0 ]; then
        fail "gate item '$item' has $skipped #[ignore]d test(s); a gate item"
        fail "  whose tests do not run is not a gate item"
        printf '%s\n' "$ignored_tests" | grep "$filter" | sed 's/^/       /' >&2
        continue
    fi

    ok "$item: $matched test(s), $in_home in $home, none ignored"
done

info "cargo test --workspace --locked"
cargo test --workspace --locked || fail "tests failed"

# §14.2 / ADR 004: the content process ships from a profile that aborts. A
# build gate that only exercises `release` would never compile it.
info "cargo build --locked --profile content-release -p px-content"
cargo build --locked --profile content-release -p px-content \
    || fail "content-release build failed"

# The product, end to end.
#
# Nothing used to run px-browser. It exited FAILURE on a completely clean run —
# broker and content process each waiting for the other, the deadline breaking
# the deadlock by reaping a healthy child — and every check above stayed green,
# because they test libraries and this is the only artifact that puts them
# together.
info "px-browser end to end"
if cargo build --workspace --locked --quiet; then
    browser="target/debug/px-browser"
    [ -f "$browser.exe" ] && browser="$browser.exe"
    if [ ! -f "$browser" ]; then
        fail "px-browser was not built; the end-to-end check cannot run"
    elif output="$("$browser" 2>&1)"; then
        ok "px-browser: $output"
    else
        fail "px-browser exited non-zero on a clean run: $output"
    fi
else
    fail "debug build failed; cannot run px-browser"
fi

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
# be readable from px-broker at all.
IDENTITY_FIELDS='^[[:space:]]*(pub[[:space:]]+)?(sender|from|identity|channel|channel_id|pid|process|process_id|tab|tab_id|origin|partition|partition_key|caller|principal)[[:space:]]*:'

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
# Two checks, because the stronger one is structural. px-broker has no serde
# dependency of its own, and without one `#[derive(Serialize)]` cannot name the
# trait — so the property holds by construction rather than by inspection. The
# parser stays as defence in depth, and because it also catches a hand-written
# impl.
if grep -qE '^[[:space:]]*serde\b' crates/px-broker/Cargo.toml; then
    fail "px-broker depends on serde directly; nothing in the broker should be"
    fail "  able to derive Serialize, least of all ChannelId"
else
    ok "px-broker cannot name serde, so it cannot derive Serialize"
fi

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

# The fuzz targets carry gate item 1 and are compiled by exactly one CI job,
# which installs cargo-fuzz first and takes minutes. A target that stopped
# compiling would silently stop testing anything, and would look like a slow
# job rather than a broken one. Type-check them here in seconds, on the stable
# pin — fuzz/ overrides to nightly, and nothing about `cargo check` needs it.
if [ -d fuzz ] && command -v rustup >/dev/null 2>&1; then
    stable_tc="$(grep -oE 'channel = "[^"]+"' rust-toolchain.toml | head -1 | cut -d'"' -f2 || true)"
    if [ -z "$stable_tc" ]; then
        fail "cannot read the pinned toolchain from rust-toolchain.toml"
    elif (cd fuzz && cargo "+$stable_tc" check --all-targets --quiet); then
        ok "fuzz targets compile"
    else
        fail "a fuzz target does not compile; gate item 1 tests nothing"
    fi
fi

verdict "ipc"
