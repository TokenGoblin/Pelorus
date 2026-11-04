//! Decode limits and the allocation chokepoint.

use crate::DecodeError;

/// Hard bounds on one decode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Largest accepted width * height of any image or canvas.
    pub max_pixels: u64,
    /// Largest accepted number of animation frames.
    pub max_frames: usize,
    /// Largest number of bytes live at once in decoder buffers (inflated rows,
    /// working planes, canvases, and every output frame kept).
    pub max_total_bytes: usize,
}

impl Limits {
    /// 2^25 pixels, 4096 frames, 512 MiB.
    pub const DEFAULT: Limits = Limits {
        max_pixels: 1 << 25,
        max_frames: 4096,
        max_total_bytes: 512 << 20,
    };
}

impl Default for Limits {
    fn default() -> Self {
        Limits::DEFAULT
    }
}

/// Tracks bytes handed out. Every input-sized buffer is obtained here, after
/// the check and with `try_reserve_exact`, so a hostile header cannot cause an
/// allocation beyond [`Limits::max_total_bytes`] (or an abort on huge sizes).
#[derive(Debug)]
pub struct Budget {
    pub(crate) limits: Limits,
    used: usize,
}

impl Budget {
    pub fn new(limits: &Limits) -> Self {
        Budget {
            limits: *limits,
            used: 0,
        }
    }

    /// Validate dimensions and return the pixel count.
    pub fn check_dims(&self, w: u64, h: u64) -> Result<usize, DecodeError> {
        if w == 0 || h == 0 {
            return Err(DecodeError::BadDimensions);
        }
        let px = w.checked_mul(h).ok_or(DecodeError::BadDimensions)?;
        if px > self.limits.max_pixels {
            return Err(DecodeError::LimitExceeded("pixels"));
        }
        usize::try_from(px).map_err(|_| DecodeError::LimitExceeded("pixels"))
    }

    /// Reserve `n` bytes of budget without allocating.
    pub fn charge(&mut self, n: usize) -> Result<(), DecodeError> {
        let total = self
            .used
            .checked_add(n)
            .ok_or(DecodeError::LimitExceeded("bytes"))?;
        if total > self.limits.max_total_bytes {
            return Err(DecodeError::LimitExceeded("bytes"));
        }
        self.used = total;
        Ok(())
    }

    /// Return `n` bytes of budget (a buffer was dropped).
    pub fn release(&mut self, n: usize) {
        self.used = self.used.saturating_sub(n);
    }

    /// A zero-filled buffer of exactly `n` bytes, charged to the budget.
    pub fn zeroed(&mut self, n: usize) -> Result<Vec<u8>, DecodeError> {
        self.charge(n)?;
        let mut v = Vec::new();
        v.try_reserve_exact(n)
            .map_err(|_| DecodeError::LimitExceeded("allocation"))?;
        v.resize(n, 0);
        Ok(v)
    }

    /// An empty buffer with capacity for exactly `n` bytes, charged.
    pub fn with_capacity(&mut self, n: usize) -> Result<Vec<u8>, DecodeError> {
        self.charge(n)?;
        let mut v = Vec::new();
        v.try_reserve_exact(n)
            .map_err(|_| DecodeError::LimitExceeded("allocation"))?;
        Ok(v)
    }

    /// Check that one more frame of `bytes` may be kept.
    pub fn check_frame_count(&self, frames_so_far: usize) -> Result<(), DecodeError> {
        if frames_so_far >= self.limits.max_frames {
            return Err(DecodeError::LimitExceeded("frames"));
        }
        Ok(())
    }
}

/// `a * b` as usize, or a limit error.
pub fn mul(a: usize, b: usize) -> Result<usize, DecodeError> {
    a.checked_mul(b).ok_or(DecodeError::LimitExceeded("size"))
}
