const CSI_MAX_BYTES: usize = 128;
const CSI_MAX_PARAMS: usize = 32;
const STRING_MAX_BYTES: usize = 4096;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Csi {
    pub private: Option<u8>,
    pub intermediates: [u8; 2],
    pub intermediate_len: u8,
    pub params: [u16; CSI_MAX_PARAMS],
    pub present: [bool; CSI_MAX_PARAMS],
    pub param_count: u8,
    pub final_byte: u8,
}

impl Csi {
    pub fn param(&self, index: usize, default: u16) -> u16 {
        if index >= self.params.len() || index >= self.param_count as usize || !self.present[index]
        {
            default
        } else {
            self.params[index]
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Print(char),
    Control(u8),
    Escape {
        intermediates: [u8; 2],
        intermediate_len: u8,
        final_byte: u8,
    },
    Csi(Csi),
    Osc(Vec<u8>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Ground,
    Escape,
    EscapeIntermediate,
    CsiEntry,
    CsiParam,
    CsiIntermediate,
    CsiIgnore,
    Osc,
    OscEscape,
    Dcs,
    DcsEscape,
}

/// Incremental VT syntax parser. It owns no terminal or IO state.
#[derive(Clone, Debug)]
pub struct Parser {
    state: State,
    escape_intermediates: [u8; 2],
    escape_intermediate_len: u8,
    csi: Csi,
    csi_bytes: usize,
    string: Vec<u8>,
    string_overflow: bool,
    string_utf8_remaining: u8,
    utf8: [u8; 4],
    utf8_len: u8,
    utf8_expected: u8,
    replay_byte: Option<u8>,
}

impl Default for Parser {
    fn default() -> Self {
        Self {
            state: State::Ground,
            escape_intermediates: [0; 2],
            escape_intermediate_len: 0,
            csi: Csi::default(),
            csi_bytes: 0,
            string: Vec::with_capacity(128),
            string_overflow: false,
            string_utf8_remaining: 0,
            utf8: [0; 4],
            utf8_len: 0,
            utf8_expected: 0,
            replay_byte: None,
        }
    }
}

impl Parser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Consumes a byte stream, retaining incomplete UTF-8 and VT sequences between calls.
    pub fn feed(&mut self, bytes: &[u8], mut emit: impl FnMut(Event)) {
        let mut offset = 0;
        while offset < bytes.len() {
            if self.state == State::Ground
                && self.utf8_len == 0
                && (0x20..=0x7e).contains(&bytes[offset])
            {
                let start = offset;
                while offset < bytes.len() && (0x20..=0x7e).contains(&bytes[offset]) {
                    offset += 1;
                }
                for &byte in &bytes[start..offset] {
                    emit(Event::Print(byte as char));
                }
                continue;
            }
            let input = bytes[offset];
            offset += 1;
            let mut byte = input;
            loop {
                if let Some(event) = self.advance_byte(byte) {
                    emit(event);
                }
                if let Some(replay) = self.replay_byte.take() {
                    byte = replay;
                } else {
                    break;
                }
            }
        }
    }

    fn advance_byte(&mut self, byte: u8) -> Option<Event> {
        if self.state == State::Ground && self.utf8_len != 0 {
            return self.advance_utf8(byte);
        }

        if byte == 0x18 || byte == 0x1a {
            self.reset_sequence();
            return None;
        }

        match self.state {
            State::Ground => self.advance_ground(byte),
            State::Escape => self.advance_escape(byte),
            State::EscapeIntermediate => self.advance_escape_intermediate(byte),
            State::CsiEntry | State::CsiParam | State::CsiIntermediate | State::CsiIgnore => {
                self.advance_csi(byte)
            }
            State::Osc => self.advance_osc(byte),
            State::OscEscape => self.advance_osc_escape(byte),
            State::Dcs => self.advance_dcs(byte),
            State::DcsEscape => self.advance_dcs_escape(byte),
        }
    }

    pub fn reset(&mut self) {
        self.reset_sequence();
        self.utf8_len = 0;
        self.utf8_expected = 0;
        self.replay_byte = None;
    }

    fn advance_ground(&mut self, byte: u8) -> Option<Event> {
        match byte {
            0x1b => {
                self.begin_escape();
                None
            }
            0x00..=0x17 | 0x19 | 0x1c..=0x1f | 0x7f => Some(Event::Control(byte)),
            0x18 | 0x1a => None,
            0x20..=0x7e => Some(Event::Print(byte as char)),
            0x80..=0x9f => self.begin_c1(byte),
            0xc2..=0xdf => {
                self.begin_utf8(byte, 2);
                None
            }
            0xe0..=0xef => {
                self.begin_utf8(byte, 3);
                None
            }
            0xf0..=0xf4 => {
                self.begin_utf8(byte, 4);
                None
            }
            _ => Some(Event::Print('\u{fffd}')),
        }
    }

    fn begin_c1(&mut self, byte: u8) -> Option<Event> {
        match byte {
            0x90 | 0x98 | 0x9e | 0x9f => {
                self.begin_string(State::Dcs);
                None
            }
            0x9b => {
                self.begin_csi();
                None
            }
            0x9d => {
                self.begin_string(State::Osc);
                None
            }
            0x9c => None,
            _ => Some(Event::Control(byte)),
        }
    }

    fn begin_utf8(&mut self, byte: u8, expected: u8) {
        self.utf8 = [0; 4];
        self.utf8[0] = byte;
        self.utf8_len = 1;
        self.utf8_expected = expected;
    }

    fn advance_utf8(&mut self, byte: u8) -> Option<Event> {
        if !(0x80..=0xbf).contains(&byte) {
            self.utf8_len = 0;
            self.utf8_expected = 0;
            self.replay_byte = Some(byte);
            return Some(Event::Print('\u{fffd}'));
        }
        self.utf8[self.utf8_len as usize] = byte;
        self.utf8_len += 1;
        if self.utf8_len != self.utf8_expected {
            return None;
        }
        let length = self.utf8_len as usize;
        let decoded = std::str::from_utf8(&self.utf8[..length])
            .ok()
            .and_then(|value| value.chars().next());
        self.utf8_len = 0;
        self.utf8_expected = 0;
        Some(Event::Print(decoded.unwrap_or('\u{fffd}')))
    }

    fn begin_escape(&mut self) {
        self.state = State::Escape;
        self.escape_intermediates = [0; 2];
        self.escape_intermediate_len = 0;
    }

    fn advance_escape(&mut self, byte: u8) -> Option<Event> {
        match byte {
            0x00..=0x17 | 0x19 | 0x1c..=0x1f | 0x7f => Some(Event::Control(byte)),
            0x1b => {
                self.begin_escape();
                None
            }
            b'[' => {
                self.begin_csi();
                None
            }
            b']' => {
                self.begin_string(State::Osc);
                None
            }
            b'P' | b'X' | b'^' | b'_' => {
                self.begin_string(State::Dcs);
                None
            }
            0x20..=0x2f => {
                self.escape_intermediates[0] = byte;
                self.escape_intermediate_len = 1;
                self.state = State::EscapeIntermediate;
                None
            }
            0x30..=0x7e => {
                self.state = State::Ground;
                Some(Event::Escape {
                    intermediates: [0; 2],
                    intermediate_len: 0,
                    final_byte: byte,
                })
            }
            _ => {
                self.state = State::Ground;
                None
            }
        }
    }

    fn advance_escape_intermediate(&mut self, byte: u8) -> Option<Event> {
        match byte {
            0x00..=0x17 | 0x19 | 0x1c..=0x1f | 0x7f => Some(Event::Control(byte)),
            0x1b => {
                self.begin_escape();
                None
            }
            0x20..=0x2f if self.escape_intermediate_len < 2 => {
                self.escape_intermediates[self.escape_intermediate_len as usize] = byte;
                self.escape_intermediate_len += 1;
                None
            }
            0x30..=0x7e => {
                self.state = State::Ground;
                Some(Event::Escape {
                    intermediates: self.escape_intermediates,
                    intermediate_len: self.escape_intermediate_len,
                    final_byte: byte,
                })
            }
            _ => {
                self.state = State::Ground;
                None
            }
        }
    }

    fn begin_csi(&mut self) {
        self.state = State::CsiEntry;
        self.csi = Csi::default();
        self.csi_bytes = 0;
    }

    fn advance_csi(&mut self, byte: u8) -> Option<Event> {
        if matches!(byte, 0x00..=0x17 | 0x19 | 0x1c..=0x1f | 0x7f) {
            return Some(Event::Control(byte));
        }
        self.csi_bytes += 1;
        if self.csi_bytes > CSI_MAX_BYTES {
            self.state = State::CsiIgnore;
        }
        if self.state == State::CsiIgnore {
            return match byte {
                0x1b => {
                    self.begin_escape();
                    None
                }
                0x00..=0x17 | 0x19 | 0x1c..=0x1f | 0x7f => Some(Event::Control(byte)),
                0x40..=0x7e => {
                    self.state = State::Ground;
                    None
                }
                _ => None,
            };
        }
        match byte {
            0x1b => {
                self.begin_escape();
                None
            }
            0x20..=0x2f => {
                if self.state == State::CsiIntermediate || self.csi.intermediate_len >= 2 {
                    self.state = State::CsiIgnore;
                } else {
                    self.csi.intermediates[self.csi.intermediate_len as usize] = byte;
                    self.csi.intermediate_len += 1;
                    self.state = State::CsiIntermediate;
                }
                None
            }
            0x30..=0x39 if self.state != State::CsiIntermediate => {
                if self.csi.param_count == 0 {
                    self.csi.param_count = 1;
                }
                let index = self.csi.param_count as usize - 1;
                self.csi.present[index] = true;
                self.csi.params[index] = self.csi.params[index]
                    .saturating_mul(10)
                    .saturating_add((byte - b'0') as u16);
                self.state = State::CsiParam;
                None
            }
            b';' if self.state != State::CsiIntermediate => {
                if self.csi.param_count == 0 {
                    self.csi.param_count = 1;
                }
                if self.csi.param_count as usize >= CSI_MAX_PARAMS {
                    self.state = State::CsiIgnore;
                } else {
                    self.csi.param_count += 1;
                    self.state = State::CsiParam;
                }
                None
            }
            0x3c..=0x3f if self.state == State::CsiEntry && self.csi.private.is_none() => {
                self.csi.private = Some(byte);
                None
            }
            0x40..=0x7e => {
                self.csi.final_byte = byte;
                self.state = State::Ground;
                Some(Event::Csi(self.csi))
            }
            _ => {
                self.state = State::CsiIgnore;
                None
            }
        }
    }

    fn begin_string(&mut self, state: State) {
        self.string.clear();
        self.string_overflow = false;
        self.string_utf8_remaining = 0;
        self.state = state;
    }

    fn push_string(&mut self, byte: u8) {
        // An ST byte inside UTF-8 (for example Ü = C3 9C) is payload, not
        // a terminator. This lexical state must survive split PTY reads.
        self.string_utf8_remaining =
            if self.string_utf8_remaining != 0 && (0x80..=0xbf).contains(&byte) {
                self.string_utf8_remaining - 1
            } else {
                match byte {
                    0xc2..=0xdf => 1,
                    0xe0..=0xef => 2,
                    0xf0..=0xf4 => 3,
                    _ => 0,
                }
            };
        if self.string.len() < STRING_MAX_BYTES {
            self.string.push(byte);
        } else {
            self.string_overflow = true;
        }
    }

    fn advance_osc(&mut self, byte: u8) -> Option<Event> {
        match byte {
            0x07 => self.finish_osc(),
            0x9c if self.string_utf8_remaining == 0 => self.finish_osc(),
            0x1b => {
                self.state = State::OscEscape;
                None
            }
            _ => {
                self.push_string(byte);
                None
            }
        }
    }

    fn advance_osc_escape(&mut self, byte: u8) -> Option<Event> {
        if byte == b'\\' {
            self.finish_osc()
        } else {
            self.push_string(0x1b);
            self.push_string(byte);
            self.state = State::Osc;
            None
        }
    }

    fn finish_osc(&mut self) -> Option<Event> {
        self.state = State::Ground;
        if self.string_overflow {
            self.string.clear();
            None
        } else {
            Some(Event::Osc(std::mem::take(&mut self.string)))
        }
    }

    fn advance_dcs(&mut self, byte: u8) -> Option<Event> {
        match byte {
            0x9c if self.string_utf8_remaining == 0 => {
                self.state = State::Ground;
                None
            }
            0x1b => {
                self.state = State::DcsEscape;
                None
            }
            _ => {
                self.push_string(byte);
                None
            }
        }
    }

    fn advance_dcs_escape(&mut self, byte: u8) -> Option<Event> {
        if byte == b'\\' {
            self.state = State::Ground;
            self.string.clear();
            self.string_overflow = false;
        } else {
            self.push_string(0x1b);
            self.push_string(byte);
            self.state = State::Dcs;
        }
        None
    }

    fn reset_sequence(&mut self) {
        self.state = State::Ground;
        self.escape_intermediates = [0; 2];
        self.escape_intermediate_len = 0;
        self.csi = Csi::default();
        self.csi_bytes = 0;
        self.string.clear();
        self.string_overflow = false;
        self.string_utf8_remaining = 0;
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn utf8_continuations_in_control_strings_are_not_c1_terminators() {
        let input = "\x1b]0;ÜМ title\x1b\\X".as_bytes();
        let expected = vec![
            Event::Osc("0;ÜМ title".as_bytes().to_vec()),
            Event::Print('X'),
        ];
        assert_eq!(parse(input), expected);
        for split in 0..=input.len() {
            let mut parser = Parser::new();
            let mut events = Vec::new();
            parser.feed(&input[..split], |event| events.push(event));
            parser.feed(&input[split..], |event| events.push(event));
            assert_eq!(events, expected, "split at {split}");
        }
        assert_eq!(
            parse("\x1bPÜМ hidden\x1b\\X".as_bytes()),
            vec![Event::Print('X')]
        );
        assert_eq!(
            parse(b"\x1b]0;c1\x9cX"),
            vec![Event::Osc(b"0;c1".to_vec()), Event::Print('X')]
        );
    }
    use super::*;

    fn parse(input: &[u8]) -> Vec<Event> {
        let mut parser = Parser::new();
        let mut events = Vec::new();
        parser.feed(input, |event| events.push(event));
        events
    }

    #[test]
    fn emits_ascii_and_incremental_utf8() {
        let mut parser = Parser::new();
        let mut events = Vec::new();
        parser.feed(b"A\xe2", |event| events.push(event));
        parser.feed(b"\x82\xac", |event| events.push(event));
        assert_eq!(events, vec![Event::Print('A'), Event::Print('€')]);
    }

    #[test]
    fn malformed_utf8_replaces_then_processes_control_byte() {
        let events = parse(&[0xe2, b'A', b'B']);
        assert_eq!(
            events,
            vec![
                Event::Print('\u{fffd}'),
                Event::Print('A'),
                Event::Print('B')
            ]
        );
    }

    #[test]
    fn bounded_arbitrary_byte_streams_are_deterministic_across_chunking() {
        let mut state = 0x4d59_5df4_d0f3_3173u64;
        for _ in 0..128 {
            let mut bytes = vec![0; 4096];
            for byte in &mut bytes {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                *byte = state as u8;
            }
            let mut first = Parser::new();
            let mut first_events = Vec::new();
            first.feed(&bytes, |event| first_events.push(event));
            let mut second = Parser::new();
            let mut second_events = Vec::new();
            for chunk in bytes.chunks(37) {
                second.feed(chunk, |event| second_events.push(event));
            }
            assert_eq!(first_events, second_events);
            assert!(first.string.len() <= STRING_MAX_BYTES);
            assert!(second.string.len() <= STRING_MAX_BYTES);
        }
    }

    #[test]
    fn parses_csi_parameters_and_private_modes() {
        assert_eq!(
            parse(b"\x1b[?1049;25h"),
            vec![Event::Csi(Csi {
                private: Some(b'?'),
                params: {
                    let mut params = [0; CSI_MAX_PARAMS];
                    params[0] = 1049;
                    params[1] = 25;
                    params
                },
                present: {
                    let mut present = [false; CSI_MAX_PARAMS];
                    present[0] = true;
                    present[1] = true;
                    present
                },
                param_count: 2,
                final_byte: b'h',
                ..Csi::default()
            })]
        );
    }

    #[test]
    fn parses_osc_terminators_and_discards_overlong_strings() {
        assert_eq!(
            parse(b"\x1b]2;title\x07"),
            vec![Event::Osc(b"2;title".to_vec())]
        );
        let mut parser = Parser::new();
        parser.feed(b"\x1b]2;", |_| {});
        parser.feed(&vec![b'x'; STRING_MAX_BYTES + 1], |_| {});
        let mut events = Vec::new();
        parser.feed(b"\x07Z", |event| events.push(event));
        assert_eq!(events, vec![Event::Print('Z')]);
    }

    #[test]
    fn can_sub_and_escape_cancel_partial_sequences() {
        let events = parse(b"\x1b[31\x18X\x1b]partial\x1aY\x1b[2J");
        assert_eq!(
            events,
            vec![
                Event::Print('X'),
                Event::Print('Y'),
                Event::Csi(Csi {
                    params: {
                        let mut params = [0; CSI_MAX_PARAMS];
                        params[0] = 2;
                        params
                    },
                    present: {
                        let mut present = [false; CSI_MAX_PARAMS];
                        present[0] = true;
                        present
                    },
                    param_count: 1,
                    final_byte: b'J',
                    ..Csi::default()
                })
            ]
        );
    }

    #[test]
    fn controls_execute_inside_csi_and_escape_recovers_from_malformed_input() {
        let events = parse(b"\x1b[31\x07mA\x1b[1?\x1b[2JB");
        assert!(matches!(events.first(), Some(Event::Control(0x07))));
        assert!(matches!(events.get(1), Some(Event::Csi(csi)) if csi.final_byte == b'm'));
        assert!(events.contains(&Event::Print('A')));
        assert!(events.iter().any(|event| matches!(event, Event::Csi(csi) if csi.final_byte == b'J' && csi.param(0, 0) == 2)));
        assert_eq!(events.last(), Some(&Event::Print('B')));
    }

    #[test]
    fn csi_parameter_and_byte_limits_ignore_until_final() {
        let mut parser = Parser::new();
        let mut events = Vec::new();
        for byte in b"\x1b["
            .iter()
            .copied()
            .chain(std::iter::repeat_n(b'1', CSI_MAX_BYTES + 2))
            .chain(*b"mQ")
        {
            parser.feed(&[byte], |event| events.push(event));
        }
        assert_eq!(events, vec![Event::Print('Q')]);

        let too_many = b"\x1b[1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1mQ";
        assert_eq!(parse(too_many), vec![Event::Print('Q')]);
    }

    #[test]
    fn dcs_is_bounded_and_terminated_without_events() {
        let mut parser = Parser::new();
        for byte in b"\x1bP"
            .iter()
            .copied()
            .chain(std::iter::repeat_n(b'x', STRING_MAX_BYTES + 5))
            .chain(b"\x1b\\Z".iter().copied())
        {
            let mut events = Vec::new();
            parser.feed(&[byte], |event| events.push(event));
            assert!(events.is_empty() || events == vec![Event::Print('Z')]);
        }
    }

    #[test]
    fn ignored_control_strings_do_not_leak_payload_into_printable_text() {
        assert_eq!(
            parse(b"\x1bXignored\x1b\\A\x1b^private\x1b\\B\x98more\x9cC"),
            vec![Event::Print('A'), Event::Print('B'), Event::Print('C')]
        );
    }
}
