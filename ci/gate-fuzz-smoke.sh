#!/usr/bin/env bash
#
# Fuzz smoke, on every push (ADR 006).
#
# 60 seconds per target against the committed corpus. This is not the 24h
# campaign Phase 1's gate requires — that runs on a schedule and reports; see
# .github/workflows/fuzz-campaign.yml. What this catches is a regression of a
# crash the campaign already found, in the time a reviewer will actually wait.
#
# Every crash the campaign finds gets its input committed to the corpus, which
# is what makes 60 seconds meaningful rather than decorative.

. "$(dirname "$0")/lib.sh"

SECONDS_PER_TARGET="${FUZZ_SECONDS:-60}"

if [ ! -d fuzz ]; then
    fail "fuzz/ does not exist"
    verdict "fuzz-smoke"
fi

if ! command -v cargo-fuzz >/dev/null 2>&1; then
    fail "cargo-fuzz is not installed (cargo install --locked cargo-fuzz)"
    verdict "fuzz-smoke"
fi

# ADR 006: the nightly is fenced to fuzz/ by a directory-scoped
# rust-toolchain.toml. Assert the fence rather than trusting it — the whole
# point of pinning stable is that nothing outside this directory drifts.
# `rustup show active-toolchain` prints installation progress and warnings on
# the first call for a toolchain that is not yet downloaded, so take the last
# line rather than the first.
# `|| true` on each: ci/lib.sh sets `set -euo pipefail`, so an assignment whose
# command substitution fails aborts the script at that line and the diagnostics
# below never run. A gate that dies with a bare non-zero exit and no message is
# a gate somebody has to reverse-engineer at midnight.
root_tc="$(rustup show active-toolchain 2>/dev/null | tail -1 | cut -d' ' -f1 || true)"
fuzz_tc="$(cd fuzz && rustup show active-toolchain 2>/dev/null | tail -1 | cut -d' ' -f1 || true)"
info "toolchain at repository root: $root_tc"
info "toolchain inside fuzz/:      $fuzz_tc"
case "$root_tc" in
    nightly*) fail "the repository root resolves to a nightly toolchain; ADR 006 fences nightly to fuzz/" ;;
esac
case "$fuzz_tc" in
    nightly*) ok "nightly is scoped to fuzz/" ;;
    *) fail "fuzz/ does not resolve to a nightly toolchain; cargo-fuzz needs one" ;;
esac

# FUZZ_TARGET restricts the run to one target. The campaign uses it to shard
# across jobs, because a GitHub-hosted job is capped at six hours and a 24-hour
# campaign therefore cannot be one job.
targets="$(cd fuzz && cargo fuzz list 2>/dev/null || true)"

# FUZZ_TARGET restricts the run to one target. The campaign uses it to shard
# across jobs, because a GitHub-hosted job is capped at six hours and a 24-hour
# campaign therefore cannot be one job.
#
# Matched with an explicit loop. The obvious `case " $targets " in *" $t "*`
# does NOT work: `cargo fuzz list` is newline-separated, so no name is ever
# surrounded by spaces and every shard fails with "not a known target". That
# shipped, and the first real campaign died in twelve seconds instead of
# running for four hours.
if [ -n "${FUZZ_TARGET:-}" ]; then
    found=""
    for candidate in $targets; do
        [ "$candidate" = "$FUZZ_TARGET" ] && found="$candidate"
    done
    if [ -z "$found" ]; then
        fail "FUZZ_TARGET=$FUZZ_TARGET is not a known target; have: $(echo $targets)"
        verdict "fuzz-smoke"
    fi
    targets="$found"
fi
if [ -z "$targets" ]; then
    fail "no fuzz targets defined"
    verdict "fuzz-smoke"
fi

# The largest input a target should be given, per target.
#
# -max_len is not optional. libFuzzer's default is 4096 bytes, which puts the
# entire large-payload path — the only code in recv that allocates, and the
# MAX_MESSAGE_BYTES boundary this gate names — out of reach. A 24-hour campaign
# ran without it and reported clean while never testing what it was meant to.
#
# And one value does not fit every target, which the Phase 3 campaign showed.
# 1,100,000 straddles px-ipc's MAX_MESSAGE_BYTES of 1,048,576 exactly as
# intended, and comes nowhere near px-net's MAX_BODY_BYTES of 32 MiB or
# MAX_CHUNK_BYTES of 8 MiB — so the HTTP targets reported clean for inputs up
# to 1.1 MB while their own size boundaries went unfuzzed.
#
# Each value straddles the limit its target actually has. The HTTP figure is
# deliberately just past MAX_CHUNK_BYTES rather than past MAX_BODY_BYTES: a
# 32 MiB ceiling would spend the whole budget generating enormous inputs
# instead of exploring structure, and the chunk bound is the one a declared
# length reaches directly rather than by accumulation.
#
# The DOM targets go the other way, and for a different reason. Their harnesses
# assert the whole tree's invariants after *every* operation, and one byte is
# roughly one operation, so cost grows with the square of the input. Measured,
# release: 1 KB is 0.4 ms, 4 KB is 2.9 ms, 16 KB is 20 ms, 64 KB is 170 ms. At
# the 1,100,000 default a single execution would take minutes and the campaign
# would explore almost nothing.
#
# 4 KB is about two thousand operations — enough to build real trees, churn
# slots, and reach the forced generation-exhaustion path — at roughly 350
# executions per second. The check-everything-every-step design is what makes
# these targets precise, and this is what it costs.
max_len_for() {
    case "$1" in
        http_response | http_chunked) echo 8500000 ;;
        dom_stale_handle | dom_mutation) echo 4096 ;;
        # The parser goes the other way again: its input is HTML, and the
        # committed corpus holds a 1.1 MB seed -- 100,000 levels of nesting
        # opened and closed, which §4.4 asks for by name. A smaller -max_len
        # would truncate it and quietly drop the coverage the seed exists for.
        #
        # Affordable because `parse` abandons a nesting bomb after eight
        # refusals: measured on release, the 1.1 MB nesting seed costs 75 ms
        # and a 1 MB shallow document 104 ms, against 6 ms at 64 KB. Large
        # inputs are the cheap ones here, which is the opposite of the two
        # arena targets above.
        dom_parse) echo 1200000 ;;
        *) echo 1100000 ;;
    esac
}

for target in $targets; do
    target_max_len="${FUZZ_MAX_LEN:-$(max_len_for "$target")}"
    info "fuzzing $target for ${SECONDS_PER_TARGET}s (-max_len=$target_max_len)"
    # -rss_limit_mb bounds the fuzzer itself: an OOM in the harness is not a
    # finding about the code under test.
    if (cd fuzz && cargo fuzz run "$target" -- \
            -max_total_time="$SECONDS_PER_TARGET" \
            -max_len="$target_max_len" \
            -rss_limit_mb=2048 \
            -print_final_stats=1); then
        ok "$target"
    else
        # A non-zero exit is not the same as a crash, and saying so matters:
        # libFuzzer writes a reproducer when it crashes the target and writes
        # nothing when it could not start. Reporting the second as the first
        # sends somebody hunting for a file that does not exist.
        #
        # Both happen. On a Windows machine without the MSVC ASAN runtime every
        # target exits 0xc0000135 (STATUS_DLL_NOT_FOUND) before executing a
        # single input, and this script reported nine crashes and nine
        # reproducer paths, all of them empty directories.
        found="$(find "fuzz/artifacts/$target" -type f 2>/dev/null | head -1)"
        if [ -n "$found" ]; then
            fail "$target found a crash; the reproducer is $found"
        else
            # One `fail`, then `info` for the explanation: `fail` increments
            # the count the verdict reports, and four lines about one problem
            # would read as four problems.
            fail "$target did not run to completion and wrote no reproducer"
            info "so this is a harness or toolchain failure, not a finding."
            info "On Windows without the MSVC ASAN runtime every target exits"
            info "0xc0000135 here; ADR 011 covers the sanitizer toolchain."
        fi
    fi
done

verdict "fuzz-smoke"
