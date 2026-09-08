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
    entries=("$dir"src/lib.rs "$dir"src/main.rs)
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

# Belt and braces: no crate other than px-sandbox may contain the token at all.
# forbid(unsafe_code) already makes this a compile error, but the source check
# catches an attribute removed in the same commit that adds the unsafe block.
while IFS= read -r -d '' path; do
    case "$path" in crates/$SANDBOX/*) continue ;; esac
    if grep -nE '(^|[^[:alnum:]_])unsafe[^[:alnum:]_]' "$path" >/dev/null 2>&1; then
        fail "the unsafe keyword appears in $path (only $SANDBOX may use it)"
        grep -nE '(^|[^[:alnum:]_])unsafe[^[:alnum:]_]' "$path" | head -n 5 | sed 's/^/       /' >&2
    fi
done < <(git ls-files -z 'crates/*.rs')

verdict "unsafe-headers"
