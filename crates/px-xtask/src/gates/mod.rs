//! The gate registry. One line per gate; the module holds the logic.

pub mod all;
pub mod all_rust;
pub mod brand_leak;
pub mod build;
pub mod build_and_hash;
pub mod check_build_python_pin;
pub mod compare_hashes;
pub mod compat_list;
pub mod count_lines;
pub mod dom;
pub mod flex_grid;
pub mod fuzz_smoke;
pub mod ipc;
pub mod layout;
pub mod make_fetch_corpus;
pub mod miri;
pub mod network;
pub mod not_serializable;
pub mod progress;
pub mod render;
pub mod sandbox;
pub mod sanitizers;
pub mod script;
pub mod setup_build_python;
pub mod structure;
pub mod style;
pub mod suites;
pub mod supply_chain;
pub mod test262;
pub mod unsafe_audit;
pub mod unsafe_headers;
pub mod update_hsts;
pub mod update_psl;
pub mod vendor_test262;
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
        name: "all",
        about: "run the pre-push gates in CI order (not the reproducible-build, layout, flex-grid, render, style or nightly ones)",
        run: all::run,
    },
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
        name: "compat-list",
        about: "the forty-site compat list: forty entries, each with assertions, no urls (build-spec §8)",
        run: compat_list::run,
    },
    GateSpec {
        name: "count-lines",
        about: "TOOL: regenerate the README size section (--check: fail if stale)",
        run: count_lines::run,
    },
    GateSpec {
        name: "dom",
        about: "Phase 4: html5lib corpus, stale-handle/nesting suites, px-dom source scans, fuzz targets",
        run: dom::run,
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
        name: "ipc",
        about: "Phase 1: hostile-identity/crash-restart/size-limit suites, content-release build, px-browser end to end, wire identity and ChannelId scans",
        run: ipc::run,
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
        name: "network",
        about: "Phase 3: fetch/PSL/connection/partition suites, 200-response corpus, network confined to px-net and px-update, PSL versioned, HTTP framing fuzz targets",
        run: network::run,
    },
    GateSpec {
        name: "not-serializable",
        about: "TOOL: assert a type is not serialisable: not-serializable <TypeName> <file>... (invariant 9)",
        run: not_serializable::run,
    },
    GateSpec {
        name: "progress",
        about: "fail if the README progress chart is stale against docs/progress.txt (--write regenerates it)",
        run: progress::run,
    },
    GateSpec {
        name: "render",
        about: "Phase 8: 25 pages rendered headless and compared exactly to committed PNG baselines (--bless rewrites them, refused in CI; --select prints the WPT corpus selection)",
        run: render::run,
    },
    GateSpec {
        name: "sandbox",
        about: "Phase 2: policy/refusal/ladder suites, px-browser refuses below the floor, no test-only override in release binaries, ladder documented",
        run: sandbox::run,
    },
    GateSpec {
        name: "sanitizers",
        about: "ASAN (+ TSAN on Linux) over px-sandbox and px-broker; --self-check proves ASAN can still fail (ADR 011)",
        run: sanitizers::run,
    },
    GateSpec {
        name: "script",
        about: "Phase 10: script host suites (eval over a real process, wire hostility, confinement, ordering, queue bounds, clock); lists unit 4 to 7 items as remaining",
        run: script::run,
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
        name: "style",
        about: "Phase 5: computed-style/cascade suites, property-set floor, stylo decision ADR, ADR 021 tripwire, px-css unsafe rule",
        run: style::run,
    },
    GateSpec {
        name: "supply-chain",
        about: "cargo-deny, cargo-vet, cargo-auditable SBOM, and the unsafe baseline (build-spec §5, §4.5)",
        run: supply_chain::run,
    },
    GateSpec {
        name: "test262",
        about: "Phase 10: run the vendored test262 subset against the engine under a ratchet (--engine, --update, --filter; not in `all`)",
        run: test262::run,
    },
    GateSpec {
        name: "unsafe-audit",
        about: "dependency-tree unsafe-token count against ci/unsafe-baseline.json (--write regenerates it)",
        run: unsafe_audit::run,
    },
    GateSpec {
        name: "unsafe-headers",
        about: "gate check 4: per-crate unsafe header rules, local waiver scans, px-css zero-unsafe counts",
        run: unsafe_headers::run,
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
        name: "vendor-test262",
        about: "TOOL: vendor the rule-selected test262 subset into tests/test262 (--rev, --from-checkout, --target-bytes)",
        run: vendor_test262::run,
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
