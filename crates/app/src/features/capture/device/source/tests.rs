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
fn uniquely_associated_console_and_raw_interface_are_required_in_either_order() {
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
        assert_eq!(source.console().unwrap().port_name, "console");
    }
    assert!(
        cdc_source(
            &receiver(),
            &inventory(vec![endpoint("console", Some(0), false)])
        )
        .is_none()
    );
    assert!(
        cdc_source(
            &receiver(),
            &inventory(vec![endpoint("data", Some(2), true)])
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
            endpoint("console", Some(0), false),
        ],
        vec![
            endpoint("data", Some(2), true),
            endpoint("console", Some(0), false),
            endpoint("other-console", Some(4), false),
        ],
    ] {
        assert!(cdc_source(&receiver(), &inventory(ports)).is_none());
    }
    let mut ambiguous = endpoint("data", Some(2), true);
    ambiguous.usb_interface.as_mut().unwrap().description = InterfaceDescription::Ambiguous;
    assert!(
        cdc_source(
            &receiver(),
            &inventory(vec![ambiguous, endpoint("console", Some(0), false)])
        )
        .is_none()
    );
    if cfg!(windows) {
        assert!(
            cdc_source(
                &receiver(),
                &inventory(vec![
                    endpoint("COM8", Some(0), false),
                    endpoint(r"\\.\com8", Some(2), true),
                ])
            )
            .is_none()
        );
    }
    let mut duplicated_receiver = inventory(vec![
        endpoint("data", Some(2), true),
        endpoint("console", Some(0), false),
    ]);
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
        &inventory(vec![
            endpoint("data", Some(2), true),
            endpoint("console", Some(0), false),
        ]),
    )
    .unwrap();
    let mut current = frozen.clone();
    current.receiver.label = "New receiver display".into();
    if let CaptureTransport::Cdc { endpoint, console } = &mut current.transport {
        console.label = "New console display".into();
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
            console: Box::new(endpoint("console", Some(0), false)),
        };
        assert!(!frozen.matches(&current));
    }
    for replacement in [
        endpoint("other-console", Some(0), false),
        endpoint("console", Some(4), false),
        endpoint("console", Some(0), true),
        SerialEndpoint {
            serial_number: Some("replacement".into()),
            ..endpoint("console", Some(0), false)
        },
    ] {
        let mut current = frozen.clone();
        let CaptureTransport::Cdc { console, .. } = &mut current.transport else {
            unreachable!()
        };
        **console = replacement;
        assert!(!frozen.matches(&current));
    }
    let valid = inventory(vec![
        endpoint("data", Some(2), true),
        endpoint("console", Some(0), false),
    ]);
    assert!(frozen.matches_inventory(&valid));
    let mut failed_enumeration = valid.clone();
    failed_enumeration.serial_enumeration_failed = true;
    assert!(!frozen.matches_inventory(&failed_enumeration));
    assert!(!frozen.matches_inventory(&inventory(vec![endpoint("data", Some(2), true)])));
    assert!(!frozen.matches_inventory(&inventory(vec![
        endpoint("data", Some(2), true),
        endpoint("replacement-console", Some(0), false),
    ])));
    assert!(!frozen.matches_inventory(&inventory(vec![
        endpoint("data", Some(2), true),
        endpoint("console", Some(0), false),
        endpoint("second-console", Some(4), false),
    ])));
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
    assert!(source.console().is_none());
    assert!(source.matches_inventory(&Inventory::default()));
    let mut current = source.clone();
    current.receiver.label = "Renamed".into();
    assert!(source.matches(&current));
    current.transport = CaptureTransport::RawHid { path: vec![1, 3] };
    assert!(!source.matches(&current));
}
