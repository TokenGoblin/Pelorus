#!/usr/bin/env bash
#
# Phase 7 gate (build-spec §9).
#
#   WPT css-flexbox subset at threshold   -> here
#   WPT css-grid subset at threshold      -> here
#   crashtests for both (ADR 031, item 5) -> here
#
# Written failing, before px-layout has a flex container in it. The phase is done
# when this passes in CI on both operating systems.
#
# ADR 031 fixes what the items mean. In short: references are paired by reading
# `rel="match"`, grid-lanes is out of scope, scripted tests are detected and
# dropped, each reftest threshold is a pinned ratchet that may not fall, flex and
# grid are graded separately so grid progress cannot hide a flex regression, and
# the crashtests are a hard pass rather than a threshold.
#
# The reftest suites do not exist yet, so this script fails on them. That is the
# point of writing it first. Each fails for its own stated reason below.

. "$(dirname "$0")/lib.sh"

if ! command -v cargo >/dev/null 2>&1; then
    fail "cargo is not on PATH"
    verdict "flex-grid"
fi

# name::test-name prefix::file that must hold them. Every prefix carries
# `layout_flex`/`layout_grid` so it cannot match another crate's tests (Phase 6
# learned that with a bare `depth_`).
SUITES="
flexbox-reftests::layout_flex_reftest_::crates/px-layout/tests/flex_reftest.rs
grid-reftests::layout_grid_reftest_::crates/px-layout/tests/grid_reftest.rs
flex-grid-crashtests::layout_flexgrid_crashtest_::crates/px-layout/tests/crashtest_flexgrid.rs
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
# The corpora are committed and cannot quietly shrink (ADR 019's pattern).
# Read from the git index: an empty directory git cannot store and `git status`
# does not report is how Phase 4 lost five commits.
# ---------------------------------------------------------------------------

check_corpus() {
    # label, directory, regex, floor
    local label="$1" dir="$2" pattern="$3" floor="$4" count
    if [ -z "$(git ls-files "$dir" | head -n 1)" ]; then
        fail "$label is not vendored at $dir"
        return
    fi
    count="$(git ls-files "$dir" | grep -c -- "$pattern" || true)"
    if [ "${count:-0}" -lt "$floor" ]; then
        fail "$label has ${count:-0} files, floor is $floor"
    else
        ok "$label has $count files in the index (floor $floor)"
    fi
}

# 27 + 35 vendored; the floor leaves room for a re-vendor to drop a few.
check_corpus "the flexbox crashtests" "tests/wpt/css-flexbox-crashtests" '\.html$' 20
check_corpus "the grid crashtests"    "tests/wpt/css-grid-crashtests"    '\.html$' 25

# The reftest subsets (ADR 031): tests, references and the few support files they
# need, 993 and 731 .html/.htm files. The pair counts are pinned separately by the
# harnesses (540 and 400); these floors catch the files themselves going missing.
# They sit a little below the counts so a re-vendor can drop a few.
check_corpus "the flexbox reftest subset" "tests/wpt/css-flexbox" '\.html\?$' 950
check_corpus "the grid reftest subset"    "tests/wpt/css-grid"    '\.html\?$' 700

# ADR 031 item 2: grid-lanes must stay out of the graded subset.
if [ -n "$(git ls-files tests/wpt/css-grid | grep 'grid-lanes' | head -n 1)" ]; then
    fail "grid-lanes files are in tests/wpt/css-grid; ADR 031 excludes them"
else
    ok "grid-lanes is not in the graded subset"
fi

verdict "flex-grid"
