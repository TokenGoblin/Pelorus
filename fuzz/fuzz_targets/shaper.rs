#![no_main]

//! Untrusted font bytes and untrusted text through the shaper (Phase 9, §4.5).
//!
//! Input layout: one control byte (direction and script), a little-endian u16
//! font length, that many font bytes, then the rest as text. Whatever the
//! bytes, the shaper must return `Ok` or `Err` and never panic, hang or
//! exceed its documented limits.

use libfuzzer_sys::fuzz_target;
use px_text::shaper::{
    Direction, Feature, HarfrustShaper, MAX_RUN_BYTES, Script, ShapeInput, Shaper,
};
use std::sync::Arc;

fuzz_target!(|data: &[u8]| {
    let (Some(&control), Some(rest)) = (data.first(), data.get(1..)) else {
        return;
    };
    let (Some(&lo), Some(&hi), Some(rest)) = (rest.first(), rest.get(1), rest.get(2..)) else {
        return;
    };
    let font_len = usize::from(u16::from_le_bytes([lo, hi])).min(rest.len());
    let (font_bytes, text_bytes) = rest.split_at(font_len);
    let Ok(shaper) = HarfrustShaper::new(Arc::from(font_bytes), 0) else {
        return;
    };
    let text = String::from_utf8_lossy(text_bytes);
    let script = match control >> 1 & 7 {
        0 => Script::LATIN,
        1 => Script::ARABIC,
        2 => Script::HEBREW,
        3 => Script::DEVANAGARI,
        4 => Script::HAN,
        5 => Script::THAI,
        6 => Script::COMMON,
        _ => Script([control; 4]),
    };
    let direction = if control & 1 == 0 {
        Direction::LeftToRight
    } else {
        Direction::RightToLeft
    };
    let features = [Feature::global(*b"liga", u32::from(control >> 4 & 1))];
    if let Ok(run) = shaper.shape(&ShapeInput {
        text: &text,
        script,
        language: None,
        direction,
        features: &features,
    }) {
        assert!(text.len() <= MAX_RUN_BYTES);
        let _ = run.total_advance();
    }
});
