#![forbid(unsafe_code)]

//! Platform-independent accessibility tree (Phase 16).
//!
//! This crate turns a `px_dom::Arena` into an [`AccessibilityTree`]: roles,
//! accessible names and descriptions, states and properties, bounds, and a
//! navigable, diffable structure. It knows nothing about UI Automation or
//! AT-SPI. Those bridges are separate units that need OS bindings and real
//! hardware; they consume this crate's types: [`AccessibilityTree`] and
//! [`A11yNode`] for the live tree, [`diff`] / [`Change`] for the events they
//! must raise, and [`A11yUpdate`] for what crosses the process boundary.
//!
//! # Specifications
//!
//! * WAI-ARIA 1.2: the role set, name-from rules and states, in [`role`] and
//!   [`state`].
//! * HTML-AAM: implicit roles and per-element naming rules, in `doc` and
//!   `accname`.
//! * Accessible Name and Description Computation 1.2: `accname`.
//!
//! # Invariants
//!
//! * `forbid(unsafe_code)`; no dependency but `px-dom`.
//! * Panic-free on any DOM, including hostile attribute values. The crate
//!   carries the workspace `unwrap`/`expect`/`panic`/indexing lints.
//! * Handles are generational: [`A11yId`] belongs to one build and every
//!   accessor returns `Option`. See [`tree`].
//! * Nothing recurses over document depth: every traversal carries its own
//!   stack, so a 100,000-level document cannot overflow a 256 KB stack.
//! * Output is a pure function of its input. Identical input, identical
//!   [`AccessibilityTree::dump`].
//! * All sizes are bounded; see [`MAX_NODES`], [`MAX_STRING`],
//!   [`accname::MAX_NODES_PER_NAME`].
//!
//! # Not implemented
//!
//! See the crate `README` section in `CLAUDE.md`; in short: ARIA 1.3
//! additions, CSS generated content in names, shadow DOM and slots, SVG and
//! MathML internals, `aria-activedescendant`, `aria-controls`/`flowto` and
//! other relations beyond `labelledby`/`describedby`/`owns`, table
//! row/column indices, `aria-roledescription`, tree/grid structure
//! validation (required owned/context roles), `radio` grouping by `name`,
//! the exact "required context" repair rules, localised strings, and
//! incremental rebuild (every build is whole; [`diff`] is how changes are
//! found).

pub mod accname;
mod attr;
mod diff;
mod doc;
mod dump;
pub mod input;
pub mod role;
pub mod state;
mod tree;
mod wire;

pub use attr::MAX_STRING;
pub use diff::{Change, diff};
pub use dump::DumpOptions;
pub use input::{
    Bounds, BoundsSource, BuildInputs, BuildOptions, MAX_NAME_VISITS, MAX_NODES, StyleHints,
    StyleSource,
};
pub use role::{ABSTRACT_ROLES, NameFrom, Role, RoleError};
pub use state::{
    Current, HasPopup, Invalid, LiveRegion, Politeness, Props, RangeValue, Relevant, States, Tri,
};
pub use tree::{A11yId, A11yNode, AccessibilityTree, MAX_OWNS, MAX_TREE_DEPTH};
pub use wire::{
    A11yUpdate, MAX_UPDATE_NODES, MAX_UPDATE_PARTS, MAX_UPDATE_REMOVED, NodeKey, UpdateError,
    UpdateKind, WireNode, key_of,
};
