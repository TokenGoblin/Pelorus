//! Locations: what a tab can be showing, how a link or typed text resolves to one,
//! and how one is loaded: local files, the built-in `about:` pages, and `http(s)`
//! pages fetched through `px-net` (see [`crate::web`]).

use std::path::{Path, PathBuf};

use px_ui::address::UrlDisplay;

use crate::page::Resources;
use crate::web::Net;

/// Pages compiled into the binary.
pub const BUILTIN: &[(&str, &str)] = &[
    ("welcome", include_str!("builtin/welcome.html")),
    ("flex", include_str!("builtin/flex.html")),
    ("long", include_str!("builtin/long.html")),
];

/// The largest file this shell will read.
const MAX_FILE: u64 = 8 << 20;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Location {
    File(PathBuf),
    About(String),
    /// An absolute `http(s)` URL, fetched through `px-net`.
    Web(String),
}

/// What loading a location produced. A failure is a page too (the error page),
/// so a tab always has something to show.
pub struct Loaded {
    pub source: String,
    /// Where relative stylesheets come from (a file, or a fetched set).
    pub res: Resources,
    pub failed: bool,
    /// For a web page: the URL after redirects, which becomes the tab's address.
    pub final_url: Option<String>,
}

impl Location {
    pub fn welcome() -> Location {
        Location::About("welcome".into())
    }

    /// Parse argv or address-bar text: `about:name`, `file:///path`, or a path.
    pub fn parse(text: &str) -> Result<Location, String> {
        let t = text.trim();
        if t.is_empty() {
            return Err("empty address".into());
        }
        if let Some(name) = t.strip_prefix("about:") {
            return Ok(Location::About(name.to_string()));
        }
        if let Some(rest) = t.strip_prefix("file://") {
            return Ok(Location::File(PathBuf::from(rest)));
        }
        let lower = t.to_ascii_lowercase();
        if lower.starts_with("http://") || lower.starts_with("https://") {
            return px_net::browse::address_from_text(t)
                .map(Location::Web)
                .ok_or_else(|| "that web address could not be understood".to_string());
        }
        if has_scheme(t) {
            return Err(format!(
                "{} pages are not available: this build opens web (http, https) and local pages only",
                t.split(':').next().unwrap_or("network")
            ));
        }
        Ok(Location::File(PathBuf::from(t)))
    }

    /// Resolve a link `href` found on a page at `self`. `None` means "do
    /// nothing": a same-page fragment, or a target that is not local.
    pub fn resolve(&self, href: &str) -> Option<Location> {
        if let Location::Web(base) = self {
            let h = href.trim();
            if h.is_empty() || h.starts_with('#') {
                return None;
            }
            let target = px_net::browse::resolve(base, h)?;
            let target = target.split('#').next().unwrap_or("").to_string();
            return Some(Location::Web(target));
        }
        let h = href.trim();
        let h = h.split(['#', '?']).next().unwrap_or("");
        if h.is_empty() {
            return None;
        }
        if let Some(name) = h.strip_prefix("about:") {
            return Some(Location::About(name.to_string()));
        }
        if let Some(rest) = h.strip_prefix("file://") {
            return Some(Location::File(PathBuf::from(rest)));
        }
        if has_scheme(h) || h.starts_with("//") {
            return None;
        }
        let base: PathBuf = match self {
            Location::File(p) => p.parent().map(Path::to_path_buf).unwrap_or_default(),
            Location::About(_) | Location::Web(_) => PathBuf::new(),
        };
        Some(Location::File(base.join(h)))
    }

    /// The address bar: scheme hidden in the edit text, the file name emphasised.
    pub fn display(&self) -> UrlDisplay {
        match self {
            Location::About(n) => UrlDisplay::from_parts("about:", "", n, ""),
            Location::Web(u) => {
                // The whole host is emphasised and nothing is muted as a
                // "subdomain": the registrable-domain split needs the public
                // suffix list, and a wrong split would highlight the wrong part.
                let (scheme, rest) = u.split_once("://").unwrap_or(("https", u.as_str()));
                let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
                let (host, tail) = rest.split_at(end);
                let tail = if tail == "/" { "" } else { tail };
                UrlDisplay::from_parts(&format!("{scheme}://"), "", host, tail)
            }
            Location::File(p) => {
                let abs = std::path::absolute(p).unwrap_or_else(|_| p.clone());
                let name = abs
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let dir = abs
                    .parent()
                    .map(|d| d.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let dir = if dir.ends_with('/') {
                    dir
                } else {
                    format!("{dir}/")
                };
                UrlDisplay::from_parts("file://", &dir, &name, "")
            }
        }
    }

    /// A tab title for a page that has none of its own.
    pub fn fallback_title(&self) -> String {
        match self {
            Location::About(n) => n.clone(),
            Location::Web(u) => u
                .split_once("://")
                .map_or(u.as_str(), |(_, r)| r)
                .split(['/', '?', '#'])
                .next()
                .unwrap_or("")
                .to_string(),
            Location::File(p) => p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
        }
    }

    pub fn load(&self, net: &mut Net) -> Loaded {
        let result = match self {
            Location::About(n) => BUILTIN
                .iter()
                .find(|(name, _)| name == n)
                .map(|(_, src)| ((*src).to_string(), None))
                .ok_or_else(|| format!("there is no built-in page called about:{n}")),
            Location::File(p) => read_file(p).map(|s| (s, Some(p.clone()))),
            Location::Web(u) => return net.load(u),
        };
        match result {
            Ok((source, file)) => Loaded {
                source,
                res: Resources {
                    file,
                    ..Resources::default()
                },
                failed: false,
                final_url: None,
            },
            Err(msg) => Loaded {
                source: error_page(&msg),
                res: Resources::default(),
                failed: true,
                final_url: None,
            },
        }
    }
}

fn read_file(p: &Path) -> Result<String, String> {
    let meta = std::fs::metadata(p).map_err(|e| format!("cannot open {}: {e}", p.display()))?;
    if !meta.is_file() {
        return Err(format!("{} is not a file", p.display()));
    }
    if meta.len() > MAX_FILE {
        return Err(format!("{} is too large", p.display()));
    }
    let bytes = std::fs::read(p).map_err(|e| format!("cannot read {}: {e}", p.display()))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// `scheme:` at the start (two or more letters, so a Windows drive letter is a
/// path, not a scheme).
fn has_scheme(s: &str) -> bool {
    let Some((head, _)) = s.split_once(':') else {
        return false;
    };
    head.len() >= 2
        && head.starts_with(|c: char| c.is_ascii_alphabetic())
        && head
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

pub fn error_page(msg: &str) -> String {
    format!(
        "<!doctype html><title>Cannot open this page</title>\
         <body style=\"padding: 32px 40px\"><h2 style=\"color: #a4262c\">Cannot open this page</h2>\
         <p style=\"width: 560px\">{}</p>\
         <p><a href=\"about:welcome\">Go to the welcome page</a></p>",
        escape(msg)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_text_and_argv() {
        assert_eq!(
            Location::parse("about:long"),
            Ok(Location::About("long".into()))
        );
        assert_eq!(
            Location::parse(" /tmp/a.html "),
            Ok(Location::File("/tmp/a.html".into()))
        );
        assert_eq!(
            Location::parse("file:///tmp/a.html"),
            Ok(Location::File("/tmp/a.html".into()))
        );
        assert!(Location::parse("https://example.com/").is_err());
        assert!(Location::parse("").is_err());
        // A drive letter is a path.
        assert!(Location::parse("C:\\pages\\a.html").is_ok());
    }

    #[test]
    fn links_resolve_locally_or_not_at_all() {
        let here = Location::File("/pages/a/index.html".into());
        assert_eq!(
            here.resolve("b.html#top"),
            Some(Location::File("/pages/a/b.html".into()))
        );
        assert_eq!(
            here.resolve("../c.html"),
            Some(Location::File("/pages/a/../c.html".into()))
        );
        assert_eq!(
            here.resolve("about:flex"),
            Some(Location::About("flex".into()))
        );
        assert_eq!(here.resolve("#frag"), None);
        assert_eq!(here.resolve("https://example.com/x"), None);
        assert_eq!(here.resolve("//example.com/x"), None);
        assert_eq!(here.resolve("javascript:alert(1)"), None);
        assert_eq!(here.resolve("mailto:a@b.c"), None);
    }

    #[test]
    fn failures_are_pages() {
        let l = Location::About("nope".into()).load();
        assert!(l.failed);
        assert!(l.source.contains("no built-in page"));
        let l = Location::File("/nonexistent/x.html".into()).load();
        assert!(l.failed);
    }

    #[test]
    fn every_builtin_loads() {
        for (n, _) in BUILTIN {
            assert!(!Location::About((*n).into()).load().failed);
        }
    }
}
