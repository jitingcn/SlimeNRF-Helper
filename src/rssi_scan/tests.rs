use super::{Recommendation, Sample, ScanDecoder, ScanReport, parse_log};

const HEADER: &str =
    "RSSI scan (ESB channels 0..88): samples/ch/sweep=50, sweeps=200 (total=10000 samples/ch)";
const TABLE: &str = "All channels sorted by RSSI (lower is better):";
const FIXTURE: &str = include_str!("fixtures/receiver_scan.log");

fn table_decoder() -> ScanDecoder {
    let mut decoder = ScanDecoder::new();
    assert!(decoder.feed_line(HEADER).is_none());
    assert!(decoder.feed_line(TABLE).is_none());
    decoder
}

fn sample(channel: u8, rssi_dbm: i16) -> Sample {
    Sample { channel, rssi_dbm }
}

#[test]
fn actual_sparse_receiver_example_retains_source_metadata_and_numeric_order() {
    let report = parse_log(FIXTURE).unwrap();
    assert_eq!(report.channel_range, (0, 88));
    assert_eq!(report.current_channel, Some(84));
    assert_eq!(
        report.recommendation,
        Recommendation::Channel(sample(86, -69))
    );
    assert_eq!(
        report.samples,
        [
            (0, -42),
            (2, -22),
            (6, -25),
            (8, -43),
            (10, -34),
            (12, -31),
            (14, -32),
            (16, -28),
            (18, -29),
            (20, -32),
            (24, -24),
            (28, -27),
            (30, -27),
            (32, -23),
            (34, -32),
            (36, -43),
            (38, -34),
            (40, -32),
            (42, -26),
            (44, -33),
            (46, -26),
            (48, -42),
            (50, -22),
            (52, -23),
            (54, -27),
            (56, -25),
            (58, -24),
            (60, -25),
            (62, -41),
            (64, -40),
            (66, -29),
            (68, -28),
            (70, -28),
            (72, -36),
            (74, -36),
            (76, -31),
            (78, -47),
            (82, -28),
            (84, -61),
            (86, -69),
            (88, -66),
        ]
        .map(|(channel, rssi_dbm)| sample(channel, rssi_dbm))
    );
    assert!(report.excluded_channels.is_empty());
}

#[test]
fn lower_magnitudes_padding_and_historical_range_are_source_values() {
    let report = parse_log(&format!(
        "RSSI scan (ESB channels 1..100): samples/ch/sweep=20, sweeps=100 (total=2000 samples/ch)\nCurrent channel (effective): 99\nRecommended: channel 100 (- 5 dBm)\n{TABLE}\n100 (- 5 dBm)  2 (-0 dBm)  1 (-127 dBm)"
    ))
    .unwrap();
    assert_eq!(report.channel_range, (1, 100));
    assert_eq!(report.current_channel, Some(99));
    assert_eq!(
        report.samples,
        [sample(1, -127), sample(2, 0), sample(100, -5)]
    );
    assert_eq!(
        report.recommendation,
        Recommendation::Channel(sample(100, -5))
    );
}

#[test]
fn snapshots_only_change_on_validated_observations_and_remain_immutable() {
    let mut decoder = table_decoder();
    assert!(decoder.feed_line("rssi_scan").is_none());
    let first = decoder.feed_line("86 (-69 dBm)").unwrap();
    assert_eq!(first.scan_id, 1);
    assert!(decoder.feed_line("86 (-69 dBm) 86 (-69 dBm)").is_none());
    assert!(decoder.feed_line(TABLE).is_none());
    assert!(
        decoder
            .feed_line("[00:00:19.017,578] <inf> esb_event: Total TPS: 0")
            .is_none()
    );
    let next = decoder.feed_line("2 (-22 dBm)").unwrap();
    assert_eq!(next.scan_id, first.scan_id);
    assert_eq!(first.report.samples, [sample(86, -69)]);
    assert_eq!(next.report.samples, [sample(2, -22), sample(86, -69)]);
    let metadata = decoder
        .feed_line("Current channel (effective): 84")
        .unwrap();
    assert_eq!(metadata.report.current_channel, Some(84));
    assert!(
        decoder
            .feed_line("Current channel (effective): 84")
            .is_none()
    );
}

#[test]
fn ansi_and_control_strings_span_lines_without_exposing_hidden_measurements() {
    let mut decoder = ScanDecoder::new();
    assert!(
        decoder
            .feed_line(&format!("\x1b[32m{HEADER}\x1b[0m"))
            .is_none()
    );
    assert!(decoder.feed_line("RSSI scan started").is_none());
    assert!(decoder.feed_line(TABLE).is_none());
    assert!(decoder.feed_line("\x1b]window title").is_none());
    assert!(decoder.feed_line("88 (-1 dBm)").is_none());
    assert!(decoder.feed_line(HEADER).is_none());
    assert!(decoder.feed_line("\x1b\\").is_none());
    assert!(decoder.feed_line("\x1bPignored device control").is_none());
    assert!(decoder.feed_line("10 (-1 dBm)").is_none());
    assert!(decoder.feed_line("\x1b\\\x1b[3").is_none());
    let result = decoder.feed_line("2m86 (-69 dBm)\x1b[0m").unwrap();
    assert_eq!(result.scan_id, 1);
    assert_eq!(result.report.samples, [sample(86, -69)]);
}

#[test]
fn recognized_log_suffix_is_accepted_but_payload_is_not_mined() {
    let mut decoder = table_decoder();
    let result = decoder
        .feed_line("86 (-69 dBm) [00:00:19.017,578] <inf> esb_event: 88 (-1 dBm)")
        .unwrap();
    assert_eq!(result.report.samples, [sample(86, -69)]);
    assert!(
        decoder
            .feed_line("[00:00:20.018,035] <inf> esb_event: 88 (-1 dBm)")
            .is_none()
    );
    assert!(
        decoder
            .feed_line("some application log 88 (-1 dBm)")
            .is_none()
    );
    assert_eq!(
        decoder
            .feed_line("2 (-22 dBm)")
            .unwrap()
            .report
            .samples
            .len(),
        2
    );
}

#[test]
fn malformed_or_conflicting_row_invalidates_whole_frame_until_new_header() {
    for bad in [
        "2 (-22 dBm) 88 (-128 dBm)",
        "2 (-22 dBm) 89 (-40 dBm)",
        "2 (-22 dBm) 256 (-40 dBm)",
        "2 (-22 dBm) 88 (+40 dBm)",
        "2 (-22 dBm) 88 (-40 dBm",
        "2 (-22 dBm) trailing garbage",
        "2 (-22 dBm) [arbitrary log payload]",
        "2 (-22 dBm) 2 (-23 dBm)",
        "86 (-68 dBm)",
    ] {
        let mut decoder = table_decoder();
        let before = decoder.feed_line("86 (-69 dBm)").unwrap();
        assert!(decoder.feed_line(bad).is_none(), "{bad}");
        assert!(decoder.feed_line("88 (-66 dBm)").is_none(), "{bad}");
        assert_eq!(before.report.samples, [sample(86, -69)]);
        assert!(decoder.feed_line(HEADER).is_none());
        assert!(decoder.feed_line(TABLE).is_none());
        let fresh = decoder.feed_line("2 (-22 dBm)").unwrap();
        assert_eq!(fresh.scan_id, 2);
        assert_eq!(fresh.report.samples, [sample(2, -22)]);
        assert!(parse_log(&format!("{HEADER}\n{TABLE}\n86 (-69 dBm)\n{bad}")).is_err());
    }
}

#[test]
fn explicit_exclusions_are_sorted_and_never_fake_samples() {
    let report = parse_log(&format!(
        "{HEADER}\nChannel 88 invalid: 1/200 visits failed; excluded\nChannel 4 invalid: 200/200 visits failed; excluded\nChannel 88 invalid: 1/200 visits failed; excluded\n{TABLE}\n86 (-69 dBm)"
    ))
    .unwrap();
    assert_eq!(report.excluded_channels, [4, 88]);
    assert_eq!(report.samples, [sample(86, -69)]);
    for invalid in [
        "Channel 86 invalid: 1/200 visits failed; excluded",
        "Channel 88 invalid: 0/200 visits failed; excluded",
        "Channel 88 invalid: 201/200 visits failed; excluded",
        "Channel 88 invalid: 1/100 visits failed; excluded",
        "Channel 89 invalid: 1/200 visits failed; excluded",
    ] {
        assert!(parse_log(&format!("{HEADER}\n{TABLE}\n86 (-69 dBm)\n{invalid}")).is_err());
    }
}

#[test]
fn metadata_conflicts_do_not_silently_replace_source_values() {
    for metadata in [
        "Recommended: channel 86 (-68 dBm)",
        "Recommended: channel 89 (-69 dBm)",
        "Recommended: none (no valid complete channel measurements)",
        "Current channel (effective): 85",
    ] {
        assert!(parse_log(&format!(
            "{HEADER}\nCurrent channel (effective): 84\nRecommended: channel 86 (-69 dBm)\n{TABLE}\n86 (-69 dBm)\n{metadata}"
        ))
        .is_err());
    }
    assert!(
        parse_log(&format!(
            "{HEADER}\nRecommended: channel 86 (-69 dBm)\n{TABLE}\n86 (-68 dBm)"
        ))
        .is_err()
    );
}

#[test]
fn explicit_empty_scan_is_a_real_empty_result_only_after_table_header() {
    let mut decoder = ScanDecoder::new();
    assert!(decoder.feed_line(HEADER).is_none());
    assert!(
        decoder
            .feed_line("Current channel (effective): 84")
            .is_none()
    );
    assert!(
        decoder
            .feed_line("Recommended: none (no valid complete channel measurements)")
            .is_none()
    );
    let empty = decoder.feed_line(TABLE).unwrap();
    assert_eq!(
        *empty.report,
        ScanReport {
            channel_range: (0, 88),
            current_channel: Some(84),
            recommendation: Recommendation::None,
            samples: Vec::new(),
            excluded_channels: Vec::new(),
        }
    );
    assert!(decoder.feed_line(TABLE).is_none());
    assert!(decoder.feed_line("2 (-22 dBm)").is_none());
    assert!(
        parse_log(&format!(
            "{HEADER}\nRecommended: none (no valid complete channel measurements)"
        ))
        .is_err()
    );
    assert!(parse_log(&format!("{HEADER}\n{TABLE}")).is_err());
}

#[test]
fn newest_header_or_failure_never_falls_back_to_old_usable_scan() {
    for suffix in [
        HEADER,
        "RSSI scan (ESB channels 88..0): samples/ch/sweep=50, sweeps=200 (total=10000 samples/ch)",
        "RSSI scan (ESB channels 0..88): samples/ch/sweep=50, sweeps=200 (total=42 samples/ch)",
        "RSSI scan busy: radio in use",
        "RSSI scan could not stop radio",
        "RSSI RADIO/frontend cleanup failed; retaining radio owner",
        "RSSI scan home radio restore failed: -5",
    ] {
        assert!(
            parse_log(&format!("{FIXTURE}\n{suffix}\n{TABLE}\n")).is_err(),
            "{suffix}"
        );
    }
    let newest = parse_log(&format!("{FIXTURE}\n{HEADER}\n{TABLE}\n2 (- 5 dBm)")).unwrap();
    assert_eq!(newest.samples, [sample(2, -5)]);
    assert_eq!(newest.current_channel, None);
    assert_eq!(newest.recommendation, Recommendation::Unknown);
}

#[test]
fn already_in_progress_and_start_race_do_not_destroy_active_output() {
    let mut decoder = table_decoder();
    assert!(decoder.feed_line("RSSI scan started").is_none());
    let first = decoder.feed_line("86 (-69 dBm)").unwrap();
    for line in ["RSSI scan already in progress", "Unknown command"] {
        assert!(decoder.feed_line(line).is_none());
    }
    let next = decoder.feed_line("2 (-22 dBm)").unwrap();
    assert_eq!(first.scan_id, next.scan_id);
    assert_eq!(next.report.samples.len(), 2);
    assert!(parse_log("RSSI scan busy: radio in use").is_err());
    assert!(parse_log(&format!("{FIXTURE}\nRSSI scan busy: radio in use")).is_err());
    assert!(decoder.feed_line("RSSI scan busy: radio in use").is_none());
    assert!(decoder.feed_line("88 (-66 dBm)").is_none());
}

#[test]
fn import_handles_cr_lf_and_full_unterminated_eof_without_completion_claim() {
    for separator in ["\n", "\r", "\r\n"] {
        let log = [HEADER, TABLE, "86 (-69 dBm)"].join(separator);
        assert_eq!(parse_log(&log).unwrap().samples, [sample(86, -69)]);
        assert!(parse_log(&format!("{log}{separator}2 (-22 dB")).is_err());
    }
}

#[test]
fn bounded_import_and_discarded_lines_cannot_continue_stale_tables() {
    assert!(parse_log(&"x".repeat(256 * 1024 + 1)).is_err());
    assert!(parse_log(&format!("{FIXTURE}{}", "x".repeat(4097))).is_err());
    assert!(
        parse_log(&format!(
            "{HEADER}\n{TABLE}\n86 (-69 dBm){}",
            " ".repeat(4096)
        ))
        .is_err()
    );
    let mut decoder = table_decoder();
    assert!(decoder.feed_line("86 (-69 dBm)").is_some());
    decoder.discard_line(b"discarded invalid UTF-8: \xff");
    assert!(decoder.feed_line("2 (-22 dBm)").is_none());
}

#[test]
fn arbitrary_channel_ids_are_bounded_by_u8_not_current_firmware_allowlist() {
    let report = parse_log(&format!(
        "RSSI scan (ESB channels 0..255): samples/ch/sweep=1, sweeps=1 (total=1 samples/ch)\n{TABLE}\n255 (-127 dBm) 4 (-0 dBm) 1 (- 1 dBm)"
    ))
    .unwrap();
    assert_eq!(
        report.samples,
        [sample(1, -1), sample(4, 0), sample(255, -127)]
    );
}

#[test]
fn discarded_bytes_preserve_osc_state_and_allow_real_header_recovery() {
    let mut decoder = table_decoder();
    assert!(decoder.feed_line("86 (-69 dBm)").is_some());
    decoder.discard_line(b"\xff\x1b]hidden window title");
    assert!(decoder.feed_line(HEADER).is_none());
    assert!(decoder.feed_line(TABLE).is_none());
    assert!(decoder.feed_line("88 (-1 dBm)").is_none());
    decoder.discard_line(b"more hidden payload\x07");
    assert!(decoder.feed_line(HEADER).is_none());
    assert!(decoder.feed_line(TABLE).is_none());
    let new = decoder.feed_line("2 (-22 dBm)").unwrap();
    assert_eq!(new.scan_id, 2);
    assert_eq!(new.report.samples, [sample(2, -22)]);
}

#[test]
fn oversized_direct_input_tracks_control_strings_without_retaining_payload() {
    let mut decoder = table_decoder();
    assert!(decoder.feed_line("86 (-69 dBm)").is_some());
    assert!(
        decoder
            .feed_line(&format!("\x1b]{}", "x".repeat(4097)))
            .is_none()
    );
    assert!(decoder.feed_line(HEADER).is_none());
    assert!(decoder.feed_line(TABLE).is_none());
    assert!(decoder.feed_line("88 (-1 dBm)").is_none());
    assert!(decoder.feed_line("\x07").is_none());
    assert!(decoder.feed_line(HEADER).is_none());
    assert!(decoder.feed_line(TABLE).is_none());
    assert_eq!(decoder.feed_line("2 (-22 dBm)").unwrap().scan_id, 2);
}

#[test]
fn malformed_recognized_metadata_and_heading_cannot_amend_old_rows() {
    for line in [
        "Current channel (effective):",
        "Current channel (effective): -1",
        "Current channel (effective): 256",
        "Recommended: channel 86 (-128 dBm)",
        "All channels sorted by RSSI: corrupted",
    ] {
        assert!(parse_log(&format!("{FIXTURE}\n{line}\n2 (-22 dBm)")).is_err());
    }
}

#[test]
fn unicode_c1_control_strings_do_not_expose_hidden_rows() {
    let mut decoder = table_decoder();
    assert!(decoder.feed_line("\u{9d}hidden title").is_none());
    assert!(decoder.feed_line("88 (-1 dBm)").is_none());
    let sample_row = decoder
        .feed_line("\u{9c}\u{9b}32m86 (-69 dBm)\u{9b}0m")
        .unwrap();
    assert_eq!(sample_row.report.samples, [sample(86, -69)]);
}

#[test]
fn discarded_unicode_continuations_never_open_control_strings() {
    for text in ["Н", "❜", "💝"] {
        let mut decoder = table_decoder();
        assert!(decoder.feed_line("86 (-69 dBm)").is_some());
        // Mirror the framer overflowing in the middle of a UTF-8 codepoint.
        for byte in text.as_bytes() {
            decoder.discard_line(std::slice::from_ref(byte));
        }
        assert!(decoder.feed_line(HEADER).is_none());
        assert!(decoder.feed_line(TABLE).is_none());
        let recovered = decoder.feed_line("2 (-22 dBm)").unwrap();
        assert_eq!(recovered.scan_id, 2, "{text}");
        assert_eq!(recovered.report.samples, [sample(2, -22)]);
    }
}

#[test]
fn oversized_unicode_logs_allow_recovery_without_false_osc_state() {
    let mut decoder = table_decoder();
    assert!(
        decoder
            .feed_line(&format!("{}Н", "x".repeat(4097)))
            .is_none()
    );
    assert!(decoder.feed_line(HEADER).is_none());
    assert!(decoder.feed_line(TABLE).is_none());
    assert_eq!(decoder.feed_line("2 (-22 dBm)").unwrap().scan_id, 2);
}

#[test]
fn unicode_continuations_inside_discarded_osc_cannot_expose_forged_tables() {
    let mut decoder = table_decoder();
    decoder.discard_line(b"\x1b]hidden title \xd0");
    decoder.discard_line(b"\x9c"); // Cyrillic М, not a Unicode C1 ST.
    assert!(decoder.feed_line(HEADER).is_none());
    assert!(decoder.feed_line(TABLE).is_none());
    assert!(decoder.feed_line("88 (-1 dBm)").is_none());
    assert!(decoder.feed_line("\x07").is_none());
    assert!(decoder.feed_line(HEADER).is_none());
    assert!(decoder.feed_line(TABLE).is_none());
    let recovered = decoder.feed_line("2 (-22 dBm)").unwrap();
    assert_eq!(recovered.scan_id, 2);
    assert_eq!(recovered.report.samples, [sample(2, -22)]);
}

#[test]
fn oversized_osc_unicode_payload_does_not_terminate_control_string() {
    let mut decoder = table_decoder();
    assert!(
        decoder
            .feed_line(&format!("\x1b]{}М", "x".repeat(4097)))
            .is_none()
    );
    assert!(decoder.feed_line(HEADER).is_none());
    assert!(decoder.feed_line(TABLE).is_none());
    assert!(decoder.feed_line("88 (-1 dBm)").is_none());
    assert!(decoder.feed_line("\x07").is_none());
    assert!(decoder.feed_line(HEADER).is_none());
    assert!(decoder.feed_line(TABLE).is_none());
    assert_eq!(decoder.feed_line("2 (-22 dBm)").unwrap().scan_id, 2);
}

#[test]
fn discarded_genuine_utf8_c1_controls_work_across_calls() {
    let mut decoder = table_decoder();
    decoder.discard_line(b"\xc2");
    decoder.discard_line(b"\x9d"); // Genuine Unicode C1 OSC.
    assert!(decoder.feed_line(HEADER).is_none());
    assert!(decoder.feed_line(TABLE).is_none());
    assert!(decoder.feed_line("88 (-1 dBm)").is_none());
    decoder.discard_line(b"\xc2");
    decoder.discard_line(b"\x9c"); // Genuine Unicode C1 ST.
    assert!(decoder.feed_line(HEADER).is_none());
    assert!(decoder.feed_line(TABLE).is_none());
    assert_eq!(decoder.feed_line("2 (-22 dBm)").unwrap().scan_id, 2);
}

#[test]
fn incomplete_discarded_utf8_does_not_absorb_next_valid_line() {
    let mut decoder = table_decoder();
    decoder.discard_line(b"\xd0");
    assert!(decoder.feed_line(HEADER).is_none());
    assert!(decoder.feed_line(TABLE).is_none());
    assert_eq!(decoder.feed_line("2 (-22 dBm)").unwrap().scan_id, 2);
}

#[test]
fn discarded_line_delimiters_break_truncated_utf8_but_not_osc() {
    let mut decoder = table_decoder();
    decoder.discard_line(b"\xc2");
    decoder.discard_line(b"\n");
    decoder.discard_line(b"\x9d"); // Invalid standalone continuation, not C1 OSC.
    assert!(decoder.feed_line(HEADER).is_none());
    assert!(decoder.feed_line(TABLE).is_none());
    assert_eq!(decoder.feed_line("2 (-22 dBm)").unwrap().scan_id, 2);

    decoder.discard_line(b"\x1b]hidden title\xc2");
    decoder.discard_line(b"\n");
    decoder.discard_line(b"\x9c"); // Must not combine across newline into C1 ST.
    assert!(decoder.feed_line(HEADER).is_none());
    assert!(decoder.feed_line(TABLE).is_none());
    assert!(decoder.feed_line("88 (-1 dBm)").is_none());
    assert!(decoder.feed_line("\x07").is_none());
    assert!(decoder.feed_line(HEADER).is_none());
    assert!(decoder.feed_line(TABLE).is_none());
    assert_eq!(decoder.feed_line("2 (-22 dBm)").unwrap().scan_id, 3);
}
