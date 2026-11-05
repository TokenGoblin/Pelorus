//! Keyboard shortcuts of the chrome, as a pure table.
//!
//! Keyboard first (this crate's CLAUDE.md): every chrome action has a key. The
//! table maps a [`KeyEvent`] to a [`Command`]; it knows nothing about focus. The
//! chrome consults it before the address field, so these work while typing, and
//! hands everything else to the field or the page.

use crate::input::{Key, KeyEvent, PrimaryModifier};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    FocusAddress,
    NewTab,
    CloseTab,
    NextTab,
    PrevTab,
    /// 0-based; the ninth shortcut is `LastTab`.
    SelectTab(usize),
    LastTab,
    MoveTabLeft,
    MoveTabRight,
    Back,
    Forward,
    Reload,
    /// Escape while loading and not editing.
    Stop,
}

/// The command for `ev`, if it is a chrome shortcut. `primary` is the
/// platform's shortcut modifier.
pub fn resolve(ev: KeyEvent, primary: PrimaryModifier) -> Option<Command> {
    let m = ev.mods;
    let cmd = m.primary(primary);
    match ev.key {
        Key::Char('l') | Key::Char('k') if cmd && !m.alt && !m.shift => Some(Command::FocusAddress),
        Key::F(6) if !cmd && !m.alt => Some(Command::FocusAddress),
        Key::Char('t') if cmd && !m.alt && !m.shift => Some(Command::NewTab),
        Key::Char('w') | Key::F(4) if cmd && !m.alt && !m.shift => Some(Command::CloseTab),
        // Tab cycling is Control+Tab on every OS, including macOS.
        Key::Tab if m.ctrl && !m.alt => Some(if m.shift {
            Command::PrevTab
        } else {
            Command::NextTab
        }),
        Key::PageDown if m.ctrl && !m.alt && !m.shift => Some(Command::NextTab),
        Key::PageUp if m.ctrl && !m.alt && !m.shift => Some(Command::PrevTab),
        Key::PageDown if m.ctrl && m.shift => Some(Command::MoveTabRight),
        Key::PageUp if m.ctrl && m.shift => Some(Command::MoveTabLeft),
        Key::Char(c @ '1'..='9') if cmd && !m.alt && !m.shift => {
            if c == '9' {
                Some(Command::LastTab)
            } else {
                c.to_digit(10).map(|d| Command::SelectTab(d as usize - 1))
            }
        }
        Key::Left if m.alt && !cmd && !m.shift => Some(Command::Back),
        Key::Right if m.alt && !cmd && !m.shift => Some(Command::Forward),
        Key::F(5) if !m.alt => Some(Command::Reload),
        Key::Char('r') if cmd && !m.alt && !m.shift => Some(Command::Reload),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::Modifiers;

    fn m(ctrl: bool, shift: bool, alt: bool, meta: bool) -> Modifiers {
        Modifiers {
            shift,
            alt,
            ctrl,
            meta,
        }
    }

    fn r(key: Key, mods: Modifiers, p: PrimaryModifier) -> Option<Command> {
        resolve(KeyEvent::with(key, mods), p)
    }

    #[test]
    fn the_basic_set_on_windows_and_linux() {
        let p = PrimaryModifier::Ctrl;
        let c = m(true, false, false, false);
        assert_eq!(r(Key::Char('l'), c, p), Some(Command::FocusAddress));
        assert_eq!(r(Key::Char('t'), c, p), Some(Command::NewTab));
        assert_eq!(r(Key::Char('w'), c, p), Some(Command::CloseTab));
        assert_eq!(r(Key::Char('r'), c, p), Some(Command::Reload));
        assert_eq!(r(Key::Char('1'), c, p), Some(Command::SelectTab(0)));
        assert_eq!(r(Key::Char('8'), c, p), Some(Command::SelectTab(7)));
        assert_eq!(r(Key::Char('9'), c, p), Some(Command::LastTab));
        assert_eq!(r(Key::Char('0'), c, p), None);
        assert_eq!(r(Key::Tab, c, p), Some(Command::NextTab));
        assert_eq!(
            r(Key::Tab, m(true, true, false, false), p),
            Some(Command::PrevTab)
        );
        assert_eq!(
            r(Key::PageDown, m(true, true, false, false), p),
            Some(Command::MoveTabRight)
        );
        assert_eq!(
            r(Key::Left, m(false, false, true, false), p),
            Some(Command::Back)
        );
        assert_eq!(
            r(Key::Right, m(false, false, true, false), p),
            Some(Command::Forward)
        );
        assert_eq!(r(Key::F(5), Modifiers::NONE, p), Some(Command::Reload));
        assert_eq!(
            r(Key::F(6), Modifiers::NONE, p),
            Some(Command::FocusAddress)
        );
    }

    #[test]
    fn on_macos_command_is_primary_but_tab_cycling_stays_on_control() {
        let p = PrimaryModifier::Meta;
        assert_eq!(
            r(Key::Char('t'), m(false, false, false, true), p),
            Some(Command::NewTab)
        );
        assert_eq!(r(Key::Char('t'), m(true, false, false, false), p), None);
        assert_eq!(
            r(Key::Tab, m(true, false, false, false), p),
            Some(Command::NextTab)
        );
    }

    #[test]
    fn editing_keys_are_not_shortcuts() {
        let p = PrimaryModifier::Ctrl;
        let c = m(true, false, false, false);
        for ch in ['a', 'c', 'x', 'v', 'z'] {
            assert_eq!(r(Key::Char(ch), c, p), None, "{ch}");
        }
        assert_eq!(r(Key::Left, Modifiers::NONE, p), None);
        assert_eq!(r(Key::Char('t'), Modifiers::NONE, p), None);
        assert_eq!(r(Key::Char('t'), m(true, true, false, false), p), None);
    }
}
