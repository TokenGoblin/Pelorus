#!/usr/bin/env bash
#
# Phase 2 gate (build-spec §9).
#
#   content process launches under a policy on both OSes   -> here
#   with the sandbox forced unavailable, the browser        -> here
#     REFUSES to launch content processes and says why
#   Linux fallback ladder documented and exercised (§14.5)  -> here
#
# The second is the one built first: it is the check that fails closed, and the
# one a real user on a restricted kernel actually meets.

. "$(dirname "$0")/lib.sh"

if ! command -v cargo >/dev/null 2>&1; then
    fail "cargo is not on PATH"
    verdict "sandbox"
fi

# ---------------------------------------------------------------------------
# Gate item 1 and 3: tests, named per item, with the same three conditions the
# Phase 1 gate learned to apply — the filter matches, the named file carries
# #[test] fns for it, and none of them is #[ignore]d.
# ---------------------------------------------------------------------------

SUITES="
policy-applied::sandbox_policy::crates/px-sandbox/src/lib.rs
refuses-below-floor::sandbox_refuses::crates/px-content/tests/sandbox.rs
ladder::sandbox_ladder::crates/px-sandbox/src/lib.rs
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

info "cargo test --workspace --locked --features testing"
cargo test --workspace --locked --features testing || fail "tests failed"

# ---------------------------------------------------------------------------
# Gate item 2, end to end: the browser must REFUSE, not degrade.
#
# Phase 1 learned that a library test is not the product. px-browser exited
# FAILURE on a clean run for a week's worth of green checks because nothing
# executed it. So this runs the binary.
# ---------------------------------------------------------------------------

info "px-browser refuses to launch when the sandbox is below the floor"
if cargo build --workspace --locked --features testing --quiet; then
    browser="target/debug/px-browser"
    [ -f "$browser.exe" ] && browser="$browser.exe"

    if [ ! -f "$browser" ]; then
        fail "px-browser was not built; the refusal check cannot run"
    else
        # PX_TEST_FORCE_SANDBOX_UNAVAILABLE only exists under --features
        # testing. See the release-artifact scan below.
        out="$(PX_TEST_FORCE_SANDBOX_UNAVAILABLE=1 "$browser" 2>&1)" && rc=0 || rc=$?

        if [ "$rc" -eq 0 ]; then
            fail "px-browser started with the sandbox unavailable; invariant 8"
            fail "  says it must refuse"
        elif printf '%s' "$out" | grep -qiE 'seccomp|no_new_privs|restricted token|job object'; then
            ok "px-browser refused and named the missing mechanism"
        else
            fail "px-browser refused but did not say why; §14.5 requires the"
            fail "  message to name what is missing. Got: $out"
        fi

        # Refusing must mean no content process exists, not one that started
        # and was killed afterwards.
        if printf '%s' "$out" | grep -qi 'px-content'; then
            info "note: output mentions px-content; check no process was spawned"
        fi
    fi
else
    fail "testing-feature build failed; cannot run the refusal check"
fi

# ---------------------------------------------------------------------------
# §14.4: the forced-unavailable override is a test-only capability, and a
# test-only capability in a release build is a critical vulnerability. A flag
# being off is not evidence; absence from the artifact is.
#
# This scan was filed in docs/backlog.md as a Phase 8 and 12 prerequisite.
# Phase 2 pulls it forward, because Phase 2 is the phase that introduces the
# first test-only capability.
# ---------------------------------------------------------------------------

info "release artifacts carry no test-only capability"
if cargo build --workspace --locked --release --quiet; then
    leaked=0
    for stem in px-browser px-content; do
        bin="target/release/$stem"
        [ -f "$bin.exe" ] && bin="$bin.exe"
        [ -f "$bin" ] || { fail "$stem was not built for release"; continue; }
        if grep -a -q 'PX_TEST_FORCE_SANDBOX_UNAVAILABLE' "$bin"; then
            fail "$stem contains the sandbox override symbol"
            leaked=1
        fi
    done
    [ "$leaked" -eq 0 ] && ok "no test-only sandbox override in release binaries"
else
    fail "release build failed; cannot scan artifacts"
fi

# ---------------------------------------------------------------------------
# ADR 007's ladder has to be written down, not just implemented. A rung nobody
# documented is a rung nobody can tell you lost.
# ---------------------------------------------------------------------------

LADDER_DOC="docs/adr/007-linux-sandbox-ladder.md"
if [ ! -f "$LADDER_DOC" ]; then
    fail "$LADDER_DOC does not exist; §14.5 requires the ladder documented"
else
    missing=""
    for rung in no_new_privs seccomp Landlock 'user namespace'; do
        grep -qi "$rung" "$LADDER_DOC" || missing="$missing $rung"
    done
    if [ -n "$missing" ]; then
        fail "$LADDER_DOC does not document:$missing"
    else
        ok "the ladder is documented rung by rung"
    fi
fi

verdict "sandbox"
