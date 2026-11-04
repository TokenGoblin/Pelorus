//! The gate registry. One line per gate; the module holds the logic.

pub mod all_rust;
pub mod brand_leak;
pub mod flex_grid;
pub mod fuzz_smoke;
pub mod layout;
pub mod make_fetch_corpus;
pub mod miri;
pub mod sanitizers;
pub mod structure;
pub mod supply_chain;
pub mod update_hsts;
pub mod update_psl;
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
        name: "flex-grid",
        about: "Phase 7: flexbox/grid reftest and crashtest suites, corpus floors, grid-lanes excluded",
        run: flex_grid::run,
    },
    GateSpec {
        name: "fuzz-smoke",
        about: "60 s of cargo-fuzz per target against the committed corpus; asserts the nightly fence (ADR 006)",
        run: fuzz_smoke::run,
    },
    GateSpec {
        name: "layout",
        about: "Phase 6: CSS2 reftest subset, box-tree determinism, deep nesting, Au source scans",
        run: layout::run,
    },
    GateSpec {
        name: "make-fetch-corpus",
        about: "TOOL: regenerate the synthetic response corpus in tests/net/corpus",
        run: make_fetch_corpus::run,
    },
    GateSpec {
        name: "miri",
        about: "Miri over px-dom's five ADR 022 suites on the pinned nightly",
        run: miri::run,
    },
    GateSpec {
        name: "sanitizers",
        about: "ASAN (+ TSAN on Linux) over px-sandbox and px-broker; --self-check proves ASAN can still fail (ADR 011)",
        run: sanitizers::run,
    },
    GateSpec {
        name: "structure",
        about: "crate CLAUDE.md, px-brand endpoint, exec bits, commit identity, fuzz corpus",
        run: structure::run,
    },
    GateSpec {
        name: "supply-chain",
        about: "cargo-deny, cargo-vet, cargo-auditable SBOM, and the unsafe baseline (build-spec §5, §4.5)",
        run: supply_chain::run,
    },
    GateSpec {
        name: "update-hsts",
        about: "TOOL: refresh data/hsts_preload.txt and its version file (--check, --from-file)",
        run: update_hsts::run,
    },
    GateSpec {
        name: "update-psl",
        about: "TOOL: refresh data/public_suffix_list.dat and its version file (--check, --from-file)",
        run: update_psl::run,
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
