use super::{CapturePin, CapturePreparation, CaptureSession};
use crate::discovery::ReceiverMonitor;
use crate::domain::{
    DeviceIdentity, InterfaceDescription, Inventory, ReceiverEndpoint, SerialEndpoint,
    SerialPortKind, UsbInterface,
};
use crate::features::capture::device::{
    CaptureMode, CaptureRequest, CaptureSource, CaptureTransport, CaptureWorker,
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
            },
        },
        mode: CaptureMode::AllPaired { rate_hz: 100 },
        destination: "capture-test.slimecap".into(),
        roster: vec![(0, "AABBCCDDEEFF".into())],
    }
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
fn stop_pending_handoff_releases_lease_and_command_gate_without_starting_worker() {
    let mut manager = SessionManager::new();
    let mut monitor = ReceiverMonitor::new();
    let token = manager.reserve("capture-test", None).unwrap();
    let mut session = CaptureSession {
        pending: Some(request()),
        pin: Some(pin()),
        serial_leases: vec![token],
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
