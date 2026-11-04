//! The gate registry. One line per gate; the module holds the logic.

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
pub static GATES: &[GateSpec] = &[];

pub fn find(name: &str) -> Option<&'static GateSpec> {
    GATES.iter().find(|g| g.name == name)
}
