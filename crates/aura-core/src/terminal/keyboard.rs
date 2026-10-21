use super::*;

impl Terminal {
    pub(super) fn keyboard_csi(&mut self, csi: &crate::Csi, replies: &mut Vec<u8>) -> bool {
        if csi.intermediate_len != 0 {
            return false;
        }
        if csi.private == Some(b'>') && csi.final_byte == b'm' && csi.param(0, 0) == 4 {
            self.modes.modify_other_keys = csi.param(1, 0).min(2) as u8;
            return true;
        }
        if csi.private == Some(b'?') && csi.final_byte == b'm' && csi.param(0, 0) == 4 {
            replies
                .extend_from_slice(format!("\x1b[>4;{}m", self.modes.modify_other_keys).as_bytes());
            return true;
        }
        if csi.final_byte != b'u' || csi.private.is_none() {
            return false;
        }
        if csi.private == Some(b'?') {
            replies.extend_from_slice(format!("\x1b[?{}u", self.modes.keyboard_flags).as_bytes());
            return true;
        }
        let screen = usize::from(self.is_alternate_screen());
        let stack = &mut self.keyboard_stacks[screen];
        let flags = (csi.param(0, 0) & 31) as u8;
        match csi.private {
            Some(b'>') => {
                if stack.len() == 32 {
                    stack.remove(0);
                }
                stack.push(flags);
            }
            Some(b'<') => {
                let remaining = stack.len().saturating_sub(csi.param(0, 1).max(1) as usize);
                stack.truncate(remaining);
                if stack.is_empty() {
                    stack.push(0);
                }
            }
            Some(b'=') => {
                let current = stack.last_mut().expect("keyboard stack base");
                *current = match csi.param(1, 1) {
                    1 => flags,
                    2 => *current | flags,
                    3 => *current & !flags,
                    _ => *current,
                };
            }
            _ => return false,
        }
        self.modes.keyboard_flags = *stack.last().expect("keyboard stack base");
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn keyboard_flags_have_bounded_independent_screen_stacks() {
        let mut terminal = Terminal::new(8, 3).unwrap();
        assert_eq!(terminal.process(b"\x1b[?u"), b"\x1b[?0u");
        terminal.process(b"\x1b[>3u\x1b[=4;2u");
        assert_eq!(terminal.modes().keyboard_flags, 7);
        terminal.process(b"\x1b[?1049h");
        assert_eq!(terminal.modes().keyboard_flags, 0);
        terminal.process(b"\x1b[>31u\x1b[?1049l");
        assert_eq!(terminal.modes().keyboard_flags, 7);
        terminal.process(b"\x1b[<u");
        assert_eq!(terminal.modes().keyboard_flags, 0);
        for _ in 0..1000 {
            terminal.process(b"\x1b[>3u");
        }
        assert_eq!(terminal.keyboard_stacks[0].len(), 32);
        terminal.process(b"\x1b[<65535u\x1b[>4;2m");
        assert_eq!(terminal.modes().keyboard_flags, 0);
        assert_eq!(terminal.process(b"\x1b[?4m"), b"\x1b[>4;2m");
        terminal.process(b"\x1bc");
        assert_eq!(terminal.modes().modify_other_keys, 0);
        assert_eq!(terminal.keyboard_stacks, [vec![0], vec![0]]);
    }
}
