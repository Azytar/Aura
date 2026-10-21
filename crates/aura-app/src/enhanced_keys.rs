use x11rb::protocol::xproto::KeyButMask;
use xkbcommon::xkb;

pub struct KeyEvent<'a> {
    pub sym: u32,
    pub base_sym: u32,
    pub state: KeyButMask,
    pub kind: u8,
    pub legacy: Option<&'a [u8]>,
}

/// Kitty progressive enhancements, negotiated independently from legacy VT input.
pub fn encode(event: KeyEvent<'_>, flags: u8) -> Option<Vec<u8>> {
    let KeyEvent {
        sym,
        base_sym,
        state,
        kind,
        legacy,
    } = event;
    let all = flags & 8 != 0;
    let reports_events = flags & 2 != 0;
    if kind == 3 && !reports_events {
        return None;
    }
    let modifiers = 1
        + u16::from(state.contains(KeyButMask::SHIFT))
        + 2 * u16::from(state.contains(KeyButMask::MOD1))
        + 4 * u16::from(state.contains(KeyButMask::CONTROL))
        + 8 * u16::from(state.contains(KeyButMask::MOD4))
        + if all {
            64 * u16::from(state.contains(KeyButMask::LOCK))
                + 128 * u16::from(state.contains(KeyButMask::MOD2))
        } else {
            0
        };
    let modifier_key = match sym {
        0xffe1 => Some(57441),
        0xffe3 => Some(57442),
        0xffe9 => Some(57443),
        0xffeb => Some(57444),
        0xffe2 => Some(57447),
        0xffe4 => Some(57448),
        0xffea => Some(57449),
        0xffec => Some(57450),
        0xffe5 => Some(57358),
        0xff7f => Some(57360),
        0xff14 => Some(57359),
        _ => None,
    };
    if modifier_key.is_some() && !all {
        return None;
    }
    let reserved = matches!(sym, 0xff0d | 0xff09 | 0xfe20 | 0xff08);
    if kind == 3 && reserved && !all {
        return None;
    }
    if reserved && !all && modifiers == 1 {
        return if kind == 3 {
            None
        } else {
            legacy.map(<[u8]>::to_vec)
        };
    }
    let text = legacy
        .and_then(|bytes| std::str::from_utf8(bytes).ok())
        .filter(|text| !text.is_empty() && text.chars().all(|c| !c.is_control()));
    let scalar = |sym| {
        xkb::keysym_to_utf8(xkb::Keysym::new(sym))
            .chars()
            .next()
            .map(u32::from)
    };
    let navigation = match sym {
        0xff50 => ("1", 'H'),
        0xff51 => ("1", 'D'),
        0xff52 => ("1", 'A'),
        0xff53 => ("1", 'C'),
        0xff54 => ("1", 'B'),
        0xff57 => ("1", 'F'),
        0xff63 => ("2", '~'),
        0xffff => ("3", '~'),
        0xff55 => ("5", '~'),
        0xff56 => ("6", '~'),
        0xffbe => ("1", 'P'),
        0xffbf => ("1", 'Q'),
        0xffc0 => ("13", '~'),
        0xffc1 => ("1", 'S'),
        0xffc2 => ("15", '~'),
        0xffc3 => ("17", '~'),
        0xffc4 => ("18", '~'),
        0xffc5 => ("19", '~'),
        0xffc6 => ("20", '~'),
        0xffc7 => ("21", '~'),
        0xffc8 => ("23", '~'),
        0xffc9 => ("24", '~'),
        _ => ("", 'u'),
    };
    let functional = modifier_key.or_else(|| match sym {
        0xff1b => Some(27),
        0xff0d => Some(13),
        0xff09 | 0xfe20 => Some(9),
        0xff08 => Some(127),
        0xffca..=0xffe0 => Some(57376 + sym - 0xffca),
        0xffb0..=0xffb9 => Some(57399 + sym - 0xffb0),
        0xffae => Some(57409),
        0xffaf => Some(57410),
        0xffaa => Some(57411),
        0xffad => Some(57412),
        0xffab => Some(57413),
        0xff8d => Some(57414),
        0xffbd => Some(57415),
        0xff95 => Some(57423),
        0xff96 => Some(57417),
        0xff97 => Some(57419),
        0xff98 => Some(57418),
        0xff99 => Some(57420),
        0xff9a => Some(57421),
        0xff9b => Some(57422),
        0xff9c => Some(57424),
        0xff9e => Some(57425),
        0xff9f => Some(57426),
        0xff61 => Some(57361),
        0xff13 => Some(57362),
        0xff67 => Some(57363),
        _ => None,
    });
    if !all && modifiers <= 2 && functional.is_none() && navigation.0.is_empty() {
        return if kind == 3 {
            None
        } else {
            legacy.map(<[u8]>::to_vec)
        };
    }
    // Alternate-key reporting by itself does not change the legacy encoding.
    if flags & (1 | 2 | 8) == 0 {
        return if kind == 3 {
            None
        } else {
            legacy.map(<[u8]>::to_vec)
        };
    }
    let (mut payload, terminator) = if !navigation.0.is_empty() {
        (navigation.0.to_string(), navigation.1)
    } else {
        let code = functional
            .or_else(|| scalar(base_sym))
            .or_else(|| scalar(sym))?;
        let mut code = code.to_string();
        if functional.is_none()
            && flags & 4 != 0
            && state.contains(KeyButMask::SHIFT)
            && let Some(shifted) = scalar(sym)
            && Some(shifted) != scalar(base_sym)
        {
            code.push_str(&format!(":{shifted}"));
        }
        (code, 'u')
    };
    let associated = if all && flags & 16 != 0 && kind != 3 {
        text
    } else {
        None
    };
    if modifiers != 1 || (reports_events && kind != 1) || associated.is_some() {
        payload.push_str(&format!(";{modifiers}"));
    }
    if reports_events && kind != 1 {
        payload.push_str(&format!(":{kind}"));
    }
    if let Some(text) = associated {
        payload.push(';');
        payload.push_str(
            &text
                .chars()
                .map(|c| u32::from(c).to_string())
                .collect::<Vec<_>>()
                .join(":"),
        );
    }
    Some(format!("\x1b[{payload}{terminator}").into_bytes())
}

pub fn modifier_mask(sym: u32) -> Option<KeyButMask> {
    match sym {
        0xffe1 | 0xffe2 => Some(KeyButMask::SHIFT),
        0xffe3 | 0xffe4 => Some(KeyButMask::CONTROL),
        0xffe9 | 0xffea => Some(KeyButMask::MOD1),
        0xffeb | 0xffec => Some(KeyButMask::MOD4),
        _ => None,
    }
}

pub fn modify_other(event: &KeyEvent<'_>, mode: u8) -> Option<Vec<u8>> {
    let modifiers = 1
        + u16::from(event.state.contains(KeyButMask::SHIFT))
        + 2 * u16::from(event.state.contains(KeyButMask::MOD1))
        + 4 * u16::from(event.state.contains(KeyButMask::CONTROL));
    let code = match event.sym {
        0xff0d => 13,
        0xff09 => 9,
        0xff08 => 127,
        0xff1b => 27,
        _ => xkb::keysym_to_utf8(xkb::Keysym::new(event.sym))
            .chars()
            .next()? as u32,
    };
    if modifiers == 1 || (mode == 1 && !matches!(code, 9 | 13 | 27 | 127)) {
        return None;
    }
    Some(format!("\x1b[27;{modifiers};{code}~").into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shift_only_printable_text_keeps_its_layout_translation() {
        for (sym, base, text) in [(0x3a, 0x3b, b":".as_slice()), (0x42, 0x62, b"B".as_slice())] {
            let event = KeyEvent {
                sym,
                base_sym: base,
                state: KeyButMask::SHIFT,
                kind: 1,
                legacy: Some(text),
            };
            assert_eq!(encode(event, 3).as_deref(), Some(text));
        }
    }
    #[test]
    fn modified_enter_text_releases_and_alternate_keys_are_unambiguous() {
        let key = |sym, base_sym, state, kind, legacy| {
            encode(
                KeyEvent {
                    sym,
                    base_sym,
                    state,
                    kind,
                    legacy,
                },
                31,
            )
        };
        assert_eq!(
            key(0xff0d, 0xff0d, KeyButMask::SHIFT, 1, Some(b"\r")).unwrap(),
            b"\x1b[13;2u"
        );
        assert_eq!(
            key(0xff0d, 0xff0d, KeyButMask::CONTROL, 1, Some(b"\r")).unwrap(),
            b"\x1b[13;5u"
        );
        assert_eq!(
            key(0x42, 0x62, KeyButMask::SHIFT, 1, Some(b"B")).unwrap(),
            b"\x1b[98:66;2;66u"
        );
        assert_eq!(
            key(0x61, 0x61, KeyButMask::default(), 3, None).unwrap(),
            b"\x1b[97;1:3u"
        );
        assert_eq!(
            key(0x61, 0x61, KeyButMask::default(), 2, Some(b"a")).unwrap(),
            b"\x1b[97;1:2;97u"
        );
    }
}
