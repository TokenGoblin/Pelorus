//! The gate registry. One line per gate; the module holds the logic.

pub mod all_rust;
pub mod brand_leak;
pub mod compat_list;
pub mod flex_grid;
pub mod layout;
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
        name: "compat-list",
        about: "the forty-site compat list: forty entries, each with assertions, no urls (build-spec §8)",
        run: compat_list::run,
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
