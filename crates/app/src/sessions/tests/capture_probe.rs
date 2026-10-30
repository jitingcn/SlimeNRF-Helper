use super::super::native::Transport;
use super::super::{
    CaptureConsoleProbe, ConnectionState, SessionManager, probe_reserved_capture_console,
};
use super::{completed, connected, fixture, peer, socket_transport, stopped, until};
use crate::domain::{
    DeviceRole, InterfaceDescription, Inventory, SerialEndpoint, SerialPortKind, UsbInterface,
};
use parking_lot::Mutex;
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

const OLD_HELP: &str = "\n=== Available Commands ===\nOther:\n  help Show this help\n\n";

fn endpoint() -> SerialEndpoint {
    SerialEndpoint {
        port_name: "fixture-console".into(),
        label: "Fixture receiver console".into(),
        manufacturer: None,
        product: None,
        kind: SerialPortKind::Usb,
        serial_number: Some("FIXTURE-RECEIVER".into()),
        vid: Some(0x1209),
        pid: Some(0x7690),
        usb_interface: Some(UsbInterface {
            number: Some(0),
            description: InterfaceDescription::Unavailable,
        }),
    }
}

fn inventory(endpoint: &SerialEndpoint) -> Inventory {
    Inventory {
        ports: vec![endpoint.clone()],
        ..Inventory::default()
    }
}

fn connect_fixture() -> (SessionManager, SerialEndpoint, TcpStream) {
    let endpoint = endpoint();
    let (mut manager, listener) = fixture();
    manager.update_inventory(&inventory(&endpoint), Instant::now());
    manager.connect(&endpoint.port_name).unwrap();
    let peer = peer(&listener);
    connected(&mut manager, &endpoint.port_name);
    (manager, endpoint, peer)
}

fn read_command(peer: &mut TcpStream) -> String {
    let mut bytes = Vec::new();
    loop {
        let mut byte = [0];
        peer.read_exact(&mut byte).unwrap();
        if byte[0] == b'\n' {
            break;
        }
        bytes.push(byte[0]);
    }
    let command = String::from_utf8(bytes).unwrap();
    assert!(
        command.starts_with("help h"),
        "unexpected command: {command}"
    );
    command
}

fn start_probe(
    probe: CaptureConsoleProbe,
) -> (mpsc::Receiver<Result<(), String>>, thread::JoinHandle<()>) {
    let (done, result) = mpsc::channel();
    let waiter = thread::spawn(move || done.send(probe.wait(&AtomicBool::new(false))).unwrap());
    (result, waiter)
}

fn finish_probe(
    result: mpsc::Receiver<Result<(), String>>,
    waiter: thread::JoinHandle<()>,
) -> Result<(), String> {
    let result = result.recv_timeout(Duration::from_secs(2)).unwrap();
    waiter.join().unwrap();
    result
}

#[test]
fn admission_defers_help_until_wait_and_retains_the_console_slot() {
    let (mut manager, endpoint, mut peer) = connect_fixture();
    let port = &endpoint.port_name;
    let generation = manager.session(port).unwrap().generation;
    let probe = manager.begin_capture_console_probe(&endpoint).unwrap();
    assert!(!manager.writes_idle(port));
    assert!(manager.send_line(port, "info").is_err());
    assert!(manager.reserve(port, Some(generation)).is_err());
    assert!(manager.request_close(port).is_err());
    assert!(manager.begin_capture_console_probe(&endpoint).is_err());
    peer.set_read_timeout(Some(Duration::from_millis(80)))
        .unwrap();
    let mut byte = [0];
    assert!(matches!(
        peer.read(&mut byte).unwrap_err().kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    ));
    drop(probe);
    assert!(manager.writes_idle(port));
    let mut receipt = manager.send_line(port, "info").unwrap();
    completed(&mut receipt).unwrap();
    let mut command = [0; 5];
    peer.read_exact(&mut command).unwrap();
    assert_eq!(&command, b"info\n");
    manager.shutdown().unwrap().join().unwrap();
}

#[test]
fn live_owner_retains_connection_transcript_identity_and_write_admission() {
    let (mut manager, endpoint, mut peer) = connect_fixture();
    let port = &endpoint.port_name;
    let generation = manager.session(port).unwrap().generation;
    peer.write_all(b"* SlimeVR Receiver *\nDevice address: 112233445566\n")
        .unwrap();
    // A fully consumed old help response must never satisfy the new probe.
    peer.write_all(OLD_HELP.as_bytes()).unwrap();
    until(|| {
        let _ = manager.poll();
        manager
            .session(port)
            .unwrap()
            .terminal
            .text()
            .contains("Show this help")
    });
    let probe = manager.begin_capture_console_probe(&endpoint).unwrap();
    assert!(!manager.writes_idle(port));
    assert!(manager.send_line(port, "info").is_err());
    assert!(manager.reserve(port, Some(generation)).is_err());
    let (result, waiter) = start_probe(probe);
    let command = read_command(&mut peer);
    assert!(result.recv_timeout(Duration::from_millis(80)).is_err());
    let response = format!("Board: Fixture Board\r\n{command}\r\n{OLD_HELP}");
    for chunk in response.as_bytes().chunks(19) {
        peer.write_all(chunk).unwrap();
    }
    finish_probe(result, waiter).unwrap();
    until(|| {
        let _ = manager.poll();
        let session = manager.session(port).unwrap();
        session.identity.board.as_deref() == Some("Fixture Board")
            && session
                .terminal
                .text()
                .matches("=== Available Commands ===")
                .count()
                >= 2
    });
    let session = manager.session(port).unwrap();
    assert_eq!(session.state, ConnectionState::Connected);
    assert_eq!(session.generation, generation);
    assert_eq!(session.identity.role, DeviceRole::Receiver);
    assert_eq!(session.identity.address.as_deref(), Some("112233445566"));
    let text = session.terminal.text();
    assert!(text.contains(&command));
    assert!(text.contains("Board: Fixture Board"));
    assert!(text.matches("=== Available Commands ===").count() >= 2);
    assert!(manager.writes_idle(port));
    let mut ordinary = manager.send_line(port, "info").unwrap();
    completed(&mut ordinary).unwrap();
    let mut written = [0; 5];
    peer.read_exact(&mut written).unwrap();
    assert_eq!(&written, b"info\n");
    manager.shutdown().unwrap().join().unwrap();
}

#[test]
fn live_worker_retries_a_discarded_pre_activation_command() {
    let (mut manager, endpoint, mut peer) = connect_fixture();
    let probe = manager.begin_capture_console_probe(&endpoint).unwrap();
    let (result, waiter) = start_probe(probe);
    let discarded = read_command(&mut peer);
    let retried = read_command(&mut peer);
    assert_eq!(discarded, retried);
    peer.write_all(format!("{retried}\n").as_bytes()).unwrap();
    finish_probe(result, waiter).unwrap();
    let _ = manager.poll();
    assert_eq!(
        manager.session(&endpoint.port_name).unwrap().state,
        ConnectionState::Connected
    );
    manager.shutdown().unwrap().join().unwrap();
}

#[test]
fn uncorrelated_old_help_alone_never_completes_probe_and_is_forwarded() {
    let (mut manager, endpoint, mut peer) = connect_fixture();
    let port = &endpoint.port_name;
    let probe = manager.begin_capture_console_probe(&endpoint).unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let waiter_cancel = Arc::clone(&cancel);
    let (done, result) = mpsc::channel();
    let waiter = thread::spawn(move || done.send(probe.wait(&waiter_cancel)).unwrap());
    let _command = read_command(&mut peer);
    peer.write_all(OLD_HELP.as_bytes()).unwrap();
    until(|| {
        let _ = manager.poll();
        manager
            .session(port)
            .unwrap()
            .terminal
            .text()
            .contains("Show this help")
    });
    assert!(matches!(result.try_recv(), Err(mpsc::TryRecvError::Empty)));
    cancel.store(true, Ordering::Release);
    assert!(
        result
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .is_err()
    );
    waiter.join().unwrap();
    assert_eq!(
        manager.session(port).unwrap().state,
        ConnectionState::Connected
    );
    let next = manager.begin_capture_console_probe(&endpoint).unwrap();
    drop(next);
    until(|| manager.writes_idle(port));
    manager.shutdown().unwrap().join().unwrap();
}

#[test]
fn fresh_echo_may_arrive_after_complete_help() {
    let (mut manager, endpoint, mut peer) = connect_fixture();
    let probe = manager.begin_capture_console_probe(&endpoint).unwrap();
    let (result, waiter) = start_probe(probe);
    let command = read_command(&mut peer);
    peer.write_all(OLD_HELP.as_bytes()).unwrap();
    until(|| {
        let _ = manager.poll();
        manager
            .session(&endpoint.port_name)
            .unwrap()
            .terminal
            .text()
            .contains("Show this help")
    });
    peer.write_all(format!("{command}\n").as_bytes()).unwrap();
    finish_probe(result, waiter).unwrap();
    let _ = manager.poll();
    assert_eq!(
        manager.session(&endpoint.port_name).unwrap().state,
        ConnectionState::Connected
    );
    manager.shutdown().unwrap().join().unwrap();
}

#[test]
fn partial_prior_line_fails_without_sending_any_probe_command() {
    let (mut manager, endpoint, mut peer) = connect_fixture();
    let port = &endpoint.port_name;
    peer.write_all(b"unfinished prior output").unwrap();
    until(|| {
        let _ = manager.poll();
        manager
            .session(port)
            .unwrap()
            .terminal
            .text()
            .contains("unfinished prior output")
    });
    let probe = manager.begin_capture_console_probe(&endpoint).unwrap();
    assert!(probe.wait(&AtomicBool::new(false)).is_err());
    assert_eq!(
        manager.session(port).unwrap().state,
        ConnectionState::Connected
    );
    peer.set_read_timeout(Some(Duration::from_millis(80)))
        .unwrap();
    let mut byte = [0];
    assert!(matches!(
        peer.read(&mut byte).unwrap_err().kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    ));
    manager.shutdown().unwrap().join().unwrap();
}

#[test]
fn cancellation_waits_for_owner_and_preserves_terminal_connection() {
    let (mut manager, endpoint, mut peer) = connect_fixture();
    let port = &endpoint.port_name;
    let probe = manager.begin_capture_console_probe(&endpoint).unwrap();
    let cancel = probe.cancel_token();
    let (result, waiter) = start_probe(probe);
    let _command = read_command(&mut peer);
    peer.write_all(b"\n=== Available Commands ===\n").unwrap();
    cancel.store(true, Ordering::Release);
    assert!(finish_probe(result, waiter).is_err());
    assert!(manager.writes_idle(port));
    let _ = manager.poll();
    assert_eq!(
        manager.session(port).unwrap().state,
        ConnectionState::Connected
    );
    let mut receipt = manager.send_line(port, "info").unwrap();
    completed(&mut receipt).unwrap();
    let mut bytes = [0; 5];
    peer.read_exact(&mut bytes).unwrap();
    assert_eq!(&bytes, b"info\n");
    manager.shutdown().unwrap().join().unwrap();
}

#[test]
fn peer_disconnect_never_resolves_probe_as_ready() {
    let (mut manager, endpoint, mut peer) = connect_fixture();
    let probe = manager.begin_capture_console_probe(&endpoint).unwrap();
    let (result, waiter) = start_probe(probe);
    let _command = read_command(&mut peer);
    peer.write_all(b"\n=== Available Commands ===\n").unwrap();
    drop(peer);
    assert!(finish_probe(result, waiter).is_err());
    stopped(&mut manager, &endpoint.port_name);
    assert_eq!(
        manager.session(&endpoint.port_name).unwrap().state,
        ConnectionState::Faulted
    );
    assert!(manager.shutdown().is_none());
}

#[test]
fn identity_ambiguity_replacement_and_generation_change_reject_probe() {
    let (mut manager, endpoint, _peer) = connect_fixture();
    for other in [
        SerialEndpoint {
            serial_number: None,
            ..endpoint.clone()
        },
        SerialEndpoint {
            serial_number: Some("OTHER".into()),
            ..endpoint.clone()
        },
        SerialEndpoint {
            vid: None,
            ..endpoint.clone()
        },
        SerialEndpoint {
            kind: SerialPortKind::Unknown,
            ..endpoint.clone()
        },
        SerialEndpoint {
            usb_interface: Some(UsbInterface {
                number: Some(0),
                description: InterfaceDescription::Ambiguous,
            }),
            ..endpoint.clone()
        },
    ] {
        assert!(manager.begin_capture_console_probe(&other).is_err());
    }
    let key = super::super::port_key(&endpoint.port_name);
    manager.entries.get_mut(&key).unwrap().view.generation += 1;
    assert!(manager.begin_capture_console_probe(&endpoint).is_err());
    manager.entries.get_mut(&key).unwrap().view.generation -= 1;
    let replacement = SerialEndpoint {
        serial_number: Some("REPLACEMENT".into()),
        ..endpoint.clone()
    };
    manager.update_inventory(&inventory(&replacement), Instant::now());
    assert!(manager.begin_capture_console_probe(&endpoint).is_err());
    assert!(manager.begin_capture_console_probe(&replacement).is_err());
    manager.shutdown().unwrap().join().unwrap();
}

#[test]
fn inventory_loss_or_replacement_cancels_a_probe_without_closing_owner() {
    for replacement in [false, true] {
        let (mut manager, endpoint, mut peer) = connect_fixture();
        let probe = manager.begin_capture_console_probe(&endpoint).unwrap();
        let (result, waiter) = start_probe(probe);
        let command = read_command(&mut peer);
        if replacement {
            let other = SerialEndpoint {
                serial_number: Some("REPLACEMENT".into()),
                ..endpoint.clone()
            };
            manager.update_inventory(&inventory(&other), Instant::now());
        } else {
            manager.invalidate_inventory();
        }
        peer.write_all(format!("{command}\n").as_bytes()).unwrap();
        assert!(finish_probe(result, waiter).is_err());
        let _ = manager.poll();
        assert_eq!(
            manager.session(&endpoint.port_name).unwrap().state,
            ConnectionState::Connected
        );
        manager.shutdown().unwrap().join().unwrap();
    }
}

fn completed_unconsumed_probe(
    manager: &mut SessionManager,
    endpoint: &SerialEndpoint,
    peer: &mut TcpStream,
) -> (CaptureConsoleProbe, Result<(), String>) {
    let probe = manager.begin_capture_console_probe(endpoint).unwrap();
    let completion = probe.enqueue(&AtomicBool::new(false)).unwrap();
    let command = read_command(peer);
    peer.write_all(format!("{command}\n").as_bytes()).unwrap();
    let result = completion.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(result.is_ok());
    assert!(!manager.writes_idle(&endpoint.port_name));
    (probe, result)
}

#[test]
fn unconsumed_success_is_revoked_by_inventory_change_and_stop() {
    for stop in [false, true] {
        let (mut manager, endpoint, mut peer) = connect_fixture();
        let (probe, result) = completed_unconsumed_probe(&mut manager, &endpoint, &mut peer);
        assert!(manager.send_line(&endpoint.port_name, "info").is_err());
        assert!(manager.reserve(&endpoint.port_name, None).is_err());
        if stop {
            probe.cancel_token().store(true, Ordering::Release);
        } else {
            manager.invalidate_inventory();
        }
        assert!(
            probe
                .finish_result(result, &AtomicBool::new(false))
                .is_err()
        );
        drop(probe);
        assert!(manager.writes_idle(&endpoint.port_name));
        assert_eq!(
            manager.session(&endpoint.port_name).unwrap().state,
            ConnectionState::Connected
        );
        manager.shutdown().unwrap().join().unwrap();
    }
}

#[test]
fn unconsumed_success_is_revoked_when_its_worker_exits() {
    let (mut manager, endpoint, mut peer) = connect_fixture();
    let (probe, result) = completed_unconsumed_probe(&mut manager, &endpoint, &mut peer);
    drop(peer);
    stopped(&mut manager, &endpoint.port_name);
    assert!(!manager.writes_idle(&endpoint.port_name));
    assert!(
        probe
            .finish_result(result, &AtomicBool::new(false))
            .is_err()
    );
    drop(probe);
    assert!(manager.writes_idle(&endpoint.port_name));
    assert!(manager.shutdown().is_none());
}

#[test]
fn unconsumed_success_is_revoked_by_generation_change() {
    let (mut manager, endpoint, mut peer) = connect_fixture();
    let (probe, result) = completed_unconsumed_probe(&mut manager, &endpoint, &mut peer);
    let key = super::super::port_key(&endpoint.port_name);
    manager.entries.get_mut(&key).unwrap().view.generation += 1;
    let _ = manager.poll();
    assert!(
        probe
            .finish_result(result, &AtomicBool::new(false))
            .is_err()
    );
    drop(probe);
    assert!(manager.writes_idle(&endpoint.port_name));
    if let Some(reaper) = manager.shutdown() {
        reaper.join().unwrap();
    }
}

#[test]
fn cancelling_an_active_probe_leaves_the_same_worker_available() {
    let (mut manager, endpoint, mut peer) = connect_fixture();
    let generation = manager.session(&endpoint.port_name).unwrap().generation;
    let probe = manager.begin_capture_console_probe(&endpoint).unwrap();
    let cancel = probe.cancel_token();
    let (result, waiter) = start_probe(probe);
    let _command = read_command(&mut peer);
    cancel.store(true, Ordering::Release);
    assert!(finish_probe(result, waiter).is_err());
    until(|| manager.writes_idle(&endpoint.port_name));
    let _ = manager.poll();
    let session = manager.session(&endpoint.port_name).unwrap();
    assert_eq!(session.state, ConnectionState::Connected);
    assert_eq!(session.generation, generation);
    let mut receipt = manager.send_line(&endpoint.port_name, "info").unwrap();
    completed(&mut receipt).unwrap();
    let mut bytes = [0; 5];
    peer.read_exact(&mut bytes).unwrap();
    assert_eq!(&bytes, b"info\n");
    manager.shutdown().unwrap().join().unwrap();
}

struct FragmentedPriorRead {
    stream: TcpStream,
    entered: mpsc::Sender<usize>,
    release: mpsc::Receiver<()>,
    reads: usize,
}

impl Read for FragmentedPriorRead {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let index = self.reads;
        self.reads += 1;
        if index < 2 {
            self.entered.send(index).unwrap();
            self.release.recv_timeout(Duration::from_secs(2)).unwrap();
            if index == 1 {
                return Err(io::ErrorKind::TimedOut.into());
            }
        }
        self.stream.read(bytes)
    }
}

impl Write for FragmentedPriorRead {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.stream.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

#[test]
fn worker_drains_and_displays_remainder_of_a_prior_fragment() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release = Mutex::new(Some(release_rx));
    let mut manager = SessionManager::with_factory(Arc::new(move |_| {
        Ok(Box::new(FragmentedPriorRead {
            stream: socket_transport(address)?,
            entered: entered_tx.clone(),
            release: release.lock().take().unwrap(),
            reads: 0,
        }) as Box<dyn Transport>)
    }));
    let endpoint = endpoint();
    manager.update_inventory(&inventory(&endpoint), Instant::now());
    manager.connect(&endpoint.port_name).unwrap();
    let mut peer = peer(&listener);
    assert_eq!(entered_rx.recv_timeout(Duration::from_secs(2)).unwrap(), 0);
    peer.write_all(b"previous fragment").unwrap();
    release_tx.send(()).unwrap();
    assert_eq!(entered_rx.recv_timeout(Duration::from_secs(2)).unwrap(), 1);
    connected(&mut manager, &endpoint.port_name);
    let probe = manager.begin_capture_console_probe(&endpoint).unwrap();
    let (result, waiter) = start_probe(probe);
    peer.write_all(b" finished\n").unwrap();
    release_tx.send(()).unwrap();
    let command = read_command(&mut peer);
    peer.write_all(format!("{command}\n").as_bytes()).unwrap();
    finish_probe(result, waiter).unwrap();
    until(|| {
        let _ = manager.poll();
        manager
            .session(&endpoint.port_name)
            .unwrap()
            .terminal
            .text()
            .contains("previous fragment finished")
    });
    assert_eq!(
        manager.session(&endpoint.port_name).unwrap().state,
        ConnectionState::Connected
    );
    manager.shutdown().unwrap().join().unwrap();
}

struct PartialProbeWrite {
    stream: TcpStream,
    entered: mpsc::Sender<()>,
    release: Option<mpsc::Receiver<()>>,
    fail_after_prefix: bool,
}

impl Read for PartialProbeWrite {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.stream.read(bytes)
    }
}

impl Write for PartialProbeWrite {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if let Some(release) = self.release.take() {
            let count = bytes.len().min(5);
            self.stream.write_all(&bytes[..count])?;
            self.entered.send(()).unwrap();
            release.recv_timeout(Duration::from_secs(2)).unwrap();
            return Ok(count);
        }
        if self.fail_after_prefix {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "injected partial write failure",
            ));
        }
        self.stream.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

#[test]
fn cancellation_after_partial_help_finishes_frame_or_faults_on_real_write_error() {
    for fail_after_prefix in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release = Mutex::new(Some(release_rx));
        let mut manager = SessionManager::with_factory(Arc::new(move |_| {
            Ok(Box::new(PartialProbeWrite {
                stream: socket_transport(address)?,
                entered: entered_tx.clone(),
                release: release.lock().take(),
                fail_after_prefix,
            }) as Box<dyn Transport>)
        }));
        let endpoint = endpoint();
        manager.update_inventory(&inventory(&endpoint), Instant::now());
        manager.connect(&endpoint.port_name).unwrap();
        let mut peer = peer(&listener);
        connected(&mut manager, &endpoint.port_name);
        let probe = manager.begin_capture_console_probe(&endpoint).unwrap();
        let cancel = probe.cancel_token();
        let (result, waiter) = start_probe(probe);
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        cancel.store(true, Ordering::Release);
        assert!(matches!(result.try_recv(), Err(mpsc::TryRecvError::Empty)));
        assert!(manager.send_line(&endpoint.port_name, "info").is_err());
        release_tx.send(()).unwrap();
        assert!(finish_probe(result, waiter).is_err());
        if fail_after_prefix {
            stopped(&mut manager, &endpoint.port_name);
            assert_eq!(
                manager.session(&endpoint.port_name).unwrap().state,
                ConnectionState::Faulted
            );
            let mut prefix = [0; 5];
            peer.read_exact(&mut prefix).unwrap();
            assert_eq!(&prefix, b"help ");
            assert!(manager.shutdown().is_none());
        } else {
            let command = read_command(&mut peer);
            assert!(command.starts_with("help h"));
            let mut receipt = manager.send_line(&endpoint.port_name, "info").unwrap();
            completed(&mut receipt).unwrap();
            let mut next = [0; 5];
            peer.read_exact(&mut next).unwrap();
            assert_eq!(&next, b"info\n");
            assert_eq!(
                manager.session(&endpoint.port_name).unwrap().state,
                ConnectionState::Connected
            );
            manager.shutdown().unwrap().join().unwrap();
        }
    }
}

struct GatedWrite {
    stream: TcpStream,
    entered: mpsc::Sender<()>,
    release: Option<mpsc::Receiver<()>>,
}

impl Read for GatedWrite {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.stream.read(bytes)
    }
}

impl Write for GatedWrite {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if let Some(release) = self.release.take() {
            self.entered.send(()).unwrap();
            release.recv_timeout(Duration::from_secs(2)).unwrap();
        }
        self.stream.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

#[test]
fn outstanding_terminal_write_rejects_probe_without_stealing_owner() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release = Mutex::new(Some(release_rx));
    let mut manager = SessionManager::with_factory(Arc::new(move |_| {
        Ok(Box::new(GatedWrite {
            stream: socket_transport(address)?,
            entered: entered_tx.clone(),
            release: release.lock().take(),
        }) as Box<dyn Transport>)
    }));
    let endpoint = endpoint();
    manager.update_inventory(&inventory(&endpoint), Instant::now());
    manager.connect(&endpoint.port_name).unwrap();
    let mut peer = peer(&listener);
    connected(&mut manager, &endpoint.port_name);
    let mut receipt = manager.send_line(&endpoint.port_name, "info").unwrap();
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(manager.begin_capture_console_probe(&endpoint).is_err());
    assert_eq!(
        manager.session(&endpoint.port_name).unwrap().state,
        ConnectionState::Connected
    );
    release_tx.send(()).unwrap();
    completed(&mut receipt).unwrap();
    let mut bytes = [0; 5];
    peer.read_exact(&mut bytes).unwrap();
    assert_eq!(&bytes, b"info\n");
    manager.shutdown().unwrap().join().unwrap();
}

struct GatedRead {
    entered: mpsc::Sender<()>,
    release: mpsc::Receiver<()>,
    writes: Arc<AtomicUsize>,
}

impl Read for GatedRead {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        self.entered.send(()).unwrap();
        self.release.recv_timeout(Duration::from_secs(2)).unwrap();
        Err(io::ErrorKind::TimedOut.into())
    }
}

impl Write for GatedRead {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn dropping_a_queued_probe_cancels_it_without_disconnecting_owner() {
    let endpoint = endpoint();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release = Mutex::new(Some(release_rx));
    let writes = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&writes);
    let mut manager = SessionManager::with_factory(Arc::new(move |_| {
        Ok(Box::new(GatedRead {
            entered: entered_tx.clone(),
            release: release.lock().take().unwrap(),
            writes: Arc::clone(&count),
        }) as Box<dyn Transport>)
    }));
    manager.update_inventory(&inventory(&endpoint), Instant::now());
    manager.connect(&endpoint.port_name).unwrap();
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    connected(&mut manager, &endpoint.port_name);
    let probe = manager.begin_capture_console_probe(&endpoint).unwrap();
    let completion = probe.enqueue(&AtomicBool::new(false)).unwrap();
    assert!(!manager.writes_idle(&endpoint.port_name));
    drop(probe);
    assert_eq!(
        manager.session(&endpoint.port_name).unwrap().state,
        ConnectionState::Connected
    );
    assert!(!manager.writes_idle(&endpoint.port_name));
    release_tx.send(()).unwrap();
    assert!(
        completion
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .is_err()
    );
    assert!(manager.writes_idle(&endpoint.port_name));
    assert_eq!(writes.load(Ordering::SeqCst), 0);
    assert_eq!(
        manager.session(&endpoint.port_name).unwrap().state,
        ConnectionState::Connected
    );
    let mut receipt = manager.send_line(&endpoint.port_name, "info").unwrap();
    release_tx.send(()).unwrap();
    completed(&mut receipt).unwrap();
    assert_eq!(writes.load(Ordering::SeqCst), 1);
    manager.disconnect(&endpoint.port_name).unwrap();
    let _ = release_tx.send(());
    stopped(&mut manager, &endpoint.port_name);
    assert!(manager.shutdown().is_none());
}

#[test]
fn cancelled_queued_probe_retains_ownership_until_the_serial_worker_finishes() {
    let endpoint = endpoint();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release = Mutex::new(Some(release_rx));
    let writes = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&writes);
    let mut manager = SessionManager::with_factory(Arc::new(move |_| {
        Ok(Box::new(GatedRead {
            entered: entered_tx.clone(),
            release: release.lock().take().unwrap(),
            writes: Arc::clone(&count),
        }) as Box<dyn Transport>)
    }));
    manager.update_inventory(&inventory(&endpoint), Instant::now());
    manager.connect(&endpoint.port_name).unwrap();
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    connected(&mut manager, &endpoint.port_name);
    let probe = manager.begin_capture_console_probe(&endpoint).unwrap();
    let completion = probe.enqueue(&AtomicBool::new(false)).unwrap();
    probe.cancel_token().store(true, Ordering::Release);
    let (done, result) = mpsc::channel();
    let waiter = thread::spawn(move || {
        let outcome = completion.recv_timeout(Duration::from_secs(2)).unwrap();
        done.send(probe.finish_result(outcome, &AtomicBool::new(false)))
            .unwrap();
    });
    assert!(result.recv_timeout(Duration::from_millis(80)).is_err());
    assert!(!manager.writes_idle(&endpoint.port_name));
    assert!(manager.send_line(&endpoint.port_name, "info").is_err());
    release_tx.send(()).unwrap();
    assert!(
        result
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .is_err()
    );
    waiter.join().unwrap();
    assert!(manager.writes_idle(&endpoint.port_name));
    assert_eq!(writes.load(Ordering::SeqCst), 0);
    assert_eq!(
        manager.session(&endpoint.port_name).unwrap().state,
        ConnectionState::Connected
    );
    manager.disconnect(&endpoint.port_name).unwrap();
    let _ = release_tx.send(());
    stopped(&mut manager, &endpoint.port_name);
    assert!(manager.shutdown().is_none());
}

#[test]
fn cancellation_before_wait_sends_nothing_and_keeps_owner_connected() {
    let endpoint = endpoint();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release = Mutex::new(Some(release_rx));
    let writes = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&writes);
    let mut manager = SessionManager::with_factory(Arc::new(move |_| {
        Ok(Box::new(GatedRead {
            entered: entered_tx.clone(),
            release: release.lock().take().unwrap(),
            writes: Arc::clone(&count),
        }) as Box<dyn Transport>)
    }));
    manager.update_inventory(&inventory(&endpoint), Instant::now());
    manager.connect(&endpoint.port_name).unwrap();
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    connected(&mut manager, &endpoint.port_name);
    let probe = manager.begin_capture_console_probe(&endpoint).unwrap();
    assert!(probe.wait(&AtomicBool::new(true)).is_err());
    let next = manager.begin_capture_console_probe(&endpoint).unwrap();
    next.cancel_token().store(true, Ordering::Release);
    assert!(next.wait(&AtomicBool::new(false)).is_err());
    assert_eq!(
        manager.session(&endpoint.port_name).unwrap().state,
        ConnectionState::Connected
    );
    assert!(manager.writes_idle(&endpoint.port_name));
    assert_eq!(writes.load(Ordering::SeqCst), 0);
    manager.disconnect(&endpoint.port_name).unwrap();
    // The worker may already be inside its next gated native read.
    let _ = release_tx.send(());
    stopped(&mut manager, &endpoint.port_name);
    assert!(manager.shutdown().is_none());
}

struct AutoConsole {
    response: Vec<u8>,
    offset: usize,
    dropped: Arc<AtomicUsize>,
    cancel_on_write: Option<Arc<AtomicBool>>,
}

impl Read for AutoConsole {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if self.offset == self.response.len() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        let count = bytes.len().min(17).min(self.response.len() - self.offset);
        bytes[..count].copy_from_slice(&self.response[self.offset..self.offset + count]);
        self.offset += count;
        Ok(count)
    }
}

impl Write for AutoConsole {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.response.extend_from_slice(bytes);
        if let Some(cancel) = &self.cancel_on_write {
            cancel.store(true, Ordering::Release);
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for AutoConsole {
    fn drop(&mut self) {
        self.dropped.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn temporary_console_is_closed_before_success_or_cancellation_returns() {
    for cancel_during_write in [false, true] {
        let dropped = Arc::new(AtomicUsize::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let mut manager = SessionManager::with_factory({
            let dropped = Arc::clone(&dropped);
            let cancel = Arc::clone(&cancel);
            Arc::new(move |_| {
                Ok(Box::new(AutoConsole {
                    response: Vec::new(),
                    offset: 0,
                    dropped: Arc::clone(&dropped),
                    cancel_on_write: cancel_during_write.then(|| Arc::clone(&cancel)),
                }) as Box<dyn Transport>)
            })
        });
        let reservation = manager.reserve("temporary", None).unwrap();
        let source = manager.reserved_console_source(&reservation).unwrap();
        let result = probe_reserved_capture_console(source, &cancel);
        assert_eq!(result.is_ok(), !cancel_during_write);
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
        manager.release(&reservation).unwrap();
    }
}
