//! Continuous raw ingestion, bounded live previews, and metadata repair selection.
use super::super::io::CaptureIo;
use super::super::{CapturePhase, CaptureSnapshot};
use super::{PREVIEW, Session, WARMUP};
use parking_lot::Mutex;
use slimenrf_recording::capture::protocol::{self, ParsedReport};
use std::sync::Arc;
use std::time::Duration;

impl Session<'_> {
    pub(super) fn warn(&mut self, warning: String) {
        if self.snapshot.warnings.len() < 64 && !self.snapshot.warnings.contains(&warning) {
            self.snapshot.warnings.push(warning);
            self.snapshot_dirty = true;
        }
    }
    fn ingest(&mut self, event: Result<ParsedReport, String>) -> Result<(), String> {
        self.snapshot_dirty = true;
        self.snapshot.counters.reports += 1;
        let event = match event {
            Ok(event) => event,
            Err(_) => {
                self.snapshot.counters.malformed += 1;
                return Ok(());
            }
        };
        let slot = match &event {
            ParsedReport::Sample { slot, .. }
            | ParsedReport::Metadata { slot, .. }
            | ParsedReport::Calibration { slot, .. } => *slot,
        };
        if !self.snapshot.tracks.iter().any(|t| t.identity.slot == slot) {
            self.identity_failed = true;
            return Err(format!(
                "Unexpected raw slot {slot}; frozen capture source changed"
            ));
        }
        let track = self
            .snapshot
            .tracks
            .iter_mut()
            .find(|t| t.identity.slot == slot)
            .expect("checked slot");
        if !self.sink_failed {
            if let Err(error) = self
                .recorder
                .as_mut()
                .expect("active recorder")
                .record(&track.identity, &event)
            {
                self.sink_failed = true;
                return Err(format!("Recording write failed: {error}"));
            }
            if matches!(event, ParsedReport::Sample { .. }) {
                self.recorded_samples += 1;
            }
        }
        match event {
            ParsedReport::Sample { sample, .. } => {
                self.snapshot.counters.samples += 1;
                if let Some(previous) = self.last_seq[slot as usize] {
                    let delta = sample.sequence.wrapping_sub(previous);
                    if delta > 1 && delta < 0x8000 {
                        self.snapshot.counters.sequence_gaps += u64::from(delta - 1);
                    }
                    if delta > 0 && delta < 0x8000 {
                        self.last_seq[slot as usize] = Some(sample.sequence);
                    }
                } else {
                    self.last_seq[slot as usize] = Some(sample.sequence);
                }
                track.samples += 1;
                if track.latest.len() == PREVIEW {
                    track.latest.pop_front();
                }
                track.latest.push_back(sample);
            }
            ParsedReport::Metadata { metadata, .. } => track.metadata = metadata,
            ParsedReport::Calibration { update, .. } => update.apply(&mut track.calibration),
        }
        Ok(())
    }
    pub(super) fn pump_data(&mut self, io: &mut impl CaptureIo) -> Result<(), String> {
        if self.data_failed {
            return Ok(());
        }
        let mut bytes = [0; 4096];
        let n = match io.read_data(&mut bytes) {
            Ok(n) => n,
            Err(e) => {
                self.data_failed = true;
                return Err(e);
            }
        };
        // After a positive reassignment, drain bytes but never attribute them to the old identity.
        if n == 0 || self.identity_failed {
            return Ok(());
        }
        if io.cdc() {
            let mut parser = std::mem::take(&mut self.parser);
            let mut failure = None;
            parser.feed(&bytes[..n], |event| {
                if failure.is_none()
                    && let Err(error) = self.ingest(event)
                {
                    failure = Some(error);
                }
            });
            self.parser = parser;
            if let Some(error) = failure {
                return Err(error);
            }
        } else {
            self.ingest(protocol::parse_hid(&bytes[..n]))?;
        }
        Ok(())
    }
    pub(super) fn publish(&mut self, io: &impl CaptureIo, shared: &Mutex<Arc<CaptureSnapshot>>) {
        if io.now().saturating_sub(self.last_publish) < Duration::from_millis(100)
            && !matches!(
                self.snapshot.phase,
                CapturePhase::Finished | CapturePhase::Failed
            )
        {
            return;
        }
        // Preflight may repeatedly observe the same registration without changing UI data.
        // Once started, elapsed/rate/warmup legitimately advance, bounded to ten publications/s.
        if !self.snapshot_dirty
            && self.start.is_none()
            && shared.lock().phase == self.snapshot.phase
        {
            return;
        }
        self.last_publish = io.now();
        if let Some(start) = self.start {
            self.snapshot.elapsed = io.now().saturating_sub(start);
            self.snapshot.rate_hz = self.snapshot.counters.samples as f64
                / self.snapshot.elapsed.as_secs_f64().max(0.001);
            if !matches!(
                self.snapshot.phase,
                CapturePhase::Starting
                    | CapturePhase::Stopping
                    | CapturePhase::Failed
                    | CapturePhase::Finished
            ) {
                self.snapshot.phase = if self.snapshot.counters.samples > 0 {
                    CapturePhase::Receiving
                } else if self.snapshot.elapsed < WARMUP {
                    CapturePhase::WarmingUp
                } else {
                    CapturePhase::WaitingForSamples
                };
            }
        }
        *shared.lock() = Arc::new(self.snapshot.clone());
        self.snapshot_dirty = false;
    }
    pub(super) fn missing_request(&mut self) -> Option<[u8; 3]> {
        for track in &self.snapshot.tracks {
            if track.samples == 0 {
                continue;
            }
            let slot = track.identity.slot;
            let cal = &track.calibration;
            let missing = [
                (1, track.metadata.raw_tx_hz.is_none()),
                (2, cal.accel.is_none()),
                (4, cal.mag.is_none()),
                (
                    8,
                    cal.gyro_bias_dps.is_none() || cal.gyro_sensitivity.is_none(),
                ),
                (16, cal.thermal.mode.is_none()),
            ];
            for (mask, absent) in missing {
                if absent && self.repairs.insert((slot, mask, 255)) {
                    return Some([slot, mask, 255]);
                }
            }
            let chunks = usize::from(cal.thermal.declared_count).div_ceil(2).min(256);
            for chunk in 0..chunks {
                let chunk = chunk as u8;
                let expected_entries =
                    (usize::from(cal.thermal.declared_count) - usize::from(chunk) * 2).min(2);
                if cal
                    .thermal
                    .chunks
                    .get(&chunk)
                    .is_none_or(|points| points.len() != expected_entries)
                    && self.repairs.insert((slot, 32, chunk))
                {
                    return Some([slot, 32, chunk]);
                }
            }
        }
        None
    }
}
