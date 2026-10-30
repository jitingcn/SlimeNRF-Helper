const LINE_LIMIT: usize = 2_048;

#[derive(Default, PartialEq, Eq)]
enum Escape {
    #[default]
    None,
    Start,
    Csi,
    Osc,
    OscEnd,
}

#[derive(Default)]
pub(crate) struct Lines {
    bytes: Vec<u8>,
    raw_length: usize,
    escape: Escape,
}

impl Lines {
    pub(crate) fn clean(&self) -> bool {
        self.raw_length == 0 && self.escape == Escape::None
    }

    pub(crate) fn byte(&mut self, byte: u8) -> Result<Option<String>, String> {
        if byte == b'\n' {
            if self.escape != Escape::None {
                return Err("Unterminated console escape sequence".into());
            }
            let line = String::from_utf8(std::mem::take(&mut self.bytes))
                .map_err(|_| "Malformed console UTF-8".to_owned())?;
            self.raw_length = 0;
            return Ok(Some(line));
        }
        self.raw_length += 1;
        if self.raw_length > LINE_LIMIT {
            return Err("Console line exceeds the 2 KiB host limit".into());
        }
        match self.escape {
            Escape::None => match byte {
                0x1b => self.escape = Escape::Start,
                b'\r' => {}
                0..=8 | 11..=12 | 14..=31 | 127 => {
                    return Err("Unexpected console control byte".into());
                }
                _ => self.bytes.push(byte),
            },
            Escape::Start => {
                self.escape = match byte {
                    b'[' => Escape::Csi,
                    b']' => Escape::Osc,
                    _ => return Err("Unsupported console escape sequence".into()),
                };
            }
            Escape::Csi => {
                if (0x40..=0x7e).contains(&byte) {
                    self.escape = Escape::None;
                } else if !(0x20..=0x3f).contains(&byte) {
                    return Err("Malformed console CSI sequence".into());
                }
            }
            Escape::Osc => match byte {
                7 => self.escape = Escape::None,
                0x1b => self.escape = Escape::OscEnd,
                _ => {}
            },
            Escape::OscEnd => {
                if byte != b'\\' {
                    return Err("Malformed console OSC sequence".into());
                }
                self.escape = Escape::None;
            }
        }
        Ok(None)
    }
}
