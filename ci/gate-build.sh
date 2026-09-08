#!/usr/bin/env bash
#
# Gate check 1 — the workspace builds on both platforms (Phase 0 gate).
#
# --locked, always: a build that silently updates Cargo.lock is not the build
# the reproducibility gate hashed.

. "$(dirname "$0")/lib.sh"

if ! command -v cargo >/dev/null 2>&1; then
    fail "cargo is not on PATH"
    verdict "build"
fi

if [ ! -f Cargo.toml ]; then
    fail "no workspace Cargo.toml"
    verdict "build"
fi
if [ ! -f Cargo.lock ]; then
    fail "no committed Cargo.lock — --locked cannot be honoured (build-spec §5)"
    verdict "build"
fi

info "cargo build --workspace --locked --release"
cargo build --workspace --locked --release || fail "release build failed"

info "cargo clippy --workspace --locked --all-targets -- -D warnings"
cargo clippy --workspace --locked --all-targets -- -D warnings || fail "clippy failed"

info "cargo fmt --all --check"
cargo fmt --all --check || fail "formatting is not canonical"

verdict "build"
