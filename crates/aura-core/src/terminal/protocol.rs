use super::*;

/// Numeric host metadata for VT queries; no window system or font dependency.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostProperties {
    pub foreground: u32,
    pub background: u32,
    pub cursor: u32,
    pub ansi: [u32; 16],
    pub cell_pixels: [u16; 2],
    pub window_pixels: [u16; 2],
}

impl HostProperties {
    fn color(&self, index: u16) -> Option<u32> {
        match index {
            0..=15 => Some(self.ansi[index as usize]),
            16..=231 => {
                let value = index - 16;
                let level = |n: u16| if n == 0 { 0 } else { 55 + u32::from(n) * 40 };
                Some((level(value / 36) << 16) | (level(value / 6 % 6) << 8) | level(value % 6))
            }
            232..=255 => Some(u32::from(8 + 10 * (index - 232)) * 0x010101),
            _ => None,
        }
    }
}

impl Terminal {
    pub fn set_host_properties(&mut self, properties: HostProperties) {
        self.host_properties = Some(properties);
    }

    /// A host timeout can release a client that never ends its update batch.
    pub fn end_synchronized_output(&mut self) {
        self.modes.synchronized_output = false;
    }

    pub(super) fn protocol_dcs(&self, bytes: &[u8], replies: &mut Vec<u8>) {
        if let Some(names) = bytes.strip_prefix(b"+q") {
            for name in names.split(|byte| *byte == b';').take(32) {
                if name.is_empty() || name.len() > 64 || !name.iter().all(u8::is_ascii_hexdigit) {
                    continue;
                }
                let value = match name {
                    b"544e" | b"544E" => Some("xterm-256color"), // TN
                    b"436f" | b"436F" => Some("256"),            // Co
                    b"524742" => Some("8/8/8"),                  // RGB
                    b"5463" => Some(""),                         // Tc
                    _ => None,
                };
                replies.extend_from_slice(if value.is_some() {
                    b"\x1bP1+r"
                } else {
                    b"\x1bP0+r"
                });
                replies.extend_from_slice(name);
                if let Some(value) = value
                    && !value.is_empty()
                {
                    replies.push(b'=');
                    for byte in value.bytes() {
                        replies.extend_from_slice(format!("{byte:02x}").as_bytes());
                    }
                }
                replies.extend_from_slice(b"\x1b\\");
            }
        } else if let Some(request) = bytes.strip_prefix(b"$q") {
            let value = match request {
                b"r" => Some(format!(
                    "{};{}r",
                    self.top_margin + 1,
                    self.bottom_margin + 1
                )),
                b" q" => {
                    let base = match self.modes.cursor_style {
                        CursorStyle::Block => 1,
                        CursorStyle::Underline => 3,
                        CursorStyle::Bar => 5,
                    };
                    Some(format!("{} q", base + u8::from(!self.modes.cursor_blink)))
                }
                _ => None,
            };
            if let Some(value) = value {
                replies.extend_from_slice(format!("\x1bP1$r{value}\x1b\\").as_bytes());
            } else {
                replies.extend_from_slice(b"\x1bP0$r\x1b\\");
            }
        }
    }

    pub(super) fn protocol_csi(&mut self, csi: &crate::Csi, replies: &mut Vec<u8>) -> bool {
        if csi.private == Some(b'?')
            && csi.intermediate_len == 1
            && csi.intermediates[0] == b'$'
            && csi.final_byte == b'p'
        {
            for index in 0..usize::from(csi.param_count) {
                let mode = csi.param(index, 0);
                let enabled = match mode {
                    1 => Some(self.modes.application_cursor),
                    6 => Some(self.modes.origin),
                    7 => Some(self.modes.auto_wrap),
                    25 => Some(self.modes.cursor_visible),
                    47 | 1047 | 1049 => Some(self.is_alternate_screen()),
                    1000 => Some(self.mouse_press_enabled),
                    1002 => Some(self.mouse_drag_enabled),
                    1003 => Some(self.mouse_motion_enabled),
                    1004 => Some(self.modes.focus_reporting),
                    1006 => Some(self.modes.mouse_sgr),
                    1016 => Some(self.modes.mouse_pixel),
                    2004 => Some(self.modes.bracketed_paste),
                    2026 => Some(self.modes.synchronized_output),
                    _ => None,
                };
                let status = enabled.map_or(0, |enabled| if enabled { 1 } else { 2 });
                replies.extend_from_slice(format!("\x1b[?{mode};{status}$y").as_bytes());
            }
            return true;
        }
        if csi.intermediate_len != 0 {
            return false;
        }
        if csi.private == Some(b'>') && csi.final_byte == b'q' && csi.param(0, 0) == 0 {
            replies.extend_from_slice(
                concat!("\x1bP>|AURA(", env!("CARGO_PKG_VERSION"), ")\x1b\\").as_bytes(),
            );
            return true;
        }
        if csi.private.is_none() && csi.final_byte == b't' {
            let size = match (csi.param(0, 0), self.host_properties) {
                (14, Some(host)) => Some((
                    4,
                    host.window_pixels[1] as usize,
                    host.window_pixels[0] as usize,
                )),
                (16, Some(host)) => Some((
                    6,
                    host.cell_pixels[1] as usize,
                    host.cell_pixels[0] as usize,
                )),
                (18, _) => Some((8, self.active_grid().height(), self.active_grid().width())),
                _ => None,
            };
            if let Some((kind, height, width)) = size {
                replies.extend_from_slice(format!("\x1b[{kind};{height};{width}t").as_bytes());
            }
            return true;
        }
        false
    }

    pub(super) fn protocol_osc(&self, data: &[u8], replies: &mut Vec<u8>) -> bool {
        let Ok(text) = std::str::from_utf8(data) else {
            return false;
        };
        let mut parts = text.split(';');
        let command = parts.next().unwrap_or("");
        if !matches!(command, "4" | "10" | "11" | "12") {
            return false;
        }
        let Some(host) = self.host_properties else {
            return true;
        };
        if command == "4" {
            while let (Some(index), Some(query)) = (parts.next(), parts.next()) {
                if query != "?" {
                    continue;
                }
                if let Ok(index) = index.parse::<u16>()
                    && let Some(color) = host.color(index)
                {
                    color_reply(&format!("4;{index}"), color, replies);
                }
            }
        } else {
            // OSC 10 can query several consecutive dynamic colors in order.
            let first = command.parse::<u16>().unwrap();
            for (offset, query) in parts.enumerate() {
                if query != "?" {
                    continue;
                }
                let (index, color) = match first + offset.min(3) as u16 {
                    10 => (10, host.foreground),
                    11 => (11, host.background),
                    12 => (12, host.cursor),
                    _ => break,
                };
                color_reply(&index.to_string(), color, replies);
            }
        }
        true
    }
}

fn color_reply(command: &str, color: u32, replies: &mut Vec<u8>) {
    let [r, g, b] = [16, 8, 0].map(|shift| ((color >> shift) & 255) * 257);
    replies
        .extend_from_slice(format!("\x1b]{command};rgb:{r:04x}/{g:04x}/{b:04x}\x1b\\").as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn queries_reflect_enabled_features_and_host_geometry_in_stream_order() {
        let mut terminal = Terminal::new(20, 8).unwrap();
        terminal.set_host_properties(HostProperties {
            foreground: 0xd8d8d8,
            background: 0x181818,
            cursor: 0xff0000,
            ansi: [0x123456; 16],
            cell_pixels: [8, 16],
            window_pixels: [168, 140],
        });
        let sequence=b"\x1b[?1003h\x1b[?1003$p\x1b[?9999$p\x1b]10;?\x07\x1b[14t\x1b[16t\x1b[18t\x1b]4;1;?;255;?\x1b\\";
        let expected=b"\x1b[?1003;1$y\x1b[?9999;0$y\x1b]10;rgb:d8d8/d8d8/d8d8\x1b\\\x1b[4;140;168t\x1b[6;16;8t\x1b[8;8;20t\x1b]4;1;rgb:1212/3434/5656\x1b\\\x1b]4;255;rgb:eeee/eeee/eeee\x1b\\";
        assert_eq!(terminal.process(sequence), expected);
        for chunk_size in [1, 2, 3, 7, 13] {
            let mut split = Terminal::new(20, 8).unwrap();
            split.set_host_properties(terminal.host_properties.unwrap());
            let actual: Vec<_> = sequence
                .chunks(chunk_size)
                .flat_map(|chunk| split.process(chunk))
                .collect();
            assert_eq!(actual, expected);
        }
    }
    #[test]
    fn bounded_dcs_queries_reply_without_interpreting_other_control_strings() {
        let bytes = b"\x1bP+q436f;5463;4d73\x1b\\\x1bP$q q\x1b\\\x1b_+q436f\x1b\\";
        let expected =
            b"\x1bP1+r436f=323536\x1b\\\x1bP1+r5463\x1b\\\x1bP0+r4d73\x1b\\\x1bP1$r1 q\x1b\\";
        for chunk_size in [1, 2, 7, bytes.len()] {
            let mut terminal = Terminal::new(8, 3).unwrap();
            let replies: Vec<_> = bytes
                .chunks(chunk_size)
                .flat_map(|chunk| terminal.process(chunk))
                .collect();
            assert_eq!(replies, expected);
        }
    }

    #[test]
    fn sync_reports_state_and_reset_cannot_leave_it_latched() {
        let mut terminal = Terminal::new(8, 3).unwrap();
        assert_eq!(
            terminal.process(b"\x1b[?2026h\x1b[?2026$p"),
            b"\x1b[?2026;1$y"
        );
        assert!(terminal.modes().synchronized_output);
        terminal.end_synchronized_output();
        assert_eq!(terminal.process(b"\x1b[?2026$p"), b"\x1b[?2026;2$y");
        terminal.process(b"\x1b[?2026h\x1bc");
        assert!(!terminal.modes().synchronized_output);
    }
}
