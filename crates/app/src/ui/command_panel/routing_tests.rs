use super::{CommandFocus, CommandPin, validate_command_pin, validate_tracker_focus};
use crate::{
    commands::RouteEvidence,
    domain::{DeviceRole, SerialEndpoint, SerialPortKind, TrackerLink},
};

fn endpoint() -> SerialEndpoint {
    SerialEndpoint {
        port_name: "COM7".into(),
        label: "Receiver".into(),
        manufacturer: None,
        product: None,
        kind: SerialPortKind::Usb,
        serial_number: Some("USB-A".into()),
        vid: Some(0x1209),
        pid: Some(0x7690),
        usb_interface: None,
    }
}

#[test]
fn command_pin_rejects_tab_switch_reconnect_and_role_change() {
    let pin = CommandPin {
        focus: CommandFocus::Serial("COM7".into()),
        generation: 4,
        role: DeviceRole::Receiver,
        forwarded: false,
        endpoint: endpoint(),
    };
    let evidence = RouteEvidence::Direct {
        port: "COM7",
        generation: 4,
        connected: true,
        role: DeviceRole::Receiver,
    };
    assert!(validate_command_pin(&pin, Some(&pin.focus), &evidence).is_ok());
    assert!(
        validate_command_pin(&pin, Some(&CommandFocus::Serial("COM8".into())), &evidence).is_err()
    );
    for (port, generation, role) in [
        ("COM8", 4, DeviceRole::Receiver),
        ("COM7", 5, DeviceRole::Receiver),
        ("COM7", 4, DeviceRole::Tracker),
    ] {
        assert!(
            validate_command_pin(
                &pin,
                Some(&pin.focus),
                &RouteEvidence::Direct {
                    port,
                    generation,
                    connected: true,
                    role
                }
            )
            .is_err()
        );
    }
}

#[test]
fn parameter_dialog_pin_cannot_change_transport_kind() {
    let pin = CommandPin {
        focus: CommandFocus::Tracker {
            receiver: "receiver-a".into(),
            slot: 2,
            address: Some("AABBCCDDEEFF".into()),
        },
        generation: 4,
        role: DeviceRole::Tracker,
        forwarded: false,
        endpoint: endpoint(),
    };
    let forwarded = RouteEvidence::Forwarded {
        port: "COM7",
        generation: 4,
        connected: true,
        receiver_key: "receiver-a",
        slot: 2,
        address: Some("AABBCCDDEEFF"),
        unique_receiver_route: true,
    };
    assert!(validate_command_pin(&pin, Some(&pin.focus), &forwarded).is_err());
    let remote_pin = CommandPin {
        forwarded: true,
        ..pin.clone()
    };
    assert!(validate_command_pin(&remote_pin, Some(&remote_pin.focus), &forwarded).is_ok());
    assert!(
        validate_command_pin(
            &remote_pin,
            Some(&remote_pin.focus),
            &RouteEvidence::Direct {
                port: "COM7",
                generation: 4,
                connected: true,
                role: DeviceRole::Tracker
            }
        )
        .is_err()
    );
}

#[test]
fn tracker_focus_cannot_follow_slot_reassignment_or_receiver_change() {
    let focus = CommandFocus::Tracker {
        receiver: "receiver-a".into(),
        slot: 2,
        address: Some("AABBCCDDEEFF".into()),
    };
    let mut tracker = TrackerLink {
        receiver_key: "receiver-a".into(),
        slot: 2,
        address: Some("AABBCCDDEEFF".into()),
        online: true,
    };
    assert!(validate_tracker_focus(&focus, &tracker).is_ok());
    tracker.online = false;
    assert!(validate_tracker_focus(&focus, &tracker).is_ok());
    tracker.address = Some("112233445566".into());
    assert!(validate_tracker_focus(&focus, &tracker).is_err());
    tracker.address = Some("AABBCCDDEEFF".into());
    tracker.receiver_key = "receiver-b".into();
    assert!(validate_tracker_focus(&focus, &tracker).is_err());
    tracker.receiver_key = "receiver-a".into();
    tracker.slot = 3;
    assert!(validate_tracker_focus(&focus, &tracker).is_err());
    tracker.slot = 2;
    tracker.address = None;
    assert!(validate_tracker_focus(&focus, &tracker).is_err());
}
