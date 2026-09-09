#!/usr/bin/env bash
#
# Gate check 4 — forbid(unsafe_code) everywhere (build-spec §4, CLAUDE.md).
#
# A source-text assertion, not a convention. Every crate's entry point must
# open with #![forbid(unsafe_code)]. px-sandbox is the sole exception and must
# instead carry #![deny(unsafe_op_in_unsafe_fn)] — and must NOT carry forbid,
# which would make the crate unable to do the one job it exists for.
#
# OS-independent: this reads source text. CI runs it on Linux only.

. "$(dirname "$0")/lib.sh"

SANDBOX="px-sandbox"

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
    while IFS= read -r extra; do
        [ -n "$extra" ] && entries+=("$dir$extra")
    done < <(grep -oE 'src/[A-Za-z0-9_/.-]+[.]rs' "$dir/Cargo.toml" 2>/dev/null | sort -u)
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

verdict "unsafe-headers"
