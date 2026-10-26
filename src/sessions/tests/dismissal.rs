use crate::domain::{SerialEndpoint, SerialPortKind};
use crate::sessions::test_support::{
    display_manager, display_test_session, held_worker_session, stopped,
};
use crate::sessions::{
    ConnectionState, LossProbe, PendingWrite, Reconnect, SessionManager, UsbIdentity, port_key,
};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

fn archive(manager: &mut SessionManager, port: &str, faulted: bool) {
    let display = display_test_session(manager, port);
    manager
        .entries
        .get_mut(&port_key(port))
        .unwrap()
        .worker
        .as_mut()
        .unwrap()
        .handle = Some(thread::spawn(|| {}));
    display.push(b"Synthetic retained transcript\n");
    display.close(faulted.then(|| "Synthetic transport fault".to_owned()), 0);
    stopped(manager, port);
}

#[test]
fn explicit_dismissal_releases_only_the_requested_settled_history() {
    let mut manager = display_manager();
    archive(&mut manager, "archive-A", false);
    archive(&mut manager, "archive-B", true);
    assert_eq!(
        manager.session("archive-A").unwrap().terminal.text(),
        "Synthetic retained transcript\n"
    );
    assert_eq!(
        manager.session("archive-B").unwrap().state,
        ConnectionState::Faulted
    );
    let owner = Arc::downgrade(&manager.entries[&port_key("archive-A")].pending_writes);

    assert!(manager.can_forget("archive-A"));
    assert!(manager.forget("archive-A"));
    assert!(owner.upgrade().is_none());
    assert!(manager.session("archive-A").is_none());
    assert_eq!(manager.sessions().count(), 1);
    assert_eq!(
        manager.session("archive-B").unwrap().terminal.text(),
        "Synthetic retained transcript\n"
    );
    assert!(manager.forget("archive-B"));
    assert_eq!(manager.sessions().count(), 0);
    assert!(!manager.forget("missing"));
}

#[test]
fn closed_worker_must_join_before_its_tab_is_removable() {
    let mut manager = display_manager();
    let display = display_test_session(&mut manager, "joining");
    let (release, gate) = mpsc::channel();
    manager
        .entries
        .get_mut(&port_key("joining"))
        .unwrap()
        .worker
        .as_mut()
        .unwrap()
        .handle = Some(thread::spawn(move || {
        gate.recv_timeout(Duration::from_secs(3)).unwrap()
    }));
    display.push(b"Last bytes before close\n");
    display.close(None, 0);
    let _ = manager.poll();
    assert_eq!(
        manager.session("joining").unwrap().state,
        ConnectionState::Disconnected
    );
    assert!(!manager.can_forget("joining"));
    assert!(!manager.forget("joining"));
    release.send(()).unwrap();
    stopped(&mut manager, "joining");
    assert_eq!(
        manager.session("joining").unwrap().terminal.text(),
        "Last bytes before close\n"
    );
    assert!(manager.forget("joining"));
}

#[test]
fn live_states_reservations_reconnects_writes_and_thermal_requests_reject_dismissal() {
    let mut manager = display_manager();
    archive(&mut manager, "retained", false);
    let key = port_key("retained");
    for state in [
        ConnectionState::Connecting,
        ConnectionState::Connected,
        ConnectionState::Closing,
    ] {
        manager.entries.get_mut(&key).unwrap().view.state = state;
        assert!(!manager.forget("retained"), "{state:?}");
        assert_eq!(
            manager.session("retained").unwrap().terminal.text(),
            "Synthetic retained transcript\n"
        );
    }
    manager.entries.get_mut(&key).unwrap().view.state = ConnectionState::Disconnected;
    let reservation = manager.reserve("retained", Some(1)).unwrap();
    assert!(!manager.can_forget("retained"));
    assert!(!manager.forget("retained"));
    manager.release(&reservation).unwrap();

    let endpoint = SerialEndpoint {
        port_name: "retained".into(),
        label: "Synthetic console".into(),
        manufacturer: None,
        product: None,
        kind: SerialPortKind::Usb,
        serial_number: Some("SYNTHETIC-DISMISSAL".into()),
        vid: Some(0x1209),
        pid: Some(0x7692),
        usb_interface: None,
    };
    manager.entries.get_mut(&key).unwrap().reconnect = Some(Reconnect {
        identity: UsbIdentity::from_endpoint(&endpoint).unwrap(),
        arrival_pending: false,
        loss_probe: LossProbe::Available,
        native_change_after: None,
    });
    assert!(manager.is_reconnecting("retained"));
    assert!(!manager.forget("retained"));
    manager.disconnect("retained").unwrap();

    let write = PendingWrite::new(&manager.entries[&key].pending_writes);
    assert!(!manager.writes_idle("retained"));
    assert!(!manager.forget("retained"));
    drop(write);
    assert!(manager.writes_idle("retained"));
    let dump = manager.thermal_dumps.lock().begin("retained", 1).unwrap();
    assert!(!manager.can_forget("retained"));
    assert!(!manager.forget("retained"));
    assert!(manager.cancel_thermal_dump(&dump));
    assert!(manager.can_forget("retained"));
    assert!(manager.forget("retained"));
}

#[test]
fn forgetting_many_settled_ports_does_not_accumulate_transcripts() {
    let mut manager = display_manager();
    for index in 0..24 {
        let port = format!("synthetic-{index}");
        archive(&mut manager, &port, index % 2 == 0);
    }
    assert_eq!(manager.sessions().count(), 24);
    for index in 0..24 {
        assert!(manager.forget(&format!("synthetic-{index}")));
    }
    assert_eq!(manager.sessions().count(), 0);
}

fn aliases(port: &str) -> Vec<String> {
    if cfg!(windows) {
        vec![
            port.to_owned(),
            format!(r"\\.\{}", port.to_ascii_lowercase()),
            format!(r"\\?\{}", port.to_ascii_lowercase()),
        ]
    } else {
        vec![port.to_owned()]
    }
}

fn rejects_closed_generation(manager: &mut SessionManager, port: &str, generation: u64) {
    let next_generation = manager.next_generation;
    let last_error = manager.session(port).unwrap().last_error.clone();
    for alias in aliases(port) {
        let expected = format!("{alias} terminal is closing");
        assert_eq!(manager.connect(&alias).unwrap_err(), expected);
        assert_eq!(
            manager
                .open_connection(&alias, true, Instant::now())
                .unwrap_err(),
            expected
        );
        assert_eq!(
            manager.send_line(&alias, "tcal dump").unwrap_err(),
            expected
        );
        assert_eq!(manager.reserve(&alias, None).unwrap_err(), expected);
        assert!(manager.is_close_requested(&alias));
        assert!(manager.can_request_close(&alias));
        manager.request_close(&alias).unwrap();
    }
    assert_eq!(manager.session(port).unwrap().generation, generation);
    assert_eq!(manager.session(port).unwrap().last_error, last_error);
    assert_eq!(manager.next_generation, next_generation);
    assert!(!manager.command_port_reserved(port));
    assert!(manager.active_thermal_dumps().is_empty());
}

#[test]
fn close_keeps_the_worker_and_final_transcript_until_join() {
    let mut manager = display_manager();
    let (display, release) = held_worker_session(&mut manager, "COM37");
    let cancel = Arc::clone(
        &manager.entries[&port_key("COM37")]
            .worker
            .as_ref()
            .unwrap()
            .cancel,
    );
    display.push(b"Final serial bytes\n");
    assert!(manager.can_request_close("COM37"));
    manager.request_close("COM37").unwrap();
    assert!(cancel.load(Ordering::Acquire));
    assert_eq!(
        manager.session("COM37").unwrap().state,
        ConnectionState::Closing
    );
    rejects_closed_generation(&mut manager, "COM37", 1);

    display.close(None, 0);
    let _ = manager.poll();
    assert_eq!(
        manager.session("COM37").unwrap().state,
        ConnectionState::Disconnected
    );
    assert_eq!(
        manager.session("COM37").unwrap().terminal.text(),
        "Final serial bytes\n"
    );
    assert!(manager.entries[&port_key("COM37")].worker.is_some());
    assert!(!manager.can_forget("COM37"));
    assert!(!manager.forget("COM37"));
    rejects_closed_generation(&mut manager, "COM37", 1);

    release.send(()).unwrap();
    stopped(&mut manager, "COM37");
    rejects_closed_generation(&mut manager, "COM37", 1);
    assert!(manager.can_forget("COM37"));
    assert!(manager.forget("COM37"));
    assert!(!manager.is_close_requested("COM37"));
    assert_eq!(manager.sessions().count(), 0);
}

#[test]
fn repeated_close_after_a_fault_is_inert_before_and_after_join() {
    let mut manager = display_manager();
    let (display, release) = held_worker_session(&mut manager, "COM38");
    display.close(Some("Synthetic transport fault".into()), 0);
    let _ = manager.poll();
    assert_eq!(
        manager.session("COM38").unwrap().state,
        ConnectionState::Faulted
    );
    manager.request_close("COM38").unwrap();
    assert!(
        !manager.entries[&port_key("COM38")]
            .worker
            .as_ref()
            .unwrap()
            .cancel
            .load(Ordering::Acquire)
    );
    rejects_closed_generation(&mut manager, "COM38", 1);
    assert!(!manager.can_forget("COM38"));
    release.send(()).unwrap();
    stopped(&mut manager, "COM38");
    rejects_closed_generation(&mut manager, "COM38", 1);
    assert_eq!(
        manager.session("COM38").unwrap().state,
        ConnectionState::Faulted
    );
    assert_eq!(
        manager.session("COM38").unwrap().last_error.as_deref(),
        Some("Synthetic transport fault")
    );
    assert!(manager.forget("COM38"));
}

#[test]
fn reservations_missing_sessions_and_shutdown_reject_close_without_latching() {
    let mut manager = display_manager();
    archive(&mut manager, "COM39", true);
    let token = manager.reserve("COM39", Some(1)).unwrap();
    for alias in aliases("COM39") {
        assert!(!manager.can_request_close(&alias));
        assert_eq!(
            manager.request_close(&alias).unwrap_err(),
            format!("{alias} is reserved for a firmware operation")
        );
        assert!(!manager.is_close_requested(&alias));
        assert_eq!(
            manager.session(&alias).unwrap().state,
            ConnectionState::Faulted
        );
        assert_eq!(manager.session(&alias).unwrap().generation, 1);
        assert_eq!(
            manager.session(&alias).unwrap().last_error.as_deref(),
            Some("Synthetic transport fault")
        );
        assert!(manager.reservation_ready(&token).unwrap());
    }
    assert!(!manager.can_request_close("missing"));
    assert!(manager.request_close("missing").is_err());
    assert_eq!(manager.sessions().count(), 1);
    manager.release(&token).unwrap();
    assert!(manager.can_request_close("COM39"));
    assert!(manager.shutdown().is_none());
    assert!(!manager.can_request_close("COM39"));
    assert!(manager.request_close("COM39").is_err());
    assert!(!manager.is_close_requested("COM39"));
    assert_eq!(manager.session("COM39").unwrap().generation, 1);
    assert_eq!(
        manager.session("COM39").unwrap().terminal.text(),
        "Synthetic retained transcript\n"
    );
}
