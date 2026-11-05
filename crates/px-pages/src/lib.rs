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

fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

/// The `error` page with its two run-time fields filled in: the address that
/// did not load and a short, calm reason. Both are escaped (the address came
/// from a page or the address bar and the reason may quote a server), and the
/// address is cut at 300 characters. The fixed sentence about the connection
/// is dropped, because the reason now says what happened.
#[must_use]
pub fn error_page(address: &str, reason: &str) -> String {
    let address: String = address.chars().take(300).collect();
    ERROR
        .replace(
            "The connection ended before the page arrived. Nothing is wrong",
            "Nothing is wrong",
        )
        .replace(
            "data-field=\"address\">The address you asked for<",
            &format!("data-field=\"address\">{}<", escape_html(&address)),
        )
        .replace(
            "data-field=\"reason\">The connection was closed or refused<",
            &format!("data-field=\"reason\">{}<", escape_html(reason)),
        )
}

#[cfg(test)]
mod error_page_tests {
    use super::error_page;

    #[test]
    fn fields_are_filled_and_escaped() {
        let p = error_page("https://x.test/<b>", "It \"failed\" & stopped");
        assert!(p.contains("https://x.test/&lt;b&gt;"));
        assert!(p.contains("It &quot;failed&quot; &amp; stopped"));
        assert!(!p.contains("The address you asked for<"));
        assert!(!p.contains("<script"));
    }
}
