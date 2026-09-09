#!/usr/bin/env bash
#
# Gate check 2 — reproducible builds (invariant 7).
#
# Usage: ci/compare-hashes.sh <hashes-a> <hashes-b>

. "$(dirname "$0")/lib.sh"

A="${1:?usage: compare-hashes.sh <a> <b>}"
B="${2:?usage: compare-hashes.sh <a> <b>}"

[ -s "$A" ] || fail "$A is empty — the first build produced no artifacts"
[ -s "$B" ] || fail "$B is empty — the second build produced no artifacts"
[ "$_fail_count" -eq 0 ] || verdict "reproducible"

if diff -u "$A" "$B" >/dev/null; then
    while read -r hash name; do ok "$name $hash"; done < "$A"
else
    fail "build hashes differ between the two environments"
    diff -u "$A" "$B" | sed 's/^/       /' >&2
fi

verdict "reproducible"
