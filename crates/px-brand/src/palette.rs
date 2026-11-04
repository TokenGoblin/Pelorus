//! The brand palette, and the contrast checks that keep it honest.
//!
//! Colours are chosen to say "steady and checkable", not "alarm". Navy for
//! calm, teal for clarity and the secure state, amber for the single thing worth
//! noticing, red only for refused or blocked. See `docs/brand/guide.md`.
//!
//! These are *brand* colours: the logo, installer, documents and site. The
//! shipped browser chrome does not change for brand reasons (invariant 3).
//!
//! There are two sets because one set cannot serve both backgrounds: teal that
//! reads at 12.6:1 on navy is under 2:1 on white. [`DARK`] is for text and marks
//! on [`NAVY`]; [`LIGHT`] is for text and marks on white or [`SURFACE_LIGHT`].
//! The tests below fail if a change breaks WCAG 2.1 AA (4.5:1 for text).

/// An sRGB colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    /// `#rrggbb`, uppercase, as written in CSS and SVG.
    pub fn hex(self) -> String {
        format!("#{:02X}{:02X}{:02X}", self.0, self.1, self.2)
    }

    /// WCAG 2.1 relative luminance.
    pub fn luminance(self) -> f64 {
        fn channel(c: u8) -> f64 {
            let c = f64::from(c) / 255.0;
            if c <= 0.03928 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        }
        0.2126 * channel(self.0) + 0.7152 * channel(self.1) + 0.0722 * channel(self.2)
    }
}

/// WCAG 2.1 contrast ratio between two colours, from 1.0 to 21.0.
pub fn contrast_ratio(a: Rgb, b: Rgb) -> f64 {
    let (la, lb) = (a.luminance(), b.luminance());
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// Deep navy: the dark background and the logo face.
pub const NAVY: Rgb = Rgb(0x0B, 0x12, 0x26);
/// Near-white page surface for light contexts.
pub const SURFACE_LIGHT: Rgb = Rgb(0xF4, 0xF7, 0xFB);
pub const WHITE: Rgb = Rgb(0xFF, 0xFF, 0xFF);

/// Foreground colours for use on [`NAVY`].
pub struct OnDark {
    pub text: Rgb,
    pub muted: Rgb,
    /// Secure / fine.
    pub teal: Rgb,
    /// Links.
    pub blue: Rgb,
    /// Look here / warning.
    pub amber: Rgb,
    /// Refused / blocked. Never used for decoration.
    pub red: Rgb,
}

/// Foreground colours for use on [`WHITE`] or [`SURFACE_LIGHT`].
pub struct OnLight {
    pub ink: Rgb,
    pub muted: Rgb,
    pub teal: Rgb,
    pub blue: Rgb,
    pub amber: Rgb,
    pub red: Rgb,
}

pub const DARK: OnDark = OnDark {
    text: Rgb(0xE2, 0xE8, 0xF0),
    muted: Rgb(0x9F, 0xB0, 0xC8),
    teal: Rgb(0x5E, 0xEA, 0xD4),
    blue: Rgb(0x60, 0xA5, 0xFA),
    amber: Rgb(0xFB, 0xBF, 0x24),
    red: Rgb(0xF8, 0x71, 0x71),
};

pub const LIGHT: OnLight = OnLight {
    ink: NAVY,
    muted: Rgb(0x47, 0x55, 0x69),
    teal: Rgb(0x0F, 0x76, 0x6E),
    blue: Rgb(0x1D, 0x4E, 0xD8),
    amber: Rgb(0xB4, 0x53, 0x09),
    red: Rgb(0xB9, 0x1C, 0x1C),
};

#[cfg(test)]
mod tests {
    use super::*;

    /// WCAG AA for normal text.
    const AA: f64 = 4.5;

    #[test]
    fn every_dark_foreground_meets_aa_on_navy() {
        let d = &DARK;
        for (name, c) in [
            ("text", d.text),
            ("muted", d.muted),
            ("teal", d.teal),
            ("blue", d.blue),
            ("amber", d.amber),
            ("red", d.red),
        ] {
            let r = contrast_ratio(c, NAVY);
            assert!(r >= AA, "{name} {} on navy is {r:.2}:1", c.hex());
        }
    }

    #[test]
    fn every_light_foreground_meets_aa_on_white_and_on_the_light_surface() {
        let l = &LIGHT;
        for (name, c) in [
            ("ink", l.ink),
            ("muted", l.muted),
            ("teal", l.teal),
            ("blue", l.blue),
            ("amber", l.amber),
            ("red", l.red),
        ] {
            for bg in [WHITE, SURFACE_LIGHT] {
                let r = contrast_ratio(c, bg);
                assert!(r >= AA, "{name} {} on {} is {r:.2}:1", c.hex(), bg.hex());
            }
        }
    }

    #[test]
    fn navy_text_is_legible_on_the_teal_and_amber_fills() {
        assert!(contrast_ratio(NAVY, DARK.teal) >= AA);
        assert!(contrast_ratio(NAVY, DARK.amber) >= AA);
    }

    #[test]
    fn contrast_matches_known_values() {
        assert!((contrast_ratio(WHITE, Rgb(0, 0, 0)) - 21.0).abs() < 1e-9);
        assert!((contrast_ratio(WHITE, WHITE) - 1.0).abs() < 1e-9);
        // Symmetric.
        assert_eq!(
            contrast_ratio(DARK.teal, NAVY),
            contrast_ratio(NAVY, DARK.teal)
        );
    }

    #[test]
    fn the_status_colours_are_distinct_from_each_other() {
        // Secure, warning and refused must never be confusable.
        let set = [DARK.teal, DARK.amber, DARK.red];
        for (i, a) in set.iter().enumerate() {
            for b in set.iter().skip(i + 1) {
                assert_ne!(a, b);
            }
        }
    }

    #[test]
    fn hex_is_uppercase_and_zero_padded() {
        assert_eq!(Rgb(0, 10, 255).hex(), "#000AFF");
        assert_eq!(NAVY.hex(), "#0B1226");
    }
}
