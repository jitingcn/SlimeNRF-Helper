use super::{CalibrationSession, GuideSelection, selections};
use crate::calibration::{
    Application, CalibrationTask, Dispatch, Fact, ObservationEnd, Persistence, RunMode, Target,
    fixture,
};
use crate::discovery::ReceiverMonitor;
use crate::domain::{
    DeviceIdentity, DeviceRole, Inventory, ReceiverEndpoint, SerialEndpoint, SerialPortKind,
};
use crate::sessions::{ConnectionState, test_support};
use std::{
    io,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

fn endpoint() -> SerialEndpoint {
    SerialEndpoint {
        port_name: "calibration-fixture".into(),
        label: "Synthetic tracker USB".into(),
        manufacturer: Some("SlimeVR".into()),
        product: Some("SlimeVR Tracker".into()),
        kind: SerialPortKind::Usb,
        serial_number: Some("SYNTHETIC-ONLY".into()),
        vid: Some(0x1209),
        pid: Some(0x0001),
        usb_interface: None,
    }
}
fn inventory() -> Inventory {
    Inventory {
        ports: vec![endpoint()],
        ..Inventory::default()
    }
}
fn selection() -> GuideSelection {
    GuideSelection {
        label: "Synthetic tracker".into(),
        target: Target::Direct {
            endpoint: endpoint(),
            generation: None,
            address: Some("010203040506".into()),
        },
    }
}
fn until(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "calibration fixture did not join"
        );
        std::thread::yield_now();
    }
}

#[test]
fn consumed_start_and_stop_before_dispatch_leave_existing_console_untouched() {
    let inventory = inventory();
    let mut manager = test_support::display_manager();
    let mut monitor = ReceiverMonitor::new();
    let mut session = CalibrationSession::default();
    let first = session
        .review(
            selection(),
            CalibrationTask::Stationary,
            &inventory,
            &manager,
            &monitor,
        )
        .unwrap();
    let duplicate = session
        .review(
            selection(),
            CalibrationTask::Stationary,
            &inventory,
            &manager,
            &monitor,
        )
        .unwrap();
    session
        .start(first, RunMode::Execute, &mut manager, &mut monitor)
        .unwrap();
    assert!(
        session
            .start(duplicate, RunMode::Execute, &mut manager, &mut monitor)
            .is_err()
    );
    assert!(session.blocks_port(&endpoint().port_name, None, Some(&endpoint())));
    assert!(!session.stopping());
    session.stop_observing();
    assert!(session.stopping());
    assert!(session.native_busy());
    assert!(session.blocks_port(&endpoint().port_name, None, Some(&endpoint())));
    assert!(!session.snapshot().unwrap().joined);
    assert_eq!(session.snapshot().unwrap().end, None);
    assert!(session.acknowledge().is_err());
    session.poll(&inventory, &mut manager, &mut monitor);
    let result = session.snapshot().unwrap();
    assert_eq!(result.dispatch, Dispatch::NotAttempted);
    assert_eq!(result.end, Some(ObservationEnd::Stopped));
    assert!(result.joined);
    assert!(!session.busy());
    assert!(!session.stopping());
    let token = manager.reserve(&endpoint().port_name, None).unwrap();
    assert!(manager.reservation_ready(&token).unwrap());
    manager.release(&token).unwrap();
}

fn gated_worker(
    session: &mut CalibrationSession,
    application: Application,
    storage_error: bool,
) -> (
    std::sync::mpsc::Receiver<()>,
    std::sync::mpsc::Receiver<()>,
    std::sync::mpsc::Sender<()>,
) {
    let pending = session.pending.take().unwrap();
    let (started, ready) = std::sync::mpsc::channel();
    let (stopped, held) = std::sync::mpsc::channel();
    let (release, cleanup) = std::sync::mpsc::channel();
    session.worker = Some(
        crate::calibration::CalibrationWorker::scripted(
            pending.request,
            None,
            move |_, engine, stop, publish| {
                engine.fact(Fact::DispatchAttempt, 1);
                engine.fact(Fact::Written, 2);
                engine.fact(Fact::Application(application), 3);
                publish(&engine.snapshot);
                started.send(()).unwrap();
                until(|| stop.load(Ordering::Acquire));
                if storage_error {
                    engine.fact(Fact::StorageError, 4);
                }
                engine.finish(ObservationEnd::Stopped, 5);
                publish(&engine.snapshot);
                stopped.send(()).unwrap();
                cleanup
                    .recv_timeout(Duration::from_secs(3))
                    .map_err(|error| error.to_string())?;
                Ok(())
            },
        )
        .unwrap(),
    );
    (ready, held, release)
}

#[test]
fn poll_reports_only_new_publications_and_join() {
    let inventory = inventory();
    let mut manager = test_support::display_manager();
    let mut monitor = ReceiverMonitor::new();
    let mut session = CalibrationSession::default();
    let ticket = session
        .review(
            selection(),
            CalibrationTask::Stationary,
            &inventory,
            &manager,
            &monitor,
        )
        .unwrap();
    session
        .start(ticket, RunMode::Execute, &mut manager, &mut monitor)
        .unwrap();
    session.serial = Some(manager.reserve(&endpoint().port_name, None).unwrap());
    let (ready, held, release) = gated_worker(&mut session, Application::Applied, true);
    ready.recv_timeout(Duration::from_secs(3)).unwrap();
    let initial = session.snapshot().unwrap();
    assert_eq!(initial.application, Application::Applied);
    assert_eq!(initial.persistence, Persistence::Unknown);
    assert!(!initial.joined);
    assert!(session.poll(&inventory, &mut manager, &mut monitor));
    assert!(!session.poll(&inventory, &mut manager, &mut monitor));
    assert!(Arc::ptr_eq(&initial, &session.snapshot().unwrap()));
    assert!(session.native_busy());
    assert!(manager.reserve(&endpoint().port_name, None).is_err());

    session.stop_observing();
    held.recv_timeout(Duration::from_secs(3)).unwrap();
    let stopped = session.snapshot().unwrap();
    assert_eq!(stopped.persistence, Persistence::StorageError);
    assert_eq!(stopped.end, Some(ObservationEnd::Stopped));
    assert!(!stopped.joined);
    assert!(session.poll(&inventory, &mut manager, &mut monitor));
    assert!(!session.poll(&inventory, &mut manager, &mut monitor));
    assert!(Arc::ptr_eq(&stopped, &session.snapshot().unwrap()));
    assert!(session.native_busy());
    assert!(session.acknowledge().is_err());
    assert!(session.blocks_port(&endpoint().port_name, None, Some(&endpoint())));
    assert!(manager.reserve(&endpoint().port_name, None).is_err());

    release.send(()).unwrap();
    until(|| {
        let changed = session.poll(&inventory, &mut manager, &mut monitor);
        if session.native_busy() {
            assert!(!changed);
            assert!(manager.reserve(&endpoint().port_name, None).is_err());
            false
        } else {
            assert!(changed);
            true
        }
    });
    let result = session.snapshot().unwrap();
    assert!(result.joined);
    assert_eq!(result.persistence, Persistence::StorageError);
    assert_eq!(result.end, Some(ObservationEnd::Stopped));
    assert!(!session.stopping());
    let token = manager.reserve(&endpoint().port_name, None).unwrap();
    manager.release(&token).unwrap();
    assert!(!session.poll(&inventory, &mut manager, &mut monitor));
}

#[test]
fn stopped_worker_retains_cleanup_and_evidence_until_join() {
    for (application, storage_error) in [
        (Application::Unconfirmed, false),
        (Application::Applied, false),
        (Application::Applied, true),
    ] {
        let inventory = inventory();
        let mut manager = test_support::display_manager();
        let mut monitor = ReceiverMonitor::new();
        let mut session = CalibrationSession::default();
        let ticket = session
            .review(
                selection(),
                CalibrationTask::Stationary,
                &inventory,
                &manager,
                &monitor,
            )
            .unwrap();
        session
            .start(ticket, RunMode::Execute, &mut manager, &mut monitor)
            .unwrap();
        session.serial = Some(manager.reserve(&endpoint().port_name, None).unwrap());
        let (ready, held, release) = gated_worker(&mut session, application, storage_error);
        ready.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(!session.stopping());
        session.stop_observing();
        assert!(session.stopping());
        assert!(session.native_busy());
        assert!(!session.snapshot().unwrap().joined);
        held.recv_timeout(Duration::from_secs(3)).unwrap();
        session.poll(&inventory, &mut manager, &mut monitor);
        let observed = session.snapshot().unwrap();
        assert!(session.stopping());
        assert!(session.native_busy());
        assert!(!observed.joined);
        assert_eq!(observed.application, application);
        let persistence = if storage_error {
            Persistence::StorageError
        } else {
            Persistence::Unknown
        };
        assert_eq!(observed.persistence, persistence);
        assert_eq!(observed.end, Some(ObservationEnd::Stopped));
        assert!(session.acknowledge().is_err());
        assert!(session.blocks_port(&endpoint().port_name, None, Some(&endpoint())));
        assert!(manager.reserve(&endpoint().port_name, None).is_err());
        release.send(()).unwrap();
        until(|| {
            session.poll(&inventory, &mut manager, &mut monitor);
            !session.native_busy()
        });
        assert!(!session.stopping());
        let result = session.snapshot().unwrap();
        assert!(result.joined);
        assert_eq!(result.application, application);
        assert_eq!(result.persistence, persistence);
        assert_eq!(result.end, Some(ObservationEnd::Stopped));
        let token = manager.reserve(&endpoint().port_name, None).unwrap();
        manager.release(&token).unwrap();
        if application == Application::Unconfirmed {
            assert!(session.busy());
            assert!(session.blocks_port(&endpoint().port_name, None, Some(&endpoint())));
            session.acknowledge().unwrap();
            assert!(session.snapshot().unwrap().acknowledged);
            assert!(!session.blocks_port(&endpoint().port_name, None, Some(&endpoint())));
        }
    }
}

struct FailedDirect;
impl fixture::DirectIo for FailedDirect {
    fn now_ms(&self) -> u64 {
        0
    }
    fn read(&mut self, _: &mut [u8]) -> Result<usize, String> {
        Err("Synthetic console disconnected".into())
    }
    fn write_command(&mut self, _: &[u8], _: &AtomicBool) -> Result<(), String> {
        panic!("disconnected fixture must not write")
    }
}
#[test]
fn actual_direct_engine_failure_joins_and_releases_without_opening_os_devices() {
    let inventory = inventory();
    let mut manager = test_support::display_manager();
    let mut monitor = ReceiverMonitor::new();
    let mut session = CalibrationSession::default();
    session.install_fixture(Arc::new(|_| {
        Ok(fixture::Transport::Direct(Box::new(FailedDirect)))
    }));
    let review = session
        .review(
            selection(),
            CalibrationTask::Stationary,
            &inventory,
            &manager,
            &monitor,
        )
        .unwrap();
    session
        .start(review, RunMode::Inspect, &mut manager, &mut monitor)
        .unwrap();
    until(|| {
        session.poll(&inventory, &mut manager, &mut monitor);
        !session.native_busy()
    });
    let result = session.snapshot().unwrap();
    assert_eq!(result.dispatch, Dispatch::NotAttempted);
    assert_eq!(result.application, Application::Unconfirmed);
    assert_eq!(result.end, Some(ObservationEnd::PreparationFailed));
    assert!(result.joined);
    assert!(!session.blocks_port(&endpoint().port_name, None, Some(&endpoint())));
}

struct FailedWireless {
    writes: Arc<Mutex<Vec<[u8; 65]>>>,
}
impl fixture::WirelessIo for FailedWireless {
    fn now_ms(&self) -> u64 {
        0
    }
    fn read(&mut self, _: &mut [u8; 65], _: i32) -> Result<usize, String> {
        Err("Synthetic receiver disconnected".into())
    }
    fn write(&mut self, bytes: &[u8; 65]) -> Result<(), String> {
        self.writes.lock().unwrap().push(*bytes);
        Ok(())
    }
}
#[test]
fn actual_wireless_engine_preparation_failure_never_dispatches_a_calibration() {
    let receiver = ReceiverEndpoint {
        key: "synthetic-receiver".into(),
        path: b"synthetic-only".to_vec(),
        label: "Synthetic receiver".into(),
        serial_number: Some("SYNTHETIC-RX".into()),
    };
    let inventory = Inventory {
        receivers: vec![receiver.clone()],
        ..Inventory::default()
    };
    let mut monitor = ReceiverMonitor::fixture(receiver, 2, "010203040506", true);
    let mut manager = test_support::display_manager();
    let mut session = CalibrationSession::default();
    let writes = Arc::new(Mutex::new(Vec::new()));
    let observed = writes.clone();
    session.install_fixture(Arc::new(move |_| {
        Ok(fixture::Transport::Wireless(Box::new(FailedWireless {
            writes: observed.clone(),
        })))
    }));
    let target = selections(&inventory, &manager, &monitor).remove(0);
    let review = session
        .review(
            target,
            CalibrationTask::Stationary,
            &inventory,
            &manager,
            &monitor,
        )
        .unwrap();
    session
        .start(review, RunMode::Execute, &mut manager, &mut monitor)
        .unwrap();
    until(|| {
        session.poll(&inventory, &mut manager, &mut monitor);
        !session.native_busy()
    });
    let result = session.snapshot().unwrap();
    assert_eq!(result.dispatch, Dispatch::NotAttempted);
    assert_eq!(result.end, Some(ObservationEnd::PreparationFailed));
    assert!(result.joined);
    assert!(writes.lock().unwrap().iter().all(|bytes| bytes[3] == 224));
    assert!(!session.blocks_receiver("synthetic-receiver"));
}

#[test]
fn acknowledging_unknown_never_rewrites_evidence_or_releases_an_unjoined_worker() {
    let inventory = inventory();
    let mut manager = test_support::display_manager();
    let mut monitor = ReceiverMonitor::new();
    let mut session = CalibrationSession::default();
    let ticket = session
        .review(
            selection(),
            CalibrationTask::Stationary,
            &inventory,
            &manager,
            &monitor,
        )
        .unwrap();
    session
        .start(ticket, RunMode::Execute, &mut manager, &mut monitor)
        .unwrap();
    let pending = session.pending.take().unwrap();
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let wait = gate.clone();
    let (published, ready) = std::sync::mpsc::channel();
    session.worker = Some(
        crate::calibration::CalibrationWorker::scripted(
            pending.request,
            None,
            move |_, engine, _, publish| {
                engine.fact(Fact::DispatchAttempt, 1);
                engine.fact(Fact::Application(Application::CandidateQueued), 2);
                engine.fact(Fact::StorageError, 3);
                engine.finish(ObservationEnd::Stopped, 4);
                publish(&engine.snapshot);
                published.send(()).unwrap();
                let (lock, condition) = &*wait;
                let mut released = lock.lock().unwrap();
                while !*released {
                    let (next, timeout) = condition
                        .wait_timeout(released, Duration::from_secs(3))
                        .unwrap();
                    released = next;
                    if timeout.timed_out() && !*released {
                        return Err("Fixture join gate timed out".into());
                    }
                }
                Ok(())
            },
        )
        .unwrap(),
    );
    ready.recv_timeout(Duration::from_secs(3)).unwrap();
    assert!(session.acknowledge().is_err());
    assert!(session.blocks_port(&endpoint().port_name, None, Some(&endpoint())));
    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();
    until(|| {
        session.poll(&inventory, &mut manager, &mut monitor);
        !session.native_busy()
    });
    assert!(session.busy());
    session.acknowledge().unwrap();
    let result = session.snapshot().unwrap();
    assert_eq!(result.application, Application::CandidateQueued);
    assert_eq!(result.persistence, Persistence::StorageError);
    assert_eq!(result.dispatch, Dispatch::Attempted);
    assert_eq!(result.end, Some(ObservationEnd::Stopped));
    assert!(result.acknowledged);
    assert!(!session.busy());
    assert!(!session.blocks_port(&endpoint().port_name, None, Some(&endpoint())));
}

#[test]
fn positive_aliases_are_monotonic_and_unrelated_devices_remain_available() {
    let mut inventory = inventory();
    let mut manager = test_support::display_manager();
    let mut monitor = ReceiverMonitor::new();
    let mut session = CalibrationSession::default();
    let ticket = session
        .review(
            selection(),
            CalibrationTask::Stationary,
            &inventory,
            &manager,
            &monitor,
        )
        .unwrap();
    session
        .start(ticket, RunMode::Execute, &mut manager, &mut monitor)
        .unwrap();
    let mut alias = endpoint();
    alias.port_name = "new-positive-alias".into();
    alias.serial_number = Some("OTHER-USB".into());
    let identity = DeviceIdentity {
        role: DeviceRole::Tracker,
        address: Some("010203040506".into()),
        ..DeviceIdentity::default()
    };
    assert!(session.blocks_port(&alias.port_name, Some(&identity), Some(&alias)));
    assert!(session.blocks_port(&alias.port_name, None, None));
    let mut moved = alias.clone();
    moved.port_name = "moved-alias".into();
    assert!(session.blocks_port(&moved.port_name, None, Some(&moved)));
    let mut unrelated = endpoint();
    unrelated.port_name = "unrelated".into();
    unrelated.serial_number = Some("UNRELATED".into());
    assert!(!session.blocks_port(&unrelated.port_name, None, Some(&unrelated)));
    let mut replacement = endpoint();
    replacement.serial_number = Some("UNRELATED-REPLACEMENT".into());
    assert!(session.blocks_port(&replacement.port_name, None, Some(&replacement)));
    replacement.port_name = "replacement-moved".into();
    assert!(!session.blocks_port(&replacement.port_name, None, Some(&replacement)));
    inventory.ports.clear();
    session.observe_aliases(&inventory, &manager, &monitor);
    assert!(session.blocks_port(&alias.port_name, None, None));
    session.stop_observing();
    session.poll(&inventory, &mut manager, &mut monitor);
}

struct BlockingWrite {
    gate: Arc<(Mutex<bool>, Condvar)>,
    written: Arc<AtomicBool>,
}
impl io::Read for BlockingWrite {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        std::thread::yield_now();
        Err(io::ErrorKind::TimedOut.into())
    }
}
impl io::Write for BlockingWrite {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.written.store(true, Ordering::Release);
        let (lock, condition) = &*self.gate;
        let mut released = lock.lock().unwrap();
        while !*released {
            let (next, timeout) = condition
                .wait_timeout(released, Duration::from_secs(3))
                .unwrap();
            released = next;
            if timeout.timed_out() && !*released {
                return Err(io::ErrorKind::TimedOut.into());
            }
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
#[test]
fn accepted_write_is_not_cancelled_to_acquire_a_console_reservation() {
    let inventory = inventory();
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let written = Arc::new(AtomicBool::new(false));
    let factory_gate = gate.clone();
    let factory_written = written.clone();
    let mut manager = test_support::with_transport_factory(move |_| {
        Ok(BlockingWrite {
            gate: factory_gate.clone(),
            written: factory_written.clone(),
        })
    });
    manager.connect(&endpoint().port_name).unwrap();
    test_support::connected(&mut manager, &endpoint().port_name);
    let mut receipt = manager.send_line(&endpoint().port_name, "info").unwrap();
    until(|| written.load(Ordering::Acquire));
    let mut monitor = ReceiverMonitor::new();
    let mut session = CalibrationSession::default();
    let review = session
        .review(
            selection(),
            CalibrationTask::Stationary,
            &inventory,
            &manager,
            &monitor,
        )
        .unwrap();
    session
        .start(review, RunMode::Inspect, &mut manager, &mut monitor)
        .unwrap();
    session.poll(&inventory, &mut manager, &mut monitor);
    assert!(session.serial.is_none());
    assert_eq!(
        manager.session(&endpoint().port_name).unwrap().state,
        ConnectionState::Connected
    );
    assert!(!manager.writes_idle(&endpoint().port_name));
    session.stop_observing();
    session.poll(&inventory, &mut manager, &mut monitor);
    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();
    assert_eq!(test_support::completed(&mut receipt), Ok(()));
    assert_eq!(session.snapshot().unwrap().dispatch, Dispatch::NotAttempted);
    // The worker publishes the receipt before dropping the command's pending-write guard.
    until(|| manager.writes_idle(&endpoint().port_name));
    let cleanup = manager.shutdown();
    if let Some(cleanup) = cleanup {
        cleanup.join().unwrap();
    }
}

#[test]
fn hidden_polling_and_shutdown_retain_the_worker_until_join() {
    let inventory = inventory();
    let mut manager = test_support::display_manager();
    let mut monitor = ReceiverMonitor::new();
    let mut session = CalibrationSession::default();
    let ticket = session
        .review(
            selection(),
            CalibrationTask::Stationary,
            &inventory,
            &manager,
            &monitor,
        )
        .unwrap();
    session
        .start(ticket, RunMode::Execute, &mut manager, &mut monitor)
        .unwrap();
    let pending = session.pending.take().unwrap();
    let (started, ready) = std::sync::mpsc::channel();
    let closed = Arc::new(AtomicBool::new(false));
    let observed = closed.clone();
    session.worker = Some(
        crate::calibration::CalibrationWorker::scripted(
            pending.request,
            None,
            move |_, engine, stop, publish| {
                engine.fact(Fact::DispatchAttempt, 1);
                engine.fact(Fact::Written, 2);
                engine.fact(Fact::Application(Application::Applied), 3);
                publish(&engine.snapshot);
                started.send(()).unwrap();
                while !stop.load(Ordering::Acquire) {
                    std::thread::yield_now();
                }
                engine.finish(ObservationEnd::Stopped, 4);
                observed.store(true, Ordering::Release);
                Ok(())
            },
        )
        .unwrap(),
    );
    ready.recv_timeout(Duration::from_secs(3)).unwrap();
    session.poll(&inventory, &mut manager, &mut monitor);
    assert_eq!(
        session.snapshot().unwrap().application,
        Application::Applied
    );
    assert!(session.native_busy());
    let cleanup = session.take_shutdown_worker().unwrap();
    std::thread::spawn(move || drop(cleanup)).join().unwrap();
    assert!(closed.load(Ordering::Acquire));
    assert!(!session.native_busy());
}

#[test]
fn unsupported_and_ambiguous_targets_never_enter_a_preparation() {
    let mut unknown_inventory = inventory();
    let manager = test_support::display_manager();
    let monitor = ReceiverMonitor::new();
    let session = CalibrationSession::default();
    unknown_inventory.ports[0].product = Some("Unidentified serial console".into());
    let mut unknown = selection();
    if let Target::Direct { endpoint, .. } = &mut unknown.target {
        *endpoint = unknown_inventory.ports[0].clone();
    }
    assert!(
        session
            .review(
                unknown,
                CalibrationTask::Stationary,
                &unknown_inventory,
                &manager,
                &monitor
            )
            .is_err()
    );
    let mut inventory = inventory();
    let mut duplicate = endpoint();
    duplicate.port_name = "duplicate-USB".into();
    inventory.ports.push(duplicate);
    assert!(
        session
            .review(
                selection(),
                CalibrationTask::Stationary,
                &inventory,
                &manager,
                &monitor
            )
            .is_err()
    );
    assert_eq!(session.snapshot().map(|snapshot| snapshot.dispatch), None);
}

struct ConsoleScript {
    now: u64,
    input: std::collections::VecDeque<u8>,
    writes: Arc<Mutex<Vec<Vec<u8>>>>,
}
impl fixture::DirectIo for ConsoleScript {
    fn now_ms(&self) -> u64 {
        self.now
    }
    fn read(&mut self, bytes: &mut [u8]) -> Result<usize, String> {
        self.now += 100;
        let count = bytes.len().min(512).min(self.input.len());
        for byte in &mut bytes[..count] {
            *byte = self.input.pop_front().unwrap();
        }
        Ok(count)
    }
    fn write_command(&mut self, bytes: &[u8], _: &AtomicBool) -> Result<(), String> {
        self.writes.lock().unwrap().push(bytes.to_vec());
        let text = match bytes {
            b"help\n" => crate::calibration::direct::FIXTURE_HELP,
            b"info\n" => crate::calibration::direct::FIXTURE_INFO,
            b"calibrate\n" => {
                "[00:00:01.000,000] <inf> cal_imu: Calibrating main accelerometer and gyroscope zero rate offset\n[00:00:01.001,000] <inf> cal_imu: Reading data\n[00:00:01.002,000] <inf> cal_imu: Calibration queued for next sensor frame\n"
            }
            _ => return Err("Unexpected fixture command".into()),
        };
        self.input.extend(text.bytes());
        Ok(())
    }
}

#[test]
fn stationary_inspection_execution_readback_recovery_and_acknowledgement_use_production_engine() {
    let inventory = inventory();
    let mut manager = test_support::display_manager();
    let mut monitor = ReceiverMonitor::new();
    let mut session = CalibrationSession::default();
    let writes = Arc::new(Mutex::new(Vec::new()));
    let observed = writes.clone();
    session.install_fixture(Arc::new(move |_| {
        Ok(fixture::Transport::Direct(Box::new(ConsoleScript {
            now: 0,
            input: Default::default(),
            writes: observed.clone(),
        })))
    }));
    let mut target = selection();
    if let Target::Direct { address, .. } = &mut target.target {
        *address = None;
    }
    let review = session
        .review(
            target.clone(),
            CalibrationTask::Stationary,
            &inventory,
            &manager,
            &monitor,
        )
        .unwrap();
    session
        .start(review, RunMode::Inspect, &mut manager, &mut monitor)
        .unwrap();
    until(|| {
        session.poll(&inventory, &mut manager, &mut monitor);
        !session.native_busy()
    });
    let inspected = session.snapshot().unwrap();
    assert_eq!(inspected.identity.as_deref(), Some("AABBCCDDEEFF"));
    assert!(inspected.stationary_supported);
    assert_eq!(inspected.dispatch, Dispatch::NotAttempted);
    if let Target::Direct { address, .. } = &mut target.target {
        *address = inspected.identity.clone();
    }
    let review = session
        .review(
            target,
            CalibrationTask::Stationary,
            &inventory,
            &manager,
            &monitor,
        )
        .unwrap();
    session
        .start(review, RunMode::Execute, &mut manager, &mut monitor)
        .unwrap();
    until(|| {
        session.poll(&inventory, &mut manager, &mut monitor);
        !session.native_busy()
    });
    let completed = session.snapshot().unwrap();
    assert_eq!(completed.application, Application::CandidateQueued);
    assert_eq!(completed.persistence, Persistence::Unknown);
    assert_eq!(
        completed.baseline.as_ref().unwrap().bias,
        Some([0.1, 0.2, 0.3])
    );
    assert_eq!(
        completed.readback.as_ref().unwrap().bias,
        Some([0.1, 0.2, 0.3])
    );
    assert_eq!(completed.end, Some(ObservationEnd::Complete));
    assert!(completed.joined);
    assert!(session.busy());
    session.recover(&mut manager, &mut monitor).unwrap();
    until(|| {
        session.poll(&inventory, &mut manager, &mut monitor);
        !session.native_busy()
    });
    let recovered = session.snapshot().unwrap();
    assert_eq!(recovered.id, completed.id);
    assert_ne!(recovered.native_open, completed.native_open);
    assert_eq!(
        recovered.baseline.as_ref().unwrap().native_open,
        completed.native_open
    );
    assert_eq!(
        recovered.readback.as_ref().unwrap().native_open,
        recovered.native_open
    );
    assert_eq!(recovered.application, Application::CandidateQueued);
    assert_eq!(recovered.persistence, Persistence::Unknown);
    assert_eq!(recovered.dispatch, Dispatch::Written);
    session.acknowledge().unwrap();
    assert!(!session.busy());
    assert_eq!(
        session.snapshot().unwrap().application,
        Application::CandidateQueued
    );
    let observed = writes.lock().unwrap();
    assert_eq!(
        *observed,
        vec![
            b"help\n".to_vec(),
            b"info\n".to_vec(),
            b"help\n".to_vec(),
            b"info\n".to_vec(),
            b"calibrate\n".to_vec(),
            b"info\n".to_vec(),
            b"help\n".to_vec(),
            b"info\n".to_vec()
        ]
    );
}

#[test]
fn reviewed_ticket_refreshes_positive_usb_aliases_before_admission() {
    let mut inventory = inventory();
    let manager = test_support::display_manager();
    let monitor = ReceiverMonitor::new();
    let session = CalibrationSession::default();
    let review = session
        .review(
            selection(),
            CalibrationTask::Stationary,
            &inventory,
            &manager,
            &monitor,
        )
        .unwrap();
    let mut late_alias = endpoint();
    late_alias.port_name = "late-positive-interface".into();
    inventory.ports.push(late_alias);
    let mut unrelated = endpoint();
    unrelated.port_name = "unrelated-interface".into();
    unrelated.serial_number = Some("UNRELATED".into());
    inventory.ports.push(unrelated);
    assert_eq!(
        review.refresh_ports(&inventory, &manager, &monitor),
        vec![
            "calibration-fixture".to_owned(),
            "late-positive-interface".to_owned()
        ]
    );
    assert_eq!(review.address(), Some("010203040506"));
}

#[test]
fn joined_unknown_wireless_run_keeps_only_tracker_and_broad_receiver_exclusion() {
    let receiver = ReceiverEndpoint {
        key: "synthetic-receiver".into(),
        path: b"synthetic-only".to_vec(),
        label: "Synthetic receiver".into(),
        serial_number: Some("SYNTHETIC-RX".into()),
    };
    let inventory = Inventory {
        receivers: vec![receiver.clone()],
        ..Inventory::default()
    };
    let mut manager = test_support::display_manager();
    let mut monitor = ReceiverMonitor::fixture(receiver.clone(), 2, "010203040506", true);
    let mut session = CalibrationSession::default();
    let ticket = session
        .review(
            selections(&inventory, &manager, &monitor).remove(0),
            CalibrationTask::Stationary,
            &inventory,
            &manager,
            &monitor,
        )
        .unwrap();
    session
        .start(ticket, RunMode::Execute, &mut manager, &mut monitor)
        .unwrap();
    assert!(session.blocks_receiver_io(&receiver.key));
    assert!(session.blocks_scoped_tracker_io(&receiver.key, "010203040506"));
    assert!(session.blocks_scoped_tracker_io(&receiver.key, "AABBCCDDEEFF"));
    assert!(!session.blocks_receiver_io("unrelated-receiver"));
    assert!(!session.blocks_scoped_tracker_io("unrelated-receiver", "AABBCCDDEEFF"));
    let pending = session.pending.take().unwrap();
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let wait = gate.clone();
    let (published, ready) = std::sync::mpsc::channel();
    session.worker = Some(
        crate::calibration::CalibrationWorker::scripted(
            pending.request,
            None,
            move |_, engine, _, publish| {
                engine.fact(Fact::DispatchAttempt, 1);
                engine.fact(Fact::Written, 2);
                engine.finish(ObservationEnd::Disconnected, 3);
                publish(&engine.snapshot);
                published.send(()).unwrap();
                let released = wait.0.lock().unwrap();
                let (released, timeout) = wait
                    .1
                    .wait_timeout_while(released, Duration::from_secs(3), |released| !*released)
                    .unwrap();
                drop(released);
                if timeout.timed_out() {
                    return Err("Fixture join gate timed out".into());
                }
                Ok(())
            },
        )
        .unwrap(),
    );
    ready.recv_timeout(Duration::from_secs(3)).unwrap();
    assert!(session.acknowledge().is_err());
    assert!(monitor.is_reserved(&receiver));
    assert!(session.blocks_receiver_io(&receiver.key));
    assert!(session.blocks_scoped_tracker_io(&receiver.key, "AABBCCDDEEFF"));
    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();
    until(|| {
        session.poll(&inventory, &mut manager, &mut monitor);
        !session.native_busy()
    });
    assert!(!monitor.is_reserved(&receiver));
    assert!(session.busy());
    assert!(session.blocks_receiver(&receiver.key));
    assert!(!session.blocks_receiver_io(&receiver.key));
    assert!(session.blocks_scoped_tracker_io(&receiver.key, "01:02:03:04:05:06"));
    assert!(session.blocks_scoped_tracker_io("another-receiver", "010203040506"));
    assert!(!session.blocks_scoped_tracker_io(&receiver.key, "AABBCCDDEEFF"));
    assert!(session.blocks_scoped_tracker_io(&receiver.key, "unknown"));
    let before = session.snapshot().unwrap();
    assert_eq!(before.dispatch, Dispatch::Written);
    assert_eq!(before.application, Application::Unconfirmed);
    assert_eq!(before.end, Some(ObservationEnd::Disconnected));
    session.acknowledge().unwrap();
    let after = session.snapshot().unwrap();
    assert_eq!(after.dispatch, before.dispatch);
    assert_eq!(after.application, before.application);
    assert_eq!(after.persistence, before.persistence);
    assert_eq!(after.end, before.end);
    assert!(after.acknowledged);
    assert!(!session.blocks_receiver(&receiver.key));
    assert!(!session.blocks_scoped_tracker_io(&receiver.key, "010203040506"));
}

#[test]
fn calibration_evidence_freezes_interface_facts_not_display_label() {
    use crate::domain::{InterfaceDescription, UsbInterface};
    let mut frozen = endpoint();
    frozen.usb_interface = Some(UsbInterface {
        number: Some(0),
        description: InterfaceDescription::Unavailable,
    });
    let mut current = frozen.clone();
    current.label = "Renamed display".into();
    assert!(super::same_endpoint(&frozen, &current));
    current.usb_interface.as_mut().unwrap().number = Some(2);
    assert!(!super::same_endpoint(&frozen, &current));
    current = frozen.clone();
    current.usb_interface.as_mut().unwrap().description =
        InterfaceDescription::Value("Changed native interface".into());
    assert!(!super::same_endpoint(&frozen, &current));
}

#[test]
fn known_raw_data_is_not_a_calibration_console_even_with_tracker_product() {
    use crate::domain::{InterfaceDescription, UsbInterface};
    let mut raw = endpoint();
    raw.pid = Some(0x7690);
    raw.usb_interface = Some(UsbInterface {
        number: Some(2),
        description: InterfaceDescription::Value("SlimeNRF Raw Data CDC".into()),
    });
    let inventory = Inventory {
        ports: vec![raw.clone()],
        ..Inventory::default()
    };
    let manager = test_support::display_manager();
    let monitor = ReceiverMonitor::new();
    assert!(selections(&inventory, &manager, &monitor).is_empty());
    let target = Target::Direct {
        endpoint: raw,
        generation: None,
        address: None,
    };
    assert!(super::validate_target(&target, &inventory, &manager, &monitor, true).is_err());
}
