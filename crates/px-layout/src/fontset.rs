//! Web fonts, system generic families and per-page `@font-face` matching.
//!
//! The registry in [`crate::text`] holds the bundled faces (ids below
//! [`px_text::registry::MAX_FONTS`]). Everything that arrives later, a web font
//! or the face a generic family resolves to on this machine, lives here, under
//! ids from [`WEB_BASE`] up. The two ranges never meet, so a `FontId` in a
//! display list names one face for the life of the process.
//!
//! # Lifetime and bounds
//!
//! A registered face is **leaked on purpose**: layout hands out `&'static Font`
//! and a display list may outlive the page that made it (a tab in the
//! background keeps its list). Instead of reference counting through every
//! consumer, the store is capped hard and deduplicated by content, so the leak
//! is bounded and a page that reloads the same font adds nothing:
//!
//! * [`MAX_WEB_FONTS`] faces (256) and [`MAX_WEB_BYTES`] of unwrapped sfnt
//!   (128 MiB) per process; a registration over either is refused and the page
//!   falls back deterministically.
//! * identical bytes (after unwrapping WOFF/WOFF2) return the same id.
//!
//! # `@font-face` matching
//!
//! A page's faces ([`PageFonts`]) are installed for the duration of a layout
//! pass ([`with_page_fonts`], thread local: layout is single threaded). For a
//! family name, [`PageFonts::candidates`] returns every face declared under it,
//! ordered by CSS Fonts 4 section 5.2: `font-style` preference first, then the
//! weight rules (the desired weight if a face's range holds it; otherwise for
//! 400..=500 first up to 500 then below then above, for under 400 below then
//! above, for over 500 above then below), then **later rules first** so a
//! redeclaration wins. A face with a `unicode-range` is only consulted for the
//! code points it names. Layout takes, per character, the first candidate that
//! both allows and maps it, so several faces with disjoint ranges compose into
//! one family and a missing glyph in the best-weight face falls to the next.
//! No synthetic bold or italic is made.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

pub use px_css::font_face::FaceStyle;
use px_text::shaper::HarfrustShaper;
use px_text::sysfonts::GenericFamily;
use px_text::{Font, FontId};

/// The first id of a face outside the bundled registry.
pub const WEB_BASE: u32 = 1 << 16;
/// Most faces the process holds outside the bundled set.
pub const MAX_WEB_FONTS: usize = 256;
/// Most unwrapped font bytes the process holds outside the bundled set.
pub const MAX_WEB_BYTES: usize = 128 * 1024 * 1024;
/// Most distinct family chains interned.
const MAX_CHAINS: usize = 4096;

/// Why a font was not registered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegisterError {
    /// The bytes are not a font this engine reads.
    Invalid(px_text::FontError),
    /// The face or byte budget is spent.
    Full,
}

/// A registered face, usable for the rest of the process.
#[derive(Clone, Copy)]
pub struct Face {
    pub font: &'static Font,
    pub shaper: Option<&'static HarfrustShaper>,
}

struct Entry {
    face: Face,
}

#[derive(Default)]
struct Store {
    entries: Vec<&'static Entry>,
    bytes: usize,
    by_hash: HashMap<u64, Vec<u32>>,
}

fn store() -> &'static RwLock<Store> {
    static S: std::sync::OnceLock<RwLock<Store>> = std::sync::OnceLock::new();
    S.get_or_init(|| RwLock::new(Store::default()))
}

fn fnv(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in data {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Unwrap (WOFF, WOFF2), validate and register a font file. The same bytes
/// registered twice give the same id.
pub fn register_font(file: &[u8]) -> Result<FontId, RegisterError> {
    let sfnt = px_text::woff::to_sfnt(file).map_err(RegisterError::Invalid)?;
    let hash = fnv(&sfnt) ^ (sfnt.len() as u64).rotate_left(32);
    if let Ok(s) = store().read()
        && let Some(ids) = s.by_hash.get(&hash)
    {
        for &i in ids {
            let same = s
                .entries
                .get(i as usize)
                .is_some_and(|e| e.face.font.bytes()[..] == sfnt[..]);
            if same {
                return Ok(FontId(WEB_BASE + i));
            }
        }
    }
    let len = sfnt.len();
    let font = Font::from_bytes(sfnt).map_err(RegisterError::Invalid)?;
    let shaper = HarfrustShaper::new(font.bytes().clone(), 0).ok();
    let mut s = store().write().map_err(|_| RegisterError::Full)?;
    if s.entries.len() >= MAX_WEB_FONTS || s.bytes.saturating_add(len) > MAX_WEB_BYTES {
        return Err(RegisterError::Full);
    }
    let idx = u32::try_from(s.entries.len()).map_err(|_| RegisterError::Full)?;
    // Leaked deliberately; bounded by the two caps above.
    let font: &'static Font = Box::leak(Box::new(font));
    let shaper: Option<&'static HarfrustShaper> = shaper.map(|x| &*Box::leak(Box::new(x)));
    let entry: &'static Entry = Box::leak(Box::new(Entry {
        face: Face { font, shaper },
    }));
    s.entries.push(entry);
    s.bytes += len;
    s.by_hash.entry(hash).or_default().push(idx);
    Ok(FontId(WEB_BASE + idx))
}

/// The registered face for `id`, if `id` is a web id.
#[must_use]
pub fn web_face(id: FontId) -> Option<Face> {
    let i = id.0.checked_sub(WEB_BASE)?;
    let s = store().read().ok()?;
    s.entries.get(i as usize).map(|e| e.face)
}

/// How many faces and unwrapped bytes the store holds (for tests and limits).
#[must_use]
pub fn web_usage() -> (usize, usize) {
    store()
        .read()
        .map(|s| (s.entries.len(), s.bytes))
        .unwrap_or((0, 0))
}

// ------------------------------------------------------------------- generics

fn generic_index(g: GenericFamily) -> usize {
    match g {
        GenericFamily::Serif => 0,
        GenericFamily::SansSerif => 1,
        GenericFamily::Monospace => 2,
        GenericFamily::Cursive => 3,
        GenericFamily::Fantasy => 4,
        GenericFamily::SystemUi => 5,
    }
}

fn system_generics() -> &'static RwLock<[Option<FontId>; 6]> {
    static G: std::sync::OnceLock<RwLock<[Option<FontId>; 6]>> = std::sync::OnceLock::new();
    G.get_or_init(|| RwLock::new([None; 6]))
}

/// Make `family` resolve to `font` for every page (the faces found on this
/// machine, chosen by the browser binary at startup). `None` removes it.
pub fn set_system_generic(family: GenericFamily, font: Option<FontId>) {
    if let Ok(mut g) = system_generics().write()
        && let Some(slot) = g.get_mut(generic_index(family))
    {
        *slot = font;
    }
}

/// The face installed for `family`, if any.
#[must_use]
pub fn system_generic(family: GenericFamily) -> Option<FontId> {
    system_generics()
        .read()
        .ok()
        .and_then(|g| g.get(generic_index(family)).copied().flatten())
}

// ------------------------------------------------------------------ page faces

/// One `@font-face` rule that loaded.
#[derive(Clone, Debug, PartialEq)]
pub struct PageFace {
    /// Lowercased family name.
    pub family: String,
    /// In thousandths of the CSS number (400 is `400_000`).
    pub weight: (i32, i32),
    pub style: FaceStyle,
    /// Empty means every code point.
    pub ranges: Vec<(u32, u32)>,
    pub font: FontId,
}

/// The fonts a page declared, in rule order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PageFonts {
    pub faces: Vec<PageFace>,
    /// Per-page generic overrides (tests, and a future per-profile setting).
    pub generics: [Option<FontId>; 6],
}

/// One face in a resolved family chain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cand {
    pub font: FontId,
    /// `None`: every code point.
    pub ranges: Option<Arc<[(u32, u32)]>>,
}

impl Cand {
    /// Whether `c` is in this candidate's `unicode-range`.
    #[must_use]
    pub fn allows(&self, c: char) -> bool {
        match &self.ranges {
            None => true,
            Some(r) => r.iter().any(|&(a, b)| (a..=b).contains(&u32::from(c))),
        }
    }
}

/// `(bucket, distance)`: smaller is better (CSS Fonts 4 section 5.2.2).
fn weight_rank(want: i32, lo: i32, hi: i32) -> (u8, i32) {
    if lo <= want && want <= hi {
        return (0, 0);
    }
    let w = if hi < want { hi } else { lo };
    if want < 400_000 {
        if w < want {
            (1, want - w)
        } else {
            (2, w - want)
        }
    } else if want > 500_000 {
        if w > want {
            (1, w - want)
        } else {
            (2, want - w)
        }
    } else if w > want && w <= 500_000 {
        (1, w - want)
    } else if w < want {
        (2, want - w)
    } else {
        (3, w - 500_000)
    }
}

fn style_rank(want: FaceStyle, have: FaceStyle) -> u8 {
    use FaceStyle::{Italic, Normal, Oblique};
    let order: [FaceStyle; 3] = match want {
        Italic => [Italic, Oblique, Normal],
        Oblique => [Oblique, Italic, Normal],
        Normal => [Normal, Oblique, Italic],
    };
    order.iter().position(|s| *s == have).unwrap_or(3) as u8
}

impl PageFonts {
    /// Every face declared under `family` (ASCII case-insensitive), best match
    /// first for the requested weight and style. Empty if the page declares none.
    #[must_use]
    pub fn candidates(&self, family: &str, weight: i32, style: FaceStyle) -> Vec<Cand> {
        let lc = family.to_ascii_lowercase();
        let mut hits: Vec<(usize, &PageFace)> = self
            .faces
            .iter()
            .enumerate()
            .filter(|(_, f)| f.family == lc)
            .collect();
        hits.sort_by(|(ia, a), (ib, b)| {
            let ra = (
                style_rank(style, a.style),
                weight_rank(weight, a.weight.0, a.weight.1),
            );
            let rb = (
                style_rank(style, b.style),
                weight_rank(weight, b.weight.0, b.weight.1),
            );
            ra.0.cmp(&rb.0)
                .then(ra.1.0.cmp(&rb.1.0))
                .then(ra.1.1.cmp(&rb.1.1))
                // Later rules win over earlier ones.
                .then(ib.cmp(ia))
        });
        hits.into_iter()
            .map(|(_, f)| Cand {
                font: f.font,
                ranges: if f.ranges.is_empty() {
                    None
                } else {
                    Some(Arc::from(f.ranges.as_slice()))
                },
            })
            .collect()
    }

    /// Override a generic family for this page only.
    pub fn set_generic(&mut self, g: GenericFamily, font: FontId) {
        if let Some(slot) = self.generics.get_mut(generic_index(g)) {
            *slot = Some(font);
        }
    }

    /// The face a generic family resolves to on this page: the page's override,
    /// else the installed system face.
    #[must_use]
    pub fn generic(&self, g: GenericFamily) -> Option<FontId> {
        self.generics
            .get(generic_index(g))
            .copied()
            .flatten()
            .or_else(|| system_generic(g))
    }
}

/// Most `@font-face` rules a page may have loaded; rules past it are ignored.
pub const MAX_FACE_RULES: usize = 32;

/// Load a page's declared fonts.
///
/// `fetch` is called for each `url()` source of each rule, in order, until one
/// yields bytes that register as a font; it is where the embedder applies its
/// network policy, its time budget and `font-display`. Whatever fails is simply
/// absent, and text falls back along the `font-family` list (deterministic: the
/// outcome depends only on which fetches succeeded, never on timing inside this
/// function). The same URL is fetched once per call, and the same bytes
/// register once per process.
pub fn build_page_fonts(
    decls: &[px_css::font_face::FontFaceDecl],
    fetch: &mut dyn FnMut(
        &px_css::font_face::FontFaceDecl,
        &px_css::font_face::FontSource,
    ) -> Option<Vec<u8>>,
) -> PageFonts {
    let mut out = PageFonts::default();
    let mut by_url: HashMap<String, Option<FontId>> = HashMap::new();
    for d in decls.iter().take(MAX_FACE_RULES) {
        let mut found = None;
        for src in &d.sources {
            let id = match by_url.get(&src.url) {
                Some(id) => *id,
                None => {
                    let id = fetch(d, src).and_then(|b| register_font(&b).ok());
                    by_url.insert(src.url.clone(), id);
                    id
                }
            };
            if id.is_some() {
                found = id;
                break;
            }
        }
        if let Some(font) = found {
            out.faces.push(PageFace {
                family: d.family.to_ascii_lowercase(),
                weight: (
                    (d.weight.0 * 1000.0).round() as i32,
                    (d.weight.1 * 1000.0).round() as i32,
                ),
                style: d.style,
                ranges: d.ranges.clone(),
                font,
            });
        }
    }
    out
}

thread_local! {
    static PAGE: RefCell<Option<Arc<PageFonts>>> = const { RefCell::new(None) };
}

/// While alive, `fonts` are the page's declared faces on this thread; dropping it
/// restores the previous ones.
#[must_use = "the fonts apply only while the guard lives"]
pub struct PageFontsGuard(Option<Arc<PageFonts>>);

impl Drop for PageFontsGuard {
    fn drop(&mut self) {
        let prev = self.0.take();
        PAGE.with(|p| *p.borrow_mut() = prev);
    }
}

/// Install `fonts` as the page's declared faces on this thread until the guard
/// drops.
pub fn enter_page_fonts(fonts: Arc<PageFonts>) -> PageFontsGuard {
    PageFontsGuard(PAGE.with(|p| p.borrow_mut().replace(fonts)))
}

/// Run `f` with `fonts` as the page's declared faces on this thread.
pub fn with_page_fonts<R>(fonts: Arc<PageFonts>, f: impl FnOnce() -> R) -> R {
    let _guard = enter_page_fonts(fonts);
    f()
}

/// The faces installed on this thread, if any.
#[must_use]
pub fn current_page_fonts() -> Option<Arc<PageFonts>> {
    PAGE.with(|p| p.borrow().clone())
}

/// Resolve a generic family on the current page.
#[must_use]
pub fn resolve_generic(g: GenericFamily) -> Option<FontId> {
    match current_page_fonts() {
        Some(p) => p.generic(g),
        None => system_generic(g),
    }
}

// ----------------------------------------------------------------------- chains

/// An interned family chain, `Copy` so that `TextStyle` stays `Copy`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ChainId(u32);

impl ChainId {
    /// No chain: the style's primary face alone.
    pub const NONE: ChainId = ChainId(0);
}

fn chains() -> &'static Mutex<Vec<Arc<[Cand]>>> {
    static C: std::sync::OnceLock<Mutex<Vec<Arc<[Cand]>>>> = std::sync::OnceLock::new();
    C.get_or_init(|| Mutex::new(vec![Arc::from(Vec::new())]))
}

/// Intern `chain`; identical chains share an id. At the cap, [`ChainId::NONE`].
pub fn intern_chain(chain: Vec<Cand>) -> ChainId {
    if chain.is_empty() {
        return ChainId::NONE;
    }
    let Ok(mut all) = chains().lock() else {
        return ChainId::NONE;
    };
    if let Some(i) = all.iter().position(|c| c[..] == chain[..]) {
        return ChainId(u32::try_from(i).unwrap_or(0));
    }
    if all.len() >= MAX_CHAINS {
        return ChainId::NONE;
    }
    all.push(Arc::from(chain));
    ChainId(u32::try_from(all.len() - 1).unwrap_or(0))
}

/// The candidates of an interned chain.
#[must_use]
pub fn chain(id: ChainId) -> Arc<[Cand]> {
    chains()
        .lock()
        .ok()
        .and_then(|c| c.get(id.0 as usize).cloned())
        .unwrap_or_else(|| Arc::from(Vec::new()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn face(
        family: &str,
        w: (i32, i32),
        style: FaceStyle,
        ranges: &[(u32, u32)],
        id: u32,
    ) -> PageFace {
        PageFace {
            family: family.to_string(),
            weight: w,
            style,
            ranges: ranges.to_vec(),
            font: FontId(id),
        }
    }

    fn ids(c: &[Cand]) -> Vec<u32> {
        c.iter().map(|c| c.font.0).collect()
    }

    #[test]
    fn weight_matching_follows_css_fonts_4() {
        let p = PageFonts {
            faces: vec![
                face("f", (300_000, 300_000), FaceStyle::Normal, &[], 1),
                face("f", (400_000, 400_000), FaceStyle::Normal, &[], 2),
                face("f", (700_000, 700_000), FaceStyle::Normal, &[], 3),
                face("f", (900_000, 900_000), FaceStyle::Normal, &[], 4),
            ],
            ..PageFonts::default()
        };
        // 400: exact, then (400..500 up) nothing, then below (300), then above.
        assert_eq!(
            ids(&p.candidates("F", 400_000, FaceStyle::Normal)),
            [2, 1, 3, 4]
        );
        // 500: nothing in 500..=500 up, below: 400 then 300, then above 500.
        assert_eq!(
            ids(&p.candidates("f", 500_000, FaceStyle::Normal)),
            [2, 1, 3, 4]
        );
        // 600: above first (700, 900), then below (400, 300).
        assert_eq!(
            ids(&p.candidates("f", 600_000, FaceStyle::Normal)),
            [3, 4, 2, 1]
        );
        // 200: below (none), then above ascending.
        assert_eq!(
            ids(&p.candidates("f", 200_000, FaceStyle::Normal)),
            [1, 2, 3, 4]
        );
        // 700 exact.
        assert_eq!(ids(&p.candidates("f", 700_000, FaceStyle::Normal))[0], 3);
        assert!(p.candidates("other", 400_000, FaceStyle::Normal).is_empty());
    }

    #[test]
    fn ranges_cover_weights_and_style_beats_weight() {
        let p = PageFonts {
            faces: vec![
                face("v", (100_000, 900_000), FaceStyle::Normal, &[], 1),
                face("v", (400_000, 400_000), FaceStyle::Italic, &[], 2),
            ],
            ..PageFonts::default()
        };
        assert_eq!(ids(&p.candidates("v", 650_000, FaceStyle::Normal))[0], 1);
        // Asking for italic prefers the italic face even though the weight
        // range of the other holds 400 too.
        assert_eq!(ids(&p.candidates("v", 400_000, FaceStyle::Italic))[0], 2);
        // Asking for oblique falls to italic before normal.
        assert_eq!(ids(&p.candidates("v", 400_000, FaceStyle::Oblique))[0], 2);
    }

    #[test]
    fn a_later_rule_wins_a_tie_and_ranges_travel_with_the_face() {
        let p = PageFonts {
            faces: vec![
                face(
                    "x",
                    (400_000, 400_000),
                    FaceStyle::Normal,
                    &[(0x41, 0x5a)],
                    1,
                ),
                face(
                    "x",
                    (400_000, 400_000),
                    FaceStyle::Normal,
                    &[(0xe000, 0xe0ff)],
                    2,
                ),
                face("x", (400_000, 400_000), FaceStyle::Normal, &[], 3),
            ],
            ..PageFonts::default()
        };
        let c = p.candidates("x", 400_000, FaceStyle::Normal);
        assert_eq!(ids(&c), [3, 2, 1]);
        assert!(c[0].allows('a'));
        assert!(c[1].allows('\u{e001}') && !c[1].allows('a'));
        assert!(c[2].allows('Q') && !c[2].allows('q'));
    }

    #[test]
    fn chains_intern_by_value_and_the_page_scope_is_restored() {
        let a = intern_chain(vec![Cand {
            font: FontId(7),
            ranges: None,
        }]);
        let b = intern_chain(vec![Cand {
            font: FontId(7),
            ranges: None,
        }]);
        assert_eq!(a, b);
        assert_ne!(a, ChainId::NONE);
        assert_eq!(intern_chain(Vec::new()), ChainId::NONE);
        assert_eq!(chain(a).len(), 1);

        assert!(current_page_fonts().is_none());
        let p = Arc::new(PageFonts::default());
        with_page_fonts(p, || assert!(current_page_fonts().is_some()));
        assert!(current_page_fonts().is_none());
    }
}
