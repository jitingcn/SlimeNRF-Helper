use super::super::native::Transport;
use super::super::{ConnectionState, SessionManager, port_key};
use super::until;
use crate::domain::{
    InterfaceDescription, Inventory, ReceiverEndpoint, SerialEndpoint, SerialPortKind, UsbInterface,
};
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

mod guards;

struct ControlledTransport {
    entered: Option<mpsc::Sender<()>>,
    fail: mpsc::Receiver<()>,
    dropped: mpsc::Sender<()>,
}

impl Read for ControlledTransport {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        if let Some(entered) = self.entered.take() {
            entered.send(()).unwrap();
        }
        match self.fail.recv_timeout(Duration::from_millis(2)) {
            Ok(()) => Err(io::Error::from_raw_os_error(22)),
            Err(_) => Err(io::ErrorKind::TimedOut.into()),
        }
    }
}

impl Write for ControlledTransport {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for ControlledTransport {
    fn drop(&mut self) {
        let _ = self.dropped.send(());
    }
}

struct ReadControl {
    entered: mpsc::Receiver<()>,
    fail: mpsc::Sender<()>,
    dropped: mpsc::Receiver<()>,
}

impl ReadControl {
    fn entered(&self) {
        self.entered.recv_timeout(Duration::from_secs(3)).unwrap();
    }

    fn fail(&self) {
        self.fail.send(()).unwrap();
        self.dropped.recv_timeout(Duration::from_secs(3)).unwrap();
    }
}

fn transport() -> (io::Result<Box<dyn Transport>>, ReadControl) {
    let (entered, entered_rx) = mpsc::channel();
    let (fail_tx, fail) = mpsc::channel();
    let (dropped, dropped_rx) = mpsc::channel();
    (
        Ok(Box::new(ControlledTransport {
            entered: Some(entered),
            fail,
            dropped,
        })),
        ReadControl {
            entered: entered_rx,
            fail: fail_tx,
            dropped: dropped_rx,
        },
    )
}

fn manager_with(
    attempts: Vec<io::Result<Box<dyn Transport>>>,
) -> (SessionManager, Arc<AtomicUsize>) {
    let attempts = Mutex::new(VecDeque::from(attempts));
    let opens = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&opens);
    let manager = SessionManager::with_factory(Arc::new(move |port| {
        assert_eq!(port, "COM41");
        count.fetch_add(1, Ordering::SeqCst);
        attempts
            .lock()
            .pop_front()
            .unwrap_or_else(|| Err(io::Error::other("unplanned automatic attempt")))
    }));
    (manager, opens)
}

fn endpoint(port: &str, serial: &str) -> SerialEndpoint {
    SerialEndpoint {
        port_name: port.into(),
        label: port.into(),
        manufacturer: None,
        product: None,
        kind: SerialPortKind::Usb,
        serial_number: Some(serial.into()),
        vid: Some(0x1209),
        pid: Some(0x7692),
        usb_interface: None,
    }
}

fn inventory() -> Inventory {
    Inventory {
        ports: vec![endpoint("COM41", "synthetic-original")],
        ..Inventory::default()
    }
}

fn receiver_inventory(hid_path: &str) -> Inventory {
    let mut console = endpoint("COM41", "synthetic-original");
    console.pid = Some(0x7690);
    console.usb_interface = Some(UsbInterface {
        number: Some(0),
        description: InterfaceDescription::Unavailable,
    });
    let mut raw = console.clone();
    raw.port_name = "COM42".into();
    raw.usb_interface = Some(UsbInterface {
        number: Some(2),
        description: InterfaceDescription::Value("SlimeNRF Raw Data CDC".into()),
    });
    Inventory {
        ports: vec![console, raw],
        receivers: vec![ReceiverEndpoint {
            key: hid_path.into(),
            path: hid_path.as_bytes().to_vec(),
            label: "Synthetic receiver".into(),
            serial_number: Some("synthetic-original".into()),
        }],
        ..Inventory::default()
    }
}

fn apply(manager: &mut SessionManager, snapshot: &Inventory, started: Instant, applied: Instant) {
    manager.update_inventory_at(snapshot, started, applied);
}

fn connected_at(manager: &mut SessionManager, control: &ReadControl, now: Instant) {
    control.entered();
    until(|| {
        let _ = manager.poll_at(now);
        manager.session("COM41").unwrap().state == ConnectionState::Connected
    });
}

fn lost_at(manager: &mut SessionManager, now: Instant) -> usize {
    let mut refreshes = 0;
    until(|| {
        refreshes += usize::from(manager.poll_at(now).refresh_inventory);
        manager.entries[&port_key("COM41")].worker.is_none()
    });
    assert_eq!(
        manager.session("COM41").unwrap().state,
        ConnectionState::Faulted
    );
    assert!(
        manager
            .session("COM41")
            .unwrap()
            .last_error
            .as_ref()
            .unwrap()
            .contains("22")
    );
    refreshes
}

fn recovered_at(
    manager: &mut SessionManager,
    control: &ReadControl,
    now: Instant,
    generation: u64,
) -> usize {
    let mut refreshes = 0;
    until(|| {
        refreshes += usize::from(manager.poll_at(now).refresh_inventory);
        manager.session("COM41").unwrap().generation == generation
    });
    connected_at(manager, control, now);
    refreshes
}

#[test]
fn unchanged_com_with_new_current_hid_association_recovers_after_loss_scan() {
    let (first, old) = transport();
    let (second, new) = transport();
    let (mut manager, opens) = manager_with(vec![first, second]);
    let now = Instant::now();
    let original = receiver_inventory("synthetic-old-hid-path");
    let moved = receiver_inventory("synthetic-new-hid-path");
    apply(&mut manager, &original, now, now);
    manager.connect("COM41").unwrap();
    connected_at(&mut manager, &old, now);
    old.fail();
    let lost = now + Duration::from_secs(1);
    assert_eq!(lost_at(&mut manager, lost), 1);
    assert_eq!(opens.load(Ordering::SeqCst), 1);
    let fresh = now + Duration::from_secs(2);
    apply(&mut manager, &moved, fresh, fresh);
    assert_eq!(recovered_at(&mut manager, &new, fresh, 2), 0);
    assert_eq!(opens.load(Ordering::SeqCst), 2);
    assert!(!manager.is_reconnecting("COM41"));
    assert_eq!(
        manager.session("COM41").unwrap().state,
        ConnectionState::Connected
    );
}

#[test]
fn native_hint_and_matching_scan_before_old_read22_survive_the_later_loss_floor() {
    let (first, old) = transport();
    let (second, new) = transport();
    let (mut manager, opens) = manager_with(vec![first, second]);
    let now = Instant::now();
    let snapshot = inventory();
    apply(&mut manager, &snapshot, now, now);
    manager.connect("COM41").unwrap();
    connected_at(&mut manager, &old, now);
    let hint = now + Duration::from_secs(1);
    manager.note_native_change(hint);
    let scanned = now + Duration::from_secs(2);
    apply(&mut manager, &snapshot, scanned, scanned);
    let _ = manager.poll_at(scanned);
    assert_eq!(opens.load(Ordering::SeqCst), 1);
    assert_eq!(manager.session("COM41").unwrap().generation, 1);
    old.fail();
    let lost = now + Duration::from_secs(3);
    assert_eq!(recovered_at(&mut manager, &new, lost, 2), 1);
    assert_eq!(opens.load(Ordering::SeqCst), 2);
    assert_eq!(
        manager.session("COM41").unwrap().state,
        ConnectionState::Connected
    );
}

#[test]
fn pre_loss_and_pre_signal_scan_completing_late_cannot_authorize_recovery() {
    for native_hint in [false, true] {
        let (first, old) = transport();
        let (second, new) = transport();
        let (mut manager, opens) = manager_with(vec![first, second]);
        let now = Instant::now();
        let snapshot = inventory();
        apply(&mut manager, &snapshot, now, now);
        manager.connect("COM41").unwrap();
        connected_at(&mut manager, &old, now);
        let old_scan = now + Duration::from_secs(1);
        if native_hint {
            manager.note_native_change(now + Duration::from_secs(2));
        }
        old.fail();
        let lost = now + Duration::from_secs(3);
        assert_eq!(lost_at(&mut manager, lost), 1);
        let completed = now + Duration::from_secs(4);
        apply(&mut manager, &snapshot, old_scan, completed);
        let _ = manager.poll_at(completed);
        assert!(manager.inventory_is_fresh(completed));
        assert_eq!(opens.load(Ordering::SeqCst), 1);
        assert_eq!(manager.session("COM41").unwrap().generation, 1);
        assert_eq!(
            manager.session("COM41").unwrap().state,
            ConnectionState::Faulted
        );
        let fresh = now + Duration::from_secs(5);
        apply(&mut manager, &snapshot, fresh, fresh);
        recovered_at(&mut manager, &new, fresh, 2);
        assert_eq!(opens.load(Ordering::SeqCst), 2);
    }
}

#[test]
fn failed_absent_and_ambiguous_scans_retain_each_recovery_source_until_valid_evidence() {
    for native_only in [false, true] {
        let (first, old) = transport();
        let (second, automatic) = transport();
        let (third, new) = transport();
        let attempts = if native_only {
            vec![first, second, third]
        } else {
            vec![first, third]
        };
        let (mut manager, opens) = manager_with(attempts);
        let now = Instant::now();
        let snapshot = inventory();
        apply(&mut manager, &snapshot, now, now);
        manager.connect("COM41").unwrap();
        connected_at(&mut manager, &old, now);
        old.fail();
        assert_eq!(lost_at(&mut manager, now + Duration::from_secs(1)), 1);
        let prior_opens = if native_only {
            let fresh = now + Duration::from_secs(2);
            apply(&mut manager, &snapshot, fresh, fresh);
            recovered_at(&mut manager, &automatic, fresh, 2);
            automatic.fail();
            assert_eq!(lost_at(&mut manager, now + Duration::from_secs(3)), 0);
            manager.note_native_change(now + Duration::from_secs(4));
            2
        } else {
            1
        };
        let mut failed = snapshot.clone();
        failed.serial_enumeration_failed = true;
        let mut duplicate_port = snapshot.clone();
        duplicate_port.ports.push(endpoint("COM41", "replacement"));
        let mut duplicate_identity = snapshot.clone();
        duplicate_identity
            .ports
            .push(endpoint("COM43", "synthetic-original"));
        manager.invalidate_inventory();
        let _ = manager.poll_at(now + Duration::from_secs(5));
        assert!(!manager.inventory_is_fresh(now + Duration::from_secs(5)));
        for (index, invalid) in [
            failed,
            Inventory::default(),
            duplicate_port,
            duplicate_identity,
        ]
        .into_iter()
        .enumerate()
        {
            let time = now + Duration::from_secs(6 + index as u64);
            apply(&mut manager, &invalid, time, time);
            let _ = manager.poll_at(time);
            assert_eq!(opens.load(Ordering::SeqCst), prior_opens);
            assert_eq!(
                manager.session("COM41").unwrap().state,
                ConnectionState::Faulted
            );
        }
        let fresh = now + Duration::from_secs(10);
        apply(&mut manager, &snapshot, fresh, fresh);
        recovered_at(&mut manager, &new, fresh, prior_opens as u64 + 1);
        assert_eq!(opens.load(Ordering::SeqCst), prior_opens + 1);
        assert_eq!(
            manager.session("COM41").unwrap().state,
            ConnectionState::Connected
        );
    }
}

#[test]
fn automatic_opened_then_read22_stays_spent_until_one_later_native_hint_and_scan() {
    let (first, old) = transport();
    let (second, automatic) = transport();
    let (third, later_native) = transport();
    let (mut manager, opens) = manager_with(vec![first, second, third]);
    let now = Instant::now();
    let snapshot = inventory();
    apply(&mut manager, &snapshot, now, now);
    manager.connect("COM41").unwrap();
    connected_at(&mut manager, &old, now);
    old.fail();
    assert_eq!(lost_at(&mut manager, now + Duration::from_secs(1)), 1);
    let fresh = now + Duration::from_secs(2);
    apply(&mut manager, &snapshot, fresh, fresh);
    until(|| {
        let _ = manager.poll_at(fresh);
        manager.session("COM41").unwrap().generation == 2
    });
    automatic.entered();
    assert_eq!(
        manager.session("COM41").unwrap().state,
        ConnectionState::Connecting
    );
    automatic.fail();
    until(|| {
        manager.entries[&port_key("COM41")]
            .worker
            .as_ref()
            .unwrap()
            .handle
            .as_ref()
            .unwrap()
            .is_finished()
    });
    assert_eq!(lost_at(&mut manager, now + Duration::from_secs(3)), 0);
    for seconds in [4, 5, 20, 100] {
        let time = now + Duration::from_secs(seconds);
        apply(&mut manager, &snapshot, time, time);
        let changes = manager.poll_at(time);
        assert!(!changes.refresh_inventory);
        assert_eq!(opens.load(Ordering::SeqCst), 2);
        assert_eq!(manager.session("COM41").unwrap().generation, 2);
        assert_eq!(
            manager.session("COM41").unwrap().state,
            ConnectionState::Faulted
        );
    }
    let old_scan = now + Duration::from_secs(101);
    manager.note_native_change(now + Duration::from_secs(102));
    let completed = now + Duration::from_secs(103);
    apply(&mut manager, &snapshot, old_scan, completed);
    let _ = manager.poll_at(completed);
    assert_eq!(opens.load(Ordering::SeqCst), 2);
    assert_eq!(
        manager.session("COM41").unwrap().state,
        ConnectionState::Faulted
    );
    let fresh = now + Duration::from_secs(104);
    apply(&mut manager, &snapshot, fresh, fresh);
    recovered_at(&mut manager, &later_native, fresh, 3);
    assert_eq!(opens.load(Ordering::SeqCst), 3);
    later_native.fail();
    assert_eq!(lost_at(&mut manager, now + Duration::from_secs(105)), 0);
    for seconds in [106, 107, 1_000] {
        let time = now + Duration::from_secs(seconds);
        apply(&mut manager, &snapshot, time, time);
        assert!(!manager.poll_at(time).refresh_inventory);
        assert_eq!(opens.load(Ordering::SeqCst), 3);
        assert_eq!(
            manager.session("COM41").unwrap().state,
            ConnectionState::Faulted
        );
    }
}

#[test]
fn native_loss_and_sampled_arrival_coalesce_without_refilling_after_opened() {
    let (first, old) = transport();
    let (second, automatic) = transport();
    let (mut manager, opens) = manager_with(vec![first, second]);
    let now = Instant::now();
    let snapshot = inventory();
    apply(&mut manager, &snapshot, now, now);
    manager.connect("COM41").unwrap();
    connected_at(&mut manager, &old, now);
    let (release_join, join_gate) = mpsc::channel();
    let worker = manager
        .entries
        .get_mut(&port_key("COM41"))
        .unwrap()
        .worker
        .as_mut()
        .unwrap();
    let predecessor = worker.handle.take().unwrap();
    worker.handle = Some(std::thread::spawn(move || {
        predecessor.join().unwrap();
        join_gate.recv_timeout(Duration::from_secs(3)).unwrap();
    }));
    old.fail();
    let lost = now + Duration::from_secs(1);
    let mut refreshes = 0;
    until(|| {
        refreshes += usize::from(manager.poll_at(lost).refresh_inventory);
        manager.session("COM41").unwrap().state == ConnectionState::Faulted
    });
    assert_eq!(refreshes, 1);
    let hint = now + Duration::from_secs(2);
    manager.note_native_change(hint);
    apply(&mut manager, &Inventory::default(), hint, hint);
    let arrived = now + Duration::from_secs(3);
    apply(&mut manager, &snapshot, arrived, arrived);
    for _ in 0..3 {
        assert!(!manager.poll_at(arrived).refresh_inventory);
        assert_eq!(opens.load(Ordering::SeqCst), 1);
        assert_eq!(manager.session("COM41").unwrap().generation, 1);
    }
    release_join.send(()).unwrap();
    assert_eq!(recovered_at(&mut manager, &automatic, arrived, 2), 0);
    assert_eq!(opens.load(Ordering::SeqCst), 2);
    automatic.fail();
    assert_eq!(lost_at(&mut manager, now + Duration::from_secs(4)), 0);
    let fresh = now + Duration::from_secs(5);
    apply(&mut manager, &snapshot, fresh, fresh);
    let changes = manager.poll_at(fresh);
    assert!(!changes.refresh_inventory);
    assert_eq!(opens.load(Ordering::SeqCst), 2);
    assert_eq!(manager.session("COM41").unwrap().generation, 2);
    assert_eq!(
        manager.session("COM41").unwrap().state,
        ConnectionState::Faulted
    );
}

#[test]
fn unrelated_global_com_hint_can_fund_one_later_loss_of_unchanged_exact_console() {
    let (first, old) = transport();
    let (second, automatic) = transport();
    let (third, after_unrelated_hint) = transport();
    let (mut manager, opens) = manager_with(vec![first, second, third]);
    let now = Instant::now();
    let snapshot = inventory();
    apply(&mut manager, &snapshot, now, now);
    manager.connect("COM41").unwrap();
    connected_at(&mut manager, &old, now);
    old.fail();
    assert_eq!(lost_at(&mut manager, now + Duration::from_secs(1)), 1);
    let fresh = now + Duration::from_secs(2);
    apply(&mut manager, &snapshot, fresh, fresh);
    recovered_at(&mut manager, &automatic, fresh, 2);
    let mut unrelated_change = snapshot.clone();
    unrelated_change
        .ports
        .push(endpoint("COM99", "unrelated-device"));
    let hint = now + Duration::from_secs(3);
    manager.note_native_change(hint);
    let scanned = now + Duration::from_secs(4);
    apply(&mut manager, &unrelated_change, scanned, scanned);
    let _ = manager.poll_at(scanned);
    assert_eq!(opens.load(Ordering::SeqCst), 2);
    assert_eq!(
        manager.session("COM41").unwrap().state,
        ConnectionState::Connected
    );
    automatic.fail();
    let lost = now + Duration::from_secs(5);
    assert_eq!(
        recovered_at(&mut manager, &after_unrelated_hint, lost, 3),
        0
    );
    assert_eq!(opens.load(Ordering::SeqCst), 3);
    after_unrelated_hint.fail();
    assert_eq!(lost_at(&mut manager, now + Duration::from_secs(6)), 0);
    let fresh = now + Duration::from_secs(7);
    apply(&mut manager, &unrelated_change, fresh, fresh);
    let _ = manager.poll_at(fresh);
    assert_eq!(opens.load(Ordering::SeqCst), 3);
    assert_eq!(
        manager.session("COM41").unwrap().state,
        ConnectionState::Faulted
    );
}
