#!/usr/bin/env bash
#
# Gate check 5 — brand leak (build-spec §2.2, Phase 0 gate).
#
# Fails if the product name appears anywhere outside tests/brand/allowlist.txt.
#
# This script contains no occurrence of the product name, by construction: it
# reads the name out of px-brand at runtime. That means it cannot accidentally
# exempt itself from its own check, and a rename does not touch it.

. "$(dirname "$0")/../../ci/lib.sh"

BRAND_SRC="crates/px-brand/src/lib.rs"
ALLOWLIST="tests/brand/allowlist.txt"

if [ ! -f "$BRAND_SRC" ]; then
    fail "$BRAND_SRC does not exist — there is no source of truth to read the name from"
    verdict "brand-leak"
fi

NAME="$(sed -n 's/^pub const PRODUCT_NAME: &str = "\([^"]*\)";.*$/\1/p' "$BRAND_SRC" | head -n1)"
if [ -z "$NAME" ]; then
    fail "could not read PRODUCT_NAME from $BRAND_SRC"
    verdict "brand-leak"
fi
info "read PRODUCT_NAME from $BRAND_SRC (${#NAME} chars); scanning tracked files"

allowed() {
    local path="$1" pat
    while IFS= read -r pat; do
        case "$pat" in ''|'#'*) continue ;; esac
        case "$pat" in
            */) [ "${path##"$pat"}" != "$path" ] && return 0 ;;
            *)  # shellcheck disable=SC2053
                [[ "$path" == $pat ]] && return 0 ;;
        esac
    done < "$ALLOWLIST"
    return 1
}

scanned=0
while IFS= read -r -d '' path; do
    allowed "$path" && continue
    scanned=$((scanned + 1))
    # -I skips binary files; -F fixed string; -i case-insensitive, because a
    # lowercase or uppercase spelling is the same leak.
    if grep -I -F -i -q -- "$NAME" "$path" 2>/dev/null; then
        fail "product name appears in $path"
        grep -I -F -i -n -- "$NAME" "$path" | head -n 5 | sed 's/^/       /' >&2
    fi
done < <(git ls-files -z)

info "scanned $scanned tracked files"
verdict "brand-leak"
