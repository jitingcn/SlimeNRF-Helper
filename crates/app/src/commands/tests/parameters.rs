use super::get;

#[test]
fn parameters_build_real_commands() {
    for (id, parameter, expected) in [
        ("sens-auto", "z 5", "sens auto z 5"),
        ("remote-reset", "acc", "reset acc"),
        ("tcal-remove", "12", "tcal remove 12"),
        ("debug-timed", "10", "debug 10"),
        ("stats-timed", "5", "stats 5"),
        ("pair-count", "3", "pair 3"),
        ("add", "AABBCCDDEEFF", "add AABBCCDDEEFF"),
        ("set", "AABBCCDDEEFF", "set AABBCCDDEEFF"),
        ("sens", "1,-2.5,0", "sens 1,-2.5,0"),
    ] {
        assert_eq!(get(id).build(parameter).unwrap(), expected);
    }
    assert!(get("dfu").build("ota").is_err());
    assert!(get("debug-timed").build("   ").is_err());
}

#[test]
fn channel_zero_and_one_hundred_are_real_channels_not_clear_operations() {
    for id in ["tracker-channel", "receiver-channel"] {
        let suggestions = get(id).parameter_presets();
        assert_eq!(suggestions.len(), 101);
        for (channel, suggestion) in suggestions.iter().enumerate() {
            assert_eq!(suggestion.label, None);
            assert_eq!(suggestion.value, channel.to_string());
            assert_eq!(
                get(id).build(suggestion.value).unwrap(),
                format!("channel {channel}")
            );
        }
        for value in ["0", "1", "99", "100"] {
            assert_eq!(get(id).build(value).unwrap(), format!("channel {value}"));
        }
        for value in [
            "",
            "-1",
            "101",
            "4294967296",
            "+1",
            "1.5",
            "all",
            "1 2",
            "1e2",
            "１００",
        ] {
            assert!(get(id).build(value).is_err(), "{id}: {value:?}");
        }
    }
}

#[test]
fn optional_integer_commands_preserve_omitted_and_explicit_zero() {
    for (id, command, max) in [
        ("test-on", "test on", "1000"),
        ("wom-timeout", "wom timeout", "35791"),
    ] {
        let preset = get(id);
        assert_eq!(preset.build("").unwrap(), command);
        assert_eq!(preset.build("   ").unwrap(), command);
        assert_eq!(preset.build("0").unwrap(), format!("{command} 0"));
        assert_eq!(preset.build("1").unwrap(), format!("{command} 1"));
        assert_eq!(preset.build(max).unwrap(), format!("{command} {max}"));
        assert_eq!(preset.build(" 0 ").unwrap(), format!("{command} 0"));
        for invalid in [
            "-1",
            "+1",
            "0.0",
            "1e2",
            "0x10",
            "4294967296",
            "18446744073709551616",
            "1 2",
            "0 extra",
            "all",
            "NaN",
            "∞",
            "１２",
        ] {
            assert!(preset.build(invalid).is_err(), "{id}: {invalid:?}");
        }
    }
    assert!(get("test-on").build("1001").is_err());
    for invalid in ["35792", "65534", "65535"] {
        assert!(get("wom-timeout").build(invalid).is_err(), "{invalid}");
    }
}

#[test]
fn sensitivity_auto_requires_lowercase_axis_and_bounds_only_explicit_revolutions() {
    let preset = get("sens-auto");
    assert_eq!(preset.parameter.default_value(), "x");
    for axis in ["x", "y", "z"] {
        for value in [axis.to_owned(), format!("{axis} 1"), format!("{axis} 100")] {
            assert_eq!(preset.build(&value).unwrap(), format!("sens auto {value}"));
        }
        assert!(
            preset
                .parameter_presets()
                .iter()
                .any(|choice| choice.value == axis),
            "axis-only default choice: {axis}"
        );
    }
    // Suggestions remain editable rather than restricting input to a fixed menu.
    assert_eq!(preset.build("y 37").unwrap(), "sens auto y 37");
    for invalid in [
        "",
        "5",
        "X",
        "Y 5",
        "Z",
        "xy",
        "a 5",
        "x 0",
        "x -1",
        "x +1",
        "x 101",
        "x 4294967296",
        "x 1.5",
        "x 1e2",
        "x 1 2",
        "x default",
        "x,5",
    ] {
        assert!(preset.build(invalid).is_err(), "{invalid:?}");
    }
}

#[test]
fn sensitivity_uses_one_finite_triplet_or_exact_reset() {
    for id in ["sens", "remote-sens"] {
        let preset = get(id);
        assert_eq!(preset.parameter.default_value(), "0,0,0");
        for value in [
            "0,0,0",
            "-327.68,327.67,0",
            "327.67,-327.68,-0.01",
            "1.25,-2.5,3",
            "-0,0.00,0",
            "reset",
        ] {
            assert_eq!(
                preset.build(value).unwrap(),
                format!("sens {value}"),
                "{id}"
            );
        }
        for invalid in [
            "",
            "   ",
            "1 1 1",
            "1,2",
            "1,2,3,4",
            "1,,3",
            ",2,3",
            "1,2,",
            "1, 2,3",
            "1 ,2,3",
            "1,2,3 extra",
            "reset extra",
            "RESET",
            "ReSeT",
            "resetting",
            "1 reset",
            "NaN,0,0",
            "0,NaN,0",
            "0,0,NaN",
            "inf,0,0",
            "0,-inf,0",
            "0,0,Infinity",
            "1e999,0,0",
            "1e39,0,0",
        ] {
            assert!(preset.build(invalid).is_err(), "{id}: {invalid:?}");
        }
    }
}

#[test]
fn only_remote_sensitivity_is_limited_by_signed_centidegree_transport() {
    for value in [
        "400,0,0",
        "-327.69,0,0",
        "0,327.68,0",
        "0,0,327.68",
        "0,-327.69,0",
        "0,0,-327.69",
        "327.68,0,0",
    ] {
        assert_eq!(get("sens").build(value).unwrap(), format!("sens {value}"));
        assert!(get("remote-sens").build(value).is_err(), "{value}");
    }
}

#[test]
fn remote_reset_rejects_direct_only_targets_and_extra_tokens() {
    let preset = get("remote-reset");
    for target in ["zro", "acc", "bat", "mag", "tcal", "fusion"] {
        assert_eq!(preset.build(target).unwrap(), format!("reset {target}"));
    }
    for invalid in [
        "",
        "sens",
        "all",
        "fusion extra",
        "acc bat",
        "FUSION",
        "unknown",
    ] {
        assert!(preset.build(invalid).is_err(), "{invalid:?}");
    }
}

#[test]
fn every_parameter_contract_rejects_injected_control_characters_before_trimming() {
    for (id, valid) in [
        ("debug-timed", "10"),
        ("tracker-channel", "100"),
        ("test-on", "1000"),
        ("wom-timeout", "0"),
        ("sens-auto", "x 5"),
        ("sens", "0,0,0"),
        ("remote-sens", "reset"),
        ("remote-reset", "fusion"),
        ("tcal-heat-start", ""),
    ] {
        for control in [
            '\n', '\r', '\0', '\t', '\u{000b}', '\u{0085}', '\u{2028}', '\u{2029}',
        ] {
            for value in [
                format!("{valid}{control}"),
                format!("{control}{valid}"),
                format!("{valid}{control}reboot"),
            ] {
                assert!(get(id).build(&value).is_err(), "{id}: {value:?}");
            }
        }
    }
}
