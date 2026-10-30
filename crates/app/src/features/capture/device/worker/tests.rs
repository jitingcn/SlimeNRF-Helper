use super::{CapturePhase, CaptureSnapshot, CaptureWorker};
use crate::features::capture::device::collection::tests::{Sim, request};
use crate::features::capture::device::collection::{collect, expected_roster};
use parking_lot::Mutex;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
#[test]
fn join_never_detaches_owned_io_or_releases_external_lease() {
    let dir = tempfile::tempdir().unwrap();
    let request = request(dir.path().join("joined.slimecap"), false);
    let cancel = Arc::new(AtomicBool::new(false));
    let state = Arc::new(Mutex::new(Arc::new(CaptureSnapshot::default())));
    let thread_state = Arc::clone(&state);
    let thread_cancel = Arc::clone(&cancel);
    let (release, wait) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        wait.recv().unwrap();
        let mut io = Sim::new(Arc::clone(&thread_cancel));
        collect(
            &request,
            expected_roster(&request).unwrap(),
            &mut io,
            &thread_cancel,
            &thread_state,
        )
    });
    let mut worker = CaptureWorker {
        cancel,
        state,
        handle: Some(handle),
        console_release: None,
        console_cancel: None,
    };
    // External lease remains caller-owned even when cancellation is requested.
    let external_lease = Arc::new(());
    worker.stop();
    assert!(worker.join().is_none());
    assert_eq!(Arc::strong_count(&external_lease), 1);
    release.send(()).unwrap();
    // The test explicitly joins the private handle; production uses nonblocking join().
    let result = worker.handle.take().unwrap().join().unwrap().unwrap();
    assert!(result.path.exists());
    assert!(worker.is_finished());
    drop(external_lease);
}
#[test]
fn snapshot_polling_reuses_immutable_publication_until_worker_replaces_it() {
    let state = Arc::new(Mutex::new(Arc::new(CaptureSnapshot::default())));
    let worker = CaptureWorker {
        cancel: Arc::new(AtomicBool::new(false)),
        state: Arc::clone(&state),
        handle: None,
        console_release: None,
        console_cancel: None,
    };
    let first = worker.snapshot();
    assert!(Arc::ptr_eq(&first, &worker.snapshot()));
    let mut updated = CaptureSnapshot::default();
    updated.counters.samples = 7;
    updated.phase = CapturePhase::Receiving;
    *state.lock() = Arc::new(updated);
    let next = worker.snapshot();
    assert!(!Arc::ptr_eq(&first, &next));
    assert!(Arc::ptr_eq(&next, &worker.snapshot()));
    assert_eq!(first.counters.samples, 0);
    assert_eq!(next.counters.samples, 7);
}

#[test]
fn stop_cancels_a_deferred_console_probe_before_native_open_completes() {
    use crate::domain::{
        InterfaceDescription, Inventory, SerialEndpoint, SerialPortKind, UsbInterface,
    };
    use crate::sessions::ConnectionState;
    use crate::sessions::test_support::{connected, fixture, peer};
    use std::io::{self, Read};
    use std::time::{Duration, Instant};

    let endpoint = SerialEndpoint {
        port_name: "console".into(),
        label: "Console".into(),
        manufacturer: None,
        product: None,
        kind: SerialPortKind::Usb,
        serial_number: Some("receiver".into()),
        vid: Some(0x1209),
        pid: Some(0x7690),
        usb_interface: Some(UsbInterface {
            number: Some(0),
            description: InterfaceDescription::Unavailable,
        }),
    };
    let inventory = Inventory {
        ports: vec![endpoint.clone()],
        ..Inventory::default()
    };
    let (mut manager, listener) = fixture();
    manager.update_inventory(&inventory, Instant::now());
    manager.connect(&endpoint.port_name).unwrap();
    let mut serial_peer = peer(&listener);
    connected(&mut manager, &endpoint.port_name);
    let probe = manager.begin_capture_console_probe(&endpoint).unwrap();
    let probe_cancel = probe.cancel_token();
    let cancel = Arc::new(AtomicBool::new(false));
    let thread_cancel = Arc::clone(&cancel);
    let (release, gate) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        gate.recv().map_err(|error| error.to_string())?;
        probe.wait(&thread_cancel)?;
        Err("cancelled probe unexpectedly succeeded".into())
    });
    let mut worker = CaptureWorker {
        cancel,
        state: Arc::new(Mutex::new(Arc::new(CaptureSnapshot::default()))),
        handle: Some(handle),
        console_release: None,
        console_cancel: Some(Arc::clone(&probe_cancel)),
    };
    // Drop the sender before the worker on assertion failure.
    let release = release;
    worker.stop();
    assert!(probe_cancel.load(Ordering::Acquire));
    assert!(!worker.is_finished());
    release.send(()).unwrap();
    assert!(worker.handle.take().unwrap().join().unwrap().is_err());
    serial_peer
        .set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    let error = serial_peer.read(&mut [0]).unwrap_err();
    assert!(matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    ));
    let _ = manager.poll();
    assert_eq!(
        manager.session(&endpoint.port_name).unwrap().state,
        ConnectionState::Connected
    );
    manager.shutdown().unwrap().join().unwrap();
}
