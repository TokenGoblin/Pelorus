#!/usr/bin/env bash
#
# Miri over px-dom (build-spec §4.5, ADR 022).
#
# §4.5: "Miri on px-dom, px-ipc, px-store unit tests (pure-Rust paths only —
# Miri cannot run the FFI ones, and that limitation is documented rather than
# papered over)." This covers one of the three; px-ipc and px-store are in
# docs/backlog.md.
#
# px-dom is #![forbid(unsafe_code)], so Miri finds nothing in its own code.
# What it exercises is the closure ADR 017 brought in — parking_lot 539 unsafe
# tokens, tendril 129, smallvec 75, string_cache 19 — which the unsafe audit
# counts and nothing else runs.
#
# ADR 022 lists which suites are included and why the rest are not: Miri
# interprets at roughly a thousandfold slowdown, so anything sized for a fuzz
# harness or a conformance corpus is out of reach.

. "$(dirname "$0")/lib.sh"

# The nightly ADR 006 pins, by exact name, invoked explicitly — the same
# construction ADR 011 used for the sanitizers. The root toolchain file is not
# touched, so ci/gate-fuzz-smoke.sh's assertion that nothing outside fuzz/
# resolves to nightly keeps holding.
NIGHTLY="$(grep -oE 'nightly-[0-9]{4}-[0-9]{2}-[0-9]{2}' fuzz/rust-toolchain.toml | head -1)"
if [ -z "$NIGHTLY" ]; then
    fail "cannot read the pinned nightly from fuzz/rust-toolchain.toml"
    verdict "miri"
fi
info "using $NIGHTLY (pinned by ADR 006, invoked per ADR 011)"

if ! rustup run "$NIGHTLY" cargo miri --version >/dev/null 2>&1; then
    fail "miri is not installed for $NIGHTLY"
    fail "  rustup component add miri --toolchain $NIGHTLY"
    verdict "miri"
fi

# The three `parse` tests skipped build 100,000- and 1,000,000-node documents.
# Under Miri that is not slow, it is unfinishable.
SKIP="--skip bounds_a_nesting_bomb --skip large_but_shallow --skip keeps_deep_but_legal"

SUITES="parse ranges handles snapshots order"

# Presence first, and separately, so a missing suite is reported as a missing
# suite. Running the loop first meant a deleted file failed with "miri found
# undefined behaviour in snapshots", which is untrue and sends somebody looking
# in the wrong place -- found by deleting one and reading what the gate said.
#
# It is also the non-vacuity guard. A Miri job whose suite list has shrunk is
# the "green job that inspects nothing" the Phase 0 backlog entry warned about,
# arrived at by attrition rather than by decision.
missing=0
for suite in $SUITES; do
    if [ ! -f "crates/px-dom/tests/$suite.rs" ]; then
        fail "Miri suite '$suite' does not exist; ADR 022 lists five"
        missing=1
    fi
done
if [ "$missing" -eq 0 ]; then
    ok "all five Miri suites are present"
fi

for suite in $SUITES; do
    [ -f "crates/px-dom/tests/$suite.rs" ] || continue
    info "miri: $suite"
    # shellcheck disable=SC2086
    if cargo "+$NIGHTLY" miri test -p px-dom --locked --test "$suite" -- $SKIP; then
        ok "miri $suite"
    else
        # Could be undefined behaviour, could be an ordinary assertion: Miri
        # runs the tests as well as interpreting them. The gate says what it
        # knows rather than guessing which.
        fail "miri run failed for $suite (undefined behaviour, or a failing test)"
    fi
done

verdict "miri"
