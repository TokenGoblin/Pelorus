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

# ADR 023: stylo's build.rs runs properties/build.py through Mako, so a Python
# interpreter and a Mako version are build inputs. Every gate that compiles the
# workspace now compiles px-css, so the pin is applied here -- once -- rather
# than in each of the ten gate scripts that would otherwise need it.
#
# Deliberately NOT fatal, and this is the one place in the gates where "fail
# closed" is not the right reading of the rule. Two reasons:
#
#   - ci/gate-structure.sh and the other source-only gates compile nothing and
#     currently run with no network at all. Making them depend on reaching PyPI
#     to check a shebang would be a worse gate, not a stricter one.
#   - A gate that does compile fails on its own when stylo's build.rs cannot find
#     Mako, and says so in stylo's own words. There is no silent-success path to
#     close here.
#
# The claim that *does* need closing is the reproducibility one, because that is
# the check which would otherwise print two matching hashes from an unpinned
# generator. ci/build-and-hash.sh refuses to build without the pin, explicitly.
#
# The setup script's stderr is shown rather than discarded. The first version of
# this block sent it to /dev/null, and when the reproducible job failed on both
# platforms the log said only "could not build the pinned Python environment"
# with no reason -- the reason being that the script sourced this file from a
# `git archive` extract with no .git, where `git rev-parse` exits under `set -e`.
# A diagnostic nobody can see costs more than the noise it saves.
PYTHON3="${PYTHON3:-}"
if [ -f build-python/requirements.txt ] && [ -x ci/setup-build-python.sh ]; then
    _python_log="$(mktemp)"
    if _pinned_python="$(ci/setup-build-python.sh 2>"$_python_log")"; then
        PYTHON3="$REPO_ROOT/$_pinned_python"
    else
        printf '  note: the pinned Python environment could not be built, so a
' >&2
        printf '  note: gate that compiles px-css will fail in stylo build.rs:
' >&2
        sed 's/^/  note:   /' "$_python_log" >&2 || true
    fi
    rm -f "$_python_log"
fi
export PYTHON3

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
