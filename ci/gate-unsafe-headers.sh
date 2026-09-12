#!/usr/bin/env bash
#
# Gate check 4 — the per-crate unsafe rules (build-spec §4, CLAUDE.md).
#
# A source-text assertion, not a convention. Every crate's entry point must
# open with #![forbid(unsafe_code)]. Two crates are excepted, with different
# rules, and the difference is the point:
#
#   px-sandbox (ADR 008)  must carry #![deny(unsafe_op_in_unsafe_fn)] and must
#                         NOT carry forbid, which would make the crate unable
#                         to do the one job it exists for.
#
#   px-css (ADR 024)      may drop forbid, because stylo's TElement declares six
#                         unsafe fn methods and forbid rejects *implementing* an
#                         unsafe method. In exchange the crate must contain zero
#                         unsafe blocks and zero unsafe impl — an unsafe fn body
#                         needs neither, so nothing in the crate does anything
#                         the compiler is not checking.
#
# px-css is checked in both states rather than only the one it will end up in.
# While it still carries forbid — true today, and true until stylo actually
# lands — the gate says so and requires nothing else. The moment forbid goes,
# the two zero-counts become the rule. Written this way because the commit that
# removes forbid is the one window in which the guarantee could lapse unnoticed,
# and that is exactly the kind of window this project keeps falling through.
#
# OS-independent: this reads source text. CI runs it on Linux only.

. "$(dirname "$0")/lib.sh"

SANDBOX="px-sandbox"
STYLO_CRATE="px-css"

shopt -s nullglob
crates=(crates/px-*/)
if [ ${#crates[@]} -eq 0 ]; then
    fail "no crates found under crates/ — nothing to audit"
verdict "unsafe-headers"
fi

for dir in "${crates[@]}"; do
    name="$(basename "$dir")"
    # Every crate root, not just the two conventional ones. A crate with an
    # extra [[bin]] target — which px-broker already has — carries a SECOND
    # crate root, and forbid(unsafe_code) in lib.rs does not cover it. The
    # gate used to look at exactly two paths and never at that file.
    entries=("$dir"src/lib.rs "$dir"src/main.rs "$dir"src/bin/*.rs)
    # Only `path = "src/..."` lines, not any `src/*.rs` the file happens to
    # mention. The looser pattern scraped a path out of a *comment* in px-css's
    # Cargo.toml and reported src/dom.rs as a crate root that had "dropped
    # forbid" -- a file that is not a crate root and never carried the attribute.
    # Harmless here because it printed an ok line, which is exactly why it would
    # have gone unnoticed: a check that invents a subject can also invent a pass.
    while IFS= read -r extra; do
        [ -n "$extra" ] && entries+=("$dir$extra")
    done < <(grep -oE '^[[:space:]]*path[[:space:]]*=[[:space:]]*"src/[A-Za-z0-9_/.-]+[.]rs"' "$dir/Cargo.toml" 2>/dev/null              | grep -oE 'src/[A-Za-z0-9_/.-]+[.]rs' | sort -u)
    found=0
    for entry in "${entries[@]}"; do
        [ -f "$entry" ] || continue
        found=1
        if [ "$name" = "$SANDBOX" ]; then
            if grep -q '^#!\[deny(unsafe_op_in_unsafe_fn)\]' "$entry"; then
                ok "$entry deny(unsafe_op_in_unsafe_fn)"
            else
                fail "$entry is missing #![deny(unsafe_op_in_unsafe_fn)]"
            fi
            if grep -q '^#!\[forbid(unsafe_code)\]' "$entry"; then
                fail "$entry carries forbid(unsafe_code); $SANDBOX is the crate that needs unsafe"
            fi
        elif [ "$name" = "$STYLO_CRATE" ]; then
            # Either state is legal; neither is unchecked.
            if grep -q '^#!\[forbid(unsafe_code)\]' "$entry"; then
                ok "$entry forbid(unsafe_code), still (ADR 024 not yet needed)"
            else
                ok "$entry has dropped forbid under ADR 024"
            fi
        else
            if grep -q '^#!\[forbid(unsafe_code)\]' "$entry"; then
                ok "$entry forbid(unsafe_code)"
            else
                fail "$entry is missing #![forbid(unsafe_code)]"
            fi
        fi
    done
    [ "$found" -eq 1 ] || fail "$dir has no src/lib.rs or src/main.rs"
done

# The header check above is what makes unsafe a compile error. What it cannot
# see is somebody switching the attribute off locally, so scan for the bypass
# itself rather than for the word `unsafe`, which appears legitimately in prose.
# Assert the scan has something to scan. Both waiver loops iterate git
# ls-files, and if a pathspec matches nothing the loop body never runs and the
# gate passes having inspected zero files — the same "a check that cannot fail
# looks like a check that passes" shape that defeated two other checks here.
scanned_any=0
while IFS= read -r -d '' path; do
    scanned_any=1
    case "$path" in crates/$SANDBOX/*) continue ;; esac
    # px-css is allowed to not carry forbid (ADR 024), so an allow(unsafe_code)
    # there is redundant rather than a bypass -- but it is also a sign somebody
    # reached for the blanket exemption the ADR rejected, so it still fails.
    # The rule for that crate is enforced positively just below.
    if grep -nE '(allow|expect)\(unsafe_code\)' "$path" >/dev/null 2>&1; then
        fail "$path switches off unsafe_code locally; only $SANDBOX may use unsafe"
        grep -nE '(allow|expect)\(unsafe_code\)' "$path" | head -n 5 | sed 's/^/       /' >&2
    fi
done < <(git ls-files -z 'crates/*.rs')
if [ "$scanned_any" -eq 0 ]; then
    fail "the unsafe_code waiver scan matched no files at all"
fi

# CLAUDE.md, hard rules: "Clippy denies these; do not add allow attributes to
# get around it." A denied lint that can be waived at the call site is a style
# preference, not a rule, so the waiver is what the gate looks for.
WAIVER='(allow|expect)\(clippy::(unwrap_used|expect_used|indexing_slicing|panic)\)'
# Must match the crates carrying `[lints] workspace = true`. px-ipc was added
# to that set and omitted here, which left an allow() in the crate that decodes
# hostile bytes passing CI silently — the exact bypass this scan exists for.
waiver_scanned=0
for crate in px-content px-net px-mcp px-ipc; do
    [ -d "crates/$crate" ] || continue
    while IFS= read -r -d '' path; do
        waiver_scanned=1
        if grep -nE "$WAIVER" "$path" >/dev/null 2>&1; then
            fail "$path waives a panic lint that CLAUDE.md denies in $crate"
            grep -nE "$WAIVER" "$path" | head -n 5 | sed 's/^/       /' >&2
        fi
    done < <(git ls-files -z "crates/$crate/*.rs")
done
if [ "$waiver_scanned" -eq 0 ]; then
    fail "the panic-lint waiver scan matched no files at all"
fi

# ADR 024's two counts, positively enforced.
#
# Duplicated from ci/gate-style.sh deliberately. That gate is Phase 5's and
# phase gates are not run forever; this one is permanent, and the rule it
# encodes outlives the phase that needed it. A rule enforced only by the gate of
# the phase that introduced it stops being enforced when the phase closes.
#
# `|| true` on each grep because under `set -o pipefail` a grep that matches
# nothing exits 1 and takes the script with it — which would skip both
# assertions in exactly the zero-unsafe case they exist to confirm.
if [ -d "crates/$STYLO_CRATE/src" ]; then
css_blocks="$( { grep -rnE '(^|[^a-zA-Z_])unsafe[[:space:]]*\{' "crates/$STYLO_CRATE/src" || true; } | { grep -vE '^[^:]*:[0-9]+:[[:space:]]*//' || true; } | wc -l)"
css_impls="$( { grep -rnE '(^|[^a-zA-Z_])unsafe[[:space:]]+impl' "crates/$STYLO_CRATE/src" || true; } | { grep -vE '^[^:]*:[0-9]+:[[:space:]]*//' || true; } | wc -l)"
if [ "$css_blocks" -eq 0 ]; then
    ok "$STYLO_CRATE contains no unsafe block (ADR 024)"
else
    fail "$STYLO_CRATE contains $css_blocks unsafe block(s); ADR 024 says zero."
    fail "  An unsafe fn required by a stylo trait signature needs no unsafe"
    fail "  body. A real unsafe operation belongs in $SANDBOX (ADR 008)."
fi
if [ "$css_impls" -eq 0 ]; then
    ok "$STYLO_CRATE contains no unsafe impl (ADR 024)"
else
    fail "$STYLO_CRATE contains $css_impls unsafe impl(s); ADR 024 says zero."
    fail "  Send/Sync over a borrowed view are derivable rather than asserted."
fi
fi

verdict "unsafe-headers"
