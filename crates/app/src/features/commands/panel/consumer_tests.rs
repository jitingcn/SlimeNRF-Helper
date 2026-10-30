use super::{
    CommandFocus, PrepareTarget, PrepareTargetSource, PreparedRouteKind, RemoteFocus, SerialGuard,
    SerialReview,
};
use crate::application::devices::{receiver_command_port, tracker_selectable};
use crate::{
    domain::{
        DeviceIdentity, DeviceRole, Inventory, ReceiverEndpoint, SerialEndpoint, TrackerLink,
        receiver_port,
    },
    features::commands::{
        batch::{BatchSnapshot, Outcome, Target, send_if_current},
        policy::RouteEvidence,
    },
    sessions::ConnectionState,
};

fn endpoint() -> SerialEndpoint {
    SerialEndpoint {
        port_name: "COM7".into(),
        label: "Receiver".into(),
        manufacturer: None,
        product: None,
        kind: crate::domain::SerialPortKind::Usb,
        serial_number: Some("USB-A".into()),
        vid: Some(0x1209),
        pid: Some(0x7690),
        usb_interface: None,
    }
}

fn inventory() -> Inventory {
    Inventory {
        ports: vec![endpoint()],
        receivers: vec![ReceiverEndpoint {
            key: "receiver-a".into(),
            path: vec![1],
            label: "Receiver".into(),
            serial_number: Some("USB-A".into()),
        }],
        ..Inventory::default()
    }
}

#[test]
fn receiver_commands_require_original_key_and_unique_usb_serial_not_vid_pid() {
    let original = inventory();
    assert_eq!(
        receiver_command_port("receiver-a", &original)
            .unwrap()
            .port_name,
        "COM7"
    );
    for scenario in 0..6 {
        let mut changed = inventory();
        match scenario {
            0 => changed.receivers.clear(),
            1 => changed.receivers[0].serial_number = None,
            2 => changed.ports[0].serial_number = Some("OTHER".into()),
            3 => {
                let mut duplicate = changed.receivers[0].clone();
                duplicate.key = "receiver-b".into();
                changed.receivers.push(duplicate);
            }
            4 => {
                let mut duplicate = changed.ports[0].clone();
                duplicate.port_name = "COM8".into();
                changed.ports.push(duplicate);
            }
            5 => changed.receivers[0].key = "receiver-b".into(),
            _ => unreachable!(),
        }
        assert!(
            receiver_command_port("receiver-a", &changed).is_err(),
            "unsafe receiver route accepted in scenario {scenario}"
        );
    }
}

#[test]
fn prepared_receiver_command_rejects_replacement_on_same_com_port() {
    let target = PrepareTarget {
        source: PrepareTargetSource::Current(CommandFocus::Receiver("receiver-a".into())),
        label: "Receiver".into(),
        port: "COM7".into(),
        endpoint: Some(endpoint()),
        route_kind: PreparedRouteKind::Direct,
    };
    let mut changed = inventory();
    changed.receivers[0].serial_number = Some("USB-B".into());
    changed.ports[0].serial_number = Some("USB-B".into());
    let replacement = receiver_command_port("receiver-a", &changed).unwrap();
    let route = RouteEvidence::Direct {
        port: &replacement.port_name,
        generation: 4,
        connected: true,
        role: DeviceRole::Receiver,
    };
    assert!(
        target
            .validate_route(&route, Some(replacement), &changed)
            .is_err()
    );
}

#[test]
fn prepared_command_route_requires_both_frozen_and_current_endpoints() {
    let route = RouteEvidence::Direct {
        port: "COM7",
        generation: 4,
        connected: true,
        role: DeviceRole::Receiver,
    };
    for (frozen, current) in [
        (None, None),
        (None, Some(endpoint())),
        (Some(endpoint()), None),
    ] {
        let target = PrepareTarget {
            source: PrepareTargetSource::Current(CommandFocus::Serial("COM7".into())),
            label: "Receiver".into(),
            port: "COM7".into(),
            endpoint: frozen,
            route_kind: PreparedRouteKind::Direct,
        };
        let mut inventory = inventory();
        inventory.ports = current.into_iter().collect();
        assert!(
            target
                .validate_route(&route, inventory.ports.first(), &inventory)
                .is_err()
        );
    }
}

fn review() -> SerialReview {
    // Two original selections share one canonical serial session. Both proofs must survive.
    SerialReview {
        snapshot: BatchSnapshot {
            command: "info".into(),
            targets: vec![Target {
                port: "COM7".into(),
                generation: 4,
            }],
            excluded: Vec::new(),
        },
        guards: vec![
            SerialGuard {
                port: "COM7".into(),
                target: PrepareTarget {
                    source: PrepareTargetSource::Current(CommandFocus::Serial("com7".into())),
                    label: "Left foot · COM7".into(),
                    port: "com7".into(),
                    endpoint: Some(endpoint()),
                    route_kind: PreparedRouteKind::Direct,
                },
            },
            SerialGuard {
                port: "COM7".into(),
                target: PrepareTarget {
                    source: PrepareTargetSource::Remote(RemoteFocus::All("receiver-a".into())),
                    label: "Receiver".into(),
                    port: "COM7".into(),
                    endpoint: Some(endpoint()),
                    route_kind: PreparedRouteKind::Broadcast,
                },
            },
        ],
    }
}

fn submit_review(
    review: SerialReview,
    inventory: &Inventory,
    generation: u64,
    change_route: bool,
) -> (usize, crate::features::commands::batch::BatchRun) {
    let mut writes = 0;
    let admission = review.authorize_all(|guard| {
        let route = match &guard.source {
            PrepareTargetSource::Current(CommandFocus::Serial(port)) if !change_route => {
                RouteEvidence::Direct {
                    port,
                    generation,
                    connected: true,
                    role: DeviceRole::Receiver,
                }
            }
            PrepareTargetSource::Remote(RemoteFocus::All(key)) => {
                let receiver = inventory
                    .receivers
                    .iter()
                    .find(|receiver| &receiver.key == key)
                    .ok_or("Receiver missing")?;
                let port = receiver_port(receiver, inventory).ok_or("Ambiguous receiver route")?;
                RouteEvidence::Broadcast {
                    port: &port.port_name,
                    generation,
                    connected: true,
                    receiver_key: key,
                    unique_receiver_route: true,
                }
            }
            _ => RouteEvidence::Forwarded {
                port: &guard.port,
                generation,
                connected: true,
                receiver_key: "receiver-a",
                slot: 2,
                address: Some("AABBCCDDEEFF"),
                unique_receiver_route: true,
            },
        };
        guard.validate_route(&route, inventory.ports.first(), inventory)
    });
    let run = review.execute(admission, |review, target| {
        send_if_current(
            target,
            &review.snapshot.command,
            Some((ConnectionState::Connected, generation)),
            |_, command| {
                assert_eq!(command, "info");
                writes += 1;
                Err("Write queue unavailable".into())
            },
        )
    });
    (writes, run)
}

#[test]
fn serial_review_keeps_all_original_guards_and_writes_once_per_canonical_port() {
    let (writes, run) = submit_review(review(), &inventory(), 4, false);
    assert_eq!(writes, 1);
    assert_eq!(run.targets.len(), 1);
    assert_eq!(run.targets[0].label, "Left foot · COM7");
}

#[test]
fn serial_review_rechecks_endpoint_route_generation_and_receiver_uniqueness_before_submit() {
    let mut changed_endpoint = inventory();
    changed_endpoint.ports[0].serial_number = Some("USB-B".into());
    let mut ambiguous = inventory();
    let mut duplicate = ambiguous.receivers[0].clone();
    duplicate.key = "receiver-b".into();
    ambiguous.receivers.push(duplicate);
    for (inventory, generation, route_changed) in [
        (changed_endpoint, 4, false),
        (ambiguous, 4, false),
        (inventory(), 5, false),
        (inventory(), 4, true),
    ] {
        let (writes, run) = submit_review(review(), &inventory, generation, route_changed);
        assert_eq!(writes, 0);
        assert!(matches!(run.targets[0].outcome, Outcome::Failed(_)));
    }
    let mut missing = review();
    missing.guards.clear();
    assert_eq!(submit_review(missing, &inventory(), 4, false).0, 0);
}

#[test]
fn silent_tracker_selection_requires_unique_connected_direct_identity() {
    let tracker = TrackerLink {
        receiver_key: "receiver-a".into(),
        slot: 2,
        address: Some("AABBCCDDEEFF".into()),
        online: false,
    };
    let identity = DeviceIdentity {
        role: DeviceRole::Tracker,
        address: tracker.address.clone(),
        ..Default::default()
    };
    assert!(tracker_selectable(
        &tracker,
        [("COM8", ConnectionState::Connected, &identity)]
    ));
    for state in [
        ConnectionState::Connecting,
        ConnectionState::Disconnected,
        ConnectionState::Closing,
    ] {
        assert!(!tracker_selectable(&tracker, [("COM8", state, &identity)]));
    }
    assert!(!tracker_selectable(
        &tracker,
        [
            ("COM8", ConnectionState::Connected, &identity),
            ("COM9", ConnectionState::Connected, &identity),
        ]
    ));
    let receiver = DeviceIdentity {
        role: DeviceRole::Receiver,
        ..identity.clone()
    };
    assert!(!tracker_selectable(
        &tracker,
        [("COM8", ConnectionState::Connected, &receiver)]
    ));
    let different = DeviceIdentity {
        address: Some("112233445566".into()),
        ..identity
    };
    assert!(!tracker_selectable(
        &tracker,
        [("COM8", ConnectionState::Connected, &different)]
    ));
}

#[test]
fn late_calibration_conflict_rejects_whole_serial_batch_before_enqueue() {
    for blocked in [false, true] {
        let mut review = review();
        review.snapshot.targets.push(Target {
            port: "COM8".into(),
            generation: 4,
        });
        review.guards.push(SerialGuard {
            port: "COM8".into(),
            target: PrepareTarget {
                source: PrepareTargetSource::Current(CommandFocus::Serial("COM8".into())),
                label: "Tracker".into(),
                port: "COM8".into(),
                endpoint: None,
                route_kind: PreparedRouteKind::Direct,
            },
        });
        let admission = review.authorize_all(|target| {
            if blocked && target.port == "COM8" {
                Err("Calibration is observing COM8".into())
            } else {
                Ok(())
            }
        });
        let mut writes = Vec::new();
        let run = review.execute(admission, |review, target| {
            writes.push((target.port.clone(), review.snapshot.command.clone()));
            Err("Queue full".into())
        });
        if blocked {
            assert_eq!(writes, []);
            assert_eq!(
                run.targets
                    .iter()
                    .map(|target| &target.outcome)
                    .collect::<Vec<_>>(),
                [
                    &Outcome::Failed("Calibration is observing COM8".into()),
                    &Outcome::Failed("Calibration is observing COM8".into())
                ],
            );
        } else {
            assert_eq!(
                writes,
                [
                    ("COM7".into(), "info".into()),
                    ("COM8".into(), "info".into())
                ]
            );
        }
    }
}

#[test]
fn all_prepared_console_routes_revalidate_new_siblings_even_when_endpoint_is_unchanged() {
    for kind in [
        PreparedRouteKind::Direct,
        PreparedRouteKind::Forwarded,
        PreparedRouteKind::Broadcast,
    ] {
        let mut inventory = inventory();
        let target = PrepareTarget {
            source: PrepareTargetSource::Current(CommandFocus::Receiver("receiver-a".into())),
            label: "Receiver".into(),
            port: "COM7".into(),
            endpoint: Some(endpoint()),
            route_kind: kind,
        };
        let route = match kind {
            PreparedRouteKind::Direct => RouteEvidence::Direct {
                port: "COM7",
                generation: 4,
                connected: true,
                role: DeviceRole::Receiver,
            },
            PreparedRouteKind::Forwarded => RouteEvidence::Forwarded {
                port: "COM7",
                generation: 4,
                connected: true,
                receiver_key: "receiver-a",
                slot: 2,
                address: Some("AABBCCDDEEFF"),
                unique_receiver_route: true,
            },
            PreparedRouteKind::Broadcast => RouteEvidence::Broadcast {
                port: "COM7",
                generation: 4,
                connected: true,
                receiver_key: "receiver-a",
                unique_receiver_route: true,
            },
        };
        assert!(
            target
                .validate_route(&route, inventory.ports.first(), &inventory)
                .is_ok()
        );
        let mut raw = endpoint();
        raw.port_name = "COM8".into();
        raw.usb_interface = Some(crate::domain::UsbInterface {
            number: Some(2),
            description: crate::domain::InterfaceDescription::Value("SlimeNRF Raw Data CDC".into()),
        });
        inventory.ports.push(raw);
        assert!(
            target
                .validate_route(&route, inventory.ports.first(), &inventory)
                .is_ok()
        );
        let mut unknown = endpoint();
        unknown.port_name = "COM9".into();
        inventory.ports.push(unknown);
        assert!(
            target
                .validate_route(&route, inventory.ports.first(), &inventory)
                .is_err()
        );
    }
}

#[test]
fn prepared_direct_route_cannot_use_raw_interface_even_with_retained_receiver_role() {
    let mut inventory = inventory();
    inventory.ports[0].usb_interface = Some(crate::domain::UsbInterface {
        number: Some(2),
        description: crate::domain::InterfaceDescription::Value("SlimeNRF Raw Data CDC".into()),
    });
    let target = PrepareTarget {
        source: PrepareTargetSource::Current(CommandFocus::Serial("COM7".into())),
        label: "Receiver".into(),
        port: "COM7".into(),
        endpoint: inventory.ports.first().cloned(),
        route_kind: PreparedRouteKind::Direct,
    };
    let route = RouteEvidence::Direct {
        port: "COM7",
        generation: 4,
        connected: true,
        role: DeviceRole::Receiver,
    };
    assert!(
        target
            .validate_route(&route, inventory.ports.first(), &inventory)
            .is_err()
    );
}

#[test]
fn frozen_command_route_rejects_interface_number_or_native_evidence_replacement() {
    for description in [
        crate::domain::InterfaceDescription::Unavailable,
        crate::domain::InterfaceDescription::Value("Console".into()),
    ] {
        let mut inventory = inventory();
        let target = PrepareTarget {
            source: PrepareTargetSource::Current(CommandFocus::Serial("COM7".into())),
            label: "Receiver".into(),
            port: "COM7".into(),
            endpoint: Some(endpoint()),
            route_kind: PreparedRouteKind::Direct,
        };
        inventory.ports[0].usb_interface = Some(crate::domain::UsbInterface {
            number: Some(0),
            description,
        });
        let route = RouteEvidence::Direct {
            port: "COM7",
            generation: 4,
            connected: true,
            role: DeviceRole::Receiver,
        };
        assert!(
            target
                .validate_route(&route, inventory.ports.first(), &inventory)
                .is_err()
        );
    }
}
