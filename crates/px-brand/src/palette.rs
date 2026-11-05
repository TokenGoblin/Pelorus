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

/// Surfaces of the browser chrome (`docs/brand/ui-direction.md`, the two
/// `ui-mockup-*.svg` files). The foregrounds are [`DARK`] / [`LIGHT`]; these are
/// what they sit on. Chrome layout is frozen by screenshot diff (invariant 3),
/// so a value here changing is a UI change and needs the same review.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChromeSurfaces {
    /// Behind the window; the shadow falls on it.
    pub backdrop: Rgb,
    /// The window body and the toolbar-to-page seam.
    pub window: Rgb,
    /// The tab strip and toolbar band.
    pub bar: Rgb,
    /// Raised fields: the active tab, the address field, a hovered button.
    pub raised: Rgb,
    /// The one 1 px hairline.
    pub hairline: Rgb,
    /// Page area while nothing is loaded.
    pub page: Rgb,
    /// Peak alpha of the window shadow, 0..=255.
    pub shadow_alpha: u8,
}

/// Chrome surfaces for the dark theme: foregrounds are [`DARK`].
pub const CHROME_DARK: ChromeSurfaces = ChromeSurfaces {
    backdrop: NAVY,
    window: Rgb(0x10, 0x1A, 0x33),
    bar: Rgb(0x0D, 0x16, 0x30),
    raised: Rgb(0x18, 0x25, 0x4A),
    hairline: Rgb(0x24, 0x33, 0x57),
    page: NAVY,
    shadow_alpha: 140,
};

/// Chrome surfaces for the light theme: foregrounds are [`LIGHT`].
pub const CHROME_LIGHT: ChromeSurfaces = ChromeSurfaces {
    backdrop: Rgb(0xE8, 0xED, 0xF5),
    window: WHITE,
    bar: SURFACE_LIGHT,
    raised: Rgb(0xE9, 0xEE, 0xF6),
    hairline: Rgb(0xD5, 0xDC, 0xE8),
    page: WHITE,
    shadow_alpha: 46,
};

#[cfg(test)]
mod tests {
    use super::*;

    const AA_TEXT: f64 = 4.5;

    #[test]
    fn chrome_text_meets_aa_on_every_surface_it_is_drawn_on() {
        for s in [CHROME_DARK.bar, CHROME_DARK.raised, CHROME_DARK.window] {
            for (name, c) in [
                ("text", DARK.text),
                ("muted", DARK.muted),
                ("teal", DARK.teal),
            ] {
                let r = contrast_ratio(c, s);
                assert!(r >= AA_TEXT, "dark {name} on {} is {r:.2}:1", s.hex());
            }
        }
        for s in [CHROME_LIGHT.bar, CHROME_LIGHT.raised, CHROME_LIGHT.window] {
            for (name, c) in [
                ("ink", LIGHT.ink),
                ("muted", LIGHT.muted),
                ("teal", LIGHT.teal),
            ] {
                let r = contrast_ratio(c, s);
                assert!(r >= AA_TEXT, "light {name} on {} is {r:.2}:1", s.hex());
            }
        }
    }

    #[test]
    fn the_hairline_is_visible_against_the_surfaces_it_separates() {
        // Not text, so not 4.5: WCAG 1.4.11 asks 3:1 for a control boundary, but
        // this hairline is decoration (the fields are also set apart by fill).
        // The floor here only stops it vanishing.
        for (name, c) in [("dark", CHROME_DARK), ("light", CHROME_LIGHT)] {
            assert!(contrast_ratio(c.hairline, c.bar) > 1.05, "{name}");
            assert_ne!(c.raised, c.bar, "{name}: a field must differ from its bar");
        }
    }

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
