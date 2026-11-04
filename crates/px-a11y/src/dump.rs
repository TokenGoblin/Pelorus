//! A hand-written text serialisation of the tree, for tests.
//!
//! One node per line, two spaces of indent per level:
//!
//! ```text
//! document name="Page"
//!   heading name="Title" level=1
//!     text "Title"
//!   button name="Save" [focusable disabled]
//! ```
//!
//! After the role come the name and description (quoted, escaped), then
//! properties (`level=`, `posinset=`, `setsize=`, `value=`, `valuenow=`,
//! `valuemin=`, `valuemax=`, `href=`, `live=`, `atomic`, `relevant=`), then
//! the states in brackets. A state that does not apply is absent;
//! `checked`/`unchecked`/`mixed` are three different statements.
//! Text runs print as `text "..."`.
//!
//! The format is stable on purpose: expectations in tests are written against
//! it, so changing it is a change to every test.

use std::fmt::Write as _;

use crate::input::Bounds;
use crate::role::Role;
use crate::state::{Current, HasPopup, Invalid, Politeness, Tri};
use crate::tree::{A11yNode, AccessibilityTree};

/// What [`AccessibilityTree::dump_with`] includes.
#[derive(Clone, Copy, Debug, Default)]
pub struct DumpOptions {
    /// Append `@x,y WxH` for nodes with bounds.
    pub bounds: bool,
}

/// Escape a string for the dump: backslash, quote, and control characters.
fn quote(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{{{:x}}}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn push_bounds(out: &mut String, b: Bounds) {
    let _ = write!(out, " @{},{} {}x{}", b.x, b.y, b.width, b.height);
}

fn line(out: &mut String, n: &A11yNode, opts: DumpOptions) {
    if n.role == Role::StaticText {
        out.push_str("text ");
        quote(out, &n.name);
        if opts.bounds
            && let Some(b) = n.bounds
        {
            push_bounds(out, b);
        }
        return;
    }
    out.push_str(n.role.token());
    if !n.name.is_empty() {
        out.push_str(" name=");
        quote(out, &n.name);
    }
    if !n.description.is_empty() {
        out.push_str(" description=");
        quote(out, &n.description);
    }
    let p = &n.props;
    if let Some(v) = p.level {
        let _ = write!(out, " level={v}");
    }
    if let Some(v) = p.pos_in_set {
        let _ = write!(out, " posinset={v}");
    }
    if let Some(v) = p.set_size {
        let _ = write!(out, " setsize={v}");
    }
    if let Some(v) = &p.value {
        out.push_str(" value=");
        quote(out, v);
    }
    if let Some(r) = p.range {
        if let Some(v) = r.now {
            let _ = write!(out, " valuenow={v}");
        }
        if let Some(v) = r.min {
            let _ = write!(out, " valuemin={v}");
        }
        if let Some(v) = r.max {
            let _ = write!(out, " valuemax={v}");
        }
    }
    if let Some(h) = &p.href {
        out.push_str(" href=");
        quote(out, h);
    }
    if p.live.politeness != Politeness::Off {
        out.push_str(match p.live.politeness {
            Politeness::Polite => " live=polite",
            _ => " live=assertive",
        });
        if p.live.atomic {
            out.push_str(" atomic");
        }
        let r = p.live.relevant;
        let mut parts: Vec<&str> = Vec::new();
        if r.additions {
            parts.push("additions");
        }
        if r.removals {
            parts.push("removals");
        }
        if r.text {
            parts.push("text");
        }
        let _ = write!(out, " relevant={}", parts.join(","));
    }

    let s = &n.states;
    let mut flags: Vec<String> = Vec::new();
    let mut flag = |f: &str| flags.push(f.to_string());
    if s.focusable {
        flag("focusable");
    }
    match n.tab_index {
        Some(t) if t != 0 => flag(&format!("tabindex={t}")),
        _ => {}
    }
    if s.disabled {
        flag("disabled");
    }
    if s.required {
        flag("required");
    }
    if s.read_only {
        flag("readonly");
    }
    match s.invalid {
        Invalid::False => {}
        Invalid::True => flag("invalid"),
        Invalid::Grammar => flag("invalid=grammar"),
        Invalid::Spelling => flag("invalid=spelling"),
    }
    match s.checked {
        None => {}
        Some(Tri::True) => flag("checked"),
        Some(Tri::False) => flag("unchecked"),
        Some(Tri::Mixed) => flag("mixed"),
    }
    match s.pressed {
        None => {}
        Some(Tri::True) => flag("pressed"),
        Some(Tri::False) => flag("unpressed"),
        Some(Tri::Mixed) => flag("pressed=mixed"),
    }
    match s.selected {
        None => {}
        Some(true) => flag("selected"),
        Some(false) => flag("unselected"),
    }
    match s.expanded {
        None => {}
        Some(true) => flag("expanded"),
        Some(false) => flag("collapsed"),
    }
    if s.multiline {
        flag("multiline");
    }
    if s.modal {
        flag("modal");
    }
    if s.busy {
        flag("busy");
    }
    match s.current {
        Current::False => {}
        Current::True => flag("current"),
        Current::Page => flag("current=page"),
        Current::Step => flag("current=step"),
        Current::Location => flag("current=location"),
        Current::Date => flag("current=date"),
        Current::Time => flag("current=time"),
    }
    match s.has_popup {
        HasPopup::False => {}
        HasPopup::Menu => flag("haspopup=menu"),
        HasPopup::Listbox => flag("haspopup=listbox"),
        HasPopup::Tree => flag("haspopup=tree"),
        HasPopup::Grid => flag("haspopup=grid"),
        HasPopup::Dialog => flag("haspopup=dialog"),
    }
    if !flags.is_empty() {
        let _ = write!(out, " [{}]", flags.join(" "));
    }
    if opts.bounds
        && let Some(b) = n.bounds
    {
        push_bounds(out, b);
    }
}

impl AccessibilityTree {
    /// The text serialisation, without bounds.
    #[must_use]
    pub fn dump(&self) -> String {
        self.dump_with(DumpOptions::default())
    }

    /// The text serialisation with the given options.
    #[must_use]
    pub fn dump_with(&self, opts: DumpOptions) -> String {
        let mut out = String::new();
        let mut stack: Vec<(u32, usize)> = Vec::new();
        if !self.is_empty() {
            stack.push((0, 0));
        }
        while let Some((i, depth)) = stack.pop() {
            let Some(n) = self.raw_nodes().get(i as usize) else {
                continue;
            };
            for _ in 0..depth {
                out.push_str("  ");
            }
            line(&mut out, n, opts);
            out.push('\n');
            for c in n.children.iter().rev() {
                stack.push((*c, depth + 1));
            }
        }
        out
    }
}
