#!/usr/bin/env bash
#
# Run the whole phase gate locally, in the order CI runs it.
#
# The five checks §9 names for Phase 0, plus the two jobs that are structure
# and deliverable rather than gate. Every one of them is the same script CI
# invokes, which is the point: a gate you cannot run on your own machine is a
# gate you will learn to argue with.
#
# The reproducibility check is not here. It builds the tree twice from scratch
# under two environments and is driven by the workflow, which varies HOME,
# CARGO_HOME and — on Linux — the user between runs. Run it by hand:
#
#   ci/build-and-hash.sh /tmp/a > /tmp/a.txt
#   ci/build-and-hash.sh /tmp/some/deeper/path > /tmp/b.txt
#   ci/compare-hashes.sh /tmp/a.txt /tmp/b.txt

set -uo pipefail
cd "$(git rev-parse --show-toplevel)"

# name:command -- `cargo xtask <gate>` once ported (ADR 032), else `bash <script>`.
# Ported gates are added with the xtask command; this list shrinks to nothing
# in scripts and then gate-all.sh itself becomes `cargo xtask all`.
CHECKS="
build:bash ci/gate-build.sh
supply-chain:bash ci/gate-supply-chain.sh
unsafe-headers:bash ci/gate-unsafe-headers.sh
brand-leak:cargo xtask brand-leak
structure:cargo xtask structure
compat-list:bash ci/gate-compat-list.sh
ipc:bash ci/gate-ipc.sh
sandbox:bash ci/gate-sandbox.sh
network:bash ci/gate-network.sh
dom:cargo xtask dom
"

failed=""
while IFS= read -r entry; do
    [ -n "$entry" ] || continue
    name="${entry%%:*}"
    script="${entry#*:}"
    printf '%-16s ' "$name"
    # shellcheck disable=SC2086 -- the command is deliberately word-split
    if output="$($script 2>&1)"; then
        printf 'PASS\n'
    else
        printf 'FAIL\n'
        printf '%s\n' "$output" | grep -E '^FAIL' | sed 's/^/                 /'
        failed="$failed $name"
    fi
done <<< "$CHECKS"

echo
if [ -z "$failed" ]; then
    echo "all checks pass"
    exit 0
fi
echo "failed:$failed"
exit 1
