//! Keycodes to keysyms to the chrome's logical keys.
//!
//! Only the compiled table from `GetKeyboardMapping` is used (levels 0 and 1 of
//! group 0): no XKB extension, so layout groups, dead keys and Compose are not
//! handled. Text comes from the keysym's code point, so a layout whose letters
//! are in group 1 types nothing until XKB lands (documented gap).

use px_ui::input::{Key, Modifiers};

pub const SHIFT: u16 = 0x1;
pub const LOCK: u16 = 0x2;
pub const CONTROL: u16 = 0x4;
pub const MOD1: u16 = 0x8;
pub const MOD4: u16 = 0x40;

pub fn modifiers(state: u16) -> Modifiers {
    Modifiers {
        shift: state & SHIFT != 0,
        alt: state & MOD1 != 0,
        ctrl: state & CONTROL != 0,
        meta: state & MOD4 != 0,
    }
}

/// The server's keycode-to-keysym table.
#[derive(Clone, Debug, Default)]
pub struct KeyMap {
    min: u8,
    per: usize,
    syms: Vec<u32>,
}

impl KeyMap {
    pub fn new(min: u8, per: usize, syms: Vec<u32>) -> KeyMap {
        KeyMap { min, per, syms }
    }

    /// `(level 0, level 1)` keysyms; level 1 falls back to level 0.
    pub fn lookup(&self, keycode: u8) -> Option<(u32, u32)> {
        if self.per == 0 || keycode < self.min {
            return None;
        }
        let base = usize::from(keycode - self.min).checked_mul(self.per)?;
        let l0 = *self.syms.get(base)?;
        let l1 = self.syms.get(base + 1).copied().filter(|s| *s != 0);
        Some((l0, l1.unwrap_or(l0)))
    }

    /// The keysym a press produces under `state`: shift picks level 1; Caps Lock
    /// does too for letters (a keysym whose two levels differ only by case).
    pub fn keysym(&self, keycode: u8, state: u16) -> Option<u32> {
        let (l0, l1) = self.lookup(keycode)?;
        let shift = state & SHIFT != 0;
        let lock = state & LOCK != 0;
        let letter = char_of(l0).is_some_and(|c| c.is_alphabetic());
        Some(if shift != (lock && letter) { l1 } else { l0 })
    }
}

/// The code point a keysym stands for, if it is a character keysym.
pub fn char_of(sym: u32) -> Option<char> {
    match sym {
        0x20..=0x7e | 0xa0..=0xff => char::from_u32(sym),
        0x0100_0100..=0x0110_ffff => char::from_u32(sym - 0x0100_0000),
        0xff80 => Some(' '), // KP_Space
        0xffaa..=0xffb9 => {
            // Keypad operators and digits.
            char::from_u32(sym - 0xff80)
        }
        _ => None,
    }
}

/// The chrome's logical key for a keysym (taken at level 0, so it is the same
/// with and without shift), or `None` for keys the chrome has no name for.
pub fn logical_key(level0: u32, level_pressed: u32) -> Option<Key> {
    Some(match level_pressed {
        0xff08 => Key::Backspace,
        0xff09 | 0xfe20 => Key::Tab,
        0xff0d | 0xff8d => Key::Enter,
        0xff1b => Key::Escape,
        0xffff => Key::Delete,
        0xff50 => Key::Home,
        0xff51 => Key::Left,
        0xff52 => Key::Up,
        0xff53 => Key::Right,
        0xff54 => Key::Down,
        0xff55 => Key::PageUp,
        0xff56 => Key::PageDown,
        0xff57 => Key::End,
        s @ 0xffbe..=0xffc9 => Key::F((s - 0xffbe + 1) as u8),
        _ => {
            let c = char_of(level0)?;
            Key::Char(c.to_lowercase().next().unwrap_or(c))
        }
    })
}

/// Text to insert for a press, if any: a printable character with no
/// Control/Alt/Super held (those are shortcuts, not typing).
pub fn text_of(sym: u32, state: u16) -> Option<String> {
    if state & (CONTROL | MOD1 | MOD4) != 0 {
        return None;
    }
    let c = char_of(sym)?;
    (!c.is_control()).then(|| c.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn us() -> KeyMap {
        // keycodes 38 = a/A, 10 = 1/!, 36 = Return.
        let mut syms = vec![0u32; 2 * 60];
        let mut set = |kc: usize, a: u32, b: u32| {
            syms[(kc - 8) * 2] = a;
            syms[(kc - 8) * 2 + 1] = b;
        };
        set(38, 0x61, 0x41);
        set(10, 0x31, 0x21);
        set(36, 0xff0d, 0xff0d);
        KeyMap::new(8, 2, syms)
    }

    #[test]
    fn shift_and_lock_pick_the_level() {
        let m = us();
        assert_eq!(m.keysym(38, 0), Some(0x61));
        assert_eq!(m.keysym(38, SHIFT), Some(0x41));
        assert_eq!(m.keysym(38, LOCK), Some(0x41));
        assert_eq!(m.keysym(38, LOCK | SHIFT), Some(0x61));
        // Lock does not shift digits.
        assert_eq!(m.keysym(10, LOCK), Some(0x31));
        assert_eq!(m.keysym(10, SHIFT), Some(0x21));
        assert_eq!(m.keysym(200, 0), None);
    }

    #[test]
    fn logical_keys_and_text() {
        assert_eq!(logical_key(0x61, 0x41), Some(Key::Char('a')));
        assert_eq!(logical_key(0xff0d, 0xff0d), Some(Key::Enter));
        assert_eq!(logical_key(0xffbe, 0xffbe), Some(Key::F(1)));
        assert_eq!(logical_key(0xfe20, 0xfe20), Some(Key::Tab));
        assert_eq!(logical_key(0xffe1, 0xffe1), None); // Shift_L
        assert_eq!(text_of(0x41, SHIFT).as_deref(), Some("A"));
        assert_eq!(text_of(0x61, CONTROL), None);
        assert_eq!(text_of(0xff0d, 0), None);
        assert_eq!(char_of(0x0100_20ac), Some('\u{20ac}'));
        assert_eq!(char_of(0x0100_d800), None);
    }

    #[test]
    fn modifier_bits() {
        let m = modifiers(SHIFT | CONTROL | MOD1 | MOD4);
        assert!(m.shift && m.ctrl && m.alt && m.meta);
        assert_eq!(modifiers(LOCK), Modifiers::NONE);
    }
}
