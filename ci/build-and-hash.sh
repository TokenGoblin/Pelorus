#!/usr/bin/env bash
#
# One half of gate check 2 — reproducible builds (invariant 7).
#
# Copies the working tree to a caller-chosen directory, builds it there, and
# writes SHA-256 of every release binary to stdout. The caller varies the
# directory, HOME, CARGO_HOME and — on Linux — the user between the two runs.
# The comparison is ci/compare-hashes.sh.
#
# Usage: ci/build-and-hash.sh <build-dir>

. "$(dirname "$0")/lib.sh"

DEST="${1:?usage: build-and-hash.sh <build-dir>}"

rm -rf "$DEST"
mkdir -p "$DEST"
# git archive, not cp: only tracked files, so a stray local file cannot make
# two builds differ for a reason that has nothing to do with the source.
git archive --format=tar HEAD | tar -x -C "$DEST"

(
    cd "$DEST"
    cargo build --workspace --locked --release >&2
)

shopt -s nullglob
found=0
for bin in "$DEST"/target/release/px-*; do
    case "$bin" in
        *.d|*.pdb|*.rlib|*.rmeta) continue ;;
    esac
    [ -f "$bin" ] || continue
    [ -x "$bin" ] || case "$bin" in *.exe) ;; *) continue ;; esac
    found=1
    printf '%s  %s\n' "$(sha256sum < "$bin" | cut -d' ' -f1)" "$(basename "$bin")"
done | sort -k2

if [ "$found" -eq 0 ]; then
    echo "FAIL no release binaries produced in $DEST/target/release" >&2
    exit 1
fi
