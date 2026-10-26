use super::{metadata_text, parse_meta};
use crate::capture::Recording;
use crate::capture::format::csv::load_csv;
use crate::capture::format::test_support::{cancel, fixture};
use crate::capture::format::{MAX_META_BYTES, MAX_SAMPLES, validate_report};
use crate::capture::protocol::{CalibrationUpdate, ParsedReport};
use std::collections::BTreeMap;
use std::io::Cursor;
#[test]
fn thermal_chunks_are_indexed_complete_only_and_signed() {
    let mut track = fixture();
    CalibrationUpdate::Thermal {
        flags: 3,
        declared_count: 3,
        min_c: Some(-20.),
        max_c: Some(50.),
        mode: 2,
    }
    .apply(&mut track.calibration);
    let point = crate::capture::ThermalPoint {
        temperature_c: -10.,
        bias_dps: [1., 2., 3.],
    };
    CalibrationUpdate::ThermalChunk {
        index: 1,
        declared_count: 3,
        points: vec![point],
    }
    .apply(&mut track.calibration);
    assert!(!track.calibration.thermal.complete());
    assert!(track.calibration.thermal.points().is_empty());
    let partial = parse_meta(&metadata_text(&track).unwrap(), "partial").unwrap();
    assert_eq!(partial.calibration.thermal, track.calibration.thermal);
    CalibrationUpdate::ThermalChunk {
        index: 0,
        declared_count: 3,
        points: vec![point, point],
    }
    .apply(&mut track.calibration);
    assert!(track.calibration.thermal.complete());
    let restored = parse_meta(&metadata_text(&track).unwrap(), "complete").unwrap();
    assert_eq!(restored.calibration.thermal.points()[0].temperature_c, -10.);
    assert_eq!(restored.calibration.thermal.mode, Some(2));
    assert!(parse_meta("tcal_legacy_offset_dps=1,2,3", "legacy").is_err());
}

// Synthetic legacy sidecar. Its complete curve omits live flags and mode.
const SYNTHETIC_LEGACY_META: &str = "\
source_kind=synthetic
gyro_range_dps=4000
accel_range_g=16
gyro_odr_hz=204.8
accel_odr_hz=102.4
mag_odr_hz=100
imu_id=16
mag_id=2
temp_source=tcal_float_c
duration_s=1
sample_count=1
acc_cal_bias=0.125,-0.25,0.5
acc_cal_matrix=1,0,0,0,1,0,0,0,1
mag_cal_bias=0.25,-0.5,0.75
mag_cal_matrix=1,0,0,0,1,0,0,0,1
gyro_tcal=10:0.25,-0.5,1;11:0.5,-0.25,0.75;12:0.75,0,0.5;13:1,0.25,-0.5
";

#[test]
fn synthetic_legacy_curve_replays_and_roundtrips_without_invented_device_mode() {
    let track = load_csv(
        Cursor::new("seq,gx,gy,gz,ax,ay,az,mx,my,mz,temp\n0,2,3,4,0,0,0,0,0,0,30\n"),
        SYNTHETIC_LEGACY_META,
        "synthetic-legacy-thermal-fixture",
        &cancel(),
        &mut MAX_SAMPLES.clone(),
    )
    .unwrap();
    let thermal = &track.calibration.thermal;
    assert!(thermal.legacy_curve && thermal.complete());
    assert_eq!(thermal.declared_count, 4);
    assert_eq!(thermal.flags, 0);
    assert_eq!(thermal.mode, None);
    assert_eq!(thermal.min_c, None);
    assert_eq!(thermal.max_c, None);
    assert_eq!(thermal.legacy_offset_dps, None);
    assert!(crate::replay::calibration_availability(&track.calibration).gyro_thermal);
    let text = metadata_text(&track).unwrap();
    let restored = parse_meta(&text, "roundtrip").unwrap();
    assert_eq!(restored.calibration, track.calibration);
    let result = crate::replay::process(
        std::sync::Arc::new(Recording {
            tracks: vec![track],
            ..Default::default()
        }),
        0,
        crate::replay::ReplayOptions::default(),
        &cancel(),
        &std::sync::atomic::AtomicUsize::new(0),
    )
    .unwrap();
    assert_eq!(
        result.options.gyro_calibration,
        crate::replay::GyroCalibration::Thermal
    );
    assert!(result.calibration_available.gyro_thermal);
    let applied = result.frames[0].thermal_correction_rad_s.unwrap();
    // Even the nearest point at 13°C has weight 1 / (1 + 17² / 2.5²)
    // below 0.03, so the 30°C query uses that point's bias without a fit.
    for (actual, bias) in applied.into_iter().zip([1f32, 0.25, -0.5]) {
        assert!((actual - bias.to_radians()).abs() < 1e-7);
    }
}

#[test]
fn modern_disabled_partial_or_indexed_metadata_is_not_a_legacy_curve() {
    for extension in [
        "tcal_flags=0\ntcal_mode=0\n",
        "tcal_flags=1\ntcal_mode=1\n",
        "tcal_declared_points=6\ntcal_complete=0\n",
        "tcal_chunks_json={}\n",
    ] {
        let track = parse_meta(&format!("{SYNTHETIC_LEGACY_META}{extension}"), "modern").unwrap();
        assert!(!track.calibration.thermal.legacy_curve);
        assert!(!crate::replay::calibration_availability(&track.calibration).gyro_thermal);
    }
    for extension in [
        "tcal_curve_source=legacy_csv\ntcal_flags=0\n",
        "tcal_curve_source=legacy_csv\ntcal_mode=0\n",
        "tcal_curve_source=legacy_csv\ntcal_min_c=10\n",
        "tcal_curve_source=legacy_csv\ntcal_declared_points=6\n",
        "tcal_curve_source=unknown\n",
    ] {
        assert!(parse_meta(&format!("{SYNTHETIC_LEGACY_META}{extension}"), "conflict").is_err());
    }
}
#[test]
fn complete_empty_thermal_snapshot_roundtrips_marker_and_metadata() {
    let mut track = fixture();
    CalibrationUpdate::Thermal {
        flags: 0,
        declared_count: 0,
        min_c: Some(0.),
        max_c: Some(0.),
        mode: 0,
    }
    .apply(&mut track.calibration);
    let marker = CalibrationUpdate::ThermalChunk {
        index: 0,
        declared_count: 0,
        points: Vec::new(),
    };
    validate_report(&ParsedReport::Calibration {
        slot: track.identity.slot,
        update: marker.clone(),
    })
    .unwrap();
    marker.apply(&mut track.calibration);
    let text = metadata_text(&track).unwrap();
    assert!(text.lines().any(|line| line == "tcal_complete=1"));
    let restored = parse_meta(&text, "empty-thermal").unwrap();
    assert_eq!(restored.calibration.thermal, track.calibration.thermal);
    assert!(restored.calibration.thermal.complete());
    assert!(restored.calibration.thermal.points().is_empty());
}

#[test]
fn metadata_parse_preserves_typed_calibration_and_complete_extra_fields() {
    let track = parse_meta(
        "gyro_odr_hz=204.8\naccel_odr_hz=102.4\nimu_id=9\n\
         gyro_bias=0.25,-0.5,1\ngyro_sens_scale=1,2,0.5\nsynthetic_count=7\n",
        "synthetic-metadata",
    )
    .unwrap();
    assert_eq!(track.metadata.raw_tx_hz, Some(204.8));
    assert_eq!(track.metadata.accel_hz, Some(102.4));
    assert_eq!(track.metadata.imu_id, Some(9));
    assert_eq!(track.calibration.gyro_bias_dps, Some([0.25, -0.5, 1.]));
    assert_eq!(track.calibration.gyro_sensitivity, Some([1., 2., 0.5]));
    assert_eq!(
        track.metadata.extra,
        BTreeMap::from([
            ("gyro_odr_hz".into(), "204.8".into()),
            ("accel_odr_hz".into(), "102.4".into()),
            ("imu_id".into(), "9".into()),
            ("gyro_bias".into(), "0.25,-0.5,1".into()),
            ("gyro_sens_scale".into(), "1,2,0.5".into()),
            ("synthetic_count".into(), "7".into()),
        ])
    );
    let restored = parse_meta(&metadata_text(&track).unwrap(), "roundtrip").unwrap();
    assert_eq!(restored.metadata.raw_tx_hz, Some(204.8));
    assert_eq!(restored.metadata.accel_hz, Some(102.4));
    assert_eq!(restored.metadata.imu_id, Some(9));
    assert_eq!(restored.calibration, track.calibration);
    assert_eq!(restored.metadata.extra["synthetic_count"], "7");
}

#[test]
fn metadata_export_enforces_cumulative_utf8_byte_limit() {
    let mut track = fixture();
    track
        .metadata
        .extra
        .insert("synthetic_a".into(), "x".repeat(MAX_META_BYTES / 2));
    track
        .metadata
        .extra
        .insert("synthetic_µ".into(), String::new());
    let remaining = MAX_META_BYTES - metadata_text(&track).unwrap().len();
    let mut padding = "é".repeat(remaining / 2);
    if !remaining.is_multiple_of(2) {
        padding.push('x');
    }
    track
        .metadata
        .extra
        .insert("synthetic_µ".into(), padding.clone());
    let text = metadata_text(&track).unwrap();
    assert_eq!(text.len(), MAX_META_BYTES);
    let restored = parse_meta(&text, "synthetic-byte-limit").unwrap();
    assert_eq!(restored.metadata.extra["synthetic_µ"], padding);
    track
        .metadata
        .extra
        .get_mut("synthetic_µ")
        .unwrap()
        .push('x');
    assert!(metadata_text(&track).is_err());
}
