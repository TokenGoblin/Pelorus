//! The style engine: a `Stylist`, a `Device`, and the two stubs stylo requires
//! of any embedder.
//!
//! This is the half of Phase 5 that is not trait plumbing. [`crate::dom`] and
//! [`crate::element`] teach stylo how to read `px-dom`; this assembles the
//! context stylo needs before it will read anything, and drives the cascade.

use px_dom::Arena;
use style::context::QuirksMode;
use style::media_queries::MediaType;
use style::stylist::Stylist;

/// Font metrics, before there is a font system.
///
/// §9 puts text shaping in Phase 9; `px-text` is an empty skeleton. stylo needs
/// a `FontMetricsProvider` to construct a `Device` because font-relative units
/// resolve through it, so this answers with the CSS initial values instead of
/// measuring anything.
///
/// **What this makes wrong, precisely.** `ex`, `ch`, `ic` and `cap` units, and
/// `font-size: xx-small`-style keywords relative to a generic family, are
/// computed against a 16px base with no real metrics behind it. `em` and `rem`
/// are *not* affected — they resolve against `font-size` itself, which is a
/// computed value this engine does have. So the property set in
/// `crates/px-css/tests/properties.toml` can be checked honestly as long as its
/// fixtures use `em`, `rem`, percentages and absolute units, which they do.
///
/// The day `px-text` exists this is the seam to replace, and it is one trait
/// with two methods.
#[derive(Debug)]
struct InitialFontMetrics;

impl style::device::servo::FontMetricsProvider for InitialFontMetrics {
    fn query_font_metrics(
        &self,
        _vertical: bool,
        _font: &style::properties::style_structs::Font,
        _base_size: style::values::computed::CSSPixelLength,
        _flags: style::values::computed::font::QueryFontMetricsFlags,
    ) -> style::font_metrics::FontMetrics {
        // All-`None` metrics. stylo falls back to its own ratios, which is the
        // same thing it does for a font it cannot measure.
        style::font_metrics::FontMetrics::default()
    }

    /// 16 CSS pixels for every generic family.
    ///
    /// The conventional default, and the same number for `serif`, `sans-serif`
    /// and `monospace` — real browsers use 13px for `monospace`, which is a
    /// deliberate difference this cannot reproduce without a font system.
    fn base_size_for_generic(
        &self,
        _generic: style::values::computed::font::GenericFontFamily,
    ) -> style::values::computed::Length {
        style::values::computed::Length::new(16.0)
    }
}

/// No Houdini paint worklets, and there never will be in this engine.
///
/// `SharedStyleContext` holds `&dyn RegisteredSpeculativePainters` and does not
/// accept `None`, so an embedder must supply a registry even to say it has none.
#[derive(Debug)]
struct NoPainters;

impl style::context::RegisteredSpeculativePainters for NoPainters {
    fn get(
        &self,
        _name: &style::Atom,
    ) -> Option<&dyn style::context::RegisteredSpeculativePainter> {
        None
    }
}

/// A style engine for one document.
///
/// Owns the `Stylist` (the indexed stylesheets), the `Device` (viewport and
/// media state), and the lock guarding stylesheet contents.
pub struct StyleEngine {
    stylist: Stylist,
    shared_lock: style::shared_lock::SharedRwLock,
    /// The document's base URL, for resolving `url()` in inline `style`.
    ///
    /// `about:blank` until a document is loaded against a real URL. Relative
    /// references in an inline style resolve against it, so it is the document's
    /// URL rather than a stylesheet's.
    url_data: style::stylesheets::UrlExtraData,
    snapshots: style::selector_parser::SnapshotMap,
    painters: NoPainters,
}

impl StyleEngine {
    /// A style engine for a viewport of `width` × `height` CSS pixels.
    #[must_use]
    pub fn new(width: f32, height: f32, quirks_mode: QuirksMode) -> Self {
        let viewport = euclid::Size2D::new(width, height);
        let device = style::device::Device::new(
            MediaType::screen(),
            quirks_mode,
            viewport,
            // Device pixels equal CSS pixels: no HiDPI scaling to model before
            // there is a window (Phase 8 owns the surface).
            euclid::Size2D::new(width, height),
            euclid::Scale::new(1.0),
            Box::new(InitialFontMetrics),
            style::properties::ComputedValues::initial_values_with_font_override(
                style::properties::style_structs::Font::initial_values(),
            ),
            // Light, and not read from the OS. A user-agent that follows the
            // system theme is a fingerprinting surface (§1's threat model names
            // the cross-site tracker), so this is a deliberate constant until
            // there is a setting for it.
            style::queries::values::PrefersColorScheme::Light,
            style::servo::media_features::PointerCapabilities::FINE,
            style::servo::media_features::PointerCapabilities::FINE,
        );

        Self {
            stylist: Stylist::new(device, quirks_mode),
            shared_lock: style::shared_lock::SharedRwLock::new(),
            url_data: style::stylesheets::UrlExtraData(style::servo_arc::Arc::new(
                url::Url::parse("about:blank").expect("a valid literal"),
            )),
            snapshots: style::selector_parser::SnapshotMap::new(),
            painters: NoPainters,
        }
    }

    /// The lock guarding this engine's stylesheet contents.
    ///
    /// A document's [`crate::view::StyleRoot`] must be built with this same lock:
    /// stylo reads declarations through a guard derived from it, and a guard from
    /// a different lock would not grant access to these stylesheets.
    #[must_use]
    pub fn shared_lock(&self) -> &style::shared_lock::SharedRwLock {
        &self.shared_lock
    }

    /// Parse `css` as an author stylesheet and add it to the cascade.
    ///
    /// Errors are not returned, because CSS parsing does not fail as a unit — a
    /// malformed declaration is discarded and the rest of the sheet applies.
    /// That is the spec's behaviour, not leniency: §CSS Syntax requires it, and a
    /// stylesheet that stopped at the first unknown property would break every
    /// page using a feature this engine has not implemented.
    pub fn add_author_stylesheet(&mut self, css: &str, url: &str) {
        use style::stylesheets::{AllowImportRules, Origin, Stylesheet};

        // `about:invalid` when the caller hands us something unparseable. The
        // URL is only used to resolve relative `url()` values inside the sheet,
        // so a sheet with a bad base still cascades -- it just cannot resolve
        // relative references, which is the same outcome as a 404 on them.
        let base = url::Url::parse(url)
            .unwrap_or_else(|_| url::Url::parse("about:invalid").expect("a valid literal"));
        let url_data = style::stylesheets::UrlExtraData(style::servo_arc::Arc::new(base));
        let sheet = Stylesheet::from_str(
            css,
            url_data,
            Origin::Author,
            // servo_arc::Arc, not std::sync::Arc. They are different types with
            // the same name and stylo wants its own; importing either one by
            // bare name is how you spend twenty minutes on "expected
            // `Arc<Locked<MediaList>>`, found a different `Arc<Locked<MediaList>>`".
            style::servo_arc::Arc::new(
                self.shared_lock
                    .wrap(style::media_queries::MediaList::empty()),
            ),
            self.shared_lock.clone(),
            None,
            None,
            QuirksMode::NoQuirks,
            AllowImportRules::No,
        );

        let guard = self.shared_lock.read();
        self.stylist.append_stylesheet(
            style::stylesheets::DocumentStyleSheet(style::servo_arc::Arc::new(sheet)),
            &guard,
        );
        self.stylist
            .force_stylesheet_origins_dirty(Origin::Author.into());
        // `flush` takes only the guards in 0.21: the element and snapshot-map
        // arguments Servo's older call sites pass are gone.
        self.stylist
            .flush(&style::shared_lock::StylesheetGuards::same(&guard));
    }

    /// A parser for inline `style` attributes, bound to this engine's lock.
    ///
    /// Hand this to `StyleData::for_arena_with_style_attributes` — or use
    /// [`Self::style_root_for`], which does it for you — so inline declarations
    /// are wrapped in the lock the cascade reads through.
    #[must_use]
    pub fn style_attribute_parser(&self) -> crate::data::StyleAttributeParser<'_> {
        crate::data::StyleAttributeParser::new(
            &self.url_data,
            &self.shared_lock,
            self.stylist.quirks_mode(),
        )
    }

    /// Build a `StyleRoot` for `arena` wired to this engine.
    ///
    /// The one call that gets every coupling right: the engine's shared lock, its
    /// quirks mode, and inline `style` attributes parsed against its base URL.
    /// Assembling a `StyleRoot` by hand is possible and is how the two silent
    /// failures documented on `StyleRoot::new` and `StyleAttributeParser` happen.
    #[must_use]
    pub fn style_root_for(&self, arena: &Arena) -> crate::view::StyleRoot {
        crate::view::StyleRoot::with_data(
            self.shared_lock.clone(),
            self.stylist.quirks_mode(),
            crate::data::StyleData::for_arena_with_style_attributes(
                arena,
                &self.style_attribute_parser(),
            ),
        )
    }

    /// The stylist, for callers that need to resolve style themselves.
    #[must_use]
    pub fn stylist(&self) -> &Stylist {
        &self.stylist
    }

    /// Build the `SharedStyleContext` a traversal runs against.
    fn shared_context<'a>(
        &'a self,
        guard: &'a style::shared_lock::SharedRwLockReadGuard<'a>,
    ) -> style::context::SharedStyleContext<'a> {
        style::context::SharedStyleContext {
            stylist: &self.stylist,
            visited_styles_enabled: false,
            options: style::context::StyleSystemOptions::default(),
            guards: style::shared_lock::StylesheetGuards::same(guard),
            // No animations, so no timeline. Zero rather than a wall-clock read:
            // a style pass that varies with the clock is a style pass whose
            // output is not reproducible, and invariant 7's habits are worth
            // keeping even where they are not required.
            current_time_for_animations: 0.0,
            traversal_flags: style::traversal_flags::TraversalFlags::empty(),
            snapshot_map: &self.snapshots,
            animations: Default::default(),
            registered_speculative_painters: &self.painters,
        }
    }
}

/// Everything needed to resolve style for one arena, borrowed together.
///
/// Separate from [`StyleEngine`] because the engine outlives a pass and the
/// borrows do not.
pub struct StylePass<'a> {
    engine: &'a StyleEngine,
    arena: &'a Arena,
}

impl<'a> StylePass<'a> {
    /// Prepare a pass over `arena`.
    #[must_use]
    pub fn new(engine: &'a StyleEngine, arena: &'a Arena) -> Self {
        Self { engine, arena }
    }

    /// The arena this pass reads.
    #[must_use]
    pub fn arena(&self) -> &'a Arena {
        self.arena
    }

    /// The engine this pass resolves against.
    #[must_use]
    pub fn engine(&self) -> &'a StyleEngine {
        self.engine
    }
}

/// The traversal stylo drives to recalculate style over a document.
///
/// Servo's equivalent is about forty-five lines, and the research note's summary
/// holds: *"the traversal driver is not the hard part; the `TElement` impl is."*
/// Everything interesting has already happened in [`crate::element`].
pub struct StyleTraversal<'a> {
    shared: style::context::SharedStyleContext<'a>,
}

impl<'a, 'dom> style::traversal::DomTraversal<crate::dom::StyleElement<'dom>>
    for StyleTraversal<'a>
{
    fn process_preorder<F>(
        &self,
        traversal_data: &style::traversal::PerLevelTraversalData,
        context: &mut style::context::StyleContext<crate::dom::StyleElement<'dom>>,
        node: crate::view::StyleNode<'dom>,
        note_child: F,
    ) where
        F: FnMut(crate::view::StyleNode<'dom>),
    {
        use style::dom::TNode as _;

        // Text nodes and comments have no style of their own; they inherit from
        // the parent at box-tree construction. Only elements are recalculated.
        let Some(element) = node.as_element() else {
            return;
        };

        // `ensure_data` is stylo's own precondition for `recalc_style_at`, and it
        // is one of the five `unsafe fn`s ADR 024 exists for. The call is in an
        // `unsafe` block because *calling* an unsafe fn needs one -- what ADR 024
        // says is that the crate contains no unsafe block, and this is the one
        // place that would break it.
        //
        // It does not, because `StyleData::ensure` is safe: the slot already
        // exists (ADR 026), so the allocation race stylo's `unsafe` warns about
        // cannot occur. The safe method is called directly and the trait method
        // is bypassed.
        let mut data = element
            .data()
            .ensure(element.id())
            .expect("ADR 026: the table is sized for every slot in this arena");

        style::traversal::recalc_style_at(
            self,
            traversal_data,
            context,
            element,
            &mut data,
            note_child,
        );
    }

    /// Nothing to do on the way up.
    ///
    /// Servo returns false from `needs_postorder_traversal` for the same reason:
    /// the post-order pass exists for Gecko's frame construction, and this engine
    /// builds boxes in Phase 6 from the computed style rather than during the
    /// style traversal.
    fn process_postorder(
        &self,
        _context: &mut style::context::StyleContext<crate::dom::StyleElement<'dom>>,
        _node: crate::view::StyleNode<'dom>,
    ) {
        unreachable!("needs_postorder_traversal() is false, so stylo never calls this")
    }

    fn needs_postorder_traversal() -> bool {
        false
    }

    fn shared_context(&self) -> &style::context::SharedStyleContext<'a> {
        &self.shared
    }
}

impl StyleEngine {
    /// Resolve computed style for every element in `arena`.
    ///
    /// `root` must have been built with [`Self::shared_lock`]; see
    /// [`crate::view::StyleRoot::new`] for what happens when it was not.
    ///
    /// Returns the number of elements whose style was resolved, or `None` if the
    /// document has no root element — an arena holding only the document node,
    /// which is what an empty parse produces.
    ///
    /// **Sequential.** `traverse_dom`'s `pool` argument is `None`, per ADR 023:
    /// stylo's thread-safety invariants are assumed rather than checked, and
    /// taking them in the same phase that first implements `TElement` would make
    /// a data race and a trait bug indistinguishable.
    pub fn resolve(&self, arena: &Arena, root: &crate::view::StyleRoot) -> Option<usize> {
        mark_thread_as_layout();

        // Scoped rather than constructed: the node records and their context are
        // two locals that reference each other, so they cannot outlive this call
        // and a `Dom` cannot be returned from one. See `view::with_dom`.
        crate::view::with_dom(arena, root, |dom| self.resolve_in(dom))?
    }

    /// The body of [`Self::resolve`], inside the `with_dom` scope.
    fn resolve_in(&self, dom: crate::view::Dom<'_>) -> Option<usize> {
        use style::dom::{TDocument as _, TElement as _, TNode as _};
        use style::traversal::DomTraversal as _;

        let document = crate::dom::StyleDocument::new(dom)?;

        // The root element, which is <html> for any parsed document. Found by
        // walking children rather than assumed, because a fragment parse or a
        // document built by hand need not have one.
        let mut root_element = None;
        let mut child = document.as_node().first_child();
        while let Some(node) = child {
            if let Some(element) = node.as_element() {
                root_element = Some(element);
                break;
            }
            child = node.next_sibling();
        }
        let root_element = root_element?;

        let guard = self.shared_lock.read();
        let traversal = StyleTraversal {
            shared: self.shared_context(&guard),
        };

        let token = <StyleTraversal<'_> as style::traversal::DomTraversal<
            crate::dom::StyleElement<'_>,
        >>::pre_traverse(root_element, traversal.shared_context());

        if !token.should_traverse() {
            return Some(0);
        }

        style::driver::traverse_dom(&traversal, token, None);

        // Count what actually got style, rather than reporting what was asked
        // for. A traversal that silently skipped a subtree would otherwise look
        // identical to one that styled it.
        let mut styled = 0usize;
        let mut stack = vec![root_element];
        while let Some(element) = stack.pop() {
            if element.has_data() {
                styled += 1;
            }
            let mut child = element.as_node().first_child();
            while let Some(node) = child {
                if let Some(el) = node.as_element() {
                    stack.push(el);
                }
                child = node.next_sibling();
            }
        }
        Some(styled)
    }
}

/// Translate `px-dom`'s quirks mode into stylo's.
///
/// Two distinct enums with the same three variants and the same meaning:
/// `px-dom` reports html5ever's `QuirksMode` (the parser decided it from the
/// DOCTYPE), and stylo wants `selectors::matching::QuirksMode`. Nothing converts
/// between them, so the mapping is written out.
///
/// This matters more than a type adapter usually does. Quirks mode changes the
/// cascade — most visibly it makes unitless lengths legal — so passing
/// `NoQuirks` for a document the parser put in quirks mode is a whole class of
/// wrong computed values on exactly the old pages that need the quirk. Writing
/// `QuirksMode::NoQuirks` at the call site is easy and silent, which is why this
/// exists rather than leaving callers to map it themselves.
#[must_use]
pub fn quirks_mode_of(dom: &px_dom::Dom) -> QuirksMode {
    match dom.quirks_mode {
        html5ever::interface::QuirksMode::Quirks => QuirksMode::Quirks,
        html5ever::interface::QuirksMode::LimitedQuirks => QuirksMode::LimitedQuirks,
        html5ever::interface::QuirksMode::NoQuirks => QuirksMode::NoQuirks,
    }
}

/// Register this thread as a layout thread, once.
///
/// stylo asserts `thread_state::get().contains(ThreadState::LAYOUT)` when it
/// builds a `StyleContext`, and panics otherwise:
///
/// ```text
/// assertion failed: thread_state::get().contains(ThreadState::LAYOUT)
/// ```
///
/// This is the second embedder requirement in this phase that appears nowhere in
/// a trait or a function signature — the first was ADR 027's pointer-sized
/// element. Servo satisfies it by initialising its layout threads on creation; a
/// library embedding stylo has to do it wherever style actually runs.
///
/// `initialize` panics if called twice with *different* states, so it is guarded
/// by a `Once` rather than called on every pass. A `Once` rather than an
/// idempotency check inside stylo, because there is no way to ask stylo whether a
/// thread is initialised without also asserting what it was initialised to.
fn mark_thread_as_layout() {
    use std::sync::Once;
    // Per-thread rather than per-process: `thread_state` is a thread-local, so a
    // process-wide `Once` would initialise the first thread to call `resolve` and
    // leave every other one failing the assertion. Phase 5 resolves on one thread
    // (ADR 023 passes `pool: None`), and this is written so that stops being true
    // safely.
    thread_local! {
        static MARKED: Once = const { Once::new() };
    }
    MARKED.with(|once| {
        once.call_once(|| {
            style::thread_state::initialize(style::thread_state::ThreadState::LAYOUT);
        });
    });
}
