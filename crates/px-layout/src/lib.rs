#![forbid(unsafe_code)]

//! Box tree; block, inline, flex and grid.
//!
//! Phase 6 in progress. Done so far: `Au` geometry, the box tree, and block
//! layout of in-flow blocks with margin collapsing. Not yet: inline layout,
//! floats, positioned boxes, the px-css → [`style::BoxStyle`] conversion.
//! See docs/adr/029 and docs/build-spec.md §9.

pub mod au;
pub mod block;
pub mod style;
pub mod tree;

pub use au::Au;
pub use block::layout;
pub use style::{BoxStyle, Display, Edges, Length};
pub use tree::{BoxId, Geometry, LayoutTree};
