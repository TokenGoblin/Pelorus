//! User-visible strings of the browser chrome (`px-ui`).
//!
//! `crates/px-ui/CLAUDE.md`: every user-visible string comes from px-brand, no
//! literals. Plain words, no exclamation marks (`docs/brand/guide.md`). None of
//! these names the product, so a rename does not touch them.

/// Label of the "this page runs in a sandbox" chip (`ui-direction.md`).
pub const ISOLATED: &str = "Isolated";
/// Address field placeholder.
pub const ADDRESS_PLACEHOLDER: &str = "Search or enter address";
/// Title shown for a tab that has none yet.
pub const NEW_TAB_TITLE: &str = "New tab";
/// Typographic ellipsis appended to a truncated title.
pub const ELLIPSIS: &str = "\u{2026}";

/// Accessible names and tooltips of the controls.
pub const BACK: &str = "Back";
pub const FORWARD: &str = "Forward";
pub const RELOAD: &str = "Reload";
pub const STOP: &str = "Stop loading";
pub const MORE: &str = "More";
pub const NEW_TAB: &str = "New tab";
pub const CLOSE_TAB: &str = "Close tab";
pub const WINDOW_CLOSE: &str = "Close window";
pub const WINDOW_MINIMIZE: &str = "Minimize window";
pub const WINDOW_MAXIMIZE: &str = "Maximize window";
