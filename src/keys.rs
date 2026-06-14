use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Translates a crossterm key event to bytes sent to PTY.
/// Covers keys needed for CLI (chars, Ctrl, Alt, navigation).
/// Mouse and bracketed paste are out of MVP scope.
pub fn key_to_bytes(key: &KeyEvent) -> Vec<u8> {
    use KeyCode::*;
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let mut out: Vec<u8> = Vec::new();
    if alt {
        out.push(0x1b);
    }
    match key.code {
        Char(c) if ctrl => {
            let lc = c.to_ascii_lowercase();
            if lc.is_ascii_lowercase() {
                out.push(lc as u8 - b'a' + 1);
            } else if lc == ' ' {
                out.push(0x00); // Ctrl+spacja = NUL (^@)
            }
        }
        Char(c) => {
            let mut buf = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        }
        Enter => out.push(b'\r'),
        Backspace => out.push(0x7f),
        Esc => out.push(0x1b),
        Tab => out.push(b'\t'),
        BackTab => out.extend_from_slice(b"\x1b[Z"),
        Up => out.extend_from_slice(b"\x1b[A"),
        Down => out.extend_from_slice(b"\x1b[B"),
        Right => out.extend_from_slice(b"\x1b[C"),
        Left => out.extend_from_slice(b"\x1b[D"),
        Home => out.extend_from_slice(b"\x1b[H"),
        End => out.extend_from_slice(b"\x1b[F"),
        PageUp => out.extend_from_slice(b"\x1b[5~"),
        PageDown => out.extend_from_slice(b"\x1b[6~"),
        Delete => out.extend_from_slice(b"\x1b[3~"),
        Insert => out.extend_from_slice(b"\x1b[2~"),
        _ => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn maps_basic_keys() {
        assert_eq!(key_to_bytes(&key(KeyCode::Char('a'), KeyModifiers::NONE)), b"a");
        assert_eq!(key_to_bytes(&key(KeyCode::Enter, KeyModifiers::NONE)), b"\r");
        assert_eq!(key_to_bytes(&key(KeyCode::Esc, KeyModifiers::NONE)), b"\x1b");
        assert_eq!(key_to_bytes(&key(KeyCode::Backspace, KeyModifiers::NONE)), b"\x7f");
        assert_eq!(key_to_bytes(&key(KeyCode::Tab, KeyModifiers::NONE)), b"\t");
    }

    #[test]
    fn maps_ctrl_chars() {
        assert_eq!(key_to_bytes(&key(KeyCode::Char('c'), KeyModifiers::CONTROL)), b"\x03");
        assert_eq!(key_to_bytes(&key(KeyCode::Char('d'), KeyModifiers::CONTROL)), b"\x04");
    }

    #[test]
    fn maps_arrows_and_navigation() {
        assert_eq!(key_to_bytes(&key(KeyCode::Up, KeyModifiers::NONE)), b"\x1b[A");
        assert_eq!(key_to_bytes(&key(KeyCode::Down, KeyModifiers::NONE)), b"\x1b[B");
        assert_eq!(key_to_bytes(&key(KeyCode::Right, KeyModifiers::NONE)), b"\x1b[C");
        assert_eq!(key_to_bytes(&key(KeyCode::Left, KeyModifiers::NONE)), b"\x1b[D");
        assert_eq!(key_to_bytes(&key(KeyCode::BackTab, KeyModifiers::SHIFT)), b"\x1b[Z");
        assert_eq!(key_to_bytes(&key(KeyCode::Delete, KeyModifiers::NONE)), b"\x1b[3~");
    }

    #[test]
    fn alt_prefixes_escape() {
        assert_eq!(key_to_bytes(&key(KeyCode::Char('f'), KeyModifiers::ALT)), b"\x1bf");
    }

    #[test]
    fn utf8_char() {
        assert_eq!(key_to_bytes(&key(KeyCode::Char('ż'), KeyModifiers::NONE)), "ż".as_bytes());
    }

    #[test]
    fn ctrl_space_is_nul() {
        assert_eq!(key_to_bytes(&key(KeyCode::Char(' '), KeyModifiers::CONTROL)), b"\x00");
    }

    #[test]
    fn alt_esc_is_double_escape() {
        assert_eq!(key_to_bytes(&key(KeyCode::Esc, KeyModifiers::ALT)), b"\x1b\x1b");
    }
}
