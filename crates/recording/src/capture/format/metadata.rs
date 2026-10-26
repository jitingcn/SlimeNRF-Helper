//! Analyzer metadata, calibration snapshots, and explicit thermal provenance.
use super::{MAX_META_BYTES, err, validate_identity, validate_report};
use crate::capture::protocol::{CalibrationUpdate, ParsedReport};
use crate::capture::{AffineCalibration, Gyro, SensorMetadata, ThermalPoint, Track};
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::path::Path;
pub(super) fn read_meta_file(path: &Path) -> Result<String, String> {
    let metadata = std::fs::metadata(path).map_err(err)?;
    if !metadata.is_file() || metadata.len() > MAX_META_BYTES as u64 {
        return Err("Metadata file exceeds limit or is not regular".into());
    }
    let mut text = String::new();
    File::open(path)
        .map_err(err)?
        .take(MAX_META_BYTES as u64 + 1)
        .read_to_string(&mut text)
        .map_err(err)?;
    if text.len() > MAX_META_BYTES {
        return Err("Metadata file grew beyond size limit".into());
    }
    Ok(text)
}
fn numbers<const N: usize>(value: &str) -> Result<[f32; N], String> {
    let mut out = [0.; N];
    let mut fields = value.split(',');
    for dst in &mut out {
        *dst = finite_number(fields.next().ok_or("Calibration vector is too short")?)?;
    }
    if fields.next().is_some() {
        return Err("Calibration vector is too long".into());
    }
    Ok(out)
}
pub(super) fn finite_number(value: &str) -> Result<f32, String> {
    let value = value.trim().parse::<f32>().map_err(err)?;
    if value.is_finite() {
        Ok(value)
    } else {
        Err("Non-finite numeric input".into())
    }
}

pub(super) fn parse_meta(text: &str, source: &str) -> Result<Track, String> {
    if text.len() > MAX_META_BYTES {
        return Err("Metadata exceeds size limit".into());
    }
    let mut fields = BTreeMap::new();
    for row in text
        .lines()
        .map(str::trim)
        .filter(|row| !row.is_empty() && !row.starts_with('#'))
    {
        let (key, value) = row.split_once('=').ok_or("Malformed metadata key/value")?;
        if fields
            .insert(key.trim().to_owned(), value.trim().to_owned())
            .is_some()
        {
            return Err(format!("Duplicate metadata key: {key}"));
        }
    }
    let mut track = Track::default();
    track.identity.receiver = fields
        .get("receiver_identity")
        .cloned()
        .unwrap_or_else(|| format!("import:{source}"));
    if let Some(value) = fields.get("tracker_id") {
        track.identity.slot = value.parse().map_err(err)?;
    }
    track.identity.rf_address = fields.get("rf_address").cloned();
    validate_identity(&track.identity)?;
    let positive = |key: &str| -> Result<Option<f32>, String> {
        fields
            .get(key)
            .map(|value| finite_number(value).map(|n| (n > 0.).then_some(n)))
            .transpose()
            .map(Option::flatten)
    };
    track.metadata = SensorMetadata {
        gyro_range_dps: positive("gyro_range_dps")?,
        accel_range_g: positive("accel_range_g")?,
        raw_tx_hz: positive("gyro_odr_hz")?,
        gyro_chip_hz: positive("gyro_chip_odr_hz")?,
        gyro_fusion_hz: positive("gyro_fusion_odr_hz")?,
        accel_hz: positive("accel_odr_hz")?,
        mag_hz: positive("mag_odr_hz")?,
        imu_id: fields
            .get("imu_id")
            .map(|v| v.parse())
            .transpose()
            .map_err(err)?,
        mag_id: fields
            .get("mag_id")
            .map(|v| v.parse())
            .transpose()
            .map_err(err)?,
        extra: BTreeMap::new(),
    };
    for (prefix, target) in [
        ("acc", &mut track.calibration.accel),
        ("mag", &mut track.calibration.mag),
    ] {
        match (
            fields.get(&format!("{prefix}_cal_bias")),
            fields.get(&format!("{prefix}_cal_matrix")),
        ) {
            (Some(b), Some(m)) => {
                let matrix: [f32; 9] = numbers(m)?;
                *target = Some(AffineCalibration {
                    bias: numbers(b)?,
                    matrix: [
                        matrix[0..3].try_into().unwrap(),
                        matrix[3..6].try_into().unwrap(),
                        matrix[6..9].try_into().unwrap(),
                    ],
                });
            }
            (None, None) => {}
            _ => return Err("Incomplete affine calibration metadata".into()),
        }
    }
    track.calibration.gyro_bias_dps = fields.get("gyro_bias").map(|v| numbers(v)).transpose()?;
    track.calibration.gyro_sensitivity = fields
        .get("gyro_sens_scale")
        .map(|v| numbers(v))
        .transpose()?;
    let thermal = &mut track.calibration.thermal;
    thermal.flags = fields
        .get("tcal_flags")
        .map(|v| v.parse())
        .transpose()
        .map_err(err)?
        .unwrap_or(0);
    thermal.mode = fields
        .get("tcal_mode")
        .map(|v| v.parse())
        .transpose()
        .map_err(err)?;
    if thermal.mode.is_some_and(|v| v > 2) {
        return Err("Unknown current thermal mode".into());
    }
    thermal.min_c = fields
        .get("tcal_min_c")
        .map(|v| finite_number(v))
        .transpose()?;
    thermal.max_c = fields
        .get("tcal_max_c")
        .map(|v| finite_number(v))
        .transpose()?;
    thermal.declared_count = fields
        .get("tcal_declared_points")
        .map(|v| v.parse())
        .transpose()
        .map_err(err)?
        .unwrap_or(0);
    if thermal.declared_count > 512 {
        return Err("Thermal point count exceeds wire limit".into());
    }
    if let Some(value) = fields.get("gyro_tcal") {
        let mut points = Vec::new();
        for value in value.split(';').filter(|v| !v.is_empty()) {
            let (temperature, bias) = value.split_once(':').ok_or("Malformed thermal point")?;
            if points.len() >= 512 {
                return Err("Too many thermal points".into());
            }
            points.push(ThermalPoint {
                temperature_c: finite_number(temperature)?,
                bias_dps: numbers(bias)?,
            });
        }
        if !fields.contains_key("tcal_declared_points") {
            thermal.declared_count = points.len() as u16;
        }
        if points.len() > usize::from(thermal.declared_count) {
            return Err("Thermal point count mismatch".into());
        }
        for (i, points) in points.chunks(2).enumerate() {
            thermal.chunks.insert(i as u8, points.to_vec());
        }
    }
    // Native extension retains indexed incomplete chunks rather than claiming a curve.
    if let Some(value) = fields.get("tcal_chunks_json") {
        thermal.chunks = serde_json::from_str(value).map_err(err)?;
        for (index, points) in &thermal.chunks {
            let report = ParsedReport::Calibration {
                slot: track.identity.slot,
                update: CalibrationUpdate::ThermalChunk {
                    index: *index,
                    declared_count: thermal.declared_count,
                    points: points.clone(),
                },
            };
            validate_report(&report)?;
        }
    }
    if fields.get("tcal_complete").is_some_and(|v| v == "0") && thermal.complete() {
        return Err("Contradictory thermal completeness metadata".into());
    }
    thermal.legacy_curve = match fields.get("tcal_curve_source").map(String::as_str) {
        Some("legacy_csv") => {
            if ["tcal_flags", "tcal_mode", "tcal_min_c", "tcal_max_c"]
                .iter()
                .any(|key| fields.contains_key(*key))
                || !fields.contains_key("gyro_tcal")
                || !thermal.complete()
            {
                return Err("Legacy CSV thermal curve conflicts with device mode or incomplete curve metadata".into());
            }
            true
        }
        Some(_) => return Err("Unknown thermal curve source".into()),
        None => {
            // The legacy/web CSV exporter provides the complete curve but no
            // live mode, flags, or LUT range. Preserve that distinct provenance;
            // never infer an enabled device mode from partial/modern metadata.
            fields.contains_key("gyro_tcal")
                && !fields.keys().any(|key| key.starts_with("tcal_"))
                && thermal.complete()
        }
    };
    if let Some(value) = fields.get("tcal_legacy_offset_dps") {
        let provenance = fields
            .get("tcal_legacy_provenance")
            .filter(|v| !v.is_empty())
            .ok_or("Legacy thermal offset requires explicit provenance")?;
        thermal.legacy_offset_dps = Some(numbers(value)?);
        thermal.legacy_provenance = Some(provenance.clone());
    }
    track.metadata.extra = fields;
    Ok(track)
}
fn join_numbers(values: impl IntoIterator<Item = f32>) -> String {
    values
        .into_iter()
        .map(|v| v.to_string())
        .collect::<Vec<_>>()
        .join(",")
}
pub(super) fn metadata_text(track: &Track) -> Result<String, String> {
    let mut values: BTreeMap<Cow<'_, str>, Cow<'_, str>> = track
        .metadata
        .extra
        .iter()
        .map(|(key, value)| (key.as_str().into(), value.as_str().into()))
        .collect();
    values.remove("capture_warnings_json");
    values.remove("capture_provenance_json");
    values.insert("tracker_id".into(), track.identity.slot.to_string().into());
    values.insert(
        "receiver_identity".into(),
        track.identity.receiver.as_str().into(),
    );
    if let Some(rf) = &track.identity.rf_address {
        values.insert("rf_address".into(), rf.as_str().into());
    }
    for (key, value) in [
        ("gyro_range_dps", track.metadata.gyro_range_dps),
        ("accel_range_g", track.metadata.accel_range_g),
        ("gyro_odr_hz", track.metadata.raw_tx_hz),
        ("gyro_chip_odr_hz", track.metadata.gyro_chip_hz),
        ("gyro_fusion_odr_hz", track.metadata.gyro_fusion_hz),
        ("accel_odr_hz", track.metadata.accel_hz),
        ("mag_odr_hz", track.metadata.mag_hz),
    ] {
        if let Some(value) = value {
            values.insert(key.into(), value.to_string().into());
        }
    }
    for (key, value) in [
        ("imu_id", track.metadata.imu_id),
        ("mag_id", track.metadata.mag_id),
    ] {
        if let Some(value) = value {
            values.insert(key.into(), value.to_string().into());
        }
    }
    values.insert(
        "data_mode".into(),
        if track
            .samples
            .first()
            .is_some_and(|s| matches!(s.gyro, Gyro::IntegratedQuat(_)))
        {
            "gyr_quat"
        } else {
            "raw"
        }
        .into(),
    );
    values.insert("temp_source".into(), "tcal_float_c".into());
    for (prefix, calibration) in [
        ("acc", &track.calibration.accel),
        ("mag", &track.calibration.mag),
    ] {
        if let Some(calibration) = calibration {
            values.insert(
                format!("{prefix}_cal_bias").into(),
                join_numbers(calibration.bias).into(),
            );
            values.insert(
                format!("{prefix}_cal_matrix").into(),
                join_numbers(calibration.matrix.into_iter().flatten()).into(),
            );
        }
    }
    if let Some(v) = track.calibration.gyro_bias_dps {
        values.insert("gyro_bias".into(), join_numbers(v).into());
    }
    if let Some(v) = track.calibration.gyro_sensitivity {
        values.insert("gyro_sens_scale".into(), join_numbers(v).into());
    }
    let thermal = &track.calibration.thermal;
    if thermal.legacy_curve {
        values.insert("tcal_curve_source".into(), "legacy_csv".into());
        for key in ["tcal_flags", "tcal_mode", "tcal_min_c", "tcal_max_c"] {
            values.remove(key);
        }
    } else {
        values.remove("tcal_curve_source");
        values.insert("tcal_flags".into(), thermal.flags.to_string().into());
    }
    values.insert(
        "tcal_declared_points".into(),
        thermal.declared_count.to_string().into(),
    );
    values.insert(
        "tcal_complete".into(),
        u8::from(thermal.complete()).to_string().into(),
    );
    if !thermal.legacy_curve {
        if let Some(v) = thermal.mode {
            values.insert("tcal_mode".into(), v.to_string().into());
        }
        if let Some(v) = thermal.min_c {
            values.insert("tcal_min_c".into(), v.to_string().into());
        }
        if let Some(v) = thermal.max_c {
            values.insert("tcal_max_c".into(), v.to_string().into());
        }
    }
    if thermal.complete() {
        values.insert(
            "gyro_tcal".into(),
            thermal
                .points()
                .iter()
                .map(|p| format!("{}:{}", p.temperature_c, join_numbers(p.bias_dps)))
                .collect::<Vec<_>>()
                .join(";")
                .into(),
        );
    } else {
        values.remove("gyro_tcal");
    }
    if !thermal.chunks.is_empty() {
        values.insert(
            "tcal_chunks_json".into(),
            serde_json::to_string(&thermal.chunks).map_err(err)?.into(),
        );
    }
    if let Some(v) = thermal.legacy_offset_dps {
        let provenance = thermal
            .legacy_provenance
            .as_ref()
            .ok_or("Legacy thermal offset lacks provenance")?;
        values.insert("tcal_legacy_offset_dps".into(), join_numbers(v).into());
        values.insert("tcal_legacy_provenance".into(), provenance.as_str().into());
    }
    values.insert(
        "sample_count".into(),
        track.samples.len().to_string().into(),
    );
    let mut gaps = 0u64;
    let mut periods = u64::from(!track.samples.is_empty());
    let mut unknown = false;
    for pair in track.samples.windows(2) {
        let delta = pair[1].sequence.wrapping_sub(pair[0].sequence);
        if pair[1].discontinuity || delta == 0 || delta > 501 {
            unknown = true;
        } else {
            gaps += u64::from(delta - 1);
            periods += u64::from(delta);
        }
    }
    values.insert("gap_count".into(), gaps.to_string().into());
    if !unknown {
        if let Some(hz) = track.metadata.raw_tx_hz {
            values.insert(
                "duration_s".into(),
                (periods as f64 / f64::from(hz)).to_string().into(),
            );
        }
    } else {
        values.remove("duration_s");
        values.insert("unknown_duration_segments".into(), "1".into());
    }
    let mut text = String::new();
    for (key, value) in values {
        if key.contains(['\n', '\r', '=']) || value.contains(['\n', '\r']) {
            return Err("Metadata cannot contain embedded line breaks".into());
        }
        let line_end = text
            .len()
            .checked_add(key.len())
            .and_then(|len| len.checked_add(value.len()))
            .and_then(|len| len.checked_add(2))
            .ok_or("Export metadata exceeds limit")?;
        if line_end > MAX_META_BYTES {
            return Err("Export metadata exceeds limit".into());
        }
        text.push_str(&key);
        text.push('=');
        text.push_str(&value);
        text.push('\n');
    }
    Ok(text)
}

#[cfg(test)]
mod tests;
