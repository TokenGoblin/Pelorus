//! Pictures in a page: which image an `<img>` means, how many are fetched, and
//! how their bytes become pixels.
//!
//! # Choosing a source (HTML "update the image data", reduced)
//!
//! For an `<img>` the candidate list is, in preference order:
//!
//! 1. when its parent is `<picture>`: the first preceding `<source>` sibling
//!    that has a `srcset`, whose `media` matches and whose `type` is a format
//!    this browser decodes (a `type` of WebP or AVIF is skipped, as a
//!    browser that does not support it would); its `srcset` candidates;
//! 2. otherwise the `<img>`'s own `srcset` (its `src` counts as a `1x`
//!    candidate when no descriptor is a width);
//! 3. then `src` alone.
//!
//! Among `srcset` candidates the choice is deterministic, with the device pixel
//! ratio fixed at 1: for `w` descriptors the slot width comes from `sizes` (the
//! first entry whose media condition holds, else its last entry, else `100vw`)
//! and the pick is the smallest candidate at least that wide, else the widest;
//! for `x` descriptors the smallest `x >= 1`, else the largest. Ties go to the
//! earlier candidate. The remaining candidates follow the pick in the list so a
//! later layout at another width can fall back on whatever was fetched.
//!
//! # Limits
//!
//! At most [`MAX_IMAGES`] distinct images are requested per page, each at most
//! `px_net::browse::MAX_IMAGE_BYTES` compressed, decoded under [`limits`] (a
//! pixel cap, so a decompression bomb is refused before it allocates), and at
//! most [`MAX_TOTAL_BYTES`] of decoded pixels are kept in all. Anything that
//! fails (refused by policy, a bad status, an unknown or unsupported format, a
//! corrupt body, a limit) simply produces no picture, and the layout shows the
//! `alt` text.
//!
//! # Formats
//!
//! What `px-image` decodes (ADR 042): PNG including APNG, GIF, JPEG baseline and
//! progressive, BMP and ICO. An animation shows its first frame. WebP is
//! recognised and refused (no decoder). SVG is drawn by `px-image`'s static
//! renderer at its intrinsic size (ADR 049); an SVG in an `<img>` or a CSS image
//! runs no script and loads nothing. Inline `<svg>` elements are drawn too (see
//! `px_paint::svg`).

use std::collections::HashSet;
use std::sync::Arc;

use px_dom::{Arena, NodeId};
use px_paint::ImageData;

/// Most distinct images requested for one page.
pub const MAX_IMAGES: usize = 64;

/// Most decoded pixel bytes kept for one page.
pub const MAX_TOTAL_BYTES: usize = 192 << 20;

/// The viewport width assumed when planning fetches (the window may change
/// width later; layout then falls back on what was fetched).
pub const PLAN_WIDTH: u32 = 1024;

/// The decoder's caps for one image: 8 Mpx (32 MiB of RGBA), 64 frames.
#[must_use]
pub fn limits() -> px_image::Limits {
    px_image::Limits {
        max_pixels: 1 << 23,
        max_frames: 64,
        max_total_bytes: 96 << 20,
    }
}

/// Decode `bytes` into the first frame as RGBA. `None` for anything the
/// decoder refuses; the reason is the caller's to drop.
#[must_use]
pub fn decode(bytes: &[u8]) -> Option<Arc<ImageData>> {
    let decoded = px_image::decode(bytes, &limits()).ok()?;
    let img = decoded.first_image()?;
    let data = ImageData {
        width: img.width,
        height: img.height,
        rgba: img.rgba.clone(),
    };
    data.is_well_formed().then(|| Arc::new(data))
}

fn attr<'a>(node: &'a px_dom::Node, local: &str) -> Option<&'a str> {
    node.attrs()?
        .iter()
        .find(|a| &*a.name.local == local)
        .map(|a| &*a.value)
}

fn local_name(node: &px_dom::Node) -> Option<&str> {
    node.element_name().map(|n| n.local.as_ref())
}

// -- srcset and sizes ---------------------------------------------------------

/// One `srcset` candidate. `x` is in thousandths; a `w` candidate has `w`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub url: String,
    pub w: Option<u32>,
    pub x_milli: Option<u32>,
}

fn parse_milli(s: &str) -> Option<u32> {
    let (whole, frac) = s.split_once('.').unwrap_or((s, ""));
    if whole.is_empty() && frac.is_empty() {
        return None;
    }
    let whole: u32 = if whole.is_empty() {
        0
    } else {
        whole.parse().ok()?
    };
    let mut milli = 0u32;
    for (i, c) in frac.chars().take(3).enumerate() {
        milli += c.to_digit(10)? * [100, 10, 1][i];
    }
    whole.checked_mul(1000)?.checked_add(milli)
}

/// HTML's "parse a srcset attribute", simplified: URLs are runs of non-space
/// (trailing commas dropped), descriptors run to the next comma outside
/// parentheses. A candidate with an invalid or repeated descriptor is dropped.
#[must_use]
pub fn parse_srcset(s: &str) -> Vec<Candidate> {
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    while i < chars.len() {
        while i < chars.len() && (chars[i].is_whitespace() || chars[i] == ',') {
            i += 1;
        }
        let start = i;
        while i < chars.len() && !chars[i].is_whitespace() {
            i += 1;
        }
        let mut url: String = chars[start..i].iter().collect();
        if url.is_empty() {
            break;
        }
        let mut descriptors = String::new();
        if url.ends_with(',') {
            while url.ends_with(',') {
                url.pop();
            }
        } else {
            let mut depth = 0u32;
            while i < chars.len() {
                let c = chars[i];
                match c {
                    '(' => depth += 1,
                    ')' => depth = depth.saturating_sub(1),
                    ',' if depth == 0 => break,
                    _ => {}
                }
                descriptors.push(c);
                i += 1;
            }
        }
        let (mut w, mut x, mut valid) = (None, None, true);
        for token in descriptors.split_whitespace() {
            let (num, unit) = token.split_at(token.len().saturating_sub(1));
            match unit {
                "w" if w.is_none() && x.is_none() => match num.parse::<u32>() {
                    Ok(n) if n > 0 => w = Some(n),
                    _ => valid = false,
                },
                "x" if x.is_none() && w.is_none() => match parse_milli(num) {
                    Some(n) if n > 0 => x = Some(n),
                    _ => valid = false,
                },
                _ => valid = false,
            }
        }
        if valid && !url.is_empty() {
            out.push(Candidate { url, w, x_milli: x });
        }
    }
    out
}

/// A length in `sizes` or a media condition: `N px`, `N vw`, `N em`/`rem`
/// (16 px). `None` for anything else (calc and friends).
fn length_px(s: &str, viewport: u32) -> Option<i64> {
    let s = s.trim();
    let split = s.find(|c: char| !(c.is_ascii_digit() || c == '.'))?;
    let (num, unit) = s.split_at(split);
    let milli = i64::from(parse_milli(num)?);
    let per_unit_milli = match unit.trim().to_ascii_lowercase().as_str() {
        "px" => 1000,
        "vw" => i64::from(viewport) * 10,
        "em" | "rem" => 16_000,
        _ => return None,
    };
    Some(milli * per_unit_milli / 1_000_000)
}

/// Evaluate a media query list (`,` separated; `and` conjunctions of
/// `(min-width: L)` / `(max-width: L)`, `all`, `screen`) at `width` px.
/// `Some(false)` for what does not hold *or is not understood*: an unknown
/// feature does not match, as in a browser that lacks it.
#[must_use]
pub fn media_matches(query: &str, width: u32) -> bool {
    let q = query.trim();
    if q.is_empty() {
        return true;
    }
    q.split(',').any(|one| {
        let one = one.trim().to_ascii_lowercase();
        if one.is_empty() {
            return false;
        }
        let mut ok = true;
        for part in one.split(" and ") {
            let part = part.trim();
            match part {
                "all" | "screen" => {}
                p if p.starts_with('(') && p.ends_with(')') => {
                    let inner = p[1..p.len() - 1].trim();
                    let Some((feature, value)) = inner.split_once(':') else {
                        ok = false;
                        continue;
                    };
                    let v = length_px(value, width);
                    ok &= match (feature.trim(), v) {
                        ("min-width", Some(l)) => i64::from(width) >= l,
                        ("max-width", Some(l)) => i64::from(width) <= l,
                        _ => false,
                    };
                }
                _ => ok = false,
            }
        }
        ok
    })
}

/// The slot width in px from a `sizes` attribute (default `100vw`).
#[must_use]
pub fn slot_width(sizes: Option<&str>, viewport: u32) -> i64 {
    let default = i64::from(viewport);
    let Some(sizes) = sizes else { return default };
    // Entries are comma separated, but a media condition may not contain a comma
    // outside parentheses here, so a plain split is enough.
    let entries: Vec<&str> = sizes.split(',').map(str::trim).collect();
    let n = entries.len();
    for (i, entry) in entries.iter().enumerate() {
        if entry.is_empty() {
            continue;
        }
        let (cond, len) = match entry.rfind(')') {
            Some(close) => (
                entry.get(..=close).unwrap_or(""),
                entry.get(close + 1..).unwrap_or("").trim(),
            ),
            None => ("", *entry),
        };
        let applies = cond.is_empty() || media_matches(cond, viewport);
        if applies || i + 1 == n {
            if !applies {
                // The last entry without a matching condition: its value anyway
                // only when it has no condition at all.
                continue;
            }
            return length_px(len, viewport)
                .filter(|l| *l > 0)
                .unwrap_or(default);
        }
    }
    default
}

/// Pick from `candidates` (DPR 1). Index of the choice, or `None` when empty.
#[must_use]
pub fn pick(candidates: &[Candidate], slot: i64) -> Option<usize> {
    if candidates.is_empty() {
        return None;
    }
    let any_w = candidates.iter().any(|c| c.w.is_some());
    // The comparable density of a candidate in thousandths.
    let density = |c: &Candidate| -> i64 {
        if any_w {
            match c.w {
                Some(w) => i64::from(w) * 1000 / slot.max(1),
                None => 1000,
            }
        } else {
            i64::from(c.x_milli.unwrap_or(1000))
        }
    };
    let mut best: Option<(usize, i64)> = None;
    // Smallest density >= 1.
    for (i, c) in candidates.iter().enumerate() {
        let d = density(c);
        if d >= 1000 && best.is_none_or(|(_, bd)| d < bd) {
            best = Some((i, d));
        }
    }
    if let Some((i, _)) = best {
        return Some(i);
    }
    // Otherwise the largest.
    let mut largest: Option<(usize, i64)> = None;
    for (i, c) in candidates.iter().enumerate() {
        let d = density(c);
        if largest.is_none_or(|(_, bd)| d > bd) {
            largest = Some((i, d));
        }
    }
    largest.map(|(i, _)| i)
}

fn supported_type(t: &str) -> bool {
    let t = t.trim().to_ascii_lowercase();
    t.is_empty()
        || matches!(
            t.as_str(),
            "image/png"
                | "image/apng"
                | "image/jpeg"
                | "image/jpg"
                | "image/pjpeg"
                | "image/gif"
                | "image/svg+xml"
                | "image/bmp"
                | "image/x-icon"
                | "image/vnd.microsoft.icon"
        )
}

fn ordered(cands: Vec<Candidate>, viewport: u32, sizes: Option<&str>) -> Vec<String> {
    let slot = slot_width(sizes, viewport);
    let Some(chosen) = pick(&cands, slot) else {
        return Vec::new();
    };
    let mut out = vec![cands.get(chosen).map(|c| c.url.clone()).unwrap_or_default()];
    for (i, c) in cands.iter().enumerate() {
        if i != chosen && !out.contains(&c.url) {
            out.push(c.url.clone());
        }
    }
    out
}

/// The raw URLs `<img>` `node` may show, preferred first (see the module docs).
#[must_use]
pub fn candidates(arena: &Arena, node: NodeId, viewport: u32) -> Vec<String> {
    let Some(img) = arena.get(node) else {
        return Vec::new();
    };
    if local_name(img) != Some("img") {
        return Vec::new();
    }
    let sizes_of = |n: &px_dom::Node| attr(n, "sizes").map(str::to_string);
    // 1. A <picture> parent's first usable <source>.
    if let Some(parent) = arena.get(node).and_then(px_dom::Node::parent)
        && arena
            .get(parent)
            .is_some_and(|p| local_name(p) == Some("picture"))
    {
        let mut sib = arena.get(parent).and_then(px_dom::Node::first_child);
        while let Some(id) = sib {
            if id == node {
                break;
            }
            let Some(s) = arena.get(id) else { break };
            if local_name(s) == Some("source")
                && let Some(srcset) = attr(s, "srcset")
                && media_matches(attr(s, "media").unwrap_or(""), viewport)
                && supported_type(attr(s, "type").unwrap_or(""))
            {
                let cands = parse_srcset(srcset);
                if !cands.is_empty() {
                    return ordered(cands, viewport, sizes_of(s).as_deref());
                }
            }
            sib = s.next_sibling();
        }
    }
    // 2. The img's own srcset, with src as a 1x candidate.
    let mut cands = attr(img, "srcset").map(parse_srcset).unwrap_or_default();
    let src = attr(img, "src").map(str::trim).filter(|s| !s.is_empty());
    if let Some(src) = src
        && cands.iter().all(|c| c.w.is_none())
        && !cands.iter().any(|c| c.url == src)
    {
        cands.push(Candidate {
            url: src.to_string(),
            w: None,
            x_milli: Some(1000),
        });
    }
    let mut out = ordered(cands, viewport, sizes_of(img).as_deref());
    // 3. src alone as the last resort.
    if let Some(src) = src
        && !out.iter().any(|u| u == src)
    {
        out.push(src.to_string());
    }
    out
}

/// Read and decode the images of a local page from next to its file. The key is
/// the file's path (see `Resources::key`, which keeps it at or below the page's
/// directory). A file that is missing, too large, not a regular file or not
/// decodable is skipped.
pub fn load_local(source: &str, res: &mut crate::page::Resources) {
    let mut total = 0usize;
    for key in crate::page::image_plan(source, res) {
        let path = std::path::Path::new(&key);
        let Ok(meta) = std::fs::metadata(path) else {
            continue;
        };
        if !meta.is_file() || meta.len() > px_net::browse::MAX_IMAGE_BYTES as u64 {
            continue;
        }
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        if let Some(img) = decode(&bytes) {
            total += img.rgba.len();
            if total > MAX_TOTAL_BYTES {
                break;
            }
            res.images.insert(key, img);
        }
    }
}

/// A de-duplicated, capped list of keys, in first-seen order.
#[derive(Default)]
pub struct Wanted {
    seen: HashSet<String>,
    pub keys: Vec<String>,
}

impl Wanted {
    pub fn add(&mut self, key: String) {
        if self.keys.len() < MAX_IMAGES && self.seen.insert(key.clone()) {
            self.keys.push(key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn urls(c: &[Candidate]) -> Vec<&str> {
        c.iter().map(|c| c.url.as_str()).collect()
    }

    #[test]
    fn srcset_parses_urls_and_descriptors() {
        let c = parse_srcset("a.png 1x, b.png 2x,c.png 480w , d.png");
        assert_eq!(urls(&c), ["a.png", "b.png", "c.png", "d.png"]);
        assert_eq!(c[0].x_milli, Some(1000));
        assert_eq!(c[1].x_milli, Some(2000));
        assert_eq!(c[2].w, Some(480));
        assert_eq!((c[3].w, c[3].x_milli), (None, None));
        // Commas inside a URL; trailing commas on a bare URL.
        let c = parse_srcset("data:image/png;base64,AAAA 2x, e.png,");
        assert_eq!(urls(&c), ["data:image/png;base64,AAAA", "e.png"]);
        // Invalid or mixed descriptors drop the candidate.
        let c = parse_srcset("a.png 0w, b.png 1x 2w, c.png -1x, d.png 1.5x");
        assert_eq!(urls(&c), ["d.png"]);
        assert_eq!(c[0].x_milli, Some(1500));
        assert!(parse_srcset("").is_empty());
    }

    #[test]
    fn media_conditions() {
        assert!(media_matches("", 500));
        assert!(media_matches("(min-width: 400px)", 500));
        assert!(!media_matches("(min-width: 600px)", 500));
        assert!(media_matches("(max-width: 500px)", 500));
        assert!(media_matches(
            "screen and (min-width: 400px) and (max-width: 600px)",
            500
        ));
        assert!(!media_matches("(prefers-color-scheme: dark)", 500));
        assert!(!media_matches("print", 500));
        assert!(media_matches("print, (min-width: 100px)", 500));
        assert!(media_matches("(min-width: 20em)", 400));
    }

    #[test]
    fn sizes_slot_widths() {
        assert_eq!(slot_width(None, 800), 800);
        assert_eq!(slot_width(Some("50vw"), 800), 400);
        assert_eq!(
            slot_width(Some("(max-width: 600px) 480px, 800px"), 500),
            480
        );
        assert_eq!(
            slot_width(Some("(max-width: 600px) 480px, 800px"), 700),
            800
        );
        // Nothing applies: 100vw.
        assert_eq!(slot_width(Some("(max-width: 100px) 50px"), 700), 700);
        // calc() is not understood: 100vw.
        assert_eq!(slot_width(Some("calc(100vw - 20px)"), 700), 700);
    }

    #[test]
    fn the_pick_is_deterministic() {
        let c = parse_srcset("s.png 400w, m.png 800w, l.png 1600w");
        assert_eq!(pick(&c, 500), Some(1));
        assert_eq!(pick(&c, 800), Some(1));
        assert_eq!(pick(&c, 801), Some(2));
        assert_eq!(
            pick(&c, 5000),
            Some(2),
            "the widest when none is wide enough"
        );
        assert_eq!(pick(&c, 100), Some(0));
        let d = parse_srcset("a.png 2x, b.png 1x, c.png 3x");
        assert_eq!(pick(&d, 100), Some(1));
        let e = parse_srcset("a.png 2x, b.png 1.5x");
        assert_eq!(pick(&e, 100), Some(1));
        let f = parse_srcset("a.png 0.5x, b.png 0.75x");
        assert_eq!(pick(&f, 100), Some(1), "the largest when none reaches 1x");
        assert_eq!(pick(&[], 100), None);
        // Ties go to the first.
        let g = parse_srcset("a.png 1x, b.png 1x");
        assert_eq!(pick(&g, 100), Some(0));
    }

    fn cands(html: &str, viewport: u32) -> Vec<String> {
        let dom = px_dom::parse(html);
        let arena = dom.arena;
        let img = arena
            .descendants(arena.document())
            .find(|n| {
                arena
                    .get(*n)
                    .and_then(|x| x.element_name())
                    .is_some_and(|q| &*q.local == "img")
            })
            .expect("an img");
        candidates(&arena, img, viewport)
    }

    #[test]
    fn img_candidates_follow_the_html_rules() {
        assert_eq!(cands("<img src=a.png>", 800), ["a.png"]);
        assert_eq!(cands("<img>", 800), Vec::<String>::new());
        assert_eq!(
            cands("<img src=a.png srcset='b.png 2x, c.png 1x'>", 800),
            ["c.png", "b.png", "a.png"]
        );
        // With w descriptors src is not a candidate but stays the last resort.
        assert_eq!(
            cands(
                "<img src=z.png srcset='s.png 300w, l.png 900w' sizes='100vw'>",
                800
            ),
            ["l.png", "s.png", "z.png"]
        );
        assert_eq!(
            cands(
                "<img src=z.png srcset='s.png 300w, l.png 900w' sizes='(max-width: 400px) 280px, 900px'>",
                350
            ),
            ["s.png", "l.png", "z.png"]
        );
    }

    #[test]
    fn picture_takes_the_first_matching_supported_source() {
        let html = "<picture>\
            <source srcset='w.webp' type='image/webp'>\
            <source srcset='dark.png' media='(prefers-color-scheme: dark)'>\
            <source srcset='wide.png' media='(min-width: 600px)'>\
            <source srcset='any.png'>\
            <img src=fallback.png></picture>";
        assert_eq!(cands(html, 800)[0], "wide.png");
        assert_eq!(cands(html, 400)[0], "any.png");
        let none = "<picture><source srcset='a.webp' type='image/webp'><img src=f.png></picture>";
        assert_eq!(cands(none, 800), ["f.png"]);
        // A source after the img is ignored.
        let after = "<picture><img src=f.png><source srcset='late.png'></picture>";
        assert_eq!(cands(after, 800), ["f.png"]);
    }

    #[test]
    fn decode_refuses_what_it_cannot_show() {
        assert!(decode(b"<html>").is_none());
        assert!(decode(b"RIFF\x00\x00\x00\x00WEBPVP8 ").is_none(), "WebP");
        assert!(decode(&[]).is_none());
    }

    #[test]
    fn svg_decodes_to_its_intrinsic_size() {
        let img = decode(
            b"<svg xmlns='http://www.w3.org/2000/svg' width='20' height='10'><rect width='20' height='10' fill='red'/></svg>",
        )
        .unwrap();
        assert_eq!((img.width, img.height), (20, 10));
        assert_eq!(img.rgba.get(..4), Some(&[255u8, 0, 0, 255][..]));
        // A document that merely mentions svg is not one.
        assert!(decode(b"<html><svg/></html>").is_none());
        let pic = "<picture><source srcset='a.svg' type='image/svg+xml'><img src=f.png></picture>";
        assert_eq!(
            cands(pic, 800),
            ["a.svg"],
            "an SVG source is no longer skipped"
        );
    }
}
