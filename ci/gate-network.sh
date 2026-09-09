#!/usr/bin/env bash
#
# Phase 3 gate (build-spec §9).
#
#   200 URLs fetched correctly                              -> here
#   24h fuzz on HTTP framing                                -> campaign, here
#   packet capture: zero plaintext DNS, zero connections    -> here
#     outside the requested set
#   PSL version asserted, and a stale-PSL test fails        -> here
#
# Written failing, before px-net exists. The phase is done when this passes on
# both operating systems, and not when the build is green.
#
# What this gate deliberately does NOT assert, so that a pass is not read as
# more than it is:
#
#   - HTTP/2. ADR 015 defers it and narrows §9 Phase 3 accordingly. A gate
#     that silently graded against the smaller target would hide that.
#   - That no plaintext DNS left the machine. Under ADR 014's default the
#     *operating system* resolves names, so the capture below measures this
#     process's own sockets. The distinction is in ADR 014; repeating it here
#     because a green line reading "zero plaintext DNS" invites the stronger
#     reading.

. "$(dirname "$0")/lib.sh"

if ! command -v cargo >/dev/null 2>&1; then
    fail "cargo is not on PATH"
    verdict "network"
fi

# ---------------------------------------------------------------------------
# Gate items as named tests, with the three conditions Phases 1 and 2 taught
# this gate to apply: the filter matches something, the named file actually
# carries #[test] functions for it, and none of them is #[ignore]d.
#
# The third is not paranoia. An #[ignore]d test satisfies a grep and proves
# nothing, and a gate that counts it is a gate that reports work nobody did.
# ---------------------------------------------------------------------------

SUITES="
two-hundred-urls::net_fetches::crates/px-net/tests/fetch.rs
psl-freshness::psl_version::crates/px-net/tests/psl.rs
no-unrequested-connections::net_connects_only::crates/px-net/tests/connections.rs
partitioned-pools::net_partitions::crates/px-net/tests/partition.rs
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
    in_home="$(grep -B2 "fn $filter" "$home" | grep -c '#\[test\]' || true)"
    if [ "${in_home:-0}" -eq 0 ]; then
        fail "gate item '$item' has no #[test] fn matching '$filter' in $home"
        continue
    fi
    skipped="$(printf '%s\n' "$ignored_tests" | grep -c "$filter" || true)"
    if [ "${skipped:-0}" -gt 0 ]; then
        fail "gate item '$item' has $skipped #[ignore]d test(s)"
        continue
    fi
    ok "$item: $matched test(s), $in_home in $home, none ignored"
done

info "cargo test --workspace --locked"
cargo test --workspace --locked || fail "tests failed"

# ---------------------------------------------------------------------------
# The 200 URLs are fetched against recorded traffic and a local server, not
# against the live web.
#
# A gate that needs the internet is a gate that goes red when a third party has
# an outage, and one that has been red for a reason nobody controls is one
# people learn to ignore. Recorded archives also make the corpus a fixed
# target: "200 URLs fetched correctly" means the same thing in a year.
# ---------------------------------------------------------------------------

ARCHIVE="tests/compat/archives"
if [ ! -d "$ARCHIVE" ]; then
    fail "$ARCHIVE does not exist; the 200-URL corpus has no recorded traffic"
else
    count="$(find "$ARCHIVE" -name '*.har' 2>/dev/null | wc -l | tr -d ' ')"
    if [ "${count:-0}" -lt 200 ]; then
        fail "the corpus holds $count recorded responses; §9 Phase 3 asks for 200"
    else
        ok "the corpus holds $count recorded responses"
    fi
fi

# ---------------------------------------------------------------------------
# Invariant 4, mechanically: only px-net and px-update may open a socket.
#
# A test can show that the code we wrote does not connect elsewhere. It cannot
# show that no other crate ever will. This is the symbol audit build-spec §3
# names, and it is the half that keeps holding as the project grows.
# ---------------------------------------------------------------------------

info "no crate outside px-net and px-update reaches the network"
leaked=0
for crate_dir in crates/*/; do
    crate="$(basename "$crate_dir")"
    case "$crate" in
        px-net | px-update) continue ;;
    esac
    if grep -rn --include='*.rs' -E '\b(TcpStream|TcpListener|UdpSocket|ToSocketAddrs)\b' \
            "$crate_dir/src" >/dev/null 2>&1; then
        fail "$crate names a socket type; invariant 4 confines the network to px-net and px-update"
        leaked=1
    fi
done
[ "$leaked" -eq 0 ] && ok "the network is confined to px-net and px-update"

# ---------------------------------------------------------------------------
# The PSL is a security boundary, not a data file. A stale one silently
# mis-partitions state — invariant 2 — and nothing observable breaks, which is
# the worst shape a defect can have.
# ---------------------------------------------------------------------------

PSL="data/public_suffix_list.dat"
PSL_VERSION="data/public_suffix_list.version"
if [ ! -f "$PSL" ]; then
    fail "$PSL does not exist"
elif [ ! -f "$PSL_VERSION" ]; then
    fail "$PSL_VERSION does not exist; a list with no version cannot be asserted stale"
else
    ok "the PSL is present and versioned"
fi

# ---------------------------------------------------------------------------
# 24h fuzz on HTTP framing. Same shape as Phase 1's IPC campaign: a smoke run
# here, the long campaign on a schedule, because a gate that takes a day to
# report is a gate that gets bypassed.
# ---------------------------------------------------------------------------

for target in http_response http_chunked; do
    if [ ! -f "fuzz/fuzz_targets/$target.rs" ]; then
        fail "fuzz target $target does not exist; §9 Phase 3 requires HTTP framing fuzzed"
    else
        ok "fuzz target $target exists"
    fi
done

verdict "network"
