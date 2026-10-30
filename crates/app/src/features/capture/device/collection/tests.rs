use super::super::io::{CaptureIo, ConsoleInitialization};
use super::super::{
    CaptureMode, CaptureOutcome, CapturePhase, CaptureRequest, CaptureSnapshot, CaptureSource,
    CaptureTransport,
};
use super::{collect, expected_roster};
use crate::domain::{
    InterfaceDescription, ReceiverEndpoint, SerialEndpoint, SerialPortKind, UsbInterface,
};
use parking_lot::Mutex;
use slimenrf_recording::capture::format::load;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
pub(in crate::features::capture::device) struct Sim {
    clock: Duration,
    commands: Vec<[u8; 65]>,
    acks: VecDeque<[u8; 64]>,
    cancel: Arc<AtomicBool>,
    started: bool,
    stopped: bool,
    emitted: u16,
    samples: u16,
    mismatch_ack: bool,
    partial_start: bool,
    unplug: bool,
    cancel_on_start: bool,
    replaced_rf: bool,
    replaced_source: bool,
    cdc: bool,
    initialized: bool,
    initialization_error: Option<String>,
    cancel_during_initialization: bool,
    initialization_calls: usize,
    console: Option<ConsoleInitialization>,
    start_signal: Option<Arc<AtomicBool>>,
    firmware_frames: Option<VecDeque<Vec<u8>>>,
    stop_after: Duration,
    observe: Option<Arc<Mutex<Arc<CaptureSnapshot>>>>,
    phases: Vec<CapturePhase>,
    reject_start: Option<u8>,
    reject_metadata: bool,
    reassign_after_sample: bool,
    unknown_after_sample: bool,
}
impl Sim {
    pub(in crate::features::capture::device) fn new(cancel: Arc<AtomicBool>) -> Self {
        Self {
            clock: Duration::ZERO,
            commands: Vec::new(),
            acks: VecDeque::new(),
            cancel,
            started: false,
            stopped: false,
            emitted: 0,
            samples: 3,
            mismatch_ack: false,
            partial_start: false,
            unplug: false,
            cancel_on_start: false,
            replaced_rf: false,
            replaced_source: false,
            cdc: false,
            initialized: false,
            initialization_error: None,
            cancel_during_initialization: false,
            initialization_calls: 0,
            console: None,
            start_signal: None,
            firmware_frames: None,
            stop_after: Duration::ZERO,
            observe: None,
            phases: Vec::new(),
            reject_start: None,
            reject_metadata: false,
            reassign_after_sample: false,
            unknown_after_sample: false,
        }
    }
    fn opcodes(&self) -> Vec<u8> {
        self.commands.iter().map(|r| r[3]).collect()
    }
}
fn raw(sequence: u16) -> [u8; 64] {
    let mut bytes = [0; 64];
    bytes[0] = 0x13;
    bytes[1] = 3;
    bytes[2..4].copy_from_slice(&sequence.to_be_bytes());
    bytes[4..8].copy_from_slice(&1.0f32.to_le_bytes());
    bytes[28..32].copy_from_slice(&1.0f32.to_le_bytes());
    bytes[45..49].copy_from_slice(&(-7.25f32).to_le_bytes());
    bytes[52] = 42;
    bytes[53..57].copy_from_slice(&(1000 + u32::from(sequence)).to_be_bytes());
    bytes
}
impl CaptureIo for Sim {
    fn initialize(&mut self, cancel: &AtomicBool) -> Result<(), String> {
        self.initialization_calls += 1;
        if self.cancel_during_initialization {
            self.cancel.store(true, Ordering::Release);
        }
        if cancel.load(Ordering::Acquire) {
            return Err("Capture start cancelled".into());
        }
        if let Some(error) = &self.initialization_error {
            return Err(error.clone());
        }
        if let Some(console) = self.console.take() {
            console.run(cancel)?;
        }
        self.initialized = true;
        Ok(())
    }

    fn now(&self) -> Duration {
        self.clock
    }
    fn validate_source(&mut self, _: &CaptureSource) -> Result<(), String> {
        if self.replaced_source {
            Err("source replaced".into())
        } else {
            Ok(())
        }
    }
    fn cdc(&self) -> bool {
        self.cdc
    }
    fn read_control(&mut self, report: &mut [u8; 65], _: i32) -> Result<usize, String> {
        self.clock += Duration::from_millis(10);
        report.fill(0xf8);
        if let Some(ack) = self.acks.pop_front() {
            report[..64].copy_from_slice(&ack);
        } else {
            report[0] = 255;
            report[1] = 3;
            let replaced = self.replaced_rf || (self.reassign_after_sample && self.emitted > 0);
            report[2..8].copy_from_slice(&[
                0x66,
                0x55,
                0x44,
                0x33,
                0x22,
                if replaced { 0x99 } else { 0x11 },
            ]);
            if self.unknown_after_sample && self.emitted == 1 {
                report[2..8].fill(0);
            }
        }
        Ok(64)
    }
    fn write_control(&mut self, report: &[u8; 65]) -> Result<(), String> {
        self.commands.push(*report);
        let opcode = report[3];
        if matches!(opcode, 214 | 221) {
            assert!(
                !self.cdc || self.initialized,
                "legacy receiver must initialize before START"
            );
            self.started = true;
            if let Some(started) = &self.start_signal {
                started.store(true, Ordering::Release);
            }
            if self.cancel_on_start {
                self.cancel.store(true, Ordering::Release);
            }
            if self.partial_start {
                return Err("Partial HID control write (12/65); not retried".into());
            }
        }
        if matches!(opcode, 215 | 222) {
            self.stopped = true;
        }
        let mut ack = [0xf8; 64];
        ack[0] = 251;
        ack[1] = report[2];
        ack[2] = opcode;
        ack[3] = 6;
        if matches!(opcode, 214 | 221)
            && let Some(status) = self.reject_start
        {
            ack[3] = status;
        }
        if opcode == 223 && self.reject_metadata {
            ack[3] = 5;
        }
        if self.mismatch_ack && matches!(opcode, 214 | 221) {
            ack[1] = report[2].wrapping_add(1);
            self.acks.push_back(ack);
            ack[1] = report[2];
            ack[2] = opcode.wrapping_add(1);
        }
        self.acks.push_back(ack);
        Ok(())
    }
    fn read_data(&mut self, bytes: &mut [u8; 4096]) -> Result<usize, String> {
        if let Some(state) = &self.observe {
            self.phases.push(state.lock().phase);
        }
        self.clock += Duration::from_millis(10);
        if !self.started {
            return Ok(0);
        }
        if self.cdc && !self.initialized {
            // Old receiver firmware acknowledges START but drops CDC samples.
            return Ok(0);
        }
        if let Some(frames) = &mut self.firmware_frames {
            if let Some(frame) = frames.pop_front() {
                bytes[..frame.len()].copy_from_slice(&frame);
                self.emitted += 1;
                return Ok(frame.len());
            }
            self.cancel.store(true, Ordering::Release);
            return Ok(0);
        }
        if self.unplug && self.emitted > 0 {
            return Err("raw device unplugged".into());
        }
        if self.emitted < self.samples {
            let packet = raw(self.emitted);
            self.emitted += 1;
            if self.cdc {
                bytes[..3].copy_from_slice(&[0xaa, 0x55, 57]);
                bytes[3..60].copy_from_slice(&packet[..57]);
                let mut crc = 7u8;
                for byte in &bytes[2..60] {
                    crc ^= byte;
                    for _ in 0..8 {
                        crc = if crc & 0x80 != 0 {
                            (crc << 1) ^ 7
                        } else {
                            crc << 1
                        };
                    }
                }
                bytes[60] = crc;
                return Ok(61);
            }
            bytes[..64].copy_from_slice(&packet);
            return Ok(64);
        }
        if !self.mismatch_ack && self.clock >= self.stop_after {
            self.cancel.store(true, Ordering::Release);
        }
        Ok(0)
    }
}
pub(in crate::features::capture::device) fn request(
    path: std::path::PathBuf,
    batch: bool,
) -> CaptureRequest {
    CaptureRequest {
        source: CaptureSource {
            receiver: ReceiverEndpoint {
                key: "receiver-path".into(),
                path: vec![1],
                label: "test receiver".into(),
                serial_number: Some("unique".into()),
            },
            transport: CaptureTransport::RawHid { path: vec![2] },
        },
        mode: if batch {
            CaptureMode::AllPaired { rate_hz: 100 }
        } else {
            CaptureMode::Single {
                slot: 3,
                rf_address: "112233445566".into(),
            }
        },
        destination: path,
        roster: vec![(3, "112233445566".into())],
    }
}

fn cdc_request(path: std::path::PathBuf, batch: bool) -> CaptureRequest {
    let mut request = request(path, batch);
    let endpoint = |port: &str, number, description| SerialEndpoint {
        port_name: port.into(),
        label: port.into(),
        manufacturer: None,
        product: None,
        kind: SerialPortKind::Usb,
        serial_number: Some("unique".into()),
        vid: Some(0x1209),
        pid: Some(0x7690),
        usb_interface: Some(UsbInterface {
            number: Some(number),
            description,
        }),
    };
    request.source.transport = CaptureTransport::Cdc {
        endpoint: endpoint(
            "data",
            2,
            InterfaceDescription::Value("SlimeNRF Raw Data CDC".into()),
        ),
        console: Box::new(endpoint("console", 0, InterfaceDescription::Unavailable)),
    };
    request
}
fn run(request: &CaptureRequest, io: &mut Sim) -> (CaptureOutcome, Arc<CaptureSnapshot>) {
    let state = Mutex::new(Arc::new(CaptureSnapshot::default()));
    let cancel = Arc::clone(&io.cancel);
    let outcome = collect(
        request,
        expected_roster(request).unwrap(),
        io,
        &cancel,
        &state,
    )
    .unwrap();
    (outcome, state.into_inner())
}
#[test]
fn real_worker_loop_writes_raw_and_cdc_samples_then_matching_stop() {
    for cdc in [false, true] {
        for batch in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("capture.slimecap");
            let request = if cdc {
                cdc_request(path, batch)
            } else {
                request(path, batch)
            };
            let mut io = Sim::new(Arc::new(AtomicBool::new(false)));
            io.cdc = cdc;
            let (outcome, snapshot) = run(&request, &mut io);
            assert!(outcome.error.is_none(), "{:?}", outcome.error);
            assert_eq!(
                io.opcodes(),
                if batch {
                    vec![221, 222]
                } else {
                    vec![214, 215]
                }
            );
            assert_eq!(outcome.samples, 3);
            assert_eq!(snapshot.phase, CapturePhase::Finished);
            let recording = load(&outcome.path, &AtomicBool::new(false)).unwrap();
            assert_eq!(recording.tracks[0].samples.len(), 3);
            assert_eq!(recording.tracks[0].samples[0].temperature_c, Some(-7.25));
            assert_eq!(
                recording.tracks[0].identity.rf_address.as_deref(),
                Some("112233445566")
            );
            assert_eq!(io.commands[0][0], 0); // hidapi report-ID prefix
            assert_eq!(io.commands[0][1], 254);
            assert!(io.clock < Duration::from_secs(3));
        }
    }
}

fn firmware_frame() -> Vec<u8> {
    // Produced by the receiver's real CDC writer with a synthetic UART/radio.
    const FRAME: &str = "aa5539130300010000803f00000000000000000000000000000000000000000000803f000000000000000000000000000000c4410000002c01020304f0";
    FRAME
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|digits| u8::from_str_radix(std::str::from_utf8(digits).unwrap(), 16).unwrap())
        .collect()
}

#[test]
fn legacy_receiver_firmware_frame_is_recorded_after_console_initialization() {
    let dir = tempfile::tempdir().unwrap();
    let request = cdc_request(dir.path().join("firmware-frame.slimecap"), false);
    let mut io = Sim::new(Arc::new(AtomicBool::new(false)));
    io.cdc = true;
    io.firmware_frames = Some(VecDeque::from([firmware_frame()]));
    let (outcome, _) = run(&request, &mut io);
    assert!(outcome.error.is_none(), "{:?}", outcome.error);
    assert_eq!(io.initialization_calls, 1);
    assert_eq!(io.opcodes(), vec![214, 215]);
    assert_eq!(outcome.samples, 1);
    let recording = load(&outcome.path, &AtomicBool::new(false)).unwrap();
    assert_eq!(recording.tracks[0].samples.len(), 1);
    assert_eq!(recording.tracks[0].samples[0].temperature_c, Some(24.5));
}

fn answer_receiver_help(peer: &mut std::net::TcpStream) {
    use std::io::{Read, Write};

    let mut command = Vec::new();
    loop {
        let mut byte = [0];
        peer.read_exact(&mut byte).unwrap();
        if byte[0] == b'\n' {
            break;
        }
        command.push(byte[0]);
        assert!(command.len() < 128);
    }
    assert!(command.starts_with(b"help h"));
    peer.write_all(&command).unwrap();
    peer.write_all(b"\r\n").unwrap();
}

#[test]
fn connected_console_worker_initializes_real_capture_without_being_stolen() {
    use crate::domain::Inventory;
    use crate::sessions::ConnectionState;
    use crate::sessions::test_support::{connected, fixture, peer};
    use std::sync::mpsc;
    use std::time::Instant;

    let dir = tempfile::tempdir().unwrap();
    let request = cdc_request(dir.path().join("connected-console.slimecap"), false);
    let CaptureTransport::Cdc { endpoint, console } = &request.source.transport else {
        unreachable!()
    };
    let inventory = Inventory {
        receivers: vec![request.source.receiver.clone()],
        ports: vec![endpoint.clone(), console.as_ref().clone()],
        ..Inventory::default()
    };
    let (mut manager, listener) = fixture();
    manager.update_inventory(&inventory, Instant::now());
    manager.connect(&console.port_name).unwrap();
    let mut server = peer(&listener);
    connected(&mut manager, &console.port_name);
    let generation = manager.session(&console.port_name).unwrap().generation;
    let probe = manager.begin_capture_console_probe(console).unwrap();
    let (finish, wait) = mpsc::channel();
    let server = std::thread::spawn(move || {
        answer_receiver_help(&mut server);
        let _ = wait.recv_timeout(Duration::from_secs(3));
    });

    let mut io = Sim::new(Arc::new(AtomicBool::new(false)));
    io.cdc = true;
    io.console = Some(ConsoleInitialization::Existing(probe));
    io.firmware_frames = Some(VecDeque::from([firmware_frame()]));
    let (outcome, _) = run(&request, &mut io);
    assert!(outcome.error.is_none(), "{:?}", outcome.error);
    assert_eq!(outcome.samples, 1);
    assert_eq!(io.opcodes(), vec![214, 215]);
    let _ = manager.poll();
    let view = manager.session(&console.port_name).unwrap();
    assert_eq!(view.state, ConnectionState::Connected);
    assert_eq!(view.generation, generation);
    assert!(view.terminal.text().contains("help h"));
    assert_eq!(
        load(&outcome.path, &AtomicBool::new(false)).unwrap().tracks[0].samples[0].temperature_c,
        Some(24.5)
    );
    finish.send(()).unwrap();
    server.join().unwrap();
}

#[test]
fn temporary_console_is_closed_and_released_before_real_capture_start() {
    use crate::sessions::test_support::{fixture, peer};
    use std::io::Read;
    use std::sync::mpsc;

    let dir = tempfile::tempdir().unwrap();
    let request = cdc_request(dir.path().join("temporary-console.slimecap"), false);
    let (mut manager, listener) = fixture();
    let raw = manager.reserve("data", None).unwrap();
    let temporary = manager.reserve("console", None).unwrap();
    let source = manager.reserved_console_source(&temporary).unwrap();
    let (peer_closed, observed_close) = mpsc::channel();
    let server = std::thread::spawn(move || {
        let mut server = peer(&listener);
        answer_receiver_help(&mut server);
        let mut byte = [0];
        assert_eq!(server.read(&mut byte).unwrap(), 0);
        peer_closed.send(()).unwrap();
    });
    let (closed, closed_rx) = mpsc::channel();
    let (released, released_rx) = mpsc::channel();
    let started = Arc::new(AtomicBool::new(false));
    let mut io = Sim::new(Arc::new(AtomicBool::new(false)));
    io.cdc = true;
    io.start_signal = Some(Arc::clone(&started));
    io.console = Some(ConsoleInitialization::Temporary {
        source,
        closed,
        released: released_rx,
    });
    io.firmware_frames = Some(VecDeque::from([firmware_frame()]));
    let worker = std::thread::spawn(move || {
        let (outcome, _) = run(&request, &mut io);
        (outcome, io)
    });

    closed_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    observed_close.recv_timeout(Duration::from_secs(3)).unwrap();
    assert!(!started.load(Ordering::Acquire));
    assert!(manager.reserve("console", None).is_err());
    manager.release(&temporary).unwrap();
    released.send(Ok(())).unwrap();
    let (outcome, io) = worker.join().unwrap();
    server.join().unwrap();
    assert!(started.load(Ordering::Acquire));
    assert!(outcome.error.is_none(), "{:?}", outcome.error);
    assert_eq!(outcome.samples, 1);
    assert_eq!(io.opcodes(), vec![214, 215]);
    assert!(manager.reserve("data", None).is_err());
    manager.release(&raw).unwrap();
    assert!(manager.reserve("console", None).is_ok());
    assert_eq!(
        load(&outcome.path, &AtomicBool::new(false)).unwrap().tracks[0].samples[0].temperature_c,
        Some(24.5)
    );
}

#[test]
fn lost_temporary_console_before_nonce_echo_never_starts_capture() {
    use crate::sessions::test_support::{fixture, peer};
    use std::io::{Read, Write};
    use std::sync::mpsc;

    let dir = tempfile::tempdir().unwrap();
    let request = cdc_request(dir.path().join("lost-console.slimecap"), false);
    let (mut manager, listener) = fixture();
    let temporary = manager.reserve("console", None).unwrap();
    let source = manager.reserved_console_source(&temporary).unwrap();
    let server = std::thread::spawn(move || {
        let mut server = peer(&listener);
        let mut command = Vec::new();
        loop {
            let mut byte = [0];
            server.read_exact(&mut byte).unwrap();
            if byte[0] == b'\n' {
                break;
            }
            command.push(byte[0]);
            assert!(command.len() < 128);
        }
        server.write_all(b"*** Receiver ***\r\n").unwrap();
    });
    let (closed, closed_rx) = mpsc::channel();
    let (_released, released_rx) = mpsc::channel();
    let mut io = Sim::new(Arc::new(AtomicBool::new(false)));
    io.cdc = true;
    io.console = Some(ConsoleInitialization::Temporary {
        source,
        closed,
        released: released_rx,
    });
    let (outcome, _) = run(&request, &mut io);
    server.join().unwrap();
    assert!(closed_rx.try_recv().is_ok());
    assert!(outcome.error.is_some());
    assert!(io.opcodes().is_empty());
    assert_eq!(outcome.samples, 0);
    manager.release(&temporary).unwrap();
}

#[test]
fn failed_or_cancelled_console_initialization_never_sends_start() {
    for cancelled in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let request = cdc_request(dir.path().join("not-started.slimecap"), false);
        let mut io = Sim::new(Arc::new(AtomicBool::new(false)));
        io.cdc = true;
        if cancelled {
            io.cancel_during_initialization = true;
        } else {
            io.initialization_error = Some("Console initialization timed out".into());
        }
        let (outcome, _) = run(&request, &mut io);
        assert!(outcome.error.is_some());
        assert_eq!(io.initialization_calls, 1);
        assert!(io.opcodes().is_empty());
        assert_eq!(outcome.samples, 0);
    }
}
#[test]
fn wrong_sequence_or_opcode_never_accepts_start_and_stop_is_still_attempted() {
    let dir = tempfile::tempdir().unwrap();
    let request = request(dir.path().join("capture.slimecap"), false);
    let mut io = Sim::new(Arc::new(AtomicBool::new(false)));
    io.mismatch_ack = true;
    let (outcome, _) = run(&request, &mut io);
    assert!(outcome.error.as_deref().unwrap().contains("ACK timeout"));
    assert_eq!(io.opcodes(), vec![214, 215]);
    assert_eq!(
        load(&outcome.path, &AtomicBool::new(false)).unwrap().tracks[0]
            .samples
            .len(),
        3
    );
}
#[test]
fn failed_partial_start_and_cancelled_start_each_send_stop_once_and_finalize() {
    for partial in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let request = request(dir.path().join("partial.slimecap"), true);
        let mut io = Sim::new(Arc::new(AtomicBool::new(false)));
        io.partial_start = partial;
        io.cancel_on_start = !partial;
        let (outcome, snapshot) = run(&request, &mut io);
        assert!(outcome.error.is_some());
        assert_eq!(snapshot.phase, CapturePhase::Failed);
        assert_eq!(io.opcodes(), vec![221, 222]);
        assert!(load(&outcome.path, &AtomicBool::new(false)).is_ok());
    }
}
#[test]
fn unplug_preserves_partial_file_and_stop_attempt_without_retry() {
    let dir = tempfile::tempdir().unwrap();
    let request = request(dir.path().join("partial.slimecap"), false);
    let mut io = Sim::new(Arc::new(AtomicBool::new(false)));
    io.unplug = true;
    let (outcome, _) = run(&request, &mut io);
    assert!(outcome.error.as_deref().unwrap().contains("unplugged"));
    assert_eq!(outcome.samples, 1);
    assert_eq!(io.opcodes(), vec![214, 215]);
    assert_eq!(
        load(&outcome.path, &AtomicBool::new(false)).unwrap().tracks[0]
            .samples
            .len(),
        1
    );
}
#[test]
fn changed_rf_or_usb_source_rejects_before_start() {
    for rf in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let request = request(dir.path().join("rejected.slimecap"), false);
        let mut io = Sim::new(Arc::new(AtomicBool::new(false)));
        io.replaced_rf = rf;
        io.replaced_source = !rf;
        let (outcome, _) = run(&request, &mut io);
        assert!(outcome.error.is_some());
        assert!(io.commands.is_empty());
        assert_eq!(outcome.samples, 0);
    }
}
#[test]
fn cancelled_before_start_finalizes_without_radio_commands() {
    let dir = tempfile::tempdir().unwrap();
    let request = request(dir.path().join("cancelled.slimecap"), false);
    let mut io = Sim::new(Arc::new(AtomicBool::new(true)));
    let (outcome, _) = run(&request, &mut io);
    assert_eq!(outcome.samples, 0);
    assert!(io.commands.is_empty());
    assert!(outcome.error.as_deref().unwrap().contains("cancelled"));
}
#[test]
fn accepted_ack_without_samples_remains_warmup_then_waiting_with_real_zero() {
    let dir = tempfile::tempdir().unwrap();
    let request = request(dir.path().join("silent.slimecap"), false);
    let state = Arc::new(Mutex::new(Arc::new(CaptureSnapshot::default())));
    let cancel = Arc::new(AtomicBool::new(false));
    let mut io = Sim::new(Arc::clone(&cancel));
    io.samples = 0;
    io.stop_after = Duration::from_secs(7);
    io.observe = Some(Arc::clone(&state));
    let result = collect(
        &request,
        expected_roster(&request).unwrap(),
        &mut io,
        &cancel,
        &state,
    )
    .unwrap();
    assert_eq!(result.samples, 0);
    assert!(io.phases.contains(&CapturePhase::WarmingUp));
    assert!(io.phases.contains(&CapturePhase::WaitingForSamples));
    assert!(!io.phases.contains(&CapturePhase::Receiving));
    assert_eq!(io.opcodes(), vec![214, 215]);
    assert_eq!(state.lock().counters.samples, 0);
}
#[test]
fn missing_metadata_repairs_are_delayed_and_bounded_per_missing_section() {
    let dir = tempfile::tempdir().unwrap();
    let request = request(dir.path().join("repair.slimecap"), false);
    let mut io = Sim::new(Arc::new(AtomicBool::new(false)));
    io.stop_after = Duration::from_secs(20);
    let (outcome, _) = run(&request, &mut io);
    assert!(outcome.error.is_none());
    let repairs: Vec<_> = io
        .commands
        .iter()
        .filter(|r| r[3] == 223)
        .map(|r| [r[5], r[6], r[7]])
        .collect();
    assert_eq!(
        repairs,
        vec![
            [3, 1, 255],
            [3, 2, 255],
            [3, 4, 255],
            [3, 8, 255],
            [3, 16, 255]
        ]
    );
    assert_eq!(io.opcodes().first(), Some(&214));
    assert_eq!(io.opcodes().last(), Some(&215));
    assert_eq!(outcome.samples, 3);
}
#[test]
fn definite_start_rejection_does_not_stop_another_owners_collection() {
    for status in [1, 3, 5] {
        let dir = tempfile::tempdir().unwrap();
        let request = request(dir.path().join("rejected.slimecap"), false);
        let mut io = Sim::new(Arc::new(AtomicBool::new(false)));
        io.reject_start = Some(status);
        let (outcome, _) = run(&request, &mut io);
        assert!(outcome.error.as_deref().unwrap().contains("rejected"));
        assert_eq!(io.opcodes(), vec![214]);
        assert_eq!(outcome.samples, 0);
    }
}
#[test]
fn unsupported_metadata_is_not_retried_and_does_not_discard_real_samples() {
    let dir = tempfile::tempdir().unwrap();
    let request = request(dir.path().join("old-firmware.slimecap"), false);
    let mut io = Sim::new(Arc::new(AtomicBool::new(false)));
    io.reject_metadata = true;
    io.stop_after = Duration::from_secs(20);
    let (outcome, snapshot) = run(&request, &mut io);
    assert!(outcome.error.is_none());
    assert_eq!(io.opcodes(), vec![214, 223, 215]);
    assert_eq!(outcome.samples, 3);
    assert!(
        snapshot
            .warnings
            .iter()
            .any(|w| w.contains("Metadata repair disabled"))
    );
}
#[test]
fn live_rf_reassignment_stops_and_drain_never_relabels_samples_as_old_tracker() {
    let dir = tempfile::tempdir().unwrap();
    let request = request(dir.path().join("reassigned.slimecap"), false);
    let mut io = Sim::new(Arc::new(AtomicBool::new(false)));
    io.reassign_after_sample = true;
    let (outcome, _) = run(&request, &mut io);
    assert!(
        outcome
            .error
            .as_deref()
            .unwrap()
            .contains("RF identity changed")
    );
    assert_eq!(io.opcodes(), vec![214, 215]);
    assert_eq!(outcome.samples, 1);
    assert_eq!(io.emitted, 3); // Later bytes are drained, not recorded under the old RF address.
    let recording = load(&outcome.path, &AtomicBool::new(false)).unwrap();
    assert_eq!(recording.tracks[0].samples.len(), 1);
    assert_eq!(
        recording.tracks[0].identity.rf_address.as_deref(),
        Some("112233445566")
    );
}
#[test]
fn temporary_unknown_registration_does_not_retarget_or_invalidate_capture() {
    let dir = tempfile::tempdir().unwrap();
    let request = request(dir.path().join("unknown.slimecap"), false);
    let mut io = Sim::new(Arc::new(AtomicBool::new(false)));
    io.unknown_after_sample = true;
    let (outcome, _) = run(&request, &mut io);
    assert!(outcome.error.is_none());
    assert_eq!(outcome.samples, 3);
    assert_eq!(io.opcodes(), vec![214, 215]);
}
