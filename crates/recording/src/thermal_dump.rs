//! Human-readable `tcal dump` analysis, independent of recordings and live metadata.
//!
//! Values are rounded firmware console output, not a lossless calibration backup.
//! Limits are host policy, not a claim about every firmware's configured slot count.

mod analysis;
#[cfg(test)]
mod tests;

pub use analysis::{AxisAnalysis, AxisStatistics, ThermalDumpAnalysis, analyze};

use std::fmt;

pub const MAX_POINTS: usize = 512;
pub const MAX_LINE_BYTES: usize = 4_096;
pub const MAX_DUMP_BYTES: usize = 256 * 1_024;

const HEADER_PREFIX: &str = "Dumping ";
const HEADER_SUFFIX: &str = " collected temperature calibration points:";
const END_PREFIX: &str = "End of dump. Total points printed: ";
const EMPTY: &str = "No temperature calibration points have been collected.";
const UNAVAILABLE: &str = "Command unavailable in this firmware";
const COLUMN_HEADER: &str = "Index | Temp (C) | Bias X   | Bias Y   | Bias Z   |";

#[derive(Clone, Debug, PartialEq)]
pub struct ThermalDumpPoint {
    /// Retained firmware slot ID; never renumbered after sorting or export.
    pub slot_index: u16,
    pub temperature_c: f64,
    pub bias_dps: [f64; 3],
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ThermalDump {
    /// Original firmware traversal order, not temperature order.
    pub points: Vec<ThermalDumpPoint>,
}

impl ThermalDump {
    /// Canonical human dump; shortest round-trip decimal values preserve imported
    /// numbers. This is still not the separate firmware `tcal export` format.
    pub fn canonical_text(&self) -> String {
        use std::fmt::Write as _;
        if self.points.is_empty() {
            return format!("{EMPTY}\n");
        }
        let mut text = format!(
            "Dumping {} collected temperature calibration points:\n--------------------------------------------------\n{COLUMN_HEADER}\n--------------------------------------------------\n",
            self.points.len()
        );
        for point in &self.points {
            // Writing a String cannot fail.
            let _ = writeln!(
                text,
                " {} | {} | {} | {} | {}",
                point.slot_index,
                point.temperature_c,
                point.bias_dps[0],
                point.bias_dps[1],
                point.bias_dps[2]
            );
        }
        let _ = writeln!(
            text,
            "--------------------------------------------------\n{END_PREFIX}{}",
            self.points.len()
        );
        text
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ThermalDumpError {
    ByteLimit,
    LineLimit,
    PointLimit,
    InvalidUtf8,
    InvalidEscape,
    MissingResponse,
    AlreadyCompleted,
    IncompleteDump,
    Unsupported,
    MalformedHeader,
    MalformedTerminator,
    UnexpectedHeader,
    UnexpectedRow,
    MalformedRow {
        line: usize,
    },
    NonFiniteValue {
        line: usize,
    },
    NumericOutOfRange {
        line: usize,
    },
    SlotOutOfBounds {
        slot_index: usize,
    },
    DuplicateSlot {
        slot_index: u16,
    },
    CountMismatch {
        declared: usize,
        printed: usize,
        parsed: usize,
    },
}

impl fmt::Display for ThermalDumpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ByteLimit => write!(f, "dump exceeds {MAX_DUMP_BYTES} input bytes"),
            Self::LineLimit => write!(f, "dump line exceeds {MAX_LINE_BYTES} input bytes"),
            Self::PointLimit => write!(f, "dump exceeds the host limit of {MAX_POINTS} points"),
            Self::InvalidUtf8 => f.write_str("dump contains invalid UTF-8"),
            Self::InvalidEscape => {
                f.write_str("dump contains an incomplete or invalid ANSI escape")
            }
            Self::MissingResponse => f.write_str("no temperature calibration dump response found"),
            Self::AlreadyCompleted => f.write_str("dump outcome was already delivered by feed"),
            Self::IncompleteDump => {
                f.write_str("temperature calibration dump has no final terminator")
            }
            Self::Unsupported => {
                f.write_str("temperature calibration command is unavailable in this firmware")
            }
            Self::MalformedHeader => f.write_str("malformed temperature calibration dump header"),
            Self::MalformedTerminator => {
                f.write_str("malformed temperature calibration dump terminator")
            }
            Self::UnexpectedHeader => {
                f.write_str("a second dump header appeared before completion")
            }
            Self::UnexpectedRow => {
                f.write_str("a dump row or terminator appeared before its header")
            }
            Self::MalformedRow { line } => write!(f, "malformed calibration point at line {line}"),
            Self::NonFiniteValue { line } => {
                write!(f, "non-finite calibration value at line {line}")
            }
            Self::NumericOutOfRange { line } => write!(
                f,
                "calibration value at line {line} exceeds firmware float32 magnitude"
            ),
            Self::SlotOutOfBounds { slot_index } => write!(
                f,
                "firmware slot {slot_index} exceeds host slot limit {MAX_POINTS}"
            ),
            Self::DuplicateSlot { slot_index } => write!(f, "duplicate firmware slot {slot_index}"),
            Self::CountMismatch {
                declared,
                printed,
                parsed,
            } => write!(
                f,
                "dump counts disagree: header {declared}, final {printed}, parsed {parsed}"
            ),
        }
    }
}

impl std::error::Error for ThermalDumpError {}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Escape {
    #[default]
    Text,
    Start,
    Csi,
    Osc,
    OscTerminator,
}

/// Incremental, bounded console decoder. Feed the original worker bytes before
/// any lossy display queue. Exactly one terminal outcome is emitted by `feed`;
/// later bytes are ignored. Timeouts and cancellation belong to the caller.
#[derive(Clone, Debug)]
pub struct ThermalDumpDecoder {
    bytes: usize,
    line_bytes: usize,
    line_number: usize,
    line: Vec<u8>,
    escape: Escape,
    declared: Option<usize>,
    points: Vec<ThermalDumpPoint>,
    seen: [bool; MAX_POINTS],
    outcome: Option<Result<ThermalDump, ThermalDumpError>>,
    emitted: bool,
    ignore_unsupported_before_header: bool,
}

impl Default for ThermalDumpDecoder {
    fn default() -> Self {
        Self {
            bytes: 0,
            line_bytes: 0,
            line_number: 0,
            line: Vec::new(),
            escape: Escape::Text,
            declared: None,
            points: Vec::new(),
            seen: [false; MAX_POINTS],
            outcome: None,
            emitted: false,
            ignore_unsupported_before_header: false,
        }
    }
}
impl ThermalDumpDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Live console streams can contain an earlier command's generic
    /// unavailable response, which cannot be attributed to this dump request.
    /// Ignore it before a dump header; the caller's original deadline remains
    /// authoritative. All input bounds remain cumulative. Ordinary saved-text
    /// parsing uses `new` and still returns `Unsupported` immediately.
    pub fn new_live_response() -> Self {
        Self {
            ignore_unsupported_before_header: true,
            ..Self::default()
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Option<Result<ThermalDump, ThermalDumpError>> {
        if self.emitted {
            return None;
        }
        for &byte in bytes {
            if let Err(error) = self.consume(byte) {
                self.outcome = Some(Err(error));
            }
            if let Some(outcome) = self.outcome.take() {
                self.emitted = true;
                return Some(outcome);
            }
        }
        None
    }

    /// If `feed` already delivered the result, returns `AlreadyCompleted`.
    /// Finish a saved file or closed stream, including its last unterminated
    /// text line. A started dump is never accepted merely because EOF arrived.
    pub fn finish(mut self) -> Result<ThermalDump, ThermalDumpError> {
        if self.emitted {
            return Err(ThermalDumpError::AlreadyCompleted);
        }
        if self.escape != Escape::Text {
            return Err(ThermalDumpError::InvalidEscape);
        }
        if !self.line.is_empty() {
            self.finish_line()?;
        }
        match self.outcome {
            Some(outcome) => outcome,
            None => Err(if self.declared.is_some() {
                ThermalDumpError::IncompleteDump
            } else {
                ThermalDumpError::MissingResponse
            }),
        }
    }

    fn consume(&mut self, byte: u8) -> Result<(), ThermalDumpError> {
        self.bytes += 1;
        if self.bytes > MAX_DUMP_BYTES {
            return Err(ThermalDumpError::ByteLimit);
        }
        if byte == b'\n' {
            if self.escape != Escape::Text {
                return Err(ThermalDumpError::InvalidEscape);
            }
            self.line_bytes = 0;
            return self.finish_line();
        }
        self.line_bytes += 1;
        if self.line_bytes > MAX_LINE_BYTES {
            return Err(ThermalDumpError::LineLimit);
        }
        match self.escape {
            Escape::Text => match byte {
                0x1b => self.escape = Escape::Start,
                b'\r' => {}
                _ => self.line.push(byte),
            },
            Escape::Start => {
                self.escape = match byte {
                    b'[' => Escape::Csi,
                    b']' => Escape::Osc,
                    0x40..=0x5f => Escape::Text,
                    _ => return Err(ThermalDumpError::InvalidEscape),
                }
            }
            Escape::Csi => match byte {
                0x40..=0x7e => self.escape = Escape::Text,
                0x20..=0x3f => {}
                _ => return Err(ThermalDumpError::InvalidEscape),
            },
            Escape::Osc => match byte {
                7 => self.escape = Escape::Text,
                0x1b => self.escape = Escape::OscTerminator,
                _ => {}
            },
            Escape::OscTerminator => match byte {
                b'\\' => self.escape = Escape::Text,
                _ => return Err(ThermalDumpError::InvalidEscape),
            },
        }
        Ok(())
    }

    fn finish_line(&mut self) -> Result<(), ThermalDumpError> {
        self.line_number += 1;
        let bytes = std::mem::take(&mut self.line);
        let line = std::str::from_utf8(&bytes)
            .map_err(|_| ThermalDumpError::InvalidUtf8)?
            .trim();
        let result = self.parse_line(line);
        self.line = bytes;
        self.line.clear();
        result
    }

    fn parse_line(&mut self, line: &str) -> Result<(), ThermalDumpError> {
        if line == UNAVAILABLE {
            if self.ignore_unsupported_before_header && self.declared.is_none() {
                return Ok(());
            }
            return Err(ThermalDumpError::Unsupported);
        }
        if line == EMPTY {
            if self.declared.is_some() {
                return Err(ThermalDumpError::UnexpectedHeader);
            }
            self.outcome = Some(Ok(ThermalDump::default()));
            return Ok(());
        }
        if line.starts_with(HEADER_PREFIX) && line.contains("temperature calibration") {
            if self.declared.is_some() {
                return Err(ThermalDumpError::UnexpectedHeader);
            }
            let count = line
                .strip_prefix(HEADER_PREFIX)
                .and_then(|text| text.strip_suffix(HEADER_SUFFIX))
                .and_then(|text| parse_count(text).ok())
                .ok_or(ThermalDumpError::MalformedHeader)?;
            if count > MAX_POINTS {
                return Err(ThermalDumpError::PointLimit);
            }
            self.declared = Some(count);
            return Ok(());
        }
        if line.starts_with("End of dump") {
            let declared = self.declared.ok_or(ThermalDumpError::UnexpectedRow)?;
            let printed = line
                .strip_prefix(END_PREFIX)
                .and_then(|text| parse_count(text).ok())
                .ok_or(ThermalDumpError::MalformedTerminator)?;
            if printed != declared || printed != self.points.len() {
                return Err(ThermalDumpError::CountMismatch {
                    declared,
                    printed,
                    parsed: self.points.len(),
                });
            }
            self.outcome = Some(Ok(ThermalDump {
                points: std::mem::take(&mut self.points),
            }));
            return Ok(());
        }
        if line.is_empty() || line.bytes().all(|byte| byte == b'-') {
            return Ok(());
        }
        if is_column_header(line) {
            return if self.declared.is_some() {
                Ok(())
            } else {
                Err(ThermalDumpError::UnexpectedRow)
            };
        }
        // Zephyr console log records may interleave with a console command,
        // including messages containing pipes. They cannot be firmware rows,
        // whose first field is a decimal slot ID.
        if line.starts_with('[') && line.contains("] ") {
            return Ok(());
        }
        // Console echoes/logging without a row delimiter can be interleaved.
        // Any pipe-delimited non-header line is a row candidate, never silently
        // skipped. Likewise, a numeric-looking damaged row inside a dump fails.
        if line.contains('|') || (self.declared.is_some() && looks_numeric(line)) {
            if self.declared.is_none() {
                return Err(ThermalDumpError::UnexpectedRow);
            }
            return self.parse_row(line);
        }
        Ok(())
    }

    fn parse_row(&mut self, line: &str) -> Result<(), ThermalDumpError> {
        let malformed = || ThermalDumpError::MalformedRow {
            line: self.line_number,
        };
        let mut fields = line.split('|').map(str::trim);
        let slot_index =
            parse_count(fields.next().ok_or_else(malformed)?).map_err(|_| malformed())?;
        if slot_index >= MAX_POINTS {
            return Err(ThermalDumpError::SlotOutOfBounds { slot_index });
        }
        let mut values = [0.0; 4];
        for value in &mut values {
            *value = fields
                .next()
                .ok_or_else(malformed)?
                .parse::<f64>()
                .map_err(|_| malformed())?;
            if !value.is_finite() {
                return Err(ThermalDumpError::NonFiniteValue {
                    line: self.line_number,
                });
            }
            // Firmware stores these fields as float32. Do not invent physical
            // temperature/bias limits, but reject magnitudes it cannot emit.
            if value.abs() > f64::from(f32::MAX) {
                return Err(ThermalDumpError::NumericOutOfRange {
                    line: self.line_number,
                });
            }
        }
        // Firmware data rows omit the trailing separator; accept one for saved
        // tables, but never accept additional fields or missing values.
        if fields.next().is_some_and(|field| !field.is_empty()) || fields.next().is_some() {
            return Err(malformed());
        }
        let slot_index = u16::try_from(slot_index).map_err(|_| malformed())?;
        if self.seen[usize::from(slot_index)] {
            return Err(ThermalDumpError::DuplicateSlot { slot_index });
        }
        if self.points.len() == MAX_POINTS {
            return Err(ThermalDumpError::PointLimit);
        }
        self.seen[usize::from(slot_index)] = true;
        self.points.push(ThermalDumpPoint {
            slot_index,
            temperature_c: values[0],
            bias_dps: [values[1], values[2], values[3]],
        });
        Ok(())
    }
}

fn parse_count(text: &str) -> Result<usize, ()> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(());
    }
    text.parse().map_err(|_| ())
}

fn looks_numeric(line: &str) -> bool {
    line.as_bytes()
        .first()
        .is_some_and(|byte| byte.is_ascii_digit() || matches!(byte, b'+' | b'-'))
}

fn is_column_header(line: &str) -> bool {
    let mut fields = line.split('|').map(str::trim);
    fields.next() == Some("Index")
        && fields.next() == Some("Temp (C)")
        && fields.next() == Some("Bias X")
        && fields.next() == Some("Bias Y")
        && fields.next() == Some("Bias Z")
        && fields.next().is_none_or(str::is_empty)
        && fields.next().is_none()
}

pub fn parse_text(text: &str) -> Result<ThermalDump, ThermalDumpError> {
    if text.len() > MAX_DUMP_BYTES {
        return Err(ThermalDumpError::ByteLimit);
    }
    let mut decoder = ThermalDumpDecoder::new();
    if let Some(outcome) = decoder.feed(text.as_bytes()) {
        outcome
    } else {
        decoder.finish()
    }
}
