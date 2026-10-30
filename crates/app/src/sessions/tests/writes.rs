use super::super::native::Transport;
use super::super::{
    Command, ConnectionState, PendingWrite, QUEUE_CAPACITY, SessionManager, WriteReceipt, port_key,
};
use super::write_transport::PartialErrorWriter;
use super::{completed, connected, fixture, peer, socket_transport, stopped, until};
use parking_lot::Mutex;
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

#[test]
fn real_socket_write_failure_is_visible_and_other_port_stays_connected() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (clone_tx, clone_rx) = mpsc::channel();
    let mut manager = SessionManager::with_factory(Arc::new(move |_| {
        let stream = socket_transport(address)?;
        clone_tx.send(stream.try_clone()?).unwrap();
        Ok(Box::new(stream))
    }));
    manager.connect("broken").unwrap();
    let _broken_peer = peer(&listener);
    let mut broken = clone_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    manager.connect("healthy").unwrap();
    let _healthy_peer = peer(&listener);
    let _healthy = clone_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    connected(&mut manager, "broken");
    connected(&mut manager, "healthy");
    broken.shutdown(Shutdown::Write).unwrap();
    let native_error = broken.write(b"info\n").unwrap_err();
    let mut receipt = manager.send_line("broken", "info").unwrap();
    stopped(&mut manager, "broken");
    let error = completed(&mut receipt).unwrap_err();
    assert!(error.contains(&native_error.to_string()), "{error}");
    assert_eq!(
        manager.session("broken").unwrap().state,
        ConnectionState::Faulted
    );
    assert_eq!(
        manager.session("broken").unwrap().last_error.as_deref(),
        Some(error.as_str())
    );
    assert_eq!(
        manager.session("healthy").unwrap().state,
        ConnectionState::Connected
    );
    manager.shutdown().unwrap().join().unwrap();
}

#[test]
fn write_errors_never_replay_partial_bytes_and_only_fault_the_owning_port() {
    for kind in [
        io::ErrorKind::TimedOut,
        io::ErrorKind::WouldBlock,
        io::ErrorKind::BrokenPipe,
    ] {
        for (confirmed, unreported) in [(0, 0), (0, 2), (2, 1)] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let (ready_tx, ready_rx) = mpsc::channel();
            let (release_tx, release_rx) = mpsc::channel();
            let release = Mutex::new(Some(release_rx));
            let calls = Arc::new(AtomicUsize::new(0));
            let factory_calls = calls.clone();
            let opens = Arc::new(AtomicUsize::new(0));
            let factory_opens = opens.clone();
            let mut manager = SessionManager::with_factory(Arc::new(move |port| {
                factory_opens.fetch_add(1, Ordering::SeqCst);
                let stream = socket_transport(address)?;
                if port == "faulting" {
                    Ok(Box::new(PartialErrorWriter {
                        stream,
                        ready: ready_tx.clone(),
                        release: release.lock().take().unwrap(),
                        calls: factory_calls.clone(),
                        confirmed_prefix: confirmed,
                        unreported_prefix: unreported,
                        error_kind: kind,
                        failed: false,
                    }) as Box<dyn Transport>)
                } else {
                    Ok(Box::new(stream) as Box<dyn Transport>)
                }
            }));
            manager.connect("faulting").unwrap();
            let mut faulting_peer = peer(&listener);
            manager.connect("healthy").unwrap();
            let mut healthy_peer = peer(&listener);
            connected(&mut manager, "faulting");
            connected(&mut manager, "healthy");
            let healthy_generation = manager.session("healthy").unwrap().generation;
            let mut active = manager.send_line("faulting", "save").unwrap();
            ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            let mut queued = manager.send_line("faulting", "queued").unwrap();
            release_tx.send(()).unwrap();
            stopped(&mut manager, "faulting");
            let error = completed(&mut active).unwrap_err();
            assert!(error.contains("injected write failure"));
            assert_eq!(completed(&mut queued).unwrap_err(), error);
            let faulting = manager.session("faulting").unwrap();
            assert_eq!(faulting.port, "faulting");
            assert_eq!(faulting.state, ConnectionState::Faulted);
            assert_eq!(faulting.last_error.as_deref(), Some(error.as_str()));
            assert_eq!(
                calls.load(Ordering::SeqCst),
                if confirmed == 0 { 1 } else { 2 },
                "An error must not replay an ambiguous write"
            );
            let mut received = Vec::new();
            faulting_peer.read_to_end(&mut received).unwrap();
            assert_eq!(received, b"save\n"[..confirmed + unreported]);
            let healthy = manager.session("healthy").unwrap();
            assert_eq!(healthy.state, ConnectionState::Connected);
            assert_eq!(healthy.generation, healthy_generation);
            assert!(healthy.last_error.is_none());
            let mut healthy_receipt = manager.send_line("healthy", "info").unwrap();
            assert!(completed(&mut healthy_receipt).is_ok());
            let mut bytes = [0; 5];
            healthy_peer.read_exact(&mut bytes).unwrap();
            assert_eq!(&bytes, b"info\n");
            assert_eq!(opens.load(Ordering::SeqCst), 2, "No automatic reconnect");
            manager.shutdown().unwrap().join().unwrap();
            let mut extra = Vec::new();
            healthy_peer.read_to_end(&mut extra).unwrap();
            assert!(
                extra.is_empty(),
                "No rerouting or replay on the healthy port"
            );
        }
    }
}
struct GatedWriter {
    stream: TcpStream,
    ready: mpsc::Sender<()>,
    release: mpsc::Receiver<()>,
    first: bool,
}

impl Read for GatedWriter {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.stream.read(bytes)
    }
}

impl Write for GatedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.first {
            self.first = false;
            self.ready.send(()).unwrap();
            self.release.recv_timeout(Duration::from_secs(2)).unwrap();
        }
        // Exercise partial writes as well as a controllable in-flight write.
        self.stream.write(&bytes[..bytes.len().min(1)])
    }
    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

#[test]
fn full_command_queue_reports_error_but_cannot_block_cancellation() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (ready_tx, ready_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release = Mutex::new(Some(release_rx));
    let mut manager = SessionManager::with_factory(Arc::new(move |_| {
        Ok(Box::new(GatedWriter {
            stream: socket_transport(address)?,
            ready: ready_tx.clone(),
            release: release.lock().take().unwrap(),
            first: true,
        }))
    }));
    manager.connect("busy").unwrap();
    let mut peer = peer(&listener);
    connected(&mut manager, "busy");
    let mut active = manager.send_line("busy", "first").unwrap();
    ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(active.poll().is_none());
    let mut queued: Vec<_> = (0..QUEUE_CAPACITY)
        .map(|_| manager.send_line("busy", "queued").unwrap())
        .collect();
    assert!(manager.send_line("busy", "overflow").is_err());
    assert!(!manager.writes_idle("busy"));
    assert!(manager.session("busy").unwrap().last_error.is_some());
    manager.disconnect("busy").unwrap();
    assert_eq!(
        manager.session("busy").unwrap().state,
        ConnectionState::Closing
    );
    release_tx.send(()).unwrap();
    stopped(&mut manager, "busy");
    assert!(manager.writes_idle("busy"));
    completed(&mut active).unwrap_err();
    for receipt in &mut queued {
        completed(receipt).unwrap_err();
    }
    assert_eq!(
        manager.session("busy").unwrap().state,
        ConnectionState::Disconnected
    );
    let mut bytes = Vec::new();
    peer.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"f");
}

fn gated_fixture() -> (
    SessionManager,
    TcpStream,
    TcpStream,
    mpsc::Receiver<()>,
    mpsc::Sender<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (ready_tx, ready_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (stream_tx, stream_rx) = mpsc::channel();
    let release = Mutex::new(Some(release_rx));
    let mut manager = SessionManager::with_factory(Arc::new(move |_| {
        let stream = socket_transport(address)?;
        stream_tx.send(stream.try_clone()?).unwrap();
        Ok(Box::new(GatedWriter {
            stream,
            ready: ready_tx.clone(),
            release: release.lock().take().unwrap(),
            first: true,
        }))
    }));
    manager.connect("gated").unwrap();
    let peer = peer(&listener);
    let stream = stream_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    connected(&mut manager, "gated");
    (manager, peer, stream, ready_rx, release_tx)
}

#[test]
fn close_rejects_accepted_writes_without_cancelling_or_abandoning_them() {
    let (mut manager, mut peer, stream, ready, release) = gated_fixture();
    drop(stream);
    let generation = manager.session("gated").unwrap().generation;
    let cancel = Arc::clone(
        &manager.entries[&port_key("gated")]
            .worker
            .as_ref()
            .unwrap()
            .cancel,
    );
    let active = manager.send_line("gated", "info").unwrap();
    ready.recv_timeout(Duration::from_secs(2)).unwrap();
    let queued = manager.send_line("gated", "help").unwrap();
    drop(active);
    drop(queued);
    assert!(!manager.can_request_close("gated"));
    assert_eq!(
        manager.request_close("gated").unwrap_err(),
        "gated has pending command writes"
    );
    assert!(!manager.is_close_requested("gated"));
    assert!(!cancel.load(Ordering::Acquire));
    assert_eq!(
        manager.entries[&port_key("gated")]
            .pending_writes
            .load(Ordering::Acquire),
        2
    );
    assert_eq!(manager.session("gated").unwrap().generation, generation);
    assert_eq!(
        manager.session("gated").unwrap().state,
        ConnectionState::Connected
    );
    assert!(manager.session("gated").unwrap().last_error.is_none());

    release.send(()).unwrap();
    until(|| manager.writes_idle("gated"));
    let mut bytes = [0; 10];
    peer.read_exact(&mut bytes).unwrap();
    assert_eq!(&bytes, b"info\nhelp\n");
    assert!(manager.can_request_close("gated"));
    manager.request_close("gated").unwrap();
    stopped(&mut manager, "gated");
    assert!(manager.is_close_requested("gated"));
    assert_eq!(manager.session("gated").unwrap().generation, generation);
    assert_eq!(peer.read(&mut [0; 1]).unwrap(), 0);
    assert!(manager.forget("gated"));
}

#[test]
fn firmware_write_barrier_waits_for_accepted_work_without_receipt_polling() {
    let (mut manager, mut peer, _stream, ready, release) = gated_fixture();
    assert!(manager.writes_idle("gated"));
    assert!(manager.writes_idle("unconnected"));
    let active = manager.send_line("gated", "info").unwrap();
    ready.recv_timeout(Duration::from_secs(2)).unwrap();
    let queued = manager.send_line("gated", "help").unwrap();
    // Firmware consumer has gated new commands; neither abandoning UI
    // receipts nor polling the manager may bypass its transport barrier.
    drop(active);
    drop(queued);
    let _ = manager.poll();
    assert!(!manager.writes_idle("gated"));
    assert!(manager.writes_idle("unconnected"));
    release.send(()).unwrap();
    until(|| manager.writes_idle("gated"));
    let mut bytes = [0; 10];
    peer.read_exact(&mut bytes).unwrap();
    assert_eq!(&bytes, b"info\nhelp\n");
    assert_eq!(
        manager.session("gated").unwrap().state,
        ConnectionState::Connected
    );
    manager.shutdown().unwrap().join().unwrap();
}

#[test]
fn firmware_write_barrier_survives_shutdown_until_worker_reaped() {
    let (mut manager, _peer, _stream, ready, release) = gated_fixture();
    let active = manager.send_line("gated", "info").unwrap();
    ready.recv_timeout(Duration::from_secs(2)).unwrap();
    let queued = manager.send_line("gated", "help").unwrap();
    let reaper = manager.shutdown().unwrap();
    assert!(!manager.writes_idle("gated"));
    drop(active);
    drop(queued);
    assert!(!manager.writes_idle("gated"));
    release.send(()).unwrap();
    reaper.join().unwrap();
    assert!(manager.writes_idle("gated"));
}

#[test]
fn receipt_waits_for_actual_complete_write_without_firmware_reply() {
    let (mut manager, mut peer, _stream, ready, release) = gated_fixture();
    let mut receipt = manager.send_line("gated", "info").unwrap();
    ready.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(receipt.poll().is_none(), "enqueue is not write completion");
    release.send(()).unwrap();
    let mut bytes = [0; 5];
    peer.read_exact(&mut bytes).unwrap();
    assert_eq!(&bytes, b"info\n");
    completed(&mut receipt).unwrap();
    let _ = manager.poll();
    assert!(manager.session("gated").unwrap().terminal.text().is_empty());
    manager.shutdown().unwrap().join().unwrap();
}

#[test]
fn forwarded_slots_share_transport_without_deduplicating_writes_or_receipts() {
    use crate::features::commands::{
        batch::{BatchRun, Outcome, TargetOutcome},
        policy::{CommandContext, CommandSnapshot, RouteEvidence, presets},
    };
    let (mut manager, listener) = fixture();
    manager.connect("shared").unwrap();
    let mut stream = peer(&listener);
    connected(&mut manager, "shared");
    let generation = manager.session("shared").unwrap().generation;
    let preset = presets(CommandContext::ForwardedTracker)
        .find(|preset| preset.id == "remote-sens")
        .unwrap();
    let mut run = BatchRun {
        command: "Sensitivity".into(),
        targets: Vec::new(),
    };
    for slot in [2, 7] {
        let evidence = RouteEvidence::Forwarded {
            port: "shared",
            generation,
            connected: true,
            receiver_key: "receiver-a",
            slot,
            address: Some("AABBCCDDEEFF"),
            unique_receiver_route: true,
        };
        let snapshot = CommandSnapshot::capture(preset, "0,0,0", evidence).unwrap();
        snapshot.authorize(evidence, true).unwrap();
        run.targets.push(TargetOutcome::submitted(
            format!("Tracker {slot}"),
            manager.send_line(snapshot.port(), snapshot.command()),
        ));
    }
    assert!(run.pending());
    let mut bytes = [0; 36];
    stream.read_exact(&mut bytes).unwrap();
    assert_eq!(&bytes, b"send 2 sens 0,0,0\nsend 7 sens 0,0,0\n");
    until(|| {
        run.poll();
        !run.pending()
    });
    assert_eq!(run.targets.len(), 2);
    assert!(
        run.targets
            .iter()
            .all(|target| target.outcome == Outcome::Written)
    );
    assert!(!run.poll(), "Each receipt resolves once");
    manager.shutdown().unwrap().join().unwrap();
    let mut extra = Vec::new();
    stream.read_to_end(&mut extra).unwrap();
    assert!(
        extra.is_empty(),
        "No duplicate writes after both receipts complete"
    );
}

#[test]
fn failed_write_resolves_active_and_all_queued_receipts_without_success() {
    let (mut manager, mut peer, mut stream, ready, release) = gated_fixture();
    let mut active = manager.send_line("gated", "info").unwrap();
    ready.recv_timeout(Duration::from_secs(2)).unwrap();
    let mut queued: Vec<_> = (0..QUEUE_CAPACITY)
        .map(|_| manager.send_line("gated", "queued").unwrap())
        .collect();
    assert!(!manager.writes_idle("gated"));
    stream.shutdown(Shutdown::Write).unwrap();
    let native_error = stream.write(b"info\n").unwrap_err();
    release.send(()).unwrap();
    stopped(&mut manager, "gated");
    assert!(manager.writes_idle("gated"));
    let error = completed(&mut active).unwrap_err();
    assert!(error.contains(&native_error.to_string()), "{error}");
    for receipt in &mut queued {
        assert_eq!(completed(receipt).unwrap_err(), error);
    }
    let session = manager.session("gated").unwrap();
    assert_eq!(session.state, ConnectionState::Faulted);
    assert_eq!(session.last_error.as_deref(), Some(error.as_str()));
    assert_eq!(peer.read(&mut [0; 1]).unwrap(), 0);
}

#[test]
fn shutdown_and_manager_drop_resolve_pending_receipts() {
    for drop_manager in [false, true] {
        let (mut manager, _peer, _stream, ready, release) = gated_fixture();
        let mut active = manager.send_line("gated", "info").unwrap();
        ready.recv_timeout(Duration::from_secs(2)).unwrap();
        let mut queued = manager.send_line("gated", "queued").unwrap();
        let pending = Arc::clone(&manager.entries[&port_key("gated")].pending_writes);
        let cancellation = Arc::clone(
            &manager.entries[&port_key("gated")]
                .worker
                .as_ref()
                .unwrap()
                .cancel,
        );
        let reaper = if drop_manager {
            Some(thread::spawn(move || drop(manager)))
        } else {
            manager.shutdown()
        };
        until(|| cancellation.load(Ordering::Acquire));
        // Drop/shutdown cannot complete while the transport write owns its
        // gated handle. Release from this thread, not the joining thread.
        assert!(!reaper.as_ref().unwrap().is_finished());
        assert_eq!(pending.load(Ordering::Acquire), 2);
        release.send(()).unwrap();
        if let Some(reaper) = reaper {
            reaper.join().unwrap();
        }
        assert_eq!(pending.load(Ordering::Acquire), 0);
        assert!(completed(&mut active).is_err());
        assert!(completed(&mut queued).is_err());
    }
}

#[test]
fn abandoned_queued_receipt_fails_once_instead_of_staying_pending() {
    let (sender, receiver) = mpsc::sync_channel(1);
    let (completion, receipt) = mpsc::sync_channel(1);
    let pending = Arc::new(AtomicUsize::new(0));
    sender
        .try_send(Command {
            bytes: b"info\n".to_vec(),
            completion,
            thermal_dump: None,
            _pending: PendingWrite::new(&pending),
        })
        .unwrap();
    let mut receipt = WriteReceipt {
        completion: Some(receipt),
    };
    assert!(receipt.poll().is_none());
    assert_eq!(pending.load(Ordering::Acquire), 1);
    drop(receiver);
    assert_eq!(pending.load(Ordering::Acquire), 0);
    assert!(completed(&mut receipt).is_err());
}
