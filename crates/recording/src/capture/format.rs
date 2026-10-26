//! Versioned, append-only capture and analyzer-compatible CSV/meta interchange.
//! Native rows refer to an interned track index, never repeat USB endpoint paths.
use super::protocol::ParsedReport;
use super::{Gyro, Recording, Sample, TrackIdentity};
use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

mod archive;
mod csv;
mod metadata;
mod native;
#[cfg(test)]
mod test_support;

pub use archive::export_csv_zip;
use archive::load_zip;
use csv::load_csv;
use metadata::read_meta_file;
use native::load_native;
pub use native::{CaptureSummary, Recorder};

pub const MAX_INPUT_BYTES: u64 = 1024 * 1024 * 1024;
pub const MAX_SAMPLES: usize = 5_000_000;
const MAX_TRACKS: usize = 512;
const MAX_META_BYTES: usize = 1024 * 1024;
const MAX_LINE_BYTES: usize = 8 * 1024 * 1024;
const MAX_FILE_DIAGNOSTICS: usize = 4096;
const MAX_RECOVERY_DIAGNOSTICS: usize = 2;
const MAX_CONTEXT_ENTRIES: usize = MAX_FILE_DIAGNOSTICS * 2 + MAX_RECOVERY_DIAGNOSTICS;
const MAX_CONTEXT_BYTES: usize = MAX_META_BYTES / 2;
const RECOVERY_CONTEXT_BYTES: usize = 1024;

#[derive(Clone, Copy)]
struct ContextBudget {
    entries: usize,
    bytes: usize,
}
impl ContextBudget {
    fn new() -> Self {
        Self {
            entries: 0,
            bytes: 4,
        }
    }

    fn add(&mut self, value: &str) -> Result<(), String> {
        let encoded = value
            .bytes()
            .try_fold(3usize, |total, byte| {
                total.checked_add(match byte {
                    b'"' | b'\\' | b'\x08' | b'\t' | b'\n' | b'\x0c' | b'\r' => 2,
                    0..=31 => 6,
                    _ => 1,
                })
            })
            .ok_or("Capture context byte limit exceeded")?;
        let bytes = self
            .bytes
            .checked_add(encoded)
            .ok_or("Capture context byte limit exceeded")?;
        if self.entries >= MAX_CONTEXT_ENTRIES || bytes > MAX_CONTEXT_BYTES {
            return Err("Capture context entry or byte limit exceeded".into());
        }
        self.entries += 1;
        self.bytes = bytes;
        Ok(())
    }

    fn add_file(&mut self, value: &str) -> Result<(), String> {
        self.add(value)?;
        if self.bytes > MAX_CONTEXT_BYTES - RECOVERY_CONTEXT_BYTES {
            return Err("Capture file context exceeds byte limit reserved for recovery".into());
        }
        Ok(())
    }
}

fn context_budget(recording: &Recording) -> Result<ContextBudget, String> {
    let mut budget = ContextBudget::new();
    for value in recording.warnings.iter().chain(&recording.provenance) {
        budget.add(value)?;
    }
    Ok(budget)
}

struct ContextValues(Vec<String>);

impl<'de> serde::Deserialize<'de> for ContextValues {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Values;
        impl<'de> serde::de::Visitor<'de> for Values {
            type Value = ContextValues;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a bounded list of capture context strings")
            }

            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut sequence: A,
            ) -> Result<Self::Value, A::Error> {
                let mut values = Vec::new();
                while values.len() < MAX_CONTEXT_ENTRIES {
                    let Some(value) = sequence.next_element::<String>()? else {
                        return Ok(ContextValues(values));
                    };
                    values.push(value);
                }
                if sequence.next_element::<serde::de::IgnoredAny>()?.is_some() {
                    return Err(serde::de::Error::custom(
                        "Capture context entry limit exceeded",
                    ));
                }
                Ok(ContextValues(values))
            }
        }
        deserializer.deserialize_seq(Values)
    }
}

fn err(error: impl std::fmt::Display) -> String {
    error.to_string()
}
fn cancelled(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) {
        Err("Cancelled".into())
    } else {
        Ok(())
    }
}

fn validate_identity(identity: &TrackIdentity) -> Result<(), String> {
    if identity.receiver.is_empty()
        || identity.receiver.len() > 16 * 1024
        || identity
            .rf_address
            .as_ref()
            .is_some_and(|rf| rf.len() > 256)
    {
        Err("Invalid recording track identity".into())
    } else {
        Ok(())
    }
}

fn validate_sample(sample: &Sample) -> Result<(), String> {
    let gyro: &[f32] = match &sample.gyro {
        Gyro::RatesDps(v) => v,
        Gyro::IntegratedQuat(q) => {
            if q.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() <= 1e-20 {
                return Err("Zero integrated quaternion".into());
            }
            q
        }
    };
    if gyro
        .iter()
        .chain(sample.acceleration_g.iter())
        .chain(sample.magnetic_gauss.iter())
        .any(|v| !v.is_finite())
        || sample.temperature_c.is_some_and(|v| !v.is_finite())
    {
        return Err("Non-finite capture observation".into());
    }
    Ok(())
}

fn validate_report(report: &ParsedReport) -> Result<(), String> {
    match report {
        ParsedReport::Sample { sample, .. } => validate_sample(sample),
        ParsedReport::Metadata { metadata, .. } => {
            for value in [
                metadata.gyro_range_dps,
                metadata.accel_range_g,
                metadata.raw_tx_hz,
                metadata.gyro_chip_hz,
                metadata.gyro_fusion_hz,
                metadata.accel_hz,
                metadata.mag_hz,
            ]
            .into_iter()
            .flatten()
            {
                if !value.is_finite() || value <= 0. {
                    return Err("Invalid positive sensor metadata value".into());
                }
            }
            if metadata
                .extra
                .iter()
                .map(|(k, v)| k.len() + v.len())
                .sum::<usize>()
                > MAX_META_BYTES
            {
                return Err("Sensor metadata too large".into());
            }
            Ok(())
        }
        ParsedReport::Calibration { update, .. } => {
            use super::protocol::CalibrationUpdate;
            let valid = match update {
                CalibrationUpdate::Accel(v) | CalibrationUpdate::Mag(v) => v
                    .bias
                    .iter()
                    .chain(v.matrix.iter().flatten())
                    .all(|v| v.is_finite()),
                CalibrationUpdate::Gyro {
                    bias_dps,
                    sensitivity,
                } => bias_dps.iter().chain(sensitivity).all(|v| v.is_finite()),
                CalibrationUpdate::Thermal {
                    declared_count,
                    min_c,
                    max_c,
                    mode,
                    ..
                } => {
                    *declared_count <= 512
                        && *mode <= 2
                        && min_c.iter().chain(max_c).all(|v| v.is_finite())
                }
                CalibrationUpdate::ThermalChunk {
                    index,
                    declared_count,
                    points,
                } => {
                    (*declared_count == 0 && *index == 0 && points.is_empty())
                        || (*declared_count <= 512
                            && !points.is_empty()
                            && points.len() <= 2
                            && usize::from(*index) * 2 + points.len()
                                <= usize::from(*declared_count)
                            && points.iter().all(|p| {
                                p.temperature_c.is_finite()
                                    && p.bias_dps.iter().all(|v| v.is_finite())
                            }))
                }
            };
            if valid {
                Ok(())
            } else {
                Err("Invalid calibration snapshot".into())
            }
        }
    }
}

pub fn load(path: &Path, cancel: &AtomicBool) -> Result<Recording, String> {
    cancelled(cancel)?;
    let path = if path.extension().is_some_and(|extension| extension == "txt")
        && path
            .with_extension("")
            .extension()
            .is_some_and(|extension| extension == "meta")
    {
        path.with_extension("").with_extension("csv")
    } else {
        path.to_owned()
    };
    let metadata = std::fs::metadata(&path).map_err(err)?;
    if !metadata.is_file() || metadata.len() > MAX_INPUT_BYTES {
        return Err("Input is not a regular file or exceeds 1 GiB".into());
    }
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("slimecap") => load_native(BufReader::new(File::open(&path).map_err(err)?), cancel),
        Some("csv") => {
            let meta_path = path.with_extension("meta.txt");
            let meta = if meta_path.exists() {
                read_meta_file(&meta_path)?
            } else {
                String::new()
            };
            let mut remaining = MAX_SAMPLES;
            let track = load_csv(
                BufReader::new(File::open(&path).map_err(err)?),
                &meta,
                &path.to_string_lossy(),
                cancel,
                &mut remaining,
            )?;
            let mut recording = Recording { tracks: vec![track], provenance: vec![format!("CSV import: {}", path.display())], warnings: vec!["CSV sampling times are reconstructed from sequence and declared raw TX rate, not receiver arrival time.".into()] };
            restore_context(&mut recording)?;
            Ok(recording)
        }
        Some("zip") => load_zip(&path, cancel),
        _ => Err("Choose a .slimecap, .csv, .meta.txt or paired CSV .zip recording".into()),
    }
}

fn restore_context(recording: &mut Recording) -> Result<(), String> {
    if recording
        .tracks
        .iter()
        .any(|track| track.metadata.extra.contains_key("capture_warnings_json"))
    {
        recording.warnings.clear();
    }
    if recording
        .tracks
        .iter()
        .any(|track| track.metadata.extra.contains_key("capture_provenance_json"))
    {
        recording.provenance.clear();
    }
    let mut budget = context_budget(recording)?;
    for track in &recording.tracks {
        for (key, target) in [
            ("capture_warnings_json", &mut recording.warnings),
            ("capture_provenance_json", &mut recording.provenance),
        ] {
            if let Some(text) = track.metadata.extra.get(key) {
                if text.len() > MAX_CONTEXT_BYTES {
                    return Err("Capture context byte limit exceeded".into());
                }
                let ContextValues(mut values) = serde_json::from_str(text).map_err(err)?;
                let mut existing = std::collections::BTreeMap::<&str, usize>::new();
                for value in target.iter() {
                    *existing.entry(value).or_default() += 1;
                }
                let mut failure = None;
                values.retain(|value| {
                    if let Some(count) = existing.get_mut(value.as_str())
                        && *count != 0
                    {
                        *count -= 1;
                        return false;
                    }
                    if let Err(error) = budget.add(value) {
                        failure = Some(error);
                        return false;
                    }
                    true
                });
                if let Some(error) = failure {
                    return Err(error);
                }
                drop(existing);
                target.append(&mut values);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
