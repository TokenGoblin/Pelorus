#!/usr/bin/env bash
#
# Phase 6 gate (build-spec §9).
#
#   WPT CSS2 reftest subset at threshold          -> here
#   identical box tree on repeat runs             -> here
#   iterative (non-recursive) tree walks, by a
#     deep-nesting test                           -> here
#
# Written failing, before px-layout has a box in it. The phase is done when this
# passes in CI on both operating systems — not when the build is green, and not
# when it passes on the machine that wrote it.
#
# # The three items are three different kinds of claim
#
# **The reftest item is a conformance number**, and like every conformance number
# in this project it is gradeable against a corpus this project chooses. So the
# subset is committed, its size is pinned, and the gate prints the graded and
# ungraded figures the way ADR 019 made Phase 4 do. A subset that shrinks to fit
# an implementation that is struggling is the failure mode, and pinning the count
# is what stops it.
#
# **The determinism item is not about correctness at all.** Two runs of the same
# input must produce the same box tree, which is a claim about the absence of
# iteration-order and hash-order dependence. It is cheap to check and almost
# impossible to debug later: a layout that differs by a pixel every hundredth run
# is a bug report nobody can reproduce. px-layout's CLAUDE.md already says
# "non-determinism here is a bug even when the pixels match".
#
# **The nesting item is a structural claim about the whole crate**, and Phase 4
# learned what that means. A hundred thousand nested boxes overflows the stack of
# any recursive walk, and a layout engine has walks everywhere: box-tree
# construction, block layout, inline layout, float placement, `Drop`. Passing it
# once by fixing one function is not passing it. So this gate checks the *shape*
# of the code as well as its behaviour, exactly as ci/gate-dom.sh does — a test
# can show today's walks are iterative, not that tomorrow's will be.

. "$(dirname "$0")/lib.sh"

if ! command -v cargo >/dev/null 2>&1; then
    fail "cargo is not on PATH"
    verdict "layout"
fi

# ---------------------------------------------------------------------------
# The three items as named test suites.
#
# The same three conditions Phases 1-5 taught the gates to apply, each of which
# was once a way a green gate meant nothing: the filter must match a test, the
# named file must carry #[test] functions for it, and none may be #[ignore]d.
# ---------------------------------------------------------------------------

# Every filter carries a `layout_` prefix, and that is not decoration. The first
# version used `depth_`, which also matches px-dom's `dom_depth_*` tests -- so the
# gate reported the deep-nesting item as having tests before px-layout had a file.
# A filter that matches another crate's tests is a gate item satisfied by somebody
# else's work.
SUITES="
css2-reftests::layout_reftest_::crates/px-layout/tests/reftest.rs
box-tree-determinism::layout_determinism_::crates/px-layout/tests/determinism.rs
deep-nesting::layout_depth_::crates/px-layout/tests/depth.rs
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
# The reftest corpus is committed and cannot quietly shrink.
#
# ADR 019's pattern, third outing. A conformance threshold is only a threshold if
# the denominator is fixed; otherwise "95% of the subset" is satisfiable by
# deleting the tests that fail.
# ---------------------------------------------------------------------------

REFTEST_DIR="tests/wpt/css2"
REFTEST_FLOOR=40

if [ ! -d "$REFTEST_DIR" ]; then
    fail "the CSS2 reftest subset is not vendored at $REFTEST_DIR"
    fail "  §9's phrase is 'WPT CSS2 reftest subset at threshold'; a subset that"
    fail "  is not committed is a subset that changes between runs"
else
    pairs="$(git ls-files "$REFTEST_DIR" | grep -c -- '-ref\.\(xht\|html\)$' || true)"
    if [ "${pairs:-0}" -lt "$REFTEST_FLOOR" ]; then
        fail "the subset has ${pairs:-0} reference files, floor is $REFTEST_FLOOR"
    else
        ok "the CSS2 subset has $pairs test/reference pairs in the index (floor $REFTEST_FLOOR)"
    fi
fi

# ---------------------------------------------------------------------------
# Au is the geometry type, and saturating is the arithmetic.
#
# §4.2: "Layout uses Au — app units, i32 at 1/60 px — with explicit saturating
# operations, so geometry overflow is defined and testable rather than a panic or
# a wrap."
#
# Both halves are checkable in source and neither is checkable by a test alone. A
# test can show that today's arithmetic saturates; only a scan says that no `+`
# on a raw i32 crept in beside it. `f32` in a layout crate is the specific thing
# §4.2 exists to prevent — it makes "identical box tree on repeat runs" depend on
# the order additions happened to be performed in.
# ---------------------------------------------------------------------------

layout_src="crates/px-layout/src"
if [ -d "$layout_src" ]; then
    floats="$( { grep -rnE '\b(f32|f64)\b' "$layout_src" || true; } \
               | { grep -vE '^[^:]*:[0-9]+:[[:space:]]*//' || true; } | wc -l)"
    if [ "$floats" -eq 0 ]; then
        ok "px-layout names no f32 or f64"
    else
        fail "px-layout names f32 or f64 in $floats place(s); geometry is Au (§4.2)."
        fail "  Floating point makes repeat runs depend on the order operations"
        fail "  happened in, which is the second gate item."
        { grep -rnE '\b(f32|f64)\b' "$layout_src" || true; } \
            | { grep -vE '^[^:]*:[0-9]+:[[:space:]]*//' || true; } | head -n 5 | sed 's/^/       /' >&2
    fi
fi

# ---------------------------------------------------------------------------
# Nothing constructs `Au` through its tuple field.
#
# `app_units::Au` is `pub struct Au(pub i32)`, and its saturating `Add` is only
# sound while every value is inside ±(2^30 - 1): two such values sum to at most
# 2^31 - 2, which is why the raw `i32` addition inside `Add` cannot overflow.
# `Au(i32::MAX)` is constructible, costs nothing, reads fine, and turns the next
# addition into the panic-or-wrap §4.2 forbids. app_units says so itself: "It is
# safe to construct invalid Au values, but it may lead to panics and overflows."
#
# `Au::new`, `Au::from_px` and this crate's `geom::px` / `geom::au` all clamp.
# `Au(0)` is exempt: it is unambiguously in range and a `const fn` cannot call
# `Au::new`, which is what the zero constants need.
#
# A test can show today's arithmetic saturates. Only a scan says no out-of-range
# value was constructed next to it.
#
# The character class is `[^A-Za-z0-9_]` and not `[^a-zA-Z_:]`. The first version
# excluded `:`, which excludes `::Au(` -- the fully-qualified form, and the one
# most likely to be written. Probed with a real `app_units::Au(i32::MAX)` and the
# scan reported ok, which is how it was found. A check that cannot fail is worse
# than no check, because it reads as a guarantee.
# ---------------------------------------------------------------------------

if [ -d "$layout_src" ]; then
    raw_au="$( { grep -rnE '(^|[^A-Za-z0-9_])Au\(' "$layout_src" || true; }                | { grep -vE '^[^:]*:[0-9]+:[[:space:]]*//' || true; }                | { grep -vE 'Au\(0\)' || true; } | wc -l)"
    if [ "$raw_au" -eq 0 ]; then
        ok "px-layout constructs no Au through its tuple field"
    else
        fail "px-layout constructs Au(...) directly in $raw_au place(s)."
        fail "  Use geom::px, geom::au, or Au::from_px -- all of which clamp."
        fail "  An out-of-range Au makes the next addition overflow, which is the"
        fail "  panic-or-wrap §4.2 exists to prevent."
        { grep -rnE '(^|[^A-Za-z0-9_])Au\(' "$layout_src" || true; }             | { grep -vE '^[^:]*:[0-9]+:[[:space:]]*//' || true; }             | { grep -vE 'Au\(0\)' || true; } | head -n 5 | sed 's/^/       /' >&2
    fi
fi

# ---------------------------------------------------------------------------
# No recursion over tree depth.
#
# The scan ci/gate-dom.sh runs, for the same reason and against the same failure.
# A function that calls itself while walking boxes is a stack overflow on a
# document a page can serve, and `Drop` is the one everybody forgets because the
# compiler writes it.
# ---------------------------------------------------------------------------

if [ -d "$layout_src" ]; then
    self_calls=0
    while IFS= read -r -d '' path; do
        # A direct self-call: `fn name(` declared in this file, and `name(` used
        # inside it. Crude, and deliberately so -- it is a prompt to look, not a
        # proof, and the deep-nesting test is what actually holds the property.
        while IFS= read -r fname; do
            [ -n "$fname" ] || continue
            uses="$(grep -cE "(^|[^a-zA-Z_.])${fname}\s*\(" "$path" || true)"
            # One occurrence is the declaration itself.
            if [ "${uses:-0}" -gt 1 ]; then
                fail "$path: ${fname}() appears to call itself; layout walks are iterative"
                self_calls=$((self_calls + 1))
            fi
        done < <(grep -oE '^\s*(pub(\([a-z]+\))? )?fn [a-z_][a-z0-9_]*' "$path" \
                 | sed 's/.*fn //' || true)
    done < <(git ls-files -z "$layout_src/*.rs")
    [ "$self_calls" -eq 0 ] && ok "no function in px-layout calls itself"
fi

verdict "layout"
