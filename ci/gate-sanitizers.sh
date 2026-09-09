#!/usr/bin/env bash
#
# Sanitizers over the audited unsafe core (ADR 011, build-spec §4.5).
#
#   ASAN  px-sandbox, px-broker    Linux and Windows
#   TSAN  px-sandbox, px-broker    Linux only — TSAN has no Windows support
#
# ADR 011 amends ADR 006's nightly fence to admit this one extra consumer. The
# fence itself is untouched: the toolchain is named explicitly with +name, so
# the repository root still resolves to stable and ci/gate-fuzz-smoke.sh still
# asserts it, unmodified.
#
#   ci/gate-sanitizers.sh              run the sanitizers
#   ci/gate-sanitizers.sh --self-check prove ASAN can still detect a fault
#
# The self-check exists because a sanitizer that cannot detect anything is
# worse than no sanitizer: it converts an absence of evidence into apparent
# assurance. ADR 011's Verification section names that as the way this decision
# would be wrong, so the check is runnable rather than a claim in a document.

. "$(dirname "$0")/lib.sh"

# The one nightly this project has, from fuzz/rust-toolchain.toml. Read rather
# than duplicated: two pins that drift apart would be two nightlies, which is
# exactly what ADR 011 says it will not create.
NIGHTLY="$(sed -n 's/^channel *= *"\(.*\)"/\1/p' fuzz/rust-toolchain.toml)"
if [ -z "$NIGHTLY" ]; then
    fail "could not read the pinned nightly from fuzz/rust-toolchain.toml"
    verdict "sanitizers"
fi
info "pinned nightly: $NIGHTLY"

if ! rustup toolchain list 2>/dev/null | grep -q "^$NIGHTLY"; then
    fail "$NIGHTLY is not installed (rustup toolchain install $NIGHTLY)"
    verdict "sanitizers"
fi

# Sanitized artifacts must not be mistaken for shippable ones, and must not
# evict the ordinary build cache.
export CARGO_TARGET_DIR="target/sanitizers"

case "$(uname -s)" in
    MINGW* | MSYS* | CYGWIN*) HOST_OS="windows" ;;
    Linux) HOST_OS="linux" ;;
    *) HOST_OS="other" ;;
esac

# ---------------------------------------------------------------------------
# Windows: the ASAN runtime is a DLL that is not on PATH by default.
#
# Without it the test binary exits 0xc0000135 (STATUS_DLL_NOT_FOUND) before
# running a single test, which reads as a broken build rather than a missing
# environment variable. Found the first time this was run locally.
# ---------------------------------------------------------------------------
add_windows_asan_runtime_to_path() {
    local vswhere="/c/Program Files (x86)/Microsoft Visual Studio/Installer/vswhere.exe"
    if [ ! -x "$vswhere" ]; then
        fail "vswhere.exe not found; cannot locate the ASAN runtime"
        return 1
    fi

    local install
    install="$("$vswhere" -latest -products '*' -property installationPath 2>/dev/null | tr -d '\r')"
    if [ -z "$install" ]; then
        # BuildTools installs are not always reported by the default query.
        install="$("$vswhere" -latest -products '*' -all -prerelease -property installationPath 2>/dev/null | tr -d '\r' | head -1)"
    fi
    if [ -z "$install" ]; then
        fail "vswhere reported no Visual Studio installation"
        return 1
    fi

    local unix_install
    unix_install="$(printf '%s' "$install" | sed 's|\\|/|g; s|^\([A-Za-z]\):|/\L\1|')"

    local dll
    dll="$(find "$unix_install/VC/Tools/MSVC" -name 'clang_rt.asan_dynamic-x86_64.dll' 2>/dev/null | head -1)"
    if [ -z "$dll" ]; then
        fail "clang_rt.asan_dynamic-x86_64.dll not found under $unix_install"
        fail "  install the 'C++ AddressSanitizer' component"
        return 1
    fi

    info "ASAN runtime: $dll"
    PATH="$(dirname "$dll"):$PATH"
    export PATH
    return 0
}

# ---------------------------------------------------------------------------
# The self-check. Injects a heap overflow, asserts ASAN names it, reverts.
# ---------------------------------------------------------------------------
self_check() {
    local probe="crates/px-sandbox/src/sanitizer_self_check.rs"
    local lib="crates/px-sandbox/src/lib.rs"
    local backup
    backup="$(mktemp)"
    cp "$lib" "$backup"

    cat > "$probe" <<'PROBE'
//! Deliberate fault, compiled only by ci/gate-sanitizers.sh --self-check.
//!
//! Never committed with the module declaration active. The script writes this
//! file, appends the declaration, runs ASAN, and restores lib.rs.
#[test]
fn sanitizer_self_check_reads_past_the_end_of_a_heap_buffer() {
    let buffer: Vec<u8> = vec![0u8; 8];
    // SAFETY: none. This is the point — the read is out of bounds on purpose,
    // to prove the sanitizer is armed.
    let past_the_end = unsafe { *buffer.as_ptr().add(64) };
    println!("{past_the_end}");
}
PROBE
    printf '\n#[cfg(test)]\nmod sanitizer_self_check;\n' >> "$lib"

    info "running ASAN against a deliberate heap overflow"
    local out
    out="$(RUSTFLAGS="-Zsanitizer=address" cargo "+$NIGHTLY" test \
            -p px-sandbox --target "$SANITIZER_TARGET" \
            sanitizer_self_check 2>&1 || true)"

    cp "$backup" "$lib"
    rm -f "$probe" "$backup"

    if printf '%s' "$out" | grep -q "AddressSanitizer: heap-buffer-overflow"; then
        ok "ASAN detected the injected fault; the sanitizer is armed"
    else
        fail "ASAN did NOT detect an injected heap overflow"
        fail "  a green sanitizer that cannot detect a fault is worse than none"
        printf '%s\n' "$out" | tail -20 >&2
    fi
}

# ---------------------------------------------------------------------------

if [ "$HOST_OS" = "windows" ]; then
    SANITIZER_TARGET="x86_64-pc-windows-msvc"
    add_windows_asan_runtime_to_path || verdict "sanitizers"
elif [ "$HOST_OS" = "linux" ]; then
    SANITIZER_TARGET="x86_64-unknown-linux-gnu"
else
    fail "unsupported host for sanitizers: $(uname -s)"
    verdict "sanitizers"
fi
info "target: $SANITIZER_TARGET"

if [ "${1:-}" = "--self-check" ]; then
    self_check
    # verdict exits non-zero on failure and *returns* on success, so the
    # explicit exit is what stops a passing self-check falling through into the
    # full run below.
    verdict "sanitizers"
    exit 0
fi

# ASAN, both platforms. No -Z build-std: it is not needed for ASAN to find
# faults in this crate's own code, and it triples the build.
info "ASAN: px-sandbox, px-broker"
if RUSTFLAGS="-Zsanitizer=address" cargo "+$NIGHTLY" test \
        -p px-sandbox -p px-broker --target "$SANITIZER_TARGET"; then
    ok "ASAN clean"
else
    fail "ASAN reported a fault"
fi

# TSAN, Linux only — there is no Windows support. -Z build-std because TSAN
# reports on uninstrumented std otherwise, which is noise rather than findings.
if [ "$HOST_OS" = "linux" ]; then
    info "TSAN: px-sandbox, px-broker"
    if RUSTFLAGS="-Zsanitizer=thread" cargo "+$NIGHTLY" test \
            -Z build-std -p px-sandbox -p px-broker \
            --target "$SANITIZER_TARGET"; then
        ok "TSAN clean"
    else
        fail "TSAN reported a data race"
    fi
else
    info "TSAN skipped: no Windows support (ADR 011)"
fi

verdict "sanitizers"
