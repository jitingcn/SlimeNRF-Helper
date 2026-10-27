use super::{
    ConnectionState, SessionManager, apply, connected_at, inventory, lost_at, manager_with,
    port_key, receiver_inventory, recovered_at, transport, until,
};
use crate::domain::{InterfaceDescription, Inventory};
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

#[test]
fn initial_permission_denied_never_recovers_until_an_admitted_manual_retry() {
    let (manual, manually_connected) = transport();
    let (automatic, automatically_connected) = transport();
    let denied = Err(io::Error::new(
        io::ErrorKind::PermissionDenied,
        "synthetic busy",
    ));
    let (mut manager, opens) = manager_with(vec![denied, manual, automatic]);
    let snapshot = inventory();
    let now = Instant::now();
    apply(&mut manager, &snapshot, now, now);
    manager.connect("COM41").unwrap();
    let mut refreshes = 0;
    until(|| {
        refreshes += usize::from(manager.poll_at(now).refresh_inventory);
        manager.can_forget("COM41")
    });
    assert_eq!(refreshes, 0);
    assert_eq!(opens.load(Ordering::SeqCst), 1);
    let view = manager.session("COM41").unwrap();
    assert_eq!(view.state, ConnectionState::Faulted);
    assert_eq!(view.generation, 1);
    assert_eq!(view.last_error.as_deref(), Some("synthetic busy"));
    assert!(!manager.is_reconnecting("COM41"));

    for seconds in [1, 3, 8] {
        manager.note_native_change(now + Duration::from_secs(seconds));
        let scanned = now + Duration::from_secs(seconds + 1);
        apply(&mut manager, &snapshot, scanned, scanned);
        assert!(!manager.poll_at(scanned).refresh_inventory);
        assert_eq!(opens.load(Ordering::SeqCst), 1);
        let view = manager.session("COM41").unwrap();
        assert_eq!(view.state, ConnectionState::Faulted);
        assert_eq!(view.generation, 1);
        assert_eq!(view.last_error.as_deref(), Some("synthetic busy"));
        assert!(!manager.is_reconnecting("COM41"));
    }

    manager.connect("COM41").unwrap();
    assert_eq!(
        manager.session("COM41").unwrap().state,
        ConnectionState::Connecting
    );
    assert_eq!(manager.session("COM41").unwrap().generation, 2);
    assert!(!manager.is_reconnecting("COM41"));
    connected_at(
        &mut manager,
        &manually_connected,
        now + Duration::from_secs(10),
    );
    assert_eq!(opens.load(Ordering::SeqCst), 2);
    assert_eq!(manager.session("COM41").unwrap().generation, 2);
    assert_eq!(manager.session("COM41").unwrap().last_error, None);

    manually_connected.fail();
    assert_eq!(lost_at(&mut manager, now + Duration::from_secs(11)), 1);
    assert_eq!(opens.load(Ordering::SeqCst), 2);
    assert_eq!(manager.session("COM41").unwrap().generation, 2);
    assert!(manager.is_reconnecting("COM41"));
    let scanned = now + Duration::from_secs(12);
    apply(&mut manager, &snapshot, scanned, scanned);
    assert_eq!(
        recovered_at(&mut manager, &automatically_connected, scanned, 3),
        0
    );
    assert_eq!(opens.load(Ordering::SeqCst), 3);
    assert_eq!(
        manager.session("COM41").unwrap().state,
        ConnectionState::Connected
    );
    assert_eq!(manager.session("COM41").unwrap().generation, 3);
    assert_eq!(manager.session("COM41").unwrap().last_error, None);
    assert!(!manager.is_reconnecting("COM41"));

    automatically_connected.fail();
    assert_eq!(lost_at(&mut manager, now + Duration::from_secs(13)), 0);
    for seconds in [14, 1_000] {
        let scanned = now + Duration::from_secs(seconds);
        apply(&mut manager, &snapshot, scanned, scanned);
        assert!(!manager.poll_at(scanned).refresh_inventory);
        assert_eq!(opens.load(Ordering::SeqCst), 3);
        assert_eq!(
            manager.session("COM41").unwrap().state,
            ConnectionState::Faulted
        );
        assert_eq!(manager.session("COM41").unwrap().generation, 3);
    }
}

fn armed_loss() -> (SessionManager, Arc<AtomicUsize>, Inventory, Instant) {
    let (first, old) = transport();
    let (mut manager, opens) = manager_with(vec![first]);
    let snapshot = inventory();
    let now = Instant::now();
    apply(&mut manager, &snapshot, now, now);
    manager.connect("COM41").unwrap();
    connected_at(&mut manager, &old, now);
    old.fail();
    assert_eq!(lost_at(&mut manager, now + Duration::from_secs(1)), 1);
    manager.note_native_change(now + Duration::from_secs(2));
    assert_eq!(opens.load(Ordering::SeqCst), 1);
    assert_eq!(manager.session("COM41").unwrap().generation, 1);
    assert!(manager.is_reconnecting("COM41"));
    (manager, opens, snapshot, now)
}

fn assert_disarmed_scan(
    manager: &mut SessionManager,
    opens: &AtomicUsize,
    snapshot: &Inventory,
    started: Instant,
    applied: Instant,
    state: ConnectionState,
) {
    apply(manager, snapshot, started, applied);
    assert!(!manager.poll_at(applied).refresh_inventory);
    assert_eq!(opens.load(Ordering::SeqCst), 1);
    assert_eq!(manager.session("COM41").unwrap().state, state);
    assert_eq!(manager.session("COM41").unwrap().generation, 1);
    assert!(!manager.is_reconnecting("COM41"));
}

fn assert_late_scans_stay_disarmed(
    manager: &mut SessionManager,
    opens: &AtomicUsize,
    snapshot: &Inventory,
    now: Instant,
    state: ConnectionState,
) {
    assert_disarmed_scan(
        manager,
        opens,
        snapshot,
        now + Duration::from_secs(3),
        now + Duration::from_secs(4),
        state,
    );
    manager.note_native_change(now + Duration::from_secs(5));
    assert_disarmed_scan(
        manager,
        opens,
        snapshot,
        now + Duration::from_secs(6),
        now + Duration::from_secs(7),
        state,
    );
}

#[test]
fn terminal_close_disarms_loss_and_native_recovery_before_late_scans() {
    let (mut manager, opens, snapshot, now) = armed_loss();
    manager.request_close("COM41").unwrap();
    assert!(!manager.is_reconnecting("COM41"));
    assert_eq!(
        manager.session("COM41").unwrap().state,
        ConnectionState::Disconnected
    );
    assert!(manager.is_close_requested("COM41"));
    assert_late_scans_stay_disarmed(
        &mut manager,
        &opens,
        &snapshot,
        now,
        ConnectionState::Disconnected,
    );
    assert_eq!(
        manager.connect("COM41").unwrap_err(),
        "COM41 terminal is closing"
    );
    assert_eq!(opens.load(Ordering::SeqCst), 1);
    assert_eq!(manager.session("COM41").unwrap().generation, 1);
}

#[test]
fn disconnect_disarms_loss_and_native_recovery_before_late_scans() {
    let (mut manager, opens, snapshot, now) = armed_loss();
    manager.disconnect("COM41").unwrap();
    assert!(!manager.is_reconnecting("COM41"));
    assert_eq!(
        manager.session("COM41").unwrap().state,
        ConnectionState::Disconnected
    );
    assert_late_scans_stay_disarmed(
        &mut manager,
        &opens,
        &snapshot,
        now,
        ConnectionState::Disconnected,
    );
    assert!(manager.can_forget("COM41"));
}

#[test]
fn reservation_and_release_disarm_loss_and_native_recovery_before_late_scans() {
    let (mut manager, opens, snapshot, now) = armed_loss();
    let reservation = manager.reserve("COM41", Some(1)).unwrap();
    assert!(!manager.is_reconnecting("COM41"));
    assert!(manager.reservation_ready(&reservation).unwrap());
    assert_eq!(
        manager.session("COM41").unwrap().state,
        ConnectionState::Faulted
    );
    assert_eq!(
        manager.connect("COM41").unwrap_err(),
        "COM41 is reserved for a firmware operation"
    );
    assert_disarmed_scan(
        &mut manager,
        &opens,
        &snapshot,
        now + Duration::from_secs(3),
        now + Duration::from_secs(4),
        ConnectionState::Faulted,
    );
    manager.release(&reservation).unwrap();
    assert!(!manager.is_reconnecting("COM41"));
    assert!(manager.can_forget("COM41"));
    assert!(
        !manager
            .poll_at(now + Duration::from_secs(4))
            .refresh_inventory
    );
    assert_eq!(opens.load(Ordering::SeqCst), 1);
    assert_eq!(manager.session("COM41").unwrap().generation, 1);
    assert_eq!(
        manager.session("COM41").unwrap().state,
        ConnectionState::Faulted
    );
    manager.note_native_change(now + Duration::from_secs(5));
    assert_disarmed_scan(
        &mut manager,
        &opens,
        &snapshot,
        now + Duration::from_secs(6),
        now + Duration::from_secs(7),
        ConnectionState::Faulted,
    );
}

#[test]
fn shutdown_disarms_loss_and_native_recovery_and_rejects_manual_reconnect() {
    let (mut manager, opens, snapshot, now) = armed_loss();
    assert!(manager.shutdown().is_none());
    assert!(!manager.is_reconnecting("COM41"));
    assert_eq!(
        manager.session("COM41").unwrap().state,
        ConnectionState::Faulted
    );
    assert_late_scans_stay_disarmed(
        &mut manager,
        &opens,
        &snapshot,
        now,
        ConnectionState::Faulted,
    );
    assert!(manager.connect("COM41").is_err());
    assert_eq!(opens.load(Ordering::SeqCst), 1);
    assert_eq!(manager.session("COM41").unwrap().generation, 1);
    assert_eq!(
        manager.session("COM41").unwrap().state,
        ConnectionState::Faulted
    );
    assert!(!manager.is_reconnecting("COM41"));
}

fn assert_identity_guards_then_bounded_busy_recovery(invalid: Vec<(&str, Inventory)>) {
    let (first, old) = transport();
    let denied = Err(io::Error::new(
        io::ErrorKind::PermissionDenied,
        "synthetic busy",
    ));
    let (third, recovered) = transport();
    let (mut manager, opens) = manager_with(vec![first, denied, third]);
    let snapshot = receiver_inventory("synthetic-original-hid");
    let now = Instant::now();
    apply(&mut manager, &snapshot, now, now);
    manager.connect("COM41").unwrap();
    connected_at(&mut manager, &old, now);
    manager.note_native_change(now + Duration::from_secs(1));
    old.fail();
    assert_eq!(lost_at(&mut manager, now + Duration::from_secs(2)), 1);
    assert!(manager.is_reconnecting("COM41"));

    let valid_at = now + Duration::from_secs(3 + invalid.len() as u64);
    for (index, (case, replacement)) in invalid.into_iter().enumerate() {
        let scanned = now + Duration::from_secs(3 + index as u64);
        apply(&mut manager, &replacement, scanned, scanned);
        assert!(!manager.poll_at(scanned).refresh_inventory, "{case}");
        assert_eq!(opens.load(Ordering::SeqCst), 1, "{case}");
        let view = manager.session("COM41").unwrap();
        assert_eq!(view.state, ConnectionState::Faulted, "{case}");
        assert_eq!(view.generation, 1, "{case}");
        assert!(view.last_error.as_ref().unwrap().contains("22"), "{case}");
        assert!(manager.is_reconnecting("COM41"), "{case}");
    }

    apply(&mut manager, &snapshot, valid_at, valid_at);
    let mut refreshes = 0;
    until(|| {
        refreshes += usize::from(manager.poll_at(valid_at).refresh_inventory);
        manager.session("COM41").unwrap().generation == 2
            && manager.entries[&port_key("COM41")].worker.is_none()
    });
    assert_eq!(refreshes, 0);
    assert_eq!(opens.load(Ordering::SeqCst), 2);
    assert_eq!(
        manager.session("COM41").unwrap().state,
        ConnectionState::Faulted
    );
    assert_eq!(manager.session("COM41").unwrap().generation, 2);
    assert_eq!(
        manager.session("COM41").unwrap().last_error.as_deref(),
        Some("synthetic busy")
    );
    assert!(manager.is_reconnecting("COM41"));
    for seconds in [1, 2, 100] {
        let scanned = valid_at + Duration::from_secs(seconds);
        apply(&mut manager, &snapshot, scanned, scanned);
        assert!(!manager.poll_at(scanned).refresh_inventory);
        assert_eq!(opens.load(Ordering::SeqCst), 2);
        let view = manager.session("COM41").unwrap();
        assert_eq!(view.state, ConnectionState::Faulted);
        assert_eq!(view.generation, 2);
        assert_eq!(view.last_error.as_deref(), Some("synthetic busy"));
    }

    manager.note_native_change(valid_at + Duration::from_secs(101));
    let scanned = valid_at + Duration::from_secs(102);
    apply(&mut manager, &snapshot, scanned, scanned);
    assert_eq!(recovered_at(&mut manager, &recovered, scanned, 3), 0);
    assert_eq!(opens.load(Ordering::SeqCst), 3);
    assert_eq!(
        manager.session("COM41").unwrap().state,
        ConnectionState::Connected
    );
    assert_eq!(manager.session("COM41").unwrap().generation, 3);
    assert_eq!(manager.session("COM41").unwrap().last_error, None);
    assert!(!manager.is_reconnecting("COM41"));

    recovered.fail();
    assert_eq!(
        lost_at(&mut manager, valid_at + Duration::from_secs(103)),
        0
    );
    for seconds in [104, 1_000] {
        let scanned = valid_at + Duration::from_secs(seconds);
        apply(&mut manager, &snapshot, scanned, scanned);
        assert!(!manager.poll_at(scanned).refresh_inventory);
        assert_eq!(opens.load(Ordering::SeqCst), 3);
        assert_eq!(
            manager.session("COM41").unwrap().state,
            ConnectionState::Faulted
        );
        assert_eq!(manager.session("COM41").unwrap().generation, 3);
    }
}

#[test]
fn replacement_identity_scans_wait_for_exact_original_before_one_busy_recovery_attempt() {
    let original = receiver_inventory("synthetic-original-hid");
    let mut invalid = Vec::new();
    for serial in [
        "synthetic-replacement",
        "SYNTHETIC-ORIGINAL",
        " synthetic-original",
        "synthetic-original ",
    ] {
        let mut replacement = original.clone();
        replacement.ports[0].serial_number = Some(serial.into());
        invalid.push((serial, replacement));
    }
    let mut wrong_vid = original.clone();
    wrong_vid.ports[0].vid = Some(0x1915);
    invalid.push(("wrong VID", wrong_vid));
    let mut wrong_pid = original.clone();
    wrong_pid.ports[0].pid = Some(0x7692);
    invalid.push(("wrong PID", wrong_pid));
    let mut wrong_port = original.clone();
    wrong_port.ports[0].port_name = "COM43".into();
    invalid.push(("different canonical COM", wrong_port));
    let mut wrong_interface = original.clone();
    wrong_interface.ports[0]
        .usb_interface
        .as_mut()
        .unwrap()
        .number = Some(1);
    invalid.push(("different interface number", wrong_interface));
    assert_identity_guards_then_bounded_busy_recovery(invalid);
}

#[test]
fn raw_or_ambiguous_receiver_scans_wait_for_console_before_one_busy_recovery_attempt() {
    let original = receiver_inventory("synthetic-original-hid");
    let mut raw_console = original.clone();
    raw_console.ports[0]
        .usb_interface
        .as_mut()
        .unwrap()
        .description = InterfaceDescription::Value("SlimeNRF Raw Data CDC".into());
    let mut ambiguous_description = original.clone();
    ambiguous_description.ports[0]
        .usb_interface
        .as_mut()
        .unwrap()
        .description = InterfaceDescription::Ambiguous;
    let mut duplicate_receiver = original.clone();
    let mut receiver = duplicate_receiver.receivers[0].clone();
    receiver.key = "synthetic-other-hid".into();
    receiver.path = b"synthetic-other-hid".to_vec();
    duplicate_receiver.receivers.push(receiver);
    let mut second_console = original.clone();
    let mut console = second_console.ports[0].clone();
    console.port_name = "COM43".into();
    console.usb_interface.as_mut().unwrap().number = Some(4);
    second_console.ports.push(console);
    assert_identity_guards_then_bounded_busy_recovery(vec![
        ("raw-data role", raw_console),
        ("ambiguous interface description", ambiguous_description),
        ("duplicate receiver association", duplicate_receiver),
        ("multiple residual consoles", second_console),
    ]);
}
