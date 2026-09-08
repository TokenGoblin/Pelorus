#!/usr/bin/env bash
#
# Repository structure invariants that no other gate covers.
#
#   - every crate carries a CLAUDE.md (build-spec §5, Phase 0 deliverable)
#   - px-brand does not hold the update URL or signing key (§14.1)
#
# OS-independent. CI runs it on Linux only.

. "$(dirname "$0")/lib.sh"

shopt -s nullglob
crates=(crates/px-*/)
if [ ${#crates[@]} -eq 0 ]; then
    fail "no crates found under crates/"
else
    for dir in "${crates[@]}"; do
        if [ -f "$dir/CLAUDE.md" ]; then
            ok "$dir CLAUDE.md"
        else
            fail "$dir has no CLAUDE.md"
        fi
    done
fi

# §14.1: renaming must never move the update endpoint. The way that breaks is
# somebody helpfully consolidating "all the constants" into px-brand.
BRAND_SRC="crates/px-brand/src/lib.rs"
if [ -f "$BRAND_SRC" ]; then
    if grep -qiE 'UPDATE_URL|SIGNING_KEY|PUBLIC_KEY|https?://' "$BRAND_SRC"; then
        fail "$BRAND_SRC looks like it holds an update URL or key; §14.1 puts"
        fail "  those in px-update, permanently, out of scope for a rename"
    else
        ok "px-brand holds no update endpoint or key (§14.1)"
    fi
fi

verdict "structure"
