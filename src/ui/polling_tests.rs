use super::{
    InventoryScanIntent, InventoryScanTrigger, ShutdownHost, ShutdownPayload,
    command_panel::CommandFocus, devices::revoke_receiver_tracker_selection,
    inventory_display_changed, inventory_scan_trigger, serial_poll_result, serial_visible_revision,
};
use crate::discovery::{BindingResetCause, ReceiverMonitor};
use crate::domain::{Inventory, ReceiverEndpoint, SerialEndpoint, SerialPortKind};
use crate::sessions::{
    SessionManager,
    test_support::{display_manager, display_test_session},
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};

fn poll_serial(
    manager: &mut SessionManager,
    visible: bool,
    active: Option<&str>,
) -> super::SerialPoll {
    let before = serial_visible_revision(manager, visible, active);
    let changes = manager.poll();
    serial_poll_result(
        &changes,
        before,
        serial_visible_revision(manager, visible, active),
    )
}

#[test]
fn serial_redraw_tracks_visible_tab_but_always_reports_metadata() {
    let mut manager = display_manager();
    let active = display_test_session(&mut manager, "active");
    let background = display_test_session(&mut manager, "background");
    assert!(!poll_serial(&mut manager, true, Some("active")).redraw);

    background.push(b"retained in dormant tab\n");
    assert!(!poll_serial(&mut manager, true, Some("active")).redraw);
    assert_eq!(
        manager.session("background").unwrap().terminal.text(),
        "retained in dormant tab\n",
    );
    // The first visible text must invalidate even before the UI has synced
    // an empty terminal and assigned TerminalSession::revision.
    active.push(b"\x1b[31m\xf0\x9f");
    assert!(!poll_serial(&mut manager, true, Some("active")).redraw);
    active.push(b"\xa6\x80\n");
    let traffic = poll_serial(&mut manager, true, Some("active"));
    assert!(traffic.redraw);
    assert!(!traffic.state_changed);
    assert_eq!(
        manager.session("active").unwrap().terminal.text(),
        "\u{1f980}\n"
    );
    assert!(!poll_serial(&mut manager, true, Some("active")).redraw);

    active.push(b"hidden page\n");
    assert!(!poll_serial(&mut manager, false, Some("active")).redraw);
    active.push(b"no selected tab\n");
    assert!(!poll_serial(&mut manager, true, None).redraw);
    assert!(
        manager
            .session("active")
            .unwrap()
            .terminal
            .text()
            .ends_with("hidden page\nno selected tab\n"),
    );
    background.push(b"selected background\n");
    assert!(poll_serial(&mut manager, true, Some("background")).redraw);

    background.set_name("Background tracker");
    let identity = poll_serial(&mut manager, false, Some("active"));
    assert!(identity.redraw);
    assert!(identity.state_changed);
    background.close(Some("background failure".into()), 0);
    assert!(poll_serial(&mut manager, true, Some("active")).redraw);
    assert_eq!(
        manager.session("background").unwrap().last_error.as_deref(),
        Some("background failure"),
    );
}

#[test]
fn unchanged_inventory_suppresses_redraw_but_replacement_and_errors_do_not() {
    let inventory = Inventory {
        ports: vec![SerialEndpoint {
            port_name: "fixture-console".into(),
            label: "Fixture console".into(),
            manufacturer: None,
            product: None,
            kind: SerialPortKind::Usb,
            serial_number: Some("fixture-a".into()),
            vid: Some(0x1209),
            pid: Some(0x7690),
            usb_interface: None,
        }],
        receivers: vec![ReceiverEndpoint {
            key: "fixture-hid".into(),
            path: b"fixture-hid".to_vec(),
            label: "Fixture receiver".into(),
            serial_number: Some("fixture-a".into()),
        }],
        ..Inventory::default()
    };
    assert!(!inventory_display_changed(&inventory, &inventory.clone()));
    let mut changed = inventory.clone();
    changed.ports[0].serial_number = Some("fixture-b".into());
    assert!(inventory_display_changed(&inventory, &changed));
    changed = inventory.clone();
    changed.receivers[0].serial_number = Some("fixture-b".into());
    assert!(inventory_display_changed(&inventory, &changed));
    changed = inventory.clone();
    changed.warnings.push("enumeration incomplete".into());
    assert!(inventory_display_changed(&inventory, &changed));
    assert!(inventory_display_changed(&changed, &inventory));
    changed = inventory.clone();
    changed.serial_enumeration_failed = true;
    assert!(inventory_display_changed(&inventory, &changed));
    assert!(!inventory_display_changed(&changed, &changed.clone()));
    assert!(inventory_display_changed(&changed, &inventory));
    assert!(inventory_display_changed(&inventory, &Inventory::default()));
}

#[test]
fn notified_and_scheduled_scans_preserve_bindings_and_selection_until_manual_rediscovery() {
    let endpoint = ReceiverEndpoint {
        key: "scan-receiver".into(),
        path: b"scan-receiver".to_vec(),
        label: "Synthetic receiver".into(),
        serial_number: Some("scan-receiver".into()),
    };
    for (intent, generation, reset) in [
        (InventoryScanIntent::Notify, 0, false),
        (InventoryScanIntent::Notify, 4, false),
        (InventoryScanIntent::Scheduled, 4, false),
        (InventoryScanIntent::Rediscover, 0, false),
        (InventoryScanIntent::Rediscover, 4, true),
    ] {
        let mut monitor = ReceiverMonitor::fixture(endpoint.clone(), 9, "AABBCCDDEEFF", true);
        let mut selected = [
            "tracker:scan-receiver:9".to_owned(),
            "tracker:other-receiver:3".to_owned(),
            "receiver:scan-receiver".to_owned(),
        ]
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
        let original_selection = selected.clone();
        let mut focus = Some(CommandFocus::Tracker {
            receiver: endpoint.key.clone(),
            slot: 9,
            address: Some("AABBCCDDEEFF".into()),
        });
        assert_eq!(
            intent.rediscover_bindings(&mut monitor, generation),
            reset,
            "{intent:?}"
        );
        let changes = monitor.take_binding_invalidations();
        for (receiver, cause) in &changes {
            assert_eq!(*cause, BindingResetCause::ManualRediscovery);
            revoke_receiver_tracker_selection(&receiver.key, &mut selected, &mut focus);
        }
        if reset {
            assert_eq!(changes.len(), 1);
            assert!(
                monitor
                    .observation_for(&endpoint)
                    .unwrap()
                    .trackers
                    .is_empty()
            );
            assert!(!selected.contains("tracker:scan-receiver:9"));
            assert!(selected.contains("tracker:other-receiver:3"));
            assert!(selected.contains("receiver:scan-receiver"));
            assert!(focus.is_none());
        } else {
            assert!(changes.is_empty(), "{intent:?}");
            let trackers = &monitor.observation_for(&endpoint).unwrap().trackers;
            assert_eq!(trackers.len(), 1, "{intent:?}");
            assert_eq!(trackers[0].receiver_key, endpoint.key, "{intent:?}");
            assert_eq!(trackers[0].slot, 9, "{intent:?}");
            assert_eq!(
                trackers[0].address.as_deref(),
                Some("AABBCCDDEEFF"),
                "{intent:?}"
            );
            assert!(trackers[0].online, "{intent:?}");
            assert_eq!(selected, original_selection, "{intent:?}");
            assert!(focus.is_some(), "{intent:?}");
        }
        assert!(monitor.take_binding_invalidations().is_empty());
    }
}

#[test]
fn native_change_while_scan_is_busy_remains_one_follow_up() {
    let now = Instant::now();
    let periodic = now + Duration::from_secs(3);
    let dirty = AtomicBool::new(true);
    let takes = AtomicUsize::new(0);
    let take_dirty = || {
        takes.fetch_add(1, Ordering::SeqCst);
        dirty.swap(false, Ordering::SeqCst)
    };
    assert_eq!(
        inventory_scan_trigger(true, periodic, now, take_dirty),
        None
    );
    assert_eq!(takes.load(Ordering::SeqCst), 0);
    assert!(dirty.load(Ordering::SeqCst));
    assert_eq!(
        inventory_scan_trigger(false, periodic, now, take_dirty),
        Some(InventoryScanTrigger::NativeChange)
    );
    assert_eq!(takes.load(Ordering::SeqCst), 1);
    assert!(!dirty.load(Ordering::SeqCst));
    assert_eq!(
        inventory_scan_trigger(false, periodic, now, take_dirty),
        None
    );
    assert_eq!(takes.load(Ordering::SeqCst), 2);
}

#[test]
fn due_loss_refresh_also_consumes_native_change_for_the_same_scan() {
    let now = Instant::now();
    let due_loss_refresh = now;
    let dirty = AtomicBool::new(true);
    assert_eq!(
        inventory_scan_trigger(false, due_loss_refresh, now, || {
            dirty.swap(false, Ordering::SeqCst)
        }),
        Some(InventoryScanTrigger::NativeChange)
    );
    assert!(!dirty.load(Ordering::SeqCst));
    let next_periodic = now + Duration::from_secs(3);
    assert_eq!(
        inventory_scan_trigger(false, next_periodic, now, || {
            dirty.swap(false, Ordering::SeqCst)
        }),
        None
    );
}

#[test]
fn scan_already_running_cannot_discard_a_new_due_loss_follow_up() {
    let now = Instant::now();
    let due_loss_refresh = now;
    assert_eq!(
        inventory_scan_trigger(true, due_loss_refresh, now, || {
            panic!("busy scan must retain the native signal")
        }),
        None
    );
    assert_eq!(
        inventory_scan_trigger(
            false,
            due_loss_refresh,
            now + Duration::from_millis(1),
            || false
        ),
        Some(InventoryScanTrigger::Scheduled)
    );
}

fn gated_shutdown_payload() -> (ShutdownPayload, mpsc::Sender<()>, Arc<AtomicUsize>) {
    let (release, gate) = mpsc::channel();
    let joined = Arc::new(AtomicUsize::new(0));
    let completed = joined.clone();
    let worker = std::thread::spawn(move || {
        gate.recv().unwrap();
        completed.fetch_add(1, Ordering::SeqCst);
    });
    (
        ShutdownPayload {
            replay: vec![worker],
            ..ShutdownPayload::default()
        },
        release,
        joined,
    )
}

#[test]
fn shutdown_spawn_failure_defers_payload_without_blocking_and_host_joins_once() {
    let host = Arc::new(ShutdownHost::default());
    use futures_util::FutureExt as _;
    let (mut payload, release, joined) = gated_shutdown_payload();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("accepted.txt");
    let target = path.clone();
    let (release_file, file_gate) = mpsc::channel();
    let executor = background_executor();
    let file = executor
        .spawn(async move {
            file_gate.recv().unwrap();
            std::fs::write(target, b"accepted work completed").unwrap();
        })
        .shared();
    payload.files.push(file.clone());
    payload._background = Some(executor.clone());
    let completion_task = executor.spawn(file);
    drop(completion_task);
    let mut permission = host.start_with(payload, |_, _| {
        Err(std::io::Error::other("fixture spawn failure"))
    });
    assert!(matches!(
        permission.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Closed)
    ));
    assert_eq!(joined.load(Ordering::SeqCst), 0);
    let (finished, completion) = mpsc::channel();
    let draining = host.clone();
    let waiter = std::thread::spawn(move || {
        draining.finish();
        finished.send(()).unwrap();
    });
    assert!(matches!(
        completion.recv_timeout(Duration::from_millis(30)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    release.send(()).unwrap();
    assert!(matches!(
        completion.recv_timeout(Duration::from_millis(30)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    release_file.send(()).unwrap();
    completion.recv_timeout(Duration::from_secs(2)).unwrap();
    waiter.join().unwrap();
    host.finish();
    assert_eq!(joined.load(Ordering::SeqCst), 1);
    assert_eq!(std::fs::read(path).unwrap(), b"accepted work completed");
}

#[test]
fn shutdown_worker_and_host_share_exactly_one_cleanup_payload() {
    let host = Arc::new(ShutdownHost::default());
    let (payload, release, joined) = gated_shutdown_payload();
    let mut permission = host.start(payload);
    assert!(matches!(
        permission.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Empty)
    ));
    release.send(()).unwrap();
    host.finish();
    permission.try_recv().unwrap();
    host.finish();
    assert_eq!(joined.load(Ordering::SeqCst), 1);
}

pub(super) fn background_executor() -> gpui_kit::BackgroundExecutor {
    background_executor_with_gate(None)
}

pub(super) fn background_executor_with_gate(
    gate: Option<mpsc::Receiver<()>>,
) -> gpui_kit::BackgroundExecutor {
    struct Dispatcher(parking_lot::Mutex<Option<mpsc::Receiver<()>>>);
    impl gpui_kit::PlatformDispatcher for Dispatcher {
        fn is_main_thread(&self) -> bool {
            false
        }

        fn dispatch(&self, runnable: gpui_kit::RunnableVariant, _: gpui_kit::Priority) {
            let gate = self.0.lock().take();
            std::thread::spawn(move || {
                if let Some(gate) = gate {
                    gate.recv().unwrap();
                }
                runnable.run();
            });
        }

        fn dispatch_on_main_thread(&self, _: gpui_kit::RunnableVariant, _: gpui_kit::Priority) {
            panic!("Background fixture must not schedule UI work");
        }

        fn dispatch_after(&self, duration: Duration, runnable: gpui_kit::RunnableVariant) {
            std::thread::spawn(move || {
                std::thread::sleep(duration);
                runnable.run();
            });
        }

        fn spawn_realtime(&self, work: Box<dyn FnOnce() + Send>) {
            std::thread::spawn(work);
        }
    }
    gpui_kit::BackgroundExecutor::new(Arc::new(Dispatcher(parking_lot::Mutex::new(gate))))
}

pub(super) fn join_background(task: futures_util::future::Shared<gpui_kit::Task<()>>) {
    ShutdownPayload {
        files: vec![task],
        ..ShutdownPayload::default()
    }
    .join();
}
