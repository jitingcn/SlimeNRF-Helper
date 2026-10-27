use super::{CaptureSource, CaptureTransport, cdc_source};
use crate::domain::{
    InterfaceDescription, Inventory, ReceiverEndpoint, SerialEndpoint, SerialPortKind, UsbInterface,
};

fn receiver() -> ReceiverEndpoint {
    ReceiverEndpoint {
        key: "synthetic-main".into(),
        path: b"synthetic-main-path".to_vec(),
        label: "Receiver".into(),
        serial_number: Some("synthetic-usb".into()),
    }
}

fn endpoint(port: &str, number: Option<u8>, raw: bool) -> SerialEndpoint {
    SerialEndpoint {
        port_name: port.into(),
        label: port.into(),
        manufacturer: None,
        product: Some("Receiver".into()),
        kind: SerialPortKind::Usb,
        serial_number: receiver().serial_number,
        vid: Some(0x1209),
        pid: Some(0x7690),
        usb_interface: Some(UsbInterface {
            number,
            description: if raw {
                InterfaceDescription::Value("SlimeNRF Raw Data CDC".into())
            } else {
                InterfaceDescription::Unavailable
            },
        }),
    }
}

fn inventory(ports: Vec<SerialEndpoint>) -> Inventory {
    Inventory {
        receivers: vec![receiver()],
        ports,
        ..Inventory::default()
    }
}

#[test]
fn only_explicit_raw_interface_becomes_cdc_source_in_either_order() {
    for ports in [
        vec![
            endpoint("console", Some(0), false),
            endpoint("data", Some(2), true),
        ],
        vec![
            endpoint("data", Some(2), true),
            endpoint("console", Some(0), false),
        ],
    ] {
        let source = cdc_source(&receiver(), &inventory(ports)).unwrap();
        assert_eq!(source.serial_port(), Some("data"));
    }
    assert!(
        cdc_source(
            &receiver(),
            &inventory(vec![endpoint("console", Some(0), false)])
        )
        .is_none()
    );
    let raw_label_only = SerialEndpoint {
        label: "SlimeNRF Raw Data CDC".into(),
        product: Some("SlimeNRF Raw Data CDC".into()),
        ..endpoint("console", Some(0), false)
    };
    assert!(cdc_source(&receiver(), &inventory(vec![raw_label_only])).is_none());
}

#[test]
fn ambiguous_native_functions_cannot_become_capture_sources() {
    for ports in [
        vec![
            endpoint("console", Some(2), false),
            endpoint("data", Some(2), true),
        ],
        vec![
            endpoint("data", Some(2), true),
            endpoint("other-data", Some(4), true),
        ],
    ] {
        assert!(cdc_source(&receiver(), &inventory(ports)).is_none());
    }
    let mut ambiguous = endpoint("data", Some(2), true);
    ambiguous.usb_interface.as_mut().unwrap().description = InterfaceDescription::Ambiguous;
    assert!(cdc_source(&receiver(), &inventory(vec![ambiguous])).is_none());
    let mut duplicated_receiver = inventory(vec![endpoint("data", Some(2), true)]);
    let mut other = receiver();
    other.key = "other-main".into();
    other.path = b"other-path".to_vec();
    duplicated_receiver.receivers.push(other);
    assert!(cdc_source(&receiver(), &duplicated_receiver).is_none());
}

#[test]
fn frozen_source_ignores_display_changes_but_rejects_native_replacement() {
    let frozen = cdc_source(
        &receiver(),
        &inventory(vec![endpoint("data", Some(2), true)]),
    )
    .unwrap();
    let mut current = frozen.clone();
    current.receiver.label = "New receiver display".into();
    if let CaptureTransport::Cdc { endpoint } = &mut current.transport {
        endpoint.label = "New serial display".into();
        endpoint.product = Some("Display product changed".into());
        endpoint.manufacturer = Some("Display manufacturer changed".into());
    }
    assert!(frozen.matches(&current));
    for replacement in [
        endpoint("data", Some(4), true),
        endpoint("data", Some(2), false),
        endpoint("other", Some(2), true),
        SerialEndpoint {
            serial_number: Some("replacement".into()),
            ..endpoint("data", Some(2), true)
        },
        SerialEndpoint {
            vid: Some(0x239a),
            ..endpoint("data", Some(2), true)
        },
    ] {
        current.transport = CaptureTransport::Cdc {
            endpoint: replacement,
        };
        assert!(!frozen.matches(&current));
    }
    current = frozen.clone();
    current.receiver.path.push(0);
    assert!(!frozen.matches(&current));
    current = frozen.clone();
    current.receiver.key = "replacement".into();
    assert!(!frozen.matches(&current));
    current = frozen.clone();
    current.receiver.serial_number = Some("replacement".into());
    assert!(!frozen.matches(&current));
}

#[test]
fn raw_hid_matching_preserves_receiver_and_exact_data_path() {
    let source = CaptureSource {
        receiver: receiver(),
        transport: CaptureTransport::RawHid { path: vec![1, 2] },
    };
    let mut current = source.clone();
    current.receiver.label = "Renamed".into();
    assert!(source.matches(&current));
    current.transport = CaptureTransport::RawHid { path: vec![1, 3] };
    assert!(!source.matches(&current));
}
