use super::super::{CommandSnapshot, RouteEvidence};
use super::{broadcast, direct, forwarded, get};
use crate::domain::DeviceRole;

#[test]
fn direct_snapshot_rejects_reconnected_disconnected_or_changed_role_sessions() {
    let snapshot = CommandSnapshot::capture(get("info"), "", direct(DeviceRole::Tracker)).unwrap();
    assert_eq!(snapshot.port(), "COM30");
    assert!(
        snapshot
            .authorize(direct(DeviceRole::Tracker), false)
            .is_ok()
    );
    for current in [
        RouteEvidence::Direct {
            port: "COM30",
            generation: 8,
            connected: true,
            role: DeviceRole::Tracker,
        },
        RouteEvidence::Direct {
            port: "COM30",
            generation: 7,
            connected: false,
            role: DeviceRole::Tracker,
        },
        RouteEvidence::Direct {
            port: "COM31",
            generation: 7,
            connected: true,
            role: DeviceRole::Tracker,
        },
        direct(DeviceRole::Receiver),
        forwarded(),
    ] {
        assert!(snapshot.authorize(current, true).is_err());
    }
}

#[test]
fn wireless_confirmation_rejects_stale_or_ambiguous_mapping() {
    let snapshot = CommandSnapshot::capture(get("reboot"), "", forwarded()).unwrap();
    assert_eq!(snapshot.command(), "send 3 reboot");
    assert!(snapshot.authorize(forwarded(), false).is_err());
    assert!(snapshot.authorize(forwarded(), true).is_ok());
    for mutation in 0..8 {
        let mut current = forwarded();
        if let RouteEvidence::Forwarded {
            port,
            generation,
            connected,
            receiver_key,
            slot,
            address,
            unique_receiver_route,
        } = &mut current
        {
            match mutation {
                0 => *address = Some("001122334455"),
                1 => *address = None,
                2 => *slot = 4,
                3 => *receiver_key = "receiver-b",
                4 => *unique_receiver_route = false,
                5 => *generation = 8,
                6 => *port = "COM31",
                _ => *connected = false,
            }
        }
        assert!(
            snapshot.authorize(current, true).is_err(),
            "mutation {mutation}"
        );
    }
    let mut unknown = forwarded();
    if let RouteEvidence::Forwarded { address, .. } = &mut unknown {
        *address = None;
    }
    assert!(CommandSnapshot::capture(get("ping"), "", unknown).is_err());
}

#[test]
fn sensitivity_reset_requires_confirmation_and_invalid_tokens_never_capture() {
    let current = direct(DeviceRole::Tracker);
    for parameter in ["reset", "  reset  "] {
        let snapshot = CommandSnapshot::capture(get("sens"), parameter, current).unwrap();
        assert!(snapshot.dangerous(), "{parameter:?}");
        assert!(snapshot.authorize(current, false).is_err(), "{parameter:?}");
        assert!(snapshot.authorize(current, true).is_ok());
    }
    let explicit = CommandSnapshot::capture(get("sens-reset"), "", current).unwrap();
    let generic = CommandSnapshot::capture(get("sens"), "reset", current).unwrap();
    assert_eq!(explicit.command(), generic.command());
    assert_eq!(explicit.dangerous(), generic.dangerous());
    for parameter in [
        "resetting",
        "1 reset",
        "reset extra",
        "RESET",
        "ReSeT extra",
    ] {
        assert!(
            CommandSnapshot::capture(get("sens"), parameter, current).is_err(),
            "{parameter:?}"
        );
    }
    let numeric = CommandSnapshot::capture(get("sens"), "1,-2.5,0", current).unwrap();
    assert!(!numeric.dangerous());
    assert!(numeric.authorize(current, false).is_ok());
}

#[test]
fn hazardous_presets_cannot_be_authorized_without_confirmation() {
    for (id, role) in [
        ("tracker-clear", DeviceRole::Tracker),
        ("dfu", DeviceRole::Tracker),
        ("reboot", DeviceRole::Tracker),
        ("tracker-clearchannel", DeviceRole::Tracker),
        ("reset-all", DeviceRole::Tracker),
        ("remove", DeviceRole::Receiver),
        ("receiver-clearchannel", DeviceRole::Receiver),
    ] {
        let route = direct(role);
        let snapshot = CommandSnapshot::capture(get(id), "", route).unwrap();
        assert!(snapshot.dangerous(), "{id}");
        assert!(snapshot.authorize(route, false).is_err(), "{id}");
        assert!(snapshot.authorize(route, true).is_ok(), "{id}");
    }
    for id in [
        "shutdown",
        "tracker-clear",
        "dfu",
        "dfu-ota",
        "reboot",
        "mag-on",
        "mag-off",
        "reset-fusion",
        "mag-cal",
        "mag-clear",
    ] {
        let snapshot = CommandSnapshot::capture(get(id), "", forwarded()).unwrap();
        assert!(snapshot.dangerous(), "{id}");
        assert!(snapshot.authorize(forwarded(), false).is_err(), "{id}");
        assert!(snapshot.authorize(forwarded(), true).is_ok(), "{id}");
    }
}

#[test]
fn remote_sensitivity_rebuilds_real_values_and_recomputes_reset_confirmation() {
    let preset = get("remote-sens");
    assert!(CommandSnapshot::capture(preset, "", forwarded()).is_err());
    let custom = CommandSnapshot::capture(preset, "0,0,0", forwarded())
        .unwrap()
        .with_parameter("1,-2,3")
        .unwrap();
    assert_eq!(custom.command(), "send 3 sens 1,-2,3");
    assert!(!custom.dangerous());
    assert!(custom.authorize(forwarded(), false).is_ok());
    let reset = custom.with_parameter("reset").unwrap();
    assert_eq!(reset.command(), "send 3 sens reset");
    assert!(reset.dangerous());
    assert!(reset.authorize(forwarded(), false).is_err());
    assert!(reset.authorize(forwarded(), true).is_ok());
    for invalid in ["", "RESET", "reset extra", "reset\nreboot"] {
        assert!(
            reset.clone().with_parameter(invalid).is_err(),
            "{invalid:?}"
        );
    }
    let values = reset.with_parameter("0,0,0").unwrap();
    assert_eq!(values.command(), "send 3 sens 0,0,0");
    assert!(!values.dangerous());
    assert!(values.authorize(forwarded(), false).is_ok());
}

#[test]
fn receiver_broadcast_pins_connection_and_requires_confirmation() {
    let route = RouteEvidence::Broadcast {
        port: "COM30",
        generation: 7,
        connected: true,
        receiver_key: "receiver-a",
        unique_receiver_route: true,
    };
    let snapshot = CommandSnapshot::capture(get("sens-auto"), "x 5", route)
        .unwrap()
        .with_parameter("x 10")
        .unwrap();
    assert_eq!(snapshot.command(), "send all sens auto x 10");
    assert!(snapshot.authorize(route, false).is_err());
    assert!(snapshot.authorize(route, true).is_ok());
    for mutation in 0..5 {
        let mut current = route;
        if let RouteEvidence::Broadcast {
            port,
            generation,
            connected,
            receiver_key,
            unique_receiver_route,
        } = &mut current
        {
            match mutation {
                0 => *port = "COM31",
                1 => *generation = 8,
                2 => *connected = false,
                3 => *receiver_key = "receiver-b",
                _ => *unique_receiver_route = false,
            }
        }
        assert!(snapshot.authorize(current, true).is_err());
    }
    assert!(snapshot.authorize(forwarded(), true).is_err());
    assert!(CommandSnapshot::capture(get("info"), "", route).is_err());
}

#[test]
fn receiver_wide_channel_operations_reject_slots_but_allow_explicit_scope_and_direct_roles() {
    for (id, parameter, payload, role) in [
        ("tracker-channel", "100", "channel 100", DeviceRole::Tracker),
        (
            "tracker-clearchannel",
            "",
            "clearchannel",
            DeviceRole::Tracker,
        ),
    ] {
        let preset = get(id);
        assert!(
            CommandSnapshot::capture(preset, parameter, forwarded()).is_err(),
            "{id}"
        );
        assert!(preset.build_with_slot(parameter, Some(3)).is_err(), "{id}");
        let local = CommandSnapshot::capture(preset, parameter, direct(role)).unwrap();
        assert_eq!(local.command(), payload);
        assert!(local.authorize(direct(role), true).is_ok());
        let remote = CommandSnapshot::capture(preset, parameter, broadcast()).unwrap();
        assert_eq!(remote.command(), format!("send all {payload}"));
        assert!(remote.authorize(broadcast(), false).is_err());
        assert!(remote.authorize(broadcast(), true).is_ok());
        assert!(remote.validate(forwarded()).is_err());
        assert!(remote.authorize(forwarded(), true).is_err());
    }
    for (id, parameter, payload) in [
        ("receiver-channel", "100", "channel 100"),
        ("receiver-clearchannel", "", "clearchannel"),
    ] {
        let route = direct(DeviceRole::Receiver);
        let local = CommandSnapshot::capture(get(id), parameter, route).unwrap();
        assert_eq!(local.command(), payload);
        assert!(local.authorize(route, true).is_ok());
    }
}

#[test]
fn parameter_rebuilding_preserves_scope_and_rejects_scope_escalation_at_submission() {
    let preset = get("tracker-channel");
    let remote = CommandSnapshot::capture(preset, "0", broadcast())
        .unwrap()
        .with_parameter("100")
        .unwrap();
    assert_eq!(remote.command(), "send all channel 100");
    assert_eq!(remote.port(), "COM30");
    assert!(remote.validate(broadcast()).is_ok());
    assert!(remote.validate(forwarded()).is_err());
    assert!(remote.validate(direct(DeviceRole::Tracker)).is_err());
    assert!(remote.with_parameter("101").is_err());

    let local = CommandSnapshot::capture(preset, "0", direct(DeviceRole::Tracker))
        .unwrap()
        .with_parameter("100")
        .unwrap();
    assert_eq!(local.command(), "channel 100");
    assert!(local.validate(direct(DeviceRole::Tracker)).is_ok());
    assert!(local.authorize(broadcast(), true).is_err());
    assert!(local.authorize(forwarded(), true).is_err());

    let selected = CommandSnapshot::capture(get("wom-timeout"), "", forwarded())
        .unwrap()
        .with_parameter("0")
        .unwrap();
    assert_eq!(selected.command(), "send 3 wom timeout 0");
    assert!(selected.validate(forwarded()).is_ok());
    assert!(selected.authorize(broadcast(), true).is_err());
    let mut other_slot = forwarded();
    if let RouteEvidence::Forwarded { slot, .. } = &mut other_slot {
        *slot = 4;
    }
    assert!(selected.authorize(other_slot, true).is_err());
}

#[test]
fn heating_and_wom_entry_require_confirmation_of_the_frozen_target() {
    for (id, route, expected) in [
        ("tcal-heat-start", forwarded(), "send 3 tcal heat start"),
        ("tcal-heat-start", broadcast(), "send all tcal heat start"),
        ("wom-enter", forwarded(), "send 3 wom enter"),
        ("wom-enter", broadcast(), "send all wom enter"),
        ("wom-enter", direct(DeviceRole::Tracker), "wom enter"),
    ] {
        let snapshot = CommandSnapshot::capture(get(id), "", route).unwrap();
        assert_eq!(snapshot.command(), expected);
        assert!(snapshot.dangerous(), "{id}");
        assert!(snapshot.authorize(route, false).is_err(), "{id}");
        assert!(snapshot.authorize(route, true).is_ok(), "{id}");
        let mut changed = route;
        match &mut changed {
            RouteEvidence::Direct { generation, .. }
            | RouteEvidence::Broadcast { generation, .. }
            | RouteEvidence::Forwarded { generation, .. } => *generation += 1,
        }
        assert!(snapshot.authorize(changed, true).is_err(), "{id}");
        let other_scope = match route {
            RouteEvidence::Forwarded { .. } => broadcast(),
            _ => forwarded(),
        };
        assert!(snapshot.authorize(other_scope, true).is_err(), "{id}");
        assert!(snapshot.with_parameter("44").is_err(), "{id}");
    }
}

#[test]
fn receiver_wide_semantics_cannot_be_bypassed_by_missing_preset_metadata_or_custom_suffix() {
    use super::super::{ParameterSpec, Preset};

    // Deliberately omit the presentation metadata: admission must still inspect
    // the actual wire verb, including a suffix supplied as editable text.
    static CUSTOM: Preset = Preset {
        id: "test-custom-suffix",
        group: "System",
        label: "Custom suffix",
        description: "Test-only editable suffix",
        command: "",
        parameter: ParameterSpec::Text { hint: "Suffix" },
        dangerous: false,
        remote_eligible: true,
        receiver_wide_only: false,
        roles: super::super::BOTH,
    };
    static CHANNEL_WITHOUT_METADATA: Preset = Preset {
        id: "test-channel-without-metadata",
        command: "channel",
        parameter: ParameterSpec::Integer {
            min: 0,
            max: 100,
            default: "0",
        },
        ..CUSTOM
    };

    assert!(CommandSnapshot::capture(&CHANNEL_WITHOUT_METADATA, "100", forwarded()).is_err());
    for suffix in ["channel 100", "clearchannel", "  channel 0  "] {
        assert!(
            CommandSnapshot::capture(&CUSTOM, suffix, forwarded()).is_err(),
            "{suffix}"
        );
        let selected = CommandSnapshot::capture(&CUSTOM, "ping", forwarded()).unwrap();
        assert!(selected.with_parameter(suffix).is_err(), "{suffix}");
        let explicit_all = CommandSnapshot::capture(&CUSTOM, suffix, broadcast()).unwrap();
        assert!(explicit_all.authorize(broadcast(), false).is_err());
        assert!(explicit_all.authorize(broadcast(), true).is_ok());
        assert!(explicit_all.validate(forwarded()).is_err());
        assert!(
            explicit_all
                .with_parameter("channel 100")
                .unwrap()
                .validate(forwarded())
                .is_err()
        );
    }
    // Receiver-local raw serial commands retain their existing unrestricted
    // text semantics; the restriction applies only to app-owned slot forwarding.
    let local =
        CommandSnapshot::capture(&CUSTOM, "send 3 channel 100", direct(DeviceRole::Receiver))
            .unwrap();
    assert!(local.authorize(direct(DeviceRole::Receiver), false).is_ok());
}
