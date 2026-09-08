#!/usr/bin/env bash
#
# Unsafe-line accounting over the whole dependency tree (build-spec §4.5).
#
# Emits, on stdout, a stable JSON object mapping vendored crate name to a count
# of `unsafe` tokens in its Rust sources. ci/gate-supply-chain.sh compares that
# against the committed baseline; any increase fails and needs an ADR.
#
# Why a script and not cargo-geiger: this is a security project, the count is a
# gate, and the gate should not depend on a thinly-maintained third-party tool
# that runs arbitrary build scripts to produce its number. This counts tokens
# in text. It over-counts — `unsafe` in a comment or a string literal is
# counted — and never under-counts, which is the correct direction for a gate.
# The baseline absorbs the over-count; only movement matters.

. "$(dirname "$0")/lib.sh"

if [ -z "$PY_BIN" ]; then
    echo "FAIL no python >= 3.11 on PATH" >&2
    exit 1
fi

VENDOR="$(mktemp -d)"
trap 'rm -rf "$VENDOR"' EXIT

cargo vendor --locked --versioned-dirs "$VENDOR/crates" >/dev/null 2>&1 || {
    echo "FAIL cargo vendor failed" >&2
    exit 1
}

"$PY_BIN" - "$VENDOR/crates" <<'PY'
import json
import os
import re
import sys

# LF on every platform: this output is compared byte-for-byte against a
# committed baseline, and a CRLF would make every Windows run a false failure.
sys.stdout.reconfigure(newline="\n")

root = sys.argv[1]
token = re.compile(rb"(?:^|[^\w])unsafe(?:[^\w]|$)")
counts = {}

if os.path.isdir(root):
    for crate in sorted(os.listdir(root)):
        crate_dir = os.path.join(root, crate)
        if not os.path.isdir(crate_dir):
            continue
        n = 0
        for dirpath, _, files in os.walk(crate_dir):
            for f in files:
                if not f.endswith(".rs"):
                    continue
                with open(os.path.join(dirpath, f), "rb") as fh:
                    n += len(token.findall(fh.read()))
        if n:
            counts[crate] = n

json.dump(counts, sys.stdout, indent=2, sort_keys=True)
sys.stdout.write("\n")
PY
