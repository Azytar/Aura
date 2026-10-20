use std::io;

use x11rb::protocol::xkb::{self, EventType, MapPart, SelectEventsAux};
use x11rb::protocol::xproto::{KeyButMask, KeyPressEvent};
use x11rb::xcb_ffi::XCBConnection;
use xkbcommon::xkb::{self as keymap, x11};

pub struct Keyboard {
    #[cfg(debug_assertions)]
    debug: bool,
    context: keymap::Context,
    state: keymap::State,
    compose: Option<keymap::compose::State>,
}

impl Keyboard {
    pub fn new(
        connection: &XCBConnection,
        _debug: bool,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let (mut major, mut minor, mut event, mut error) = (0, 0, 0, 0);
        if !x11::setup_xkb_extension(
            connection,
            x11::MIN_MAJOR_XKB_VERSION,
            x11::MIN_MINOR_XKB_VERSION,
            x11::SetupXkbExtensionFlags::NoFlags,
            &mut major,
            &mut minor,
            &mut event,
            &mut error,
        ) {
            return Err(io::Error::other("X11 server does not support XKB").into());
        }
        let context = keymap::Context::new(keymap::CONTEXT_NO_FLAGS);
        if context.get_raw_ptr().is_null() {
            return Err(io::Error::other("cannot create XKB context").into());
        }
        let state = device_state(&context, connection)?;
        #[cfg(debug_assertions)]
        if _debug {
            eprintln!(
                "DEBUG keyboard device={} kc24=0x{:x} kc31=0x{:x} kc33=0x{:x} kc54=0x{:x}",
                x11::get_core_keyboard_device_id(connection),
                state.key_get_one_sym(keymap::Keycode::new(24)).raw(),
                state.key_get_one_sym(keymap::Keycode::new(31)).raw(),
                state.key_get_one_sym(keymap::Keycode::new(33)).raw(),
                state.key_get_one_sym(keymap::Keycode::new(54)).raw(),
            );
        }
        // Types and virtual modifiers are part of a layout, not just its symbols.
        xkb::select_events(
            connection,
            x11::get_core_keyboard_device_id(connection) as u16,
            EventType::default(),
            EventType::NEW_KEYBOARD_NOTIFY | EventType::MAP_NOTIFY,
            MapPart::from(0xffu16),
            MapPart::from(0xffu16),
            &SelectEventsAux::default(),
        )?
        .check()?;
        let locale = ["LC_ALL", "LC_CTYPE", "LANG"]
            .into_iter()
            .filter_map(std::env::var_os)
            .find(|value| !value.is_empty())
            .unwrap_or_else(|| "C.UTF-8".into());
        let table = keymap::compose::Table::new_from_locale(
            &context,
            &locale,
            keymap::compose::COMPILE_NO_FLAGS,
        )
        .or_else(|_| {
            keymap::compose::Table::new_from_locale(
                &context,
                std::ffi::OsStr::new("C.UTF-8"),
                keymap::compose::COMPILE_NO_FLAGS,
            )
        });
        let compose = table
            .ok()
            .map(|table| keymap::compose::State::new(&table, keymap::compose::STATE_NO_FLAGS))
            .filter(|state| !state.get_raw_ptr().is_null());
        Ok(Self {
            #[cfg(debug_assertions)]
            debug: _debug,
            context,
            state,
            compose,
        })
    }

    pub fn reload(&mut self, connection: &XCBConnection) -> Result<(), io::Error> {
        self.state = device_state(&self.context, connection)?;
        #[cfg(debug_assertions)]
        if self.debug {
            eprintln!(
                "DEBUG reload: kc24=0x{:x} kc31=0x{:x} kc54=0x{:x}",
                self.state.key_get_one_sym(keymap::Keycode::new(24)).raw(),
                self.state.key_get_one_sym(keymap::Keycode::new(31)).raw(),
                self.state.key_get_one_sym(keymap::Keycode::new(54)).raw(),
            );
        }
        if let Some(compose) = &mut self.compose {
            compose.reset();
        }
        Ok(())
    }

    pub fn key(&mut self, event: &KeyPressEvent) -> (u32, KeyButMask) {
        // Core X events carry the effective real modifiers in bits 0..7 and
        // the effective XKB group in bits 13..14. Use the event snapshot, not
        // today's server state: another key may already have changed a lock.
        let mask = u16::from(event.state);
        self.state.update_mask(
            u32::from(mask & 0xff),
            0,
            0,
            0,
            0,
            u32::from((mask >> 13) & 3),
        );
        let keycode = keymap::Keycode::new(u32::from(event.detail));
        let sym = self.state.key_get_one_sym(keycode).raw();
        #[cfg(debug_assertions)]
        if self.debug {
            eprintln!(
                "DEBUG key event: keycode={} sym=0x{:x} state=0x{:x}",
                event.detail, sym, mask
            );
        }
        let consumed = self.state.key_get_consumed_mods(keycode) as u16;
        // AltGr can consume Control/Alt on some layouts; those modifiers must
        // not turn its printable character into an application shortcut.
        let shortcuts = u16::from(KeyButMask::CONTROL | KeyButMask::MOD1);
        (sym, KeyButMask::from(mask & !(consumed & shortcuts)))
    }

    pub fn bytes(
        &mut self,
        sym: u32,
        modifiers: KeyButMask,
        modes: aura_core::Modes,
    ) -> Option<Vec<u8>> {
        if let Some(compose) = &mut self.compose {
            if modifiers.intersects(KeyButMask::CONTROL | KeyButMask::MOD1) {
                compose.reset();
            } else {
                compose.feed(keymap::Keysym::new(sym));
                match compose.status() {
                    keymap::compose::Status::Composing => return None,
                    keymap::compose::Status::Composed => {
                        let bytes = composed_bytes(compose);
                        compose.reset();
                        return bytes;
                    }
                    keymap::compose::Status::Cancelled => {
                        compose.reset();
                        // A cancelling dead key can start a new sequence. Feed
                        // it once after reset; ordinary keys still fall through.
                        compose.feed(keymap::Keysym::new(sym));
                        match compose.status() {
                            keymap::compose::Status::Composing => return None,
                            keymap::compose::Status::Composed => {
                                let bytes = composed_bytes(compose);
                                compose.reset();
                                return bytes;
                            }
                            _ => {}
                        }
                        // The key that aborts a dead-key sequence is
                        // ordinary input (dead-acute followed by `c` has no
                        // composed form). Dropping it loses a keystroke,
                        // which is especially costly during password entry,
                        // so fall through and encode it like any other key.
                    }
                    keymap::compose::Status::Nothing => {}
                }
            }
        }
        translate_key(
            sym,
            modifiers,
            modes.application_cursor,
            modes.application_keypad,
        )
    }
}

fn composed_bytes(state: &keymap::compose::State) -> Option<Vec<u8>> {
    let mut bytes = [0u8; 257];
    // SAFETY: state owns a valid compose handle; bytes is writable and its
    // length includes NUL space. Custom Compose strings can exceed the buffer:
    // validate the returned required length before indexing or decoding it.
    let len = unsafe {
        keymap::ffi::compose::xkb_compose_state_get_utf8(
            state.get_raw_ptr(),
            bytes.as_mut_ptr().cast(),
            bytes.len(),
        )
    };
    if len <= 0 || len as usize >= bytes.len() {
        return None;
    }
    let text = std::str::from_utf8(&bytes[..len as usize]).ok()?;
    Some(text.as_bytes().to_vec())
}

fn device_state(
    context: &keymap::Context,
    connection: &XCBConnection,
) -> Result<keymap::State, io::Error> {
    let device = x11::get_core_keyboard_device_id(connection);
    if device < 0 {
        return Err(io::Error::other("cannot find the XKB core keyboard"));
    }
    let map =
        x11::keymap_new_from_device(context, connection, device, keymap::KEYMAP_COMPILE_NO_FLAGS);
    if map.get_raw_ptr().is_null() {
        return Err(io::Error::other("cannot read the XKB keyboard map"));
    }
    let state = keymap::State::new(&map);
    if state.get_raw_ptr().is_null() {
        return Err(io::Error::other("cannot create XKB keyboard state"));
    }
    Ok(state)
}

pub fn translate_key(
    sym: u32,
    state: KeyButMask,
    application_cursor: bool,
    application_keypad: bool,
) -> Option<Vec<u8>> {
    let shift = state.contains(KeyButMask::SHIFT);
    let ctrl = state.contains(KeyButMask::CONTROL);
    let alt = state.contains(KeyButMask::MOD1);
    let modifier = 1 + u8::from(shift) + 2 * u8::from(alt) + 4 * u8::from(ctrl);
    if application_keypad {
        let final_byte = match sym {
            0xffb0..=0xffb9 => Some(b'p' + (sym - 0xffb0) as u8),
            0xff8d => Some(b'M'),
            0xff95 => Some(b'w'),
            0xff96 => Some(b't'),
            0xff97 => Some(b'x'),
            0xff98 => Some(b'v'),
            0xff99 => Some(b'r'),
            0xff9a => Some(b'y'),
            0xff9b => Some(b's'),
            0xff9c => Some(b'q'),
            0xff9d => Some(b'u'),
            0xff9e => Some(b'p'),
            0xff9f => Some(b'n'),
            0xffaa..=0xffaf => Some(b'j' + (sym - 0xffaa) as u8),
            0xffbd => Some(b'X'),
            _ => None,
        };
        if let Some(byte) = final_byte {
            return Some(if modifier == 1 {
                vec![0x1b, b'O', byte]
            } else {
                format!("\x1bO{modifier}{}", byte as char).into_bytes()
            });
        }
    }
    let final_byte = match sym {
        0xff50 | 0xff95 => Some(b'H'),
        0xff51 | 0xff96 => Some(b'D'),
        0xff52 | 0xff97 => Some(b'A'),
        0xff53 | 0xff98 => Some(b'C'),
        0xff54 | 0xff99 => Some(b'B'),
        0xff57 | 0xff9c => Some(b'F'),
        0xffbe..=0xffc1 => Some(b'P' + (sym - 0xffbe) as u8),
        _ => None,
    };
    if let Some(final_byte) = final_byte {
        return Some(if modifier != 1 {
            format!("\x1b[1;{modifier}{}", final_byte as char).into_bytes()
        } else if application_cursor || (0xffbe..=0xffc1).contains(&sym) {
            vec![0x1b, b'O', final_byte]
        } else {
            vec![0x1b, b'[', final_byte]
        });
    }
    let number = match sym {
        0xff63 | 0xff9e => Some(2),
        0xffff | 0xff9f => Some(3),
        0xff55 | 0xff9a => Some(5),
        0xff56 | 0xff9b => Some(6),
        0xffc2..=0xffc9 => Some([15, 17, 18, 19, 20, 21, 23, 24][(sym - 0xffc2) as usize]),
        _ => None,
    };
    if let Some(number) = number {
        return Some(if modifier == 1 {
            format!("\x1b[{number}~").into_bytes()
        } else {
            format!("\x1b[{number};{modifier}~").into_bytes()
        });
    }
    let mut bytes = match sym {
        0xff08 => vec![if ctrl { 0x08 } else { 0x7f }],
        0xff09 | 0xfe20 if shift || sym == 0xfe20 => b"\x1b[Z".to_vec(),
        0xff09 => vec![b'\t'],
        0xff0d | 0xff8d => vec![b'\r'],
        0xff1b => vec![0x1b],
        _ => {
            let text = keymap::keysym_to_utf8(keymap::Keysym::new(sym));
            if text.is_empty() {
                return None;
            }
            text.into_bytes()
        }
    };
    if ctrl && bytes.len() == 1 {
        let byte = bytes[0].to_ascii_uppercase();
        bytes[0] = match byte {
            b'@'..=b'_' => byte & 0x1f,
            b' ' | b'2' => 0,
            b'3'..=b'7' => byte - b'3' + 0x1b,
            b'8' | b'?' => 0x7f,
            _ => bytes[0],
        };
    }
    if alt {
        bytes.insert(0, 0x1b);
    }
    Some(bytes)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontSizeAction {
    Grow,
    Shrink,
    Reset,
}

pub fn font_size_action(sym: u32, state: KeyButMask) -> Option<FontSizeAction> {
    if !state.contains(KeyButMask::CONTROL) || state.contains(KeyButMask::MOD1) {
        return None;
    }
    match sym {
        0x2b | 0x3d | 0xffab | 0xffbd => Some(FontSizeAction::Grow),
        0x2d | 0x5f | 0xffad => Some(FontSizeAction::Shrink),
        0x30 | 0xffb0 => Some(FontSizeAction::Reset),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup(layout: &str, name: &str, modifiers: u32, group: u32) -> u32 {
        let context = keymap::Context::new(keymap::CONTEXT_NO_FLAGS);
        let map = keymap::Keymap::new_from_names(
            &context,
            "evdev",
            "pc105",
            layout,
            "",
            None,
            keymap::KEYMAP_COMPILE_NO_FLAGS,
        )
        .unwrap();
        let mut state = keymap::State::new(&map);
        state.update_mask(modifiers, 0, 0, 0, 0, group);
        state.key_get_one_sym(map.key_by_name(name).unwrap()).raw()
    }

    #[test]
    fn repeated_keypress_snapshots_do_not_accumulate_modifiers_or_groups() {
        let context = keymap::Context::new(keymap::CONTEXT_NO_FLAGS);
        let map = keymap::Keymap::new_from_names(
            &context,
            "evdev",
            "pc105",
            "us,de",
            "",
            None,
            keymap::KEYMAP_COMPILE_NO_FLAGS,
        )
        .unwrap();
        let mut keyboard = Keyboard {
            #[cfg(debug_assertions)]
            debug: false,
            state: keymap::State::new(&map),
            compose: None,
            context,
        };
        for (name, mask, expected) in [
            ("AC01", 1u16, b"A".as_slice()),
            ("AC01", 4, b"\x01"),
            ("AC01", 8, b"\x1ba"),
            ("AD01", (1 << 13) | (1 << 7), b"@"),
            ("AD06", 1 << 13, b"z"),
            ("AC01", 0, b"a"),
        ] {
            let event = KeyPressEvent {
                detail: map.key_by_name(name).unwrap().raw() as u8,
                state: KeyButMask::from(mask),
                ..Default::default()
            };
            for _ in 0..8 {
                let (sym, modifiers) = keyboard.key(&event);
                assert_eq!(
                    keyboard
                        .bytes(sym, modifiers, aura_core::Modes::default())
                        .as_deref(),
                    Some(expected)
                );
            }
        }
    }

    #[test]
    fn a_dead_key_cancelling_compose_can_start_a_new_sequence() {
        let context = keymap::Context::new(keymap::CONTEXT_NO_FLAGS);
        let map = keymap::Keymap::new_from_names(
            &context,
            "",
            "",
            "us",
            "",
            None,
            keymap::KEYMAP_COMPILE_NO_FLAGS,
        )
        .unwrap();
        let table = keymap::compose::Table::new_from_buffer(
            &context,
            "<dead_acute> <e> : \"é\"\n<dead_grave> <a> : \"à\"\n",
            "C.UTF-8",
            keymap::compose::FORMAT_TEXT_V1,
            keymap::compose::COMPILE_NO_FLAGS,
        )
        .unwrap();
        let mut keyboard = Keyboard {
            #[cfg(debug_assertions)]
            debug: false,
            state: keymap::State::new(&map),
            compose: Some(keymap::compose::State::new(
                &table,
                keymap::compose::STATE_NO_FLAGS,
            )),
            context,
        };
        let modifiers = KeyButMask::default();
        let modes = aura_core::Modes::default();
        assert_eq!(keyboard.bytes(0xfe51, modifiers, modes), None);
        assert_eq!(keyboard.bytes(0xfe50, modifiers, modes), None);
        assert_eq!(
            keyboard.bytes('a' as u32, modifiers, modes),
            Some("à".as_bytes().to_vec())
        );
    }

    #[test]
    fn compose_and_dead_keys_emit_only_complete_utf8() {
        let context = keymap::Context::new(keymap::CONTEXT_NO_FLAGS);
        let map = keymap::Keymap::new_from_names(
            &context,
            "",
            "",
            "us",
            "",
            None,
            keymap::KEYMAP_COMPILE_NO_FLAGS,
        )
        .unwrap();
        let table = keymap::compose::Table::new_from_buffer(
            &context,
            "<dead_acute> <e> : \"é\"\n<Multi_key> <o> <e> : \"œ\"\n",
            "C.UTF-8",
            keymap::compose::FORMAT_TEXT_V1,
            keymap::compose::COMPILE_NO_FLAGS,
        )
        .unwrap();
        let mut keyboard = Keyboard {
            #[cfg(debug_assertions)]
            debug: false,
            state: keymap::State::new(&map),
            compose: Some(keymap::compose::State::new(
                &table,
                keymap::compose::STATE_NO_FLAGS,
            )),
            context,
        };
        let modifiers = KeyButMask::default();
        let modes = aura_core::Modes::default();
        assert_eq!(keyboard.bytes(0xfe51, modifiers, modes), None);
        assert_eq!(
            keyboard.bytes('e' as u32, modifiers, modes),
            Some("é".as_bytes().to_vec())
        );
        for sym in [0xff20, 'o' as u32] {
            assert_eq!(keyboard.bytes(sym, modifiers, modes), None);
        }
        assert_eq!(
            keyboard.bytes('e' as u32, modifiers, modes),
            Some("œ".as_bytes().to_vec())
        );
        assert_eq!(keyboard.bytes(0xfe51, modifiers, modes), None);
        assert_eq!(
            keyboard.bytes('c' as u32, KeyButMask::CONTROL, modes),
            Some(vec![3])
        );
        assert_eq!(
            keyboard.bytes('e' as u32, modifiers, modes),
            Some(vec![b'e'])
        );
    }

    #[test]
    fn cancelled_compose_still_emits_the_cancelling_key() {
        // Regression test: the key that aborts a dead-key sequence has no
        // composed form, but it is still ordinary input. Dropping it lost a
        // keystroke on every aborted sequence, including password entry.
        let context = keymap::Context::new(keymap::CONTEXT_NO_FLAGS);
        let map = keymap::Keymap::new_from_names(
            &context,
            "",
            "",
            "us",
            "",
            None,
            keymap::KEYMAP_COMPILE_NO_FLAGS,
        )
        .unwrap();
        let table = keymap::compose::Table::new_from_buffer(
            &context,
            "<dead_acute> <e> : \"é\"\n",
            "C.UTF-8",
            keymap::compose::FORMAT_TEXT_V1,
            keymap::compose::COMPILE_NO_FLAGS,
        )
        .unwrap();
        let mut keyboard = Keyboard {
            #[cfg(debug_assertions)]
            debug: false,
            state: keymap::State::new(&map),
            compose: Some(keymap::compose::State::new(
                &table,
                keymap::compose::STATE_NO_FLAGS,
            )),
            context,
        };
        let modifiers = KeyButMask::default();
        let modes = aura_core::Modes::default();
        assert_eq!(keyboard.bytes(0xfe51, modifiers, modes), None);
        assert_eq!(
            keyboard.bytes('c' as u32, modifiers, modes),
            Some(vec![b'c'])
        );
        assert_eq!(
            keyboard.bytes('e' as u32, modifiers, modes),
            Some(vec![b'e'])
        );
    }

    #[test]
    fn application_keypad_retains_physical_key_identity() {
        for (sym, suffix) in [
            (0xffb1, b'q'),
            (0xff9c, b'q'),
            (0xff8d, b'M'),
            (0xffaa, b'j'),
            (0xff9f, b'n'),
        ] {
            assert_eq!(
                translate_key(sym, KeyButMask::default(), false, true).unwrap(),
                [0x1b, b'O', suffix]
            );
            assert_eq!(
                translate_key(sym, KeyButMask::CONTROL, false, true).unwrap(),
                [0x1b, b'O', b'5', suffix]
            );
        }
    }

    #[test]
    fn caps_lock_shift_and_layout_levels_follow_xkb_types() {
        for (mask, want) in [(0, 'a'), (1, 'A'), (2, 'A'), (3, 'a')] {
            assert_eq!(lookup("us", "AC01", mask, 0), want as u32);
        }
        assert_eq!(lookup("us", "AE01", 2, 0), '1' as u32);
        assert_eq!(lookup("us", "AE01", 3, 0), '!' as u32);
        assert_eq!(lookup("us,de", "AD06", 0, 1), 'z' as u32);
        assert_eq!(lookup("de", "AD01", 1 << 7, 0), '@' as u32);
        assert_eq!(lookup("us", "KP1", 1 << 4, 0), 0xffb1);
    }

    #[test]
    fn navigation_modifiers_backtab_and_control_bytes_match_xterm() {
        let key = |sym, mods, app| translate_key(sym, mods, app, false).unwrap();
        assert_eq!(key(0xff51, KeyButMask::default(), true), b"\x1bOD");
        assert_eq!(key(0xff51, KeyButMask::CONTROL, true), b"\x1b[1;5D");
        assert_eq!(
            key(0xff52, KeyButMask::MOD1 | KeyButMask::SHIFT, false),
            b"\x1b[1;4A"
        );
        assert_eq!(key(0xff09, KeyButMask::SHIFT, false), b"\x1b[Z");
        assert_eq!(key(0xffc9, KeyButMask::CONTROL, false), b"\x1b[24;5~");
        // The encodings the alternate-screen fall-through relies on: when a
        // fullscreen application runs, Shift+PgUp/PgDn and Ctrl+Shift+Home/End
        // must reach it as modified navigation keys instead of scrolling the
        // local view.
        assert_eq!(key(0xff55, KeyButMask::SHIFT, false), b"\x1b[5;2~");
        assert_eq!(key(0xff56, KeyButMask::SHIFT, false), b"\x1b[6;2~");
        assert_eq!(
            key(0xff50, KeyButMask::CONTROL | KeyButMask::SHIFT, false),
            b"\x1b[1;6H"
        );
        assert_eq!(
            key(0xff57, KeyButMask::CONTROL | KeyButMask::SHIFT, false),
            b"\x1b[1;6F"
        );
        for (sym, byte) in [
            ('a', 1),
            ('A', 1),
            (' ', 0),
            ('2', 0),
            ('3', 0x1b),
            ('7', 0x1f),
            ('8', 0x7f),
            ('?', 0x7f),
        ] {
            assert_eq!(key(sym as u32, KeyButMask::CONTROL, false), [byte]);
        }
        assert_eq!(
            key('é' as u32, KeyButMask::default(), false),
            "é".as_bytes()
        );
        assert_eq!(key(0xffb1, KeyButMask::MOD2, false), b"1");
        assert_eq!(key('x' as u32, KeyButMask::MOD1, false), b"\x1bx");
    }

    #[test]
    fn zoom_uses_only_the_selected_level() {
        assert_eq!(
            font_size_action('+' as u32, KeyButMask::CONTROL),
            Some(FontSizeAction::Grow)
        );
        assert_eq!(
            font_size_action('0' as u32, KeyButMask::CONTROL),
            Some(FontSizeAction::Reset)
        );
        assert_eq!(
            font_size_action(')' as u32, KeyButMask::CONTROL | KeyButMask::SHIFT),
            None
        );
        assert_eq!(font_size_action(0xff9e, KeyButMask::CONTROL), None);
        assert_eq!(
            font_size_action('+' as u32, KeyButMask::CONTROL | KeyButMask::MOD1),
            None
        );
    }
}
