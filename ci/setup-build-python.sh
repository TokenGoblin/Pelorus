#!/usr/bin/env bash
#
# Build the pinned Python environment stylo's build.rs needs, and print the path
# to its interpreter on stdout.
#
# ADR 023: `stylo`'s `build.rs` reads `PYTHON3` from the environment (falling
# back to `python3` / `python.exe`), then runs `properties/build.py` through Mako
# to generate its property definitions. So an interpreter and a Mako version are
# build inputs, which is why invariant 7 now reads "same source + same toolchain
# + same pinned build-time generators".
#
# The point of this script is that the pin is *materialised* rather than
# described. `build-python/requirements.txt` names exact versions; this builds an
# environment containing exactly those and nothing else, so a Mako that happens
# to be installed system-wide cannot be the one that generates the release
# binary's contents.
#
# Usage:
#   PYTHON3="$(ci/setup-build-python.sh)"
#   export PYTHON3
#
# Idempotent: an existing venv whose pins already match is reused, because this
# runs in front of every reproducible build and creating a venv twice per CI job
# is pure latency.

. "$(dirname "$0")/lib.sh"

VENV=".build-python-venv"
REQUIREMENTS="build-python/requirements.txt"

# Everything this script says goes to stderr. Stdout carries exactly one thing —
# the interpreter path — because the caller substitutes it into PYTHON3, and a
# progress line on stdout would become part of the path.
say() { printf '  %s\n' "$*" >&2; }

if [ ! -f "$REQUIREMENTS" ]; then
    printf 'FAIL %s is missing; there is no pin to build against\n' "$REQUIREMENTS" >&2
    exit 1
fi

if [ -z "$PY_BIN" ]; then
    printf 'FAIL no python >= 3.11 on PATH to create the build venv with\n' >&2
    exit 1
fi

# Windows puts it in Scripts/, everything else in bin/.
venv_python() {
    if [ -x "$VENV/bin/python" ]; then printf '%s' "$VENV/bin/python"
    elif [ -x "$VENV/Scripts/python.exe" ]; then printf '%s' "$VENV/Scripts/python.exe"
    else return 1
    fi
}


# Reuse only on an exact match against the pin. A venv holding Mako 1.4.0 when
# the pin says 1.4.1 is not a venv this build may use; "close enough" is the
# whole failure mode ADR 023 exists to close.
#
# The comparison lives in ci/check_build_python_pin.py, not here, and that file
# says why: written in shell it was wrong four times, because `pip freeze` emits
# CRLF on Windows and the failure message printed `wanted` and `got` identically.
matches_pin() {
    "$1" ci/check_build_python_pin.py "$REQUIREMENTS"
}

if existing="$(venv_python)"; then
    if matches_pin "$existing" 2>/dev/null; then
        say "reusing $VENV (pins match)"
        printf '%s' "$existing"
        exit 0
    fi
    say "$VENV does not match the pin; rebuilding it"
    rm -rf "$VENV"
fi

say "creating $VENV with $("$PY_BIN" --version 2>&1)"
"$PY_BIN" -m venv "$VENV" >&2

interpreter="$(venv_python)" || {
    printf 'FAIL %s was created but has no interpreter in bin/ or Scripts/\n' "$VENV" >&2
    exit 1
}

# --no-deps with a fully-enumerated requirements file: pip is not permitted to
# resolve anything. If Mako gains a dependency the build fails here and the
# requirements file is updated deliberately, rather than the closure growing on
# its own between two builds that were supposed to be identical.
say "installing the pinned generators"
"$interpreter" -m pip install --quiet --no-deps --disable-pip-version-check \
    -r "$REQUIREMENTS" >&2

if ! matches_pin "$interpreter"; then
    printf 'FAIL %s does not match %s after install\n' "$VENV" "$REQUIREMENTS" >&2
    exit 1
fi

printf '%s' "$interpreter"
