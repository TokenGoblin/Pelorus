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

# Invariant 7: the binary must not contain the path it was built at or the name
# of the user who built it. Cargo's `trim-paths` profile option is still
# unstable, so remap explicitly here, where both paths are known. Both runs
# remap onto the SAME synthetic prefixes, which is what makes the hashes
# comparable across machines.
REMAP="--remap-path-prefix=$DEST=/src"
REMAP="$REMAP --remap-path-prefix=${CARGO_HOME:-$HOME/.cargo}=/cargo"
REMAP="$REMAP --remap-path-prefix=$HOME=/home"

# MSVC's linker stamps the PE header with the wall-clock time of the link, so
# two builds of identical source differ by construction. /Brepro replaces that
# timestamp with a hash of the content, which is what makes a Windows build
# reproducible at all. There is no equivalent to disable on ELF: the GNU and
# LLVM linkers are already deterministic.
HOST="$(rustc -vV | sed -n 's/^host: //p')"
case "$HOST" in
    *windows-msvc) REMAP="$REMAP -C link-arg=/Brepro" ;;
esac

(
    cd "$DEST"
    RUSTFLAGS="$REMAP" cargo build --workspace --locked --release >&2
)

shopt -s nullglob
hashes=""
for bin in "$DEST"/target/release/px-*; do
    case "$bin" in
        *.d | *.pdb | *.rlib | *.rmeta | *.exp | *.lib) continue ;;
    esac
    [ -f "$bin" ] || continue
    hashes="$hashes$(sha256sum < "$bin" | cut -d' ' -f1)  $(basename "$bin")
"
done

if [ -z "$hashes" ]; then
    echo "FAIL no release binaries produced in $DEST/target/release" >&2
    exit 1
fi

printf '%s' "$hashes" | sort -k2
