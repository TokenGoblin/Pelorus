//! Dark and light, both from the brand palette.

use px_brand::palette::{self, ChromeSurfaces, Rgb};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Theme {
    Dark,
    Light,
}

/// Every colour the chrome draws with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Colors {
    pub surfaces: ChromeSurfaces,
    /// Primary text and icons.
    pub text: Rgb,
    pub muted: Rgb,
    /// "Fine": the Isolated chip. Never used for anything else.
    pub teal: Rgb,
    /// Focus ring and selection.
    pub blue: Rgb,
    pub amber: Rgb,
    pub red: Rgb,
}

impl Theme {
    pub fn colors(self) -> Colors {
        match self {
            Theme::Dark => Colors {
                surfaces: palette::CHROME_DARK,
                text: palette::DARK.text,
                muted: palette::DARK.muted,
                teal: palette::DARK.teal,
                blue: palette::DARK.blue,
                amber: palette::DARK.amber,
                red: palette::DARK.red,
            },
            Theme::Light => Colors {
                surfaces: palette::CHROME_LIGHT,
                text: palette::LIGHT.ink,
                muted: palette::LIGHT.muted,
                teal: palette::LIGHT.teal,
                blue: palette::LIGHT.blue,
                amber: palette::LIGHT.amber,
                red: palette::LIGHT.red,
            },
        }
    }
}
