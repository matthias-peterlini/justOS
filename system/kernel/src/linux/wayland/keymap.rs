//! The keyboard as Wayland clients see it: an XKB keymap of our own and
//! the key codes in it.
//!
//! justOS turns key presses into characters itself, with the user's
//! layout. Clients get a US-shaped keymap (so that shortcuts such as Ctrl+C
//! sit where programs expect them), and every character that is not on it
//! (è, €, ß...) gets a key of its own: Latin-1 at the key code equal to the
//! character, others at spare codes, the keymap sent again when one is new.
//! The text is complete in itself (no `include`), so a client needs no XKB
//! data files.

use crate::dev::input::Key;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

/// The modifier bits of `wl_keyboard.modifiers`: XKB's real modifiers.
pub const SHIFT: u32 = 1;
pub const CTRL: u32 = 4;
pub const ALT: u32 = 8;

/// XKB key codes (evdev + 8) of the modifier keys.
pub const KEY_SHIFT: u32 = 50;
pub const KEY_CTRL: u32 = 37;
pub const KEY_ALT: u32 = 64;
const KEY_SUPER: u32 = 133;

/// The US layout: (key code, unshifted, shifted).
const US: &[(u32, char, char)] = &[
    (10, '1', '!'), (11, '2', '@'), (12, '3', '#'), (13, '4', '$'), (14, '5', '%'), (15, '6', '^'),
    (16, '7', '&'), (17, '8', '*'), (18, '9', '('), (19, '0', ')'), (20, '-', '_'), (21, '=', '+'),
    (24, 'q', 'Q'), (25, 'w', 'W'), (26, 'e', 'E'), (27, 'r', 'R'), (28, 't', 'T'), (29, 'y', 'Y'),
    (30, 'u', 'U'), (31, 'i', 'I'), (32, 'o', 'O'), (33, 'p', 'P'), (34, '[', '{'), (35, ']', '}'),
    (38, 'a', 'A'), (39, 's', 'S'), (40, 'd', 'D'), (41, 'f', 'F'), (42, 'g', 'G'), (43, 'h', 'H'),
    (44, 'j', 'J'), (45, 'k', 'K'), (46, 'l', 'L'), (47, ';', ':'), (48, '\'', '"'), (49, '`', '~'),
    (51, '\\', '|'), (52, 'z', 'Z'), (53, 'x', 'X'), (54, 'c', 'C'), (55, 'v', 'V'), (56, 'b', 'B'),
    (57, 'n', 'N'), (58, 'm', 'M'), (59, ',', '<'), (60, '.', '>'), (61, '/', '?'), (65, ' ', ' '),
];

/// Keys that are not characters: (key code, keysym name).
const SPECIAL: &[(u32, &str)] = &[
    (9, "Escape"), (22, "BackSpace"), (23, "Tab"), (36, "Return"), (KEY_CTRL, "Control_L"), (KEY_SHIFT, "Shift_L"),
    (KEY_ALT, "Alt_L"), (67, "F1"), (68, "F2"), (69, "F3"), (70, "F4"), (71, "F5"), (72, "F6"), (73, "F7"),
    (74, "F8"), (75, "F9"), (76, "F10"), (95, "F11"), (96, "F12"), (110, "Home"), (111, "Up"), (112, "Prior"),
    (113, "Left"), (114, "Right"), (115, "End"), (116, "Down"), (117, "Next"), (118, "Insert"), (119, "Delete"),
    (KEY_SUPER, "Super_L"),
];

/// Codes for characters beyond Latin-1, given out as they are typed.
const SPARE: core::ops::Range<u32> = 120..133;

/// The keymap one client has: the characters given spare codes so far.
#[derive(Default)]
pub struct Keymap {
    extra: Vec<char>,
}

/// Where a key is: its code and whether Shift makes it.
pub struct Press {
    pub code: u32,
    pub shift: bool,
}

impl Keymap {
    /// The key for `k`, and whether the keymap had to grow for it (then it
    /// must be sent again first). None for keys clients never get.
    pub fn press(&mut self, k: Key) -> Option<(Press, bool)> {
        let plain = |code| Some((Press { code, shift: false }, false));
        let special = |name: &str| SPECIAL.iter().find(|(_, n)| *n == name).map(|&(c, _)| c);
        match k {
            Key::Char(c) => self.char(c),
            Key::Enter => plain(36),
            Key::Backspace => plain(22),
            Key::Tab => plain(23),
            Key::Escape => plain(9),
            Key::Delete => plain(119),
            Key::Insert => plain(118),
            Key::Home => plain(110),
            Key::End => plain(115),
            Key::PageUp => plain(112),
            Key::PageDown => plain(117),
            Key::Up => plain(111),
            Key::Down => plain(116),
            Key::Left => plain(113),
            Key::Right => plain(114),
            Key::F(n @ 1..=12) => plain(special(&alloc::format!("F{n}"))?),
            _ => None,
        }
    }

    fn char(&mut self, c: char) -> Option<(Press, bool)> {
        if let Some(&(code, lo, _)) = US.iter().find(|(_, lo, hi)| *lo == c || *hi == c) {
            return Some((Press { code, shift: c != lo }, false));
        }
        let u = c as u32;
        if (0xa0..=0xff).contains(&u) {
            return Some((Press { code: u, shift: false }, false));
        }
        if u < 0x20 || u == 0x7f {
            return None;
        }
        if let Some(i) = self.extra.iter().position(|&e| e == c) {
            return Some((Press { code: SPARE.start + i as u32, shift: false }, false));
        }
        // A new one: the oldest spare code goes to it when all are taken.
        if self.extra.len() == SPARE.len() {
            self.extra.remove(0);
        }
        self.extra.push(c);
        let i = self.extra.len() - 1;
        Some((Press { code: SPARE.start + i as u32, shift: false }, true))
    }

    /// The keymap in XKB's text format, ending with a zero byte.
    pub fn text(&self) -> Vec<u8> {
        let mut codes: Vec<u32> = US.iter().map(|k| k.0).chain(SPECIAL.iter().map(|k| k.0)).chain(0xa0..=0xff).collect();
        codes.extend((0..self.extra.len() as u32).map(|i| SPARE.start + i));
        codes.sort_unstable();
        let mut t = String::new();
        t.push_str("xkb_keymap {\nxkb_keycodes \"justos\" {\n    minimum = 8;\n    maximum = 255;\n");
        for c in &codes {
            let _ = writeln!(t, "    <K{c}> = {c};");
        }
        t.push_str("};\n");
        t.push_str(concat!(
            "xkb_types \"justos\" {\n",
            "    type \"ONE_LEVEL\" { modifiers = none; level_name[Level1] = \"Any\"; };\n",
            "    type \"TWO_LEVEL\" { modifiers = Shift; map[Shift] = Level2; level_name[Level1] = \"Base\"; level_name[Level2] = \"Shift\"; };\n",
            "    type \"ALPHABETIC\" { modifiers = Shift + Lock; map[Shift] = Level2; map[Lock] = Level2; level_name[Level1] = \"Base\"; level_name[Level2] = \"Caps\"; };\n",
            "};\n",
            "xkb_compatibility \"justos\" {\n",
            "    interpret Shift_L { action = SetMods(modifiers = Shift); };\n",
            "    interpret Control_L { action = SetMods(modifiers = Control); };\n",
            "    interpret Alt_L { action = SetMods(modifiers = Mod1); };\n",
            "    interpret Super_L { action = SetMods(modifiers = Mod4); };\n",
            "};\n",
            "xkb_symbols \"justos\" {\n",
            "    name[Group1] = \"justOS\";\n",
        ));
        let sym = |c: char| alloc::format!("U{:04X}", c as u32);
        for &(code, lo, hi) in US {
            if lo == hi {
                let _ = writeln!(t, "    key <K{code}> {{ type = \"ONE_LEVEL\", [ {} ] }};", if lo == ' ' { String::from("space") } else { sym(lo) });
            } else {
                let ty = if lo.is_ascii_lowercase() { "ALPHABETIC" } else { "TWO_LEVEL" };
                let _ = writeln!(t, "    key <K{code}> {{ type = \"{ty}\", [ {}, {} ] }};", sym(lo), sym(hi));
            }
        }
        for &(code, name) in SPECIAL {
            let _ = writeln!(t, "    key <K{code}> {{ type = \"ONE_LEVEL\", [ {name} ] }};");
        }
        for u in 0xa0..=0xffu32 {
            let c = char::from_u32(u).unwrap_or(' ');
            let _ = writeln!(t, "    key <K{u}> {{ type = \"ONE_LEVEL\", [ {} ] }};", sym(c));
        }
        for (i, &c) in self.extra.iter().enumerate() {
            let _ = writeln!(t, "    key <K{}> {{ type = \"ONE_LEVEL\", [ {} ] }};", SPARE.start + i as u32, sym(c));
        }
        let _ = writeln!(t, "    modifier_map Shift {{ <K{KEY_SHIFT}> }};");
        let _ = writeln!(t, "    modifier_map Control {{ <K{KEY_CTRL}> }};");
        let _ = writeln!(t, "    modifier_map Mod1 {{ <K{KEY_ALT}> }};");
        let _ = writeln!(t, "    modifier_map Mod4 {{ <K{KEY_SUPER}> }};");
        t.push_str("};\n};\n");
        let mut v = t.into_bytes();
        v.push(0);
        v
    }
}
