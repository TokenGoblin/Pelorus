//! The backend trait (ADR 033 (d)): a display list and a size in, a bitmap out.
//!
//! Two implementations are planned and exactly one exists. [`SoftwareRasterizer`]
//! wraps `px_paint::rasterize` and is what makes CI rendering deterministic and
//! what every failure of any other backend falls back to. A wgpu backend would
//! be a second implementation of the same trait, by its own ADR (ADR 034).
//!
//! The session that supervises a GPU *process* is not a backend and owns no
//! pixels; see [`crate::session`].

use std::fmt;

use px_paint::raster::RasterError;
use px_paint::{Canvas, DisplayList};

/// Why a backend produced no frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderError {
    /// The input is unrenderable by any backend (empty or oversized viewport,
    /// unbalanced clips). Retrying elsewhere gives the same answer.
    Input(RasterError),
    /// This backend failed for a reason of its own. Another backend may well
    /// succeed.
    Backend(&'static str),
}

impl fmt::Display for RenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Input(error) => write!(f, "unrenderable input: {error}"),
            Self::Backend(why) => write!(f, "backend failure: {why}"),
        }
    }
}

impl std::error::Error for RenderError {}

impl From<RasterError> for RenderError {
    fn from(error: RasterError) -> Self {
        Self::Input(error)
    }
}

/// Something that turns a display list into pixels.
pub trait Rasterizer {
    /// Rasterize `list` onto a fresh white `width` by `height` canvas.
    fn rasterize(
        &mut self,
        list: &DisplayList,
        width: u32,
        height: u32,
    ) -> Result<Canvas, RenderError>;

    /// A short name for logs.
    fn name(&self) -> &'static str;
}

/// The deterministic software backend.
#[derive(Clone, Copy, Debug, Default)]
pub struct SoftwareRasterizer;

impl Rasterizer for SoftwareRasterizer {
    fn rasterize(
        &mut self,
        list: &DisplayList,
        width: u32,
        height: u32,
    ) -> Result<Canvas, RenderError> {
        Ok(px_paint::rasterize(list, width, height)?)
    }

    fn name(&self) -> &'static str {
        "software"
    }
}

/// A backend that fails when told to, so the fallback path is exercised
/// without a process (ADR 033 unit 4). After its configured number of
/// successes it fails every call, as a lost device would.
#[derive(Clone, Debug)]
pub struct FailingRasterizer {
    succeed_first: u32,
    calls: u32,
}

impl FailingRasterizer {
    /// Fails from the first call.
    pub fn always() -> Self {
        Self::after(0)
    }

    /// Renders `successes` frames in software, then fails every call.
    pub fn after(successes: u32) -> Self {
        Self {
            succeed_first: successes,
            calls: 0,
        }
    }
}

impl Rasterizer for FailingRasterizer {
    fn rasterize(
        &mut self,
        list: &DisplayList,
        width: u32,
        height: u32,
    ) -> Result<Canvas, RenderError> {
        self.calls = self.calls.saturating_add(1);
        if self.calls > self.succeed_first {
            return Err(RenderError::Backend("told to fail"));
        }
        SoftwareRasterizer.rasterize(list, width, height)
    }

    fn name(&self) -> &'static str {
        "failing"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use px_layout::geom::px;
    use px_paint::{Color, Item, Rect};

    fn list() -> DisplayList {
        let mut list = DisplayList::new();
        list.push(Item::FillRect {
            rect: Rect {
                x: px(1),
                y: px(1),
                width: px(2),
                height: px(2),
            },
            color: Color {
                r: 10,
                g: 20,
                b: 30,
                a: 255,
            },
        });
        list
    }

    #[test]
    fn the_software_backend_is_px_paint_rasterize() {
        let expected = px_paint::rasterize(&list(), 4, 4).expect("raster");
        let got = SoftwareRasterizer.rasterize(&list(), 4, 4).expect("raster");
        assert_eq!(got, expected);
    }

    #[test]
    fn unrenderable_input_is_reported_as_input_not_as_a_backend_fault() {
        assert_eq!(
            SoftwareRasterizer.rasterize(&list(), 0, 4),
            Err(RenderError::Input(RasterError::EmptyViewport))
        );
    }

    #[test]
    fn the_test_backend_fails_on_command() {
        let mut backend = FailingRasterizer::after(1);
        assert!(backend.rasterize(&list(), 4, 4).is_ok());
        assert!(matches!(
            backend.rasterize(&list(), 4, 4),
            Err(RenderError::Backend(_))
        ));
        assert!(
            FailingRasterizer::always()
                .rasterize(&list(), 4, 4)
                .is_err()
        );
    }
}
