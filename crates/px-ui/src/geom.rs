//! Integer geometry in logical pixels, origin at the window's top-left.

/// A point.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

/// A size. Never negative: constructors clamp.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Size {
    pub w: i32,
    pub h: i32,
}

impl Size {
    pub fn new(w: i32, h: i32) -> Size {
        Size {
            w: w.max(0),
            h: h.max(0),
        }
    }
}

/// A rectangle with non-negative width and height.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub fn new(x: i32, y: i32, w: i32, h: i32) -> Rect {
        Rect {
            x,
            y,
            w: w.max(0),
            h: h.max(0),
        }
    }

    pub fn right(&self) -> i32 {
        self.x.saturating_add(self.w)
    }

    pub fn bottom(&self) -> i32 {
        self.y.saturating_add(self.h)
    }

    pub fn is_empty(&self) -> bool {
        self.w == 0 || self.h == 0
    }

    /// Half-open: the right and bottom edges are outside.
    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.x && p.x < self.right() && p.y >= self.y && p.y < self.bottom()
    }

    /// Shrink by `d` on every side (grow if negative).
    pub fn inset(&self, d: i32) -> Rect {
        Rect::new(
            self.x.saturating_add(d),
            self.y.saturating_add(d),
            self.w.saturating_sub(d.saturating_mul(2)),
            self.h.saturating_sub(d.saturating_mul(2)),
        )
    }

    /// The overlap, or an empty rectangle when there is none.
    pub fn intersect(&self, o: Rect) -> Rect {
        let x0 = self.x.max(o.x);
        let y0 = self.y.max(o.y);
        let x1 = self.right().min(o.right());
        let y1 = self.bottom().min(o.bottom());
        if x1 <= x0 || y1 <= y0 {
            Rect::default()
        } else {
            Rect::new(x0, y0, x1 - x0, y1 - y0)
        }
    }

    pub fn center(&self) -> Point {
        Point {
            x: self.x + self.w / 2,
            y: self.y + self.h / 2,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contains_is_half_open() {
        let r = Rect::new(2, 3, 4, 5);
        assert!(r.contains(Point { x: 2, y: 3 }));
        assert!(r.contains(Point { x: 5, y: 7 }));
        assert!(!r.contains(Point { x: 6, y: 3 }));
        assert!(!r.contains(Point { x: 2, y: 8 }));
        assert!(!Rect::new(0, 0, 0, 5).contains(Point { x: 0, y: 0 }));
    }

    #[test]
    fn negative_sizes_clamp_and_inset_cannot_go_negative() {
        assert_eq!(Rect::new(0, 0, -4, 3).w, 0);
        assert_eq!(Rect::new(0, 0, 4, 4).inset(5).w, 0);
        assert_eq!(Rect::new(0, 0, 4, 4).inset(-1), Rect::new(-1, -1, 6, 6));
        assert_eq!(Size::new(-1, 2), Size { w: 0, h: 2 });
    }
}
