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

# name:script — the two after the blank comment are not §9 gate checks.
CHECKS="
build:ci/gate-build.sh
supply-chain:ci/gate-supply-chain.sh
unsafe-headers:ci/gate-unsafe-headers.sh
brand-leak:tests/brand/gate-brand-leak.sh
structure:ci/gate-structure.sh
compat-list:ci/gate-compat-list.sh
ipc:ci/gate-ipc.sh
sandbox:ci/gate-sandbox.sh
"

failed=""
for entry in $CHECKS; do
    name="${entry%%:*}"
    script="${entry#*:}"
    printf '%-16s ' "$name"
    if output="$(bash "$script" 2>&1)"; then
        printf 'PASS\n'
    else
        printf 'FAIL\n'
        printf '%s\n' "$output" | grep -E '^FAIL' | sed 's/^/                 /'
        failed="$failed $name"
    fi
done

echo
if [ -z "$failed" ]; then
    echo "all checks pass"
    exit 0
fi
echo "failed:$failed"
exit 1
