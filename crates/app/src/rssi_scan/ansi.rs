#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum State {
    #[default]
    Text,
    Escape,
    Csi,
    Osc,
    OscEscape,
    ControlString,
    ControlStringEscape,
}

#[derive(Default)]
struct DiscardUtf8 {
    remaining: u8,
    scalar: u32,
    minimum: u32,
}

#[derive(Default)]
pub(crate) struct AnsiFilter {
    state: State,
    scratch: String,
    discard_utf8: DiscardUtf8,
}

impl AnsiFilter {
    pub(crate) fn filter<'a>(&'a mut self, line: &'a str) -> &'a str {
        // A normal line is already valid UTF-8 and starts at a character
        // boundary. Do not combine it with a truncated discarded codepoint.
        if self.discard_utf8.remaining != 0 {
            self.discard_utf8.remaining = 0;
            self.advance('\u{fffd}');
        }
        // Ordinary serial output takes the borrowed, allocation-free path.
        if self.state == State::Text
            && !line.bytes().any(|byte| {
                byte == 0x1b || byte == 0xc2 || (byte.is_ascii_control() && byte != b'\t')
            })
        {
            return line;
        }
        // ANSI-colored unrelated logs reuse this bounded buffer after its first
        // allocation instead of allocating a sanitized String on every line.
        self.scratch.clear();
        self.scratch.reserve(line.len());
        for character in line.chars() {
            if self.advance(character) {
                if !character.is_control() || character == '\t' {
                    self.scratch.push(character);
                } else {
                    // Do not join numeric tokens across an unexpected control.
                    self.scratch.push(' ');
                }
            }
        }
        &self.scratch
    }

    /// Advance ANSI state without rendering or allocating discarded text.
    /// Decode UTF-8 incrementally: continuation bytes inside ordinary Unicode
    /// must never be mistaken for standalone C1 control characters. State is
    /// retained across the shared framer's one-byte overflow calls.
    pub(crate) fn discard(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            if self.discard_utf8.remaining != 0 {
                if (0x80..=0xbf).contains(&byte) {
                    self.discard_utf8.scalar =
                        (self.discard_utf8.scalar << 6) | u32::from(byte & 0x3f);
                    self.discard_utf8.remaining -= 1;
                    if self.discard_utf8.remaining == 0 {
                        let character = (self.discard_utf8.scalar >= self.discard_utf8.minimum)
                            .then(|| char::from_u32(self.discard_utf8.scalar))
                            .flatten()
                            .unwrap_or('\u{fffd}');
                        self.advance(character);
                    }
                    continue;
                }
                self.discard_utf8.remaining = 0;
                self.advance('\u{fffd}');
                // The interrupting byte can itself be ASCII (including an ANSI
                // delimiter), or start another valid codepoint. Process it too.
            }
            let (remaining, scalar, minimum) = match byte {
                0x00..=0x7f => {
                    self.advance(char::from(byte));
                    continue;
                }
                0xc2..=0xdf => (1, u32::from(byte & 0x1f), 0x80),
                0xe0..=0xef => (2, u32::from(byte & 0x0f), 0x800),
                0xf0..=0xf4 => (3, u32::from(byte & 0x07), 0x10000),
                _ => {
                    self.advance('\u{fffd}');
                    continue;
                }
            };
            self.discard_utf8 = DiscardUtf8 {
                remaining,
                scalar,
                minimum,
            };
        }
    }

    fn advance(&mut self, character: char) -> bool {
        let visible =
            self.state == State::Text && !matches!(character, '\u{1b}' | '\u{90}'..='\u{9f}');
        self.state = match (self.state, character) {
            (State::Text, '\u{1b}') => State::Escape,
            (State::Text, '\u{9b}') => State::Csi,
            (State::Text, '\u{9d}') => State::Osc,
            (State::Text, '\u{90}' | '\u{98}' | '\u{9e}' | '\u{9f}') => State::ControlString,
            (State::Text, _) => State::Text,
            (State::Escape, '[') => State::Csi,
            (State::Escape, ']') => State::Osc,
            (State::Escape, 'P' | 'X' | '^' | '_') => State::ControlString,
            (State::Escape, '\u{1b}') => State::Escape,
            (State::Escape, _) => State::Text,
            (State::Csi, '\u{1b}') => State::Escape,
            (State::Csi, '\u{40}'..='\u{7e}') => State::Text,
            (State::Csi, _) => State::Csi,
            (State::Osc, '\u{7}' | '\u{9c}') => State::Text,
            (State::Osc, '\u{1b}') => State::OscEscape,
            (State::Osc, _) => State::Osc,
            (State::OscEscape, '\\' | '\u{7}' | '\u{9c}') => State::Text,
            (State::OscEscape, '\u{1b}') => State::OscEscape,
            (State::OscEscape, _) => State::Osc,
            (State::ControlString, '\u{9c}') => State::Text,
            (State::ControlString, '\u{1b}') => State::ControlStringEscape,
            (State::ControlString, _) => State::ControlString,
            (State::ControlStringEscape, '\\' | '\u{9c}') => State::Text,
            (State::ControlStringEscape, '\u{1b}') => State::ControlStringEscape,
            (State::ControlStringEscape, _) => State::ControlString,
        };
        visible
    }
}
