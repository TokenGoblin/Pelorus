//! `TElement` for [`StyleElement`] — the largest of the five trait impls.
//!
//! Its own file because it is 47 items against `TNode`'s 16, and because the
//! honest gaps in this phase are concentrated here: inline `style`, animations,
//! presentational attributes and interaction state are all `TElement` methods,
//! and each one is documented at the method rather than in a list somewhere else.
//!
//! Nothing here contains an `unsafe` block. Five of these methods are declared
//! `unsafe fn` because the trait declares them so — that is ADR 024's whole
//! subject — and the body of an `unsafe fn` needs no `unsafe` block. The writes
//! they perform go through `Cell`s in [`crate::data`].

use crate::dom::{StyleElement, StyleShadowRoot};
use crate::view::StyleNode;

/// The children of one node, as an iterator of nodes.
///
/// Written here rather than reusing `px_dom::Children` because `TElement`
/// requires `TraversalChildrenIterator: Iterator<Item = Self::ConcreteNode>` —
/// the item must be a `StyleNode` carrying the `Dom` borrow, not a bare
/// `NodeId`.
#[derive(Clone, Copy)]
pub struct StyleChildren<'a> {
    next: Option<StyleNode<'a>>,
}

impl<'a> StyleChildren<'a> {
    pub(crate) fn starting_at(first: Option<StyleNode<'a>>) -> Self {
        Self { next: first }
    }
}

impl<'a> Iterator for StyleChildren<'a> {
    type Item = StyleNode<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        use style::dom::TNode as _;
        let current = self.next?;
        self.next = current.next_sibling();
        Some(current)
    }
}

impl<'a> style::dom::TElement for StyleElement<'a> {
    type ConcreteNode = StyleNode<'a>;
    type TraversalChildrenIterator = StyleChildren<'a>;

    fn as_node(&self) -> StyleNode<'a> {
        self.node()
    }

    /// The children stylo should traverse for style.
    ///
    /// The DOM children, because there is no shadow DOM to flatten and no slot
    /// assignment to follow. `LayoutIterator` is stylo's own wrapper, which skips
    /// nodes that need no layout.
    fn traversal_children(&self) -> style::dom::LayoutIterator<StyleChildren<'a>> {
        use style::dom::TNode as _;
        style::dom::LayoutIterator(StyleChildren::starting_at(self.node().first_child()))
    }

    fn is_html_element(&self) -> bool {
        self.qual_name().ns == html5ever::ns!(html)
    }

    fn is_mathml_element(&self) -> bool {
        self.qual_name().ns == html5ever::ns!(mathml)
    }

    fn is_svg_element(&self) -> bool {
        self.qual_name().ns == html5ever::ns!(svg)
    }

    /// The `style` attribute, parsed.
    ///
    /// **`None` in Phase 5, and this is the phase's largest honest gap.** The
    /// return type is a borrow of a parsed, lock-wrapped `PropertyDeclarationBlock`
    /// that must already exist somewhere, so producing one means parsing the
    /// attribute with a `ParserContext` and storing the result at a stable
    /// address — the `ElementData` problem of ADR 026 again, with a parser
    /// attached.
    ///
    /// The consequence is specific and testable: inline `style="..."` does not
    /// participate in the cascade, while author stylesheets do. Named in the
    /// backlog and in the phase report rather than left for a fixture to find.
    fn style_attribute(
        &self,
    ) -> Option<
        style::servo_arc::ArcBorrow<
            '_,
            style::shared_lock::Locked<style::properties::PropertyDeclarationBlock>,
        >,
    > {
        None
    }

    /// No animations before there is a timeline.
    ///
    /// §9 puts animation well after style and there is no event loop until Phase
    /// 13, so this is not a stub standing in for something that exists.
    fn animation_rule(
        &self,
        _: &style::context::SharedStyleContext<'_>,
    ) -> Option<
        style::servo_arc::Arc<
            style::shared_lock::Locked<style::properties::PropertyDeclarationBlock>,
        >,
    > {
        None
    }

    fn transition_rule(
        &self,
        _: &style::context::SharedStyleContext<'_>,
    ) -> Option<
        style::servo_arc::Arc<
            style::shared_lock::Locked<style::properties::PropertyDeclarationBlock>,
        >,
    > {
        None
    }

    /// The interaction state behind `:hover`, `:focus`, `:checked` and friends.
    ///
    /// Empty, because this engine has no input. The computed style is therefore
    /// the one a user sees before touching the page, which is correct until there
    /// is a pointer. See `match_non_ts_pseudo_class` in [`crate::dom`].
    fn state(&self) -> stylo_dom::ElementState {
        stylo_dom::ElementState::empty()
    }

    fn has_part_attr(&self) -> bool {
        false
    }

    fn exports_any_part(&self) -> bool {
        false
    }

    /// The interned `id`.
    ///
    /// Returns a *reference*, which is why ADR 026's table interns ids when it is
    /// built rather than answering from the attribute on demand: html5ever stores
    /// attribute values as string tendrils, so there would be no interned atom to
    /// lend out.
    ///
    /// The return type is the raw atom, not the `AtomIdent` wrapper `each_class`
    /// below hands out — the trait is not consistent about which it wants, and
    /// `AtomIdent` derefs to exactly this, so the conversion is `&**`.
    fn id(&self) -> Option<&style::Atom> {
        self.data().id_of(self.node().id()).map(|ident| &**ident)
    }

    fn each_class<F>(&self, mut callback: F)
    where
        F: FnMut(&style::values::AtomIdent),
    {
        for class in self.data().classes_of(self.node().id()) {
            callback(class);
        }
    }

    /// Custom states belong to custom elements, which style does not reach.
    fn each_custom_state<F>(&self, _callback: F)
    where
        F: FnMut(&style::values::AtomIdent),
    {
    }

    /// The wrapped name type here, and the raw one in `local_name` below.
    ///
    /// `each_attr_name`'s callback takes stylo's `LocalName`, which is
    /// `GenericAtomIdent<LocalNameStaticSet>`, while `local_name` returns
    /// `web_atoms::LocalName`, which is the bare `Atom`. Same trait, same concept,
    /// two spellings — hence [`crate::dom::local_name_of`] for one and a plain
    /// borrow for the other.
    fn each_attr_name<F>(&self, mut callback: F)
    where
        F: FnMut(&style::values::GenericAtomIdent<web_atoms::LocalNameStaticSet>),
    {
        for attr in self.attrs() {
            callback(crate::dom::local_name_of(&attr.name));
        }
    }

    fn has_dirty_descendants(&self) -> bool {
        self.data().has_dirty_descendants(self.node().id())
    }

    /// Whether `px-dom` recorded a pre-mutation snapshot for this element.
    ///
    /// Phase 4 built those for exactly this consumer: stylo compares an element's
    /// prior state against its current one to decide what to invalidate.
    fn has_snapshot(&self) -> bool {
        self.node().arena().has_snapshot(self.node().id())
    }

    fn handled_snapshot(&self) -> bool {
        self.data().handled_snapshot(self.node().id())
    }

    // The five `unsafe fn`s ADR 024 exists for. No `unsafe` block in any body.
    unsafe fn set_handled_snapshot(&self) {
        self.data().set_handled_snapshot(self.node().id());
    }

    unsafe fn set_dirty_descendants(&self) {
        self.data().set_dirty_descendants(self.node().id(), true);
    }

    unsafe fn unset_dirty_descendants(&self) {
        self.data().set_dirty_descendants(self.node().id(), false);
    }

    unsafe fn ensure_data(&self) -> style::data::ElementDataMut<'_> {
        self.data()
            .ensure(self.node().id())
            .expect("the style table is sized for every slot in this arena (ADR 026)")
    }

    unsafe fn clear_data(&self) {
        self.data().clear(self.node().id());
    }

    fn has_data(&self) -> bool {
        self.data().has_data(self.node().id())
    }

    fn borrow_data(&self) -> Option<style::data::ElementDataRef<'_>> {
        self.data().borrow(self.node().id())
    }

    fn mutate_data(&self) -> Option<style::data::ElementDataMut<'_>> {
        self.data().mutate(self.node().id())
    }

    fn store_children_to_process(&self, n: isize) {
        self.data().store_children_to_process(self.node().id(), n);
    }

    fn did_process_child(&self) -> isize {
        self.data().did_process_child(self.node().id())
    }

    /// Whether to skip the `display` fixup that blockifies flex and grid items.
    ///
    /// False: the fixup applies. Servo returns true only for pseudo-elements it
    /// synthesises, and there are none here.
    fn skip_item_display_fixup(&self) -> bool {
        false
    }

    fn may_have_animations(&self) -> bool {
        false
    }

    fn has_animations(&self, _: &style::context::SharedStyleContext<'_>) -> bool {
        false
    }

    fn has_css_animations(
        &self,
        _: &style::context::SharedStyleContext<'_>,
        _: Option<style::selector_parser::PseudoElement>,
    ) -> bool {
        false
    }

    fn has_css_transitions(
        &self,
        _: &style::context::SharedStyleContext<'_>,
        _: Option<style::selector_parser::PseudoElement>,
    ) -> bool {
        false
    }

    fn shadow_root(&self) -> Option<StyleShadowRoot<'a>> {
        None
    }

    fn containing_shadow(&self) -> Option<StyleShadowRoot<'a>> {
        None
    }

    fn lang_attr(&self) -> Option<style::values::AtomString> {
        self.attr_no_ns(&html5ever::local_name!("lang"))
            .map(style::values::AtomString::from)
    }

    /// `:lang()` matching.
    ///
    /// Walks to the nearest ancestor carrying a `lang` attribute, because `lang`
    /// applies down the tree rather than being repeated on every element, then
    /// compares by BCP 47 language-range rules: ASCII-case-insensitive, and a
    /// prefix match only at a subtag boundary. Without the boundary check `en`
    /// would match `english`.
    fn match_element_lang(
        &self,
        override_lang: Option<Option<style::values::AtomString>>,
        value: &std::boxed::Box<str>,
    ) -> bool {
        use style::dom::TNode as _;

        let lang = match override_lang {
            Some(overridden) => overridden,
            None => {
                let mut cursor = Some(*self);
                let mut found = None;
                while let Some(el) = cursor {
                    if let Some(l) = style::dom::TElement::lang_attr(&el) {
                        found = Some(l);
                        break;
                    }
                    cursor = el.node().parent_node().and_then(StyleElement::new);
                }
                found
            }
        };
        let Some(lang) = lang else { return false };
        let actual = lang.to_ascii_lowercase();
        let wanted = value.to_ascii_lowercase();
        if !actual.starts_with(&wanted) {
            return false;
        }
        actual.len() == wanted.len() || actual.as_bytes().get(wanted.len()) == Some(&b'-')
    }

    /// Whether this is the `<body>` of an HTML document.
    ///
    /// The cascade needs it: `<body>`'s background propagates to the canvas.
    fn is_html_document_body_element(&self) -> bool {
        use style::dom::TNode as _;
        let name = self.qual_name();
        if name.ns != html5ever::ns!(html) || name.local != html5ever::local_name!("body") {
            return false;
        }
        // Servo additionally requires the parent to be the root <html>, which is
        // what stops a stray <body> deeper in the tree from claiming the canvas.
        self.node()
            .parent_node()
            .and_then(StyleElement::new)
            .is_some_and(|p| p.qual_name().local == html5ever::local_name!("html"))
    }

    /// Presentational attributes such as `width="100"` and `bgcolor`.
    ///
    /// **Not synthesised in Phase 5.** These are the legacy HTML attributes that
    /// map to CSS declarations at the user-agent origin. Each is a small mapping
    /// and there are dozens; they matter mostly for old markup. Concretely:
    /// `<table border="1">` gets no border, while the same table styled with CSS
    /// does. In the backlog and in the phase report.
    fn synthesize_presentational_hints_for_legacy_attributes<V>(
        &self,
        _: selectors::context::VisitedHandlingMode,
        _: &mut V,
    ) where
        V: selectors::sink::Push<style::applicable_declarations::ApplicableDeclarationBlock>,
    {
    }

    fn local_name(&self) -> &web_atoms::LocalName {
        &self.qual_name().local
    }

    fn namespace(&self) -> &web_atoms::Namespace {
        &self.qual_name().ns
    }

    /// The containing block size for container queries.
    ///
    /// Unknown in both axes, which is what `None` means. Container queries need
    /// layout to have run and layout is Phase 6 — a size invented here would be a
    /// wrong answer rather than a missing one.
    fn query_container_size(
        &self,
        _: &style::values::computed::Display,
    ) -> euclid::Size2D<Option<app_units::Au>, euclid::UnknownUnit> {
        euclid::Size2D::new(None, None)
    }

    fn has_selector_flags(&self, flags: selectors::matching::ElementSelectorFlags) -> bool {
        self.data().has_selector_flags(self.node().id(), flags)
    }

    /// Which way a relative selector (`:has()`) should search from here.
    ///
    /// Read back from the flags the selector engine set during matching, one bit
    /// at a time because the stored value is a union and the caller wants only
    /// the direction bits.
    fn relative_selector_search_direction(&self) -> selectors::matching::ElementSelectorFlags {
        use selectors::matching::ElementSelectorFlags as F;
        let mut found = F::empty();
        for flag in [
            F::RELATIVE_SELECTOR_SEARCH_DIRECTION_ANCESTOR,
            F::RELATIVE_SELECTOR_SEARCH_DIRECTION_SIBLING,
        ] {
            if self.data().has_selector_flags(self.node().id(), flag) {
                found |= flag;
            }
        }
        found
    }

    /// `ElementContext::get_attr`, which stylo's blanket impl leaves to us.
    ///
    /// Takes the *wrapped* name types, unlike `local_name`. Compared through
    /// `GenericAtomIdent`'s `Deref` rather than by casting our side, which is the
    /// same conversion in the other direction.
    fn get_attr(
        &self,
        attr: &style::values::GenericAtomIdent<web_atoms::LocalNameStaticSet>,
        namespace: &style::values::GenericAtomIdent<web_atoms::NamespaceStaticSet>,
    ) -> Option<String> {
        self.attrs()
            .iter()
            .find(|a| a.name.local == **attr && a.name.ns == **namespace)
            .map(|a| a.value.to_string())
    }
}
