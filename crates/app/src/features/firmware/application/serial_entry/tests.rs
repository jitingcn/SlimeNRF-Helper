use super::{
    Action, FlashState, SerialDfuOutcome, SerialDfuPhase, SerialDfuState, recognized_device,
};
use crate::sessions::test_support::{
    connected, fixture, peer, serial_open_error, socket_transport, stopped, with_socket_factory,
    with_transport_factory,
};
use crate::{
    domain::{
        DeviceRole, InterfaceDescription, Inventory, SerialEndpoint, SerialPortKind, UsbInterface,
    },
    sessions::{ConnectionState, SessionManager},
};
use parking_lot::Mutex;
use std::{
    io::{self, Read, Write},
    net::{Shutdown, TcpListener, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

fn inventory() -> Inventory {
    Inventory {
        ports: vec![SerialEndpoint {
            port_name: "dfu-test".into(),
            label: "dfu-test — SlimeNRF USB Tracker".into(),
            serial_number: Some("synthetic-dfu".into()),
            vid: Some(0x1209),
            pid: Some(0x7690),
            manufacturer: Some("SlimeNRF".into()),
            product: Some("Tracker".into()),
            kind: SerialPortKind::Usb,
            usb_interface: None,
        }],
        ..Inventory::default()
    }
}

fn selected() -> SerialDfuState {
    let mut state = SerialDfuState::default();
    state.select("dfu-test".into());
    state
}

fn request_confirmed(state: &mut SerialDfuState, manager: &SessionManager, inventory: &Inventory) {
    state.request(manager, inventory, true, Ok(()));
    assert!(state.warning.is_some());
    state.confirm(manager, inventory, Ok(()));
    assert!(state.busy());
}

fn tick(
    state: &mut SerialDfuState,
    manager: &mut SessionManager,
    inventory: &Inventory,
    generation: u64,
    scanning: bool,
    current: bool,
    gate: Result<(), String>,
) {
    if let Some(port) = state.advance(manager, inventory, generation, scanning, current, gate) {
        state.submitted(manager.send_line(&port, "dfu"));
    }
}

fn finish(state: &mut SerialDfuState, manager: &mut SessionManager, inventory: &Inventory) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while state.busy() {
        assert!(Instant::now() < deadline, "DFU result timed out");
        let _ = manager.poll();
        tick(state, manager, inventory, 1, false, true, Ok(()));
        thread::sleep(Duration::from_millis(2));
    }
}

fn assert_failed(state: &SerialDfuState) -> &str {
    match &state.outcome {
        Some(SerialDfuOutcome::Failed(error)) => error,
        _ => panic!("expected page-local failure"),
    }
}

#[test]
fn usb_branding_and_generic_reported_roles_require_consent_before_any_open() {
    let calls = Arc::new(AtomicUsize::new(0));
    let factory_calls = calls.clone();
    let mut manager = with_socket_factory(move |_| {
        factory_calls.fetch_add(1, Ordering::SeqCst);
        Err(io::Error::other("must not open before consent"))
    });
    let inventory = inventory();
    let mut state = selected();
    state.request(&manager, &inventory, true, Ok(()));
    assert!(state.warning.is_some());
    for _ in 0..3 {
        tick(&mut state, &mut manager, &inventory, 0, false, true, Ok(()));
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(manager.session("dfu-test").is_none());
    state.warning = None; // Same action used by the Cancel button and navigation.
    tick(&mut state, &mut manager, &inventory, 0, false, true, Ok(()));
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    let (mut manager, listener) = fixture();
    manager.connect("dfu-test").unwrap();
    let mut stream = peer(&listener);
    connected(&mut manager, "dfu-test");
    stream
        .write_all(b"Device name: Promicro Receiver\n")
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while manager.session("dfu-test").unwrap().identity.role == DeviceRole::Unknown {
        assert!(Instant::now() < deadline);
        let _ = manager.poll();
        thread::sleep(Duration::from_millis(2));
    }
    assert!(!recognized_device("dfu-test", &manager, &inventory, true));
    state.request(&manager, &inventory, true, Ok(()));
    assert!(state.warning.is_some());
    manager.shutdown().unwrap().join().unwrap();
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).unwrap();
    assert!(bytes.is_empty());
}

#[test]
fn connected_owner_cannot_submit_dfu_with_failed_inventory_or_reopen() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let opens = Arc::new(AtomicUsize::new(0));
    let factory_opens = opens.clone();
    let mut manager = with_socket_factory(move |_| {
        factory_opens.fetch_add(1, Ordering::SeqCst);
        socket_transport(address)
    });
    manager.connect("dfu-test").unwrap();
    let mut stream = peer(&listener);
    connected(&mut manager, "dfu-test");
    let generation = manager.session("dfu-test").unwrap().generation;
    let mut state = selected();
    let inventory = Inventory {
        serial_enumeration_failed: true,
        ..Inventory::default()
    };
    state.request(&manager, &inventory, false, Ok(()));
    assert!(state.warning.is_none());
    assert_failed(&state);
    assert!(!state.needs_scan());
    tick(
        &mut state,
        &mut manager,
        &inventory,
        99,
        false,
        false,
        Ok(()),
    );
    finish(&mut state, &mut manager, &inventory);
    assert_failed(&state);
    assert_eq!(manager.session("dfu-test").unwrap().generation, generation);
    assert_eq!(opens.load(Ordering::SeqCst), 1);
    for _ in 0..3 {
        tick(
            &mut state,
            &mut manager,
            &inventory,
            99,
            false,
            false,
            Ok(()),
        );
    }
    manager.shutdown().unwrap().join().unwrap();
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).unwrap();
    assert!(bytes.is_empty());
}

#[test]
fn new_open_waits_for_fresh_inventory_and_actual_readiness_and_locks_selection() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (opened_tx, opened_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let opens = Arc::new(AtomicUsize::new(0));
    let factory_opens = opens.clone();
    let mut manager = with_socket_factory(move |_| {
        factory_opens.fetch_add(1, Ordering::SeqCst);
        let stream = socket_transport(address)?;
        opened_tx.send(()).unwrap();
        release_rx
            .lock()
            .recv_timeout(Duration::from_secs(3))
            .unwrap();
        Ok(stream)
    });
    let inventory = inventory();
    let mut state = selected();
    request_confirmed(&mut state, &manager, &inventory);
    state.select("other".into());
    assert_eq!(state.selected_port.as_deref(), Some("dfu-test"));
    tick(&mut state, &mut manager, &inventory, 0, false, true, Ok(()));
    assert_eq!(opens.load(Ordering::SeqCst), 0);
    state.scanned(1);
    tick(&mut state, &mut manager, &inventory, 1, true, false, Ok(()));
    assert_eq!(opens.load(Ordering::SeqCst), 0);
    tick(&mut state, &mut manager, &inventory, 1, false, true, Ok(()));
    let mut stream = peer(&listener);
    opened_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    for _ in 0..3 {
        tick(&mut state, &mut manager, &inventory, 1, false, true, Ok(()));
    }
    assert!(matches!(
        state.pending.as_ref().unwrap().phase,
        SerialDfuPhase::Connecting(_)
    ));
    stream.set_nonblocking(true).unwrap();
    assert_eq!(
        stream.read(&mut [0]).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    stream.set_nonblocking(false).unwrap();
    release_tx.send(()).unwrap();
    connected(&mut manager, "dfu-test");
    finish(&mut state, &mut manager, &inventory);
    assert!(matches!(state.outcome, Some(SerialDfuOutcome::Sent)));
    assert_eq!(opens.load(Ordering::SeqCst), 1);
    manager.shutdown().unwrap().join().unwrap();
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"dfu\n");
}

#[test]
fn native_open_failure_is_local_and_never_retried() {
    let calls = Arc::new(AtomicUsize::new(0));
    let factory_calls = calls.clone();
    let mut manager = with_socket_factory(move |port| {
        factory_calls.fetch_add(1, Ordering::SeqCst);
        Err(serial_open_error(
            port,
            &serialport::Error::new(
                serialport::ErrorKind::Io(io::ErrorKind::PermissionDenied),
                "synthetic access denied",
            ),
        ))
    });
    let inventory = inventory();
    let mut state = selected();
    request_confirmed(&mut state, &manager, &inventory);
    state.scanned(1);
    tick(&mut state, &mut manager, &inventory, 1, false, true, Ok(()));
    stopped(&mut manager, "dfu-test");
    finish(&mut state, &mut manager, &inventory);
    assert!(assert_failed(&state).contains("synthetic access denied"));
    for _ in 0..5 {
        tick(&mut state, &mut manager, &inventory, 1, false, true, Ok(()));
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn stale_failed_disappeared_or_replaced_inventory_never_opens() {
    for scenario in 0..5 {
        let calls = Arc::new(AtomicUsize::new(0));
        let factory_calls = calls.clone();
        let mut manager = with_socket_factory(move |_| {
            factory_calls.fetch_add(1, Ordering::SeqCst);
            Err(io::Error::other("must not open"))
        });
        let mut inventory = inventory();
        let mut state = selected();
        request_confirmed(&mut state, &manager, &inventory);
        state.scanned(1);
        match scenario {
            2 => inventory.ports.clear(),
            3 => inventory.ports[0].serial_number = Some("replacement".into()),
            4 => state.selected_port = Some("different selection".into()),
            _ => {}
        }
        tick(
            &mut state,
            &mut manager,
            &inventory,
            if scenario == 0 { 2 } else { 1 },
            false,
            scenario != 1,
            Ok(()),
        );
        assert_failed(&state);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn confirmation_is_revoked_by_changed_endpoint_or_generation() {
    let (mut manager, listener) = fixture();
    let mut inventory = inventory();
    let mut state = selected();
    state.request(&manager, &inventory, true, Ok(()));
    inventory.ports[0].pid = Some(7);
    state.confirm(&manager, &inventory, Ok(()));
    assert_failed(&state);
    assert!(manager.session("dfu-test").is_none());

    state.request(&manager, &inventory, true, Ok(()));
    manager.connect("dfu-test").unwrap();
    let _stream = peer(&listener);
    connected(&mut manager, "dfu-test");
    state.confirm(&manager, &inventory, Ok(()));
    assert_failed(&state);
    manager.shutdown().unwrap().join().unwrap();
}

#[test]
fn existing_owner_closure_and_reconnect_never_reopens_or_replays_old_intent() {
    for reconnect in [false, true] {
        let (mut manager, listener) = fixture();
        manager.connect("dfu-test").unwrap();
        let stream = peer(&listener);
        connected(&mut manager, "dfu-test");
        let inventory = inventory();
        let mut state = selected();
        request_confirmed(&mut state, &manager, &inventory);
        stream.shutdown(Shutdown::Both).unwrap();
        stopped(&mut manager, "dfu-test");
        let mut replacement = None;
        if reconnect {
            manager.connect("dfu-test").unwrap();
            replacement = Some(peer(&listener));
            connected(&mut manager, "dfu-test");
        }
        tick(&mut state, &mut manager, &inventory, 1, false, true, Ok(()));
        assert_failed(&state);
        if let Some(join) = manager.shutdown() {
            join.join().unwrap();
        }
        if let Some(mut replacement) = replacement {
            let mut bytes = Vec::new();
            replacement.read_to_end(&mut bytes).unwrap();
            assert!(bytes.is_empty());
        }
    }
}

#[test]
fn operation_and_capture_admission_failure_revokes_preparation_before_write() {
    for gate_error in [
        "capture owns this endpoint",
        "firmware review is active",
        "update lease owns this endpoint",
    ] {
        let (mut manager, listener) = fixture();
        manager.connect("dfu-test").unwrap();
        let mut stream = peer(&listener);
        connected(&mut manager, "dfu-test");
        let inventory = inventory();
        let mut state = selected();
        request_confirmed(&mut state, &manager, &inventory);
        tick(
            &mut state,
            &mut manager,
            &inventory,
            1,
            false,
            true,
            Err(gate_error.into()),
        );
        assert_eq!(assert_failed(&state), gate_error);
        manager.shutdown().unwrap().join().unwrap();
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes).unwrap();
        assert!(bytes.is_empty());
    }
}

#[test]
fn manager_reservation_cannot_be_bypassed_by_page_submission() {
    let (mut manager, listener) = fixture();
    manager.connect("dfu-test").unwrap();
    let mut stream = peer(&listener);
    connected(&mut manager, "dfu-test");
    let inventory = inventory();
    let mut state = selected();
    request_confirmed(&mut state, &manager, &inventory);
    let port = state
        .advance(&mut manager, &inventory, 1, false, true, Ok(()))
        .unwrap();
    let generation = manager.session(&port).unwrap().generation;
    let lease = manager.reserve(&port, Some(generation)).unwrap();
    state.submitted(manager.send_line(&port, "dfu"));
    assert_failed(&state);
    stopped(&mut manager, &port);
    manager.release(&lease).unwrap();
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).unwrap();
    assert!(bytes.is_empty());
}

struct ControlledWriter {
    stream: TcpStream,
    started: mpsc::Sender<()>,
    release: mpsc::Receiver<()>,
    fail: bool,
}

impl Read for ControlledWriter {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.stream.read(bytes)
    }
}

impl Write for ControlledWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.started.send(()).unwrap();
        self.release.recv_timeout(Duration::from_secs(3)).unwrap();
        if self.fail {
            self.stream.write_all(&bytes[..2])?;
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "synthetic partial write",
            ));
        }
        self.stream.write_all(bytes)?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        panic!("DFU session write must not flush")
    }
}

#[test]
fn retained_receipt_distinguishes_pending_written_eof_and_partial_failure_without_replay() {
    for fail in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(Some(release_rx));
        let opens = Arc::new(AtomicUsize::new(0));
        let factory_opens = opens.clone();
        let mut manager = with_transport_factory(move |_| {
            factory_opens.fetch_add(1, Ordering::SeqCst);
            Ok(ControlledWriter {
                stream: socket_transport(address)?,
                started: started_tx.clone(),
                release: release_rx.lock().take().unwrap(),
                fail,
            })
        });
        manager.connect("dfu-test").unwrap();
        let mut stream = peer(&listener);
        connected(&mut manager, "dfu-test");
        let inventory = inventory();
        let mut state = selected();
        request_confirmed(&mut state, &manager, &inventory);
        tick(&mut state, &mut manager, &inventory, 1, false, true, Ok(()));
        started_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        tick(&mut state, &mut manager, &inventory, 1, false, true, Ok(()));
        assert!(state.busy());
        assert!(state.outcome.is_none());
        release_tx.send(()).unwrap();
        if !fail {
            let mut bytes = [0; 4];
            stream.read_exact(&mut bytes).unwrap();
            assert_eq!(&bytes, b"dfu\n");
            stream.shutdown(Shutdown::Both).unwrap();
        }
        stopped(&mut manager, "dfu-test");
        // Later capture/update/roster/session changes cannot override the receipt.
        let deadline = Instant::now() + Duration::from_secs(3);
        while state.busy() {
            assert!(Instant::now() < deadline);
            tick(
                &mut state,
                &mut manager,
                &Inventory::default(),
                999,
                false,
                false,
                Err("new conflicting work".into()),
            );
            thread::sleep(Duration::from_millis(2));
        }
        if fail {
            assert!(assert_failed(&state).contains("synthetic partial write"));
            let mut bytes = Vec::new();
            stream.read_to_end(&mut bytes).unwrap();
            assert_eq!(bytes, b"df");
        } else {
            assert!(matches!(state.outcome, Some(SerialDfuOutcome::Sent)));
        }
        for _ in 0..4 {
            tick(&mut state, &mut manager, &inventory, 1, false, true, Ok(()));
        }
        assert_eq!(opens.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn reported_slime_branding_and_current_role_recognize_only_live_generation() {
    let (mut manager, listener) = fixture();
    manager.connect("dfu-test").unwrap();
    let mut stream = peer(&listener);
    connected(&mut manager, "dfu-test");
    stream
        .write_all(b"Device name: SlimeNRF Tracker\n")
        .unwrap();
    let inventory = inventory();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !recognized_device("dfu-test", &manager, &inventory, true) {
        assert!(Instant::now() < deadline);
        let _ = manager.poll();
        thread::sleep(Duration::from_millis(2));
    }
    let mut state = selected();
    state.request(&manager, &inventory, true, Ok(()));
    assert!(state.warning.is_none());
    assert!(state.busy());
    stream.shutdown(Shutdown::Both).unwrap();
    stopped(&mut manager, "dfu-test");
    assert!(!recognized_device("dfu-test", &manager, &inventory, false));
    tick(
        &mut state,
        &mut manager,
        &inventory,
        1,
        false,
        false,
        Ok(()),
    );
    assert_failed(&state);
}

#[test]
fn unique_receiver_association_requires_current_unambiguous_inventory() {
    let manager = with_socket_factory(|_| panic!("recognition cannot open a serial port"));
    let mut inventory = inventory();
    inventory.receivers.push(crate::domain::ReceiverEndpoint {
        key: "synthetic-hid".into(),
        path: b"synthetic-hid".to_vec(),
        label: "Receiver".into(),
        serial_number: Some("synthetic-dfu".into()),
    });
    assert!(recognized_device("dfu-test", &manager, &inventory, true));
    assert!(!recognized_device("dfu-test", &manager, &inventory, false));
    let mut state = selected();
    state.request(&manager, &inventory, true, Ok(()));
    assert!(state.warning.is_none());
    assert!(state.needs_scan());
    inventory.receivers.push(inventory.receivers[0].clone());
    assert!(!recognized_device("dfu-test", &manager, &inventory, true));
    let mut manager = manager;
    state.scanned(1);
    tick(&mut state, &mut manager, &inventory, 1, false, true, Ok(()));
    assert_failed(&state);
    assert!(manager.session("dfu-test").is_none());
}

#[test]
fn active_serial_work_blocks_firmware_but_does_not_block_its_own_continuation() {
    let manager = with_socket_factory(|_| panic!("admission cannot open a serial port"));
    let inventory = inventory();
    let mut flash = FlashState {
        serial_dfu: selected(),
        ..Default::default()
    };
    request_confirmed(&mut flash.serial_dfu, &manager, &inventory);
    assert!(flash.busy());
    assert!(!flash.serial_dfu_blocked());
    flash.review = Some(Action::Reboot(Vec::new()));
    assert!(flash.serial_dfu_blocked());
    flash.review = None;
    flash.auto = true;
    assert!(flash.serial_dfu_blocked());
    flash.auto = false;
    flash.auto_requested = true;
    assert!(flash.serial_dfu_blocked());
    flash.auto_requested = false;
    flash.pending = Some(Action::Reboot(Vec::new()));
    assert!(flash.serial_dfu_blocked());
    flash.pending = None;
    assert!(!flash.serial_dfu_blocked());
    flash.serial_dfu.fail("cancelled by admission".into());
    assert!(!flash.busy());
}

#[test]
fn already_connecting_owner_is_observed_without_reopen_and_failure_is_not_retried() {
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let calls = Arc::new(AtomicUsize::new(0));
    let factory_calls = calls.clone();
    let mut manager = with_socket_factory(move |_| {
        factory_calls.fetch_add(1, Ordering::SeqCst);
        started_tx.send(()).unwrap();
        release_rx
            .lock()
            .recv_timeout(Duration::from_secs(3))
            .unwrap();
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "synthetic occupied port",
        ))
    });
    manager.connect("dfu-test").unwrap();
    started_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    let inventory = inventory();
    let mut state = selected();
    request_confirmed(&mut state, &manager, &inventory);
    assert!(!state.needs_scan());
    for _ in 0..3 {
        tick(&mut state, &mut manager, &inventory, 1, false, true, Ok(()));
    }
    assert!(state.busy());
    release_tx.send(()).unwrap();
    stopped(&mut manager, "dfu-test");
    tick(&mut state, &mut manager, &inventory, 1, false, true, Ok(()));
    assert!(assert_failed(&state).contains("synthetic occupied port"));
    for _ in 0..3 {
        tick(&mut state, &mut manager, &inventory, 1, false, true, Ok(()));
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn new_open_loses_authority_if_inventory_fails_or_is_replaced_during_readiness() {
    for failed in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        let calls = Arc::new(AtomicUsize::new(0));
        let factory_calls = calls.clone();
        let mut manager = with_socket_factory(move |_| {
            factory_calls.fetch_add(1, Ordering::SeqCst);
            let stream = socket_transport(address)?;
            started_tx.send(()).unwrap();
            release_rx
                .lock()
                .recv_timeout(Duration::from_secs(3))
                .unwrap();
            Ok(stream)
        });
        let inventory = inventory();
        let mut state = selected();
        request_confirmed(&mut state, &manager, &inventory);
        state.scanned(1);
        tick(&mut state, &mut manager, &inventory, 1, false, true, Ok(()));
        let mut stream = peer(&listener);
        started_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        // A failed scan retains descriptors for display; neither those stale
        // descriptors nor a newer successful scan preserves this open's consent.
        tick(
            &mut state,
            &mut manager,
            &inventory,
            2,
            false,
            !failed,
            Ok(()),
        );
        assert_failed(&state);
        assert_eq!(
            manager.session("dfu-test").unwrap().state,
            ConnectionState::Connecting
        );
        release_tx.send(()).unwrap();
        connected(&mut manager, "dfu-test");
        for _ in 0..3 {
            tick(&mut state, &mut manager, &inventory, 2, false, true, Ok(()));
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        manager.shutdown().unwrap().join().unwrap();
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes).unwrap();
        assert!(bytes.is_empty());
    }
}

fn associated_inventory() -> Inventory {
    let mut inventory = inventory();
    inventory.receivers.push(crate::domain::ReceiverEndpoint {
        key: "synthetic-receiver-a".into(),
        path: b"synthetic-receiver-a".to_vec(),
        label: "Receiver".into(),
        serial_number: Some("synthetic-dfu".into()),
    });
    inventory
}

#[test]
fn original_healthy_receiver_owner_is_not_revoked_by_a_routine_scan_in_progress() {
    let (mut manager, listener) = fixture();
    manager.connect("dfu-test").unwrap();
    let mut stream = peer(&listener);
    connected(&mut manager, "dfu-test");
    let inventory = associated_inventory();
    let mut state = selected();
    state.request(&manager, &inventory, true, Ok(()));
    assert!(state.warning.is_none());
    assert!(state.busy());
    tick(&mut state, &mut manager, &inventory, 2, true, false, Ok(()));
    finish(&mut state, &mut manager, &inventory);
    assert!(matches!(state.outcome, Some(SerialDfuOutcome::Sent)));
    manager.shutdown().unwrap().join().unwrap();
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"dfu\n");
}

#[test]
fn frozen_receiver_association_cannot_be_replaced_by_another_hid_with_the_same_serial() {
    for owned in [false, true] {
        let (mut manager, listener) = fixture();
        let mut stream = if owned {
            manager.connect("dfu-test").unwrap();
            let stream = peer(&listener);
            connected(&mut manager, "dfu-test");
            Some(stream)
        } else {
            None
        };
        let mut inventory = associated_inventory();
        let mut state = selected();
        state.request(&manager, &inventory, true, Ok(()));
        assert!(state.warning.is_none());
        if !owned {
            state.scanned(1);
        }
        inventory.receivers[0].key = "synthetic-receiver-b".into();
        inventory.receivers[0].path = b"synthetic-receiver-b".to_vec();
        assert!(recognized_device("dfu-test", &manager, &inventory, true));
        tick(&mut state, &mut manager, &inventory, 1, false, true, Ok(()));
        assert_failed(&state);
        if owned {
            manager.shutdown().unwrap().join().unwrap();
            let mut bytes = Vec::new();
            stream.as_mut().unwrap().read_to_end(&mut bytes).unwrap();
            assert!(bytes.is_empty());
        } else {
            assert!(manager.session("dfu-test").is_none());
        }
    }
}

#[test]
fn connecting_owner_loses_submission_authority_without_current_inventory() {
    for disappeared in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        let mut manager = with_socket_factory(move |_| {
            let stream = socket_transport(address)?;
            started_tx.send(()).unwrap();
            release_rx
                .lock()
                .recv_timeout(Duration::from_secs(3))
                .unwrap();
            Ok(stream)
        });
        manager.connect("dfu-test").unwrap();
        let mut stream = peer(&listener);
        started_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        let mut inventory = inventory();
        let mut state = selected();
        request_confirmed(&mut state, &manager, &inventory);
        if disappeared {
            inventory.ports.clear();
        }
        tick(
            &mut state,
            &mut manager,
            &inventory,
            1,
            false,
            disappeared,
            Ok(()),
        );
        assert_failed(&state);
        assert_eq!(
            manager.session("dfu-test").unwrap().state,
            ConnectionState::Connecting
        );
        release_tx.send(()).unwrap();
        connected(&mut manager, "dfu-test");
        tick(&mut state, &mut manager, &inventory, 2, false, true, Ok(()));
        manager.shutdown().unwrap().join().unwrap();
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes).unwrap();
        assert!(bytes.is_empty());
    }
}

#[test]
fn raw_or_ambiguous_interface_cannot_enter_dfu_even_with_show_other_or_old_console_banner() {
    let (mut manager, listener) = fixture();
    manager.connect("dfu-test").unwrap();
    let mut stream = peer(&listener);
    connected(&mut manager, "dfu-test");
    stream
        .write_all(b"Device name: SlimeNRF Receiver\n")
        .unwrap();
    let mut inventory = inventory();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !recognized_device("dfu-test", &manager, &inventory, true) {
        assert!(Instant::now() < deadline);
        let _ = manager.poll();
        thread::sleep(Duration::from_millis(2));
    }
    for description in [
        InterfaceDescription::Value("SlimeNRF Raw Data CDC".into()),
        InterfaceDescription::Ambiguous,
    ] {
        inventory.ports[0].usb_interface = Some(UsbInterface {
            number: Some(2),
            description,
        });
        assert!(!recognized_device("dfu-test", &manager, &inventory, true));
        for show_other in [false, true] {
            let mut state = selected();
            state.show_other = show_other;
            state.request(&manager, &inventory, true, Ok(()));
            assert!(state.warning.is_none());
            assert!(!state.busy());
            assert_failed(&state);
        }
    }
    manager.shutdown().unwrap().join().unwrap();
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).unwrap();
    assert!(bytes.is_empty());
}

#[test]
fn pending_dfu_is_revoked_by_new_unknown_sibling_without_endpoint_replacement() {
    let mut manager = with_socket_factory(|_| panic!("revoked route must not open"));
    let mut inventory = associated_inventory();
    let mut state = selected();
    state.request(&manager, &inventory, true, Ok(()));
    assert!(state.busy());
    state.scanned(1);
    let mut sibling = inventory.ports[0].clone();
    sibling.port_name = "another-interface".into();
    inventory.ports.push(sibling);
    tick(&mut state, &mut manager, &inventory, 1, false, true, Ok(()));
    assert_failed(&state);
    assert!(!state.busy());
    assert!(manager.session("dfu-test").is_none());
}

#[test]
fn frozen_dfu_confirmation_rejects_interface_replacement_but_not_display_renaming() {
    let manager = with_socket_factory(|_| panic!("confirmation must not open"));
    for changed_identity in [false, true] {
        let mut inventory = inventory();
        inventory.ports[0].usb_interface = Some(UsbInterface {
            number: Some(0),
            description: InterfaceDescription::Unavailable,
        });
        let mut state = selected();
        state.request(&manager, &inventory, true, Ok(()));
        assert!(state.warning.is_some());
        if changed_identity {
            inventory.ports[0].usb_interface.as_mut().unwrap().number = Some(2);
        } else {
            inventory.ports[0].label = "Updated display label".into();
        }
        state.confirm(&manager, &inventory, Ok(()));
        assert_eq!(state.busy(), !changed_identity);
        if changed_identity {
            assert_failed(&state);
        }
    }
}

#[gpui_kit::test]
fn pending_dfu_refresh_owns_only_its_archive_and_survives_terminal_dismissal(
    cx: &mut gpui_kit::TestAppContext,
) {
    let _ui_session = crate::test_support::UiTestSession::new(cx);
    use crate::application::test_support::offline_workbench;
    use gpui_kit::VisualContext as _;

    let (mut manager, listener) = fixture();
    let mut inventory = inventory();
    inventory.ports.push(SerialEndpoint {
        port_name: "unrelated-archive".into(),
        serial_number: Some("synthetic-unrelated-archive".into()),
        ..inventory.ports[0].clone()
    });
    manager.update_inventory(&inventory, Instant::now());
    for port in ["dfu-test", "unrelated-archive"] {
        manager.connect(port).unwrap();
        let mut old_stream = peer(&listener);
        connected(&mut manager, port);
        manager.disconnect(port).unwrap();
        stopped(&mut manager, port);
        let mut bytes = Vec::new();
        old_stream.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"");
    }
    let archived_generation = manager.session("dfu-test").unwrap().generation;
    let runtime = crate::app_runtime::AppRuntime::new().unwrap();
    let executor = runtime.executor();

    let cx = cx.add_empty_window();
    let workbench = cx.new_window_entity(move |window, cx| {
        let mut this = offline_workbench(window, cx, manager, executor);
        this.inventory = inventory;
        this.sync_tabs(window, cx);
        this
    });
    cx.update(|window, cx| {
        workbench.update(cx, |this, cx| {
            this.flash_select_serial_dfu("dfu-test".into());
            this.flash_start_serial_dfu(cx);
            assert!(this.flash.serial_dfu.warning.is_some());
            this.scanning_inventory = true;
            this.inventory_generation = 1;
            this.flash_confirm_serial_dfu(cx);
            this.flash.serial_dfu.scanned(1);
            assert!(matches!(
                this.flash.serial_dfu.pending.as_ref().unwrap().phase,
                SerialDfuPhase::Inventory(Some(1)),
            ));
            assert!(!this.can_dismiss_terminal("dfu-test"));
            assert!(!this.can_request_terminal_close("dfu-test"));
            assert!(this.can_dismiss_terminal("unrelated-archive"));
            this.request_terminal_close("dfu-test", window, cx);
            assert!(!this.manager.is_close_requested("dfu-test"));
            assert_eq!(
                this.manager.session("dfu-test").unwrap().generation,
                archived_generation,
            );
            this.request_terminal_close("unrelated-archive", window, cx);
            assert!(this.manager.session("unrelated-archive").is_none());
            assert_eq!(this.terminals.len(), 1);
            assert!(this.flash.serial_dfu.busy());
            this.scanning_inventory = false;
            this.manager
                .update_inventory(&this.inventory, Instant::now());
            this.poll_serial_dfu(cx);
            assert!(matches!(
                this.flash.serial_dfu.pending.as_ref().unwrap().phase,
                SerialDfuPhase::Connecting(_),
            ));
        });
    });
    let mut stream = peer(&listener);
    crate::sessions::test_support::until(|| {
        workbench.update(cx, |this, cx| {
            this.poll_serial_dfu(cx);
            !this.flash.serial_dfu.busy()
        })
    });
    workbench.update(cx, |this, _| {
        assert!(matches!(
            this.flash.serial_dfu.outcome,
            Some(SerialDfuOutcome::Sent)
        ));
        assert!(this.manager.session("dfu-test").unwrap().generation > archived_generation);
        this.manager.shutdown().unwrap().join().unwrap();
    });
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"dfu\n");
}
