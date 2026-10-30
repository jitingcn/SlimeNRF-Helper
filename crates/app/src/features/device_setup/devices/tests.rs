use super::live_tracker_port;
use crate::{
    domain::{
        DeviceIdentity, DeviceRole, InterfaceDescription, Inventory, ReceiverEndpoint,
        SerialEndpoint, TrackerLink, UsbInterface, console_port_allowed, receiver_port,
        receiver_serial_ports,
    },
    sessions::ConnectionState,
};

fn receiver(key: &str, serial: &str) -> ReceiverEndpoint {
    ReceiverEndpoint {
        key: key.into(),
        path: key.as_bytes().to_vec(),
        label: key.into(),
        serial_number: Some(serial.into()),
    }
}

fn port(name: &str, serial: &str) -> SerialEndpoint {
    SerialEndpoint {
        port_name: name.into(),
        label: name.into(),
        manufacturer: None,
        product: None,
        kind: crate::domain::SerialPortKind::Usb,
        serial_number: Some(serial.into()),
        vid: Some(0x1209),
        pid: Some(0x7690),
        usb_interface: None,
    }
}

#[test]
fn usb_association_requires_unique_receiver_and_port_serials() {
    let mut inventory = Inventory {
        receivers: vec![
            receiver("first-path", "USB-A"),
            receiver("second-path", " usb-a "),
        ],
        ports: vec![port("COM7", "USB-A")],
        ..Inventory::default()
    };
    for receiver in &inventory.receivers {
        assert!(receiver_port(receiver, &inventory).is_none());
    }
    inventory.receivers.pop();
    assert_eq!(
        receiver_port(&inventory.receivers[0], &inventory).map(|port| port.port_name.as_str()),
        Some("COM7")
    );
    inventory.ports.push(port("COM8", "usb-a"));
    assert!(receiver_port(&inventory.receivers[0], &inventory).is_none());
}

#[test]
fn composite_receiver_groups_console_and_data_without_exposing_data_as_console() {
    let console = port("COM7", "USB-A");
    let mut raw = port("COM8", "USB-A");
    raw.usb_interface = Some(UsbInterface {
        number: Some(2),
        description: InterfaceDescription::Value("SlimeNRF Raw Data CDC".into()),
    });
    for ports in [
        vec![console.clone(), raw.clone()],
        vec![raw.clone(), console.clone()],
    ] {
        let inventory = Inventory {
            receivers: vec![receiver("receiver", "USB-A")],
            ports,
            ..Inventory::default()
        };
        let grouped = receiver_serial_ports(&inventory.receivers[0], &inventory).unwrap();
        assert_eq!(grouped.console.unwrap().port_name, "COM7");
        assert_eq!(grouped.raw_data.unwrap().port_name, "COM8");
        assert!(console_port_allowed(&console, &inventory));
        assert!(!console_port_allowed(&raw, &inventory));
    }
    let inventory = Inventory {
        ports: vec![raw.clone()],
        ..Inventory::default()
    };
    assert!(!console_port_allowed(&raw, &inventory));
}

#[test]
fn tracker_association_uses_only_connected_identity_evidence() {
    let tracker = TrackerLink {
        receiver_key: "receiver".into(),
        slot: 0,
        address: Some("AA:BB:CC:DD:EE:FF".into()),
        online: true,
    };
    let identity = DeviceIdentity {
        address: Some("AABBCCDDEEFF".into()),
        ..Default::default()
    };
    for state in [
        ConnectionState::Connecting,
        ConnectionState::Closing,
        ConnectionState::Disconnected,
        ConnectionState::Faulted,
    ] {
        assert_eq!(
            live_tracker_port(&tracker, [("COM7", state, &identity)]),
            None
        );
        assert_eq!(
            live_tracker_port(
                &tracker,
                [
                    ("COM7", state, &identity),
                    ("COM8", ConnectionState::Connected, &identity),
                ]
            ),
            Some("COM8")
        );
    }
    assert_eq!(
        live_tracker_port(
            &tracker,
            [
                ("COM7", ConnectionState::Connected, &identity),
                ("COM8", ConnectionState::Connected, &identity),
            ]
        ),
        None
    );
    let new_identity = DeviceIdentity::default();
    assert_eq!(
        live_tracker_port(
            &tracker,
            [("COM7", ConnectionState::Connected, &new_identity),]
        ),
        None
    );
    let receiver_identity = DeviceIdentity {
        role: DeviceRole::Receiver,
        address: identity.address.clone(),
        ..Default::default()
    };
    assert_eq!(
        live_tracker_port(
            &tracker,
            [("COM7", ConnectionState::Connected, &receiver_identity)]
        ),
        None
    );
}

#[test]
fn binding_invalidation_revokes_receiver_local_slots_and_address_focus_on_every_page() {
    use super::revoke_receiver_tracker_selection;
    use crate::application::command_panel::CommandFocus;

    let mut selected = [
        "serial:COM7".to_owned(),
        "tracker:a:receiver:0".to_owned(),
        "tracker:a:receiver:9".to_owned(),
        "tracker:b:receiver:0".to_owned(),
    ]
    .into();
    let mut focus = Some(CommandFocus::Tracker {
        receiver: "a:receiver".into(),
        slot: 0,
        address: Some("010203040506".into()),
    });
    revoke_receiver_tracker_selection("a:receiver", &mut selected, &mut focus);
    assert_eq!(
        selected,
        ["serial:COM7".to_owned(), "tracker:b:receiver:0".to_owned()].into()
    );
    assert!(focus.is_none());
    focus = Some(CommandFocus::Tracker {
        receiver: "b:receiver".into(),
        slot: 0,
        address: Some("010203040506".into()),
    });
    revoke_receiver_tracker_selection("a:receiver", &mut selected, &mut focus);
    assert!(
        matches!(focus, Some(CommandFocus::Tracker { receiver, .. }) if receiver == "b:receiver")
    );
}
