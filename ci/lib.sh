# Shared helpers for gate scripts. Sourced, not executed.
#
# Gate scripts are plain shell so that one implementation serves both CI
# platforms and can be run locally without CI. CI invokes these; it does not
# reimplement them. A gate you cannot run on your own machine is a gate you
# will stop trusting.

set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

_fail_count=0

# python3 is the name on Linux; on Windows the same interpreter is usually
# `python`, and the bare `python3` resolves to a Microsoft Store stub that
# exits without running anything. Resolve once, here, rather than in each gate.
# Gate output contains section signs and em dashes. Without this, Python on a
# Windows console falls back to cp1252 and mangles them.
export PYTHONIOENCODING=utf-8

PY_BIN=""
for _c in python3 python; do
    if command -v "$_c" >/dev/null 2>&1 && "$_c" -c 'import sys; sys.exit(0 if sys.version_info >= (3, 11) else 1)' >/dev/null 2>&1; then
        PY_BIN="$_c"
        break
    fi
done

info() { printf '  %s\n' "$*"; }
ok()   { printf 'ok   %s\n' "$*"; }
fail() { printf 'FAIL %s\n' "$*" >&2; _fail_count=$((_fail_count + 1)); }

# Print the gate's verdict and exit accordingly.
verdict() {
    local gate="$1"
    if [ "$_fail_count" -eq 0 ]; then
        printf '\n%s: PASS\n' "$gate"
        return 0
    fi
    printf '\n%s: FAIL (%d)\n' "$gate" "$_fail_count" >&2
    exit 1
}
