use super::super::native::Transport;
use super::super::{
    ConnectionState, DisplayQueue, Entry, IdentitySnapshot, Lifecycle, QUEUE_CAPACITY,
    SessionIdentityObservation, SessionManager, SessionUsbIdentity, SessionView, Worker, port_key,
};
use super::{completed, connected, fixture, peer, socket_transport, stopped, until};
use crate::domain::{DeviceIdentity, Inventory, SerialEndpoint, SerialPortKind};
use crate::terminal::TerminalBuffer;
use parking_lot::Mutex;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

#[test]
fn disconnect_after_closed_preserves_terminal_state_before_and_after_reap() {
    for error in [None, Some("Read failed: disconnected".to_string())] {
        let expected_state = if error.is_some() {
            ConnectionState::Faulted
        } else {
            ConnectionState::Disconnected
        };
        let mut manager = SessionManager::with_factory(Arc::new(|_| {
            panic!("lifecycle-only test must not open a transport")
        }));
        let (commands, _command_rx) = mpsc::sync_channel(QUEUE_CAPACITY);
        let display = Arc::new(Mutex::new(DisplayQueue::new()));
        let (lifecycle_tx, lifecycle) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        // Hold the thread alive after Closed is observable, reproducing the
        // scheduling window without timing sleeps or physical I/O.
        let handle = thread::spawn(move || {
            release_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        });
        let cancel = Arc::new(AtomicBool::new(false));
        manager.entries.insert(
            port_key("closed"),
            Entry {
                pending_writes: Arc::new(AtomicUsize::new(0)),
                close_requested: false,
                opening_identity: None,
                captured_usb_identity: None,
                reconnect: None,
                view: SessionView {
                    port: "closed".into(),
                    generation: 1,
                    state: ConnectionState::Connected,
                    last_error: None,
                    identity: DeviceIdentity::default(),
                    terminal: TerminalBuffer::new(),
                },
                worker: Some(Worker {
                    generation: 1,
                    cancel: Arc::clone(&cancel),
                    commands,
                    display,
                    lifecycle,
                    identity: Arc::new(Mutex::new(IdentitySnapshot {
                        identity: DeviceIdentity::default(),
                        revision: 0,
                        pairing_changed: false,
                    })),
                    identity_revision: 0,
                    rssi_scan: Arc::new(Mutex::new(None)),
                    delivered_rssi_scan: None,
                    handle: Some(handle),
                    opened: true,
                    closed: false,
                }),
            },
        );
        lifecycle_tx
            .send(Lifecycle::Closed {
                error: error.clone(),
                dropped: 0,
            })
            .unwrap();
        let _ = manager.poll();
        assert!(
            manager.entries[&port_key("closed")]
                .worker
                .as_ref()
                .unwrap()
                .closed
        );
        assert_eq!(manager.session("closed").unwrap().state, expected_state);
        manager.disconnect("closed").unwrap();
        assert_eq!(manager.session("closed").unwrap().state, expected_state);
        assert!(!cancel.load(Ordering::Acquire));
        let reservation = manager.reserve("closed", Some(1)).unwrap();
        // Closed alone is not a handoff: the thread is deliberately alive.
        assert!(!manager.reservation_ready(&reservation).unwrap());
        assert!(manager.connect("closed").is_err());

        release_tx.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while !manager.entries[&port_key("closed")]
            .worker
            .as_ref()
            .unwrap()
            .handle
            .as_ref()
            .unwrap()
            .is_finished()
        {
            assert!(Instant::now() < deadline, "gated worker failed to finish");
            thread::yield_now();
        }
        let _ = manager.poll();
        assert!(manager.entries[&port_key("closed")].worker.is_none());
        assert!(manager.reservation_ready(&reservation).unwrap());
        manager.release(&reservation).unwrap();
        let view = manager.session("closed").unwrap();
        assert_eq!(view.state, expected_state);
        assert_eq!(view.last_error, error);
    }
}

#[test]
fn two_real_streams_keep_commands_identity_and_close_independent() {
    let (mut manager, listener) = fixture();
    manager.connect("first").unwrap();
    let mut first = peer(&listener);
    manager.connect("second").unwrap();
    let mut second = peer(&listener);
    connected(&mut manager, "first");
    connected(&mut manager, "second");
    first
        .write_all(b"Device address: 112233445566\r\nfirst log\n")
        .unwrap();
    second
        .write_all(b"Device address: AABBCCDDEEFF\nsecond log\n")
        .unwrap();
    until(|| {
        let _ = manager.poll();
        manager
            .session("first")
            .unwrap()
            .identity
            .address
            .as_deref()
            == Some("112233445566")
            && manager
                .session("second")
                .unwrap()
                .identity
                .address
                .as_deref()
                == Some("AABBCCDDEEFF")
    });
    let mut first_receipt = manager.send_line("first", "alpha\r\n").unwrap();
    let mut second_receipt = manager.send_line("second", "beta").unwrap();
    let mut first_command = [0; 6];
    let mut second_command = [0; 5];
    first.read_exact(&mut first_command).unwrap();
    second.read_exact(&mut second_command).unwrap();
    assert_eq!(&first_command, b"alpha\n");
    assert_eq!(&second_command, b"beta\n");
    completed(&mut first_receipt).unwrap();
    completed(&mut second_receipt).unwrap();
    manager.disconnect("first").unwrap();
    stopped(&mut manager, "first");
    assert_eq!(first.read(&mut [0; 1]).unwrap(), 0);
    assert_eq!(
        manager.session("second").unwrap().state,
        ConnectionState::Connected
    );
    assert!(
        !manager
            .session("second")
            .unwrap()
            .terminal
            .text()
            .contains("first log")
    );
    second.write_all(b"still alive\n").unwrap();
    until(|| {
        let _ = manager.poll();
        manager
            .session("second")
            .unwrap()
            .terminal
            .text()
            .contains("still alive")
    });
    manager.shutdown().unwrap().join().unwrap();
    assert_eq!(second.read(&mut [0; 1]).unwrap(), 0);
}

#[test]
fn cancelled_blocked_open_cannot_publish_or_reopen_owned_port() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (ready_tx, ready_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let mut manager = SessionManager::with_factory(Arc::new(move |_| {
        let stream = socket_transport(address)?;
        ready_tx.send(()).unwrap();
        release_rx
            .lock()
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        Ok(Box::new(stream))
    }));
    manager.connect("blocked").unwrap();
    let mut stream = peer(&listener);
    ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    manager.disconnect("blocked").unwrap();
    assert!(manager.connect("blocked").is_err());
    let _ = manager.poll();
    assert_eq!(
        manager.session("blocked").unwrap().state,
        ConnectionState::Closing
    );
    release_tx.send(()).unwrap();
    until(|| {
        let _ = manager.poll();
        assert_ne!(
            manager.session("blocked").unwrap().state,
            ConnectionState::Connected
        );
        manager.entries[&port_key("blocked")].worker.is_none()
    });
    assert_eq!(
        manager.session("blocked").unwrap().state,
        ConnectionState::Disconnected
    );
    assert_eq!(stream.read(&mut [0; 1]).unwrap(), 0);
}

#[test]
fn close_during_blocked_open_keeps_generation_frozen_until_forget() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (ready_tx, ready_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let opens = Arc::new(AtomicUsize::new(0));
    let factory_opens = Arc::clone(&opens);
    let mut manager = SessionManager::with_factory(Arc::new(move |_| {
        factory_opens.fetch_add(1, Ordering::SeqCst);
        let stream = socket_transport(address)?;
        ready_tx.send(()).unwrap();
        release_rx
            .lock()
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        Ok(Box::new(stream))
    }));
    manager.connect("COM40").unwrap();
    let mut first = peer(&listener);
    ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let generation = manager.session("COM40").unwrap().generation;
    let alias = if cfg!(windows) { r"\\.\com40" } else { "COM40" };
    assert!(manager.can_request_close(alias));
    manager.request_close(alias).unwrap();
    manager.request_close("COM40").unwrap();
    assert!(!manager.forget("COM40"));
    assert_eq!(
        manager.session("COM40").unwrap().state,
        ConnectionState::Closing
    );
    assert_eq!(
        manager.connect(alias).unwrap_err(),
        format!("{alias} terminal is closing")
    );
    assert_eq!(opens.load(Ordering::SeqCst), 1);
    assert_eq!(manager.next_generation, generation);
    release_tx.send(()).unwrap();
    until(|| {
        let _ = manager.poll();
        assert_ne!(
            manager.session("COM40").unwrap().state,
            ConnectionState::Connected
        );
        manager.entries[&port_key("COM40")].worker.is_none()
    });
    assert_eq!(first.read(&mut [0; 1]).unwrap(), 0);
    assert_eq!(
        manager.session("COM40").unwrap().state,
        ConnectionState::Disconnected
    );
    assert_eq!(
        manager.connect(alias).unwrap_err(),
        format!("{alias} terminal is closing")
    );
    assert_eq!(manager.session("COM40").unwrap().generation, generation);
    assert_eq!(manager.next_generation, generation);
    assert_eq!(opens.load(Ordering::SeqCst), 1);

    assert!(manager.forget(alias));
    assert!(!manager.is_close_requested("COM40"));
    manager.connect("COM40").unwrap();
    let mut second = peer(&listener);
    ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    release_tx.send(()).unwrap();
    connected(&mut manager, "COM40");
    assert_eq!(manager.session("COM40").unwrap().generation, generation + 1);
    assert_eq!(opens.load(Ordering::SeqCst), 2);
    assert!(!manager.is_close_requested("COM40"));
    let mut receipt = manager.send_line("COM40", "info").unwrap();
    let mut command = [0; 5];
    second.read_exact(&mut command).unwrap();
    assert_eq!(&command, b"info\n");
    assert_eq!(completed(&mut receipt), Ok(()));
    manager.shutdown().unwrap().join().unwrap();
}

#[test]
fn reconnect_clears_identity_and_uses_a_new_generation() {
    let (mut manager, listener) = fixture();
    manager.connect("port").unwrap();
    let mut old = peer(&listener);
    connected(&mut manager, "port");
    old.write_all(b"Device address: 112233445566\n").unwrap();
    until(|| {
        let _ = manager.poll();
        manager.session("port").unwrap().identity.address.is_some()
    });
    let generation = manager.session("port").unwrap().generation;
    manager.disconnect("port").unwrap();
    stopped(&mut manager, "port");
    manager.connect("port").unwrap();
    let _new = peer(&listener);
    connected(&mut manager, "port");
    let view = manager.session("port").unwrap();
    assert!(view.generation > generation);
    assert!(view.identity.address.is_none());
    assert!(view.terminal.text().is_empty());
    manager.shutdown().unwrap().join().unwrap();
}
#[test]
fn open_failure_is_asynchronous_and_send_rejection_is_visible() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let mut manager = SessionManager::with_factory(Arc::new(move |_| {
        Ok(Box::new(socket_transport(address)?) as Box<dyn Transport>)
    }));
    manager.connect("unavailable").unwrap();
    assert_eq!(
        manager.session("unavailable").unwrap().state,
        ConnectionState::Connecting
    );
    stopped(&mut manager, "unavailable");
    assert_eq!(
        manager.session("unavailable").unwrap().state,
        ConnectionState::Faulted
    );
    assert!(manager.session("unavailable").unwrap().last_error.is_some());
    assert!(manager.send_line("unavailable", "info").is_err());
    assert!(
        manager
            .session("unavailable")
            .unwrap()
            .last_error
            .as_ref()
            .unwrap()
            .contains("not connected")
    );
}

#[test]
fn stale_generation_cannot_mutate_view_and_shutdown_prevents_reopen() {
    let (mut manager, listener) = fixture();
    manager.connect("port").unwrap();
    let mut stream = peer(&listener);
    connected(&mut manager, "port");
    // Inject a stale worker generation, not a simulated transport.
    manager
        .entries
        .get_mut(&port_key("port"))
        .unwrap()
        .view
        .generation += 1;
    stream
        .write_all(b"Device address: 112233445566\nstale\n")
        .unwrap();
    stopped(&mut manager, "port");
    assert!(manager.session("port").unwrap().identity.address.is_none());
    assert!(manager.session("port").unwrap().terminal.text().is_empty());
    // The peer may observe reset rather than EOF when unread input remained.
    let result = stream.read(&mut [0; 1]);
    assert!(matches!(result, Ok(0) | Err(_)));
    let _ = manager.shutdown();
    assert!(manager.connect("port").is_err());
}

fn identity_endpoint() -> SerialEndpoint {
    SerialEndpoint {
        port_name: "identity-fixture".into(),
        label: "Synthetic tracker".into(),
        manufacturer: None,
        product: None,
        kind: SerialPortKind::Usb,
        serial_number: Some("synthetic-opening-usb".into()),
        vid: Some(0x1209),
        pid: Some(0x7692),
        usb_interface: None,
    }
}

#[test]
fn final_positive_identity_is_returned_before_same_poll_automatic_reconnect_resets_it() {
    let (mut manager, listener) = fixture();
    let endpoint = identity_endpoint();
    let inventory = Inventory {
        ports: vec![endpoint.clone()],
        ..Inventory::default()
    };
    manager.update_inventory(&inventory, Instant::now());
    manager.connect(&endpoint.port_name).unwrap();
    let mut stream = peer(&listener);
    connected(&mut manager, &endpoint.port_name);
    let generation = manager.session(&endpoint.port_name).unwrap().generation;
    let unrelated_handoff = manager.reserve("unrelated-handoff", None).unwrap();
    manager.update_inventory(&Inventory::default(), Instant::now());
    manager.update_inventory(&inventory, Instant::now());
    stream
        .write_all(b"Device address: 112233445566\nfinal identity\n")
        .unwrap();
    drop(stream);
    until(|| {
        manager.entries[&port_key(&endpoint.port_name)]
            .worker
            .as_ref()
            .unwrap()
            .handle
            .as_ref()
            .unwrap()
            .is_finished()
    });
    assert!(manager.reservation_ready(&unrelated_handoff).unwrap());
    assert_eq!(
        manager.session(&endpoint.port_name).unwrap().generation,
        generation
    );
    let changes = manager.poll();
    let view = manager.session(&endpoint.port_name).unwrap();
    assert_eq!(view.generation, generation + 1);
    assert_eq!(view.identity.address, None);
    assert_eq!(
        changes.identities,
        vec![SessionIdentityObservation {
            port: endpoint.port_name.clone(),
            generation,
            address: "112233445566".into(),
            usb: SessionUsbIdentity::new("synthetic-opening-usb", 0x1209, 0x7692),
        }],
    );
    let _new_stream = peer(&listener);
    connected(&mut manager, &endpoint.port_name);
    assert!(manager.poll().identities.is_empty());
    manager.release(&unrelated_handoff).unwrap();
}

#[test]
fn identity_publications_do_not_repeat_for_unchanged_rf_or_name_only_updates() {
    let (mut manager, listener) = fixture();
    let endpoint = identity_endpoint();
    manager.update_inventory(
        &Inventory {
            ports: vec![endpoint.clone()],
            ..Inventory::default()
        },
        Instant::now(),
    );
    manager.connect(&endpoint.port_name).unwrap();
    let mut stream = peer(&listener);
    connected(&mut manager, &endpoint.port_name);
    stream
        .write_all(b"Device name: Synthetic tracker\nDevice address: 112233445566\n")
        .unwrap();
    let mut publications = Vec::new();
    until(|| {
        publications.extend(manager.poll().identities);
        manager
            .session(&endpoint.port_name)
            .unwrap()
            .identity
            .address
            .as_deref()
            == Some("112233445566")
    });
    assert_eq!(
        publications,
        vec![SessionIdentityObservation {
            port: endpoint.port_name.clone(),
            generation: 1,
            address: "112233445566".into(),
            usb: SessionUsbIdentity::new("synthetic-opening-usb", 0x1209, 0x7692),
        }],
    );
    stream
        .write_all(
            b"Device name: Renamed synthetic tracker\nDevice address: 112233445566\nname update complete\n",
        )
        .unwrap();
    let mut repeated = Vec::new();
    until(|| {
        repeated.extend(manager.poll().identities);
        manager
            .session(&endpoint.port_name)
            .unwrap()
            .terminal
            .text()
            .contains("name update complete")
    });
    assert!(repeated.is_empty());
    assert_eq!(
        manager
            .session(&endpoint.port_name)
            .unwrap()
            .identity
            .name
            .as_deref(),
        Some("Renamed synthetic tracker"),
    );
}

#[test]
fn later_inventory_never_supplies_missing_opening_usb_proof() {
    let (mut manager, listener) = fixture();
    let endpoint = identity_endpoint();
    manager.update_inventory(
        &Inventory {
            ports: vec![endpoint.clone()],
            ..Inventory::default()
        },
        Instant::now(),
    );
    manager.invalidate_inventory();
    manager.connect(&endpoint.port_name).unwrap();
    let mut stream = peer(&listener);
    connected(&mut manager, &endpoint.port_name);
    manager.update_inventory(
        &Inventory {
            ports: vec![endpoint.clone()],
            ..Inventory::default()
        },
        Instant::now(),
    );
    stream.write_all(b"Device address: 112233445566\n").unwrap();
    let mut publications = Vec::new();
    until(|| {
        publications.extend(manager.poll().identities);
        manager
            .session(&endpoint.port_name)
            .unwrap()
            .identity
            .address
            .as_deref()
            == Some("112233445566")
    });
    assert_eq!(
        publications,
        vec![SessionIdentityObservation {
            port: endpoint.port_name,
            generation: 1,
            address: "112233445566".into(),
            usb: None,
        }],
    );
}

#[test]
fn manual_open_rejects_an_inventory_replacement_before_publishing_rf_or_usb_proof() {
    for restore_original_before_open in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (opening_tx, opening_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release = Mutex::new(Some(release_rx));
        let mut manager = SessionManager::with_factory(Arc::new(move |_| {
            if let Some(release) = release.lock().take() {
                opening_tx.send(()).unwrap();
                release.recv_timeout(Duration::from_secs(3)).unwrap();
            }
            Ok(Box::new(socket_transport(address)?))
        }));
        let original = identity_endpoint();
        manager.update_inventory(
            &Inventory {
                ports: vec![original.clone()],
                ..Inventory::default()
            },
            Instant::now(),
        );
        manager.connect(&original.port_name).unwrap();
        opening_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        let replacement = SerialEndpoint {
            serial_number: Some("synthetic-replacement-usb".into()),
            ..original.clone()
        };
        manager.update_inventory(
            &Inventory {
                ports: vec![replacement.clone()],
                ..Inventory::default()
            },
            Instant::now(),
        );
        if restore_original_before_open {
            manager.update_inventory(
                &Inventory {
                    ports: vec![original.clone()],
                    ..Inventory::default()
                },
                Instant::now(),
            );
        }
        release_tx.send(()).unwrap();
        let mut replaced_stream = peer(&listener);
        let _ = replaced_stream.write_all(b"Device address: AABBCCDDEEFF\n");
        drop(replaced_stream);
        let mut publications = Vec::new();
        until(|| {
            publications.extend(manager.poll().identities);
            manager.entries[&port_key(&original.port_name)]
                .worker
                .is_none()
        });
        assert_eq!(publications, []);
        assert_eq!(
            manager
                .session(&original.port_name)
                .unwrap()
                .identity
                .address,
            None
        );
        assert_eq!(manager.identity_observations().count(), 0);
        assert!(!manager.is_reconnecting(&original.port_name));

        let moved_original = SerialEndpoint {
            port_name: "original-at-another-port".into(),
            ..original
        };
        manager.update_inventory(
            &Inventory {
                ports: vec![replacement, moved_original.clone()],
                ..Inventory::default()
            },
            Instant::now(),
        );
        manager.connect(&moved_original.port_name).unwrap();
        let mut control_stream = peer(&listener);
        connected(&mut manager, &moved_original.port_name);
        completed(
            &mut manager
                .send_line(&moved_original.port_name, "help")
                .unwrap(),
        )
        .unwrap();
        manager.disconnect(&moved_original.port_name).unwrap();
        stopped(&mut manager, &moved_original.port_name);
        let mut bytes = Vec::new();
        control_stream.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"help\n");
    }
}

#[test]
fn validated_opening_usb_proof_survives_replacement_before_the_first_rf_report() {
    let (mut manager, listener) = fixture();
    let original = identity_endpoint();
    manager.update_inventory(
        &Inventory {
            ports: vec![original.clone()],
            ..Inventory::default()
        },
        Instant::now(),
    );
    manager.connect(&original.port_name).unwrap();
    let mut stream = peer(&listener);
    connected(&mut manager, &original.port_name);
    manager.update_inventory(
        &Inventory {
            ports: vec![SerialEndpoint {
                serial_number: Some("synthetic-later-replacement".into()),
                ..original.clone()
            }],
            ..Inventory::default()
        },
        Instant::now(),
    );
    stream.write_all(b"Device address: 112233445566\n").unwrap();
    let mut publications = Vec::new();
    until(|| {
        publications.extend(manager.poll().identities);
        !publications.is_empty()
    });
    assert_eq!(
        publications,
        [SessionIdentityObservation {
            port: original.port_name,
            generation: 1,
            address: "112233445566".into(),
            usb: SessionUsbIdentity::new("synthetic-opening-usb", 0x1209, 0x7692),
        }],
    );
}
