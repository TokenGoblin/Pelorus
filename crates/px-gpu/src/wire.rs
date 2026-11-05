//! Between the in-process display list and what crosses the process boundary,
//! and the frame's trip back in strips.
//!
//! Everything here is a pure function or a small state machine over untrusted
//! input with `Result` on every path, so it is tested without a process. The
//! two ends of the boundary use the same code: the GPU process decodes display
//! items and encodes strips with it; the broker's session encodes items and
//! *reassembles and size-checks* strips with it.
//!
//! # The receiver does the arithmetic
//!
//! How many rows a strip carries is a function of the frame's width alone
//! ([`rows_per_strip`]), and the receiver computes it. A sender that sends a
//! different split is malformed, however plausible its pixels look. That is
//! what makes "reassembled and size-checked" mean something: there is exactly
//! one acceptable sequence of strips for a given frame, and the assembler
//! accepts it and nothing else.

use px_ipc::gpu::{
    MAX_FRAME_DIMENSION, MAX_FRAME_PIXELS, MAX_STRIP_BYTES, WireBorderSide, WireBorderStyle,
    WireColor, WireCorner, WireGlyph, WireItem, WireRect, WireRoundedRect,
};
use px_layout::geom::{Au, au};
use px_paint::{
    BorderSide, BorderStyle, Canvas, Color, Corner, CornerShape, FontId, Item, PositionedGlyph,
    Rect, RoundedRect,
};

/// A wire value that is not a display item layout could have produced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BadCoordinate;

fn to_au(value: i32) -> Result<Au, BadCoordinate> {
    // `au` clamps to the range layout works in. A value it had to change was
    // never in range, and is refused rather than quietly altered: the two ends
    // of the boundary must draw the same thing.
    let converted = au(value);
    if converted.0 == value {
        Ok(converted)
    } else {
        Err(BadCoordinate)
    }
}

fn rect_to_wire(rect: &Rect) -> WireRect {
    WireRect {
        x: rect.x.0,
        y: rect.y.0,
        width: rect.width.0,
        height: rect.height.0,
    }
}

fn rect_from_wire(rect: &WireRect) -> Result<Rect, BadCoordinate> {
    Ok(Rect {
        x: to_au(rect.x)?,
        y: to_au(rect.y)?,
        width: to_au(rect.width)?,
        height: to_au(rect.height)?,
    })
}

fn color_to_wire(color: Color) -> WireColor {
    WireColor {
        r: color.r,
        g: color.g,
        b: color.b,
        a: color.a,
    }
}

fn color_from_wire(color: WireColor) -> Color {
    Color {
        r: color.r,
        g: color.g,
        b: color.b,
        a: color.a,
    }
}

fn side_to_wire(side: &BorderSide) -> WireBorderSide {
    WireBorderSide {
        width: side.width.0,
        style: match side.style {
            BorderStyle::None => WireBorderStyle::None,
            BorderStyle::Solid => WireBorderStyle::Solid,
        },
        color: color_to_wire(side.color),
    }
}

fn side_from_wire(side: &WireBorderSide) -> Result<BorderSide, BadCoordinate> {
    Ok(BorderSide {
        width: to_au(side.width)?,
        style: match side.style {
            WireBorderStyle::None => BorderStyle::None,
            WireBorderStyle::Solid => BorderStyle::Solid,
        },
        color: color_from_wire(side.color),
    })
}

fn shape_to_wire(shape: &RoundedRect) -> WireRoundedRect {
    WireRoundedRect {
        rect: rect_to_wire(&shape.rect),
        corners: shape.corners.map(|c| {
            let (concave, n16) = match c.shape {
                CornerShape::Convex(n) => (false, n),
                CornerShape::Concave(n) => (true, n),
            };
            WireCorner {
                rx: c.rx.0,
                ry: c.ry.0,
                concave,
                n16,
            }
        }),
    }
}

fn shape_from_wire(shape: &WireRoundedRect) -> Result<RoundedRect, BadCoordinate> {
    let corner = |c: &WireCorner| -> Result<Corner, BadCoordinate> {
        Ok(Corner {
            rx: to_au(c.rx)?,
            ry: to_au(c.ry)?,
            shape: if c.concave {
                CornerShape::Concave(c.n16)
            } else {
                CornerShape::Convex(c.n16)
            },
        })
    };
    Ok(RoundedRect {
        rect: rect_from_wire(&shape.rect)?,
        corners: [
            corner(&shape.corners[0])?,
            corner(&shape.corners[1])?,
            corner(&shape.corners[2])?,
            corner(&shape.corners[3])?,
        ],
    })
}

/// A display item as it crosses the boundary.
pub fn item_to_wire(item: &Item) -> WireItem {
    match item {
        Item::FillRect { rect, color } => WireItem::FillRect {
            rect: rect_to_wire(rect),
            color: color_to_wire(*color),
        },
        Item::Border { rect, sides } => WireItem::Border {
            rect: rect_to_wire(rect),
            sides: [
                side_to_wire(&sides[0]),
                side_to_wire(&sides[1]),
                side_to_wire(&sides[2]),
                side_to_wire(&sides[3]),
            ],
        },
        Item::TextRun { rect, color } => WireItem::TextRun {
            rect: rect_to_wire(rect),
            color: color_to_wire(*color),
        },
        Item::PushClip { rect } => WireItem::PushClip {
            rect: rect_to_wire(rect),
        },
        Item::PopClip => WireItem::PopClip,
        Item::FillRoundedRect { shape, color } => WireItem::FillRoundedRect {
            shape: shape_to_wire(shape),
            color: color_to_wire(*color),
        },
        Item::RoundedBorder { shape, sides } => WireItem::RoundedBorder {
            shape: shape_to_wire(shape),
            sides: [
                side_to_wire(&sides[0]),
                side_to_wire(&sides[1]),
                side_to_wire(&sides[2]),
                side_to_wire(&sides[3]),
            ],
        },
        Item::PushRoundedClip { shape } => WireItem::PushRoundedClip {
            shape: shape_to_wire(shape),
        },
        Item::GlyphRun {
            font,
            size,
            color,
            start,
            len,
        } => WireItem::GlyphRun {
            font: font.0,
            size: size.0,
            color: color_to_wire(*color),
            start: *start,
            len: *len,
        },
    }
}

/// A glyph of the arena as it crosses the boundary.
pub fn glyph_to_wire(glyph: &PositionedGlyph) -> WireGlyph {
    WireGlyph {
        glyph: glyph.glyph_id,
        x: glyph.x.0,
        y: glyph.y.0,
    }
}

/// A glyph from the wire, or why it is refused.
pub fn glyph_from_wire(glyph: &WireGlyph) -> Result<PositionedGlyph, BadCoordinate> {
    Ok(PositionedGlyph {
        glyph_id: glyph.glyph,
        x: to_au(glyph.x)?,
        y: to_au(glyph.y)?,
    })
}

/// A display item from the wire, or why it is refused.
pub fn item_from_wire(item: &WireItem) -> Result<Item, BadCoordinate> {
    Ok(match item {
        WireItem::FillRect { rect, color } => Item::FillRect {
            rect: rect_from_wire(rect)?,
            color: color_from_wire(*color),
        },
        WireItem::Border { rect, sides } => Item::Border {
            rect: rect_from_wire(rect)?,
            sides: [
                side_from_wire(&sides[0])?,
                side_from_wire(&sides[1])?,
                side_from_wire(&sides[2])?,
                side_from_wire(&sides[3])?,
            ],
        },
        WireItem::TextRun { rect, color } => Item::TextRun {
            rect: rect_from_wire(rect)?,
            color: color_from_wire(*color),
        },
        WireItem::PushClip { rect } => Item::PushClip {
            rect: rect_from_wire(rect)?,
        },
        WireItem::PopClip => Item::PopClip,
        WireItem::FillRoundedRect { shape, color } => Item::FillRoundedRect {
            shape: shape_from_wire(shape)?,
            color: color_from_wire(*color),
        },
        WireItem::RoundedBorder { shape, sides } => Item::RoundedBorder {
            shape: shape_from_wire(shape)?,
            sides: [
                side_from_wire(&sides[0])?,
                side_from_wire(&sides[1])?,
                side_from_wire(&sides[2])?,
                side_from_wire(&sides[3])?,
            ],
        },
        WireItem::PushRoundedClip { shape } => Item::PushRoundedClip {
            shape: shape_from_wire(shape)?,
        },
        WireItem::GlyphRun {
            font,
            size,
            color,
            start,
            len,
        } => Item::GlyphRun {
            font: FontId(*font),
            size: to_au(*size)?,
            color: color_from_wire(*color),
            start: *start,
            len: *len,
        },
    })
}

/// Whether a viewport is one the protocol will carry.
pub fn viewport_ok(width: u32, height: u32) -> bool {
    width > 0
        && height > 0
        && width <= MAX_FRAME_DIMENSION
        && height <= MAX_FRAME_DIMENSION
        && u64::from(width) * u64::from(height) <= MAX_FRAME_PIXELS
}

/// Rows in every strip but possibly the last, for a frame `width` pixels wide:
/// as many as fit in [`MAX_STRIP_BYTES`], and at least one. `None` for a width
/// the protocol will not carry.
pub fn rows_per_strip(width: u32) -> Option<u32> {
    if width == 0 || width > MAX_FRAME_DIMENSION {
        return None;
    }
    let row_bytes = usize::try_from(width).ok()?.checked_mul(4)?;
    let rows = MAX_STRIP_BYTES.checked_div(row_bytes)?;
    // A row wider than a strip cannot be sent at all.
    if rows == 0 {
        return None;
    }
    u32::try_from(rows).ok()
}

/// A canvas as the strips that carry it: `(first_row, rows, pixels)`.
///
/// Empty for a canvas the protocol will not carry or whose buffer is not the
/// size its dimensions say; a sender never produces an inconsistent strip.
pub fn strips_of(canvas: &Canvas) -> Vec<(u32, u32, &[u8])> {
    let mut strips = Vec::new();
    let Some(per) = rows_per_strip(canvas.width) else {
        return strips;
    };
    let Some(row_bytes) = usize::try_from(canvas.width)
        .ok()
        .and_then(|w| w.checked_mul(4))
    else {
        return strips;
    };
    let mut first = 0u32;
    while first < canvas.height {
        let rows = per.min(canvas.height - first);
        let start = usize::try_from(first)
            .ok()
            .and_then(|f| f.checked_mul(row_bytes));
        let len = usize::try_from(rows)
            .ok()
            .and_then(|r| r.checked_mul(row_bytes));
        let slice = start
            .zip(len)
            .and_then(|(start, len)| canvas.pixels.get(start..start.checked_add(len)?));
        match slice {
            Some(pixels) => strips.push((first, rows, pixels)),
            None => return Vec::new(),
        }
        first += rows;
    }
    strips
}

/// Why a strip was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StripError {
    /// The frame is a size the protocol will not carry.
    BadFrame,
    /// The strip does not start where the next one must.
    OutOfOrder,
    /// The strip's row count is not what the receiver's arithmetic says.
    WrongRows,
    /// The strip's byte length is not `rows * width * 4`.
    WrongLength,
    /// More strips than the frame has rows for.
    Surplus,
    /// The frame ended before all its rows arrived.
    Incomplete,
}

/// Reassembles a frame from strips, accepting only the one valid sequence.
///
/// Memory is taken as strips arrive, never from a declared size: a sender that
/// promises a large frame and stops costs what it actually sent.
#[derive(Debug)]
pub struct FrameAssembler {
    width: u32,
    height: u32,
    per_strip: u32,
    next_row: u32,
    pixels: Vec<u8>,
}

impl FrameAssembler {
    /// Begin assembling a `width` by `height` frame.
    pub fn new(width: u32, height: u32) -> Result<Self, StripError> {
        if !viewport_ok(width, height) {
            return Err(StripError::BadFrame);
        }
        let per_strip = rows_per_strip(width).ok_or(StripError::BadFrame)?;
        Ok(Self {
            width,
            height,
            per_strip,
            next_row: 0,
            pixels: Vec::new(),
        })
    }

    /// The `(first_row, rows)` the next strip must carry, or `None` when the
    /// frame is complete.
    pub fn expected(&self) -> Option<(u32, u32)> {
        if self.next_row >= self.height {
            return None;
        }
        Some((
            self.next_row,
            self.per_strip.min(self.height - self.next_row),
        ))
    }

    /// Accept one strip.
    pub fn push(&mut self, first_row: u32, rows: u32, pixels: &[u8]) -> Result<(), StripError> {
        let (want_first, want_rows) = self.expected().ok_or(StripError::Surplus)?;
        if first_row != want_first {
            return Err(StripError::OutOfOrder);
        }
        if rows != want_rows {
            return Err(StripError::WrongRows);
        }
        let want_len = usize::try_from(rows)
            .ok()
            .zip(usize::try_from(self.width).ok())
            .and_then(|(r, w)| r.checked_mul(w)?.checked_mul(4))
            .ok_or(StripError::WrongLength)?;
        if pixels.len() != want_len {
            return Err(StripError::WrongLength);
        }
        self.pixels.extend_from_slice(pixels);
        self.next_row += rows;
        Ok(())
    }

    /// The finished canvas, or [`StripError::Incomplete`].
    pub fn finish(self) -> Result<Canvas, StripError> {
        if self.next_row != self.height {
            return Err(StripError::Incomplete);
        }
        Ok(Canvas {
            width: self.width,
            height: self.height,
            pixels: self.pixels,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use px_layout::geom::px;

    #[test]
    fn every_item_shape_survives_the_trip() {
        let rect = Rect {
            x: px(-3),
            y: au(7),
            width: px(40),
            height: px(9),
        };
        let color = Color {
            r: 1,
            g: 2,
            b: 3,
            a: 4,
        };
        let side = BorderSide {
            width: px(2),
            style: BorderStyle::Solid,
            color,
        };
        let corner = |rx: i32, shape| Corner {
            rx: px(rx),
            ry: au(7 * rx),
            shape,
        };
        let shape = RoundedRect {
            rect,
            corners: [
                corner(1, CornerShape::ROUND),
                corner(2, CornerShape::SQUIRCLE),
                corner(3, CornerShape::SCOOP),
                corner(4, CornerShape::NOTCH),
            ],
        };
        let items = [
            Item::FillRect { rect, color },
            Item::Border {
                rect,
                sides: [
                    side,
                    BorderSide {
                        style: BorderStyle::None,
                        ..side
                    },
                    side,
                    side,
                ],
            },
            Item::TextRun { rect, color },
            Item::PushClip { rect },
            Item::PopClip,
            Item::FillRoundedRect { shape, color },
            Item::RoundedBorder {
                shape,
                sides: [side; 4],
            },
            Item::PushRoundedClip { shape },
            Item::GlyphRun {
                font: FontId(u32::MAX),
                size: px(16),
                color,
                start: 5,
                len: 9,
            },
        ];
        for item in items {
            assert_eq!(item_from_wire(&item_to_wire(&item)), Ok(item));
        }
        let glyph = PositionedGlyph {
            glyph_id: 70_000,
            x: px(-3),
            y: au(7),
        };
        assert_eq!(glyph_from_wire(&glyph_to_wire(&glyph)), Ok(glyph));
    }

    #[test]
    fn a_corner_radius_outside_layouts_range_is_refused() {
        let wire_corner = WireCorner {
            rx: i32::MAX,
            ry: 0,
            concave: false,
            n16: 32,
        };
        let item = WireItem::PushRoundedClip {
            shape: WireRoundedRect {
                rect: WireRect {
                    x: 0,
                    y: 0,
                    width: 60,
                    height: 60,
                },
                corners: [wire_corner; 4],
            },
        };
        assert_eq!(item_from_wire(&item), Err(BadCoordinate));
    }

    #[test]
    fn glyph_and_run_coordinates_outside_layouts_range_are_refused() {
        let bad = WireGlyph {
            glyph: 1,
            x: i32::MAX,
            y: 0,
        };
        assert_eq!(glyph_from_wire(&bad), Err(BadCoordinate));
        let run = WireItem::GlyphRun {
            font: 0,
            size: i32::MIN,
            color: WireColor {
                r: 0,
                g: 0,
                b: 0,
                a: 0,
            },
            start: 0,
            len: 0,
        };
        assert_eq!(item_from_wire(&run), Err(BadCoordinate));
    }

    #[test]
    fn the_two_glyph_arena_caps_agree() {
        assert_eq!(
            px_ipc::gpu::MAX_DISPLAY_GLYPHS as usize,
            px_paint::display_list::MAX_GLYPHS
        );
    }

    #[test]
    fn a_coordinate_outside_layouts_range_is_refused_not_clamped() {
        for bad in [i32::MAX, i32::MIN, 1 << 30, -(1 << 30) - 1] {
            let item = WireItem::PushClip {
                rect: WireRect {
                    x: bad,
                    y: 0,
                    width: 1,
                    height: 1,
                },
            };
            assert_eq!(item_from_wire(&item), Err(BadCoordinate), "{bad}");
        }
        // The extreme in-range values are fine.
        let edge = (1 << 30) - 1;
        let item = WireItem::PushClip {
            rect: WireRect {
                x: edge,
                y: -edge,
                width: 0,
                height: 0,
            },
        };
        assert!(item_from_wire(&item).is_ok());
    }

    #[test]
    fn strips_are_within_the_cap_and_cover_the_frame_exactly() {
        for (width, height) in [(1u32, 1u32), (800, 600), (16384, 3), (1, 5000), (333, 777)] {
            let canvas = Canvas {
                width,
                height,
                pixels: vec![9; width as usize * height as usize * 4],
            };
            let strips = strips_of(&canvas);
            let mut next = 0;
            for (first, rows, pixels) in &strips {
                assert_eq!(*first, next);
                assert!(pixels.len() <= MAX_STRIP_BYTES);
                assert_eq!(pixels.len(), *rows as usize * width as usize * 4);
                next += rows;
            }
            assert_eq!(next, height, "{width}x{height}");
        }
    }

    #[test]
    fn reassembly_reproduces_the_canvas() {
        let mut canvas = Canvas {
            width: 300,
            height: 901,
            pixels: Vec::new(),
        };
        canvas.pixels = (0..300 * 901 * 4).map(|i| (i % 251) as u8).collect();
        let mut assembler = FrameAssembler::new(300, 901).expect("assembler");
        for (first, rows, pixels) in strips_of(&canvas) {
            assembler.push(first, rows, pixels).expect("strip");
        }
        assert_eq!(assembler.expected(), None);
        assert_eq!(assembler.finish().expect("frame"), canvas);
    }

    #[test]
    fn the_assembler_refuses_everything_but_the_one_valid_sequence() {
        let per = rows_per_strip(1000).expect("per");
        assert!(per > 1);
        let height = per * 2 + 3;
        let fresh = || FrameAssembler::new(1000, height).expect("assembler");
        let bytes = |rows: u32| vec![0u8; rows as usize * 4000];

        let mut a = fresh();
        assert_eq!(a.push(1, per, &bytes(per)), Err(StripError::OutOfOrder));
        assert_eq!(
            a.push(0, per - 1, &bytes(per - 1)),
            Err(StripError::WrongRows)
        );
        assert_eq!(
            a.push(0, per + 1, &bytes(per + 1)),
            Err(StripError::WrongRows)
        );
        assert_eq!(
            a.push(0, per, &bytes(per - 1)),
            Err(StripError::WrongLength)
        );
        assert_eq!(
            a.push(0, per, &bytes(per + 1)),
            Err(StripError::WrongLength)
        );
        assert!(a.push(0, per, &bytes(per)).is_ok());
        // The same strip again.
        assert_eq!(a.push(0, per, &bytes(per)), Err(StripError::OutOfOrder));
        assert!(a.push(per, per, &bytes(per)).is_ok());
        // The last strip is short, and must be exactly the remainder.
        assert_eq!(
            a.push(per * 2, per, &bytes(per)),
            Err(StripError::WrongRows)
        );
        assert!(a.push(per * 2, 3, &bytes(3)).is_ok());
        // And nothing after the end.
        assert_eq!(a.push(height, 1, &bytes(1)), Err(StripError::Surplus));
        assert!(a.finish().is_ok());

        let mut b = fresh();
        assert!(b.push(0, per, &bytes(per)).is_ok());
        assert_eq!(b.finish().err(), Some(StripError::Incomplete));
    }

    #[test]
    fn frames_the_protocol_will_not_carry_are_refused_up_front() {
        for (w, h) in [(0, 1), (1, 0), (16385, 1), (1, 16385), (16384, 16384)] {
            assert!(FrameAssembler::new(w, h).is_err(), "{w}x{h}");
        }
        assert_eq!(rows_per_strip(0), None);
        assert_eq!(rows_per_strip(16385), None);
        assert!(rows_per_strip(16384).is_some_and(|rows| rows >= 1));
    }

    /// A declared frame costs only what is sent: a maximal frame, assembled
    /// with no strips yet, holds no pixel memory.
    #[test]
    fn an_assembler_allocates_from_strips_not_from_its_size() {
        let assembler = FrameAssembler::new(8192, 4096).expect("assembler");
        assert_eq!(assembler.pixels.capacity(), 0);
    }
}
