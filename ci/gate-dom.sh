#!/usr/bin/env bash
#
# Phase 4 gate (build-spec §9).
#
#   html5lib-tests >= 99%                                   -> here
#   stale-handle fuzz target: every stale lookup is None     -> here
#   24h mutation fuzz clean                                  -> campaign, here
#   100,000-level nesting without stack overflow             -> here
#
# Written failing, before px-dom has an arena. The phase is done when this
# passes on both operating systems, and not when the build is green.
#
# # The one that is not like the others
#
# Three of these are ordinary: a conformance percentage, a fuzz target, a
# campaign. The nesting item is a structural claim about the whole crate. A
# hundred thousand levels of nesting overflows the stack of any recursive tree
# walk, and a DOM has walks everywhere — parsing, dropping, serialising,
# querying, styling. Passing it once by fixing one function is not passing it;
# the crate has to be written without recursion over tree depth, and `Drop` is
# the one everybody forgets because the compiler writes it for you.
#
# So this gate also checks the *shape* of the code, not only its behaviour.
# A test can show that today's walks are iterative. It cannot show that
# tomorrow's will be.

. "$(dirname "$0")/lib.sh"

if ! command -v cargo >/dev/null 2>&1; then
    fail "cargo is not on PATH"
    verdict "dom"
fi

# ---------------------------------------------------------------------------
# Gate items as named tests, with the three conditions Phases 1-3 taught this
# gate to apply: the filter matches something, the named file actually carries
# #[test] functions for it, and none of them is #[ignore]d.
# ---------------------------------------------------------------------------

# `ranges` is not one of §9 Phase 4's four gate items. It is in this list
# anyway, because §9's *phase* names four deliverables -- "mutation-safe
# iteration, tree ordering, ranges, depth limits" -- and its gate only covers
# three of them. A deliverable named in the phase and absent from its gate is
# one that can be skipped with nothing going red, which was very nearly what
# happened here: ranges were the last thing found missing, after the gate had
# already gone green.
#
# This gate has always been allowed to check more than §9 lists -- the
# no-infallible-accessor and no-owned-children scans below are not gate items
# either. This is the same kind of addition.
SUITES="
stale-handles::dom_stale::crates/px-dom/tests/handles.rs
deep-nesting::dom_depth::crates/px-dom/tests/depth.rs
tree-order::dom_order::crates/px-dom/tests/order.rs
mutation-safety::dom_mutation::crates/px-dom/tests/mutation.rs
ranges::range_::crates/px-dom/tests/ranges.rs
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

# The fuzz targets' bodies, as ordinary tests.
#
# `px_dom::harness` holds what `fuzz_targets/dom_stale_handle.rs` and
# `dom_mutation.rs` run, and it is behind the `testing` feature, so the
# workspace run above does not execute a line of it. Without this step the two
# targets are checked by nothing until the next campaign — and Phase 1 already
# shipped a 24-hour campaign that reported clean while never reaching the code
# it was built for.
#
# Cheap enough to be unconditional: about a tenth of a second for twelve
# hundred operation sequences.
info "cargo test -p px-dom --features testing (the fuzz bodies)"
cargo test -p px-dom --features testing --locked \
    || fail "the fuzz target bodies failed as tests"

# ---------------------------------------------------------------------------
# §4.1: there is no infallible index API, not even a private one.
#
# The rule the whole design rests on, and the one that erodes quietly. Somebody
# writing a tree walk gets tired of `?` on every hop and adds a small private
# helper that unwraps — and every caller after that is one stale handle away
# from DOM-level type confusion, in safe Rust, with no unsafe block to review.
#
# The tests can show that today's accessors return Option. Only a source check
# can say that no private one appeared.
# ---------------------------------------------------------------------------

# Scan the code, not the prose.
#
# `grep -rn` over a crate matches its comments too, and here that breaks the
# gate in both directions at once.
#
# Forward: the ban has to be stateable. This script's own header names
# `Vec<Node>` as the thing it forbids, and px-dom's module docs explain at
# length why a node must not own its children — and without this filter every
# one of those sentences is a violation. A rule that cannot be written down
# without tripping is a rule people comply with by deleting the explanation.
#
# Backward, and worse: the non-vacuity guard below passes when it finds an
# `Option<&Node>` anywhere in the crate. A doc comment mentioning the shape
# would satisfy it. That would leave the guard reporting "the phase has
# started" on the strength of a sentence about starting the phase, which is
# exactly the failure the guard was added to prevent.
#
# Only whole-line comments are dropped. `let v: Vec<Node> = x; // note` still
# trips, and should.
# `-e` is not decoration: OPTION_ACCESSOR below starts with `->`, and without
# it grep reads the pattern as a bundle of options and never scans anything.
scan_code() {
    grep -rnE -e "$1" crates/px-dom/src 2>/dev/null \
        | grep -vE '^[^:]*:[0-9]+:[[:space:]]*//'
}

# Anchored on `->` rather than matching `Option<...Node>` anywhere.
#
# Without the arrow this matched the arena's own `node: Option<Node>` slot
# field, so the guard reported "px-dom has an Option-returning accessor" on the
# strength of a struct member. It would have gone green on a crate with storage
# and no accessors at all — which is precisely the half-started state the guard
# exists to catch. Found by deleting the real accessors and watching it pass.
OPTION_ACCESSOR='->[[:space:]]*Option<[[:space:]]*&?[[:space:]]*(mut[[:space:]]+)?Node\b'
BARE_ACCESSOR='^\s*(pub(\([^)]*\))?\s+)?fn\s+\w+\s*\([^)]*\)\s*->\s*&?\s*(mut\s+)?Node\b'

# This check must not be able to pass by finding nothing. px-dom starts as a
# stub, and "no bad accessor exists" is trivially true of a crate with no
# accessors at all — a green line meaning the phase has not started. So it
# requires the good shape to be present before it reports on the bad one, the
# same way ci/gate-ipc.sh refuses to pass on an empty scan.
info "no infallible node accessor"
if [ -d crates/px-dom/src ]; then
    if [ -z "$(scan_code "$OPTION_ACCESSOR")" ]; then
        fail "px-dom declares no Option-returning node accessor; this check"
        fail "  cannot pass by finding nothing"
    # A function returning &Node or &mut Node rather than Option<&Node> is the
    # shape being banned. Deliberately matches private ones too.
    elif [ -n "$(scan_code "$BARE_ACCESSOR")" ]; then
        fail "px-dom has an accessor returning Node rather than Option<Node>"
        scan_code "$BARE_ACCESSOR" | head -5 | sed 's/^/       /' >&2
    else
        ok "every node accessor returns Option"
    fi
else
    fail "crates/px-dom/src does not exist"
fi

# ---------------------------------------------------------------------------
# Tree walks are iterative, including the one the compiler writes.
#
# A recursive Drop on a linked tree is the classic 100,000-nesting crash, and
# it is invisible: nobody writes `fn drop` recursively on purpose, it happens
# because dropping a node drops its children. The arena makes that avoidable —
# nodes live in a Vec and dropping the arena drops a flat allocation — but only
# if nothing holds a Box<Node> or a Vec<Node> *inside* a node.
# ---------------------------------------------------------------------------

# Same rule: a crate with no Node type trivially has no node owning another.
OWNED_CHILD='Box<\s*Node\b|Vec<\s*Node\b|Rc<\s*Node\b|Arc<\s*Node\b'

info "no owned child nodes inside a node"
if [ -z "$(scan_code '(struct|enum)[[:space:]]+Node\b')" ]; then
    fail "px-dom declares no Node type; this check cannot pass by finding nothing"
elif [ -n "$(scan_code "$OWNED_CHILD")" ]; then
    fail "a node owns child nodes directly; dropping the tree will recurse"
    scan_code "$OWNED_CHILD" | head -5 | sed 's/^/       /' >&2
else
    ok "nodes are held in the arena, not inside each other"
fi

# ---------------------------------------------------------------------------
# html5lib-tests: the conformance corpus §9 sets at 99%.
#
# Vendored rather than fetched, for the reason ci/gate-network.sh gives about
# recorded traffic: a gate that needs the internet is a gate that goes red for
# reasons nobody here controls.
# ---------------------------------------------------------------------------

HTML5LIB="tests/html5lib"
if [ ! -d "$HTML5LIB" ]; then
    fail "$HTML5LIB does not exist; the conformance corpus is not vendored"
else
    count="$(find "$HTML5LIB" -name '*.dat' 2>/dev/null | wc -l | tr -d ' ')"
    if [ "${count:-0}" -eq 0 ]; then
        fail "$HTML5LIB holds no .dat files"
    else
        ok "the conformance corpus holds $count files"
    fi
fi

# The percentage itself, printed rather than merely asserted.
#
# ADR 019: the graded figure sets two causes aside — tests needing a
# JavaScript engine, and the whatwg/html#12118 processing-instruction change
# that html5ever 0.39 predates — while the unadjusted figure is measured on
# every run and held to a floor. Both numbers reach the log here, because
# "Phase 4 passed" and "94.62%" have to be reconcilable by somebody reading
# the output rather than only by somebody who finds the ADR.
info "html5lib conformance (see docs/adr/019-conformance-gate-exclusions.md)"
# One thread, so the two suites' output does not interleave into a line that
# reads as neither number.
if cargo test -p px-dom --locked --test html5lib -- --nocapture --test-threads=1 2>&1         | grep -E "^(html5lib |  set aside:)" ; then
    ok "conformance reported above"
else
    fail "the conformance suite did not report a number"
fi

# ---------------------------------------------------------------------------
# 24h mutation fuzz, and the stale-handle target §4.1 names explicitly.
# ---------------------------------------------------------------------------

for target in dom_stale_handle dom_mutation; do
    if [ ! -f "fuzz/fuzz_targets/$target.rs" ]; then
        fail "fuzz target $target does not exist; §9 Phase 4 requires it"
    else
        ok "fuzz target $target exists"
    fi
done

# ---------------------------------------------------------------------------
# §14.3 asks Phase 4 to measure two NodeId layouts and record the choice:
# 32/32 index/generation at 8 bytes, against 24/8 with slot retirement at 4.
# A handle is in every node, every range, every style computation, so the
# difference is real at scale — and the decision is only meaningful if the
# measurement was actually taken.
# ---------------------------------------------------------------------------

if ls docs/adr/*node-id* >/dev/null 2>&1 || grep -rlq "24/8" docs/adr/ 2>/dev/null; then
    ok "the NodeId layout choice is recorded in an ADR"
else
    fail "§14.3 asks Phase 4 to measure both NodeId layouts and record the choice"
fi

verdict "dom"
