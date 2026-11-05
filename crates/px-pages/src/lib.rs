#![forbid(unsafe_code)]

//! The browser's own pages: plain HTML and CSS, authored in the brand palette,
//! drawn by our own engine.
//!
//! [`page`] returns the source of a page by name. The pages carry no script, no
//! external URL and no remote resource (a test enforces it), and each is one
//! self-contained file. Both colour schemes are in the page, behind
//! `prefers-color-scheme`.
//!
//! Where a page shows a value the browser knows at run time (the version, the
//! address that failed to load, the sandbox state), the element carries a
//! `data-field` attribute and a plain-words placeholder, so a caller can fill it
//! and a page opened without a caller still reads sensibly.

/// Names of every built-in page, in the order the `about:` index lists them.
pub const NAMES: &[&str] = &["newtab", "privacy", "version", "error"];

const NEWTAB: &str = include_str!("../pages/newtab.html");
const PRIVACY: &str = include_str!("../pages/privacy.html");
const VERSION: &str = include_str!("../pages/version.html");
const ERROR: &str = include_str!("../pages/error.html");

/// The phase progress data the version page charts, as maintained in
/// `docs/progress.txt`. Included so a test can check the page against it.
pub const PROGRESS: &str = include_str!("../../../docs/progress.txt");

/// The HTML source of the built-in page `name`, or `None` for an unknown name.
///
/// `newtab`, `privacy`, `version` and `error`; see [`NAMES`]. A name is the part
/// after `about:`, so `about:privacy` is `page("privacy")`.
#[must_use]
pub fn page(name: &str) -> Option<&'static str> {
    match name {
        "newtab" => Some(NEWTAB),
        "privacy" => Some(PRIVACY),
        "version" => Some(VERSION),
        "error" => Some(ERROR),
        _ => None,
    }
}
