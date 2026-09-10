//! Mutation-side snapshots: what an element looked like *before* it changed.
//!
//! Nothing in this crate reads these. They exist because stylo's invalidation
//! needs them and because they are a **mutation-path** feature — every
//! attribute write has to record one — so the cost of adding them is
//! proportional to the number of mutation sites that exist when you do it.
//! `px-dom` has three today. It will have dozens once there is a scripting
//! surface, and `docs/research/stylo-requirements.md` puts it plainly:
//! *"Retrofitting it in Phase 5 means touching every mutation site twice. Add
//! it to the Phase 4 scope and gate."*
//!
//! # What stylo needs, and what this deliberately is not
//!
//! Stylo wants `ServoElementSnapshot`-shaped records: the prior `ElementState`,
//! the prior attribute values, and `class_changed` / `id_changed` /
//! `other_attributes_changed` flags, keyed by an opaque node id, plus
//! `has_snapshot` / `handled_snapshot` bits on the element.
//!
//! This is that shape in **our** types. It does not import stylo, because
//! stylo is a Phase 5 dependency and would need an ADR, and because a
//! Phase 4 crate taking a dependency on the thing Phase 5 exists to try is
//! backwards — §9 calls Phase 5 the phase most likely to force a `px-dom`
//! redesign. `ElementState` is absent for the same reason: it is stylo's type
//! and there is no state to record until something computes style.
//!
//! # The rule that makes a snapshot useful
//!
//! A snapshot holds the element's state **as of the last restyle**, not as of
//! the last mutation. So the first write since a flush captures; every write
//! after that only updates the change flags and leaves the captured values
//! alone.
//!
//! Getting that backwards produces a snapshot that says an attribute changed
//! from its second-most-recent value to its most recent, which for
//! invalidation purposes is a description of a change that never happened.
//! It is also invisible: the flags are right, the values are plausible, and
//! the selectors that get re-matched are simply the wrong ones.

use html5ever::{Attribute, QualName, local_name, ns};

/// What an element looked like before the first mutation since the last flush.
#[derive(Clone, Debug, PartialEq)]
pub struct ElementSnapshot {
    attrs: Vec<Attribute>,
    class_changed: bool,
    id_changed: bool,
    other_attributes_changed: bool,
}

impl ElementSnapshot {
    pub(crate) fn capture(attrs: &[Attribute]) -> Self {
        Self {
            attrs: attrs.to_vec(),
            class_changed: false,
            id_changed: false,
            other_attributes_changed: false,
        }
    }

    /// The attributes as they were. Not the current ones.
    pub fn attrs(&self) -> &[Attribute] {
        &self.attrs
    }

    /// Whether `class` changed since the snapshot was taken.
    pub fn class_changed(&self) -> bool {
        self.class_changed
    }

    /// Whether `id` changed.
    pub fn id_changed(&self) -> bool {
        self.id_changed
    }

    /// Whether anything other than `class` or `id` changed.
    ///
    /// Separate from the other two because stylo's invalidation treats them
    /// separately: `class` and `id` have their own selector-matching fast
    /// paths, and a change to either invalidates a different set of rules than
    /// a change to, say, `href`.
    pub fn other_attributes_changed(&self) -> bool {
        self.other_attributes_changed
    }

    /// The value an attribute had at snapshot time, if it had one.
    pub fn attr(&self, name: &QualName) -> Option<&str> {
        self.attrs
            .iter()
            .find(|attr| attr.name == *name)
            .map(|attr| &*attr.value)
    }

    /// Record that `name` was written, updating the right flag.
    pub(crate) fn note_change(&mut self, name: &QualName) {
        // Namespace matters: an `xlink:class` is not the `class` selectors
        // match against, and counting it as one would invalidate every class
        // rule on the page for an attribute nothing selects on.
        if name.ns == ns!() && name.local == local_name!("class") {
            self.class_changed = true;
        } else if name.ns == ns!() && name.local == local_name!("id") {
            self.id_changed = true;
        } else {
            self.other_attributes_changed = true;
        }
    }
}
