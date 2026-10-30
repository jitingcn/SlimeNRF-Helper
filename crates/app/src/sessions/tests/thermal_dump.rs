use super::super::native::Transport;
use super::super::thermal_dump::ThermalDumpEvent;
use super::super::{
    CHUNK_BYTES, ConnectionState, QUEUE_CAPACITY, SessionManager, ThermalDumpOutcome,
    ThermalDumpRequest, port_key,
};
use super::{completed, connected, fixture, peer, socket_transport, stopped, until};
use crate::domain::DeviceRole;
use crate::features::commands::policy::{CommandContext, presets};
use parking_lot::Mutex;
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::{Arc, mpsc};
use std::time::Duration;

const DUMP: &[u8] = b"Dumping 1 collected temperature calibration points:\r\n\
--------------------------------------------------\r\n\
Index | Temp (C) | Bias X   | Bias Y   | Bias Z   |\r\n\
--------------------------------------------------\r\n\
 7 | -2.50 | 0.10000 | -0.20000 | 0.30000\r\n\
--------------------------------------------------\r\n\
End of dump. Total points printed: 1\r\n";
const EMPTY: &[u8] = b"No temperature calibration points have been collected.\n";

fn request(manager: &SessionManager, port: &str) -> ThermalDumpRequest {
    manager
        .active_thermal_dumps()
        .into_iter()
        .find(|request| request.port == port_key(port))
        .unwrap()
}

// No manager poll: dump completion must not depend on consuming terminal display.
fn event(manager: &mut SessionManager) -> ThermalDumpEvent {
    let mut events = Vec::new();
    until(|| {
        events = manager.drain_thermal_dump_events();
        !events.is_empty()
    });
    assert_eq!(events.len(), 1);
    events.pop().unwrap()
}

fn read_command(peer: &mut TcpStream, expected: &[u8]) {
    let mut bytes = vec![0; expected.len()];
    peer.read_exact(&mut bytes).unwrap();
    assert_eq!(bytes, expected);
}

#[test]
fn terminal_and_preset_use_one_worker_collection_before_lossy_display() {
    let (mut manager, listener) = fixture();
    manager.connect("port").unwrap();
    let mut stream = peer(&listener);
    connected(&mut manager, "port");
    let preset = presets(CommandContext::Direct(DeviceRole::Tracker))
        .find(|preset| preset.id == "tcal-dump")
        .unwrap();
    for line in ["  tcal dump  \r\n".to_owned(), preset.build("").unwrap()] {
        // Saturate the display without consuming the acquisition byte stream.
        let display = Arc::clone(
            &manager.entries[&port_key("port")]
                .worker
                .as_ref()
                .unwrap()
                .display,
        );
        display
            .lock()
            .push(&vec![b'x'; CHUNK_BYTES * QUEUE_CAPACITY]);
        let mut receipt = manager.send_line("port", &line).unwrap();
        let accepted = request(&manager, "port");
        assert_eq!(
            accepted.generation,
            manager.session("port").unwrap().generation
        );
        let wire = format!("{}\n", line.trim_end_matches(['\r', '\n']));
        read_command(&mut stream, wire.as_bytes());
        assert!(completed(&mut receipt).is_ok());
        assert!(manager.send_line("port", "info").is_err());
        assert!(manager.send_line("port", "tcal dump").is_err());
        // Every boundary, including CRLF and number tokens, can be fragmented.
        for byte in DUMP {
            stream.write_all(&[*byte]).unwrap();
        }
        let result = event(&mut manager);
        assert_eq!(result.request, accepted);
        let ThermalDumpOutcome::Complete(dump) = result.outcome else {
            panic!("expected a complete dump");
        };
        assert_eq!(dump.points.len(), 1);
        assert_eq!(dump.points[0].slot_index, 7);
        assert_eq!(dump.points[0].temperature_c, -2.5);
        assert_eq!(dump.points[0].bias_dps, [0.1, -0.2, 0.3]);
        until(|| display.lock().take_dropped() > 0);
        assert!(manager.active_thermal_dumps().is_empty());
    }
}

#[test]
fn only_exact_trimmed_direct_dump_arms_and_other_ports_remain_usable() {
    let (mut manager, listener) = fixture();
    manager.connect("first").unwrap();
    let mut first = peer(&listener);
    manager.connect("second").unwrap();
    let mut second = peer(&listener);
    connected(&mut manager, "first");
    connected(&mut manager, "second");
    for line in ["send 3 tcal dump", "tcal dump extra", "TCAL dump"] {
        let mut receipt = manager.send_line("first", line).unwrap();
        read_command(&mut first, format!("{line}\n").as_bytes());
        assert!(completed(&mut receipt).is_ok());
        assert!(manager.active_thermal_dumps().is_empty());
    }
    let mut receipt = manager.send_line("first", "tcal dump").unwrap();
    read_command(&mut first, b"tcal dump\n");
    assert!(completed(&mut receipt).is_ok());
    let first_request = request(&manager, "first");
    let mut other = manager.send_line("second", "info").unwrap();
    read_command(&mut second, b"info\n");
    assert!(completed(&mut other).is_ok());
    let mut other_dump = manager.send_line("second", "tcal dump").unwrap();
    read_command(&mut second, b"tcal dump\n");
    assert!(completed(&mut other_dump).is_ok());
    let second_request = request(&manager, "second");
    second.write_all(EMPTY).unwrap();
    assert_eq!(event(&mut manager).request, second_request);
    assert_eq!(request(&manager, "first"), first_request);
    first.write_all(DUMP).unwrap();
    assert_eq!(event(&mut manager).request, first_request);
}

#[test]
fn cancellation_is_local_and_late_response_cannot_finish_cancelled_request() {
    let (mut manager, listener) = fixture();
    manager.connect("port").unwrap();
    let mut stream = peer(&listener);
    connected(&mut manager, "port");
    let mut receipt = manager.send_line("port", "tcal dump").unwrap();
    read_command(&mut stream, b"tcal dump\n");
    assert!(completed(&mut receipt).is_ok());
    let accepted = request(&manager, "port");
    assert!(manager.cancel_thermal_dump(&accepted));
    assert!(!manager.cancel_thermal_dump(&accepted));
    let result = event(&mut manager);
    assert_eq!(result.request, accepted);
    assert!(matches!(result.outcome, ThermalDumpOutcome::Cancelled));
    stream.write_all(DUMP).unwrap();
    // The wire protocol has no nonce. Never arm another request over an old tail.
    assert!(manager.thermal_dump_needs_reconnect("port"));
    assert!(
        manager
            .send_line("port", "tcal dump")
            .unwrap_err()
            .contains("reconnect")
    );
    assert!(manager.send_line("port", "info").is_err());
    stream
        .set_read_timeout(Some(Duration::from_millis(60)))
        .unwrap();
    assert!(
        matches!(stream.read(&mut [0]), Err(error) if matches!(error.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock))
    );
    assert!(manager.drain_thermal_dump_events().is_empty());
    assert_eq!(
        manager.session("port").unwrap().state,
        ConnectionState::Connected
    );
    manager.disconnect("port").unwrap();
    stopped(&mut manager, "port");
    manager.connect("port").unwrap();
    let mut fresh = peer(&listener);
    connected(&mut manager, "port");
    assert!(!manager.thermal_dump_needs_reconnect("port"));
    let mut next = manager.send_line("port", "tcal dump").unwrap();
    read_command(&mut fresh, b"tcal dump\n");
    assert!(completed(&mut next).is_ok());
    fresh.write_all(EMPTY).unwrap();
    let result = event(&mut manager);
    assert_ne!(result.request, accepted);
    assert!(matches!(result.outcome, ThermalDumpOutcome::Complete(dump) if dump.points.is_empty()));
}

#[test]
fn close_rejects_active_collection_but_clears_response_only_quarantine() {
    let (mut manager, listener) = fixture();
    manager.connect("port").unwrap();
    let mut stream = peer(&listener);
    connected(&mut manager, "port");
    let generation = manager.session("port").unwrap().generation;
    let mut receipt = manager.send_line("port", "tcal dump").unwrap();
    read_command(&mut stream, b"tcal dump\n");
    assert_eq!(completed(&mut receipt), Ok(()));
    until(|| manager.writes_idle("port"));
    let accepted = request(&manager, "port");
    assert!(!manager.can_request_close("port"));
    assert_eq!(
        manager.request_close("port").unwrap_err(),
        "port has an active temperature dump; wait or cancel it first"
    );
    assert!(!manager.is_close_requested("port"));
    assert_eq!(manager.active_thermal_dumps(), vec![accepted.clone()]);
    assert_eq!(manager.session("port").unwrap().generation, generation);
    assert_eq!(
        manager.session("port").unwrap().state,
        ConnectionState::Connected
    );
    assert!(manager.session("port").unwrap().last_error.is_none());
    assert!(
        !manager.entries[&port_key("port")]
            .worker
            .as_ref()
            .unwrap()
            .cancel
            .load(std::sync::atomic::Ordering::Acquire)
    );
    assert!(manager.drain_thermal_dump_events().is_empty());

    assert!(manager.cancel_thermal_dump(&accepted));
    let result = event(&mut manager);
    assert_eq!(result.request, accepted);
    assert!(matches!(result.outcome, ThermalDumpOutcome::Cancelled));
    assert!(manager.thermal_dump_needs_reconnect("port"));
    assert!(manager.thermal_dump_busy("port"));
    assert!(manager.can_request_close("port"));
    manager.request_close("port").unwrap();
    assert!(manager.is_close_requested("port"));
    assert!(!manager.thermal_dump_needs_reconnect("port"));
    assert!(!manager.thermal_dump_busy("port"));
    stopped(&mut manager, "port");
    assert_eq!(stream.read(&mut [0; 1]).unwrap(), 0);
    assert_eq!(manager.session("port").unwrap().generation, generation);
    assert!(manager.drain_thermal_dump_events().is_empty());
    assert!(manager.can_forget("port"));
    assert!(manager.forget("port"));
}

#[test]
fn disconnect_handoff_and_generation_replacement_are_terminal_once() {
    for action in 0..4 {
        let (mut manager, listener) = fixture();
        manager.connect("port").unwrap();
        let mut stream = peer(&listener);
        connected(&mut manager, "port");
        let mut receipt = manager.send_line("port", "tcal dump").unwrap();
        read_command(&mut stream, b"tcal dump\n");
        assert!(completed(&mut receipt).is_ok());
        let accepted = request(&manager, "port");
        let reservation = match action {
            0 => {
                manager.disconnect("port").unwrap();
                None
            }
            1 => Some(manager.reserve("port", Some(accepted.generation)).unwrap()),
            2 => {
                manager
                    .entries
                    .get_mut(&port_key("port"))
                    .unwrap()
                    .view
                    .generation += 1;
                let _ = manager.poll();
                None
            }
            _ => {
                stream.shutdown(Shutdown::Both).unwrap();
                None
            }
        };
        let result = event(&mut manager);
        assert_eq!(result.request, accepted);
        assert!(matches!(
            (action, result.outcome),
            (0 | 3, ThermalDumpOutcome::Disconnected)
                | (1, ThermalDumpOutcome::Handoff)
                | (2, ThermalDumpOutcome::GenerationReplaced)
        ));
        stopped(&mut manager, "port");
        assert!(manager.writes_idle("port"));
        if let Some(reservation) = reservation {
            assert!(manager.reservation_ready(&reservation).unwrap());
            manager.release(&reservation).unwrap();
        }
        assert!(manager.drain_thermal_dump_events().is_empty());
        manager.connect("port").unwrap();
        let mut fresh = peer(&listener);
        connected(&mut manager, "port");
        let mut receipt = manager.send_line("port", "tcal dump").unwrap();
        read_command(&mut fresh, b"tcal dump\n");
        assert!(completed(&mut receipt).is_ok());
        let current = request(&manager, "port");
        assert!(current.request_id > accepted.request_id);
        assert_ne!(current.generation, accepted.generation);
        assert!(!manager.cancel_thermal_dump(&accepted));
        fresh.write_all(EMPTY).unwrap();
        let result = event(&mut manager);
        assert_eq!(result.request, current);
        assert!(matches!(result.outcome, ThermalDumpOutcome::Complete(_)));
    }
}

struct FailingWrite(TcpStream);
impl Read for FailingWrite {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.0.read(bytes)
    }
}
impl Write for FailingWrite {
    fn write(&mut self, _bytes: &[u8]) -> io::Result<usize> {
        Err(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "injected dump write failure",
        ))
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn failed_write_reports_acquisition_failure_independently_of_write_receipt() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let mut manager = SessionManager::with_factory(Arc::new(move |_| {
        Ok(Box::new(FailingWrite(socket_transport(address)?)) as Box<dyn Transport>)
    }));
    manager.connect("port").unwrap();
    let _stream = peer(&listener);
    connected(&mut manager, "port");
    let receipt = manager.send_line("port", "tcal dump").unwrap();
    drop(receipt);
    let result = event(&mut manager);
    assert_eq!(result.request.port, port_key("port"));
    assert_eq!(
        result.request.generation,
        manager.session("port").unwrap().generation
    );
    assert!(
        matches!(result.outcome, ThermalDumpOutcome::WriteFailed(error) if error.contains("injected dump write failure"))
    );
    stopped(&mut manager, "port");
    assert!(manager.active_thermal_dumps().is_empty());
    assert!(manager.writes_idle("port"));
    assert!(manager.drain_thermal_dump_events().is_empty());
}

#[test]
fn deadline_and_parse_failure_finish_independently_and_quarantine_old_tails() {
    let (mut manager, listener) = fixture();
    manager.thermal_dumps.lock().timeout = Duration::from_millis(90);
    manager.connect("port").unwrap();
    let mut stream = peer(&listener);
    connected(&mut manager, "port");
    let mut receipt = manager.send_line("port", "tcal dump").unwrap();
    read_command(&mut stream, b"tcal dump\n");
    assert!(completed(&mut receipt).is_ok());
    stream
        .write_all(b"Dumping 1 collected temperature calibration points:\n")
        .unwrap();
    assert!(matches!(
        event(&mut manager).outcome,
        ThermalDumpOutcome::TimedOut
    ));
    assert_eq!(
        manager.session("port").unwrap().state,
        ConnectionState::Connected
    );
    assert!(manager.thermal_dump_needs_reconnect("port"));
    stream.write_all(DUMP).unwrap();
    assert!(manager.send_line("port", "tcal dump").is_err());
    assert!(manager.drain_thermal_dump_events().is_empty());
    manager.disconnect("port").unwrap();
    stopped(&mut manager, "port");
    manager.connect("port").unwrap();
    let mut stream = peer(&listener);
    connected(&mut manager, "port");
    manager.thermal_dumps.lock().timeout = Duration::from_secs(2);
    let mut next = manager.send_line("port", "tcal dump").unwrap();
    read_command(&mut stream, b"tcal dump\n");
    assert!(completed(&mut next).is_ok());
    stream
        .write_all(b"Dumping 1 collected temperature calibration points:\n7 | bad | 0 | 0 | 0\n")
        .unwrap();
    assert!(matches!(
        event(&mut manager).outcome,
        ThermalDumpOutcome::Failed(_)
    ));
    assert!(manager.active_thermal_dumps().is_empty());
    assert!(manager.thermal_dump_needs_reconnect("port"));
    assert!(manager.send_line("port", "tcal dump").is_err());
}

#[test]
fn completed_result_retention_is_bounded_even_without_a_consumer() {
    let (mut manager, listener) = fixture();
    manager.connect("port").unwrap();
    let mut stream = peer(&listener);
    connected(&mut manager, "port");
    for _ in 0..40 {
        let mut receipt = manager.send_line("port", "tcal dump").unwrap();
        read_command(&mut stream, b"tcal dump\n");
        assert!(completed(&mut receipt).is_ok());
        stream.write_all(EMPTY).unwrap();
        until(|| manager.active_thermal_dumps().is_empty());
    }
    let results = manager.drain_thermal_dump_events();
    assert_eq!(results.len(), 32);
    assert_eq!(results[0].request.request_id, 9);
    assert_eq!(results[31].request.request_id, 40);
    assert!(results.iter().all(|event| matches!(&event.outcome, ThermalDumpOutcome::Complete(dump) if dump.points.is_empty())));
}

struct GatedRead {
    stream: TcpStream,
    ready: mpsc::Sender<()>,
    release: Option<mpsc::Receiver<()>>,
}

impl Read for GatedRead {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if let Some(release) = self.release.take() {
            self.ready.send(()).unwrap();
            release.recv_timeout(Duration::from_secs(2)).unwrap();
        }
        self.stream.read(bytes)
    }
}

impl Write for GatedRead {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.stream.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

#[test]
fn cancelling_before_worker_arm_skips_write_without_quarantining_port() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (ready_tx, ready_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release = Mutex::new(Some(release_rx));
    let mut manager = SessionManager::with_factory(Arc::new(move |_| {
        Ok(Box::new(GatedRead {
            stream: socket_transport(address)?,
            ready: ready_tx.clone(),
            release: release.lock().take(),
        }) as Box<dyn Transport>)
    }));
    manager.connect("port").unwrap();
    let mut stream = peer(&listener);
    connected(&mut manager, "port");
    ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let mut dump = manager.send_line("port", "tcal dump").unwrap();
    let accepted = request(&manager, "port");
    assert!(manager.cancel_thermal_dump(&accepted));
    assert!(!manager.thermal_dump_needs_reconnect("port"));
    let mut info = manager.send_line("port", "info").unwrap();
    release_tx.send(()).unwrap();
    read_command(&mut stream, b"info\n");
    assert!(completed(&mut dump).is_err());
    assert!(completed(&mut info).is_ok());
    until(|| manager.writes_idle("port"));
    let result = event(&mut manager);
    assert_eq!(result.request, accepted);
    assert!(matches!(result.outcome, ThermalDumpOutcome::Cancelled));
    stream
        .set_read_timeout(Some(Duration::from_millis(60)))
        .unwrap();
    assert!(
        matches!(stream.read(&mut [0]), Err(error) if matches!(error.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock))
    );
}

#[test]
fn complete_footer_count_mismatch_releases_response_for_ordinary_commands() {
    let (mut manager, listener) = fixture();
    manager.connect("port").unwrap();
    let mut stream = peer(&listener);
    connected(&mut manager, "port");
    let mut receipt = manager.send_line("port", "tcal dump").unwrap();
    read_command(&mut stream, b"tcal dump\n");
    assert!(completed(&mut receipt).is_ok());
    stream
        .write_all(b"Dumping 1 collected temperature calibration points:\nEnd of dump. Total points printed: 1\n")
        .unwrap();
    assert!(matches!(
        event(&mut manager).outcome,
        ThermalDumpOutcome::Failed(_)
    ));
    assert!(!manager.thermal_dump_needs_reconnect("port"));
    let mut info = manager.send_line("port", "info").unwrap();
    read_command(&mut stream, b"info\n");
    assert!(completed(&mut info).is_ok());
}

#[test]
fn generic_unavailable_without_attributable_dump_times_out_and_quarantines() {
    let (mut manager, listener) = fixture();
    manager.thermal_dumps.lock().timeout = Duration::from_millis(90);
    manager.connect("port").unwrap();
    let mut stream = peer(&listener);
    connected(&mut manager, "port");
    let mut receipt = manager.send_line("port", "tcal dump").unwrap();
    read_command(&mut stream, b"tcal dump\n");
    assert!(completed(&mut receipt).is_ok());
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let writer_stop = Arc::clone(&stop);
    let started = std::time::Instant::now();
    let mut writer_stream = stream.try_clone().unwrap();
    let writer = std::thread::spawn(move || {
        while started.elapsed() < Duration::from_secs(1)
            && !writer_stop.load(std::sync::atomic::Ordering::Acquire)
        {
            writer_stream
                .write_all(b"Command unavailable in this firmware\n")
                .unwrap();
            std::thread::sleep(Duration::from_millis(5));
        }
    });
    let result = event(&mut manager);
    let elapsed = started.elapsed();
    stop.store(true, std::sync::atomic::Ordering::Release);
    writer.join().unwrap();
    assert!(matches!(result.outcome, ThermalDumpOutcome::TimedOut));
    // Continuous generic noise cannot restart the original absolute deadline.
    assert!(elapsed < Duration::from_millis(500));
    assert!(manager.thermal_dump_needs_reconnect("port"));
    assert!(manager.send_line("port", "info").is_err());
    assert!(manager.send_line("port", "tcal dump").is_err());
    assert!(manager.drain_thermal_dump_events().is_empty());
}

/// The first read gates queue submission, then yields without receiving data.
/// The next read batches the exact peer response so both merged and one-byte
/// worker reads are deterministic rather than dependent on TCP packetization.
struct QueuedResponseTransport {
    stream: TcpStream,
    ready: mpsc::Sender<()>,
    release: Option<mpsc::Receiver<()>>,
    response_len: Option<usize>,
    response: io::Cursor<Vec<u8>>,
    fragment: usize,
}

impl Read for QueuedResponseTransport {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if let Some(release) = self.release.take() {
            self.ready.send(()).unwrap();
            release.recv_timeout(Duration::from_secs(2)).unwrap();
            return Err(io::ErrorKind::TimedOut.into());
        }
        if let Some(length) = self.response_len.take() {
            let mut response = vec![0; length];
            self.stream.set_read_timeout(Some(Duration::from_secs(2)))?;
            let result = self.stream.read_exact(&mut response);
            self.stream
                .set_read_timeout(Some(super::super::IO_TIMEOUT))?;
            result?;
            self.response = io::Cursor::new(response);
        }
        if self.response.position() < self.response.get_ref().len() as u64 {
            let length = bytes.len().min(self.fragment);
            return self.response.read(&mut bytes[..length]);
        }
        self.stream.read(bytes)
    }
}

impl Write for QueuedResponseTransport {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.stream.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

#[test]
fn queued_ordinary_error_cannot_complete_or_steal_following_dump() {
    for fragment in [CHUNK_BYTES, 1] {
        let mut response = b"\x1b[31mCommand unavailable in this firmware\x1b[0m\r\n".to_vec();
        response.extend_from_slice(DUMP);
        let response_len = response.len();
        assert!(response_len < CHUNK_BYTES);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (ready_tx, ready_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release = Mutex::new(Some(release_rx));
        let mut manager = SessionManager::with_factory(Arc::new(move |_| {
            Ok(Box::new(QueuedResponseTransport {
                stream: socket_transport(address)?,
                ready: ready_tx.clone(),
                release: release.lock().take(),
                response_len: Some(response_len),
                response: io::Cursor::new(Vec::new()),
                fragment,
            }) as Box<dyn Transport>)
        }));
        manager.connect("port").unwrap();
        let mut stream = peer(&listener);
        connected(&mut manager, "port");
        ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let mut ordinary = manager.send_line("port", "sens 0,0,0").unwrap();
        let mut dump = manager.send_line("port", "tcal dump").unwrap();
        let accepted = request(&manager, "port");
        release_tx.send(()).unwrap();
        // Both real worker writes are observed before either response is sent.
        read_command(&mut stream, b"sens 0,0,0\ntcal dump\n");
        assert!(completed(&mut ordinary).is_ok());
        assert!(completed(&mut dump).is_ok());
        stream.write_all(&response).unwrap();
        let result = event(&mut manager);
        assert_eq!(result.request, accepted);
        assert!(matches!(
            result.outcome,
            ThermalDumpOutcome::Complete(dump) if dump.points.len() == 1
        ));
        assert!(!manager.thermal_dump_needs_reconnect("port"));
        assert!(manager.drain_thermal_dump_events().is_empty());
        let mut retry = manager.send_line("port", "tcal dump").unwrap();
        read_command(&mut stream, b"tcal dump\n");
        assert!(completed(&mut retry).is_ok());
        let next = request(&manager, "port");
        assert_ne!(next.request_id, accepted.request_id);
        stream.write_all(EMPTY).unwrap();
        let result = event(&mut manager);
        assert_eq!(result.request, next);
        assert!(matches!(
            result.outcome,
            ThermalDumpOutcome::Complete(dump) if dump.points.is_empty()
        ));
        assert!(manager.drain_thermal_dump_events().is_empty());
    }
}

#[test]
fn generic_error_after_dump_header_cannot_release_an_unfinished_response() {
    let (mut manager, listener) = fixture();
    manager.connect("port").unwrap();
    let mut stream = peer(&listener);
    connected(&mut manager, "port");
    let mut receipt = manager.send_line("port", "tcal dump").unwrap();
    read_command(&mut stream, b"tcal dump\n");
    assert!(completed(&mut receipt).is_ok());
    let accepted = request(&manager, "port");
    stream
        .write_all(b"Dumping 1 collected temperature calibration points:\nCommand unavailable in this firmware\n")
        .unwrap();
    let result = event(&mut manager);
    assert_eq!(result.request, accepted);
    assert!(matches!(
        result.outcome,
        ThermalDumpOutcome::Failed(slimenrf_recording::thermal_dump::ThermalDumpError::Unsupported)
    ));
    assert!(manager.thermal_dump_needs_reconnect("port"));
    assert!(manager.send_line("port", "info").is_err());
    assert!(manager.send_line("port", "tcal dump").is_err());
    stream.write_all(DUMP).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_millis(60)))
        .unwrap();
    assert!(
        matches!(stream.read(&mut [0]), Err(error) if matches!(error.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock))
    );
    assert!(manager.drain_thermal_dump_events().is_empty());
}
