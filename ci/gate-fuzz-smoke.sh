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

for target in $targets; do
    info "fuzzing $target for ${SECONDS_PER_TARGET}s"
    # -max_len is not optional. libFuzzer's default is 4096 bytes, which puts
    # the entire large-payload path — the only code in recv that allocates,
    # and the MAX_MESSAGE_BYTES boundary this gate specifically names — out
    # of reach. A 24-hour campaign ran without it and reported clean while
    # never testing what it was meant to test.
    #
    # -rss_limit_mb bounds the fuzzer itself: an OOM in the harness is not a
    # finding about the code under test.
    if (cd fuzz && cargo fuzz run "$target" -- \
            -max_total_time="$SECONDS_PER_TARGET" \
            -max_len="${FUZZ_MAX_LEN:-1100000}" \
            -rss_limit_mb=2048 \
            -print_final_stats=1); then
        ok "$target"
    else
        fail "$target found a crash; the reproducer is under fuzz/artifacts/$target/"
    fi
done

verdict "fuzz-smoke"
