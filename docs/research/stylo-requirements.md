# What stylo requires of a DOM — and what Phase 4 must build to satisfy it

Research note for Pelorus. Written 2026-09-08. No code, no repository edits.

Target reader: whoever writes `px-dom` in Phase 4, and whoever writes the Phase 4
ADR. Every section below is here because it changes a Phase 4 decision. Sections
that would only have restated the spec were cut.

---

## 0. Provenance — what was verified, against what

Everything in sections 1–4 marked **[V]** was read from real source fetched today
(2026-09-08) over the network. Claims marked **[I]** are inference or design
proposal and are flagged as such inline. Nothing here is from training-data
recall alone; where I had a recollection I fetched the file and checked it.

| Source | Ref | Fetched |
|---|---|---|
| `servo/stylo` `main` | HEAD `e81a3d9ad10d6b2d97cb53a7930eb806ea5051a4`, committed 2026-09-08 | 2026-09-08 |
| `style/dom.rs` | last touched `50ae4a76250e160c8e1b4de06496bc88e1effcc1`, 2026-06-30 ("Bug 2051667 — Share ElementContext logic with Servo") | 2026-09-08 |
| `style/parallel.rs`, `style/traversal.rs`, `style/driver.rs`, `style/data.rs`, `style/context.rs`, `style/sharing/mod.rs`, `style/lib.rs`, `style/Cargo.toml`, `style/build.rs`, `style/invalidation/element/element_wrapper.rs`, `style/servo/selector_parser.rs` | same HEAD | 2026-09-08 |
| `selectors/tree.rs`, `style_traits/dom.rs` | same HEAD | 2026-09-08 |
| `servo/servo` `main` — `components/script/layout_dom/*`, `components/script/dom/node/layout_dom.rs`, `components/script/dom/element/element.rs`, `components/layout/traversal.rs` | `main` as of 2026-09-08 | 2026-09-08 |
| crates.io | `stylo` latest published **0.21.0** (published 2026-09); 24 releases since 2024-04-29 | 2026-09-08 |

Version churn is real and is a Phase 5 planning input: 24 published versions in
~28 months, with breaking trait changes in most. The `TElement` shape described
below is a snapshot, not a contract. `dom.rs` changed as recently as 2026-06-30
in a way that *added* a supertrait (`ElementContext`).

Local copies of everything fetched are in
`<scratchpad>/stylo-src/` if you want to re-read rather than re-fetch.

---

## 1. What stylo actually requires of a DOM

### 1.1 The trait stack

Five traits, all in `style/dom.rs` except `Element` (`selectors/tree.rs`) and
`ElementSnapshot` (`style/invalidation/element/element_wrapper.rs`). **[V]**

```
NodeInfo            is_element / is_text_node
  └─ TNode          navigation, identity, downcasts
       ├─ TDocument quirks mode, shared lock, id index
       ├─ TShadowRoot host, per-tree CascadeData
       └─ TElement   everything else (≈70 methods)
            └─ selectors::Element (≈30 methods, supertrait of TElement)
            └─ ElementContext (4 methods, supertrait since 2026-06-30)
```

Full signatures are long; the shape that matters is:

```rust
// style/dom.rs — VERBATIM, servo/stylo @ e81a3d9
pub trait TNode: Sized + Copy + Clone + Debug + NodeInfo + PartialEq {
    type ConcreteElement: TElement<ConcreteNode = Self>;
    type ConcreteDocument: TDocument<ConcreteNode = Self>;
    type ConcreteShadowRoot: TShadowRoot<ConcreteNode = Self>;

    fn parent_node(&self) -> Option<Self>;
    fn first_child(&self) -> Option<Self>;
    fn last_child(&self) -> Option<Self>;
    fn prev_sibling(&self) -> Option<Self>;
    fn next_sibling(&self) -> Option<Self>;
    fn owner_doc(&self) -> Self::ConcreteDocument;      // <-- infallible
    fn is_in_document(&self) -> bool;
    fn depth(&self) -> usize;                            // <-- infallible
    fn traversal_parent(&self) -> Option<Self::ConcreteElement>;
    fn opaque(&self) -> OpaqueNode;                      // <-- infallible
    fn debug_id(self) -> usize;
    fn as_element(&self) -> Option<Self::ConcreteElement>;
    fn as_document(&self) -> Option<Self::ConcreteDocument>;
    fn as_shadow_root(&self) -> Option<Self::ConcreteShadowRoot>;
}

pub trait TElement:
    Eq + PartialEq + Debug + Hash + Sized + Copy + Clone
    + SelectorsElement<Impl = SelectorImpl>
    + ElementContext
{ /* ~70 methods */ }
```

Three structural facts follow immediately, and all three constrain Phase 4:

**(a) The handle is `Copy`, and there is no `&Arena` parameter anywhere.** No
method on any of these traits takes a context, an arena, a tree, or a session.
Whatever `TNode` resolves to must contain everything needed to answer
`first_child()`, `local_name()`, and `borrow_data()` on its own. **[V]**

**(b) `TElement: SelectorsElement<Impl = SelectorImpl>` fixes the atom types.**
`SelectorImpl` here is *stylo's* `style::selector_parser::SelectorImpl`, not a
parameter you choose. With the `servo` feature, `style/lib.rs` sets: **[V]**

```rust
pub type LocalName = crate::values::GenericAtomIdent<web_atoms::LocalNameStaticSet>;
pub type Namespace = crate::values::GenericAtomIdent<web_atoms::NamespaceStaticSet>;
pub type Prefix    = crate::values::GenericAtomIdent<web_atoms::PrefixStaticSet>;
use stylo_atoms::Atom as WeakAtom;
```

So `px-dom` must store element local names as `web_atoms` static-set atoms and
ids/classes as `stylo_atoms::Atom`. This is not negotiable and not adaptable:
`fn local_name(&self) -> &BorrowedLocalName` returns a *borrow* of the stored
value, so you cannot convert on the fly. **A home-grown string interner in
Phase 4 is dead work that Phase 5 will delete.** `web_atoms` is what
`html5ever` 0.39 uses, so the parser and stylo agree — but confirm the exact
`web_atoms` minor version matches between the `html5ever` you pin and the
`stylo` you pin, because `string_cache` static sets are not interchangeable
across versions.

**(c) Several methods return borrows whose lifetime is derived from the handle,
not from a borrow of the tree.** **[V]** The tell is the `where Self: 'a` bound:

```rust
// TShadowRoot
fn style_data<'a>(&self) -> Option<&'a CascadeData> where Self: 'a;
fn elements_with_id<'a>(&self, _id: &AtomIdent)
    -> Result<&'a [Element], ()> where Self: 'a;
// TElement
fn each_applicable_non_document_style_rule_data<'a, F>(&self, f: F) -> bool
    where Self: 'a, F: FnMut(&'a CascadeData, Self);
fn style_attribute(&self) -> Option<ArcBorrow<'_, Locked<PropertyDeclarationBlock>>>;
fn id(&self) -> Option<&WeakAtom>;
fn local_name(&self) -> &<SelectorImpl as SelectorImpl>::BorrowedLocalName;
fn slotted_nodes(&self) -> &[Self::ConcreteNode];
fn borrow_data(&self) -> Option<ElementDataRef<'_>>;
```

`where Self: 'a` only makes sense if `Self` carries a lifetime parameter. This
is stylo telling you, in the type system, that **the handle is expected to be a
lifetime-parameterised borrow, not a bare index.** Servo obliges: **[V]**

```rust
// servo/components/script/layout_dom/servo_dangerous_style_node.rs
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(transparent)]
pub struct ServoDangerousStyleNode<'dom> { pub(crate) node: LayoutDom<'dom, Node> }
unsafe impl Send for ServoDangerousStyleNode<'_> {}
unsafe impl Sync for ServoDangerousStyleNode<'_> {}
```

This is the single most important finding in this note for section 3. **The
shape stylo wants is already lifetime-scoped**, which is exactly what a
snapshot-borrow design gives you for free.

### 1.2 What stylo writes back into your DOM

`TElement` is not read-only. It requires per-element mutable state reachable
through a shared `&`: **[V]**

| Method | What your DOM must hold |
|---|---|
| `ensure_data() -> ElementDataMut<'_>` (**unsafe fn**) | a `style::data::ElementDataWrapper` per element |
| `clear_data()` (**unsafe fn**), `has_data()`, `borrow_data()`, `mutate_data()` | same |
| `set_dirty_descendants()` / `unset_dirty_descendants()` (**unsafe fn**), `has_dirty_descendants()` | one bit per element |
| `set_handled_snapshot()` (**unsafe fn**), `has_snapshot()`, `handled_snapshot()` | two bits per element |
| `store_children_to_process(isize)` / `did_process_child() -> isize` | an `AtomicIsize` per element |
| `apply_selector_flags(ElementSelectorFlags)` (from `selectors::Element`) | atomic flag word per element, **written on self *and on the parent*** |
| `has_selector_flags`, `relative_selector_search_direction` | reads of that flag word |

`ElementData` itself is stylo's type and small (`size_of_test!(ElementData, 24)`
in `style/data.rs`): `styles`, `damage`, `hint`, `flags`. It holds
`Arc<ComputedValues>`, no node handles. That matters — see §3.4.

### 1.3 The entry point Phase 5 will write

Concretely, from `style/driver.rs` and Servo's `components/layout/traversal.rs`: **[V]**

```rust
pub fn traverse_dom<E, D>(traversal: &D, token: PreTraverseToken<E>,
                          pool: Option<&rayon::ThreadPool>) -> E
where E: TElement, D: DomTraversal<E>;

pub trait DomTraversal<E: TElement>: Sync {
    fn process_preorder<F>(&self, &PerLevelTraversalData, &mut StyleContext<E>,
                           E::ConcreteNode, note_child: F) where F: FnMut(E::ConcreteNode);
    fn process_postorder(&self, &mut StyleContext<E>, E::ConcreteNode);
    fn needs_postorder_traversal() -> bool { true }
    fn shared_context(&self) -> &SharedStyleContext<'_>;
    // pre_traverse(), element_needs_traversal() have defaults
}
```

Servo's whole `DomTraversal` impl is ~45 lines: initialise style data, call
`style::traversal::recalc_style_at`, and `needs_postorder_traversal() = false`.
**The traversal driver is not the hard part. The `TElement` impl is.**

`SharedStyleContext<'a>` holds `stylist: &'a Stylist`, `guards:
StylesheetGuards<'a>`, `snapshot_map: &'a SnapshotMap`, `traversal_flags`, and
(servo feature) `animations: DocumentAnimationSet` and
`registered_speculative_painters: &'a dyn RegisteredSpeculativePainters`. **[V]**
The last one means Phase 5 must provide a stub painter registry even with no
Houdini support.

### 1.4 Two build-system facts the spec does not account for

**`stylo`'s `build.rs` shells out to Python 3 at build time.** **[V]**

```rust
// style/build.rs @ e81a3d9
pub static PYTHON: LazyLock<String> = LazyLock::new(|| {
    env::var("PYTHON3").ok().unwrap_or_else(|| {
        let candidates = if cfg!(windows) { ["python.exe"] } else { ["python3"] };
        ... panic!("Can't find python (tried {})! ...")
    })});
fn generate_properties(engine: &str) { /* runs properties/build.py via Mako */ }
```

This collides directly with build-spec §Phase 0's shipped gate: *"hashes
reproducible across two machines with different paths and usernames"*. From
Phase 5 onward the release binary's contents depend on a Python interpreter that
is not in `Cargo.lock`, not in `rust-toolchain.toml`, and not
version-pinned. `ci/build-and-hash.sh` and the reproducibility claim need a
decision — pin `PYTHON3` to an exact interpreter in CI, or accept that
reproducibility is now "reproducible given the same Python 3.x". **This belongs
in the Phase 4 ADR, not discovered in Phase 5**, because it is the kind of thing
that gets waived under deadline pressure.

**stylo's direct dependency list is ~45 crates** including `rayon`,
`rayon-core`, `parking_lot`, `icu_segmenter`, `cssparser`, `servo_arc`,
`atomic_refcell`, `to_shmem`, `string_cache`, `thin-vec`, `uluru`, `euclid`,
`app_units`, `encoding_rs`, `num_cpus`, `serde`, `url`. **[V]** §3 approves
`stylo`; it does not obviously approve `rayon` inside a content process that
builds `panic = "abort"`, or `num_cpus` reading `/proc` and `sched_getaffinity`
inside a seccomp sandbox. Both should be named explicitly in the ADR. The
unsafe-count baseline (`ci/unsafe-baseline.json`) will jump by a large,
one-time amount; commit the new baseline deliberately with the ADR rather than
letting the gate get bumped silently.

---

## 2. The thread-safety invariants stylo assumes and does not check

The spec's sentence is accurate. Here is exactly where the holes are.

### 2.1 The one compile-time guarantee, and how stylo defeats it

`TNode` and `TElement` are deliberately **not** `Send`. The module doc in
`style/parallel.rs` says so: **[V]**

> "As such, TNode and TElement are not Send, so ordinary style system code cannot
> accidentally share them with other threads. In the parallel traversal, we
> explicitly invoke `unsafe { SendNode::new(n) }` to put nodes in containers
> that may be sent to other threads."

And then, in `style/dom.rs`: **[V]**

```rust
pub struct SendNode<N: TNode>(N);
unsafe impl<N: TNode> Send for SendNode<N> {}
pub struct SendElement<E: TElement>(E);
unsafe impl<E: TElement> Send for SendElement<E> {}
```

The `unsafe impl` is **unconditional**. It does not require `N: Send`. It does
not require `N: Sync`. **Stylo asserts, on your behalf, that your handle type is
safe to send to another thread and to use concurrently with other handles into
the same tree.** If your handle contains an `Rc`, a `Cell`, a `RefCell`, a
`*const` into a non-`Sync` structure, or a `&` to anything non-`Sync`, stylo
will happily ship it to a rayon worker and the compiler will say nothing.

That is the entire compile-time story. Everything else below is convention.

### 2.2 `ElementData` has no runtime check in release builds

This is the one that will bite hardest, and the module doc that describes it is
stale. `style/parallel.rs` still says: **[V]**

> "Accessing a DOM element concurrently on multiple threads is actually mostly
> 'safe', since all the mutable state is protected by an AtomicRefCell, and so
> we'll generally panic if something goes wrong."

But `style/data.rs` at the same commit says: **[V]**

```rust
pub struct ElementDataWrapper {
    inner: std::cell::UnsafeCell<ElementData>,
    /// Implements optional (debug_assertions-only) thread-safety checking.
    #[cfg(debug_assertions)]
    refcell: AtomicRefCell<()>,
}
impl ElementDataWrapper {
    pub fn borrow(&self) -> ElementDataRef<'_> {
        #[cfg(debug_assertions)] let borrow = self.refcell.borrow();
        ElementDataRef { v: unsafe { &*self.inner.get() }, ... }
    }
    pub fn borrow_mut(&self) -> ElementDataMut<'_> {
        #[cfg(debug_assertions)] let borrow = self.refcell.borrow_mut();
        ElementDataMut { v: unsafe { &mut *self.inner.get() }, ... }
    }
}
```

**In a release build the borrow check is compiled out.** `borrow()` and
`borrow_mut()` are safe functions handing out `&` and `&mut` from an
`UnsafeCell` with no check whatsoever. A traversal bug that would panic loudly
in `cargo test` is undefined behaviour in the shipping binary, silently.

Direct consequences for Phase 4/5:
- **All stylo bring-up, all fixtures, all WPT runs must have `debug_assertions`
  on.** Add a CI job that runs the style gate with `debug-assertions = true` in
  a release-like profile — otherwise the only builds that can catch aliasing
  bugs are the slow ones nobody runs on the full corpus.
- The `[profile.content-release]` in `Cargo.toml` inherits `release`. Consider a
  `content-release-checked` profile with `debug-assertions = true` used for the
  40-site compat suite. Cheap now, impossible to retrofit interest in later.
- `#![forbid(unsafe_code)]` in `px-dom` will remain literally true while `px-dom`
  embeds a type whose safe API dereferences an `UnsafeCell`. The crate attribute
  will stop meaning what `/CLAUDE.md` implies it means. Say so in
  `crates/px-dom/CLAUDE.md` when you get there.

### 2.3 The unwritten contract: subtree ownership

Reading `style/parallel.rs::style_trees` and `style/traversal.rs::note_children`
together, the actual invariant is: **[V, reading the algorithm]**

> At any instant during a parallel traversal, each element is *owned* by exactly
> one worker, which may read and write its `ElementData` and its dirty bits with
> no synchronisation. Ownership is established by the element being popped from
> a work queue, and released when `process_preorder` returns.

Work is distributed by pushing `SendNode`s into a `VecDeque` and handing chunks
to `rayon::ScopeFifo::spawn_fifo`. Since each node is pushed exactly once (by
its parent's `note_child`), the partition is disjoint. **Nothing checks this.**
It is a property of the algorithm, and it holds only if your
`traversal_children()` iterator yields each child exactly once and your tree is
acyclic. A `px-dom` bug that produces a diamond in the child links — which a
generational arena makes easy to write by accident, since a node can be
"appended" twice — turns into two workers holding `&mut ElementData` for the
same element, in release, with no panic. **Phase 4 must make double-parenting
structurally impossible or assert against it, not merely avoid it.**

### 2.4 The three places that legitimately cross subtree boundaries

These are the exceptions, and they are exactly the fields that must be atomic:

1. **`did_process_child()` / `store_children_to_process()`.** The postorder
   bubble-up. `style/traversal.rs` says: *"The only communication between
   siblings is that they both fetch-and-subtract the parent's children count.
   This makes it safe to call durign the parallel traversal."* **[V]** Servo
   implements this with `AtomicIsize` and `Ordering::Relaxed`. **[V]**

2. **`apply_selector_flags()` writes to the parent.** `selectors/tree.rs`:
   *"Sets selector flags on the elemnt itself or the parent."* **[V]** Servo's
   implementation walks `self.as_node().parent_element()` and calls
   `insert_selector_flags` on it. **[V]** Two siblings on two threads race on
   their shared parent's flag word. Servo makes it an `AtomicUsize` with
   `fetch_or(.., Relaxed)`. **[V]** This one is easy to miss because the method
   name does not suggest cross-node mutation.

3. **`set_dirty_descendants()` is called on the parent from `note_children`.**
   Same shape. Servo does *not* handle this one correctly, and admits it in a
   live FIXME in `components/script/dom/node/layout_dom.rs`: **[V]**

   ```rust
   // FIXME(nox): get_flag/set_flag (especially the latter) are not safe because
   // they mutate stuff while values of this type can be used from multiple
   // threads at once, this should be revisited.
   pub(crate) unsafe fn set_flag(self, flag: NodeFlags, value: bool) {
       let this = self.unsafe_get();
       let mut flags = (this).flags().get();   // Cell<NodeFlags>, non-atomic
       if value { flags.insert(flag); } else { flags.remove(flag); }
       (this).flags().set(flags);
   }
   ```

   That is a non-atomic read-modify-write on a `Cell` from parallel style
   threads, in the reference implementation, today. **Do not use Servo's node
   flags as a model.** Use an atomic word. This is a concrete case of "the
   guarantees are conventions" — the convention here is not even upheld by the
   only other consumer of the API.

### 2.5 `SharedStyleContext` must be `Sync`, and nothing checks that either

`DomTraversal<E>: Sync` is the only bound. `SharedStyleContext` is reached as
`traversal.shared_context()` *inside* the worker, so the compiler never asks
whether `&SharedStyleContext` is `Send`. **[V, by construction from
`parallel.rs::distribute_one_chunk`]** Everything reachable from your
`DomTraversal` impl — your `Device`, your font-metrics provider, your stub
painter registry, any `px-text` handle you hang off it — must be genuinely
`Sync`, and the compiler will not tell you if it is not. **[I on the
consequence; V on the mechanism.]**

### 2.6 Node lifetime during a traversal

The requirement is absolute and simple: **no structural mutation of the DOM
between `pre_traverse` and the return of `traverse_dom`**, including no node
destruction and no reparenting. `traversal_children()` is called during
`note_children` on the traversing thread while other threads are elsewhere in
the tree; `apply_selector_flags` dereferences a parent that some other thread is
finishing with; `clear_descendant_data` (an `unsafe fn`) walks a whole subtree
freeing `ElementData`. **[V]**

Servo enforces this by construction: layout runs on the script thread's data
while script is blocked, and the `'dom` lifetime prevents escape. Its
`layout_dom/mod.rs` states the rules as a whitelist discipline: *"1. Layout is
not allowed to mutate the DOM. 2. Layout is not allowed to see anything with
`LayoutDom` in the name, because it could hang onto these objects and cause
use-after-free."* **[V]**

Pelorus can do better than a whitelist discipline, and §3 explains how.

### 2.7 Summary: checked vs. unchecked

| Invariant | Enforced by |
|---|---|
| `DomTraversal: Sync` | compiler |
| handle is `Copy + Eq + Hash + Debug` | compiler |
| handle is safe to `Send` | **`unsafe impl` in stylo — nothing** |
| `SharedStyleContext` reachable state is `Sync` | **nothing** |
| one writer per element's `ElementData` | **nothing in release; `AtomicRefCell` in debug only** |
| child links form a tree, each child yielded once | **nothing** |
| parent-mutating fields are atomic | **nothing — Servo gets one of three wrong** |
| no structural mutation during traversal | **nothing** (Servo: convention + `'dom`) |
| `ElementData` on an element ⇒ data on all ancestors | **nothing** (stated as a comment in `clear_descendant_data`) |

---

## 3. The collision with generational handles

### 3.1 State the collision precisely

`/CLAUDE.md` and §4.1: *"DOM handles are generational and every accessor returns
Option. Never add an infallible index API."*

Stylo requires many infallible accessors. Non-exhaustive, all **[V]**:
`TNode::owner_doc`, `depth`, `opaque`, `debug_id`; `TElement::as_node`,
`state`, `local_name`, `namespace`, `id`, `each_class`, `each_attr_name`,
`traversal_children`, `has_dirty_descendants`, `has_data`, `ensure_data`,
`store_children_to_process`, `did_process_child`,
`synthesize_presentational_hints_for_legacy_attributes`,
`query_container_size`, `hash_for_bloom_filter`; `TShadowRoot::host`;
`selectors::Element::has_local_name`, `attr_matches`, `is_empty`, `is_root`,
`apply_selector_flags`, `add_element_unique_hashes`, and ~20 more.

**Therefore `NodeId { index, generation }` cannot itself implement `TNode` or
`TElement`.** There is no version of this where it can. Any attempt requires
either an infallible `get_unchecked`-shaped API (banned by §4.1) or an
`expect()` on every call (banned in spirit by §4.3, and a per-property panic
risk in the hottest loop in the engine).

**That is the whole collision. And it is not a real problem**, because §4.1 is a
rule about `px-dom`'s public arena API, not a rule that says one type must serve
every consumer.

### 3.2 The resolution: resolve once at the boundary, borrow thereafter

The `where Self: 'a` bounds in §1.1(c) are stylo telling you it wants a
lifetime-carrying handle. Give it one. **[I — this is a design proposal, built
on verified trait shapes and on Servo's verified precedent.]**

```rust
// ILLUSTRATIVE ONLY — not code for this project.
//
// px-dom offers exactly one way in, and it is fallible:
impl DomTree {
    pub fn style_view(&self) -> StyleView<'_>;                 // &self => no mutation possible
}
impl<'dom> StyleView<'dom> {
    pub fn node(&self, id: NodeId) -> Option<StyleNode<'dom>>; // the generation check, once
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct StyleNode<'dom> {
    slot: &'dom NodeSlot,   // already resolved; no further lookup, no further Option
    tree: &'dom DomTree,    // for resolving sibling/child NodeIds
}
```

Why this composes rather than compromises:

- **The generational check still happens, exactly once, at the only place a
  stale id can enter.** `style_view().node(id)` returns `Option`. §4.1's actual
  safety property — a stale `NodeId` never silently addresses a live node — is
  preserved in full.
- **After that, staleness is impossible, not merely checked.** `StyleView<'dom>`
  borrows the tree immutably. While any `StyleNode<'dom>` exists, no slot can be
  freed, reused, or have its generation bumped, because no `&mut DomTree`
  exists. The borrow checker enforces §2.6 — the invariant that stylo assumes
  and does not check, and that Servo enforces with a comment.
- **`first_child()` still returns `Option`.** Internal links are
  `Option<NodeId>`; resolving one through `self.tree` yields `Option<StyleNode>`.
  Nothing is unwrapped. **No infallible index API is added, at any visibility.**
- **The infallible methods become infallible honestly.** `local_name()` is
  `&self.slot.local_name` — a field access on an already-resolved reference. No
  lookup, no `Option`, no panic path.
- **`Send`/`Sync` become true rather than asserted.** If `NodeSlot` and
  `DomTree` are `Sync` (see §4), then `StyleNode<'dom>` is `Send + Sync`
  automatically. Stylo's `unsafe impl Send for SendNode<N>` becomes redundant
  rather than a lie. Servo needs its own `unsafe impl Send`/`Sync` because
  `LayoutDom` wraps a raw pointer into GC'd, `Cell`-laden objects. **Pelorus
  does not have to inherit that.** This is the clearest place where a
  from-scratch DOM can be *better* than the reference implementation rather
  than merely as good.

### 3.3 The constraint this puts on the arena, which is the actual Phase 4 decision

`StyleNode<'dom>` holding `&'dom NodeSlot` requires **stable slot addresses for
the lifetime of the view**. A `Vec<NodeSlot>` arena reallocates on growth and
invalidates every outstanding reference. Within a traversal that cannot happen
(the view holds `&DomTree`), so this is sound as written — but it silently
forbids ever handing out a `StyleNode` that outlives a mutation window, and it
means the borrow of the whole tree is the unit of safety.

Two arena layouts, both viable, decide differently: **[I]**

- **(a) Chunked arena** — `Vec<Box<[Slot; N]>>` or equivalent. Slot addresses
  are stable for the life of the slot even as the arena grows. `StyleNode` can
  hold `&'dom NodeSlot` directly. Everything above works with no lookup cost in
  the hot path.
- **(b) Flat `Vec` arena + `StyleNode { id: NodeId, tree: &'dom DomTree }`** —
  every accessor does an index+generation check. `local_name()` then needs
  `.expect()` on a lookup that cannot fail. That reintroduces a panic path into
  selector matching. Rejected.

**Recommendation: chunked, stable-address arena, decided in Phase 4.** Retro
fitting stable addresses onto a flat `Vec` design touches every accessor, every
iterator, and the Miri tests. This single choice is most of the "px-dom
redesign" the risk register is worried about, and it is fully decidable today.

### 3.4 Does stylo ever hold a node reference across a mutation?

Verified answer: **no, provided you drop the view.** **[V — traced through every
container that stores `E` or `E::ConcreteNode`]**

Within one `traverse_dom` call, handles live in:
`VecDeque<SendNode<..>>` (work queues), `StyleBloom<E>` (ancestor stack),
`StyleSharingCache<E>` (LRU of style-sharing candidates, `sharing/mod.rs`), and
`SequentialTaskList<E>` (`SendElement<E>`, drained on drop). All four live in
`ThreadLocalStyleContext<E>`, which lives in a `ScopedTLS` created inside
`traverse_dom` and dropped before it returns. **[V]**

Across calls, stylo retains:
- `ElementData` — stored in *your* node, holding `Arc<ComputedValues>`,
  `RestyleHint`, `RestyleDamage`. **No node handles.** **[V]**
- `Stylist` / `CascadeData` — selector maps and declarations. No node handles.
- `SnapshotMap` — and this one matters. For the servo backend it is
  `FxHashMap<OpaqueNode, ServoElementSnapshot>` (`style/servo/selector_parser.rs`). **[V]**

So the design in §3.2 is sound: nothing stylo owns outlives the view, and the
one thing that spans mutations is keyed by `OpaqueNode`, not by a reference.

### 3.5 `OpaqueNode` and `OpaqueElement` — a free win, and a trap

```rust
// style_traits/dom.rs
pub struct OpaqueNode(pub usize);
// selectors/tree.rs
pub struct OpaqueElement(NonNull<()>);
```
**[V]**

`OpaqueNode` is used as an identity key that spans mutations (the `SnapshotMap`)
and as the traversal-root comparison. In Servo it is a pointer. **Pointer-derived
identity is stale-unsafe across free and reuse**: a removed element's snapshot
can be matched to a *different* element later allocated at the same address.

Pack the generational `NodeId` into it instead — `(index as usize) << 32 |
generation as usize` — and that class of bug becomes structurally impossible: a
reused slot has a different generation, so it gets a different `OpaqueNode`, so
the stale snapshot simply misses. **This is §4.1's guarantee extending into
stylo's own data structures for free, and it is a Phase 4 decision because it
constrains how `NodeId` is laid out.**

**The trap:** `OpaqueElement` is `NonNull<()>`, and `from_non_null_ptr` /
`NonNull::new_unchecked` mean a zero value is UB. **[V]** A node at
`index = 0, generation = 0` packs to zero. Bias the packing (e.g.
`generation` starts at 1, or set a high tag bit). Getting this wrong produces a
null `NonNull` — instant UB, no diagnostic. One line in Phase 4, an afternoon of
debugging in Phase 5.

### 3.6 The `forbid(unsafe_code)` problem — verified, not inferred

`px-css/src/lib.rs` and `px-dom/src/lib.rs` today both start with
`#![forbid(unsafe_code)]`. `/CLAUDE.md`: *"Every crate begins with
`#![forbid(unsafe_code)]`. The sole exception is `px-sandbox`."* The Phase 0 gate
asserts it.

`TElement` declares six `unsafe fn` methods that an implementor **must** write as
`unsafe fn`. I compiled a probe rather than infer this — `rustc 1.98.1`,
edition 2024: **[V]**

```
error: implementation of an `unsafe` method
 --> probe.rs:4:16
4 | impl T for S { unsafe fn f(&self) { ... } }
note: the lint level is defined here
1 | #![forbid(unsafe_code)]
```

**Phase 5 cannot be written under the current hard rule.** This is not a
surprise to defer; it is a working-agreement amendment due in the Phase 4 ADR.

The good news is that the waiver can be made much narrower than "px-css may use
unsafe", and the probe shows exactly how: the *body* of an `unsafe fn` needs no
`unsafe` block. With the §3.2 design there is no `unsafe impl Send` either,
because `Send`/`Sync` are derived legitimately. So the amendment can be:

> `px-css` may declare `unsafe fn` where a `stylo` trait signature requires it.
> It may contain **zero** `unsafe` blocks and **zero** `unsafe impl`.

That is greppable, and `ci/gate-unsafe-headers.sh` can enforce it as a stricter
rule than the one it replaces. Losing `forbid` entirely and gaining nothing back
would be the bad outcome; this keeps a real, checkable property.

---

## 4. What Phase 4 should build differently

Ordered by cost-of-retrofit, most expensive first.

1. **Chunked, stable-address arena.** §3.3. Slot addresses must not move when
   the arena grows. Everything else in this list assumes it.

2. **A borrowed style view type. Yes, `px-dom` needs a parallel-safe snapshot
   type.** `DomTree::style_view(&self) -> StyleView<'_>`, yielding
   `StyleNode<'dom>` / `StyleElement<'dom>` — `Copy`, two words, `Send + Sync`,
   constructed only through a fallible generation-checked lookup, statically
   incapable of outliving the immutable borrow of the tree. This is the type
   `px-css` implements `TNode`/`TElement` on. It lives in `px-dom` (it needs
   private field access); the trait impls live in `px-css` (they need the
   `unsafe fn` waiver, and `px-dom` should keep `forbid`).
   *Build it in Phase 4 even though nothing consumes it until Phase 5.* Its
   whole value is that it forces the arena and slot layout decisions early,
   which is precisely what this note exists to do.

3. **Per-element style state, allocated eagerly, atomic where §2.4 requires.**
   Per element slot:
   - `style::data::ElementDataWrapper` (stylo's type — a direct dependency of
     `px-dom`, which is worth noting in the ADR),
   - `AtomicIsize children_to_process`,
   - `AtomicU32 selector_flags`,
   - `AtomicU32 node_flags` (dirty-descendants, has-snapshot, handled-snapshot,
     is-in-document, plus a `has_style_data` bit).

   **Allocate it with the element rather than lazily.** Servo's `ensure_data` /
   `clear_data` are `unsafe fn` precisely because they allocate and free through
   a shared reference during the traversal — that is Servo's single most
   dangerous unsafe in this area. If the storage always exists, `ensure_data`
   becomes "set the `has_style_data` bit, return `borrow_mut()`" and
   `clear_data` becomes "clear the bit, reset `ElementData` to `Default`"; no
   allocation, no interior mutability of an `Option<Box<_>>`, no race to
   allocate. Cost is ~50–60 bytes per element unconditionally. **[I — needs a
   Phase 5 spike to confirm stylo never distinguishes "never had data" from
   "data reset to default" beyond `has_data()`; `traversal.rs::clear_descendant_data`
   relies on `has_data()` to prune its walk, and the bit preserves that.]**

4. **Atoms: `web_atoms` and `stylo_atoms`, from the start.** §1.1(b). Store
   local names, namespaces, prefixes, ids and classes in the exact interner
   types stylo's `SelectorImpl` names. Do not write an interner. Verify the
   `web_atoms` version agreement between your pinned `html5ever` and your pinned
   `stylo` **before** writing the tree builder.

5. **`NodeId` packing that satisfies `OpaqueNode`/`OpaqueElement`.** §3.5.
   Non-zero by construction; a total, allocation-free
   `NodeId -> usize -> NonNull<()>` mapping; generations that never wrap
   (already required by §14.3) so the mapping stays injective for the process
   lifetime.

6. **Mutation-side snapshot recording.** Stylo's invalidation needs
   `ServoElementSnapshot`-shaped records — prior `ElementState`, prior attribute
   values, and `class_changed` / `id_changed` / `other_attributes_changed`
   flags — captured *at mutation time*, keyed by `OpaqueNode`, plus the
   `has_snapshot` / `handled_snapshot` bits on the element. **[V — `style/servo/selector_parser.rs`, `style/invalidation/element/element_wrapper.rs`]**
   The Phase 4 gate as written (html5lib-tests, stale-handle fuzz, mutation
   fuzz, deep nesting) does not mention this at all, and it is a *mutation-path*
   feature — it has to be threaded through every attribute setter,
   `classList` operation and state change in `px-dom`. Retrofitting it in
   Phase 5 means touching every mutation site twice. **Add it to the Phase 4
   scope and gate.**

7. **Structural invariants asserted, not assumed.** §2.3. Each node has at most
   one parent; a node appears exactly once in its parent's child list; the child
   links form a tree. Add a `debug_assert`-level tree validator and run it in
   the existing 24h mutation fuzz target. Stylo's parallel correctness rests
   entirely on this and checks none of it.

8. **A `debug_assertions`-enabled release-like profile for the style gate.**
   §2.2. Without it, the only builds that can detect an `ElementData` aliasing
   bug are the ones too slow to run the corpus on.

9. **Decide the sequential-first plan now.** `traverse_dom(traversal, token,
   pool: Option<&rayon::ThreadPool>)` — passing `None` runs the whole traversal
   on the calling thread, and `with_pool_in_place_scope` short-circuits to
   `closure(None)` when `work_unit_max == 0` or the pool is absent. **[V]** So
   Phase 5 can land stylo with *zero* parallelism, and §2.1–2.5 stop being
   correctness risks and become future work. The atomics in item 3 are still
   worth having (they cost nothing and `apply_selector_flags` still writes to
   parents), but the ownership discipline of §2.3 stops being load-bearing.
   **This is the single biggest de-risking lever available and it costs
   nothing.** Turning parallelism on later is a one-line change plus a real
   TSAN/loom campaign. Recommend the Phase 4 ADR state "Phase 5 ships
   sequential; parallel restyle is a separate, gated phase."

10. **Do not build these in Phase 4, but leave room:** a `#[repr]`-stable
    `NodeSlot` with the style state adjacent (cache locality in the hottest
    loop); shadow-root slots carrying a `style::stylist::CascadeData`; a
    document-level `elements_with_id` index (stylo asks for it and accepts
    `Err(())`, so it can be a stub — but the id map is cheap to maintain in the
    mutation path and expensive to add later).

---

## 5. The honest alternative — writing the cascade

The Phase 5 gate requires *"a documented decision on whether stylo survived
contact or is being replaced."* Both sides, no advocacy.

### 5.1 What replacement actually costs

Measured today from the `servo/stylo` tree at HEAD: **[V]**

| Crate | Files | Source bytes |
|---|---|---|
| `style/` | 314 | **4.98 MiB** (of which `style/values/` 1.62 MiB, `style/properties/` 0.97 MiB) |
| `selectors/` | 19 | 314 KiB |
| `style_derive`, `style_traits`, `servo_arc`, `to_shmem`, others | ~40 | ~280 KiB |

At typical Rust source density that is order **150,000 lines**, and it is not
padding: `style/values/` is the computed-value type for every CSS property, and
`style/properties/` is Mako-generated shorthand/longhand machinery for the full
property set.

A from-scratch replacement has to deliver, minimally: CSS tokenising and parsing
(you would still take `cssparser`); selector parsing, specificity, and matching
including `:has()` and `:nth-*`; cascade origins, `!important`, `@layer`,
`@scope`, `@container`; custom properties and `var()` substitution with cycle
detection; ~400 properties' specified→computed conversion; inheritance and
initial values; shorthand expansion and serialisation (WPT tests
`getComputedStyle` output character-for-character); media queries; and — the part
people forget — **invalidation**: given an attribute or state change, which
elements must be restyled. Stylo's invalidation directory alone is a
substantial subsystem, and getting it wrong is not a correctness bug you can
ship past, it is a "the page doesn't update when you hover" bug.

Realistic estimate: **this is not a phase, it is a project comparable in size to
Phases 4–9 combined.** Servo has had funded engineers on it since 2012. Against
build-spec §12's own calibration ("Solo with AI assistance, Phases 0–9 are
genuinely achievable"), replacing stylo converts a plan that closes into one
that does not.

### 5.2 What you buy

Not nothing. The honest list:
- `forbid(unsafe_code)` stays true and *means* something in the style engine.
- The unsafe-count baseline does not jump; `cargo-vet` scope does not expand by
  ~45 crates.
- No Python in the build; the Phase 0 reproducibility claim survives intact.
- No `rayon` thread pool inside a `panic = "abort"` content process.
- No churn treadmill against a dependency that has published 24 breaking
  versions in 28 months and whose trait surface changed as recently as
  2026-06-30.
- The cascade you write can be *smaller* than stylo's: no Gecko back-end, no
  XUL, no SMIL, no shared-memory UA sheets, no Houdini painters, no visited-link
  machinery. Stylo carries a lot of weight Pelorus will never lift, and its
  `TElement` makes you implement or stub all of it.

### 5.3 Realistic signals during Phase 5 that stylo is the wrong bet

State these in the Phase 4 ADR so Phase 5 has a decision rule rather than a
judgement call at the end.

- **Any required `unsafe` block in `px-css`.** Under §3.2 the impl should need
  `unsafe fn` declarations and nothing else. If a real `unsafe` block or an
  `unsafe impl Send` proves necessary, the DOM shape and stylo's assumptions
  have not actually reconciled — you have adopted Servo's convention-based model
  under a different name, and §4's memory-safety claim degrades.
- **`px-dom` needing interior mutability in places stylo forced and nothing else
  wants.** One or two atomics is a fine price. If the arena ends up with
  `UnsafeCell` in the *structural* fields (links, parents), stop.
- **Being unable to satisfy `TElement` without stubs that break WPT.** Count the
  methods you can only stub (`is_visited_link`, `smil_override`,
  `each_exported_part`, `implicit_scope_for_sheet`, …) and check them against
  the `css/css-cascade` subset. Stubs that lose tests are informative; stubs
  that lose nothing are free.
- **A `stylo` minor bump breaking the impl more than once during Phase 5.** Two
  breakages in one phase, at one release per month, projects to a permanent
  maintenance tax for a solo maintainer for the life of the project. Measure it;
  do not estimate it.
- **The build-reproducibility gate cannot be made green.** §1.4. If pinning
  Python does not restore byte-identical output across two machines, invariant 7
  is gone and that is a product-level regression, not a build annoyance.
- **Not a signal:** integration being slow or unpleasant, or the trait surface
  being large. Seventy methods is a week of typing, not a design failure.

### 5.4 The middle path worth naming

There is a third option the gate's binary phrasing hides: **take `selectors` and
`cssparser`, write the cascade.** `selectors/` is 314 KiB against `style/`'s
4.98 MiB, has a much smaller trait (`selectors::Element`, ~30 methods, no
`unsafe fn`, no `SendNode`, no `ElementData`), and the genuinely hard, spec-dense,
easy-to-get-subtly-wrong part of CSS matching is selector matching, not cascade
bookkeeping. It also drops `rayon`, `to_shmem`, `servo_arc` and the Python build
step. It does *not* drop the ~400-property computed-value problem, which is the
bulk of `style/`. Worth costing explicitly in the Phase 5 decision rather than
being framed as "stylo or nothing."

---

## 6. Unknowns — what I could not determine without trying it

Explicitly: these are gaps, not omissions.

1. **Whether the §3.2 borrowed-view design survives contact.** The trait
   signatures say it should, and Servo's `'dom`-parameterised handle is the same
   shape. I did not compile a `TElement` impl against `stylo` 0.21. The specific
   risks are the `where Self: 'a` methods (`style_data`,
   `each_applicable_non_document_style_rule_data`, `elements_with_id`) and
   whether their elided lifetimes unify cleanly with a two-reference handle.
   **A one-day Phase 4 spike — a stub `TElement` impl over a toy chunked arena,
   compiled against the pinned stylo — would remove almost all remaining
   uncertainty in this note.** I recommend that spike be the *first* Phase 4
   task, before the tree builder.

2. **Whether eagerly-allocated `ElementData` is behaviourally equivalent to
   Servo's lazy allocation** (§4 item 3). The `has_data()` bit should preserve
   `clear_descendant_data`'s pruning and `element_needs_traversal`'s logic, but
   I traced only those two call sites, not every consumer of `has_data()`.

3. **Whether stylo can be built and run without a full `Device` /
   `FontMetricsProvider`.** `SharedStyleContext` reaches the `Stylist`'s
   `Device` for viewport size and device pixel ratio; font-relative units
   (`ex`, `ch`, `lh`) need font metrics, which is `px-text` — Phase 9. How much
   of the `css/css-cascade` WPT subset is reachable with a stub metrics provider
   is unknown to me and directly determines whether the Phase 5 gate is
   achievable before Phase 9.

4. **The real cost of the stylo pref system.** `stylo_static_prefs` generates
   process-global atomics from a build-time list, read via `pref!` (e.g.
   `layout.css.stylo-work-unit-size`, which gates parallelism). I did not
   determine how an embedder that is not Servo supplies or overrides the pref
   list, or whether the generated defaults are usable as-is.

5. **`icu_segmenter`'s `compiled_data` footprint.** It is a non-optional
   dependency of `style` and ships embedded ICU data. Binary-size and
   `data/`-versioning implications (build-spec §3 versions PSL/HSTS/root store
   deliberately; embedded ICU data is a fourth versioned data set nobody has
   decided about) are unquantified here.

6. **How `rayon` behaves in a `panic = "abort"` content process under seccomp.**
   Even at zero threads, `num_cpus` and `rayon-core` initialisation run. Phase 2
   has a permissive policy so this will not bite until Phase 17, which is
   exactly the wrong time to find out.

7. **Servo/stylo version skew.** I read Servo's `main` and stylo's `main`, which
   are not the same tree. Servo pins a stylo release; a couple of things I cited
   from Servo (e.g. `ServoDangerousStyleNode` not implementing `depth`, which
   `dom.rs` at stylo HEAD declares without a default) suggest a small skew. This
   does not change any conclusion, but pin both deliberately in Phase 5 and read
   the *pinned* `dom.rs`, not `main`.

8. **Not investigated at all:** `style::dom_apis` (`querySelector` /
   `matches` — Phase 11's dependency on this same trait impl), animation and
   transition requirements (`animation_declarations`, `transition_rule`,
   `has_css_animations` are non-default `TElement` methods that Phase 5 must
   implement or stub), and the shared-lock model
   (`SharedRwLock`/`StylesheetGuards`) governing stylesheet mutation vs.
   traversal.

---

*Sources: all file paths above are in `servo/stylo` @ `e81a3d9ad10d6b2d97cb53a7930eb806ea5051a4`
and `servo/servo` @ `main`, both fetched 2026-09-08. crates.io metadata for
`stylo` 0.21.0 fetched the same day. The `forbid(unsafe_code)` behaviour in §3.6
was verified by compiling a probe with the repository's pinned toolchain,
rustc 1.98.1 (48a229cea 2026-09-01).*

---

## 7. Checked against `stylo 0.21.0` in Phase 5 — three corrections

Sections 1–6 were written against `servo/stylo` HEAD `e81a3d9` on 2026-09-08.
ADR 023 takes the **published 0.21.0**, and Phase 5 compiled against it. Three
things this note got wrong, recorded here rather than in a commit message
because this is the document people will read first.

**The integration surface, measured.** Nobody had this number. Required methods
an implementor must write, counted from the published crate:

| trait | required fn | of which `unsafe fn` | provided fn |
|---|---:|---:|---:|
| `TNode` | 13 | 0 | 7 |
| `TElement` | 35 | 5 | 43 |
| `TDocument` | 4 | 0 | 1 |
| `TShadowRoot` | 2 | 0 | 4 |
| `selectors::Element` (0.40.0) | 20 | 0 | 10 |
| **total** | **74** | **5** | **65** |

**§3.1's collision is narrower than stated, and for `TNode` it does not exist.**
The note says §4.1's `Option`-returning accessors "genuinely cannot implement
`TNode` — dozens of its methods are infallible". In 0.21.0 every one of
`TNode`'s tree accessors — `parent_node`, `first_child`, `last_child`,
`prev_sibling`, `next_sibling` — **returns `Option<Self>` already**. They line up
with §4.1 with no adaptation at all. The two infallible ones, `owner_doc` and
`is_in_document`, are infallible about things that cannot fail. The `Copy`
requirement is the real constraint, and a shared reference plus an 8-byte
`NodeId` satisfies it.

**§3.6 said six `unsafe fn`; there are eight, five required.** `TElement`
declares `set_handled_snapshot`, `set_dirty_descendants`,
`unset_dirty_descendants`, `ensure_data` and `clear_data` without bodies, and
`set_animation_only_dirty_descendants`, its `unset_` partner and
`clear_descendant_bits` with them. The conclusion is unaffected — one required
`unsafe fn` is enough to make `#![forbid(unsafe_code)]` impossible, which is
ADR 024 — but the count in the note was not the count in the crate.

**What §3.2's design cost in practice: nothing yet.** `crates/px-css/src/view.rs`
implements the borrowed view and compiles against `px-dom` **unchanged** — no
edit to the slot layout, to `NodeId`'s representation, or to the opaque packing.
That is ADR 021's tripwire, and so far it has not fired. `ci/gate-style.sh`
asserts it mechanically by pinning the blob hashes of `px-dom`'s `tests/layout.rs`
and `tests/opaque.rs`, so the claim is checked on every push rather than
remembered.
