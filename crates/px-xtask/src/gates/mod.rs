//! The gate registry. One line per gate; the module holds the logic.

pub mod all_rust;
pub mod brand_leak;
pub mod build;
pub mod build_and_hash;
pub mod check_build_python_pin;
pub mod compare_hashes;
pub mod flex_grid;
pub mod layout;
pub mod setup_build_python;
pub mod structure;
pub mod vendor_wpt;

use crate::Gate;
use std::path::Path;

/// A gate: reports through `Gate`, never exits the process itself.
pub type GateFn = fn(&mut Gate, &Path, &[String]);

pub struct GateSpec {
    pub name: &'static str,
    pub about: &'static str,
    pub run: GateFn,
}

/// Registered gates. Keep alphabetical. A gate ported from a script keeps the
/// script's gate name (the word it passed to `verdict`).
pub static GATES: &[GateSpec] = &[
    GateSpec {
        name: "all-rust",
        about: "fail on first-party non-Rust source outside the shrinking allowlist (ADR 032)",
        run: all_rust::run,
    },
    GateSpec {
        name: "brand-leak",
        about: "the product name appears only where tests/brand/allowlist.txt permits (build-spec §2.2)",
        run: brand_leak::run,
    },
    GateSpec {
        name: "build",
        about: "Phase 0: release build, clippy -D warnings and fmt --check, all --locked",
        run: build::run,
    },
    GateSpec {
        name: "build-and-hash",
        about: "TOOL: reproducible build in a scratch dir; prints the SHA-256 manifest of the release binaries (invariant 7)",
        run: build_and_hash::run,
    },
    GateSpec {
        name: "check-build-python-pin",
        about: "TOOL: a Python env holds exactly the pins in build-python/requirements.txt (ADR 023)",
        run: check_build_python_pin::run,
    },
    GateSpec {
        name: "compare-hashes",
        about: "reproducible builds: two hash manifests (from build-and-hash) must be identical (invariant 7)",
        run: compare_hashes::run,
    },
    GateSpec {
        name: "flex-grid",
        about: "Phase 7: flexbox/grid reftest and crashtest suites, corpus floors, grid-lanes excluded",
        run: flex_grid::run,
    },
    GateSpec {
        name: "layout",
        about: "Phase 6: CSS2 reftest subset, box-tree determinism, deep nesting, Au source scans",
        run: layout::run,
    },
    GateSpec {
        name: "setup-build-python",
        about: "TOOL: build the pinned Python venv stylo's build.rs needs; prints the interpreter path (ADR 023)",
        run: setup_build_python::run,
    },
    GateSpec {
        name: "structure",
        about: "crate CLAUDE.md, px-brand endpoint, exec bits, commit identity, fuzz corpus",
        run: structure::run,
    },
    GateSpec {
        name: "vendor-wpt",
        about: "TOOL: vendor a WPT subset into tests/wpt (CSS2, or --module css-flexbox|css-grid)",
        run: vendor_wpt::run,
    },
];

pub fn find(name: &str) -> Option<&'static GateSpec> {
    GATES.iter().find(|g| g.name == name)
}
