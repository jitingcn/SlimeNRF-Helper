use super::{CapturePin, CapturePreparation, CaptureSession};
use crate::discovery::ReceiverMonitor;
use crate::domain::{
    DeviceIdentity, InterfaceDescription, Inventory, ReceiverEndpoint, SerialEndpoint,
    SerialPortKind, UsbInterface,
};
use crate::features::capture::device::{
    CaptureMode, CaptureRequest, CaptureSource, CaptureTransport, CaptureWorker,
    TestConsoleReleaseWorker,
};
use crate::sessions::SessionManager;
use std::collections::BTreeSet;
use std::time::{Duration, Instant};

fn pin() -> CapturePin {
    CapturePin {
        receiver: ReceiverEndpoint {
            key: "main-a".into(),
            path: vec![1],
            label: "Receiver A".into(),
            serial_number: Some("USB-A".into()),
        },
        addresses: BTreeSet::from(["AABBCCDDEEFF".into()]),
        ports: BTreeSet::from(["COM8".into()]),
        learned_ports: Default::default(),
        learned_usb: Default::default(),
    }
}

fn pinned() -> CaptureSession {
    CaptureSession {
        pin: Some(pin()),
        ..Default::default()
    }
}

fn request() -> CaptureRequest {
    CaptureRequest {
        source: CaptureSource {
            receiver: pin().receiver,
            transport: CaptureTransport::Cdc {
                endpoint: receiver_port("capture-test", 2, true),
                console: Box::new(receiver_port("console-test", 0, false)),
            },
        },
        mode: CaptureMode::AllPaired { rate_hz: 100 },
        destination: "capture-test.slimecap".into(),
        roster: vec![(0, "AABBCCDDEEFF".into())],
    }
}

fn pending_capture(manager: &SessionManager) -> (CaptureSession, ReceiverMonitor, Inventory) {
    let source = request().source;
    let mut monitor = ReceiverMonitor::fixture(source.receiver.clone(), 0, "AABBCCDDEEFF", true);
    let inventory = Inventory {
        receivers: vec![source.receiver.clone()],
        ports: vec![
            receiver_port("console-test", 0, false),
            receiver_port("capture-test", 2, true),
        ],
        ..Inventory::default()
    };
    let preparation = CapturePreparation::freeze(
        source,
        CaptureMode::AllPaired { rate_hz: 100 },
        "capture-test.slimecap".into(),
        &inventory,
        manager,
        &monitor,
    )
    .unwrap();
    let mut session = CaptureSession::default();
    session.prepare(preparation, &mut monitor).unwrap();
    (session, monitor, inventory)
}

#[test]
fn frozen_capture_identity_blocks_only_related_ports() {
    let session = pinned();
    assert!(session.blocks_port("COM8", None, None));
    assert!(!session.blocks_port("COM9", None, None));
    let endpoint = SerialEndpoint {
        port_name: "COM10".into(),
        label: "Receiver".into(),
        manufacturer: None,
        product: None,
        kind: crate::domain::SerialPortKind::Usb,
        serial_number: Some("usb-a".into()),
        vid: None,
        pid: None,
        usb_interface: None,
    };
    assert!(session.blocks_port("COM10", None, Some(&endpoint)));
    let tracker = DeviceIdentity {
        address: Some("aa:bb:cc:dd:ee:ff".into()),
        ..Default::default()
    };
    assert!(session.blocks_port("COM11", Some(&tracker), None));
    assert!(session.blocks_port("COM11", Some(&DeviceIdentity::default()), None));
    assert!(!session.blocks_port("COM12", Some(&DeviceIdentity::default()), None));
    let usb_tracker = SerialEndpoint {
        port_name: "COM11".into(),
        label: "Tracker".into(),
        manufacturer: None,
        product: None,
        kind: crate::domain::SerialPortKind::Usb,
        serial_number: Some("TRACKER-A".into()),
        vid: Some(0x1209),
        pid: Some(0x7691),
        usb_interface: None,
    };
    assert!(session.blocks_port("COM11", Some(&tracker), Some(&usb_tracker)));
    let moved = SerialEndpoint {
        port_name: "COM15".into(),
        ..usb_tracker
    };
    assert!(session.blocks_port("COM15", Some(&DeviceIdentity::default()), Some(&moved)));
}

#[test]
fn canonical_windows_port_alias_cannot_bypass_capture() {
    let session = pinned();
    if cfg!(windows) {
        assert!(session.blocks_port(r"\\.\com8", None, None));
    }
    assert!(!session.blocks_port("COM80", None, None));
}

#[test]
fn stop_retains_identity_and_serial_lease_until_worker_joins() {
    let (worker, unblock) = CaptureWorker::test_blocked_worker();
    let mut manager = SessionManager::new();
    let mut monitor = ReceiverMonitor::new();
    // reserve() never opens a device, even for an unconnected port.
    let token = manager.reserve("capture-test", None).unwrap();
    let mut session = CaptureSession {
        worker: Some(worker),
        pin: Some(pin()),
        serial_leases: vec![token],
        ..Default::default()
    };
    // Drop the sender before session on assertion failure, so its worker can join.
    let unblock = unblock;
    let inventory = Inventory::default();
    let publication = session.snapshot().unwrap();
    session.stop(&mut manager, &mut monitor);
    assert!(!session.poll(&inventory, &mut manager, &mut monitor));
    assert!(session.take_completion().is_none());
    assert!(session.busy());
    assert!(session.blocks_port("COM8", None, None));
    let console = receiver_port("console-test", 0, false);
    let data = receiver_port("capture-test", 2, true);
    assert!(session.blocks_port(&console.port_name, None, Some(&console)));
    assert!(session.blocks_port(&data.port_name, None, Some(&data)));
    let console_lease = manager.reserve(&console.port_name, None).unwrap();
    manager.release(&console_lease).unwrap();
    assert!(manager.reserve("capture-test", None).is_err());
    unblock.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        session.poll(&inventory, &mut manager, &mut monitor);
        if let Some(result) = session.take_completion() {
            assert_eq!(result.unwrap_err(), "Test worker released");
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    assert!(std::sync::Arc::ptr_eq(
        &publication,
        &session.snapshot().unwrap()
    ));
    assert!(session.take_completion().is_none());
    assert!(!session.busy());
    assert!(!session.running());
    assert!(!session.blocks_port("COM8", None, None));
    assert!(!session.blocks_port(&console.port_name, None, Some(&console)));
    assert!(!session.blocks_port(&data.port_name, None, Some(&data)));
    assert!(manager.reserve("capture-test", None).is_ok());
}

#[test]
fn cancelled_preparation_retains_console_until_close_and_raw_until_join() {
    let TestConsoleReleaseWorker {
        worker,
        closed,
        released,
        unblock,
    } = CaptureWorker::test_console_release_worker();
    let mut manager = SessionManager::new();
    let mut monitor = ReceiverMonitor::new();
    let raw = manager.reserve("capture-test", None).unwrap();
    let console = manager.reserve("console-test", None).unwrap();
    let mut session = CaptureSession {
        worker: Some(worker),
        pin: Some(pin()),
        serial_leases: vec![raw],
        console_lease: Some(console),
        ..Default::default()
    };
    // Ensure the worker's gate is dropped first if an assertion fails.
    let (closed, released, unblock) = (closed, released, unblock);
    let inventory = Inventory::default();
    assert!(!session.poll(&inventory, &mut manager, &mut monitor));
    assert!(manager.reserve("console-test", None).is_err());
    assert!(manager.reserve("capture-test", None).is_err());
    session.stop(&mut manager, &mut monitor);
    assert!(!session.poll(&inventory, &mut manager, &mut monitor));
    assert!(manager.reserve("console-test", None).is_err());
    assert!(manager.reserve("capture-test", None).is_err());

    closed.send(()).unwrap();
    assert!(session.poll(&inventory, &mut manager, &mut monitor));
    assert!(released.try_recv().unwrap().is_ok());
    let available = manager.reserve("console-test", None).unwrap();
    manager.release(&available).unwrap();
    assert!(manager.reserve("capture-test", None).is_err());

    session.stop(&mut manager, &mut monitor);
    assert!(manager.reserve("capture-test", None).is_err());
    unblock.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while session.take_completion().is_none() {
        session.poll(&inventory, &mut manager, &mut monitor);
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    assert!(!session.busy());
    assert!(manager.reserve("capture-test", None).is_ok());
}

#[test]
fn stop_pending_handoff_releases_lease_and_command_gate_without_starting_worker() {
    let mut manager = SessionManager::new();
    let mut monitor = ReceiverMonitor::new();
    let token = manager.reserve("capture-test", None).unwrap();
    let console = manager.reserve("console-test", None).unwrap();
    let mut session = CaptureSession {
        pending: Some(request()),
        pin: Some(pin()),
        serial_leases: vec![token],
        console_lease: Some(console),
        ..Default::default()
    };
    assert!(session.pending());
    assert!(session.blocks_port("COM8", None, None));
    session.stop(&mut manager, &mut monitor);
    assert!(!session.pending());
    assert!(!session.busy());
    assert!(!session.running());
    assert!(session.take_completion().is_none());
    assert!(session.take_error().is_none());
    assert!(manager.reserve("capture-test", None).is_ok());
    assert!(manager.reserve("console-test", None).is_ok());
}

#[test]
fn connecting_console_is_not_taken_over_while_capture_handoff_waits() {
    use crate::sessions::ConnectionState;
    use crate::sessions::test_support::{
        display_manager, display_session_state, display_test_session,
    };

    for state in [ConnectionState::Connecting, ConnectionState::Closing] {
        let mut manager = display_manager();
        let _display = display_test_session(&mut manager, "console-test");
        display_session_state(&mut manager, "console-test", state);
        let source = request().source;
        let mut monitor =
            ReceiverMonitor::fixture(source.receiver.clone(), 0, "AABBCCDDEEFF", true);
        let inventory = Inventory {
            receivers: vec![source.receiver.clone()],
            ports: vec![
                receiver_port("console-test", 0, false),
                receiver_port("capture-test", 2, true),
            ],
            ..Inventory::default()
        };
        let preparation = CapturePreparation::freeze(
            source,
            CaptureMode::AllPaired { rate_hz: 100 },
            "capture-test.slimecap".into(),
            &inventory,
            &manager,
            &monitor,
        )
        .unwrap();
        let mut session = CaptureSession::default();
        session.prepare(preparation, &mut monitor).unwrap();
        session.poll(&inventory, &mut manager, &mut monitor);
        assert!(session.pending());
        assert!(!session.running());
        assert!(session.console_lease.is_none());
        assert_eq!(manager.session("console-test").unwrap().state, state);
        assert!(manager.reserve("capture-test", None).is_err());
        session.stop(&mut manager, &mut monitor);
        assert!(manager.reserve("capture-test", None).is_ok());
        assert_eq!(manager.session("console-test").unwrap().state, state);
    }
}

#[test]
fn console_reserved_by_another_owner_is_not_taken_over() {
    let mut manager = SessionManager::new();
    let other_owner = manager.reserve("console-test", None).unwrap();
    let source = request().source;
    let mut monitor = ReceiverMonitor::fixture(source.receiver.clone(), 0, "AABBCCDDEEFF", true);
    let inventory = Inventory {
        receivers: vec![source.receiver.clone()],
        ports: vec![
            receiver_port("console-test", 0, false),
            receiver_port("capture-test", 2, true),
        ],
        ..Inventory::default()
    };
    let preparation = CapturePreparation::freeze(
        source,
        CaptureMode::AllPaired { rate_hz: 100 },
        "capture-test.slimecap".into(),
        &inventory,
        &manager,
        &monitor,
    )
    .unwrap();
    let mut session = CaptureSession::default();
    session.prepare(preparation, &mut monitor).unwrap();
    session.poll(&inventory, &mut manager, &mut monitor);
    assert!(session.pending());
    assert!(!session.running());
    assert!(session.console_lease.is_none());
    session.stop(&mut manager, &mut monitor);
    assert!(manager.reserve("capture-test", None).is_ok());
    assert!(manager.reserve("console-test", None).is_err());
    manager.release(&other_owner).unwrap();
}

#[test]
fn closing_terminal_rejects_capture_and_releases_the_pin() {
    use crate::sessions::test_support::{display_manager, display_test_session};

    let mut manager = display_manager();
    let _display = display_test_session(&mut manager, "console-test");
    manager.request_close("console-test").unwrap();
    let (mut session, mut monitor, inventory) = pending_capture(&manager);
    session.poll(&inventory, &mut manager, &mut monitor);
    assert!(!session.pending());
    assert!(!session.busy());
    assert!(!session.running());
    assert!(session.take_error().unwrap().contains("closing"));
    assert!(manager.is_close_requested("console-test"));
    assert!(!session.blocks_port("console-test", None, None));
}

#[test]
fn exhausted_console_reconnect_rejects_capture_and_allows_manual_recovery() {
    use crate::sessions::test_support::{display_manager, exhausted_reconnect_session};

    let mut manager = display_manager();
    let console = receiver_port("console-test", 0, false);
    exhausted_reconnect_session(&mut manager, &console);
    assert!(manager.is_reconnecting("console-test"));
    assert!(!manager.can_forget("console-test"));
    let (mut session, mut monitor, inventory) = pending_capture(&manager);
    session.poll(&inventory, &mut manager, &mut monitor);
    assert!(!session.pending());
    assert!(!session.busy());
    assert!(!session.running());
    assert!(session.take_error().unwrap().contains("reconnect"));
    assert!(!session.blocks_port("console-test", None, Some(&console)));
    manager.disconnect("console-test").unwrap();
    assert!(manager.can_forget("console-test"));
}

#[test]
fn interrupted_console_dump_rejects_capture_instead_of_blocking_reconnect() {
    use crate::sessions::ConnectionState;
    use crate::sessions::test_support::{completed, connected, fixture, peer};
    use std::io::Read;

    let (mut manager, listener) = fixture();
    let source = request().source;
    let inventory = Inventory {
        receivers: vec![source.receiver],
        ports: vec![
            receiver_port("console-test", 0, false),
            receiver_port("capture-test", 2, true),
        ],
        ..Inventory::default()
    };
    manager.update_inventory(&inventory, Instant::now());
    manager.connect("console-test").unwrap();
    let mut serial_peer = peer(&listener);
    connected(&mut manager, "console-test");
    let mut receipt = manager.send_line("console-test", "tcal dump").unwrap();
    let mut command = [0; 10];
    serial_peer.read_exact(&mut command).unwrap();
    assert_eq!(&command, b"tcal dump\n");
    completed(&mut receipt).unwrap();
    let dump = manager.active_thermal_dumps().pop().unwrap();
    assert!(manager.cancel_thermal_dump(&dump));
    assert!(manager.thermal_dump_needs_reconnect("console-test"));

    let (mut session, mut monitor, inventory) = pending_capture(&manager);
    session.poll(&inventory, &mut manager, &mut monitor);
    assert!(!session.pending());
    assert!(!session.busy());
    assert!(!session.running());
    assert!(session.take_error().unwrap().contains("temperature dump"));
    assert_eq!(
        manager.session("console-test").unwrap().state,
        ConnectionState::Connected
    );
    assert!(!session.blocks_port("console-test", None, None));
    manager.disconnect("console-test").unwrap();
}

#[test]
fn replaced_console_rejects_capture_handoff_before_open_or_start() {
    let mut manager = SessionManager::new();
    let source = request().source;
    let mut monitor = ReceiverMonitor::fixture(source.receiver.clone(), 0, "AABBCCDDEEFF", true);
    let inventory = Inventory {
        receivers: vec![source.receiver.clone()],
        ports: vec![
            receiver_port("console-test", 0, false),
            receiver_port("capture-test", 2, true),
        ],
        ..Inventory::default()
    };
    let preparation = CapturePreparation::freeze(
        source,
        CaptureMode::AllPaired { rate_hz: 100 },
        "capture-test.slimecap".into(),
        &inventory,
        &manager,
        &monitor,
    )
    .unwrap();
    let mut session = CaptureSession::default();
    session.prepare(preparation, &mut monitor).unwrap();
    let changed = Inventory {
        ports: vec![
            receiver_port("replacement-console", 0, false),
            receiver_port("capture-test", 2, true),
        ],
        ..inventory
    };
    session.poll(&changed, &mut manager, &mut monitor);
    assert!(!session.pending());
    assert!(!session.running());
    assert!(!session.busy());
    assert!(session.take_error().unwrap().contains("source changed"));
    assert!(manager.reserve("capture-test", None).is_ok());
    assert!(manager.reserve("console-test", None).is_ok());
}

#[test]
fn failed_admission_does_not_install_command_gate_or_pending_request() {
    let mut session = CaptureSession::default();
    let mut monitor = ReceiverMonitor::new();
    let preparation = CapturePreparation {
        request: request(),
        pin: pin(),
    };
    // No monitor endpoint exists; reserve rejects admission without opening devices.
    assert!(session.prepare(preparation, &mut monitor).is_err());
    assert!(!session.busy());
    assert!(!session.pending());
    assert!(session.receiver().is_none());
    assert!(!session.blocks_port("COM8", None, None));
}

#[test]
fn shutdown_transfers_worker_but_retains_capture_exclusion_until_app_cleanup() {
    let (worker, unblock) = CaptureWorker::test_blocked_worker();
    let mut manager = SessionManager::new();
    let token = manager.reserve("capture-test", None).unwrap();
    let mut session = CaptureSession {
        worker: Some(worker),
        pin: Some(pin()),
        serial_leases: vec![token],
        ..Default::default()
    };
    let mut cleanup = session.take_shutdown_worker().unwrap();
    // Drop the sender before cleanup on assertion failure, so its worker can join.
    let unblock = unblock;
    assert!(cleanup.join().is_none());
    assert!(session.busy());
    assert!(session.blocks_port("COM8", None, None));
    assert!(manager.reserve("capture-test", None).is_err());
    assert!(session.take_shutdown_worker().is_none());
    unblock.send(()).unwrap();
    // The app's background cleanup drops this retained worker to finalize and join.
    drop(cleanup);
    assert!(session.busy());
    assert!(manager.reserve("capture-test", None).is_err());
}

fn receiver_port(port: &str, number: u8, raw: bool) -> SerialEndpoint {
    SerialEndpoint {
        port_name: port.into(),
        label: port.into(),
        manufacturer: None,
        product: None,
        kind: SerialPortKind::Usb,
        serial_number: Some("USB-A".into()),
        vid: Some(0x1209),
        pid: Some(0x7690),
        usb_interface: Some(UsbInterface {
            number: Some(number),
            description: if raw {
                InterfaceDescription::Value("SlimeNRF Raw Data CDC".into())
            } else {
                InterfaceDescription::Unavailable
            },
        }),
    }
}
