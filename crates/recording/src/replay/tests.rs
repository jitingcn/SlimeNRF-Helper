use super::{GyroCalibration, ReplayOptions, ReplayParams, process, recovered_rate};
use crate::capture::{
    AffineCalibration, Gyro, Recording, Sample, SensorMetadata, ThermalPoint, Track,
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize},
};

fn sample(sequence: u16, gyro: Gyro) -> Sample {
    Sample {
        sequence,
        gyro,
        acceleration_g: [0.; 3],
        magnetic_gauss: [0.; 3],
        new_mag: false,
        acceleration_fresh: true,
        temperature_c: None,
        discontinuity: false,
        receiver_ticks: None,
        rssi: None,
    }
}
fn recording(samples: Vec<Sample>) -> Recording {
    Recording {
        tracks: vec![Track {
            samples,
            metadata: SensorMetadata {
                raw_tx_hz: Some(100.),
                accel_hz: Some(100.),
                mag_hz: Some(50.),
                ..Default::default()
            },
            ..Default::default()
        }],
        ..Default::default()
    }
}
fn run(recording: Recording) -> super::ReplayResult {
    run_options(recording, ReplayOptions::default())
}
fn run_options(recording: Recording, options: ReplayOptions) -> super::ReplayResult {
    process(
        Arc::new(recording),
        0,
        options,
        &AtomicBool::new(false),
        &AtomicUsize::new(0),
    )
    .unwrap()
}
#[test]
fn unknown_cumulative_interval_does_not_apply_a_fabricated_bias_rate() {
    let mut r = recording(vec![
        sample(0, Gyro::IntegratedQuat([1., 0., 0., 0.])),
        sample(900, Gyro::IntegratedQuat([1., 0., 0., 0.])),
    ]);
    r.tracks[0].calibration.gyro_bias_dps = Some([0., 0., 10.]);
    r.tracks[0].calibration.gyro_sensitivity = Some([2.; 3]);
    let result = run(r);
    assert_eq!(result.segments.len(), 2);
    for frame in result.frames {
        assert!(!frame.gyro_known);
        assert!(!frame.accel_observed);
        assert!(!frame.mag_observed);
        assert!(frame.thermal_correction_rad_s.is_none());
        assert_eq!(frame.gyro_rad_s, [0.; 3]);
        assert_eq!(frame.q6, [1., 0., 0., 0.]);
        assert_eq!(frame.bias_rad_s, [0.; 3]);
    }
}
#[test]
fn numerical_domain_rejects_finite_extremes_before_cmsis() {
    let process_options = |r, options| {
        process(
            Arc::new(r),
            0,
            options,
            &AtomicBool::new(false),
            &AtomicUsize::new(0),
        )
    };
    let extreme = recording(vec![sample(0, Gyro::RatesDps([1e30, 0., 0.]))]);
    assert!(
        process_options(extreme, ReplayOptions::default())
            .unwrap_err()
            .contains("numerical range")
    );
    let mut huge_scale = recording(vec![sample(0, Gyro::RatesDps([1., 0., 0.]))]);
    huge_scale.tracks[0].calibration.gyro_sensitivity = Some([1e30; 3]);
    assert!(
        process_options(huge_scale, ReplayOptions::default())
            .unwrap_err()
            .contains("numerical range")
    );
    let mut options = ReplayOptions::default();
    options.params.bias_sigma_rest = f32::MIN_POSITIVE;
    let ordinary = recording(vec![sample(0, Gyro::RatesDps([0.; 3]))]);
    assert!(
        process_options(ordinary, options)
            .unwrap_err()
            .contains("parameter")
    );
}
#[test]
fn cumulative_shortest_arc_is_sign_and_scale_invariant() {
    let angle = 0.2f32;
    let rate = recovered_rate(
        [2., 0., 0., 0.],
        [-angle.cos() * 3., 0., 0., -angle.sin() * 3.],
        0.1,
    )
    .unwrap();
    assert!(rate[0].abs() < 1e-5 && rate[1].abs() < 1e-5);
    assert!((rate[2] - 4f32.to_degrees()).abs() < 1e-4);
}
#[test]
fn cumulative_gap_preserves_endpoint_angle_without_future_acceleration() {
    let a = sample(10, Gyro::IntegratedQuat([1., 0., 0., 0.]));
    let mut b = sample(
        14,
        Gyro::IntegratedQuat([0.1f32.cos(), 0., 0., 0.1f32.sin()]),
    );
    b.acceleration_g = [1., 0., 0.];
    let result = run(recording(vec![a, b]));
    assert_eq!(result.frames.len(), 5);
    for frame in &result.frames[1..4] {
        assert_eq!(frame.accel_m_s2, [0.; 3]);
        assert!((frame.gyro_rad_s[2] - 5.).abs() < 1e-5);
    }
    let before_acc = result.frames[3].q6;
    assert!((2. * before_acc[3].atan2(before_acc[0]) - 0.15).abs() < 1e-5);
}
#[test]
fn freshness_and_absence_are_applied_before_affine_calibration() {
    let mut a = sample(0, Gyro::RatesDps([0.; 3]));
    a.acceleration_g = [0., 0., 1.];
    a.acceleration_fresh = false;
    let mut r = recording(vec![a]);
    r.tracks[0].calibration.accel = Some(crate::capture::AffineCalibration {
        bias: [0.1, 0.2, 0.3],
        matrix: [[1., 0., 0.], [0., 1., 0.], [0., 0., 1.]],
    });
    assert_eq!(run(r).frames[0].accel_m_s2, [0.; 3]);
}
#[test]
fn gap_is_causal_and_repeated_acceleration_is_not_deduplicated() {
    let mut a = sample(65535, Gyro::RatesDps([0., 0., 10.]));
    a.acceleration_g = [0., 0., 1.];
    a.temperature_c = Some(-10.);
    let mut b = sample(2, Gyro::RatesDps([0., 0., 20.]));
    b.acceleration_g = [0., 0., 1.];
    b.temperature_c = Some(20.);
    b.magnetic_gauss = [0., 1., 0.];
    b.new_mag = true;
    let result = run(recording(vec![a, b]));
    assert_eq!(result.frames.len(), 4);
    for frame in &result.frames[1..3] {
        assert!(!frame.observed);
        assert!(frame.gyro_known);
        assert!(!frame.accel_observed);
        assert!(!frame.mag_observed);
        assert!(!frame.heading9_valid);
        assert_eq!(frame.accel_m_s2, [0.; 3]);
        assert_eq!(frame.magnetic_gauss, [0.; 3]);
        assert_eq!(frame.temperature_c, Some(-10.));
        assert!((frame.gyro_rad_s[2] - 10f32.to_radians()).abs() < 1e-6);
    }
    assert_eq!(result.frames[0].accel_m_s2, result.frames[3].accel_m_s2);
    assert!(result.frames[3].accel_observed);
    assert!(result.frames[3].mag_observed);
    assert!(result.frames[3].heading9_valid);
}
#[test]
fn unknown_gap_resets_instead_of_integrating_unknown_duration() {
    let r = run(recording(vec![
        sample(1, Gyro::RatesDps([0., 0., 90.])),
        sample(900, Gyro::RatesDps([0., 0., 90.])),
    ]));
    assert_eq!(r.segments.len(), 2);
    assert_eq!(r.frames[0].q6, r.frames[1].q6);
}
#[test]
fn raw_clock_not_fusion_metadata_controls_integrated_angle() {
    let samples = (0..100)
        .map(|i| sample(i, Gyro::RatesDps([0., 0., 90.])))
        .collect();
    let mut r = recording(samples);
    r.tracks[0].metadata.gyro_fusion_hz = Some(400.);
    let r = run(r);
    let q = r.frames.last().unwrap().q6;
    assert!((2. * q[3].atan2(q[0]) - std::f32::consts::FRAC_PI_2).abs() < 1e-5);
}
#[test]
fn explicit_higher_gyro_rate_preserves_integrated_angle() {
    let r = recording(
        (0..100)
            .map(|i| sample(i, Gyro::RatesDps([0., 0., 90.])))
            .collect(),
    );
    let options = ReplayOptions {
        gyro_rate_hz: Some(400.),
        ..Default::default()
    };
    let r = process(
        Arc::new(r),
        0,
        options,
        &AtomicBool::new(false),
        &AtomicUsize::new(0),
    )
    .unwrap();
    let q = r.frames.last().unwrap().q6;
    assert!((2. * q[3].atan2(q[0]) - std::f32::consts::FRAC_PI_2).abs() < 1e-5);
}
#[test]
fn cancellation_and_unknown_required_rates_are_errors() {
    let mut r = recording(vec![sample(1, Gyro::RatesDps([0.; 3]))]);
    r.tracks[0].metadata.raw_tx_hz = None;
    assert!(
        process(
            Arc::new(r),
            0,
            ReplayOptions::default(),
            &AtomicBool::new(false),
            &AtomicUsize::new(0)
        )
        .is_err()
    );
    assert!(
        process(
            Arc::new(recording(vec![sample(1, Gyro::RatesDps([0.; 3]))])),
            0,
            ReplayOptions::default(),
            &AtomicBool::new(true),
            &AtomicUsize::new(0)
        )
        .unwrap_err()
        .contains("cancelled")
    );
}

#[test]
fn independent_calibration_and_frozen_effective_options() {
    let mut row = sample(0, Gyro::RatesDps([3.; 3]));
    row.acceleration_g = [1., 2., 3.];
    row.magnetic_gauss = [1., 2., 3.];
    row.new_mag = true;
    let mut data = recording(vec![row]);
    let snapshot = &mut data.tracks[0].calibration;
    let affine = AffineCalibration {
        bias: [0.5; 3],
        matrix: [[2., 0., 0.], [0., 2., 0.], [0., 0., 2.]],
    };
    snapshot.accel = Some(affine.clone());
    snapshot.mag = Some(affine);
    snapshot.gyro_bias_dps = Some([1.; 3]);
    snapshot.gyro_sensitivity = Some([2.; 3]);
    for calibrate_accel in [false, true] {
        for calibrate_mag in [false, true] {
            for gyro_calibration in [
                GyroCalibration::None,
                GyroCalibration::Bias,
                GyroCalibration::Thermal,
            ] {
                let options = ReplayOptions {
                    calibrate_accel,
                    calibrate_mag,
                    gyro_calibration,
                    ..Default::default()
                };
                let result = run_options(data.clone(), options);
                let frame = &result.frames[0];
                let a = if calibrate_accel {
                    [1., 3., 5.]
                } else {
                    row.acceleration_g
                };
                let m = if calibrate_mag {
                    [1., 3., 5.]
                } else {
                    row.magnetic_gauss
                };
                assert_eq!(frame.accel_m_s2, a.map(|v| v * 9.80665));
                assert_eq!(frame.magnetic_gauss, m);
                let gyro = if gyro_calibration == GyroCalibration::None {
                    3f32
                } else {
                    4.
                };
                assert_eq!(frame.gyro_rad_s, [gyro * (std::f32::consts::PI / 180.); 3]);
                assert!(frame.thermal_correction_rad_s.is_none());
                assert_eq!(result.options.calibrate_accel, calibrate_accel);
                assert_eq!(result.options.calibrate_mag, calibrate_mag);
                assert_eq!(
                    result.options.gyro_calibration,
                    if gyro_calibration == GyroCalibration::None {
                        GyroCalibration::None
                    } else {
                        GyroCalibration::Bias
                    }
                );
                assert_eq!(result.options.raw_rate_hz, Some(100.));
                assert_eq!(result.options.gyro_rate_hz, Some(100.));
                assert_eq!(result.options.accel_rate_hz, Some(100.));
                assert_eq!(result.options.mag_rate_hz, Some(50.));
                assert_eq!(result.timestep_s, 0.01);
                assert_eq!(result.options.params, ReplayParams::default());
                assert_eq!(result.identity, data.tracks[0].identity);
            }
        }
    }
}

#[test]
fn thermal_diagnostics_are_actual_applied_curve_not_static_or_unknown() {
    let mut data = oracle_recording(2);
    let snapshot = &mut data.tracks[0].calibration;
    snapshot.gyro_bias_dps = Some([7.; 3]);
    snapshot.thermal.legacy_offset_dps = Some([0.5; 3]);
    snapshot.thermal.legacy_provenance = Some("recorded legacy offset".into());
    let mut first = sample(0, Gyro::RatesDps([1.; 3]));
    first.temperature_c = Some(-10.);
    let last = sample(2, Gyro::RatesDps([1.; 3]));
    let mut unknown = sample(900, Gyro::IntegratedQuat([1., 0., 0., 0.]));
    unknown.temperature_c = Some(-10.);
    data.tracks[0].samples = vec![first, last, unknown];
    let result = run(data.clone());
    assert_eq!(result.options.gyro_calibration, GyroCalibration::Thermal);
    assert!(result.calibration_available.gyro_thermal);
    for frame in &result.frames[..3] {
        let thermal = frame.thermal_correction_rad_s.unwrap();
        for axis in 0..3 {
            let offset = (-(axis as f32 + 1.) + 0.5) * [1., 1.1, 1.2][axis];
            assert!((thermal[axis] - offset.to_radians()).abs() < 1e-6);
        }
    }
    assert!(!result.frames[1].observed);
    assert!(result.frames[1].gyro_known);
    assert!(!result.frames[3].gyro_known);
    assert!(result.frames[3].thermal_correction_rad_s.is_none());
    let bias = run_options(
        data.clone(),
        ReplayOptions {
            gyro_calibration: GyroCalibration::Bias,
            ..Default::default()
        },
    );
    assert!(
        bias.frames
            .iter()
            .all(|frame| frame.thermal_correction_rad_s.is_none())
    );
    assert!((bias.frames[0].gyro_rad_s[0] - (-6f32).to_radians()).abs() < 1e-6);
    for sample in &mut data.tracks[0].samples {
        sample.temperature_c = None;
    }
    let absent_temperature = run(data);
    assert!(!absent_temperature.calibration_available.gyro_thermal);
    assert_eq!(
        absent_temperature.options.gyro_calibration,
        GyroCalibration::Bias
    );
    assert!(
        absent_temperature
            .frames
            .iter()
            .all(|frame| frame.thermal_correction_rad_s.is_none())
    );
}

#[test]
fn magnetic_heading_validity_resets_and_zero_affine_is_still_an_observation() {
    let mut first = sample(0, Gyro::RatesDps([0.; 3]));
    first.magnetic_gauss = [0., 1., 0.];
    first.new_mag = true;
    let second = sample(1, Gyro::RatesDps([0.; 3]));
    let boundary = sample(900, Gyro::RatesDps([0.; 3]));
    let result = run(recording(vec![first, second, boundary]));
    assert!(result.frames[0].heading9_valid);
    assert!(result.frames[1].heading9_valid);
    assert!(!result.frames[1].mag_observed);
    assert!(!result.frames[2].heading9_valid);
    let mut data = recording(vec![first]);
    data.tracks[0].calibration.mag = Some(AffineCalibration {
        bias: first.magnetic_gauss,
        matrix: [[1., 0., 0.], [0., 1., 0.], [0., 0., 1.]],
    });
    let zero = run(data);
    assert!(zero.frames[0].mag_observed);
    assert_eq!(zero.frames[0].magnetic_gauss, [0.; 3]);
    assert!(!zero.frames[0].heading9_valid);
    let disabled = run_options(
        recording(vec![first]),
        ReplayOptions {
            use_magnetometer: false,
            ..Default::default()
        },
    );
    assert!(!disabled.frames[0].mag_observed);
    assert!(!disabled.frames[0].heading9_valid);
    assert_eq!(disabled.options.mag_rate_hz, None);
}

#[test]
fn full_parameter_bridge_order_and_validation() {
    let defaults = ReplayParams::default();
    assert_eq!(
        defaults.values(),
        [
            1.011, 9., 5., 1.55, 325., 0.1053, 0.0108, 0.25, 2.6, 0.4, 0.1, 0.0001, 1., 1., 1.,
            0.20, 0.10, 5., 10., 15., 5., 10., 0.5, 60., 1000.,
        ]
    );
    assert!(defaults.validate().is_ok());
    let mut params = defaults.clone();
    params.mag_current_tau = -1.;
    assert!(params.validate().is_ok());
    params.mag_current_tau = 0.;
    assert!(params.validate().unwrap_err().contains("magCurrentTau"));
    params.mag_current_tau = f32::NAN;
    assert!(params.validate().is_err());
    params = defaults.clone();
    params.bias_vertical_forgetting_factor = 0.;
    assert!(
        params
            .validate()
            .unwrap_err()
            .contains("biasVerticalForgettingFactor")
    );
    params = defaults.clone();
    params.mag_rejection_factor = 0.;
    assert!(
        params
            .validate()
            .unwrap_err()
            .contains("magRejectionFactor")
    );
    params = defaults;
    params.mag_dip_th = 181.;
    assert!(params.validate().unwrap_err().contains("magDipTh"));
}

#[test]
fn native_feature_toggles_reach_the_c_filter() {
    let data = recording(
        (0..500)
            .map(|sequence| {
                let mut row = sample(sequence, Gyro::RatesDps([0.1; 3]));
                row.acceleration_g = [0., 0., 1.];
                row.magnetic_gauss = [1., 0., 0.];
                row.new_mag = true;
                row
            })
            .collect(),
    );
    assert!(run(data.clone()).frames.last().unwrap().rest);
    let result = run_options(
        data,
        ReplayOptions {
            params: ReplayParams {
                motion_bias_est_enabled: false,
                rest_bias_est_enabled: false,
                mag_dist_rejection_enabled: false,
                mag_current_tau: -1.,
                ..Default::default()
            },
            ..Default::default()
        },
    );
    assert!(
        result
            .frames
            .iter()
            .all(|frame| !frame.rest && frame.bias_rad_s == [0.; 3])
    );
    // Disabled rejection leaves the kernel's cold-start disturbance flag untouched,
    // but actually permits heading correction without accepting a field reference.
    assert!(
        result
            .frames
            .iter()
            .all(|frame| frame.mag_disturbed && frame.mag_ref_norm == 0.)
    );
    assert!(result.frames[0].delta_rad.abs() > 1.);
    assert!(result.profile.contains("custom VQF"));
}

// Input recipe from the independent external reference oracle. Compute temperature
// in f64 before converting it once to f32, preserving the reference order.
fn oracle_sample(case: usize, index: usize) -> Sample {
    let gyro = match case {
        0 => [0.; 3],
        1 if index < 1600 => [0.12, -0.21, 0.3],
        1 => [20., 5., -10.],
        2 => [1., -2., 3.],
        _ => panic!("unknown oracle case"),
    };
    let mut row = sample(index as u16, Gyro::RatesDps(gyro));
    row.acceleration_g = match case {
        0 => [0.000001, 0., -1.],
        1 if index.is_multiple_of(11) => [0.; 3],
        _ => [0., 0., 1.],
    };
    if case == 1 && matches!(index % 7, 0 | 2) {
        row.magnetic_gauss = [0.1, 0.3, -0.2];
        row.new_mag = true;
    }
    row.temperature_c = Some((-15f64 + (index % 90) as f64 * 0.01f64) as f32);
    row
}

fn oracle_recording(case: usize) -> Recording {
    let mut data = recording((0..2400).map(|index| oracle_sample(case, index)).collect());
    if case == 2 {
        let calibration = &mut data.tracks[0].calibration;
        calibration.gyro_sensitivity = Some([1., 1.1, 1.2]);
        calibration.thermal.flags = 3;
        calibration.thermal.mode = Some(2);
        calibration.thermal.declared_count = 4;
        for (index, temps) in [[-20f32, -10.], [10., 20.]].iter().enumerate() {
            calibration.thermal.chunks.insert(
                index as u8,
                temps
                    .iter()
                    .map(|&t| ThermalPoint {
                        temperature_c: t,
                        bias_dps: std::array::from_fn(|j| t * (j + 1) as f32 * 0.1),
                    })
                    .collect(),
            );
        }
    }
    data
}

fn oracle_rows(text: &str) -> [Vec<(Sample, [f32; 22])>; 3] {
    let mut cases = std::array::from_fn(|_| Vec::new());
    for line in text.lines().skip(1) {
        let v: Vec<f32> = line.split(',').map(|s| s.parse().unwrap()).collect();
        assert_eq!(v.len(), 34);
        let mut row = sample(v[1] as u16, Gyro::RatesDps([v[2], v[3], v[4]]));
        row.acceleration_g = [v[5], v[6], v[7]];
        row.magnetic_gauss = [v[8], v[9], v[10]];
        row.new_mag = row.magnetic_gauss != [0.; 3];
        row.temperature_c = Some(v[11]);
        cases[v[0] as usize].push((row, std::array::from_fn(|i| v[12 + i])));
    }
    cases
}

fn diagnostic_channels(frame: &super::ReplayFrame) -> [f32; 22] {
    [
        frame.q6[0],
        frame.q6[1],
        frame.q6[2],
        frame.q6[3],
        frame.q9[0],
        frame.q9[1],
        frame.q9[2],
        frame.q9[3],
        frame.bias_rad_s[0],
        frame.bias_rad_s[1],
        frame.bias_rad_s[2],
        frame.delta_rad,
        u8::from(frame.rest) as f32,
        u8::from(frame.mag_disturbed) as f32,
        frame.mag_norm,
        frame.mag_ref_norm,
        frame.mag_dip_rad,
        frame.mag_ref_dip_rad,
        frame.bias_covariance[0],
        frame.bias_covariance[1],
        frame.bias_covariance[2],
        frame.k_mag_init,
    ]
}

fn compare_oracle(cases: [Vec<(Sample, [f32; 22])>; 3], full_trace: bool) {
    const CHECKPOINTS: [u16; 16] = [
        0, 1, 2, 24, 25, 26, 89, 90, 101, 259, 260, 899, 1599, 1600, 1601, 2399,
    ];
    let mut maximum_error = [0f32; 22];
    let mut maximum_pose_degrees = [0f64; 2];
    let mut compared = 0usize;
    for (case, rows) in cases.iter().enumerate() {
        if full_trace {
            assert_eq!(rows.len(), 2400, "oracle trace truncated");
            assert!(
                rows.iter()
                    .enumerate()
                    .all(|(i, (row, _))| row.sequence as usize == i)
            );
        } else {
            assert_eq!(
                rows.iter().map(|(row, _)| row.sequence).collect::<Vec<_>>(),
                CHECKPOINTS
            );
        }
        // All 2400 real input events are fed even when only checkpoints are frozen.
        let result = run(oracle_recording(case));
        assert_eq!(result.frames.len(), 2400);
        for (row, expected) in rows {
            let index = row.sequence as usize;
            assert_eq!(
                *row,
                oracle_sample(case, index),
                "external input recipe mismatch in case {case} at {index}"
            );
            let actual = diagnostic_channels(&result.frames[index]);
            for channel in 0..22 {
                let error = (actual[channel] - expected[channel]).abs();
                maximum_error[channel] = maximum_error[channel].max(error);
                let tolerance = if (18..21).contains(&channel) {
                    0.02
                } else {
                    2e-5
                };
                assert!(
                    error <= tolerance,
                    "case {case} frame {index} channel {channel}: actual={} golden={} error={error}",
                    actual[channel],
                    expected[channel]
                );
            }
            for (pose, start) in [0, 4].into_iter().enumerate() {
                let q = &actual[start..start + 4];
                let r = &expected[start..start + 4];
                let dot = q
                    .iter()
                    .zip(r)
                    .map(|(&a, &b)| a as f64 * b as f64)
                    .sum::<f64>()
                    .abs();
                let norm = (q.iter().map(|&v| (v as f64).powi(2)).sum::<f64>()
                    * r.iter().map(|&v| (v as f64).powi(2)).sum::<f64>())
                .sqrt();
                let angle = (2. * (dot / norm).clamp(0., 1.).acos()).to_degrees();
                maximum_pose_degrees[pose] = maximum_pose_degrees[pose].max(angle);
            }
            compared += 1;
        }
    }
    eprintln!(
        "Native replay versus external full-SDK CMSIS: {compared} compared states / 7200 evolved events; max_abs_s0..s21={maximum_error:?}; max_pose_degrees_6d_9d={maximum_pose_degrees:?}"
    );
}

#[test]
fn frozen_external_firmware_checkpoints() {
    compare_oracle(
        oracle_rows(include_str!("fixtures/cmsis_checkpoints.csv")),
        false,
    );
}

/// Independent full regeneration command and provenance are in the development guide.
/// Never derive golden outputs from the production replay bridge under test.
#[test]
#[ignore = "requires actual-reference firmware trace via SLIME_REPLAY_ORACLE"]
fn firmware_reference_trace() {
    let path = std::env::var("SLIME_REPLAY_ORACLE")
        .expect("Run the slimenrf-tools reference-oracle command and set SLIME_REPLAY_ORACLE");
    compare_oracle(oracle_rows(&std::fs::read_to_string(path).unwrap()), true);
}
