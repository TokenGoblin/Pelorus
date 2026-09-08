#!/usr/bin/env bash
#
# Gate check 3 — supply chain (build-spec §5, §4.5).
#
#   cargo deny       licence and advisory policy
#   cargo vet        audit coverage, importing Mozilla's and Google's sets
#   cargo auditable  SBOM embedded in the shipped artifact, asserted present
#   unsafe audit     dependency-tree unsafe count against a committed baseline
#
# HONEST NOTE, true for Phase 0 only: the workspace has zero third-party
# dependencies, so deny, vet and the unsafe baseline are all trivially green.
# They prove the harness runs. They prove nothing about any dependency until
# Phase 3 brings the first one in. That is the right time to wire them and the
# wrong time to read a pass as assurance.

. "$(dirname "$0")/lib.sh"

need() {
    command -v "$1" >/dev/null 2>&1 && return 0
    fail "$1 is not installed ($2)"
    return 1
}

if need cargo-deny "cargo install --locked cargo-deny"; then
    info "cargo deny check"
    cargo deny check || fail "cargo deny found policy violations"
fi

if need cargo-vet "cargo install --locked cargo-vet"; then
    info "cargo vet --locked"
    cargo vet --locked || fail "cargo vet found unaudited dependencies"
fi

if need cargo-auditable "cargo install --locked cargo-auditable"; then
    # Into a separate target directory, always. Cargo will not re-link a
    # binary that is already up to date, so if a plain `cargo build` ran first
    # the auditable build is a no-op and the SBOM assertion below fails on a
    # stale artifact for a reason that has nothing to do with the SBOM.
    AUDITABLE_DIR="target/auditable"
    info "cargo auditable build --release (into $AUDITABLE_DIR)"
    CARGO_TARGET_DIR="$AUDITABLE_DIR"         cargo auditable build --workspace --locked --release         || fail "auditable build failed"

    # Assert the SBOM reached the artifact. A flag that was passed is not
    # evidence; a section in the binary is. cargo-auditable writes zlib-
    # compressed JSON into a section named .dep-v0, so the section name is
    # what is readable, not the contents.
    shopt -s nullglob
    checked=0
    for stem in px-browser px-content; do
        bin=""
        for candidate in "$AUDITABLE_DIR/release/$stem.exe" "$AUDITABLE_DIR/release/$stem"; do
            if [ -f "$candidate" ]; then bin="$candidate"; break; fi
        done
        [ -n "$bin" ] || { fail "$stem was not built"; continue; }
        checked=$((checked + 1))
        if grep -a -q 'dep-v0' "$bin"; then
            ok "$(basename "$bin") carries an embedded SBOM"
        else
            fail "$(basename "$bin") has no .dep-v0 SBOM section"
        fi
    done
    [ "$checked" -gt 0 ] || fail "no release binaries found to check for an SBOM"
fi

BASELINE="ci/unsafe-baseline.json"
if [ ! -f "$BASELINE" ]; then
    fail "$BASELINE is missing — there is no committed unsafe baseline to compare against"
else
    info "unsafe audit against $BASELINE"
    current="$(mktemp)"
    if bash ci/unsafe-audit.sh > "$current"; then
        "$PY_BIN" - "$BASELINE" "$current" <<'PY' || fail "dependency-tree unsafe count increased"
import json, sys
base = json.load(open(sys.argv[1]))
cur = json.load(open(sys.argv[2]))
bad = False
for crate, n in sorted(cur.items()):
    was = base.get(crate)
    if was is None:
        print(f"FAIL new crate with unsafe: {crate} ({n})", file=sys.stderr)
        bad = True
    elif n > was:
        print(f"FAIL {crate} unsafe count {was} -> {n}", file=sys.stderr)
        bad = True
for crate, n in sorted(base.items()):
    if crate not in cur:
        print(f"note baseline crate no longer present: {crate} ({n}) — "
              f"regenerate the baseline", file=sys.stderr)
    elif cur[crate] < n:
        print(f"note {crate} unsafe count fell {n} -> {cur[crate]} — "
              f"regenerate the baseline", file=sys.stderr)
sys.exit(1 if bad else 0)
PY
    else
        fail "unsafe audit could not run"
    fi
    rm -f "$current"
fi

verdict "supply-chain"
