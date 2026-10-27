use super::super::native::Transport;
use super::super::{ConnectionState, SessionManager, port_key};
use super::write_transport::PartialErrorWriter;
use super::{completed, connected, fixture, peer, socket_transport, stopped, until};
use crate::commands::{CommandContext, CommandSnapshot, RouteEvidence, presets};
use crate::domain::{DeviceRole, Inventory, SerialEndpoint};
use parking_lot::Mutex;
use std::io::{self, Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

#[test]
fn stale_inventory_blocks_a_real_arrival_until_successful_refresh() {
    let (mut manager, listener) = fixture();
    let scan_started_before_loss = Instant::now();
    let now = scan_started_before_loss + Duration::from_secs(1);
    let endpoint = usb("port", "serial");
    manager.update_inventory_at(
        &serial_inventory(std::slice::from_ref(&endpoint)),
        scan_started_before_loss,
        scan_started_before_loss,
    );
    manager.connect("port").unwrap();
    let stream = peer(&listener);
    connected_at(&mut manager, "port", now);
    drop(stream);
    stopped_at(&mut manager, "port", now);
    manager.update_inventory_at(&serial_inventory(&[]), scan_started_before_loss, now);
    manager.update_inventory_at(
        &serial_inventory(std::slice::from_ref(&endpoint)),
        scan_started_before_loss,
        now,
    );
    let stale = now + Duration::from_secs(11);
    let _ = manager.poll_at(stale);
    assert!(manager.entries[&port_key("port")].worker.is_none());
    assert!(manager.is_reconnecting("port"));
    manager.invalidate_inventory();
    let _ = manager.poll_at(stale);
    assert!(manager.entries[&port_key("port")].worker.is_none());
    manager.update_inventory_at(&serial_inventory(&[endpoint]), stale, stale);
    let _ = manager.poll_at(stale);
    let _new = peer(&listener);
    connected_at(&mut manager, "port", stale);
    assert_eq!(manager.session("port").unwrap().generation, 2);
}

fn usb(port: &str, serial: &str) -> SerialEndpoint {
    SerialEndpoint {
        port_name: port.into(),
        label: port.into(),
        manufacturer: None,
        product: None,
        kind: crate::domain::SerialPortKind::Usb,
        serial_number: Some(serial.into()),
        vid: Some(0x1209),
        pid: Some(0x7692),
        usb_interface: None,
    }
}

fn serial_inventory(endpoints: &[SerialEndpoint]) -> Inventory {
    Inventory {
        ports: endpoints.to_vec(),
        ..Inventory::default()
    }
}

fn stopped_at(manager: &mut SessionManager, port: &str, now: Instant) {
    until(|| {
        let _ = manager.poll_at(now);
        manager.entries[&port_key(port)].worker.is_none()
    });
}

fn connected_at(manager: &mut SessionManager, port: &str, now: Instant) {
    until(|| {
        let _ = manager.poll_at(now);
        manager.session(port).unwrap().state == ConnectionState::Connected
    });
}

#[test]
fn matching_inventory_after_read_failure_recovers_without_sampled_absence() {
    struct GatedReadFailure {
        history: io::Cursor<&'static [u8]>,
        ready: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
        writes: Arc<Mutex<Vec<u8>>>,
    }

    impl Read for GatedReadFailure {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            let length = self.history.read(bytes)?;
            if length != 0 {
                return Ok(length);
            }
            self.ready.send(()).unwrap();
            self.release.recv_timeout(Duration::from_secs(3)).unwrap();
            Err(io::Error::from_raw_os_error(22))
        }
    }

    impl Write for GatedReadFailure {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.writes.lock().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let recovered_transport = socket_transport(listener.local_addr().unwrap()).unwrap();
    let mut recovered_peer = peer(&listener);
    let (ready_tx, ready_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let old_writes = Arc::new(Mutex::new(Vec::new()));
    let first = Mutex::new(Some(GatedReadFailure {
        history: io::Cursor::new(
            b"Device address: 112233445566\nhistory before read failure\n".as_slice(),
        ),
        ready: ready_tx,
        release: release_rx,
        writes: Arc::clone(&old_writes),
    }));
    let second = Mutex::new(Some(recovered_transport));
    let opens = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&opens);
    let mut manager = SessionManager::with_factory(Arc::new(move |port| {
        assert_eq!(port, "COM41");
        match count.fetch_add(1, Ordering::SeqCst) {
            0 => Ok(Box::new(first.lock().take().unwrap()) as Box<dyn Transport>),
            1 => Ok(Box::new(second.lock().take().unwrap()) as Box<dyn Transport>),
            _ => panic!("unexpected additional transport open"),
        }
    }));
    let inventory = serial_inventory(&[usb("COM41", "synthetic-read-loss")]);
    let now = Instant::now();
    manager.update_inventory_at(&inventory, now, now);
    manager.connect("COM41").unwrap();
    ready_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    connected_at(&mut manager, "COM41", now);
    until(|| {
        let _ = manager.poll_at(now);
        let view = manager.session("COM41").unwrap();
        view.identity.address.is_some()
            && view.terminal.text().contains("history before read failure")
    });
    let generation = manager.session("COM41").unwrap().generation;
    let mut queued = manager.send_line("COM41", "must-not-replay").unwrap();
    let before_loss = now + Duration::from_secs(1);
    manager.update_inventory_at(&inventory, before_loss, before_loss);
    let _ = manager.poll_at(before_loss);
    assert_eq!(manager.session("COM41").unwrap().generation, generation);
    assert_eq!(
        manager.session("COM41").unwrap().state,
        ConnectionState::Connected
    );
    assert_eq!(opens.load(Ordering::SeqCst), 1);
    assert!(queued.poll().is_none());

    release_tx.send(()).unwrap();
    let lost_at = now + Duration::from_secs(2);
    stopped_at(&mut manager, "COM41", lost_at);
    assert!(completed(&mut queued).is_err());
    assert!(old_writes.lock().is_empty());
    assert!(manager.writes_idle("COM41"));
    assert_eq!(opens.load(Ordering::SeqCst), 1);
    assert_eq!(manager.session("COM41").unwrap().generation, generation);

    let refreshed = now + Duration::from_secs(3);
    manager.update_inventory_at(&inventory, refreshed, refreshed);
    connected_at(&mut manager, "COM41", refreshed);
    let view = manager.session("COM41").unwrap();
    assert_eq!(view.generation, generation + 1);
    assert!(view.identity.address.is_none());
    assert!(view.terminal.text().contains("history before read failure"));
    assert!(!manager.is_reconnecting("COM41"));
    for seconds in [4, 5, 6] {
        let later = now + Duration::from_secs(seconds);
        manager.update_inventory_at(&inventory, later, later);
        let _ = manager.poll_at(later);
        assert_eq!(manager.session("COM41").unwrap().generation, generation + 1);
        assert_eq!(
            manager.session("COM41").unwrap().state,
            ConnectionState::Connected
        );
        assert_eq!(opens.load(Ordering::SeqCst), 2);
    }
    let mut fresh = manager.send_line("COM41", "info").unwrap();
    assert!(completed(&mut fresh).is_ok());
    manager.disconnect("COM41").unwrap();
    stopped_at(&mut manager, "COM41", refreshed);
    let mut bytes = Vec::new();
    recovered_peer.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"info\n");
}

#[test]
fn explicit_close_disarms_healthy_and_pending_arrival_recovery() {
    for waiting_for_arrival in [false, true] {
        let (mut manager, listener) = fixture();
        let endpoint = usb("COM41", "synthetic-close");
        let inventory = serial_inventory(std::slice::from_ref(&endpoint));
        manager.update_inventory(&inventory, Instant::now());
        manager.connect("COM41").unwrap();
        let mut stream = Some(peer(&listener));
        connected(&mut manager, "COM41");
        let generation = manager.session("COM41").unwrap().generation;
        if waiting_for_arrival {
            drop(stream.take());
            stopped(&mut manager, "COM41");
            assert!(manager.is_reconnecting("COM41"));
            manager.update_inventory(&serial_inventory(&[]), Instant::now());
            manager.update_inventory(&inventory, Instant::now());
            assert!(
                manager.entries[&port_key("COM41")]
                    .reconnect
                    .as_ref()
                    .unwrap()
                    .arrival_pending
            );
        }
        let alias = if cfg!(windows) { r"\\.\com41" } else { "COM41" };
        manager.request_close(alias).unwrap();
        assert!(manager.is_close_requested("COM41"));
        assert!(!manager.is_reconnecting("COM41"));
        assert!(manager.entries[&port_key("COM41")].reconnect.is_none());
        stopped(&mut manager, "COM41");
        if let Some(mut stream) = stream {
            assert_eq!(stream.read(&mut [0; 1]).unwrap(), 0);
        }
        for _ in 0..3 {
            manager.update_inventory(&serial_inventory(&[]), Instant::now());
            manager.update_inventory(&inventory, Instant::now());
            let _ = manager.poll();
            assert!(!manager.is_reconnecting(alias));
            assert!(manager.entries[&port_key("COM41")].worker.is_none());
            assert_eq!(manager.session("COM41").unwrap().generation, generation);
            assert_eq!(manager.next_generation, generation);
        }
        assert!(manager.can_forget(alias));
        assert!(manager.forget(alias));
        manager.connect("COM41").unwrap();
        let _fresh = peer(&listener);
        connected(&mut manager, "COM41");
        assert_eq!(manager.session("COM41").unwrap().generation, generation + 1);
        assert!(!manager.is_close_requested(alias));
    }
}

#[test]
fn recovery_waits_for_unique_original_usb_and_keeps_history_not_authority() {
    let (mut manager, listener) = fixture();
    let original = usb("COM41", "simulated-original");
    let scan_started_before_loss = Instant::now();
    manager.update_inventory(
        &serial_inventory(std::slice::from_ref(&original)),
        scan_started_before_loss,
    );
    manager.connect("COM41").unwrap();
    let mut old = peer(&listener);
    connected(&mut manager, "COM41");
    assert!(!manager.is_reconnecting("COM41"));
    old.write_all(b"Device address: 112233445566\nold history\n")
        .unwrap();
    until(|| {
        let _ = manager.poll();
        manager.session("COM41").unwrap().identity.address.is_some()
    });
    let generation = manager.session("COM41").unwrap().generation;
    let evidence = RouteEvidence::Direct {
        port: "COM41",
        generation,
        connected: true,
        role: DeviceRole::Unknown,
    };
    let preset = presets(CommandContext::Direct(DeviceRole::Unknown))
        .find(|preset| preset.id == "info")
        .unwrap();
    let snapshot = CommandSnapshot::capture(preset, "", evidence).unwrap();
    let now = Instant::now();
    drop(old);
    stopped_at(&mut manager, "COM41", now);
    assert!(manager.is_reconnecting("COM41"));
    let error = manager.session("COM41").unwrap().last_error.clone();
    assert!(error.is_some());
    let due = now + Duration::from_secs(1);
    for endpoints in [
        vec![],
        vec![usb("COM41", "replacement")],
        vec![SerialEndpoint {
            vid: Some(0x239a),
            ..original.clone()
        }],
        vec![SerialEndpoint {
            pid: Some(0x7690),
            ..original.clone()
        }],
        vec![usb("COM42", "simulated-original")],
        vec![original.clone(), usb("COM42", "simulated-original")],
        vec![original.clone(), usb("COM41", "replacement")],
    ] {
        manager.update_inventory_at(&serial_inventory(&endpoints), scan_started_before_loss, due);
        let _ = manager.poll_at(due);
        assert!(manager.entries[&port_key("COM41")].worker.is_none());
        assert_eq!(manager.session("COM41").unwrap().generation, generation);
        assert!(manager.is_reconnecting("COM41"));
    }
    manager.update_inventory_at(
        &serial_inventory(std::slice::from_ref(&original)),
        scan_started_before_loss,
        due,
    );
    let _ = manager.poll_at(due);
    assert!(manager.entries[&port_key("COM41")].worker.is_none());
    // With only pre-loss scans, resolving ambiguity/replacement is not removal.
    // Sampled recovery still needs an absent canonical port and unique return.
    manager.update_inventory_at(&serial_inventory(&[]), scan_started_before_loss, due);
    manager.update_inventory_at(
        &serial_inventory(&[original]),
        scan_started_before_loss,
        due,
    );
    let _ = manager.poll_at(due);
    assert_eq!(manager.session("COM41").unwrap().last_error, error);
    let mut new = peer(&listener);
    connected_at(&mut manager, "COM41", due);
    let view = manager.session("COM41").unwrap();
    assert!(view.generation > generation);
    assert!(view.identity.address.is_none());
    assert!(view.terminal.text().contains("old history"));
    assert!(!manager.is_reconnecting("COM41"));
    assert!(
        snapshot
            .authorize(
                RouteEvidence::Direct {
                    port: "COM41",
                    generation: view.generation,
                    connected: true,
                    role: DeviceRole::Unknown,
                },
                false
            )
            .is_err()
    );
    new.write_all(b"new history\n").unwrap();
    until(|| {
        let _ = manager.poll_at(due);
        manager
            .session("COM41")
            .unwrap()
            .terminal
            .text()
            .contains("new history")
    });
    drop(new);
    stopped_at(&mut manager, "COM41", due);
    assert!(
        !manager.entries[&port_key("COM41")]
            .reconnect
            .as_ref()
            .unwrap()
            .arrival_pending
    );
}

#[test]
fn recovery_requires_identity_at_successful_open_and_never_retries_initial_failure() {
    for endpoints in [
        vec![],
        vec![usb("port", "")],
        vec![SerialEndpoint {
            vid: None,
            ..usb("port", "serial")
        }],
        vec![SerialEndpoint {
            pid: None,
            ..usb("port", "serial")
        }],
        vec![usb("port", "serial"), usb("other", "serial")],
    ] {
        let (mut manager, listener) = fixture();
        manager.update_inventory(&serial_inventory(&endpoints), Instant::now());
        manager.connect("port").unwrap();
        let stream = peer(&listener);
        connected(&mut manager, "port");
        drop(stream);
        let now = Instant::now();
        stopped_at(&mut manager, "port", now);
        manager.update_inventory(&serial_inventory(&[]), Instant::now());
        manager.update_inventory(&serial_inventory(&[usb("port", "serial")]), Instant::now());
        let _ = manager.poll_at(now);
        assert!(!manager.is_reconnecting("port"));
        assert_eq!(manager.session("port").unwrap().generation, 1);
    }
    let opens = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&opens);
    let mut manager = SessionManager::with_factory(Arc::new(move |_| {
        count.fetch_add(1, Ordering::SeqCst);
        Err(io::Error::new(io::ErrorKind::PermissionDenied, "occupied"))
    }));
    manager.update_inventory(&serial_inventory(&[usb("port", "serial")]), Instant::now());
    manager.connect("port").unwrap();
    let now = Instant::now();
    stopped_at(&mut manager, "port", now);
    manager.update_inventory(&serial_inventory(&[]), Instant::now());
    manager.update_inventory(&serial_inventory(&[usb("port", "serial")]), Instant::now());
    let _ = manager.poll_at(now);
    assert_eq!(
        opens.load(Ordering::SeqCst),
        1,
        "Initial open needs explicit retry"
    );
    assert!(!manager.is_reconnecting("port"));
}

#[test]
fn pre_loss_scans_and_failed_arrival_do_not_retry_without_another_removal() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let opens = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&opens);
    let mut manager = SessionManager::with_factory(Arc::new(move |_| {
        let attempt = count.fetch_add(1, Ordering::SeqCst);
        if attempt == 0 || attempt == 2 {
            Ok(Box::new(socket_transport(address)?) as Box<dyn Transport>)
        } else {
            Err(io::Error::new(io::ErrorKind::PermissionDenied, "occupied"))
        }
    }));
    let scan_started_before_loss = Instant::now();
    manager.update_inventory(
        &serial_inventory(&[usb("port", "serial")]),
        scan_started_before_loss,
    );
    manager.connect("port").unwrap();
    let stream = peer(&listener);
    connected(&mut manager, "port");
    drop(stream);
    let now = Instant::now();
    stopped_at(&mut manager, "port", now);
    for expected_opens in 1..=2 {
        for seconds in [0, 1, 2, 4, 8, 10, 100, 10_000] {
            let refreshed = now + Duration::from_secs(seconds);
            manager.update_inventory_at(
                &serial_inventory(&[usb("port", "serial")]),
                scan_started_before_loss,
                refreshed,
            );
            let _ = manager.poll_at(refreshed);
            assert!(manager.entries[&port_key("port")].worker.is_none());
            assert_eq!(opens.load(Ordering::SeqCst), expected_opens);
        }
        manager.update_inventory_at(&serial_inventory(&[]), scan_started_before_loss, now);
        let _ = manager.poll_at(now);
        assert_eq!(opens.load(Ordering::SeqCst), expected_opens);
        manager.update_inventory_at(
            &serial_inventory(&[usb("port", "serial")]),
            scan_started_before_loss,
            now,
        );
        let _ = manager.poll_at(now);
        if expected_opens == 1 {
            stopped_at(&mut manager, "port", now);
            assert_eq!(opens.load(Ordering::SeqCst), 2);
            assert!(
                manager
                    .session("port")
                    .unwrap()
                    .last_error
                    .as_ref()
                    .unwrap()
                    .contains("occupied")
            );
        }
    }
    let stream = peer(&listener);
    connected_at(&mut manager, "port", now);
    assert_eq!(opens.load(Ordering::SeqCst), 3);
    assert_eq!(manager.session("port").unwrap().generation, 3);
    assert!(!manager.is_reconnecting("port"));
    assert!(manager.session("port").unwrap().last_error.is_none());
    drop(stream);
    stopped_at(&mut manager, "port", now);
    manager.update_inventory_at(&serial_inventory(&[usb("port", "serial")]), now, now);
    let _ = manager.poll_at(now);
    assert_eq!(opens.load(Ordering::SeqCst), 3);
    assert!(manager.entries[&port_key("port")].worker.is_none());
}

#[test]
fn discovery_errors_and_ambiguous_snapshots_do_not_invent_arrivals() {
    let (mut manager, listener) = fixture();
    let original = usb("port", "serial");
    let scan_started_before_loss = Instant::now();
    manager.update_inventory(
        &serial_inventory(std::slice::from_ref(&original)),
        scan_started_before_loss,
    );
    manager.connect("port").unwrap();
    let stream = peer(&listener);
    connected(&mut manager, "port");
    drop(stream);
    stopped(&mut manager, "port");
    for endpoints in [
        vec![original.clone()],
        vec![usb("port", "replacement")],
        vec![original.clone(), usb("other", "serial")],
        vec![original.clone(), usb("port", "replacement")],
        vec![usb("port", "")],
    ] {
        manager.invalidate_inventory();
        let _ = manager.poll();
        assert!(!manager.inventory_is_fresh(Instant::now()));
        manager.update_inventory(&serial_inventory(&endpoints), scan_started_before_loss);
        let _ = manager.poll();
        manager.update_inventory(
            &serial_inventory(std::slice::from_ref(&original)),
            scan_started_before_loss,
        );
        let _ = manager.poll();
        assert!(manager.entries[&port_key("port")].worker.is_none());
        assert_eq!(manager.session("port").unwrap().generation, 1);
    }
    manager.update_inventory(&serial_inventory(&[]), scan_started_before_loss);
    manager.invalidate_inventory();
    let _ = manager.poll();
    manager.update_inventory(&serial_inventory(&[original]), scan_started_before_loss);
    let _ = manager.poll();
    let _stream = peer(&listener);
    connected(&mut manager, "port");
    assert_eq!(manager.session("port").unwrap().generation, 2);
}

#[test]
fn manual_stop_reservation_release_and_shutdown_disarm_pending_recovery() {
    for action in 0..3 {
        let (mut manager, listener) = fixture();
        manager.update_inventory(&serial_inventory(&[usb("port", "serial")]), Instant::now());
        manager.connect("port").unwrap();
        let stream = peer(&listener);
        connected(&mut manager, "port");
        let generation = manager.session("port").unwrap().generation;
        drop(stream);
        let now = Instant::now();
        stopped_at(&mut manager, "port", now);
        assert!(manager.is_reconnecting("port"));
        manager.update_inventory(&serial_inventory(&[]), Instant::now());
        manager.update_inventory(&serial_inventory(&[usb("port", "serial")]), Instant::now());
        match action {
            0 => manager.disconnect("port").unwrap(),
            1 => {
                let token = manager.reserve("port", Some(generation)).unwrap();
                assert!(manager.reservation_ready(&token).unwrap());
                manager.release(&token).unwrap();
            }
            _ => {
                assert!(manager.shutdown().is_none());
            }
        }
        manager.update_inventory(&serial_inventory(&[]), Instant::now());
        manager.update_inventory(&serial_inventory(&[usb("port", "serial")]), Instant::now());
        let _ = manager.poll_at(now);
        assert!(!manager.is_reconnecting("port"));
        assert!(manager.entries[&port_key("port")].worker.is_none());
        assert_eq!(manager.session("port").unwrap().generation, generation);
        if action == 2 {
            assert!(manager.connect("port").is_err());
        }
    }
}

#[test]
fn cancelled_or_replaced_inflight_recovery_never_publishes_connected() {
    for action in 0..5 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (ready_tx, ready_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release = Mutex::new(release_rx);
        let opens = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&opens);
        let mut manager = SessionManager::with_factory(Arc::new(move |_| {
            let stream = socket_transport(address)?;
            if count.fetch_add(1, Ordering::SeqCst) != 0 {
                ready_tx.send(()).unwrap();
                release.lock().recv_timeout(Duration::from_secs(3)).unwrap();
            }
            Ok(Box::new(stream))
        }));
        manager.update_inventory(&serial_inventory(&[usb("port", "serial")]), Instant::now());
        manager.connect("port").unwrap();
        let stream = peer(&listener);
        connected(&mut manager, "port");
        drop(stream);
        let now = Instant::now();
        stopped_at(&mut manager, "port", now);
        let due = now + Duration::from_secs(1);
        manager.update_inventory(&serial_inventory(&[]), Instant::now());
        manager.update_inventory(&serial_inventory(&[usb("port", "serial")]), Instant::now());
        let _ = manager.poll_at(due);
        let mut stream = peer(&listener);
        ready_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(manager.is_reconnecting("port"));
        let mut reaper = None;
        match action {
            0 => manager.disconnect("port").unwrap(),
            1 => {
                let token = manager.reserve("port", None).unwrap();
                manager.release(&token).unwrap();
            }
            2 => reaper = manager.shutdown(),
            3 => {
                manager.update_inventory(
                    &serial_inventory(&[usb("port", "replacement")]),
                    Instant::now(),
                );
                manager
                    .update_inventory(&serial_inventory(&[usb("port", "serial")]), Instant::now());
            }
            _ => manager.invalidate_inventory(),
        }
        release_tx.send(()).unwrap();
        if action == 4 {
            // Failed inventory refresh does not itself cancel the blocked
            // factory. It can return, send Opened and observe a whole scan
            // before lifecycle poll rejects the stale automatic recovery.
            stream
                .write_all(
                    b"RSSI scan (ESB channels 0..88): samples/ch/sweep=50, sweeps=200 (total=10000 samples/ch)\r\n\
Current channel (effective): 84\r\n\
Recommended: channel 86 (-69 dBm)\r\n\
All channels sorted by RSSI (lower is better):\r\n\
 86 (-69 dBm)\r\n",
                )
                .unwrap();
            let latest = Arc::clone(
                &manager.entries[&port_key("port")]
                    .worker
                    .as_ref()
                    .unwrap()
                    .rssi_scan,
            );
            // No manager poll: prove the real worker has already parsed data,
            // while the main-thread view has not admitted its Opened event.
            until(|| latest.lock().is_some());
            assert_eq!(
                manager.session("port").unwrap().state,
                ConnectionState::Connecting
            );
            assert!(manager.take_rssi_scans().is_empty());
            assert!(latest.lock().is_none());
        }
        if let Some(reaper) = reaper {
            reaper.join().unwrap();
        }
        stopped_at(&mut manager, "port", due);
        assert_ne!(
            manager.session("port").unwrap().state,
            ConnectionState::Connected
        );
        let _ = manager.poll_at(due + Duration::from_secs(100));
        manager.update_inventory(&serial_inventory(&[usb("port", "serial")]), Instant::now());
        let _ = manager.poll_at(due);
        assert_eq!(opens.load(Ordering::SeqCst), 2);
        assert_eq!(manager.is_reconnecting("port"), action >= 3);
        assert_eq!(stream.read(&mut [0; 1]).unwrap(), 0);
    }
}

#[test]
fn new_arrival_while_cancelled_open_is_closing_waits_for_join_then_opens_once() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (ready_tx, ready_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release = Mutex::new(release_rx);
    let opens = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&opens);
    let mut manager = SessionManager::with_factory(Arc::new(move |_| {
        let stream = socket_transport(address)?;
        if count.fetch_add(1, Ordering::SeqCst) == 1 {
            ready_tx.send(()).unwrap();
            release.lock().recv_timeout(Duration::from_secs(3)).unwrap();
        }
        Ok(Box::new(stream))
    }));
    manager.update_inventory(&serial_inventory(&[usb("port", "serial")]), Instant::now());
    manager.connect("port").unwrap();
    let stream = peer(&listener);
    connected(&mut manager, "port");
    drop(stream);
    stopped(&mut manager, "port");
    manager.update_inventory(&serial_inventory(&[]), Instant::now());
    manager.update_inventory(&serial_inventory(&[usb("port", "serial")]), Instant::now());
    let _ = manager.poll();
    let mut cancelled_peer = peer(&listener);
    ready_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    manager.update_inventory(&serial_inventory(&[]), Instant::now());
    assert_eq!(
        manager.session("port").unwrap().state,
        ConnectionState::Closing
    );
    manager.update_inventory(&serial_inventory(&[usb("port", "serial")]), Instant::now());
    for _ in 0..10 {
        let _ = manager.poll();
        assert_eq!(manager.session("port").unwrap().generation, 2);
        assert_eq!(opens.load(Ordering::SeqCst), 2);
    }
    release_tx.send(()).unwrap();
    until(|| {
        let _ = manager.poll();
        manager.session("port").unwrap().generation == 3
    });
    assert_eq!(cancelled_peer.read(&mut [0; 1]).unwrap(), 0);
    let _stream = peer(&listener);
    connected(&mut manager, "port");
    assert_eq!(opens.load(Ordering::SeqCst), 3);
    assert!(!manager.is_reconnecting("port"));
}

#[test]
fn arrival_before_old_worker_finishes_preserves_transition_without_command_replay() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (ready_tx, ready_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release = Mutex::new(Some(release_rx));
    let calls = Arc::new(AtomicUsize::new(0));
    let writes = Arc::clone(&calls);
    let mut manager = SessionManager::with_factory(Arc::new(move |_| {
        let stream = socket_transport(address)?;
        if let Some(release) = release.lock().take() {
            Ok(Box::new(PartialErrorWriter {
                stream,
                ready: ready_tx.clone(),
                release,
                calls: Arc::clone(&writes),
                confirmed_prefix: 2,
                unreported_prefix: 1,
                error_kind: io::ErrorKind::TimedOut,
                failed: false,
            }) as Box<dyn Transport>)
        } else {
            Ok(Box::new(stream) as Box<dyn Transport>)
        }
    }));
    manager.update_inventory(&serial_inventory(&[usb("port", "serial")]), Instant::now());
    manager.connect("port").unwrap();
    let mut old = peer(&listener);
    connected(&mut manager, "port");
    let generation = manager.session("port").unwrap().generation;
    let mut active = manager.send_line("port", "save").unwrap();
    ready_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    let mut queued = manager.send_line("port", "queued").unwrap();
    // The first worker is still blocked inside its failing write. Retain
    // the inventory event without opening a competing owner.
    manager.update_inventory(&serial_inventory(&[]), Instant::now());
    manager.update_inventory(&serial_inventory(&[usb("port", "serial")]), Instant::now());
    let _ = manager.poll();
    assert_eq!(manager.session("port").unwrap().generation, generation);
    release_tx.send(()).unwrap();
    let now = Instant::now();
    until(|| {
        let _ = manager.poll_at(now);
        manager.session("port").unwrap().generation > generation
    });
    assert!(completed(&mut active).is_err());
    assert!(completed(&mut queued).is_err());
    assert!(manager.writes_idle("port"));
    let mut bytes = Vec::new();
    old.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"sav");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let mut new = peer(&listener);
    connected_at(&mut manager, "port", now);
    assert!(manager.session("port").unwrap().generation > generation);
    let mut fresh = manager.send_line("port", "info").unwrap();
    assert!(completed(&mut fresh).is_ok());
    manager.disconnect("port").unwrap();
    stopped_at(&mut manager, "port", now);
    let mut bytes = Vec::new();
    new.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"info\n");
    let _ = manager.poll_at(now + Duration::from_secs(100));
    assert!(!manager.is_reconnecting("port"));
}

#[test]
fn arrival_requires_exact_usb_serial_without_case_or_whitespace_normalization() {
    for replacement in ["SERIAL", " serial", "serial "] {
        let (mut manager, listener) = fixture();
        let scan_started_before_loss = Instant::now();
        manager.update_inventory(
            &serial_inventory(&[usb("port", "serial")]),
            scan_started_before_loss,
        );
        manager.connect("port").unwrap();
        let stream = peer(&listener);
        connected(&mut manager, "port");
        drop(stream);
        stopped(&mut manager, "port");
        manager.update_inventory(&serial_inventory(&[]), scan_started_before_loss);
        manager.update_inventory(
            &serial_inventory(&[usb("port", replacement)]),
            scan_started_before_loss,
        );
        let _ = manager.poll();
        assert!(manager.entries[&port_key("port")].worker.is_none());
        manager.update_inventory(
            &serial_inventory(&[usb("port", "serial")]),
            scan_started_before_loss,
        );
        let _ = manager.poll();
        assert!(manager.entries[&port_key("port")].worker.is_none());
        assert_eq!(manager.session("port").unwrap().generation, 1);
    }
}

#[cfg(windows)]
#[test]
fn windows_port_aliases_share_single_ownership_key() {
    assert_eq!(port_key("com12"), port_key(r"\\.\COM12"));
    assert_eq!(port_key("COM12"), port_key(r"\\?\com12"));
    let (mut manager, listener) = fixture();
    let scan_started_before_loss = Instant::now();
    manager.update_inventory(
        &serial_inventory(&[usb("COM12", "serial")]),
        scan_started_before_loss,
    );
    manager.connect("COM12").unwrap();
    let stream = peer(&listener);
    connected(&mut manager, "COM12");
    drop(stream);
    stopped(&mut manager, "COM12");
    manager.update_inventory(
        &serial_inventory(&[usb(r"\\.\com12", "serial")]),
        scan_started_before_loss,
    );
    let _ = manager.poll();
    assert!(manager.entries[&port_key("COM12")].worker.is_none());
    manager.update_inventory(&serial_inventory(&[]), scan_started_before_loss);
    manager.update_inventory(
        &serial_inventory(&[usb(r"\\?\com12", "serial")]),
        scan_started_before_loss,
    );
    let _ = manager.poll();
    let _stream = peer(&listener);
    connected(&mut manager, "COM12");
    assert_eq!(manager.session("COM12").unwrap().generation, 2);
}

fn receiver_inventory(ports: Vec<SerialEndpoint>) -> Inventory {
    Inventory {
        ports,
        receivers: vec![crate::domain::ReceiverEndpoint {
            key: "synthetic-receiver".into(),
            path: b"synthetic-receiver-path".to_vec(),
            label: "Receiver".into(),
            serial_number: Some("serial".into()),
        }],
        ..Inventory::default()
    }
}

fn receiver_function(port: &str, number: Option<u8>, raw: bool) -> SerialEndpoint {
    SerialEndpoint {
        pid: Some(0x7690),
        usb_interface: Some(crate::domain::UsbInterface {
            number,
            description: if raw {
                crate::domain::InterfaceDescription::Value("SlimeNRF Raw Data CDC".into())
            } else {
                crate::domain::InterfaceDescription::Unavailable
            },
        }),
        ..usb(port, "serial")
    }
}

#[test]
fn sampled_console_recovery_requires_matching_interface_rearrival() {
    let (mut manager, listener) = fixture();
    let console = receiver_function("port", Some(0), false);
    let data = receiver_function("raw", Some(2), true);
    let inventory = receiver_inventory(vec![data.clone(), console.clone()]);
    let scan_started_before_loss = Instant::now();
    manager.update_inventory(&inventory, scan_started_before_loss);
    assert!(manager.connect("raw").is_err());
    manager.connect("port").unwrap();
    let stream = peer(&listener);
    connected(&mut manager, "port");
    drop(stream);
    stopped(&mut manager, "port");
    assert!(manager.is_reconnecting("port"));
    manager.update_inventory(&inventory, scan_started_before_loss);
    let _ = manager.poll();
    assert!(manager.entries[&port_key("port")].worker.is_none());
    manager.update_inventory(
        &receiver_inventory(vec![data.clone()]),
        scan_started_before_loss,
    );
    manager.update_inventory(
        &receiver_inventory(vec![
            receiver_function("port", Some(4), false),
            data.clone(),
        ]),
        scan_started_before_loss,
    );
    let _ = manager.poll();
    manager.update_inventory(&inventory, scan_started_before_loss);
    let _ = manager.poll();
    assert!(manager.entries[&port_key("port")].worker.is_none());
    manager.update_inventory(&receiver_inventory(vec![data]), scan_started_before_loss);
    manager.update_inventory(&inventory, scan_started_before_loss);
    let _ = manager.poll();
    let _stream = peer(&listener);
    connected(&mut manager, "port");
    assert_eq!(manager.session("port").unwrap().generation, 2);
}

#[test]
fn distinct_roles_do_not_disambiguate_duplicate_or_missing_interface_numbers() {
    for (console_number, raw_number) in [
        (Some(0), Some(0)),
        (None, Some(2)),
        (Some(0), None),
        (None, None),
    ] {
        let mut manager = SessionManager::new();
        let inventory = receiver_inventory(vec![
            receiver_function("port", console_number, false),
            receiver_function("raw", raw_number, true),
        ]);
        manager.update_inventory(&inventory, Instant::now());
        assert!(
            manager.inventory[&port_key("port")]
                .recovery_identity
                .is_none()
        );
        assert!(
            manager.inventory[&port_key("raw")]
                .recovery_identity
                .is_none()
        );
        assert!(manager.connect("raw").is_err());
        if console_number == raw_number && console_number.is_some() {
            assert!(manager.connect("port").is_err());
        }
    }
}

#[test]
fn unknown_sibling_revokes_contextual_console_without_inventing_arrival() {
    let (mut manager, listener) = fixture();
    let console = receiver_function("port", Some(0), false);
    let data = receiver_function("raw", Some(2), true);
    let inventory = receiver_inventory(vec![console.clone(), data.clone()]);
    let scan_started_before_loss = Instant::now();
    manager.update_inventory(&inventory, scan_started_before_loss);
    manager.connect("port").unwrap();
    let stream = peer(&listener);
    connected(&mut manager, "port");
    drop(stream);
    stopped(&mut manager, "port");
    manager.update_inventory(
        &receiver_inventory(vec![
            console,
            data.clone(),
            receiver_function("unknown", Some(4), false),
        ]),
        scan_started_before_loss,
    );
    assert!(manager.connect("port").is_err());
    manager.update_inventory(&inventory, scan_started_before_loss);
    let _ = manager.poll();
    assert!(manager.entries[&port_key("port")].worker.is_none());
    assert_eq!(manager.session("port").unwrap().generation, 1);
    manager.update_inventory(&receiver_inventory(vec![data]), scan_started_before_loss);
    manager.update_inventory(&inventory, scan_started_before_loss);
    let _ = manager.poll();
    let _stream = peer(&listener);
    connected(&mut manager, "port");
    assert_eq!(manager.session("port").unwrap().generation, 2);
}

#[test]
fn known_raw_remains_denied_when_inventory_becomes_stale_or_hid_is_absent() {
    let opens = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&opens);
    let mut manager = SessionManager::with_factory(Arc::new(move |_| {
        count.fetch_add(1, Ordering::SeqCst);
        Err(io::Error::other("must not open raw interface"))
    }));
    manager.update_inventory(
        &serial_inventory(&[receiver_function("raw", Some(2), true)]),
        Instant::now(),
    );
    assert!(manager.connect("raw").is_err());
    manager.invalidate_inventory();
    assert!(manager.connect("raw").is_err());
    assert_eq!(opens.load(Ordering::SeqCst), 0);
}

#[test]
fn contextual_console_revocation_cancels_inflight_manual_open() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (ready_tx, ready_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release = Mutex::new(release_rx);
    let mut manager = SessionManager::with_factory(Arc::new(move |_| {
        let stream = socket_transport(address)?;
        ready_tx.send(()).unwrap();
        release.lock().recv_timeout(Duration::from_secs(3)).unwrap();
        Ok(Box::new(stream))
    }));
    let console = receiver_function("port", Some(0), false);
    let data = receiver_function("raw", Some(2), true);
    manager.update_inventory(
        &receiver_inventory(vec![console.clone(), data.clone()]),
        Instant::now(),
    );
    manager.connect("port").unwrap();
    let mut stream = peer(&listener);
    ready_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    manager.update_inventory(
        &receiver_inventory(vec![
            console.clone(),
            data.clone(),
            receiver_function("unknown", Some(4), false),
        ]),
        Instant::now(),
    );
    assert_eq!(
        manager.session("port").unwrap().state,
        ConnectionState::Closing
    );
    manager.update_inventory(&receiver_inventory(vec![console, data]), Instant::now());
    release_tx.send(()).unwrap();
    stopped(&mut manager, "port");
    assert_ne!(
        manager.session("port").unwrap().state,
        ConnectionState::Connected
    );
    assert!(!manager.is_reconnecting("port"));
    assert_eq!(stream.read(&mut [0; 1]).unwrap(), 0);
}
