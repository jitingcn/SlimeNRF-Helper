use std::collections::VecDeque;

pub const MAX_LINES: usize = 1_000;
pub const MAX_LINE_BYTES: usize = 8_192;
const CLIPPED: &str = " … [line clipped]";

const MAX_COMMAND_HISTORY: usize = 100;

/// Per-port submitted commands and a transient navigation draft.
#[derive(Default)]
pub struct CommandHistory {
    commands: VecDeque<String>,
    position: Option<usize>,
    draft: String,
}

impl CommandHistory {
    /// A connection attempt or rejected submission must never enter history.
    pub fn record_submission<T, E>(&mut self, command: &str, result: &Result<T, E>) {
        if result.is_err() || command.is_empty() {
            return;
        }
        if self.commands.back().is_none_or(|last| last != command) {
            if self.commands.len() == MAX_COMMAND_HISTORY {
                self.commands.pop_front();
            }
            self.commands.push_back(command.to_owned());
        }
        self.position = None;
        self.draft.clear();
    }

    pub fn navigate(&mut self, older: bool, current: &str) -> Option<String> {
        // Editing a recalled command begins a new draft rather than losing it.
        if self
            .position
            .is_some_and(|index| self.commands[index] != current)
        {
            self.position = None;
        }
        if older {
            let index = match self.position {
                Some(0) => return None,
                Some(index) => index - 1,
                None if self.commands.is_empty() => return None,
                None => {
                    self.draft.clear();
                    self.draft.push_str(current);
                    self.commands.len() - 1
                }
            };
            self.position = Some(index);
            Some(self.commands[index].clone())
        } else {
            let index = self.position?;
            if index + 1 < self.commands.len() {
                self.position = Some(index + 1);
                Some(self.commands[index + 1].clone())
            } else {
                self.position = None;
                Some(std::mem::take(&mut self.draft))
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum AnsiColor {
    #[default]
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct AnsiStyle {
    pub foreground: AnsiColor,
    pub background: AnsiColor,
    pub bold: bool,
    pub dim: bool,
    pub inverse: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StyleRun {
    pub range: std::ops::Range<usize>,
    pub style: AnsiStyle,
}

// Fixed storage also bounds numeric parsing and work per completed sequence.
const MAX_CSI_BYTES: usize = 128;

fn sgr_number(value: &str) -> Option<u16> {
    if value.is_empty() {
        None
    } else {
        value.parse().ok()
    }
}

fn rgb_color(values: [Option<u16>; 3]) -> Option<AnsiColor> {
    Some(AnsiColor::Rgb(
        u8::try_from(values[0]?).ok()?,
        u8::try_from(values[1]?).ok()?,
        u8::try_from(values[2]?).ok()?,
    ))
}

fn colon_color(group: &str) -> Option<(u16, AnsiColor)> {
    let mut fields = group.split(':');
    let target = sgr_number(fields.next()?)?;
    if target != 38 && target != 48 {
        return None;
    }
    let mode = sgr_number(fields.next()?)?;
    let mut values = [None; 4];
    let mut count = 0;
    for field in fields {
        if count == values.len() {
            return None;
        }
        // Empty is allowed only for the optional default color-space field.
        values[count] = if field.is_empty() {
            Some(0)
        } else {
            sgr_number(field)
        };
        if field.is_empty() && !(mode == 2 && count == 0) {
            return None;
        }
        count += 1;
    }
    let color = match (mode, count) {
        (5, 1) => AnsiColor::Indexed(u8::try_from(values[0]?).ok()?),
        (2, 3) => {
            // The first channel may not be empty in the short RGB form.
            if group.split(':').nth(2)?.is_empty() {
                return None;
            }
            rgb_color([values[0], values[1], values[2]])?
        }
        (2, 4) if values[0] == Some(0) => rgb_color([values[1], values[2], values[3]])?,
        _ => return None,
    };
    Some((target, color))
}

impl AnsiStyle {
    fn set_color(&mut self, target: u16, color: AnsiColor) {
        if target == 38 {
            self.foreground = color;
        } else {
            self.background = color;
        }
    }

    fn apply_sgr(&mut self, parameters: &str) {
        let mut groups = parameters.split(';');
        while let Some(group) = groups.next() {
            if group.contains(':') {
                if let Some((target, color)) = colon_color(group) {
                    self.set_color(target, color);
                }
                continue;
            }
            let Some(parameter) = (if group.is_empty() {
                Some(0)
            } else {
                sgr_number(group)
            }) else {
                continue;
            };
            match parameter {
                0 => *self = Self::default(),
                1 => self.bold = true,
                2 => self.dim = true,
                7 => self.inverse = true,
                22 => {
                    self.bold = false;
                    self.dim = false;
                }
                27 => self.inverse = false,
                30..=37 => self.foreground = AnsiColor::Indexed((parameter - 30) as u8),
                40..=47 => self.background = AnsiColor::Indexed((parameter - 40) as u8),
                90..=97 => self.foreground = AnsiColor::Indexed((parameter - 90 + 8) as u8),
                100..=107 => self.background = AnsiColor::Indexed((parameter - 100 + 8) as u8),
                39 => self.foreground = AnsiColor::Default,
                49 => self.background = AnsiColor::Default,
                38 | 48 => {
                    let color = match groups.next().and_then(sgr_number) {
                        Some(5) => groups
                            .next()
                            .and_then(sgr_number)
                            .and_then(|index| u8::try_from(index).ok())
                            .map(AnsiColor::Indexed),
                        Some(2) => {
                            // Consume all channels even if one is malformed: a channel
                            // of 0, 1, 2, 7 or 31 must never become an independent SGR.
                            let red = groups.next().and_then(sgr_number);
                            let green = groups.next().and_then(sgr_number);
                            let blue = groups.next().and_then(sgr_number);
                            rgb_color([red, green, blue])
                        }
                        // Unknown modes have no unambiguous channel count.
                        _ => break,
                    };
                    if let Some(color) = color {
                        self.set_color(parameter, color);
                    }
                }
                _ => {}
            }
        }
    }
}

/// Replace one character's style, shifting subsequent byte ranges if necessary.
/// Appended output never scans existing runs; plain rows never allocate runs.
fn replace_style(
    runs: &mut Vec<StyleRun>,
    start: usize,
    end: usize,
    length: usize,
    style: AnsiStyle,
) -> bool {
    let default = AnsiStyle::default();
    let new_end = start + length;
    if start == end {
        if style == default {
            return false;
        }
        if let Some(last) = runs.last_mut()
            && last.range.end == start
            && last.style == style
        {
            last.range.end = new_end;
        } else {
            runs.push(StyleRun {
                range: start..new_end,
                style,
            });
        }
        return true;
    }
    let first = runs.partition_point(|run| run.range.end <= start);
    let after = runs.partition_point(|run| run.range.start < end);
    let old_style = runs
        .get(first)
        .filter(|run| run.range.start <= start)
        .map_or(default, |run| run.style);
    if old_style == style && end == new_end {
        return false;
    }
    let left = runs
        .get(first)
        .filter(|run| run.range.start < start)
        .map(|run| StyleRun {
            range: run.range.start..start,
            style: run.style,
        });
    let right = after
        .checked_sub(1)
        .and_then(|index| runs.get(index))
        .filter(|run| run.range.end > end)
        .map(|run| StyleRun {
            range: new_end..(run.range.end - end + new_end),
            style: run.style,
        });
    for run in &mut runs[after..] {
        run.range.start = run.range.start - end + new_end;
        run.range.end = run.range.end - end + new_end;
    }
    let middle = (style != default).then_some(StyleRun {
        range: start..new_end,
        style,
    });
    let inserted =
        usize::from(left.is_some()) + usize::from(middle.is_some()) + usize::from(right.is_some());
    runs.splice(first..after, [left, middle, right].into_iter().flatten());
    let mut index = first.saturating_sub(1);
    let mut boundary = (first + inserted).min(runs.len());
    while index + 1 < runs.len() && index < boundary {
        if runs[index].range.end == runs[index + 1].range.start
            && runs[index].style == runs[index + 1].style
        {
            runs[index].range.end = runs[index + 1].range.end;
            runs.remove(index + 1);
            boundary = boundary.saturating_sub(1);
        } else {
            index += 1;
        }
    }
    true
}

#[derive(Clone, Copy, Default)]
enum Escape {
    #[default]
    Ground,
    Start,
    Intermediate,
    Csi,
    String {
        bell: bool,
    },
    StringTerminator {
        bell: bool,
    },
}

/// Bounded plain-text scrollback with byte-indexed SGR styling. Other ANSI
/// sequences are discarded; carriage return overwrites the current line.
pub struct TerminalBuffer {
    lines: VecDeque<String>,
    line_revisions: VecDeque<u64>,
    line_styles: VecDeque<Vec<StyleRun>>,
    style: AnsiStyle,
    csi: [u8; MAX_CSI_BYTES],
    csi_len: usize,
    csi_invalid: bool,
    first_line_id: u64,
    cursor: usize,
    clipped: bool,
    escape: Escape,
    utf8: [u8; 4],
    utf8_len: usize,
    utf8_need: usize,
    revision: u64,
    // Changes whenever retained byte offsets may refer to different content.
    selection_epoch: u64,
}

impl Default for TerminalBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl TerminalBuffer {
    pub fn new() -> Self {
        let mut lines = VecDeque::new();
        lines.push_back(String::new());
        Self {
            lines,
            line_revisions: VecDeque::from([0]),
            line_styles: VecDeque::from([Vec::new()]),
            style: AnsiStyle::default(),
            csi: [0; MAX_CSI_BYTES],
            csi_len: 0,
            csi_invalid: false,
            first_line_id: 0,
            cursor: 0,
            clipped: false,
            escape: Escape::Ground,
            utf8: [0; 4],
            utf8_len: 0,
            utf8_need: 0,
            revision: 0,
            selection_epoch: 0,
        }
    }

    pub fn push(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.byte(byte);
        }
    }

    pub fn finish(&mut self) {
        if self.utf8_len != 0 {
            self.utf8_len = 0;
            self.utf8_need = 0;
            self.print('\u{fffd}');
        }
        self.escape = Escape::Ground;
        self.style = AnsiStyle::default();
        self.csi_len = 0;
        self.csi_invalid = false;
    }

    pub fn clear(&mut self) {
        let revision = self.revision.wrapping_add(1);
        let selection_epoch = self.selection_epoch.wrapping_add(1);
        let first_line_id = self.first_line_id.wrapping_add(self.lines.len() as u64);
        *self = Self::new();
        self.revision = revision;
        self.selection_epoch = selection_epoch;
        self.first_line_id = first_line_id;
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// A selection remains valid only within the same connection generation and epoch.
    /// Unlike the revision, ordinary appended output does not change this value.
    pub fn selection_epoch(&self) -> u64 {
        self.selection_epoch
    }

    /// Stable row identities and revisions allow visible consumers to copy only
    /// changed rows. The final row has no separator, even when it is empty.
    pub(crate) fn rows(&self) -> impl Iterator<Item = (u64, u64, &str, &[StyleRun], bool)> {
        self.lines
            .iter()
            .zip(&self.line_revisions)
            .zip(&self.line_styles)
            .enumerate()
            .map(|(index, ((text, revision), styles))| {
                (
                    self.first_line_id.wrapping_add(index as u64),
                    *revision,
                    text.as_str(),
                    styles.as_slice(),
                    index + 1 < self.lines.len(),
                )
            })
    }

    pub fn text(&self) -> String {
        let size =
            self.lines.iter().map(String::len).sum::<usize>() + self.lines.len().saturating_sub(1);
        let mut text = String::with_capacity(size);
        for (index, line) in self.lines.iter().enumerate() {
            if index != 0 {
                text.push('\n');
            }
            text.push_str(line);
        }
        text
    }

    fn byte(&mut self, byte: u8) {
        // A malformed UTF-8 prefix must not swallow the next ASCII/control byte.
        if self.utf8_len != 0 {
            let valid_second = self.utf8_len != 1
                || match self.utf8[0] {
                    0xe0 => byte >= 0xa0,
                    0xed => byte <= 0x9f,
                    0xf0 => byte >= 0x90,
                    0xf4 => byte <= 0x8f,
                    _ => true,
                };
            if byte & 0xc0 == 0x80 && valid_second {
                self.utf8[self.utf8_len] = byte;
                self.utf8_len += 1;
                if self.utf8_len == self.utf8_need {
                    let character = std::str::from_utf8(&self.utf8[..self.utf8_len])
                        .ok()
                        .and_then(|s| s.chars().next())
                        .unwrap_or('\u{fffd}');
                    self.utf8_len = 0;
                    self.print(character);
                }
                return;
            }
            self.utf8_len = 0;
            self.print('\u{fffd}');
        }

        match self.escape {
            Escape::String { bell } => {
                if byte == 0x1b {
                    self.escape = Escape::StringTerminator { bell };
                } else if (bell && byte == 7) || byte == 0x18 || byte == 0x1a {
                    self.escape = Escape::Ground;
                }
                return;
            }
            Escape::StringTerminator { bell } => {
                self.escape = if byte == b'\\' || (bell && byte == 7) {
                    Escape::Ground
                } else if byte == 0x1b {
                    Escape::StringTerminator { bell }
                } else {
                    Escape::String { bell }
                };
                return;
            }
            _ => {}
        }
        if byte == 0x1b {
            self.escape = Escape::Start;
            return;
        }
        if byte == 0x18 || byte == 0x1a {
            self.escape = Escape::Ground;
            return;
        }
        match self.escape {
            Escape::Start => {
                self.escape = match byte {
                    b'[' => {
                        self.csi_len = 0;
                        self.csi_invalid = false;
                        Escape::Csi
                    }
                    b']' => Escape::String { bell: true },
                    b'P' | b'X' | b'^' | b'_' => Escape::String { bell: false },
                    0x20..=0x2f => Escape::Intermediate,
                    _ => Escape::Ground,
                };
            }
            Escape::Intermediate => {
                if !(0x20..=0x2f).contains(&byte) {
                    self.escape = Escape::Ground;
                }
            }
            Escape::Csi => {
                if (0x40..=0x7e).contains(&byte) {
                    if byte == b'm' && !self.csi_invalid {
                        // Only ASCII digits and separators enter this fixed buffer.
                        self.style
                            .apply_sgr(std::str::from_utf8(&self.csi[..self.csi_len]).unwrap());
                    }
                    self.escape = Escape::Ground;
                } else if !self.csi_invalid {
                    if (byte.is_ascii_digit() || byte == b';' || byte == b':')
                        && self.csi_len < MAX_CSI_BYTES
                    {
                        self.csi[self.csi_len] = byte;
                        self.csi_len += 1;
                    } else {
                        // Discard through the final byte, without applying a prefix.
                        self.csi_invalid = true;
                    }
                }
            }
            Escape::Ground => match byte {
                b'\n' => {
                    self.revision = self.revision.wrapping_add(1);
                    *self.line_revisions.back_mut().unwrap() = self.revision;
                    self.lines.push_back(String::new());
                    self.line_revisions.push_back(self.revision);
                    self.line_styles.push_back(Vec::new());
                    if self.lines.len() > MAX_LINES {
                        self.lines.pop_front();
                        self.line_revisions.pop_front();
                        self.line_styles.pop_front();
                        self.first_line_id = self.first_line_id.wrapping_add(1);
                        self.selection_epoch = self.selection_epoch.wrapping_add(1);
                    }
                    self.cursor = 0;
                    self.clipped = false;
                }
                b'\r' => {
                    self.cursor = 0;
                }
                8 => {
                    let line = self.lines.back().unwrap();
                    if self.cursor > 0 {
                        self.cursor -= 1;
                        while !line.is_char_boundary(self.cursor) {
                            self.cursor -= 1;
                        }
                    }
                }
                b'\t' => {
                    for _ in 0..4 {
                        self.print(' ');
                    }
                }
                0x00..=0x1f | 0x7f => {}
                0x20..=0x7e => self.print(byte as char),
                0xc2..=0xf4 => {
                    self.utf8[0] = byte;
                    self.utf8_len = 1;
                    self.utf8_need = if byte <= 0xdf {
                        2
                    } else if byte <= 0xef {
                        3
                    } else {
                        4
                    };
                }
                _ => self.print('\u{fffd}'),
            },
            Escape::String { .. } | Escape::StringTerminator { .. } => unreachable!(),
        }
    }

    fn print(&mut self, character: char) {
        if self.clipped {
            return;
        }
        let line = self.lines.back_mut().unwrap();
        let end = if self.cursor < line.len() {
            self.cursor + line[self.cursor..].chars().next().unwrap().len_utf8()
        } else {
            self.cursor
        };
        let new_length = line.len() - (end - self.cursor) + character.len_utf8();
        if new_length > MAX_LINE_BYTES {
            let mut keep = MAX_LINE_BYTES - CLIPPED.len();
            while !line.is_char_boundary(keep) {
                keep -= 1;
            }
            line.truncate(keep);
            let runs = self.line_styles.back_mut().unwrap();
            runs.truncate(runs.partition_point(|run| run.range.start < keep));
            if let Some(last) = runs.last_mut() {
                last.range.end = last.range.end.min(keep);
            }
            self.selection_epoch = self.selection_epoch.wrapping_add(1);
            line.push_str(CLIPPED);
            self.cursor = line.len();
            self.clipped = true;
        } else {
            let mut encoded = [0; 4];
            let replacement: &str = character.encode_utf8(&mut encoded);
            let text_changed = &line[self.cursor..end] != replacement;
            let style_changed = replace_style(
                self.line_styles.back_mut().unwrap(),
                self.cursor,
                end,
                character.len_utf8(),
                self.style,
            );
            if text_changed {
                if self.cursor < line.len() {
                    self.selection_epoch = self.selection_epoch.wrapping_add(1);
                }
                line.replace_range(self.cursor..end, replacement);
            }
            if text_changed || style_changed {
                self.revision = self.revision.wrapping_add(1);
                *self.line_revisions.back_mut().unwrap() = self.revision;
            }
            self.cursor += character.len_utf8();
            return;
        }
        self.revision = self.revision.wrapping_add(1);
        *self.line_revisions.back_mut().unwrap() = self.revision;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_style_invariants(terminal: &TerminalBuffer) {
        assert_eq!(terminal.lines.len(), terminal.line_styles.len());
        assert_eq!(terminal.lines.len(), terminal.line_revisions.len());
        for (_, _, text, runs, _) in terminal.rows() {
            let mut previous: Option<&StyleRun> = None;
            for run in runs {
                assert!(run.range.start < run.range.end);
                assert!(text.is_char_boundary(run.range.start));
                assert!(text.is_char_boundary(run.range.end));
                assert_ne!(run.style, AnsiStyle::default());
                if let Some(previous) = previous {
                    assert!(previous.range.end <= run.range.start);
                    assert!(previous.range.end != run.range.start || previous.style != run.style);
                }
                previous = Some(run);
            }
        }
    }

    fn first_row_styles(terminal: &TerminalBuffer) -> Vec<StyleRun> {
        terminal.rows().next().unwrap().3.to_vec()
    }

    fn foreground(index: u8) -> AnsiStyle {
        AnsiStyle {
            foreground: AnsiColor::Indexed(index),
            ..AnsiStyle::default()
        }
    }

    #[test]
    fn style_state_is_per_port_and_persists_across_lines() {
        let mut first = TerminalBuffer::new();
        let mut second = TerminalBuffer::new();
        first.push(b"\x1b[31");
        second.push(b"plain\n");
        first.push(b"mred\nstill red");
        second.push(b"\x1b[32mgreen");
        assert_eq!(
            first.rows().nth(1).unwrap().3,
            &[StyleRun {
                range: 0..9,
                style: foreground(1),
            }]
        );
        assert!(first_row_styles(&second).is_empty());
        assert_eq!(
            second.rows().nth(1).unwrap().3,
            &[StyleRun {
                range: 0..5,
                style: foreground(2),
            }]
        );
    }

    #[test]
    fn malformed_sequences_have_the_same_recovery_at_every_split() {
        let inputs = [
            "\x1b[34m\x1b[38;2;999;0;7;1m界",
            "\x1b[34m\x1b[38:2::999:0:7;1m界",
            "\x1b[34m\x1b[38;2;1m界",
            "\x1b[34m\x1b[?31m界",
            "\x1b[34m\x1b[31 m界",
            "\x1b[34m\x1b[31\x1b[32m界",
        ];
        for input in inputs {
            let mut whole = TerminalBuffer::new();
            whole.push(input.as_bytes());
            for split in 0..=input.len() {
                let mut chunked = TerminalBuffer::new();
                chunked.push(&input.as_bytes()[..split]);
                chunked.push(&input.as_bytes()[split..]);
                assert_eq!(chunked.text(), "界", "{input:?}, split {split}");
                assert_eq!(first_row_styles(&chunked), first_row_styles(&whole));
                assert_style_invariants(&chunked);
            }
        }
    }

    #[test]
    fn standard_and_bright_foreground_background_and_defaults() {
        for index in 0..16 {
            let fg = if index < 8 {
                30 + index
            } else {
                90 + index - 8
            };
            let bg = if index < 8 {
                40 + index
            } else {
                100 + index - 8
            };
            let mut terminal = TerminalBuffer::new();
            terminal.push(format!("\x1b[{fg};{bg};1;2;7mA\x1b[39;22;27mB\x1b[49mC").as_bytes());
            assert_eq!(terminal.text(), "ABC");
            assert_eq!(
                first_row_styles(&terminal),
                vec![
                    StyleRun {
                        range: 0..1,
                        style: AnsiStyle {
                            foreground: AnsiColor::Indexed(index),
                            background: AnsiColor::Indexed(index),
                            bold: true,
                            dim: true,
                            inverse: true,
                        }
                    },
                    StyleRun {
                        range: 1..2,
                        style: AnsiStyle {
                            background: AnsiColor::Indexed(index),
                            ..AnsiStyle::default()
                        }
                    },
                ]
            );
            assert_style_invariants(&terminal);
        }
        for reset in ["0", ""] {
            let mut terminal = TerminalBuffer::new();
            terminal.push(format!("\x1b[31;42;1;2;7mA\x1b[{reset}mB").as_bytes());
            assert_eq!(first_row_styles(&terminal).len(), 1);
            assert_eq!(first_row_styles(&terminal)[0].range, 0..1);
            assert_eq!(terminal.style, AnsiStyle::default());
        }
    }

    #[test]
    fn extended_colors_and_utf8_survive_every_stream_split() {
        let sequences = [
            (
                "\x1b[38;5;200;48;2;10;20;30m",
                AnsiColor::Indexed(200),
                AnsiColor::Rgb(10, 20, 30),
            ),
            (
                "\x1b[38:5:200;48:2:10:20:30m",
                AnsiColor::Indexed(200),
                AnsiColor::Rgb(10, 20, 30),
            ),
            (
                "\x1b[38:2::1:2:3;48:2:0:10:20:30m",
                AnsiColor::Rgb(1, 2, 3),
                AnsiColor::Rgb(10, 20, 30),
            ),
        ];
        for (sequence, foreground, background) in sequences {
            let input = format!("a{sequence}界🦀\x1b[0mz\n");
            for split in 0..=input.len() {
                let mut terminal = TerminalBuffer::new();
                terminal.push(&input.as_bytes()[..split]);
                terminal.push(&input.as_bytes()[split..]);
                terminal.finish();
                assert_eq!(terminal.text(), "a界🦀z\n", "split {split}");
                assert_eq!(
                    first_row_styles(&terminal),
                    vec![StyleRun {
                        range: 1..8,
                        style: AnsiStyle {
                            foreground,
                            background,
                            ..AnsiStyle::default()
                        },
                    }],
                    "split {split}"
                );
                assert_style_invariants(&terminal);
            }
            let mut terminal = TerminalBuffer::new();
            for byte in input.bytes() {
                terminal.push(&[byte]);
            }
            assert_eq!(first_row_styles(&terminal)[0].range, 1..8);
        }
    }

    #[test]
    fn malformed_extended_colors_never_reinterpret_channels_as_attributes() {
        for sequence in [
            "38;2;999;1;7",
            "38;2;;31;0",
            "48;2;65536;0;7",
            "38;5;256",
            "38:2::256:1:7",
            "38:2:1::7",
            "38:2:1:2",
            "38:2:1:2:3:4",
            "38:5:",
            "38:5:256",
        ] {
            let mut terminal = TerminalBuffer::new();
            terminal.push(format!("\x1b[34m\x1b[{sequence};2mX").as_bytes());
            assert_eq!(terminal.text(), "X", "{sequence}");
            assert_eq!(
                first_row_styles(&terminal)[0].style,
                AnsiStyle {
                    dim: true,
                    ..foreground(4)
                },
                "{sequence}"
            );
        }
        for sequence in ["38;2;1", "38;2", "48;9;0;1;7", "38;;0;1;7"] {
            let mut terminal = TerminalBuffer::new();
            terminal.push(format!("\x1b[34m\x1b[{sequence}mX").as_bytes());
            assert_eq!(
                first_row_styles(&terminal)[0].style,
                foreground(4),
                "{sequence}"
            );
        }
        let mut terminal = TerminalBuffer::new();
        terminal.push(b"\x1b[34m\x1b[999999;1;4;9;4:3;48:2:4:5:6;27mX");
        assert_eq!(
            first_row_styles(&terminal)[0].style,
            AnsiStyle {
                bold: true,
                background: AnsiColor::Rgb(4, 5, 6),
                ..foreground(4)
            }
        );
    }

    #[test]
    fn csi_storage_and_recovery_are_bounded() {
        let mut terminal = TerminalBuffer::new();
        terminal.push(b"\x1b[34m\x1b[");
        for _ in 0..MAX_LINE_BYTES {
            terminal.push(b"1;");
            assert!(terminal.csi_len <= MAX_CSI_BYTES);
        }
        terminal.push(b"31mX\x1b[?31mY\x1b[31 mZ\x1b[0m!");
        assert_eq!(terminal.text(), "XYZ!");
        assert_eq!(
            first_row_styles(&terminal),
            vec![StyleRun {
                range: 0..3,
                style: foreground(4),
            }]
        );
        terminal.push(b"\x1b[31\x18A\x1b[31\x1b[32mB");
        assert_eq!(terminal.text(), "XYZ!AB");
        assert_eq!(
            first_row_styles(&terminal)[1],
            StyleRun {
                range: 5..6,
                style: foreground(2),
            }
        );
        assert_style_invariants(&terminal);
    }

    #[test]
    fn pure_style_overwrite_revises_row_but_keeps_plain_selection_valid() {
        let mut terminal = TerminalBuffer::new();
        terminal.push(b"\x1b[31mXXX");
        let revision = terminal.rows().next().unwrap().1;
        let epoch = terminal.selection_epoch();
        terminal.push(b"\rX");
        assert_eq!(terminal.rows().next().unwrap().1, revision);
        terminal.push(b"\x1b[32mX");
        assert_ne!(terminal.rows().next().unwrap().1, revision);
        assert_eq!(terminal.selection_epoch(), epoch);
        assert_eq!(terminal.text(), "XXX");
        assert_eq!(
            first_row_styles(&terminal),
            vec![
                StyleRun {
                    range: 0..1,
                    style: foreground(1)
                },
                StyleRun {
                    range: 1..2,
                    style: foreground(2)
                },
                StyleRun {
                    range: 2..3,
                    style: foreground(1)
                },
            ]
        );
        terminal.push(b"\x08\x1b[31mX");
        assert_eq!(
            first_row_styles(&terminal),
            vec![StyleRun {
                range: 0..3,
                style: foreground(1)
            },]
        );
        terminal.push(b"\r\x1b[0mXXX");
        assert!(first_row_styles(&terminal).is_empty());
        assert_eq!(terminal.selection_epoch(), epoch);
        assert_style_invariants(&terminal);
    }

    #[test]
    fn unicode_overwrite_shifts_styles_and_keeps_default_gaps() {
        let mut terminal = TerminalBuffer::new();
        terminal.push("\x1b[31m世\x1b[0ma\x1b[32m🦀!\x1b[0mz".as_bytes());
        terminal.push("\r\x1b[34mQ\x1b[0m界\x08🦀".as_bytes());
        assert_eq!(terminal.text(), "Q🦀🦀!z");
        assert_eq!(
            first_row_styles(&terminal),
            vec![
                StyleRun {
                    range: 0..1,
                    style: foreground(4)
                },
                StyleRun {
                    range: 5..10,
                    style: foreground(2)
                },
            ]
        );
        terminal.push("\r\x1b[32m界世".as_bytes());
        assert_eq!(terminal.text(), "界世🦀!z");
        assert_eq!(
            first_row_styles(&terminal),
            vec![StyleRun {
                range: 0..11,
                style: foreground(2)
            },]
        );
        assert_style_invariants(&terminal);
    }

    #[test]
    fn styled_clipping_and_history_trimming_preserve_ranges_and_ids() {
        let mut terminal = TerminalBuffer::new();
        terminal.push(b"\x1b[31m");
        terminal.push("界".repeat(MAX_LINE_BYTES / 3 + 1).as_bytes());
        let text = terminal.text();
        assert!(text.ends_with(CLIPPED));
        assert_eq!(
            first_row_styles(&terminal),
            vec![StyleRun {
                range: 0..text.len() - CLIPPED.len(),
                style: foreground(1),
            }]
        );
        assert_style_invariants(&terminal);
        let revision = terminal.revision();
        terminal.push(b"\x1b[32mignored");
        assert_eq!(terminal.revision(), revision);
        terminal.push(b"\n");
        let id = terminal.rows().last().unwrap().0;
        for _ in 0..MAX_LINES {
            terminal.push(b"\x1b[32mx\x1b[0my\n");
        }
        assert_eq!(terminal.rows().count(), MAX_LINES);
        assert_eq!(terminal.rows().next().unwrap().0, id + 1);
        assert_eq!(
            first_row_styles(&terminal),
            vec![StyleRun {
                range: 0..1,
                style: foreground(2),
            }]
        );
        assert!(terminal.rows().last().unwrap().3.is_empty());
        assert_style_invariants(&terminal);
    }

    #[test]
    fn default_rows_allocate_no_runs_and_repeated_styles_coalesce() {
        let mut terminal = TerminalBuffer::new();
        terminal.push(b"plain\x1b[0m\x1b[39;49;22;27m output\nmore");
        assert!(terminal.line_styles.iter().all(|runs| runs.capacity() == 0));
        terminal.clear();
        for _ in 0..100 {
            terminal.push(b"\x1b[31mx");
        }
        assert_eq!(
            first_row_styles(&terminal),
            vec![StyleRun {
                range: 0..100,
                style: foreground(1)
            },]
        );
    }

    #[test]
    fn finish_and_clear_end_incomplete_sequences_and_reset_stream_style() {
        let mut terminal = TerminalBuffer::new();
        terminal.push(b"\x1b[31mA\x1b[48;2;1;");
        terminal.finish();
        terminal.push(b"B");
        assert_eq!(terminal.text(), "AB");
        assert_eq!(
            first_row_styles(&terminal),
            vec![StyleRun {
                range: 0..1,
                style: foreground(1)
            },]
        );
        terminal.push(b"\x1b[32m\xf0\x9f");
        terminal.finish();
        terminal.push(b"C");
        assert_eq!(terminal.text(), "AB\u{fffd}C");
        assert_eq!(
            first_row_styles(&terminal)[1],
            StyleRun {
                range: 2..5,
                style: foreground(2),
            }
        );
        terminal.push(b"\x1b[1;2;7;45m\x1b[");
        terminal.clear();
        terminal.push(b"plain");
        assert_eq!(terminal.text(), "plain");
        assert!(first_row_styles(&terminal).is_empty());
        assert_eq!(terminal.style, AnsiStyle::default());
        assert_style_invariants(&terminal);
    }

    #[test]
    fn appended_unicode_and_whitespace_keep_selected_byte_offsets_valid() {
        let mut terminal = TerminalBuffer::new();
        terminal.push("prefix  世界 \n  🦀  ".as_bytes());
        let selected = "  世界 \n  🦀  ";
        let range = "prefix".len()..terminal.text().len();
        let epoch = terminal.selection_epoch();
        let revision = terminal.revision();
        // Split a multibyte character across polling chunks.
        terminal.push(&[0xe7, 0x95]);
        assert_eq!(terminal.revision(), revision);
        terminal.push(&[0x8c, b'\n', b' ', b' ']);
        assert_eq!(terminal.selection_epoch(), epoch);
        assert_ne!(terminal.revision(), revision);
        assert_eq!(&terminal.text()[range], selected);
        // ANSI-only input changes neither the rendered snapshot nor its offsets.
        let revision = terminal.revision();
        terminal.push(b"\x1b[31m\x1b[0m");
        assert_eq!(terminal.revision(), revision);
        assert_eq!(terminal.selection_epoch(), epoch);
    }

    #[test]
    fn rewriting_and_clear_invalidate_even_when_original_bytes_return() {
        let mut terminal = TerminalBuffer::new();
        terminal.push("世界 ".as_bytes());
        let epoch = terminal.selection_epoch();
        // Moving the cursor and overwriting with identical content is harmless.
        terminal.push("\r世界 ".as_bytes());
        assert_eq!(terminal.selection_epoch(), epoch);
        terminal.push("\r好".as_bytes());
        assert_ne!(terminal.selection_epoch(), epoch);
        terminal.push("\r世".as_bytes());
        assert_eq!(terminal.text(), "世界 ");
        assert_ne!(terminal.selection_epoch(), epoch);
        let epoch = terminal.selection_epoch();
        terminal.clear();
        terminal.push("世界 ".as_bytes());
        assert_eq!(terminal.text(), "世界 ");
        assert_ne!(terminal.selection_epoch(), epoch);
    }

    #[test]
    fn trimming_repeated_head_lines_invalidates_an_identical_snapshot() {
        let mut terminal = TerminalBuffer::new();
        for _ in 0..MAX_LINES - 1 {
            terminal.push(b"same\n");
        }
        let text = terminal.text();
        let epoch = terminal.selection_epoch();
        terminal.push(b"same\n");
        assert_eq!(terminal.text(), text);
        assert_ne!(terminal.selection_epoch(), epoch);
        assert_eq!(terminal.lines.len(), MAX_LINES);
    }

    #[test]
    fn clipping_invalidates_selection_once_and_stops_revising_discarded_tail() {
        let mut terminal = TerminalBuffer::new();
        terminal.push("界".repeat(MAX_LINE_BYTES / 3).as_bytes());
        let epoch = terminal.selection_epoch();
        terminal.push("🦀".as_bytes());
        assert_ne!(terminal.selection_epoch(), epoch);
        let text = terminal.text();
        assert!(text.ends_with(CLIPPED));
        assert!(text.len() <= MAX_LINE_BYTES);
        let revision = terminal.revision();
        let epoch = terminal.selection_epoch();
        terminal.push("discarded 界🦀".as_bytes());
        assert_eq!(terminal.text(), text);
        assert_eq!(terminal.revision(), revision);
        assert_eq!(terminal.selection_epoch(), epoch);
    }

    #[test]
    fn command_history_restores_draft_and_stops_at_both_boundaries() {
        let mut history = CommandHistory::default();
        assert_eq!(history.navigate(true, "draft"), None);
        assert_eq!(history.navigate(false, "draft"), None);
        history.record_submission("info", &Ok::<_, ()>(()));
        history.record_submission("help", &Ok::<_, ()>(()));
        assert_eq!(history.navigate(true, "unsent"), Some("help".into()));
        assert_eq!(history.navigate(true, "help"), Some("info".into()));
        assert_eq!(history.navigate(true, "info"), None);
        assert_eq!(history.navigate(false, "info"), Some("help".into()));
        assert_eq!(history.navigate(false, "help"), Some("unsent".into()));
        assert_eq!(history.navigate(false, "unsent"), None);
    }

    #[test]
    fn command_history_is_independent_for_each_port() {
        let mut ports = std::collections::BTreeMap::<&str, CommandHistory>::new();
        ports
            .entry("COM1")
            .or_default()
            .record_submission("info", &Ok::<_, ()>(()));
        ports
            .entry("COM2")
            .or_default()
            .record_submission("help", &Ok::<_, ()>(()));
        assert_eq!(
            ports.get_mut("COM1").unwrap().navigate(true, "one"),
            Some("info".into())
        );
        assert_eq!(
            ports.get_mut("COM2").unwrap().navigate(true, "two"),
            Some("help".into())
        );
        assert_eq!(
            ports.get_mut("COM1").unwrap().navigate(false, "info"),
            Some("one".into())
        );
        assert_eq!(
            ports.get_mut("COM2").unwrap().navigate(false, "help"),
            Some("two".into())
        );
    }

    #[test]
    fn command_history_bounds_capacity_and_deduplicates_only_neighbors() {
        let mut history = CommandHistory::default();
        history.record_submission("info", &Ok::<_, ()>(()));
        history.record_submission("info", &Ok::<_, ()>(()));
        history.record_submission("help", &Ok::<_, ()>(()));
        history.record_submission("info", &Ok::<_, ()>(()));
        assert_eq!(history.commands.len(), 3);
        for index in 0..MAX_COMMAND_HISTORY + 5 {
            history.record_submission(&format!("command {index}"), &Ok::<_, ()>(()));
        }
        assert_eq!(history.commands.len(), MAX_COMMAND_HISTORY);
        assert_eq!(history.commands.front().unwrap(), "command 5");
    }

    #[test]
    fn command_history_records_only_successfully_submitted_commands() {
        let mut history = CommandHistory::default();
        history.record_submission("failed", &Err::<(), _>("not connected"));
        history.record_submission("", &Ok::<_, ()>(()));
        assert!(history.commands.is_empty());
        history.record_submission("info", &Ok::<_, ()>(()));
        assert_eq!(history.navigate(true, "draft"), Some("info".into()));
        history.record_submission("rejected", &Err::<(), _>("queue full"));
        assert_eq!(history.navigate(false, "info"), Some("draft".into()));
        assert_eq!(history.commands.len(), 1);
    }

    #[test]
    fn editing_recalled_command_preserves_the_new_draft() {
        let mut history = CommandHistory::default();
        history.record_submission("info", &Ok::<_, ()>(()));
        assert_eq!(history.navigate(true, "original"), Some("info".into()));
        assert_eq!(history.navigate(true, "edited"), Some("info".into()));
        assert_eq!(history.navigate(false, "info"), Some("edited".into()));
    }

    #[test]
    fn every_utf8_and_ansi_split_preserves_partial_output() {
        let bytes = "hé世界\u{1b}[31m red\u{1b}[0m\r\nnext".as_bytes();
        for split in 0..=bytes.len() {
            let mut terminal = TerminalBuffer::new();
            terminal.push(&bytes[..split]);
            terminal.push(&bytes[split..]);
            terminal.finish();
            assert_eq!(terminal.text(), "hé世界 red\nnext", "split {split}");
        }
    }

    #[test]
    fn split_control_strings_are_discarded_without_buffering() {
        let mut terminal = TerminalBuffer::new();
        for byte in b"a\x1b]0;title\x1b\\b\x1bPpayload\x1b\\c\x1b]x\x07d" {
            terminal.push(&[*byte]);
        }
        assert_eq!(terminal.text(), "abcd");
    }

    #[test]
    fn carriage_return_and_backspace_overwrite_unicode_boundaries() {
        let mut terminal = TerminalBuffer::new();
        terminal.push("世界!\r好\x08新\r".as_bytes());
        terminal.push(b"\nready");
        assert_eq!(terminal.text(), "新界!\nready");
    }

    #[test]
    fn clipping_is_utf8_safe_and_scrollback_is_bounded() {
        let mut terminal = TerminalBuffer::new();
        terminal.push("界".repeat(MAX_LINE_BYTES).as_bytes());
        assert!(terminal.lines.back().unwrap().ends_with(CLIPPED));
        assert!(terminal.lines.back().unwrap().len() <= MAX_LINE_BYTES);
        terminal.push(b"\n");
        for _ in 0..MAX_LINES + 10 {
            terminal.push(b"next\n");
        }
        assert_eq!(terminal.lines.len(), MAX_LINES);
        assert_eq!(terminal.lines.back().unwrap(), "");
    }

    #[test]
    fn finish_replaces_incomplete_utf8_and_clear_resets_all_parser_state() {
        let mut terminal = TerminalBuffer::new();
        terminal.push(&[0xf0, 0x9f]);
        terminal.finish();
        assert_eq!(terminal.text(), "�");
        terminal.push(b"\x1b[");
        let revision = terminal.revision();
        terminal.clear();
        assert_ne!(terminal.revision(), revision);
        terminal.push(b"plain");
        assert_eq!(terminal.text(), "plain");
        terminal.clear();
        terminal.push(&[0xe2, b'A', 0xff]);
        assert_eq!(terminal.text(), "�A�");
    }

    #[test]
    fn invalid_utf8_uses_maximal_subpart_replacement_at_every_split() {
        let bytes = [
            0xe0, 0x80, 0x80, b'x', 0xed, 0xa0, 0x80, 0xf4, 0x90, 0x80, 0x80, 0xe2, 0x82,
        ];
        for split in 0..=bytes.len() {
            let mut terminal = TerminalBuffer::new();
            terminal.push(&bytes[..split]);
            terminal.push(&bytes[split..]);
            terminal.finish();
            assert_eq!(terminal.text(), String::from_utf8_lossy(&bytes));
        }
    }
}
