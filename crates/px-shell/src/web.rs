//! Loading an `http(s)` page for a tab, through `px-net`.
//!
//! The shell never opens a socket: [`Net::load`] asks `px_net::browse` for the
//! navigation and then for each stylesheet the page links, and every one of
//! those requests passes the checks wired into `px-net`'s secure fetch
//! (partitioned state, HSTS, CSP, mixed content, CORS, referrer policy,
//! redirect rules). What the engine cannot use yet (scripts, images, fonts,
//! frames, forms) is never requested and never run. A failure of any kind
//! becomes the built-in error page, in calm words.
//!
//! This runs on the UI thread and blocks while it waits; the content/network
//! process split and a non-blocking load are Phase 19.

use std::collections::{HashMap, HashSet};

use px_net::browse::{LoadError, WebSession};

use crate::decode::decode_html;
use crate::nav::Loaded;
use crate::page::{Resources, subresource_plan};

/// The most stylesheets fetched for one page.
const MAX_SHEETS: usize = 12;

/// The shell's handle on the network: the parsed lists, built on first use.
#[derive(Default)]
pub struct Net {
    session: Option<Result<WebSession, ()>>,
}

fn failed(url: &str, reason: &str) -> Loaded {
    Loaded {
        source: px_pages::error_page(url, reason),
        res: Resources::default(),
        failed: true,
        final_url: None,
    }
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

impl Net {
    /// Fetch and prepare the page at `url`.
    pub fn load(&mut self, url: &str) -> Loaded {
        let session = self
            .session
            .get_or_insert_with(|| WebSession::new().map_err(|_| ()));
        let Ok(session) = session else {
            return failed(url, &LoadError::Setup.reason());
        };
        let mut loader = session.loader();
        let doc = match loader.navigate(url) {
            Ok(d) => d,
            Err(e) => return failed(url, &e.reason()),
        };
        let ct = doc.content_type.clone();
        let kind = ct.as_deref().map(|c| {
            c.split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase()
        });
        let source = match kind.as_deref() {
            None | Some("" | "text/html" | "application/xhtml+xml") => {
                decode_html(&doc.body, ct.as_deref())
            }
            Some("text/plain") => format!(
                "<!doctype html><body><pre>{}</pre>",
                escape(&decode_html(&doc.body, ct.as_deref()))
            ),
            Some(other) => {
                return failed(
                    &doc.url,
                    &format!("This kind of content ({other}) cannot be shown yet."),
                );
            }
        };
        if doc.body.is_empty() {
            let reason = if doc.status >= 400 {
                format!("The site answered with status {} and no page.", doc.status)
            } else {
                "The site sent an empty page.".to_string()
            };
            return failed(&doc.url, &reason);
        }

        let (links, styles) = subresource_plan(&source);
        let mut res = Resources {
            base: Some(doc.url.clone()),
            fetched: HashMap::new(),
            blocked_inline: HashSet::new(),
            file: None,
        };
        for text in styles {
            if !loader.allows_inline_style(&text) {
                res.blocked_inline.insert(text);
            }
        }
        for href in links.iter().take(MAX_SHEETS) {
            let Some(abs) = px_net::browse::resolve(&doc.url, href) else {
                continue;
            };
            if res.fetched.contains_key(&abs) {
                continue;
            }
            // A stylesheet that cannot be fetched is skipped: the page still
            // shows, without it.
            if let Ok(bytes) = loader.stylesheet(href) {
                let css = decode_html(&bytes, None);
                // A server that answers a stylesheet request with a page is
                // not sending a stylesheet.
                if !css.trim_start().starts_with('<') {
                    res.fetched.insert(abs, css);
                }
            }
        }
        Loaded {
            source,
            res,
            failed: false,
            final_url: Some(doc.url),
        }
    }
}
