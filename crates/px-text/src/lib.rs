#![forbid(unsafe_code)]
// Fonts are hostile input: the reader never panics. Tests may unwrap.
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic
    )
)]

//! Shaping, font fallback, bidi, line breaking, IME.
//!
//! Phase 9 unit 1 (this commit): the std-only font core. [`font::Font`] reads
//! the TrueType tables Ahem needs ([`sfnt`], [`cmap`], [`glyf`]) and
//! [`registry::FontRegistry`] resolves `font-family`. Shaping, bidi and line
//! breaking are separate units; see docs/phase-09-plan.md and this crate's
//! CLAUDE.md.

mod brotli_tables;
mod cff;
mod cmap;
pub mod font;
pub mod glyf;
pub mod registry;
pub mod sfnt;
pub mod sysfonts;
pub mod woff;
mod woff2;

// Phase 9 units 2, 6 and 7 (ADR 036).
pub mod bidi;
pub mod brotli;
pub mod linebreak;
pub mod shaper;

pub use font::{Font, FontId};
pub use glyf::{Outline, PathCmd, Pt};
pub use registry::FontRegistry;
pub use sfnt::FontError;
