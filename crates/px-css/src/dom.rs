//! The four stylo DOM traits, implemented over `px-dom`.
//!
//! `TNode`, `TElement`, `TDocument` and `TShadowRoot` are mutually referential —
//! each names the others as associated types — so none of them can be
//! implemented alone and all four types are declared here together.
//!
//! `selectors::Element` is the fifth trait, a supertrait of `TElement`, and is
//! the selector-matching half of the same surface. `ElementContext`, which
//! `TElement` also requires, needs nothing: stylo ships a blanket
//! `impl<T: TElement> ElementContext for T`.
//!
//! Every view here is `Copy` and holds a [`Dom`], which is a shared borrow of
//! the arena and the style root together. See [`crate::view`] for why that
//! borrow is what makes §4.1's generational handles and stylo's infallible
//! traversal coexist.

use px_dom::NodeId;

use crate::view::{Dom, StyleNode};

/// An element, which is a node that has been checked to be one.
///
/// Separate from [`StyleNode`] because stylo's traits are: `TNode` is any node,
/// `TElement` is specifically an element, and the conversion between them is
/// fallible in one direction. Holding the node inside rather than duplicating
/// its fields keeps the two in step.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct StyleElement<'a> {
    node: StyleNode<'a>,
}

impl<'a> StyleElement<'a> {
    /// View `node` as an element, or `None` if it is not one.
    ///
    /// The only constructor, so a `StyleElement` that exists is an element —
    /// which is what lets the `TElement` methods below read `element_name` and
    /// `attrs` without re-checking the node kind at every call.
    #[must_use]
    pub fn new(node: StyleNode<'a>) -> Option<Self> {
        node.arena()
            .get(node.id())?
            .element_name()
            .map(|_| Self { node })
    }

    /// This element as a node.
    #[must_use]
    pub fn node(self) -> StyleNode<'a> {
        self.node
    }

    /// The handle this element resolved from.
    #[must_use]
    pub fn id(self) -> NodeId {
        self.node.id()
    }
}

impl core::fmt::Debug for StyleElement<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("StyleElement")
            .field("id", &self.node.id())
            .finish()
    }
}

impl core::hash::Hash for StyleElement<'_> {
    /// `TElement: Hash`. Hashes the handle only.
    ///
    /// Not the arena pointer, even though `PartialEq` compares it. stylo hashes
    /// elements into per-traversal caches keyed within one document, so the
    /// arena is constant across every element that meets in a bucket; including
    /// it would cost a word of hashing to distinguish values that never collide
    /// in practice. `Hash` stays consistent with `Eq` because equal elements
    /// have equal handles.
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        self.node.id().hash(state);
    }
}

/// The document, as stylo sees it.
///
/// Carries no node of its own: `px-dom`'s document is always the arena's root
/// handle, so this is the [`Dom`] borrow and nothing else.
#[derive(Clone, Copy, PartialEq)]
pub struct StyleDocument<'a> {
    node: StyleNode<'a>,
}

impl<'a> StyleDocument<'a> {
    /// The document view for this arena, or `None` if its root will not resolve.
    ///
    /// The resolved node is stored rather than looked up on demand, and that is
    /// forced by the trait: `TDocument::as_node` returns a node infallibly. §4.1
    /// still gets its fallible boundary — it is here, at construction, once —
    /// which is the same shape as [`StyleNode::new`] and for the same reason.
    #[must_use]
    pub fn new(dom: Dom<'a>) -> Option<Self> {
        dom.node(dom.arena().document()).map(|node| Self { node })
    }

    /// The document node.
    #[must_use]
    pub fn node(self) -> StyleNode<'a> {
        self.node
    }

    /// The arena and style root this document borrows.
    #[must_use]
    pub fn dom(self) -> Dom<'a> {
        self.node.dom()
    }

    /// Wrap a node already known to be the document.
    ///
    /// `pub(crate)`, and the caller is `TNode::owner_doc`, which stylo declares
    /// infallible. The node comes from [`Dom::document_node`], which is the one
    /// place a `StyleNode` is built without a second generation check, and that
    /// function documents why it is sound.
    pub(crate) fn from_document_node(node: StyleNode<'a>) -> Self {
        Self { node }
    }
}

impl core::fmt::Debug for StyleDocument<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("StyleDocument").finish_non_exhaustive()
    }
}

/// A shadow root — **uninhabited in Phase 5**.
///
/// `px-dom` cannot construct a shadow tree and §9 puts shadow DOM well after
/// style, but `TNode` requires `ConcreteShadowRoot` to name a type that
/// implements `TShadowRoot`, whose methods return hosts and cascade data
/// infallibly.
///
/// The usual way to fill that in is `unreachable!()`. This does it in the type
/// system instead: the `Infallible` field means no value of this type can ever
/// exist, so every method body is `match self.never {}` — an exhaustive match
/// over zero variants, which the compiler accepts and which cannot panic at
/// runtime because it cannot be reached at runtime. A `todo!()` here would be a
/// live panic path inside a style traversal, waiting for the first page with a
/// shadow root.
///
/// When shadow DOM arrives, this field becomes a `StyleNode` and the bodies stop
/// compiling one at a time — which is the right way to find them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct StyleShadowRoot<'a> {
    never: core::convert::Infallible,
    _dom: core::marker::PhantomData<Dom<'a>>,
}

// ---------------------------------------------------------------------------
// Trait impls, being filled in. Empty bodies here so the compiler enumerates
// exactly what each trait requires against this version of stylo, rather than
// working from a trait listing that may not match the published crate.
// ---------------------------------------------------------------------------

impl style::dom::NodeInfo for StyleNode<'_> {
    /// Whether this node is an element.
    ///
    /// Asked through `px-dom`'s `element_name()`, which is `Some` exactly for
    /// element nodes. A node whose handle no longer resolves answers `false`
    /// rather than panicking -- it cannot happen while the `&Arena` borrow is
    /// held, and answering is cheaper than arguing about it.
    fn is_element(&self) -> bool {
        self.arena()
            .get(self.id())
            .is_some_and(|n| n.element_name().is_some())
    }

    fn is_text_node(&self) -> bool {
        self.arena()
            .get(self.id())
            .is_some_and(|n| n.text().is_some())
    }
}

impl<'a> style::dom::TDocument for StyleDocument<'a> {
    type ConcreteNode = StyleNode<'a>;

    fn as_node(&self) -> Self::ConcreteNode {
        self.node
    }

    /// Every document this engine parses is an HTML document.
    ///
    /// `px-dom` drives `html5ever` and has no XML path, so this is true rather
    /// than unimplemented. It stops being a constant the day XML arrives, and
    /// the cascade cares -- it is what makes tag names ASCII-case-insensitive.
    fn is_html_document(&self) -> bool {
        true
    }

    fn quirks_mode(&self) -> style::context::QuirksMode {
        self.node.dom().root().quirks_mode()
    }

    fn shared_lock(&self) -> &style::shared_lock::SharedRwLock {
        self.node.dom().root().shared_lock()
    }
}

impl<'a> style::dom::TShadowRoot for StyleShadowRoot<'a> {
    type ConcreteNode = StyleNode<'a>;

    // Every body here is an exhaustive match over an uninhabited type. See the
    // type's own documentation: this is Phase 5 having no shadow DOM, expressed
    // so that it cannot panic rather than so that it panics with a good message.
    fn as_node(&self) -> Self::ConcreteNode {
        match self.never {}
    }

    fn host(&self) -> StyleElement<'a> {
        match self.never {}
    }

    fn style_data<'b>(&self) -> Option<&'b style::stylist::CascadeData>
    where
        Self: 'b,
    {
        match self.never {}
    }
}

impl<'a> style::dom::TNode for StyleNode<'a> {
    type ConcreteElement = StyleElement<'a>;
    type ConcreteDocument = StyleDocument<'a>;
    type ConcreteShadowRoot = StyleShadowRoot<'a>;

    // The five tree accessors. Every one returns Option<Self> in stylo's own
    // signature, which is why §4.1's generational handles needed no adaptation
    // here at all -- see the correction in docs/research/stylo-requirements.md
    // §7, where the research note predicted the opposite.
    //
    // `?` on the arena lookup, then `?` on the link, then a re-resolve. The
    // re-resolve cannot fail while the borrow is held; it is written rather than
    // asserted because that is what §4.1 asks for.
    fn parent_node(&self) -> Option<Self> {
        self.resolve(self.arena().get(self.id())?.parent()?)
    }

    fn first_child(&self) -> Option<Self> {
        self.resolve(self.arena().get(self.id())?.first_child()?)
    }

    fn last_child(&self) -> Option<Self> {
        self.resolve(self.arena().get(self.id())?.last_child()?)
    }

    fn prev_sibling(&self) -> Option<Self> {
        self.resolve(self.arena().get(self.id())?.prev_sibling()?)
    }

    fn next_sibling(&self) -> Option<Self> {
        self.resolve(self.arena().get(self.id())?.next_sibling()?)
    }

    fn owner_doc(&self) -> StyleDocument<'a> {
        StyleDocument::from_document_node(self.dom().document_node())
    }

    /// Whether this node is attached to the document.
    ///
    /// Walked rather than flagged. `px-dom` keeps no is-attached bit, and
    /// deriving one would mean maintaining it across every mutation -- the kind
    /// of denormalised state that goes wrong silently. The walk is bounded by
    /// tree depth and every step is a link read.
    ///
    /// Iterative, not recursive. §9 Phase 4's nesting gate is about exactly this
    /// shape of function, and a recursive version here would reintroduce the
    /// stack overflow one crate over from where it was fixed.
    fn is_in_document(&self) -> bool {
        let document = self.dom().document_node().id();
        let mut current = *self;
        loop {
            if current.id() == document {
                return true;
            }
            let Some(parent) = style::dom::TNode::parent_node(&current) else {
                return false;
            };
            current = parent;
        }
    }

    /// The parent for traversal purposes.
    ///
    /// Identical to the element parent while there is no shadow DOM. Servo's
    /// version differs here because it crosses shadow boundaries; when Phase 14
    /// brings those, this is one of the places that has to change, and it is
    /// called out rather than left looking like a synonym for `parent_node`.
    fn traversal_parent(&self) -> Option<StyleElement<'a>> {
        style::dom::TNode::parent_node(self).and_then(StyleElement::new)
    }

    /// The opaque identity stylo uses to compare nodes it cannot dereference.
    ///
    /// ADR 018 built `to_opaque` for this exact consumer: it packs index and
    /// generation so that a reused slot is never confused with the node that
    /// used to live there, and biases the packing so the result is never zero,
    /// because stylo's `OpaqueElement` is a `NonNull`.
    fn opaque(&self) -> style::dom::OpaqueNode {
        style::dom::OpaqueNode(self.id().to_opaque().get())
    }

    /// An identifier for debug output only.
    ///
    /// The same packed value as [`Self::opaque`]. stylo prints it in traversal
    /// logs; it is not used to make decisions.
    fn debug_id(self) -> usize {
        self.id().to_opaque().get()
    }

    fn as_element(&self) -> Option<StyleElement<'a>> {
        StyleElement::new(*self)
    }

    /// This node as a document, if it is the document.
    fn as_document(&self) -> Option<StyleDocument<'a>> {
        let document = self.dom().document_node();
        (self.id() == document.id()).then(|| StyleDocument::from_document_node(document))
    }

    /// Always `None`: Phase 5 has no shadow DOM.
    ///
    /// Not a stub. `StyleShadowRoot` is uninhabited, so `None` is the only value
    /// this function *can* return -- the type system says what a comment would
    /// otherwise have to.
    fn as_shadow_root(&self) -> Option<StyleShadowRoot<'a>> {
        None
    }
}

impl<'a> StyleElement<'a> {
    /// This element's qualified name.
    ///
    /// Internally fallible, externally not. `StyleElement::new` already
    /// established that this node is an element, and the `&Arena` borrow forbids
    /// the mutation that could change that — so the `expect` is unreachable, and
    /// it is confined to this one function rather than repeated at every caller
    /// that needs a tag name.
    pub(crate) fn qual_name(self) -> &'a html5ever::QualName {
        self.node
            .arena()
            .get(self.node.id())
            .and_then(px_dom::Node::element_name)
            .expect(
                "StyleElement::new checked this node is an element, and the \
                 &Arena borrow forbids the mutation that could change it",
            )
    }
}

/// The name types html5ever hands us, as the types stylo's selector engine wants.
///
/// This is where the integration could have failed, and it does not.
///
/// `web_atoms` is unified at one version across html5ever and stylo — the Phase 4
/// report listed that as unverifiable until stylo was a dependency and warned
/// that two copies would surface as a type error exactly here. So the *static
/// sets* match (`LocalNameStaticSet`, `NamespaceStaticSet`). What differs is the
/// wrapper: html5ever stores a `string_cache::Atom<Set>`, and stylo's
/// `SelectorImpl` asks for `GenericAtomIdent<Set>`.
///
/// `GenericAtomIdent` is `#[repr(transparent)]` over exactly that `Atom`, and
/// stylo ships `GenericAtomIdent::cast` as a **safe** public function — the
/// transmute is inside stylo, justified by its own `repr`. So the conversion is a
/// no-op reference cast: no copy, no atom re-interning, and **no `unsafe` in
/// `px-css`**, which is what ADR 024 requires.
///
/// Had `cast` not existed, this would have been the phase's first real obstacle.
/// The only other route from `&Atom<S>` to `&GenericAtomIdent<S>` is a transmute,
/// which ADR 024 forbids here, and the fallback would have been to make `px-dom`
/// store stylo's types — inverting §3's layering for the sake of a wrapper.
pub fn local_name_of(
    name: &html5ever::QualName,
) -> &style::values::GenericAtomIdent<web_atoms::LocalNameStaticSet> {
    style::values::GenericAtomIdent::cast(&name.local)
}

/// The same cast for namespaces.
pub fn namespace_of(
    name: &html5ever::QualName,
) -> &style::values::GenericAtomIdent<web_atoms::NamespaceStaticSet> {
    style::values::GenericAtomIdent::cast(&name.ns)
}

/// The element-side helpers the two big trait impls share.
impl<'a> StyleElement<'a> {
    /// This element's attributes.
    pub(crate) fn attrs(self) -> &'a [html5ever::Attribute] {
        self.node
            .arena()
            .get(self.node.id())
            .and_then(px_dom::Node::attrs)
            .unwrap_or(&[])
    }

    /// The value of an attribute in no namespace, which is nearly all of them.
    pub(crate) fn attr_no_ns(self, name: &web_atoms::LocalName) -> Option<&'a str> {
        self.attrs()
            .iter()
            .find(|a| a.name.ns == html5ever::ns!() && a.name.local == *name)
            .map(|a| &*a.value)
    }

    /// The nearest preceding sibling that is an element.
    pub(crate) fn prev_element(self) -> Option<Self> {
        let mut cursor = self.node;
        loop {
            let prev = cursor.arena().get(cursor.id())?.prev_sibling()?;
            cursor = cursor.resolve(prev)?;
            if let Some(el) = Self::new(cursor) {
                return Some(el);
            }
        }
    }

    /// The nearest following sibling that is an element.
    pub(crate) fn next_element(self) -> Option<Self> {
        let mut cursor = self.node;
        loop {
            let next = cursor.arena().get(cursor.id())?.next_sibling()?;
            cursor = cursor.resolve(next)?;
            if let Some(el) = Self::new(cursor) {
                return Some(el);
            }
        }
    }

    /// The style data table this element's flags and computed style live in.
    pub(crate) fn data(self) -> &'a crate::data::StyleData {
        self.node.dom().root().data()
    }
}

impl<'a> selectors::Element for StyleElement<'a> {
    type Impl = style::selector_parser::SelectorImpl;

    /// The identity the selector engine compares and caches by.
    ///
    /// Built from ADR 018's packed handle rather than from the address of this
    /// view. A `StyleElement` is a temporary `Copy` value on the stack, so
    /// `OpaqueElement::new(&self)` — the obvious call, and what the doc comment
    /// invites — would hand out the address of a local and compare two views of
    /// the same element as different elements.
    ///
    /// `to_opaque()` packs index and generation and is biased so it is never
    /// zero, which ADR 018 did specifically because this is a `NonNull`.
    fn opaque(&self) -> selectors::OpaqueElement {
        let packed = self.node.id().to_opaque().get();
        let ptr = core::ptr::NonNull::new(core::ptr::without_provenance_mut::<()>(packed))
            .expect("NodeId::to_opaque is documented never to return zero");
        selectors::OpaqueElement::from_non_null_ptr(ptr)
    }

    fn parent_element(&self) -> Option<Self> {
        use style::dom::TNode as _;
        self.node.parent_node().and_then(Self::new)
    }

    /// Phase 5 has no shadow DOM, so no node's parent is a shadow root.
    fn parent_node_is_shadow_root(&self) -> bool {
        false
    }

    fn containing_shadow_host(&self) -> Option<Self> {
        None
    }

    /// This engine never constructs an element *for* a pseudo-element.
    ///
    /// `::before` and `::after` boxes are Phase 6's, generated during box-tree
    /// construction from the computed style this phase produces. Nothing here
    /// wraps one in an element, so the answer is false rather than unimplemented.
    fn is_pseudo_element(&self) -> bool {
        false
    }

    fn prev_sibling_element(&self) -> Option<Self> {
        self.prev_element()
    }

    fn next_sibling_element(&self) -> Option<Self> {
        self.next_element()
    }

    fn first_element_child(&self) -> Option<Self> {
        use style::dom::TNode as _;
        let mut child = self.node.first_child();
        while let Some(node) = child {
            if let Some(el) = Self::new(node) {
                return Some(el);
            }
            child = node.next_sibling();
        }
        None
    }

    /// Whether tag names match ASCII-case-insensitively for this element.
    ///
    /// True for HTML elements in an HTML document, which is every element this
    /// engine currently produces — `px-dom` drives html5ever and has no XML
    /// path. SVG and MathML elements inside an HTML document are *not* HTML
    /// elements, and the namespace check is what keeps `<clipPath>` from
    /// matching `clippath`.
    fn is_html_element_in_html_document(&self) -> bool {
        self.qual_name().ns == html5ever::ns!(html)
    }

    /// No cast here, and the asymmetry is worth knowing.
    ///
    /// stylo's `SelectorImpl` sets `BorrowedLocalName = web_atoms::LocalName`,
    /// which *is* html5ever's `Atom<LocalNameStaticSet>` — the borrowed name types
    /// are the raw atoms and compare directly. It is the **owned** `LocalName` and
    /// `NamespaceUrl`, used by `attr_matches` below, that are the
    /// `GenericAtomIdent` wrappers needing [`local_name_of`].
    ///
    /// Writing the cast on this side compiles as far as the `==`, then fails with
    /// "can't compare `GenericAtomIdent<LocalNameStaticSet>` with
    /// `Atom<LocalNameStaticSet>`", which is a clearer error than it deserves to
    /// be.
    fn has_local_name(
        &self,
        local_name: &<Self::Impl as selectors::parser::SelectorImpl>::BorrowedLocalName,
    ) -> bool {
        self.qual_name().local == *local_name
    }

    fn has_namespace(
        &self,
        ns: &<Self::Impl as selectors::parser::SelectorImpl>::BorrowedNamespaceUrl,
    ) -> bool {
        self.qual_name().ns == *ns
    }

    /// Whether two elements have the same tag, for `+`/`~` type matching.
    fn is_same_type(&self, other: &Self) -> bool {
        let (a, b) = (self.qual_name(), other.qual_name());
        a.local == b.local && a.ns == b.ns
    }

    fn is_link(&self) -> bool {
        // An <a>, <area> or <link> with an href. The href is what makes it a
        // link for :link purposes -- a bare <a> is not one.
        let name = self.qual_name();
        name.ns == html5ever::ns!(html)
            && matches!(
                name.local,
                ref l if *l == html5ever::local_name!("a")
                    || *l == html5ever::local_name!("area")
                    || *l == html5ever::local_name!("link")
            )
            && self.attr_no_ns(&html5ever::local_name!("href")).is_some()
    }

    fn is_html_slot_element(&self) -> bool {
        self.qual_name().ns == html5ever::ns!(html)
            && self.qual_name().local == html5ever::local_name!("slot")
    }

    fn has_custom_state(
        &self,
        _name: &<Self::Impl as selectors::parser::SelectorImpl>::Identifier,
    ) -> bool {
        // Custom states belong to custom elements, which arrive with the
        // element-definition machinery well after style.
        false
    }

    fn imported_part(
        &self,
        _name: &<Self::Impl as selectors::parser::SelectorImpl>::Identifier,
    ) -> Option<<Self::Impl as selectors::parser::SelectorImpl>::Identifier> {
        None
    }

    fn is_part(&self, _name: &<Self::Impl as selectors::parser::SelectorImpl>::Identifier) -> bool {
        false
    }

    /// `:empty` — no child elements and no non-empty text.
    ///
    /// Comments and processing instructions do not count, which is why this
    /// cannot be `first_child().is_none()`.
    fn is_empty(&self) -> bool {
        use style::dom::TNode as _;
        let mut child = self.node.first_child();
        while let Some(node) = child {
            if let Some(n) = node.arena().get(node.id()) {
                if n.element_name().is_some() {
                    return false;
                }
                if n.text().is_some_and(|t| !t.is_empty()) {
                    return false;
                }
            }
            child = node.next_sibling();
        }
        true
    }

    /// `:root` — the document element.
    fn is_root(&self) -> bool {
        use style::dom::TNode as _;
        let Some(parent) = self.node.parent_node() else {
            return false;
        };
        parent.id() == self.node.dom().document_node().id()
    }

    fn apply_selector_flags(&self, flags: selectors::matching::ElementSelectorFlags) {
        self.data().insert_selector_flags(self.node.id(), flags);
    }

    /// `[attr]`, `[attr=value]`, `[attr~=value]` and the rest.
    ///
    /// The namespace constraint is honoured rather than ignored: `[href]` means
    /// "href in no namespace" and must not match `xlink:href`, which is a real
    /// attribute on real SVG content.
    fn attr_matches(
        &self,
        ns: &selectors::attr::NamespaceConstraint<
            &<Self::Impl as selectors::parser::SelectorImpl>::NamespaceUrl,
        >,
        local_name: &<Self::Impl as selectors::parser::SelectorImpl>::LocalName,
        operation: &selectors::attr::AttrSelectorOperation<
            &<Self::Impl as selectors::parser::SelectorImpl>::AttrValue,
        >,
    ) -> bool {
        self.attrs().iter().any(|attr| {
            if local_name_of(&attr.name) != local_name {
                return false;
            }
            let ns_ok = match ns {
                selectors::attr::NamespaceConstraint::Any => true,
                selectors::attr::NamespaceConstraint::Specific(url) => {
                    namespace_of(&attr.name) == *url
                }
            };
            // `eval_str` is selectors' own comparison, so the operators and
            // their case-sensitivity rules come from the selector engine rather
            // than being re-derived here -- which is where a hand-written `~=`
            // splits on the wrong whitespace.
            ns_ok && operation.eval_str(&attr.value)
        })
    }

    /// Pseudo-classes that need more than the selector engine can see.
    ///
    /// **Phase 5 answers only the structural ones and returns `false` for the
    /// rest, and that is a real limitation rather than a stub.** `:hover`,
    /// `:focus`, `:active` and friends are driven by `ElementState`, which is
    /// input state this engine does not have yet — there is no event loop before
    /// Phase 13. Returning `false` means such a rule does not apply, which is
    /// the same thing the user sees before they interact with the page.
    ///
    /// It is listed in the phase report rather than left to be discovered: a
    /// stylesheet that styles `a:hover` will compute the un-hovered style, and
    /// that is correct until there is a pointer.
    fn match_non_ts_pseudo_class(
        &self,
        pc: &<Self::Impl as selectors::parser::SelectorImpl>::NonTSPseudoClass,
        _context: &mut selectors::context::MatchingContext<'_, Self::Impl>,
    ) -> bool {
        use style::selector_parser::NonTSPseudoClass as P;
        match pc {
            // :link is an unvisited link; :any-link is either. With no history
            // store there are no visited links, so the two coincide -- and that
            // is also the privacy-preserving answer, which §1's threat model
            // cares about more than the cascade does.
            P::Link | P::AnyLink => selectors::Element::is_link(self),
            P::Visited => false,
            // `:root`, `:empty`, `:first-child` and the rest are *structural*
            // and are not NonTSPseudoClass variants at all -- the selector
            // engine answers them itself through `is_root`, `is_empty` and the
            // sibling accessors above. Reaching for `P::Root` here is the
            // natural mistake and does not compile, which is the good outcome.
            _ => false,
        }
    }

    /// No element matches a pseudo-element selector.
    ///
    /// `::before` and `::after` produce boxes during Phase 6's box-tree
    /// construction, from the computed style this phase resolves. Nothing in
    /// Phase 5 wraps one in an element, so there is nothing for a pseudo-element
    /// selector to match against.
    fn match_pseudo_element(
        &self,
        _pe: &<Self::Impl as selectors::parser::SelectorImpl>::PseudoElement,
        _context: &mut selectors::context::MatchingContext<'_, Self::Impl>,
    ) -> bool {
        false
    }

    fn has_id(
        &self,
        id: &<Self::Impl as selectors::parser::SelectorImpl>::Identifier,
        case_sensitivity: selectors::attr::CaseSensitivity,
    ) -> bool {
        self.attr_no_ns(&html5ever::local_name!("id"))
            .is_some_and(|value| case_sensitivity.eq(value.as_bytes(), id.as_bytes()))
    }

    /// `.name`, over the space-separated `class` attribute.
    fn has_class(
        &self,
        name: &<Self::Impl as selectors::parser::SelectorImpl>::Identifier,
        case_sensitivity: selectors::attr::CaseSensitivity,
    ) -> bool {
        self.attr_no_ns(&html5ever::local_name!("class"))
            .is_some_and(|value| {
                value
                    .split_ascii_whitespace()
                    .any(|c| case_sensitivity.eq(c.as_bytes(), name.as_bytes()))
            })
    }

    /// Ancestor hashes for the selector engine's bloom filter.
    ///
    /// **Returns `false`: this element contributes no hashes, and the filter is
    /// skipped for it.** That is correct and slower, and it is deliberate.
    ///
    /// The filter is a *negative* cache — the engine consults it to reject
    /// `.a .b` without walking ancestors. For that to be sound, the hashes put in
    /// here must be computed exactly the way the engine hashes the corresponding
    /// selector components. They are not part of the public API, so matching them
    /// means reproducing an internal hash function and hoping it does not change.
    /// Getting it wrong does not fail loudly: it produces *false negatives*, and
    /// a false negative in a negative cache is a rule that silently stops
    /// applying. A page where one selector in a thousand does not match, with no
    /// error anywhere.
    ///
    /// That is precisely the failure mode `/CLAUDE.md` warns about — code that
    /// passes your tests and fails real sites — so the optimisation waits until
    /// the hashing can be shared rather than guessed. Filed in docs/backlog.md.
    fn add_element_unique_hashes(&self, _filter: &mut selectors::bloom::BloomFilter) -> bool {
        false
    }
}
