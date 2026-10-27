use super::super::{CommandContext, CommandSnapshot, ParameterSpec, RouteEvidence, presets};
use super::{broadcast, direct, forwarded, get};
use crate::domain::DeviceRole;

#[test]
fn available_groups_expose_each_command_once() {
    for context in [
        CommandContext::Direct(DeviceRole::Tracker),
        CommandContext::Direct(DeviceRole::Receiver),
        CommandContext::Direct(DeviceRole::Unknown),
        CommandContext::ForwardedTracker,
    ] {
        let groups: Vec<_> = super::super::preset_groups(context).collect();
        let visible: Vec<_> = groups
            .iter()
            .flat_map(|group| presets(context).filter(move |preset| preset.group == *group))
            .collect();
        for group in &groups {
            assert!(
                visible.iter().any(|preset| preset.group == *group),
                "{group}"
            );
        }
        for preset in presets(context) {
            assert_eq!(
                visible.iter().filter(|card| card.id == preset.id).count(),
                1,
                "{}",
                preset.id
            );
        }
        for id in ["info", "help"] {
            let cards: Vec<_> = visible.iter().filter(|preset| preset.id == id).collect();
            if context == CommandContext::ForwardedTracker {
                assert!(cards.is_empty());
            } else {
                assert_eq!(cards.len(), 1);
                assert_eq!(cards[0].group, "commands.group.information");
            }
        }
        assert_eq!(
            groups.contains(&"commands.group.diagnostics"),
            matches!(
                context,
                CommandContext::Direct(DeviceRole::Tracker) | CommandContext::ForwardedTracker
            )
        );
    }
}

#[test]
fn information_commands_are_direct_only_for_all_roles() {
    for role in [
        DeviceRole::Tracker,
        DeviceRole::Receiver,
        DeviceRole::Unknown,
    ] {
        for id in ["info", "help"] {
            let snapshot = CommandSnapshot::capture(get(id), "", direct(role)).unwrap();
            assert!(snapshot.authorize(direct(role), false).is_ok());
        }
    }
    for id in ["info", "help"] {
        assert!(CommandSnapshot::capture(get(id), "", forwarded()).is_err());
        assert!(
            CommandSnapshot::capture(
                get(id),
                "",
                RouteEvidence::Broadcast {
                    port: "COM30",
                    generation: 7,
                    connected: true,
                    receiver_key: "receiver-a",
                    unique_receiver_route: true,
                }
            )
            .is_err()
        );
    }
}
#[test]
fn unknown_roles_only_have_safe_generic_info_and_help() {
    let commands: Vec<_> = presets(CommandContext::Direct(DeviceRole::Unknown))
        .map(|p| p.command)
        .collect();
    assert_eq!(commands, ["info", "help"]);
    for p in presets(CommandContext::Direct(DeviceRole::Unknown)) {
        let snapshot = CommandSnapshot::capture(p, "", direct(DeviceRole::Unknown)).unwrap();
        assert!(
            snapshot
                .authorize(direct(DeviceRole::Unknown), false)
                .is_ok()
        );
    }
    assert!(CommandSnapshot::capture(get("remove"), "", direct(DeviceRole::Tracker)).is_err());
    assert!(CommandSnapshot::capture(get("calibrate"), "", direct(DeviceRole::Receiver)).is_err());
}

#[test]
fn remote_defaults_build_only_for_the_explicit_supported_scope() {
    for preset in presets(CommandContext::ForwardedTracker) {
        let parameter = preset.parameter.default_value();
        let payload = preset.build(parameter).unwrap();
        let route = if preset.receiver_wide_only {
            assert!(
                CommandSnapshot::capture(preset, parameter, forwarded()).is_err(),
                "{}",
                preset.id
            );
            broadcast()
        } else {
            forwarded()
        };
        let snapshot = CommandSnapshot::capture(preset, parameter, route).unwrap();
        let expected = if preset.receiver_wide_only {
            format!("send all {payload}")
        } else {
            format!("send 3 {payload}")
        };
        assert_eq!(snapshot.command(), expected, "{}", preset.id);
        assert!(!snapshot.command().contains('\n'));
        assert!(snapshot.authorize(route, true).is_ok(), "{}", preset.id);
    }
}

#[test]
fn tracker_shortcuts_cover_direct_and_remote_firmware_operations() {
    for (id, parameter, payload) in [
        ("calibrate-acc", "", "calibrate acc"),
        ("meow", "", "meow"),
        ("tdma-on", "", "tdma on"),
        ("tdma-off", "", "tdma off"),
        ("test-off", "", "test off"),
        ("shutdown", "", "shutdown"),
        ("reset-fusion", "", "reset fusion"),
        ("sens-auto", "y", "sens auto y"),
        ("wom-enter", "", "wom enter"),
        ("wom-timeout", "", "wom timeout"),
    ] {
        let preset = get(id);
        let local =
            CommandSnapshot::capture(preset, parameter, direct(DeviceRole::Tracker)).unwrap();
        let remote = CommandSnapshot::capture(preset, parameter, forwarded()).unwrap();
        assert_eq!(local.command(), payload, "{id}");
        assert_eq!(remote.command(), format!("send 3 {payload}"), "{id}");
        assert!(
            CommandSnapshot::capture(preset, parameter, direct(DeviceRole::Receiver)).is_err(),
            "{id}"
        );
    }
}

#[test]
fn direct_test_mode_cannot_inherit_remote_tps_or_remote_only_heating() {
    let route = direct(DeviceRole::Tracker);
    let local = CommandSnapshot::capture(get("tracker-test-on"), "", route).unwrap();
    assert_eq!(local.command(), "test on");
    assert!(get("tracker-test-on").build("100").is_err());
    assert!(CommandSnapshot::capture(get("tracker-test-on"), "", forwarded()).is_err());
    for (id, parameter, payload) in [
        ("test-on", "1000", "test on 1000"),
        ("tcal-heat-start", "", "tcal heat start"),
    ] {
        assert!(
            CommandSnapshot::capture(get(id), parameter, route).is_err(),
            "{id}"
        );
        assert_eq!(
            CommandSnapshot::capture(get(id), parameter, forwarded())
                .unwrap()
                .command(),
            format!("send 3 {payload}"),
            "{id}"
        );
    }
    assert!(get("tcal-heat-start").build("44").is_err());
}

#[test]
fn local_workflows_and_real_slot_bounds_are_not_remote_shortcuts() {
    for id in [
        "info",
        "help",
        "list",
        "remove",
        "tcal-dump",
        "reset-all",
        "sens",
    ] {
        let parameter = get(id).parameter.default_value();
        for route in [forwarded(), broadcast()] {
            assert!(
                CommandSnapshot::capture(get(id), parameter, route).is_err(),
                "{id}"
            );
        }
    }
    let dump = CommandSnapshot::capture(get("tcal-dump"), "", direct(DeviceRole::Tracker)).unwrap();
    assert_eq!(dump.command(), "tcal dump");
    assert!(!dump.dangerous());
    assert!(dump.authorize(direct(DeviceRole::Tracker), false).is_ok());
    for slot_value in [0, 15, 16, 255] {
        let mut route = forwarded();
        if let RouteEvidence::Forwarded { slot, .. } = &mut route {
            *slot = slot_value;
        }
        let result = CommandSnapshot::capture(get("ping"), "", route);
        if slot_value <= 15 {
            assert_eq!(result.unwrap().command(), format!("send {slot_value} ping"));
        } else {
            assert!(result.is_err(), "{slot_value}");
        }
    }
}
#[test]
fn catalog_defaults_are_valid_and_identifiers_are_unique() {
    for (index, p) in super::super::PRESETS.iter().enumerate() {
        assert!(
            !super::super::PRESETS[..index]
                .iter()
                .any(|other| other.id == p.id)
        );
        for choice in p.parameter_presets() {
            assert!(p.build(choice.value).is_ok(), "{}: {}", p.id, choice.value);
        }
        if !matches!(p.parameter, ParameterSpec::Text { .. }) {
            assert!(p.build(p.parameter.default_value()).is_ok(), "{}", p.id);
        }
    }
}
