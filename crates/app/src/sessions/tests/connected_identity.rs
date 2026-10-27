use super::super::{ConnectionState, SessionManager, port_key};
use super::{connected, fixture, peer, stopped, until};
use crate::domain::{
    DeviceRole, InterfaceDescription, Inventory, SerialEndpoint, SerialPortKind, UsbInterface,
};
use std::io::Write;
use std::net::TcpStream;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

fn endpoint() -> SerialEndpoint {
    SerialEndpoint {
        port_name: "COM41".into(),
        label: "Fixture tracker".into(),
        manufacturer: None,
        product: None,
        kind: SerialPortKind::Usb,
        serial_number: Some("fixture-original".into()),
        vid: Some(0x1209),
        pid: Some(0x7692),
        usb_interface: Some(UsbInterface {
            number: Some(0),
            description: InterfaceDescription::Unavailable,
        }),
    }
}

fn inventory(ports: Vec<SerialEndpoint>) -> Inventory {
    Inventory {
        ports,
        ..Inventory::default()
    }
}

fn connect_identity(
    endpoint: &SerialEndpoint,
    inventory: &Inventory,
) -> (SessionManager, TcpStream, u64) {
    let (mut manager, listener) = fixture();
    manager.update_inventory(inventory, Instant::now());
    manager.connect(&endpoint.port_name).unwrap();
    let generation = manager.session(&endpoint.port_name).unwrap().generation;
    assert!(
        manager
            .connected_identity_for(endpoint, generation)
            .is_none()
    );
    let mut stream = peer(&listener);
    connected(&mut manager, &endpoint.port_name);
    stream
        .write_all(
            b"Device name: Fixture Tracker\nDevice address: 112233445566\nTarget: fixture-target\nBoard: fixture-board\nTracker ID: 7\n",
        )
        .unwrap();
    until(|| {
        let _ = manager.poll();
        manager
            .session(&endpoint.port_name)
            .unwrap()
            .identity
            .tracker_id
            == Some(7)
    });
    (manager, stream, generation)
}

fn assert_identity(manager: &SessionManager, endpoint: &SerialEndpoint, generation: u64) {
    let identity = manager
        .connected_identity_for(endpoint, generation)
        .expect("matching live USB generation must supply identity evidence");
    assert_eq!(identity.role, DeviceRole::Tracker);
    assert_eq!(identity.name.as_deref(), Some("Fixture Tracker"));
    assert_eq!(identity.address.as_deref(), Some("112233445566"));
    assert_eq!(identity.target.as_deref(), Some("fixture-target"));
    assert_eq!(identity.board.as_deref(), Some("fixture-board"));
    assert_eq!(identity.tracker_id, Some(7));
    assert!(std::ptr::eq(
        identity,
        &manager.session(&endpoint.port_name).unwrap().identity
    ));
}

#[test]
fn connected_identity_borrows_only_the_matching_live_usb_generation() {
    let endpoint = endpoint();
    let (manager, _stream, generation) =
        connect_identity(&endpoint, &inventory(vec![endpoint.clone()]));
    assert_identity(&manager, &endpoint, generation);
    assert!(
        manager
            .connected_identity_for(&endpoint, generation + 1)
            .is_none()
    );
    assert!(manager.connected_identity_for(&endpoint, 0).is_none());
    for caller in [
        SerialEndpoint {
            port_name: "COM42".into(),
            ..endpoint.clone()
        },
        SerialEndpoint {
            serial_number: Some("fixture-replacement".into()),
            ..endpoint.clone()
        },
        SerialEndpoint {
            serial_number: None,
            ..endpoint.clone()
        },
        SerialEndpoint {
            serial_number: Some(" ".into()),
            ..endpoint.clone()
        },
        SerialEndpoint {
            vid: Some(0x239a),
            ..endpoint.clone()
        },
        SerialEndpoint {
            vid: None,
            ..endpoint.clone()
        },
        SerialEndpoint {
            pid: Some(0x7690),
            ..endpoint.clone()
        },
        SerialEndpoint {
            pid: None,
            ..endpoint.clone()
        },
        SerialEndpoint {
            usb_interface: None,
            ..endpoint.clone()
        },
        SerialEndpoint {
            usb_interface: Some(UsbInterface {
                number: Some(2),
                description: InterfaceDescription::Unavailable,
            }),
            ..endpoint.clone()
        },
        SerialEndpoint {
            usb_interface: Some(UsbInterface {
                number: Some(0),
                description: InterfaceDescription::Ambiguous,
            }),
            ..endpoint.clone()
        },
        SerialEndpoint {
            kind: SerialPortKind::Unknown,
            ..endpoint.clone()
        },
    ] {
        assert!(
            manager
                .connected_identity_for(&caller, generation)
                .is_none()
        );
    }
    let relabeled = SerialEndpoint {
        label: "Different display label".into(),
        manufacturer: Some("Different manufacturer text".into()),
        product: Some("Different product text".into()),
        ..endpoint
    };
    assert_identity(&manager, &relabeled, generation);
}

#[test]
fn a_replacement_at_the_same_port_requires_its_own_generation_and_report() {
    let original = endpoint();
    let replacement = SerialEndpoint {
        serial_number: Some("fixture-replacement".into()),
        ..original.clone()
    };
    let (mut manager, listener) = fixture();
    manager.update_inventory(&inventory(vec![original.clone()]), Instant::now());
    manager.connect(&original.port_name).unwrap();
    let mut old = peer(&listener);
    connected(&mut manager, &original.port_name);
    old.write_all(b"Device name: Original Tracker\nDevice address: 112233445566\n")
        .unwrap();
    until(|| {
        let _ = manager.poll();
        manager
            .session(&original.port_name)
            .unwrap()
            .identity
            .address
            .as_deref()
            == Some("112233445566")
    });
    let generation = manager.session(&original.port_name).unwrap().generation;
    assert_eq!(
        manager
            .connected_identity_for(&original, generation)
            .unwrap()
            .address
            .as_deref(),
        Some("112233445566")
    );
    manager.update_inventory(&inventory(vec![replacement.clone()]), Instant::now());
    assert!(
        manager
            .connected_identity_for(&replacement, generation)
            .is_none()
    );
    assert!(
        manager
            .connected_identity_for(&original, generation)
            .is_none()
    );
    drop(old);
    stopped(&mut manager, &original.port_name);
    manager.connect(&replacement.port_name).unwrap();
    let mut new = peer(&listener);
    connected(&mut manager, &replacement.port_name);
    let replacement_generation = manager.session(&replacement.port_name).unwrap().generation;
    assert_eq!(replacement_generation, generation + 1);
    assert!(
        manager
            .connected_identity_for(&replacement, generation)
            .is_none()
    );
    assert!(
        manager
            .connected_identity_for(&replacement, replacement_generation)
            .unwrap()
            .address
            .is_none()
    );
    new.write_all(b"Device name: Replacement Tracker\nDevice address: AABBCCDDEEFF\n")
        .unwrap();
    until(|| {
        let _ = manager.poll();
        manager
            .session(&replacement.port_name)
            .unwrap()
            .identity
            .address
            .as_deref()
            == Some("AABBCCDDEEFF")
    });
    let identity = manager
        .connected_identity_for(&replacement, replacement_generation)
        .unwrap();
    assert_eq!(identity.name.as_deref(), Some("Replacement Tracker"));
    assert_eq!(identity.address.as_deref(), Some("AABBCCDDEEFF"));
}

#[test]
fn current_inventory_revokes_stale_identity_without_relabeling_the_connected_owner() {
    let endpoint = endpoint();
    let valid = inventory(vec![endpoint.clone()]);
    let (mut manager, _stream, generation) = connect_identity(&endpoint, &valid);
    assert_identity(&manager, &endpoint, generation);
    for invalid in [
        inventory(vec![]),
        inventory(vec![SerialEndpoint {
            kind: SerialPortKind::Unknown,
            ..endpoint.clone()
        }]),
        inventory(vec![SerialEndpoint {
            serial_number: Some("fixture-replacement".into()),
            ..endpoint.clone()
        }]),
        inventory(vec![SerialEndpoint {
            vid: Some(0x239a),
            ..endpoint.clone()
        }]),
        inventory(vec![SerialEndpoint {
            pid: Some(0x7690),
            ..endpoint.clone()
        }]),
        inventory(vec![SerialEndpoint {
            usb_interface: None,
            ..endpoint.clone()
        }]),
        inventory(vec![SerialEndpoint {
            usb_interface: Some(UsbInterface {
                number: Some(2),
                description: InterfaceDescription::Unavailable,
            }),
            ..endpoint.clone()
        }]),
        inventory(vec![endpoint.clone(), endpoint.clone()]),
        inventory(vec![
            endpoint.clone(),
            SerialEndpoint {
                port_name: "COM42".into(),
                ..endpoint.clone()
            },
        ]),
        inventory(vec![SerialEndpoint {
            serial_number: None,
            ..endpoint.clone()
        }]),
        inventory(vec![SerialEndpoint {
            serial_number: Some(" ".into()),
            ..endpoint.clone()
        }]),
        inventory(vec![SerialEndpoint {
            vid: None,
            ..endpoint.clone()
        }]),
        inventory(vec![SerialEndpoint {
            pid: None,
            ..endpoint.clone()
        }]),
        inventory(vec![SerialEndpoint {
            usb_interface: Some(UsbInterface {
                number: Some(0),
                description: InterfaceDescription::Ambiguous,
            }),
            ..endpoint.clone()
        }]),
    ] {
        manager.update_inventory(&invalid, Instant::now());
        assert!(
            manager
                .connected_identity_for(&endpoint, generation)
                .is_none()
        );
        for current in &invalid.ports {
            assert!(
                manager
                    .connected_identity_for(current, generation)
                    .is_none()
            );
        }
        let stale = manager.session(&endpoint.port_name).unwrap();
        assert_eq!(stale.state, ConnectionState::Connected);
        assert_eq!(stale.generation, generation);
        assert_eq!(stale.identity.address.as_deref(), Some("112233445566"));
        manager.update_inventory(&valid, Instant::now());
        assert_identity(&manager, &endpoint, generation);
    }
}

#[test]
fn failed_or_stale_enumeration_revokes_identity_until_a_successful_refresh() {
    let endpoint = endpoint();
    let valid = inventory(vec![endpoint.clone()]);
    let (mut manager, _stream, generation) = connect_identity(&endpoint, &valid);
    assert_identity(&manager, &endpoint, generation);
    manager.invalidate_inventory();
    assert!(
        manager
            .connected_identity_for(&endpoint, generation)
            .is_none()
    );
    manager.update_inventory(&valid, Instant::now());
    assert_identity(&manager, &endpoint, generation);
    manager.update_inventory(
        &Inventory {
            serial_enumeration_failed: true,
            ..valid.clone()
        },
        Instant::now(),
    );
    assert!(
        manager
            .connected_identity_for(&endpoint, generation)
            .is_none()
    );
    manager.update_inventory(&valid, Instant::now());
    assert_identity(&manager, &endpoint, generation);
    let stale = Instant::now() - Duration::from_secs(11);
    manager.update_inventory_at(&valid, stale, stale);
    assert!(
        manager
            .connected_identity_for(&endpoint, generation)
            .is_none()
    );
    manager.update_inventory(&valid, Instant::now());
    assert_identity(&manager, &endpoint, generation);
}

#[test]
fn a_late_unique_inventory_cannot_manufacture_the_opened_generations_usb_pin() {
    let endpoint = endpoint();
    let (manager, _stream, generation) =
        connect_identity(&endpoint, &inventory(vec![endpoint.clone()]));
    assert_identity(&manager, &endpoint, generation);
    for initial in [
        inventory(vec![]),
        inventory(vec![SerialEndpoint {
            serial_number: None,
            ..endpoint.clone()
        }]),
        inventory(vec![
            endpoint.clone(),
            SerialEndpoint {
                port_name: "COM42".into(),
                ..endpoint.clone()
            },
        ]),
    ] {
        let (mut manager, _stream, generation) = connect_identity(&endpoint, &initial);
        assert!(
            manager
                .connected_identity_for(&endpoint, generation)
                .is_none()
        );
        manager.update_inventory(&inventory(vec![endpoint.clone()]), Instant::now());
        assert!(
            manager
                .connected_identity_for(&endpoint, generation)
                .is_none()
        );
        assert_eq!(
            manager
                .session(&endpoint.port_name)
                .unwrap()
                .identity
                .address
                .as_deref(),
            Some("112233445566")
        );
    }
}

#[test]
fn a_receiver_raw_interface_is_not_identity_evidence_for_its_console() {
    let console = SerialEndpoint {
        pid: Some(0x7690),
        ..endpoint()
    };
    let raw = SerialEndpoint {
        port_name: "COM42".into(),
        usb_interface: Some(UsbInterface {
            number: Some(2),
            description: InterfaceDescription::Value("SlimeNRF Raw Data CDC".into()),
        }),
        ..console.clone()
    };
    let valid = Inventory {
        ports: vec![console.clone(), raw.clone()],
        receivers: vec![crate::domain::ReceiverEndpoint {
            key: "fixture-receiver".into(),
            path: b"fixture-hid".to_vec(),
            label: "Fixture receiver".into(),
            serial_number: console.serial_number.clone(),
        }],
        ..Inventory::default()
    };
    let (mut manager, _stream, generation) = connect_identity(&console, &valid);
    assert_identity(&manager, &console, generation);
    assert!(manager.connected_identity_for(&raw, generation).is_none());
    let reused_as_raw = SerialEndpoint {
        port_name: console.port_name.clone(),
        usb_interface: Some(UsbInterface {
            number: Some(0),
            description: InterfaceDescription::Value("SlimeNRF Raw Data CDC".into()),
        }),
        ..raw
    };
    assert!(
        manager
            .connected_identity_for(&reused_as_raw, generation)
            .is_none()
    );
    manager.update_inventory(&inventory(vec![reused_as_raw.clone()]), Instant::now());
    assert!(
        manager
            .connected_identity_for(&console, generation)
            .is_none()
    );
    assert!(
        manager
            .connected_identity_for(&reused_as_raw, generation)
            .is_none()
    );
    manager.update_inventory(&valid, Instant::now());
    assert_identity(&manager, &console, generation);
}

#[test]
fn worker_liveness_is_required_even_while_the_view_still_says_connected() {
    let endpoint = endpoint();
    let (mut manager, stream, generation) =
        connect_identity(&endpoint, &inventory(vec![endpoint.clone()]));
    let key = port_key(&endpoint.port_name);
    assert_identity(&manager, &endpoint, generation);
    manager
        .entries
        .get_mut(&key)
        .unwrap()
        .worker
        .as_mut()
        .unwrap()
        .generation += 1;
    assert!(
        manager
            .connected_identity_for(&endpoint, generation)
            .is_none()
    );
    manager
        .entries
        .get_mut(&key)
        .unwrap()
        .worker
        .as_mut()
        .unwrap()
        .generation = generation;
    assert_identity(&manager, &endpoint, generation);
    manager
        .entries
        .get_mut(&key)
        .unwrap()
        .worker
        .as_mut()
        .unwrap()
        .closed = true;
    assert!(
        manager
            .connected_identity_for(&endpoint, generation)
            .is_none()
    );
    manager
        .entries
        .get_mut(&key)
        .unwrap()
        .worker
        .as_mut()
        .unwrap()
        .closed = false;
    assert_identity(&manager, &endpoint, generation);
    let cancel = std::mem::replace(
        &mut manager
            .entries
            .get_mut(&key)
            .unwrap()
            .worker
            .as_mut()
            .unwrap()
            .cancel,
        Arc::new(AtomicBool::new(true)),
    );
    assert!(
        manager
            .connected_identity_for(&endpoint, generation)
            .is_none()
    );
    manager
        .entries
        .get_mut(&key)
        .unwrap()
        .worker
        .as_mut()
        .unwrap()
        .cancel = cancel;
    assert_identity(&manager, &endpoint, generation);
    let handle = manager
        .entries
        .get_mut(&key)
        .unwrap()
        .worker
        .as_mut()
        .unwrap()
        .handle
        .take();
    assert!(
        manager
            .connected_identity_for(&endpoint, generation)
            .is_none()
    );
    manager
        .entries
        .get_mut(&key)
        .unwrap()
        .worker
        .as_mut()
        .unwrap()
        .handle = handle;
    assert_identity(&manager, &endpoint, generation);
    let worker = manager.entries.get_mut(&key).unwrap().worker.take();
    assert!(
        manager
            .connected_identity_for(&endpoint, generation)
            .is_none()
    );
    manager.entries.get_mut(&key).unwrap().worker = worker;
    assert_identity(&manager, &endpoint, generation);
    drop(stream);
    until(|| {
        manager.entries[&key]
            .worker
            .as_ref()
            .unwrap()
            .handle
            .as_ref()
            .unwrap()
            .is_finished()
    });
    assert_eq!(
        manager.session(&endpoint.port_name).unwrap().state,
        ConnectionState::Connected
    );
    assert!(
        manager
            .connected_identity_for(&endpoint, generation)
            .is_none()
    );
    stopped(&mut manager, &endpoint.port_name);
    assert!(
        manager
            .connected_identity_for(&endpoint, generation)
            .is_none()
    );
}

#[test]
fn disconnect_reservation_and_shutdown_revoke_connected_identity() {
    let endpoint = endpoint();
    let valid = inventory(vec![endpoint.clone()]);
    let (mut manager, _stream, generation) = connect_identity(&endpoint, &valid);
    assert_identity(&manager, &endpoint, generation);
    manager.disconnect(&endpoint.port_name).unwrap();
    assert_eq!(
        manager.session(&endpoint.port_name).unwrap().state,
        ConnectionState::Closing
    );
    assert!(
        manager
            .connected_identity_for(&endpoint, generation)
            .is_none()
    );

    let (mut manager, _stream, generation) = connect_identity(&endpoint, &valid);
    assert_identity(&manager, &endpoint, generation);
    let reservation = manager
        .reserve(&endpoint.port_name, Some(generation))
        .unwrap();
    assert!(
        manager
            .connected_identity_for(&endpoint, generation)
            .is_none()
    );
    manager.release(&reservation).unwrap();
    assert!(
        manager
            .connected_identity_for(&endpoint, generation)
            .is_none()
    );

    let (mut manager, _stream, generation) = connect_identity(&endpoint, &valid);
    assert_identity(&manager, &endpoint, generation);
    let reaper = manager.shutdown();
    assert!(
        manager
            .connected_identity_for(&endpoint, generation)
            .is_none()
    );
    if let Some(reaper) = reaper {
        reaper.join().unwrap();
    }
}
