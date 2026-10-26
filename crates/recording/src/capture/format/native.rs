//! Durable append-only native capture events and bounded prefix recovery.
use super::{
    ContextBudget, MAX_FILE_DIAGNOSTICS, MAX_INPUT_BYTES, MAX_LINE_BYTES, MAX_SAMPLES, MAX_TRACKS,
    cancelled, err, validate_identity, validate_report,
};
use crate::capture::protocol::ParsedReport;
use crate::capture::{Recording, Track, TrackIdentity};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

const FORMAT_VERSION: u32 = 1;
const FINAL_EVENT_RESERVE: u64 = 128 * 1024;
#[derive(Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum EventRef<'a> {
    Header {
        format: &'a str,
        version: u32,
        identity: &'a str,
    },
    Track {
        index: usize,
        identity: &'a TrackIdentity,
    },
    Report {
        index: usize,
        report: &'a ParsedReport,
    },
    Warning {
        message: &'a str,
    },
    End {
        samples: u64,
    },
}

#[derive(Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum Event {
    Header {
        format: String,
        version: u32,
        identity: String,
    },
    Track {
        index: usize,
        identity: TrackIdentity,
    },
    Report {
        index: usize,
        report: ParsedReport,
    },
    Warning {
        message: String,
    },
    Boundary {
        index: usize,
        reason: String,
    },
    End {
        samples: u64,
    },
}

#[derive(Clone, Debug)]
pub struct CaptureSummary {
    pub path: PathBuf,
    pub samples: u64,
}

pub struct Recorder {
    writer: BufWriter<File>,
    path: PathBuf,
    tracks: BTreeMap<TrackIdentity, usize>,
    samples: u64,
    pending_rows: usize,
    last_sync: Instant,
    row_buffer: Vec<u8>,
    written_bytes: u64,
    diagnostics: usize,
    context: ContextBudget,
    failed: bool,
}

impl Recorder {
    pub fn create(path: &Path, identity: impl Into<String>) -> Result<Self, String> {
        let identity = identity.into();
        if identity.len() > 16 * 1024 {
            return Err("Capture provenance identity is too long".into());
        }
        let mut context = ContextBudget::new();
        context.add_file(&identity)?;
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(err)?;
        let mut recorder = Self {
            writer: BufWriter::with_capacity(64 * 1024, file),
            path: path.to_owned(),
            tracks: BTreeMap::new(),
            samples: 0,
            pending_rows: 0,
            last_sync: Instant::now(),
            row_buffer: Vec::with_capacity(1024),
            written_bytes: 0,
            diagnostics: 0,
            context,
            failed: false,
        };
        recorder.event(EventRef::Header {
            format: "slimecap",
            version: FORMAT_VERSION,
            identity: &identity,
        })?;
        recorder.sync()?;
        Ok(recorder)
    }

    fn event(&mut self, event: EventRef<'_>) -> Result<(), String> {
        if self.failed {
            return Err("Capture sink failed; retaining its recoverable prefix without appending more events".into());
        }
        let diagnostic = matches!(&event, EventRef::Warning { .. });
        if diagnostic && self.diagnostics >= MAX_FILE_DIAGNOSTICS {
            return Err("Capture diagnostic limit reached".into());
        }
        let mut context = self.context;
        if let EventRef::Warning { message } = &event {
            context.add_file(message)?;
        }
        let finishing = matches!(&event, EventRef::End { .. });
        let limit = if finishing {
            MAX_INPUT_BYTES
        } else if diagnostic {
            MAX_INPUT_BYTES - 128
        } else {
            MAX_INPUT_BYTES - FINAL_EVENT_RESERVE
        };
        self.row_buffer.clear();
        serde_json::to_writer(&mut self.row_buffer, &event).map_err(err)?;
        self.row_buffer.push(b'\n');
        if self.row_buffer.len() > MAX_LINE_BYTES
            || self.written_bytes + self.row_buffer.len() as u64 > limit
        {
            return Err(
                "Capture size limit reached; stop and start a new recording to continue".into(),
            );
        }
        if let Err(error) = self.writer.write_all(&self.row_buffer) {
            self.failed = true;
            return Err(error.to_string());
        }
        self.written_bytes += self.row_buffer.len() as u64;
        self.context = context;
        self.diagnostics += usize::from(diagnostic);
        self.pending_rows += 1;
        if self.pending_rows >= 256 {
            if let Err(error) = self.writer.flush() {
                self.failed = true;
                return Err(error.to_string());
            }
            self.pending_rows = 0;
        }
        if self.last_sync.elapsed() >= Duration::from_secs(1) {
            self.sync()?;
        }
        Ok(())
    }

    fn sync(&mut self) -> Result<(), String> {
        if let Err(error) = self
            .writer
            .flush()
            .and_then(|()| self.writer.get_ref().sync_data())
        {
            self.failed = true;
            return Err(error.to_string());
        }
        self.pending_rows = 0;
        self.last_sync = Instant::now();
        Ok(())
    }

    pub fn register_track(&mut self, identity: TrackIdentity) -> Result<(), String> {
        if self.tracks.contains_key(&identity) {
            return Ok(());
        }
        if self.tracks.len() >= MAX_TRACKS {
            return Err("Capture track limit exceeded".into());
        }
        validate_identity(&identity)?;
        let index = self.tracks.len();
        self.event(EventRef::Track {
            index,
            identity: &identity,
        })?;
        self.tracks.insert(identity, index);
        self.sync()
    }

    pub fn record(
        &mut self,
        identity: &TrackIdentity,
        report: &ParsedReport,
    ) -> Result<(), String> {
        let index = *self
            .tracks
            .get(identity)
            .ok_or("Capture track is not registered")?;
        if identity.slot != report.slot() {
            return Err("Report slot does not match registered identity".into());
        }
        validate_report(report)?;
        if matches!(report, ParsedReport::Sample { .. }) && self.samples >= MAX_SAMPLES as u64 {
            return Err(
                "Capture sample limit reached; stop and start a new recording to continue".into(),
            );
        }
        self.event(EventRef::Report { index, report })?;
        if matches!(report, ParsedReport::Sample { .. }) {
            self.samples += 1;
        }
        Ok(())
    }

    pub fn warning(&mut self, message: &str) -> Result<(), String> {
        if message.len() > 16 * 1024 {
            return Err("Capture warning is too long".into());
        }
        self.event(EventRef::Warning { message })
    }

    pub fn finish(mut self) -> Result<CaptureSummary, String> {
        self.event(EventRef::End {
            samples: self.samples,
        })?;
        self.writer.flush().map_err(err)?;
        self.writer.get_ref().sync_all().map_err(err)?;
        Ok(CaptureSummary {
            path: self.path.clone(),
            samples: self.samples,
        })
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        let _ = self.writer.flush();
        let _ = self.writer.get_ref().sync_data();
    }
}
/// A physical line has a strict allocation bound, even for a missing newline.
fn line(reader: &mut impl BufRead, buffer: &mut Vec<u8>, max: usize) -> Result<bool, String> {
    buffer.clear();
    loop {
        let available = reader.fill_buf().map_err(err)?;
        if available.is_empty() {
            return Ok(!buffer.is_empty());
        }
        let count = available
            .iter()
            .position(|b| *b == b'\n')
            .map_or(available.len(), |i| i + 1);
        if buffer.len() + count > max {
            return Err("Capture input line exceeds size limit".into());
        }
        let ended = available[count - 1] == b'\n';
        buffer.extend_from_slice(&available[..count]);
        reader.consume(count);
        if ended {
            return Ok(true);
        }
    }
}
pub(super) fn load_native(
    mut reader: impl BufRead,
    cancel: &AtomicBool,
) -> Result<Recording, String> {
    let mut recording = Recording::default();
    let mut buffer = Vec::new();
    let mut header = false;
    let mut ended = false;
    let mut total = 0usize;
    let mut rows = 0usize;
    let mut boundaries = BTreeSet::new();
    let mut diagnostics = 0;
    let mut context = ContextBudget::new();
    while line(&mut reader, &mut buffer, MAX_LINE_BYTES)? {
        cancelled(cancel)?;
        total += buffer.len();
        if total as u64 > MAX_INPUT_BYTES {
            return Err("Capture input exceeds size limit".into());
        }
        if ended {
            return Err("Native capture contains data after its end marker".into());
        }
        let event = match serde_json::from_slice::<Event>(&buffer) {
            Ok(event) => event,
            Err(error) if !buffer.ends_with(b"\n") && header => {
                let message = format!(
                    "Recovered partial capture; incomplete last event ignored (JSON {:?} at line {}, column {}).",
                    error.classify(),
                    error.line(),
                    error.column()
                );
                context.add(&message)?;
                recording.warnings.push(message);
                break;
            }
            Err(error) => return Err(format!("Invalid capture event: {error}")),
        };
        if !header && !matches!(&event, Event::Header { .. }) {
            return Err("Native capture is missing its version header".into());
        }
        match event {
            Event::Header {
                format,
                version,
                identity,
            } => {
                if header || format != "slimecap" || version != FORMAT_VERSION {
                    return Err("Unsupported or repeated native capture header".into());
                }
                header = true;
                context.add_file(&identity)?;
                recording.provenance.push(identity);
            }
            Event::Track { index, identity } => {
                if index != recording.tracks.len()
                    || index >= MAX_TRACKS
                    || recording.tracks.iter().any(|t| t.identity == identity)
                {
                    return Err("Invalid or duplicate native capture track index".into());
                }
                validate_identity(&identity)?;
                recording.tracks.push(Track {
                    identity,
                    ..Track::default()
                });
            }
            Event::Report { index, report } => {
                validate_report(&report)?;
                let track = recording
                    .tracks
                    .get_mut(index)
                    .ok_or("Unknown native track index")?;
                if report.slot() != track.identity.slot {
                    return Err("Native report/track identity mismatch".into());
                }
                match report {
                    ParsedReport::Sample { mut sample, .. } => {
                        if rows >= MAX_SAMPLES {
                            return Err("Recording exceeds 5 million observations".into());
                        }
                        rows += 1;
                        sample.discontinuity |= boundaries.remove(&index);
                        track.samples.push(sample);
                    }
                    ParsedReport::Metadata { metadata, .. } => track.metadata = metadata,
                    ParsedReport::Calibration { update, .. } => {
                        update.apply(&mut track.calibration)
                    }
                }
            }
            Event::Warning { message } => {
                if diagnostics >= MAX_FILE_DIAGNOSTICS {
                    return Err("Too many capture warning events".into());
                }
                context.add_file(&message)?;
                diagnostics += 1;
                recording.warnings.push(message);
            }
            Event::Boundary { index, reason } => {
                if index >= recording.tracks.len() {
                    return Err("Unknown boundary track index".into());
                }
                boundaries.insert(index);
                if diagnostics >= MAX_FILE_DIAGNOSTICS {
                    return Err("Too many capture boundaries".into());
                }
                context.add_file(&reason)?;
                diagnostics += 1;
                recording.warnings.push(reason);
            }
            Event::End { samples } => {
                if samples != rows as u64 {
                    return Err("Capture end count does not match observations".into());
                }
                ended = true;
            }
        }
    }
    if !header {
        return Err("Empty native capture".into());
    }
    if !ended {
        let message =
            "Recovered unfinished capture; no durable finish marker was present.".to_owned();
        context.add(&message)?;
        recording.warnings.push(message);
    }
    Ok(recording)
}

#[cfg(test)]
mod tests;
