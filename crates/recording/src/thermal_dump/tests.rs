use super::*;

const TWO: &str = "Dumping 2 collected temperature calibration points:\n--------------------------------------------------\nIndex | Temp (C) | Bias X   | Bias Y   | Bias Z   |\n--------------------------------------------------\n 3 | -12.50 | -0.00123 | 0.00234 | -0.00345\n 69 | 0.25 | 0.00456 | -0.00567 | 0.00678\n--------------------------------------------------\nEnd of dump. Total points printed: 2\n";

fn document(points: &[(f64, f64)]) -> ThermalDump {
    ThermalDump {
        points: points
            .iter()
            .enumerate()
            .map(|(slot, &(temperature, bias))| ThermalDumpPoint {
                slot_index: slot as u16,
                temperature_c: temperature,
                bias_dps: [bias, 2.0 * bias, -bias],
            })
            .collect(),
    }
}

fn close(actual: f64, expected: f64) {
    assert!((actual - expected).abs() < 1e-9, "{actual} != {expected}");
}

#[test]
fn firmware_rows_preserve_sparse_slots_and_signed_values() {
    let parsed = parse_text(TWO).unwrap();
    assert_eq!(parsed.points.len(), 2);
    assert_eq!(parsed.points[0].slot_index, 3);
    assert_eq!(parsed.points[1].slot_index, 69);
    assert_eq!(parsed.points[0].temperature_c, -12.5);
    assert_eq!(parsed.points[0].bias_dps, [-0.00123, 0.00234, -0.00345]);
    assert_eq!(parse_text(&parsed.canonical_text()).unwrap(), parsed);
}

#[test]
fn all_two_chunk_boundaries_and_single_bytes_match_saved_parse() {
    let colored = format!(
        "boot log\r\ntcal dump\r\n\x1b[32m{}\x1b[0m",
        TWO.replace('\n', "\r\n")
    );
    let expected = parse_text(TWO).unwrap();
    for split in 0..=colored.len() {
        let mut decoder = ThermalDumpDecoder::new();
        let result = decoder
            .feed(&colored.as_bytes()[..split])
            .or_else(|| decoder.feed(&colored.as_bytes()[split..]))
            .unwrap_or_else(|| decoder.finish());
        assert_eq!(result.unwrap(), expected, "split {split}");
    }
    let mut decoder = ThermalDumpDecoder::new();
    let mut result = None;
    for byte in colored.bytes() {
        if let Some(outcome) = decoder.feed(&[byte]) {
            assert!(result.replace(outcome).is_none());
        }
    }
    assert_eq!(result.unwrap().unwrap(), expected);
    assert!(decoder.feed(TWO.as_bytes()).is_none());
    assert_eq!(decoder.finish(), Err(ThermalDumpError::AlreadyCompleted));
}

#[test]
fn osc_ansi_utf8_noise_and_no_final_newline_are_supported() {
    let text = format!("\x1b]0;温度\x07log\n\x1b]title\x1b\\{}", TWO.trim_end());
    assert_eq!(parse_text(&text).unwrap(), parse_text(TWO).unwrap());
}

#[test]
fn empty_response_completes_without_dump_marker() {
    let mut decoder = ThermalDumpDecoder::new();
    assert!(
        decoder
            .feed(b"No temperature calibration points have been ")
            .is_none()
    );
    let dump = decoder.feed(b"collected.\r\n").unwrap().unwrap();
    assert!(dump.points.is_empty());
    assert_eq!(parse_text(EMPTY).unwrap(), dump);
    assert_eq!(parse_text(&dump.canonical_text()).unwrap(), dump);
    assert_eq!(parse_text("Dumping 0 collected temperature calibration points:\nEnd of dump. Total points printed: 0").unwrap(), dump);
}

#[test]
fn noise_never_substitutes_for_completion() {
    assert_eq!(
        parse_text("tcal dump\n[log] still booting\n"),
        Err(ThermalDumpError::MissingResponse)
    );
    let missing = TWO.split("End of dump").next().unwrap();
    assert_eq!(parse_text(missing), Err(ThermalDumpError::IncompleteDump));
    assert_eq!(
        parse_text("Command unavailable in this firmware\n"),
        Err(ThermalDumpError::Unsupported)
    );
    assert_eq!(
        parse_text("End of dump. Total points printed: 0\n"),
        Err(ThermalDumpError::UnexpectedRow)
    );
    assert_eq!(
        parse_text("3 | 1 | 2 | 3 | 4\n"),
        Err(ThermalDumpError::UnexpectedRow)
    );
}

#[test]
fn header_final_and_observed_counts_must_agree() {
    assert_eq!(
        parse_text(&TWO.replace("printed: 2", "printed: 1")),
        Err(ThermalDumpError::CountMismatch {
            declared: 2,
            printed: 1,
            parsed: 2
        })
    );
    assert_eq!(
        parse_text(&TWO.replace("Dumping 2", "Dumping 3")),
        Err(ThermalDumpError::CountMismatch {
            declared: 3,
            printed: 2,
            parsed: 2
        })
    );
    assert_eq!(
        parse_text(&TWO.replace(" 69 | 0.25 | 0.00456 | -0.00567 | 0.00678\n", "")),
        Err(ThermalDumpError::CountMismatch {
            declared: 2,
            printed: 2,
            parsed: 1
        })
    );
    assert_eq!(
        parse_text(&TWO.replace("Dumping 2", "Dumping two")),
        Err(ThermalDumpError::MalformedHeader)
    );
    assert_eq!(
        parse_text(&TWO.replace("printed: 2", "printed: two")),
        Err(ThermalDumpError::MalformedTerminator)
    );
    assert_eq!(
        parse_text(&format!(
            "Dumping 1 collected temperature calibration points:\n{TWO}"
        )),
        Err(ThermalDumpError::UnexpectedHeader)
    );
}

#[test]
fn malformed_nonfinite_duplicate_and_out_of_range_rows_fail() {
    for row in [
        "3 | -12.5 | 1 | 2",
        "3 | -12.5 | 1 | 2 | 3 | 4",
        "3 | bad | 1 | 2 | 3",
        "-1 | 1 | 2 | 3 | 4",
        "3 12.5 1 2 3",
    ] {
        let text = TWO.replace("3 | -12.50 | -0.00123 | 0.00234 | -0.00345", row);
        assert!(
            matches!(
                parse_text(&text),
                Err(ThermalDumpError::MalformedRow { .. })
            ),
            "row {row}"
        );
    }
    for number in ["NaN", "inf", "-inf", "1e999"] {
        for field in ["-12.50", "-0.00123", "0.00234", "-0.00345"] {
            assert!(matches!(
                parse_text(&TWO.replacen(field, number, 1)),
                Err(ThermalDumpError::NonFiniteValue { .. })
            ));
        }
    }
    assert_eq!(
        parse_text(&TWO.replace("69 |", "3 |")),
        Err(ThermalDumpError::DuplicateSlot { slot_index: 3 })
    );
    assert_eq!(
        parse_text(&TWO.replace("69 |", "512 |")),
        Err(ThermalDumpError::SlotOutOfBounds { slot_index: 512 })
    );
    // A trailing table separator is valid but not needed by actual firmware.
    assert!(parse_text(&TWO.replace("-0.00345\n", "-0.00345 |\n")).is_ok());
}

#[test]
fn raw_byte_line_and_point_limits_are_enforced_incrementally() {
    assert_eq!(
        parse_text(&format!(
            "Dumping {} collected temperature calibration points:\n",
            MAX_POINTS + 1
        )),
        Err(ThermalDumpError::PointLimit)
    );
    let mut decoder = ThermalDumpDecoder::new();
    assert!(decoder.feed(&vec![b'x'; MAX_LINE_BYTES]).is_none());
    assert_eq!(decoder.feed(b"x"), Some(Err(ThermalDumpError::LineLimit)));
    assert_eq!(decoder.finish(), Err(ThermalDumpError::AlreadyCompleted));
    let mut decoder = ThermalDumpDecoder::new();
    let noise = vec![b'\n'; MAX_DUMP_BYTES];
    assert!(decoder.feed(&noise).is_none());
    assert_eq!(decoder.feed(b"\n"), Some(Err(ThermalDumpError::ByteLimit)));
    let maximum = document(
        &(0..MAX_POINTS)
            .map(|index| (index as f64, 1.0))
            .collect::<Vec<_>>(),
    );
    assert_eq!(parse_text(&maximum.canonical_text()).unwrap(), maximum);
    // Bounds count escape bytes too, so invisible input cannot grow forever.
    let mut decoder = ThermalDumpDecoder::new();
    assert_eq!(
        decoder.feed(&format!("\x1b]{}", "x".repeat(MAX_LINE_BYTES)).into_bytes()),
        Some(Err(ThermalDumpError::LineLimit))
    );
}

#[test]
fn incomplete_escapes_and_invalid_utf8_are_explicit() {
    let mut decoder = ThermalDumpDecoder::new();
    assert!(decoder.feed(b"\x1b[").is_none());
    assert_eq!(decoder.finish(), Err(ThermalDumpError::InvalidEscape));
    let mut decoder = ThermalDumpDecoder::new();
    assert_eq!(
        decoder.feed(b"\xff\n"),
        Some(Err(ThermalDumpError::InvalidUtf8))
    );
    let mut decoder = ThermalDumpDecoder::new();
    assert_eq!(
        decoder.feed(b"\x1b[31\n"),
        Some(Err(ThermalDumpError::InvalidEscape))
    );
}

#[test]
fn empty_single_and_repeated_temperature_analysis_is_defined() {
    let empty = analyze(&ThermalDump::default());
    assert_eq!(empty, ThermalDumpAnalysis::default());
    let single = analyze(&document(&[(-3.0, -0.25)]));
    assert_eq!(single.temperature_range_c, Some([-3.0, -3.0]));
    assert_eq!(single.axes[0].raw, [[-3.0, -0.25]]);
    assert!(single.axes.iter().all(|axis| axis.linear.is_empty()
        && axis.mls.is_empty()
        && axis.statistics.linear_slope.is_none()));
    let repeated = analyze(&document(&[(2.0, 1.0), (2.0, 3.0)]));
    let axis = &repeated.axes[0];
    assert_eq!(axis.statistics.adjacent_slope_range, None);
    close(axis.statistics.linear_slope.unwrap(), 0.0);
    close(axis.statistics.linear_residual_sum_squares.unwrap(), 2.0);
    close(axis.statistics.mls_residual_sum_squares.unwrap(), 2.0);
    assert!(
        axis.linear
            .iter()
            .chain(&axis.mls)
            .all(|&[x, y]| x.is_finite() && (y - 2.0).abs() < 1e-9)
    );
}

#[test]
fn exact_linear_input_has_expected_curves_slopes_and_signed_extrapolation() {
    let points: Vec<_> = [-8.0, -6.0, -4.0, -2.0, 0.0]
        .into_iter()
        .map(|t| (t, 2.0 * t + 3.0))
        .collect();
    let dump = document(&points);
    let before = dump.clone();
    let result = analyze(&dump);
    assert_eq!(dump, before);
    assert_eq!(result.temperature_range_c, Some([-8.0, 0.0]));
    let axis = &result.axes[0];
    close(axis.statistics.linear_slope.unwrap(), 2.0);
    close(axis.statistics.linear_residual_sum_squares.unwrap(), 0.0);
    close(axis.statistics.mls_residual_sum_squares.unwrap(), 0.0);
    assert_eq!(axis.statistics.adjacent_slope_range, Some([2.0, 2.0]));
    close(axis.statistics.bias_range, 16.0);
    assert_eq!(axis.linear.len(), 100);
    assert_eq!(axis.mls.len(), 100);
    assert_eq!(axis.extrapolation_low.len(), 50);
    assert_eq!(axis.extrapolation_high.len(), 50);
    assert_eq!(axis.extrapolation_low.first().unwrap()[0], -14.0);
    assert_eq!(axis.extrapolation_low.last().unwrap()[0], -8.0);
    assert_eq!(axis.extrapolation_high.last().unwrap()[0], 6.0);
    for &[temperature, bias] in axis
        .linear
        .iter()
        .chain(&axis.mls)
        .chain(&axis.extrapolation_low)
        .chain(&axis.extrapolation_high)
    {
        close(bias, 2.0 * temperature + 3.0);
    }
}

#[test]
fn boundary_four_degree_subset_and_global_fallback_are_distinct() {
    let result = analyze(&document(&[
        (0.0, 0.0),
        (2.0, 2.0),
        (4.0, 4.0),
        (10.0, 100.0),
    ]));
    let axis = &result.axes[0];
    close(axis.extrapolation_low.first().unwrap()[1], -6.0);
    // Only one point in the rightmost 4°C: use the GLOBAL fit.
    let global_slope = axis.statistics.linear_slope.unwrap();
    let global_at_maximum = axis.linear.last().unwrap()[1];
    close(
        axis.extrapolation_high.last().unwrap()[1],
        global_at_maximum + 6.0 * global_slope,
    );
}

#[test]
fn residuals_are_sums_of_squares_not_mean_or_root() {
    let result = analyze(&document(&[(-1.0, 1.0), (0.0, 0.0), (1.0, 1.0)]));
    close(result.axes[0].statistics.linear_slope.unwrap(), 0.0);
    close(
        result.axes[0]
            .statistics
            .linear_residual_sum_squares
            .unwrap(),
        2.0 / 3.0,
    );
    close(result.axes[0].statistics.bias_range, 1.0);
}

#[test]
fn repeated_temperature_pairs_do_not_divide_by_zero_in_slope_stats() {
    let result = analyze(&document(&[(1.0, 1.0), (0.0, 0.0), (1.0, 2.0), (3.0, 6.0)]));
    assert_eq!(
        result.axes[0].statistics.adjacent_slope_range,
        Some([1.0, 2.0])
    );
}

#[test]
fn mls_top_ten_cutoff_nearest_single_and_singular_rules() {
    use super::analysis::evaluate_mls_for_test as mls;
    let repeated = document(&[(0.0, 1.0), (0.0, 3.0)]);
    close(mls(&repeated.points, 0, 0.0), 2.0);
    let nearly_repeated = document(&[(0.0, 1.0), (1e-8, 3.0)]);
    close(mls(&nearly_repeated.points, 0, 0.0), 2.0);
    let isolated = document(&[(0.0, 7.0), (100.0, 9.0)]);
    close(mls(&isolated.points, 0, 0.0), 7.0); // one in cutoff
    close(mls(&isolated.points, 0, 80.0), 9.0); // none: nearest
    let mut ten = vec![(0.0, 2.0); 10];
    ten.push((0.0, 1_000.0));
    close(mls(&document(&ten).points, 0, 0.0), 2.0); // source-stable tie, limit10
    let outlier = document(&[(0.0, 1.0), (1.0, 3.0), (15.0, 1e8)]);
    close(mls(&outlier.points, 0, 0.0), 1.0); // 15°C weight below .03
    let curved = document(&[(-2.0, 4.0), (0.0, 0.0), (2.0, 4.0)]);
    // At symmetric query0 slope cancels, leaving the Cauchy-weighted mean.
    let weight = 1.0 / (1.0 + (2.0_f64 / 2.5).powi(2));
    close(
        mls(&curved.points, 0, 0.0),
        8.0 * weight / (1.0 + 2.0 * weight),
    );
}

#[test]
fn fixture_import_analysis_and_canonical_roundtrip_are_real_consumers() {
    let dump = parse_text(include_str!("fixtures/signed_sparse.txt")).unwrap();
    assert_eq!(dump.points.len(), 6);
    assert_eq!(dump.points.first().unwrap().slot_index, 3);
    assert_eq!(dump.points.last().unwrap().slot_index, 69);
    let analysis = analyze(&dump);
    assert_eq!(analysis.temperature_range_c, Some([-12.5, 9.5]));
    close(analysis.axes[0].statistics.linear_slope.unwrap(), 0.0025);
    close(analysis.axes[1].statistics.linear_slope.unwrap(), -0.001);
    close(analysis.axes[2].statistics.linear_slope.unwrap(), 0.002);
    assert_eq!(parse_text(&dump.canonical_text()).unwrap(), dump);
}

#[test]
fn firmware_float32_magnitude_bound_prevents_nonfinite_analysis() {
    for field in ["-12.50", "-0.00123", "0.00234", "-0.00345"] {
        assert!(matches!(
            parse_text(&TWO.replacen(field, "1e308", 1)),
            Err(ThermalDumpError::NumericOutOfRange { .. })
        ));
    }
    let bound = f64::from(f32::MAX);
    let dump = ThermalDump {
        points: vec![
            ThermalDumpPoint {
                slot_index: 0,
                temperature_c: -bound,
                bias_dps: [-bound, bound, 0.0],
            },
            ThermalDumpPoint {
                slot_index: 1,
                temperature_c: bound,
                bias_dps: [bound, -bound, 0.0],
            },
        ],
    };
    let imported = parse_text(&dump.canonical_text()).unwrap();
    let analysis = analyze(&imported);
    for axis in &analysis.axes {
        assert!(axis.statistics.bias_range.is_finite());
        assert!(axis.statistics.linear_slope.unwrap().is_finite());
        assert!(
            axis.statistics
                .linear_residual_sum_squares
                .unwrap()
                .is_finite()
        );
        assert!(
            axis.statistics
                .mls_residual_sum_squares
                .unwrap()
                .is_finite()
        );
        assert!(
            axis.raw
                .iter()
                .chain(&axis.linear)
                .chain(&axis.mls)
                .chain(&axis.extrapolation_low)
                .chain(&axis.extrapolation_high)
                .all(|point| point.iter().all(|value| value.is_finite()))
        );
    }
}

#[test]
fn interleaved_zephyr_logs_do_not_become_points_or_imply_completion() {
    let with_noise = TWO.replace(
        " 69 |",
        "[00:00:12.345] <inf> sensor: status | ready\n 69 |",
    );
    assert_eq!(parse_text(&with_noise).unwrap(), parse_text(TWO).unwrap());
    let truncated = with_noise.split("End of dump").next().unwrap();
    assert_eq!(parse_text(truncated), Err(ThermalDumpError::IncompleteDump));
}

#[test]
fn saved_file_limit_includes_noise_after_a_complete_response() {
    let mut text = TWO.to_owned();
    text.push_str(&"x".repeat(MAX_DUMP_BYTES));
    assert_eq!(parse_text(&text), Err(ThermalDumpError::ByteLimit));
}

#[test]
fn live_unavailable_noise_does_not_hide_following_same_chunk_dump() {
    let mut decoder = ThermalDumpDecoder::new_live_response();
    assert!(
        decoder
            .feed(format!("{UNAVAILABLE}\n").as_bytes())
            .is_none()
    );
    let outcome = decoder.feed(TWO.as_bytes()).unwrap().unwrap();
    assert_eq!(outcome, parse_text(TWO).unwrap());
    let mut decoder = ThermalDumpDecoder::new_live_response();
    let response = format!("{UNAVAILABLE}\n{TWO}");
    assert_eq!(decoder.feed(response.as_bytes()).unwrap().unwrap(), outcome);
    assert_eq!(parse_text(UNAVAILABLE), Err(ThermalDumpError::Unsupported));
}

#[test]
fn live_unavailable_noise_preserves_cumulative_limits_and_incomplete_dump() {
    let mut decoder = ThermalDumpDecoder::new_live_response();
    let line = format!("{UNAVAILABLE}\n");
    for _ in 0..MAX_DUMP_BYTES / line.len() {
        assert!(decoder.feed(line.as_bytes()).is_none());
    }
    assert_eq!(
        decoder.feed(line.as_bytes()),
        Some(Err(ThermalDumpError::ByteLimit))
    );
    let mut decoder = ThermalDumpDecoder::new_live_response();
    assert!(decoder.feed(line.as_bytes()).is_none());
    assert!(
        decoder
            .feed(b"Dumping 1 collected temperature calibration points:\n")
            .is_none()
    );
    assert_eq!(decoder.finish(), Err(ThermalDumpError::IncompleteDump));
}

#[test]
fn seven_equal_temperatures_have_constant_global_and_boundary_fits() {
    for biases in [vec![1.0; 7], vec![-2.0, -1.0, 0.0, 1.0, 2.0, 3.0, 4.0]] {
        let points: Vec<_> = biases.iter().map(|&bias| (2.0, bias)).collect();
        let dump = document(&points);
        let result = analyze(&dump);
        for (axis_index, axis) in result.axes.iter().enumerate() {
            let scale = [1.0, 2.0, -1.0][axis_index];
            let expected_mean = biases.iter().sum::<f64>() / biases.len() as f64 * scale;
            assert_eq!(axis.statistics.linear_slope, Some(0.0));
            assert_eq!(axis.statistics.adjacent_slope_range, None);
            for &[temperature, bias] in axis
                .linear
                .iter()
                .chain(&axis.extrapolation_low)
                .chain(&axis.extrapolation_high)
            {
                assert!(temperature.is_finite());
                close(bias, expected_mean);
            }
            let expected_ssr = biases
                .iter()
                .map(|bias| (bias * scale - expected_mean).powi(2))
                .sum();
            close(
                axis.statistics.linear_residual_sum_squares.unwrap(),
                expected_ssr,
            );
        }
    }
}
