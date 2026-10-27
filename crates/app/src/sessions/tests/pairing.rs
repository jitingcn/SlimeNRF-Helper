use super::super::{SessionManager, port_key};
use super::{completed, connected, fixture, peer, stopped, until};
use crate::domain::{DeviceRole, Inventory, ReceiverEndpoint, SerialEndpoint, SerialPortKind};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

const SUCCESS: &[u8] = b"[00:00:20.018,035] <inf> esb_event: Pairings cleared and saved\r\n";

fn inventory() -> Inventory {
    Inventory {
        ports: vec![SerialEndpoint {
            port_name: "receiver-console".into(),
            label: "Synthetic receiver console".into(),
            manufacturer: None,
            product: None,
            kind: SerialPortKind::Usb,
            serial_number: Some("synthetic-receiver".into()),
            vid: Some(0x1209),
            pid: Some(0x7690),
            usb_interface: None,
        }],
        receivers: vec![ReceiverEndpoint {
            key: "synthetic-hid".into(),
            path: b"synthetic-hid".to_vec(),
            label: "Synthetic receiver".into(),
            serial_number: Some("synthetic-receiver".into()),
        }],
        ..Inventory::default()
    }
}

fn receiver() -> (SessionManager, TcpStream, u64) {
    let (mut manager, listener) = fixture();
    manager.update_inventory(&inventory(), Instant::now());
    manager.connect("receiver-console").unwrap();
    let mut stream = peer(&listener);
    connected(&mut manager, "receiver-console");
    stream
        .write_all(b"Device name: SlimeNRF Receiver\n")
        .unwrap();
    until(|| {
        let _ = manager.poll();
        manager.session("receiver-console").unwrap().identity.role == DeviceRole::Receiver
    });
    let generation = manager.session("receiver-console").unwrap().generation;
    (manager, stream, generation)
}

fn pending(manager: &SessionManager) -> bool {
    manager.entries[&port_key("receiver-console")]
        .worker
        .as_ref()
        .unwrap()
        .identity
        .lock()
        .pairing_changed
}

fn observed(manager: &SessionManager) {
    until(|| pending(manager));
}

#[test]
fn successful_write_is_not_mutation_but_received_receiver_success_is() {
    let (mut manager, mut stream, generation) = receiver();
    let mut write = manager.send_line("receiver-console", "clear").unwrap();
    assert!(completed(&mut write).is_ok());
    let mut command = [0; 6];
    stream.read_exact(&mut command).unwrap();
    assert_eq!(&command, b"clear\n");
    assert!(manager.take_receiver_pairing_changes().is_empty());
    stream.write_all(SUCCESS).unwrap();
    observed(&manager);
    assert_eq!(
        manager.take_receiver_pairing_changes(),
        [("receiver-console".to_owned(), generation)]
    );
    assert!(manager.take_receiver_pairing_changes().is_empty());
}

#[test]
fn pending_success_is_rejected_after_session_or_usb_identity_authority_changes() {
    for transition in [
        "disconnect",
        "reserve",
        "generation",
        "cancel",
        "inventory",
        "role",
    ] {
        let (mut manager, mut stream, generation) = receiver();
        stream.write_all(SUCCESS).unwrap();
        observed(&manager);
        let key = port_key("receiver-console");
        match transition {
            "disconnect" => manager.disconnect("receiver-console").unwrap(),
            "reserve" => {
                manager
                    .reserve("receiver-console", Some(generation))
                    .unwrap();
            }
            "generation" => manager.entries.get_mut(&key).unwrap().view.generation += 1,
            "cancel" => manager.entries[&key]
                .worker
                .as_ref()
                .unwrap()
                .cancel
                .store(true, Ordering::Release),
            "inventory" => {
                let mut replaced = inventory();
                replaced.ports[0].serial_number = Some("another-receiver".into());
                manager.update_inventory(&replaced, Instant::now());
            }
            "role" => {
                stream
                    .write_all(b"Device name: SlimeNRF Tracker\n")
                    .unwrap();
                until(|| {
                    let _ = manager.poll();
                    manager.session("receiver-console").unwrap().identity.role
                        == DeviceRole::Tracker
                });
            }
            _ => unreachable!(),
        }
        assert!(
            manager.take_receiver_pairing_changes().is_empty(),
            "{transition}"
        );
        assert!(!pending(&manager), "{transition}");
        if transition == "generation" {
            let _ = manager.poll();
        }
    }
}

#[test]
fn inventory_freshness_loss_defers_success_until_the_same_identity_is_fresh_again() {
    for transition in ["failed", "aged"] {
        let (mut manager, mut stream, generation) = receiver();
        match transition {
            "failed" => manager.invalidate_inventory(),
            "aged" => {
                let old = Instant::now() - Duration::from_secs(60);
                manager.update_inventory_at(&inventory(), old, old);
            }
            _ => unreachable!(),
        }
        stream.write_all(SUCCESS).unwrap();
        observed(&manager);
        for _ in 0..3 {
            let _ = manager.poll();
            assert!(
                manager.take_receiver_pairing_changes().is_empty(),
                "{transition}"
            );
            assert!(pending(&manager), "{transition}");
        }
        assert_eq!(
            manager.session("receiver-console").unwrap().generation,
            generation
        );
        manager.update_inventory(&inventory(), Instant::now());
        assert_eq!(
            manager.take_receiver_pairing_changes(),
            [("receiver-console".to_owned(), generation)],
            "{transition}"
        );
        assert!(!pending(&manager), "{transition}");
        assert!(
            manager.take_receiver_pairing_changes().is_empty(),
            "{transition}"
        );
    }
}

#[test]
fn deferred_success_is_discarded_when_usb_identity_or_serial_generation_is_replaced() {
    for transition in ["inventory", "generation"] {
        let (mut manager, mut stream, _) = receiver();
        manager.invalidate_inventory();
        stream.write_all(SUCCESS).unwrap();
        observed(&manager);
        assert!(manager.take_receiver_pairing_changes().is_empty());
        assert!(pending(&manager));
        match transition {
            "inventory" => {
                let mut replaced = inventory();
                replaced.ports[0].serial_number = Some("another-receiver".into());
                manager.update_inventory(&replaced, Instant::now());
            }
            "generation" => {
                manager
                    .entries
                    .get_mut(&port_key("receiver-console"))
                    .unwrap()
                    .view
                    .generation += 1;
                manager.update_inventory(&inventory(), Instant::now());
            }
            _ => unreachable!(),
        }
        assert!(
            manager.take_receiver_pairing_changes().is_empty(),
            "{transition}"
        );
        assert!(!pending(&manager), "{transition}");
        manager.update_inventory(&inventory(), Instant::now());
        assert!(
            manager.take_receiver_pairing_changes().is_empty(),
            "{transition}"
        );
        if transition == "generation" {
            let _ = manager.poll();
        }
    }
}

#[test]
fn closed_generation_cannot_deliver_its_retained_success() {
    let (mut manager, mut old_stream, _) = receiver();
    old_stream.write_all(SUCCESS).unwrap();
    observed(&manager);
    manager.disconnect("receiver-console").unwrap();
    stopped(&mut manager, "receiver-console");
    assert!(manager.take_receiver_pairing_changes().is_empty());
}
