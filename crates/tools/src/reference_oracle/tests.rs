use super::{Arguments, compiler_recipe, extract, parse_args, trace};
use std::ffi::OsString;
use std::fs;
use std::path::Path;

fn args(values: &[&str]) -> Vec<OsString> {
    values.iter().map(|&value| OsString::from(value)).collect()
}

#[test]
fn extract_matches_only_definitions_and_preserves_nested_bodies() {
    let source = "void wanted(int a);\nstatic ALWAYS_INLINE bool wanted(int a)\n{ if (a) { return true; } return false; }\nint other(void) { return 1; }";
    assert_eq!(
        extract(source, "wanted").expect("definition should be found"),
        "static ALWAYS_INLINE bool wanted(int a)\n{ if (a) { return true; } return false; }"
    );
    assert!(extract(source, "missing").is_err());
    assert!(extract("void broken(void) { if (1) { }", "broken").is_err());
}

#[test]
fn compiler_recipe_keeps_full_sdk_and_logical_argument_order() {
    let actual = compiler_recipe(
        Path::new("${COMPILER}").as_os_str(),
        Path::new("${FIRMWARE}"),
        Some(Path::new("${CMSIS_DSP}")),
        Path::new("${BUILD}"),
    );
    let actual: Vec<String> = actual
        .iter()
        .map(|s| s.to_string_lossy().replace('\\', "/"))
        .collect();
    let provenance: serde_json::Value = serde_json::from_str(include_str!(
        "../../../recording/src/replay/fixtures/cmsis_checkpoints.provenance.json"
    ))
    .expect("committed provenance is valid JSON");
    let expected: Vec<&str> = provenance["compiler_invocation"]["arguments"]
        .as_array()
        .expect("argument array")
        .iter()
        .map(|arg| arg.as_str().expect("argument string"))
        .collect();
    assert_eq!(actual, expected);

    let portable = compiler_recipe(
        Path::new("gcc").as_os_str(),
        Path::new("firmware"),
        None,
        Path::new("build"),
    );
    assert!(!portable.iter().any(|arg| arg == "-D__GNUC_PYTHON__"));
    assert!(portable.iter().any(|arg| arg == "-ffp-contract=off"));
}

#[test]
fn command_line_requires_paths_and_retains_equals_options() {
    assert!(parse_args(&args(&["--firmware", "firmware"])).is_err());
    assert!(parse_args(&args(&["--bogus"])).is_err());
    assert!(matches!(
        parse_args(&args(&["--help"])),
        Ok(Arguments::Help)
    ));
    let Arguments::Run(options) = parse_args(&args(&[
        "--firmware=firmware path",
        "--output",
        "out.csv",
        "--cmsis-dsp",
        "sdk",
        "--compare=base.csv",
    ]))
    .expect("valid options") else {
        panic!("expected run options");
    };
    assert_eq!(options.firmware, Path::new("firmware path"));
    assert_eq!(options.output, Path::new("out.csv"));
    assert_eq!(options.cmsis_dsp.as_deref(), Some(Path::new("sdk")));
    assert_eq!(options.compare.as_deref(), Some(Path::new("base.csv")));
}

#[cfg(unix)]
#[test]
fn command_line_preserves_non_utf8_path_values() {
    use std::os::unix::ffi::OsStringExt;

    let name = OsString::from_vec(b"firmware-\xff".to_vec());
    let values = vec![
        OsString::from("--firmware"),
        name.clone(),
        OsString::from("--output"),
        OsString::from("out.csv"),
    ];
    let Arguments::Run(options) = parse_args(&values).expect("non-UTF-8 value should work") else {
        panic!("expected run options");
    };
    assert_eq!(options.firmware.as_os_str(), name.as_os_str());
}

#[test]
fn missing_actual_firmware_does_not_create_a_trace() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let output = temp.path().join("trace.csv");
    let result = super::run(&[
        OsString::from("--firmware"),
        temp.path().join("missing-firmware").into_os_string(),
        OsString::from("--output"),
        output.clone().into_os_string(),
    ]);
    assert!(result.is_err());
    assert!(!output.exists());
}

#[test]
fn floating_csv_matches_committed_python_checkpoint_representation() {
    let fixture = include_str!("../../../recording/src/replay/fixtures/cmsis_checkpoints.csv");
    for (row, line) in fixture.lines().skip(1).enumerate() {
        for (column, field) in line.split(',').enumerate().skip(2) {
            let value: f64 = field.parse().expect("numeric checkpoint field");
            assert_eq!(
                trace::python_float(value).to_string(),
                field,
                "case row {row}, column {column}"
            );
        }
    }
    assert_eq!(trace::python_float(1e16).to_string(), "1e+16");
    assert_eq!(trace::python_float(f64::NAN).to_string(), "nan");
    assert_eq!(trace::python_float(f64::NEG_INFINITY).to_string(), "-inf");
}

#[test]
fn floating_csv_rounds_actual_midpoint_to_even() {
    // This exactly representable f32-derived value occurs in the full oracle
    // trace, where the two shortest decimal candidates end in 2 and 3.
    let value = 130_765.0_f64 / 131_072.0;
    assert_eq!(trace::python_float(value).to_string(), "0.9976577758789062");
    let checkpoint_value = 4_808_509.0_f64 / 32_768.0;
    assert_eq!(
        trace::python_float(checkpoint_value).to_string(),
        "146.74404907226562"
    );
}

#[test]
fn floating_csv_uses_python_scientific_notation_threshold() {
    assert_eq!(trace::python_float(0.00001).to_string(), "1e-05");
    assert_eq!(trace::python_float(0.000012345).to_string(), "1.2345e-05");
    assert_eq!(trace::python_float(-0.000012345).to_string(), "-1.2345e-05");
    assert_eq!(trace::python_float(0.0001).to_string(), "0.0001");
}

#[test]
fn event_recipe_uses_f64_temperature_and_actual_magnetometer_elapsed_time() {
    let mut last_mag = None;
    let first = trace::event(1, 0, &mut last_mag);
    assert_eq!(first.gyro, [0.12, -0.21, 0.3]);
    assert_eq!(first.acceleration, [0.0; 3]);
    assert_eq!(first.mag, [0.1, 0.3, -0.2]);
    assert_eq!(first.mag_dt, 0.02);
    let second = trace::event(1, 1, &mut last_mag);
    assert_eq!(second.mag, [0.0; 3]);
    assert_eq!(second.mag_dt, 0.01);
    let third = trace::event(1, 2, &mut last_mag);
    assert_eq!(third.mag_dt, 0.02);
    assert_eq!(third.temperature, -15.0 + 2.0 * 0.01);
    assert_eq!(
        trace::event(1, 1600, &mut last_mag).gyro,
        [20.0, 5.0, -10.0]
    );
    assert!(trace::event(2, 0, &mut None).thermal_on);
}

fn row(channels: &[f64; 22]) -> String {
    let mut values = vec!["0".to_owned(), "0".to_owned()];
    values.extend(["0.0"; 10].into_iter().map(str::to_owned));
    values.extend(
        channels
            .iter()
            .map(|&value| trace::python_float(value).to_string()),
    );
    values.join(",")
}

#[test]
fn compare_counts_states_normalizes_poses_and_identifies_baseline_by_hash() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let baseline = temp.path().join("private-name.csv");
    let measured = temp.path().join("trace.csv");
    let mut left = [0.0; 22];
    left[0] = 1.0;
    left[4] = 1.0;
    left[12] = 1.0;
    let mut right = left;
    right[0] = -2.0;
    right[4] = -3.0;
    right[12] = 0.0;
    right[13] = 1.0;
    fs::write(&baseline, format!("header\r\n{}\r\n", row(&left))).expect("baseline");
    fs::write(&measured, format!("header\r\n{}\r\n", row(&right))).expect("measured");
    let comparison = trace::compare(&baseline, &measured).expect("matching input");
    assert_eq!(comparison.rows, 1);
    assert_eq!(comparison.max_abs_s0_through_s21[0], 3.0);
    assert_eq!(comparison.max_pose_degrees_6d_9d, [0.0, 0.0]);
    assert_eq!(comparison.rest_magdist_mismatches, [1, 1]);
    assert_eq!(comparison.baseline.logical_id, "comparison-baseline");
    assert_eq!(
        comparison.baseline.sha256,
        super::file_sha256(&baseline).unwrap()
    );
    assert!(
        !serde_json::to_string(&comparison)
            .unwrap()
            .contains("private-name")
    );
}

#[test]
fn compare_preserves_compensated_quaternion_sums() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let baseline = temp.path().join("baseline.csv");
    let measured = temp.path().join("measured.csv");
    let small = 1.0_f64 / 134_217_728.0;
    let mut left = [0.0; 22];
    let mut right = [0.0; 22];
    for start in [0, 4] {
        left[start..start + 4].copy_from_slice(&[1.0, small, small, small]);
        right[start..start + 4].copy_from_slice(&[1.0, -small, -small, -small]);
    }
    fs::write(&baseline, format!("header\r\n{}\r\n", row(&left))).unwrap();
    fs::write(&measured, format!("header\r\n{}\r\n", row(&right))).unwrap();
    let comparison = trace::compare(&baseline, &measured).expect("matching input");
    let epsilon = f64::EPSILON;
    let expected = (2.0 * ((1.0 - epsilon) / (1.0 + epsilon)).acos()).to_degrees();
    assert!(expected > 0.0);
    for observed in comparison.max_pose_degrees_6d_9d {
        assert_eq!(observed, expected);
    }
}

#[test]
fn compare_rejects_different_inputs_lengths_and_nonfinite_results() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let baseline = temp.path().join("base.csv");
    let measured = temp.path().join("measured.csv");
    let mut channels = [0.0; 22];
    channels[0] = 1.0;
    channels[4] = 1.0;
    let valid = row(&channels);
    fs::write(&baseline, format!("header\r\n{valid}\r\n")).unwrap();
    fs::write(&measured, "header\r\n").unwrap();
    assert!(
        trace::compare(&baseline, &measured)
            .unwrap_err()
            .contains("length")
    );
    fs::write(
        &measured,
        format!("header\r\n{}\r\n", valid.replacen("0,0,", "1,0,", 1)),
    )
    .unwrap();
    assert!(
        trace::compare(&baseline, &measured)
            .unwrap_err()
            .contains("input events")
    );
    fs::write(
        &measured,
        format!(
            "header\r\n{}\r\n{valid}\r\n",
            valid.replacen("0,0,", "1,0,", 1)
        ),
    )
    .unwrap();
    assert!(
        trace::compare(&baseline, &measured)
            .unwrap_err()
            .contains("length")
    );
    let quoted = valid.replacen("0,0,", "\"0\",0,", 1);
    fs::write(&measured, format!("header\r\n{quoted}\r\n")).unwrap();
    assert!(trace::compare(&baseline, &measured).is_ok());
    channels[8] = f64::INFINITY;
    fs::write(&measured, format!("header\r\n{}\r\n", row(&channels))).unwrap();
    assert!(
        trace::compare(&baseline, &measured)
            .unwrap_err()
            .contains("Nonfinite")
    );
    channels[8] = f64::NAN;
    let nan_row = row(&channels);
    fs::write(&baseline, format!("header\r\n{nan_row}\r\n")).unwrap();
    fs::write(&measured, format!("header\r\n{nan_row}\r\n")).unwrap();
    assert!(trace::compare(&baseline, &measured).is_ok());
}

#[test]
#[ignore = "requires actual pinned firmware and full external CMSIS SDK"]
fn actual_external_oracle_matches_frozen_checkpoints_and_compares_portable() {
    let firmware = std::env::var_os("SLIME_REFERENCE_FIRMWARE")
        .expect("set SLIME_REFERENCE_FIRMWARE to the actual firmware root");
    let cmsis = std::env::var_os("SLIME_REFERENCE_CMSIS_DSP")
        .expect("set SLIME_REFERENCE_CMSIS_DSP to the actual SDK cmsis-dsp root");
    let temp = tempfile::tempdir().expect("temporary directory");
    let portable = temp.path().join("portable.csv");
    let measured = temp.path().join("sdk.csv");
    super::run(&[
        OsString::from("--firmware"),
        firmware.clone(),
        OsString::from("--output"),
        portable.clone().into_os_string(),
    ])
    .expect("generate independent portable trace");
    super::run(&[
        OsString::from("--firmware"),
        firmware.clone(),
        OsString::from("--cmsis-dsp"),
        cmsis.clone(),
        OsString::from("--output"),
        measured.clone().into_os_string(),
        OsString::from("--compare"),
        portable.into_os_string(),
    ])
    .expect("generate and compare actual SDK trace");
    let mut full = csv::Reader::from_path(&measured).expect("full trace");
    let full_rows: Vec<_> = full.records().map(|r| r.expect("valid row")).collect();
    assert_eq!(full_rows.len(), 3 * 2400);
    let mut frozen = csv::Reader::from_reader(
        include_bytes!("../../../recording/src/replay/fixtures/cmsis_checkpoints.csv").as_slice(),
    );
    for result in frozen.records() {
        let row = result.expect("checkpoint row");
        let case: usize = row[0].parse().unwrap();
        let index: usize = row[1].parse().unwrap();
        let actual = &full_rows[case * 2400 + index];
        assert!(
            row.iter().take(12).eq(actual.iter().take(12)),
            "input recipe changed for case {case} at index {index}"
        );
        for channel in 0..22 {
            let expected: f64 = row[12 + channel].parse().unwrap();
            let observed: f64 = actual[12 + channel].parse().unwrap();
            let tolerance = if (18..21).contains(&channel) {
                0.02
            } else {
                2e-5
            };
            assert!(
                (expected - observed).abs() <= tolerance,
                "case {case}, index {index}, channel {channel}: {expected} vs {observed}"
            );
        }
    }
    let provenance: serde_json::Value = serde_json::from_slice(
        &fs::read(measured.with_extension("provenance.json")).expect("provenance file"),
    )
    .expect("valid provenance JSON");
    assert_eq!(provenance["comparison"]["rows"], 7200);
    assert_eq!(
        provenance["trace_sha256"],
        super::file_sha256(&measured).unwrap()
    );
    let published = serde_json::to_string(&provenance).unwrap();
    assert!(!published.contains(temp.path().to_string_lossy().as_ref()));
    assert!(!published.contains(firmware.to_string_lossy().as_ref()));
    assert!(!published.contains(cmsis.to_string_lossy().as_ref()));
}
