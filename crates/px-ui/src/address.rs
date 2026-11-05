//! The address bar as a state machine: focus, editing, selection, keys.
//!
//! Two modes. *Idle* shows the page's origin as [`UrlDisplay`]: the registrable
//! domain emphasised, the rest de-emphasised, no padlock. *Editing* shows a text
//! buffer with a caret and selection. Focusing selects everything (so typing
//! replaces the URL); Enter submits the raw text as [`AppRequest::Navigate`] and
//! leaves it to the browser to decide URL versus search; Escape first reverts an
//! edit, then leaves.
//!
//! Positions are byte offsets on `char` boundaries. Caret movement is by `char`,
//! not by grapheme cluster, and ignores bidi order: both need segmentation and
//! reordering tables that this crate does not carry (see the report).

use crate::input::{AppRequest, Effect, Key, KeyEvent, PlatformEffect, Preedit, PrimaryModifier};
use crate::text;

/// Longest address text accepted, in characters. Input past it is cut.
pub const MAX_CHARS: usize = 8192;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Emphasis {
    Strong,
    Muted,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Segment {
    pub text: String,
    pub emphasis: Emphasis,
}

/// How the current page's address is shown when the field is not being edited,
/// plus the text editing starts from.
///
/// The browser builds it: it alone has the public-suffix list (to find the
/// registrable domain) and the IDN rules (to turn a mixed-script label into
/// punycode). The chrome draws what it is given and does no parsing of its own,
/// so there is exactly one place that decides what an origin looks like.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UrlDisplay {
    pub segments: Vec<Segment>,
    pub edit_text: String,
}

impl UrlDisplay {
    /// `hidden_prefix` (for example a scheme) is in the edit text but not drawn;
    /// `subdomains` and `rest` are muted; `registrable` is the one emphasised
    /// part. Unsafe characters are removed from every part.
    pub fn from_parts(
        hidden_prefix: &str,
        subdomains: &str,
        registrable: &str,
        rest: &str,
    ) -> Self {
        let clean =
            |s: &str| -> String { s.chars().filter(|c| !text::is_unsafe_char(*c)).collect() };
        let (p, s, g, r) = (
            clean(hidden_prefix),
            clean(subdomains),
            clean(registrable),
            clean(rest),
        );
        let mut segments = Vec::new();
        for (t, e) in [
            (&s, Emphasis::Muted),
            (&g, Emphasis::Strong),
            (&r, Emphasis::Muted),
        ] {
            if !t.is_empty() {
                segments.push(Segment {
                    text: t.clone(),
                    emphasis: e,
                });
            }
        }
        UrlDisplay {
            segments,
            edit_text: cap(format!("{p}{s}{g}{r}")),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }
}

fn cap(s: String) -> String {
    match s.char_indices().nth(MAX_CHARS) {
        Some((i, _)) => s[..i].to_string(),
        None => s,
    }
}

#[derive(Clone, Debug, Default)]
pub struct AddressBar {
    display: UrlDisplay,
    editing: bool,
    text: String,
    cursor: usize,
    anchor: usize,
    preedit: Option<String>,
    /// The user changed the text since focus; Escape reverts instead of leaving.
    edited: bool,
    isolated: bool,
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric()
}

impl AddressBar {
    pub fn new() -> AddressBar {
        AddressBar::default()
    }

    // -- state in -------------------------------------------------------------

    pub fn set_display(&mut self, display: UrlDisplay) {
        self.display = display;
        if self.editing && !self.edited {
            self.text = self.display.edit_text.clone();
            self.select_all();
        }
    }

    pub fn set_isolated(&mut self, isolated: bool) {
        self.isolated = isolated;
    }

    // -- state out ------------------------------------------------------------

    pub fn is_editing(&self) -> bool {
        self.editing
    }

    pub fn is_isolated(&self) -> bool {
        self.isolated
    }

    pub fn display(&self) -> &UrlDisplay {
        &self.display
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn is_edited(&self) -> bool {
        self.edited
    }

    /// Selected byte range, ordered, if non-empty.
    pub fn selection(&self) -> Option<(usize, usize)> {
        let (a, b) = (self.anchor.min(self.cursor), self.anchor.max(self.cursor));
        (a != b).then_some((a, b))
    }

    pub fn selected_text(&self) -> Option<&str> {
        self.selection().and_then(|(a, b)| self.text.get(a..b))
    }

    /// The text as drawn while editing: the buffer with the IME preedit spliced
    /// in at the caret, and the byte offset where the caret is drawn.
    pub fn visible_text(&self) -> (String, usize) {
        match &self.preedit {
            Some(p) if self.editing => {
                let (head, tail) = self.text.split_at(self.cursor);
                (format!("{head}{p}{tail}"), self.cursor + p.len())
            }
            _ => (self.text.clone(), self.cursor),
        }
    }

    /// Byte range of the preedit inside [`AddressBar::visible_text`].
    pub fn preedit_range(&self) -> Option<(usize, usize)> {
        let p = self.preedit.as_ref().filter(|_| self.editing)?;
        Some((self.cursor, self.cursor + p.len()))
    }

    // -- focus ----------------------------------------------------------------

    /// Ctrl+L, F6, or the first click: edit, with everything selected.
    pub fn focus(&mut self) -> Vec<Effect> {
        let was = self.editing;
        self.editing = true;
        self.edited = false;
        self.preedit = None;
        self.text = self.display.edit_text.clone();
        self.select_all();
        if was {
            Vec::new()
        } else {
            vec![Effect::Platform(PlatformEffect::SetImeAllowed(true))]
        }
    }

    pub fn blur(&mut self) -> Vec<Effect> {
        if !self.editing {
            return Vec::new();
        }
        self.editing = false;
        self.edited = false;
        self.preedit = None;
        self.text.clear();
        self.cursor = 0;
        self.anchor = 0;
        vec![Effect::Platform(PlatformEffect::SetImeAllowed(false))]
    }

    // -- editing primitives ---------------------------------------------------

    fn select_all(&mut self) {
        self.anchor = 0;
        self.cursor = self.text.len();
    }

    fn prev_char(&self, i: usize) -> usize {
        self.text
            .get(..i)
            .and_then(|s| s.char_indices().next_back())
            .map_or(0, |(j, _)| j)
    }

    fn next_char(&self, i: usize) -> usize {
        self.text
            .get(i..)
            .and_then(|s| s.chars().next())
            .map_or(self.text.len(), |c| i + c.len_utf8())
    }

    fn prev_word(&self, i: usize) -> usize {
        let head: Vec<(usize, char)> = self
            .text
            .get(..i)
            .map_or_else(Vec::new, |s| s.char_indices().collect());
        let mut k = head.len();
        while k > 0 && head.get(k - 1).is_some_and(|(_, c)| !is_word(*c)) {
            k -= 1;
        }
        while k > 0 && head.get(k - 1).is_some_and(|(_, c)| is_word(*c)) {
            k -= 1;
        }
        // `k` is `head.len()` only when `head` is empty, i.e. at the start.
        head.get(k).map_or(0, |(j, _)| *j)
    }

    fn next_word(&self, i: usize) -> usize {
        let tail: Vec<(usize, char)> = self
            .text
            .get(i..)
            .map_or_else(Vec::new, |s| s.char_indices().collect());
        let mut k = 0;
        while tail.get(k).is_some_and(|(_, c)| is_word(*c)) {
            k += 1;
        }
        while tail.get(k).is_some_and(|(_, c)| !is_word(*c)) {
            k += 1;
        }
        tail.get(k).map_or(self.text.len(), |(j, _)| i + *j)
    }

    fn delete_range(&mut self, a: usize, b: usize) {
        if a < b && b <= self.text.len() {
            self.text.replace_range(a..b, "");
            self.cursor = a;
            self.anchor = a;
            self.edited = true;
        }
    }

    fn delete_selection(&mut self) -> bool {
        match self.selection() {
            Some((a, b)) => {
                self.delete_range(a, b);
                true
            }
            None => false,
        }
    }

    fn move_to(&mut self, to: usize, extend: bool) {
        self.cursor = to.min(self.text.len());
        if !extend {
            self.anchor = self.cursor;
        }
    }

    /// Replace the selection with `s`, filtered and cut to [`MAX_CHARS`].
    /// Controls (so newlines in a paste) and bidi overrides are dropped.
    pub fn insert_text(&mut self, s: &str) {
        if !self.editing {
            return;
        }
        self.preedit = None;
        let clean: String = s.chars().filter(|c| !text::is_unsafe_char(*c)).collect();
        if clean.is_empty() {
            return;
        }
        self.delete_selection();
        let room = MAX_CHARS.saturating_sub(self.text.chars().count());
        let clean: String = clean.chars().take(room).collect();
        if clean.is_empty() {
            return;
        }
        self.text.insert_str(self.cursor, &clean);
        self.cursor += clean.len();
        self.anchor = self.cursor;
        self.edited = true;
    }

    pub fn set_preedit(&mut self, p: Option<Preedit>) {
        if !self.editing {
            return;
        }
        self.preedit = p
            .map(|p| {
                p.text
                    .chars()
                    .filter(|c| !text::is_unsafe_char(*c))
                    .collect::<String>()
            })
            .filter(|t| !t.is_empty());
    }

    /// Caret placement by pointer: `index` is a byte boundary the platform-free
    /// caller found with [`crate::text::Shaped::boundary_at`].
    pub fn place_caret(&mut self, index: usize, extend: bool) {
        if self.editing {
            self.move_to(index, extend);
        }
    }

    // -- keys -----------------------------------------------------------------

    /// A key while editing. Everything is consumed: the page never sees a
    /// keystroke meant for the address field.
    pub fn key(&mut self, ev: KeyEvent, primary: PrimaryModifier) -> Vec<Effect> {
        if !self.editing {
            return Vec::new();
        }
        let m = ev.mods;
        let cmd = m.primary(primary);
        let word = cmd || m.alt;
        match ev.key {
            Key::Enter => {
                let text = self.text.trim().to_string();
                let mut out = self.blur();
                if !text.is_empty() {
                    out.push(Effect::App(AppRequest::Navigate(text)));
                }
                return out;
            }
            Key::Escape => {
                if self.edited {
                    self.text = self.display.edit_text.clone();
                    self.select_all();
                    self.edited = false;
                    self.preedit = None;
                } else {
                    return self.blur();
                }
            }
            Key::Tab => return self.blur(),
            Key::Backspace => {
                if !self.delete_selection() {
                    let to = if word {
                        self.prev_word(self.cursor)
                    } else {
                        self.prev_char(self.cursor)
                    };
                    self.delete_range(to, self.cursor);
                }
            }
            Key::Delete => {
                if !self.delete_selection() {
                    let to = if word {
                        self.next_word(self.cursor)
                    } else {
                        self.next_char(self.cursor)
                    };
                    self.delete_range(self.cursor, to);
                }
            }
            Key::Left => {
                if let (Some((a, _)), false, false) = (self.selection(), m.shift, word) {
                    self.move_to(a, false);
                } else {
                    let to = if word {
                        self.prev_word(self.cursor)
                    } else {
                        self.prev_char(self.cursor)
                    };
                    self.move_to(to, m.shift);
                }
            }
            Key::Right => {
                if let (Some((_, b)), false, false) = (self.selection(), m.shift, word) {
                    self.move_to(b, false);
                } else {
                    let to = if word {
                        self.next_word(self.cursor)
                    } else {
                        self.next_char(self.cursor)
                    };
                    self.move_to(to, m.shift);
                }
            }
            Key::Home | Key::Up => self.move_to(0, m.shift),
            Key::End | Key::Down => self.move_to(self.text.len(), m.shift),
            Key::Char('a') if cmd => self.select_all(),
            Key::Char('c') if cmd => {
                return self.copy();
            }
            Key::Char('x') if cmd => {
                let out = self.copy();
                self.delete_selection();
                return out;
            }
            Key::Char('v') if cmd => {
                return vec![Effect::Platform(PlatformEffect::ClipboardRead)];
            }
            _ => {}
        }
        Vec::new()
    }

    fn copy(&self) -> Vec<Effect> {
        match self.selected_text() {
            Some(t) => vec![Effect::Platform(PlatformEffect::ClipboardWrite(
                t.to_string(),
            ))],
            None => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::Modifiers;

    const P: PrimaryModifier = PrimaryModifier::Ctrl;

    fn bar(url: &str) -> AddressBar {
        let mut b = AddressBar::new();
        b.set_display(UrlDisplay::from_parts(
            "https://",
            "www.",
            "example.com",
            url,
        ));
        b
    }

    fn k(key: Key) -> KeyEvent {
        KeyEvent::new(key)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::with(
            Key::Char(c),
            Modifiers {
                ctrl: true,
                ..Modifiers::NONE
            },
        )
    }

    fn shift(key: Key) -> KeyEvent {
        KeyEvent::with(
            key,
            Modifiers {
                shift: true,
                ..Modifiers::NONE
            },
        )
    }

    fn app(e: &[Effect]) -> Vec<&AppRequest> {
        e.iter()
            .filter_map(|e| match e {
                Effect::App(a) => Some(a),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn display_marks_the_registrable_domain_and_hides_the_prefix() {
        let d = UrlDisplay::from_parts("https://", "www.", "example.com", "/a");
        assert_eq!(d.edit_text, "https://www.example.com/a");
        let kinds: Vec<_> = d
            .segments
            .iter()
            .map(|s| (s.text.as_str(), s.emphasis))
            .collect();
        assert_eq!(
            kinds,
            vec![
                ("www.", Emphasis::Muted),
                ("example.com", Emphasis::Strong),
                ("/a", Emphasis::Muted)
            ]
        );
        assert!(UrlDisplay::from_parts("", "", "", "").is_empty());
        // A hostile page cannot smuggle an override into the origin line.
        let d = UrlDisplay::from_parts("", "", "ex\u{202E}ample.com", "");
        assert_eq!(d.edit_text, "example.com");
    }

    #[test]
    fn focus_selects_everything_and_typing_replaces_it() {
        let mut b = bar("/path");
        assert!(!b.is_editing());
        let e = b.focus();
        assert_eq!(
            e,
            vec![Effect::Platform(PlatformEffect::SetImeAllowed(true))]
        );
        assert_eq!(b.selected_text(), Some("https://www.example.com/path"));
        b.insert_text("rust");
        assert_eq!(b.text(), "rust");
        assert_eq!(b.cursor(), 4);
        assert!(b.is_edited());
        // Focusing again while editing re-selects without a second IME effect.
        assert!(b.focus().is_empty());
    }

    #[test]
    fn enter_submits_the_raw_text_and_leaves() {
        let mut b = bar("");
        b.focus();
        b.insert_text("  hello world ");
        let e = b.key(k(Key::Enter), P);
        assert_eq!(app(&e), vec![&AppRequest::Navigate("hello world".into())]);
        assert!(!b.is_editing());
        // Enter on an empty field submits nothing.
        b.focus();
        b.insert_text("   ");
        let e = b.key(k(Key::Enter), P);
        assert!(app(&e).is_empty());
    }

    #[test]
    fn escape_reverts_an_edit_then_leaves() {
        let mut b = bar("/x");
        b.focus();
        b.insert_text("typed");
        b.key(k(Key::Escape), P);
        assert!(b.is_editing());
        assert_eq!(b.text(), "https://www.example.com/x");
        assert!(!b.is_edited());
        assert_eq!(b.selected_text(), Some("https://www.example.com/x"));
        b.key(k(Key::Escape), P);
        assert!(!b.is_editing());
    }

    #[test]
    fn caret_movement_collapses_selection_and_extends_with_shift() {
        let mut b = bar("");
        b.focus();
        b.insert_text("abc def");
        b.key(k(Key::Home), P);
        assert_eq!(b.cursor(), 0);
        b.key(shift(Key::Right), P);
        b.key(shift(Key::Right), P);
        assert_eq!(b.selected_text(), Some("ab"));
        b.key(k(Key::Right), P);
        assert_eq!(
            (b.cursor(), b.selection()),
            (2, None),
            "collapses to the right edge"
        );
        b.key(shift(Key::End), P);
        assert_eq!(b.selected_text(), Some("c def"));
        b.key(k(Key::Left), P);
        assert_eq!(b.cursor(), 2, "collapses to the left edge");
    }

    #[test]
    fn word_movement_and_word_deletion() {
        let mut b = bar("");
        b.focus();
        b.insert_text("foo.bar/baz");
        b.key(ctrl_key(Key::Left), P);
        assert_eq!(b.cursor(), 8);
        b.key(ctrl_key(Key::Left), P);
        assert_eq!(b.cursor(), 4);
        b.key(ctrl_key(Key::Right), P);
        assert_eq!(b.cursor(), 8);
        b.key(ctrl_key(Key::Backspace), P);
        assert_eq!(b.text(), "foo.baz");
        b.key(ctrl_key(Key::Delete), P);
        assert_eq!(b.text(), "foo.");
        b.key(ctrl_key(Key::Delete), P);
        assert_eq!(b.text(), "foo.", "nothing to delete at the end");
    }

    fn ctrl_key(key: Key) -> KeyEvent {
        KeyEvent::with(
            key,
            Modifiers {
                ctrl: true,
                ..Modifiers::NONE
            },
        )
    }

    #[test]
    fn backspace_and_delete_work_by_char_not_byte() {
        let mut b = bar("");
        b.focus();
        b.insert_text("aé\u{1F600}z");
        b.key(k(Key::Left), P);
        b.key(k(Key::Backspace), P);
        assert_eq!(b.text(), "aéz");
        b.key(k(Key::Backspace), P);
        assert_eq!(b.text(), "az");
        b.key(k(Key::Home), P);
        b.key(k(Key::Delete), P);
        assert_eq!(b.text(), "z");
        b.key(k(Key::Backspace), P);
        assert_eq!(b.text(), "z", "backspace at the start is a no-op");
    }

    #[test]
    fn clipboard_keys() {
        let mut b = bar("");
        b.focus();
        b.insert_text("hello");
        assert!(
            b.key(ctrl('c'), P).is_empty(),
            "no selection, nothing to copy"
        );
        b.key(ctrl('a'), P);
        let e = b.key(ctrl('c'), P);
        assert_eq!(
            e,
            vec![Effect::Platform(PlatformEffect::ClipboardWrite(
                "hello".into()
            ))]
        );
        assert_eq!(b.text(), "hello");
        let e = b.key(ctrl('x'), P);
        assert_eq!(
            e,
            vec![Effect::Platform(PlatformEffect::ClipboardWrite(
                "hello".into()
            ))]
        );
        assert_eq!(b.text(), "");
        let e = b.key(ctrl('v'), P);
        assert_eq!(e, vec![Effect::Platform(PlatformEffect::ClipboardRead)]);
        // The platform answers with insert_text.
        b.insert_text("a\r\nb\u{202E}c");
        assert_eq!(
            b.text(),
            "abc",
            "newlines and overrides are stripped from a paste"
        );
    }

    #[test]
    fn the_primary_modifier_is_the_platforms_choice() {
        let mut b = bar("");
        b.focus();
        b.insert_text("x");
        let meta_a = KeyEvent::with(
            Key::Char('a'),
            Modifiers {
                meta: true,
                ..Modifiers::NONE
            },
        );
        b.key(meta_a, PrimaryModifier::Ctrl);
        assert_eq!(b.selection(), None, "meta is not primary on Linux/Windows");
        b.key(meta_a, PrimaryModifier::Meta);
        assert_eq!(b.selected_text(), Some("x"));
    }

    #[test]
    fn input_is_capped() {
        let mut b = bar("");
        b.focus();
        b.insert_text(&"a".repeat(MAX_CHARS - 1));
        b.insert_text("bcdef");
        assert_eq!(b.text().chars().count(), MAX_CHARS);
        assert!(b.text().ends_with('b'));
        b.insert_text("more");
        assert_eq!(b.text().chars().count(), MAX_CHARS);
    }

    #[test]
    fn preedit_is_shown_at_the_caret_and_replaced_by_the_commit() {
        let mut b = bar("");
        b.focus();
        b.insert_text("ab");
        b.key(k(Key::Left), P);
        b.set_preedit(Some(Preedit { text: "ni".into() }));
        assert_eq!(b.visible_text(), ("anib".to_string(), 3));
        assert_eq!(b.preedit_range(), Some((1, 3)));
        assert_eq!(b.text(), "ab", "the buffer is untouched until commit");
        b.insert_text("\u{4F60}");
        assert_eq!(b.text(), "a\u{4F60}b");
        assert_eq!(b.preedit_range(), None);
        b.set_preedit(Some(Preedit {
            text: String::new(),
        }));
        assert_eq!(b.preedit_range(), None);
    }

    #[test]
    fn idle_bar_ignores_keys_and_text() {
        let mut b = bar("");
        assert!(b.key(k(Key::Backspace), P).is_empty());
        b.insert_text("x");
        b.set_preedit(Some(Preedit { text: "y".into() }));
        assert_eq!(b.text(), "");
        assert_eq!(b.preedit_range(), None);
    }

    #[test]
    fn a_navigation_while_unedited_refreshes_the_text_but_never_clobbers_an_edit() {
        let mut b = bar("/one");
        b.focus();
        b.set_display(UrlDisplay::from_parts("https://", "", "other.org", ""));
        assert_eq!(b.text(), "https://other.org");
        b.insert_text("mine");
        b.set_display(UrlDisplay::from_parts("https://", "", "third.org", ""));
        assert_eq!(b.text(), "mine");
    }

    #[test]
    fn tab_leaves_the_field() {
        let mut b = bar("");
        b.focus();
        let e = b.key(k(Key::Tab), P);
        assert_eq!(
            e,
            vec![Effect::Platform(PlatformEffect::SetImeAllowed(false))]
        );
        assert!(!b.is_editing());
    }
}
