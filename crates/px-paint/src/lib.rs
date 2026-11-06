#![forbid(unsafe_code)]

//! Display list construction (Phase 8).
//!
//! [`display_list`] defines the flat, ordered list of paint operations that sits
//! between layout and any rasterizer. Building one from a laid-out document, the
//! software rasterizer and the PNG encoder are separate modules added in their own
//! commits; see docs/build-spec.md section 9 and this crate's CLAUDE.md.

pub mod build;
pub mod display_list;
pub mod glyph;
pub mod image;
pub mod imgpaint;
pub mod png;
pub mod raster;
pub mod round;
pub mod svg;

pub use build::{build, build_with_images, build_with_viewport};
pub use display_list::{
    BorderSide, BorderStyle, Color, Corner, CornerShape, DisplayList, FontId, ImageData,
    ImageFilter, Item, ListError, PositionedGlyph, Rect, RoundedRect,
};
pub use imgpaint::{ImageSet, Vector};
pub use png::{decode, encode};
pub use raster::{Canvas, rasterize};
