use super::{
    CustomCommandState, FrozenCommandReview, Invocation, PendingCustomCommand, ReviewTargetSummary,
    capture_terminal_broadcast, connected_command_targets, validate_target_identity,
};
use crate::commands::{CustomCommandSnapshot, RouteEvidence};
use crate::domain::{
    DeviceRole, InterfaceDescription, Inventory, ReceiverEndpoint, SerialEndpoint, SerialPortKind,
    UsbInterface,
};
use crate::i18n::tr;
use crate::operations::{
    CommandOrigin, CommandRun, CommandRunSummary, CommandStepResult, CommandTargetPlan, Excluded,
    RunId,
};
use crate::sessions::test_support::{
    connected, display_manager, display_session_state, display_test_session, fixture, peer,
    socket_transport, stopped, until, with_transport_factory,
};
use crate::sessions::{ConnectionState, SessionManager, WriteReceipt};
use crate::shortcuts::{SavedCommandDraft, SavedCommands, TargetMode, parse_editor_lines};
use parking_lot::Mutex;
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

const PORT_A: &str = "receiver-a-console";
const PORT_B: &str = "receiver-b-console";

fn inventory() -> Inventory {
    let mut inventory = Inventory::default();
    for (port, serial, key) in [
        (PORT_A, "TEST-RECEIVER-A", "hid:test-receiver-a"),
        (PORT_B, "TEST-RECEIVER-B", "hid:test-receiver-b"),
    ] {
        inventory.ports.push(SerialEndpoint {
            port_name: port.into(),
            label: "Synthetic receiver console".into(),
            manufacturer: None,
            product: None,
            kind: SerialPortKind::Usb,
            serial_number: Some(serial.into()),
            vid: Some(0x1209),
            pid: Some(0x7690),
            usb_interface: Some(UsbInterface {
                number: Some(0),
                description: InterfaceDescription::Value("SlimeNRF Console CDC".into()),
            }),
        });
        inventory.receivers.push(ReceiverEndpoint {
            key: key.into(),
            path: key.as_bytes().to_vec(),
            label: "Synthetic receiver".into(),
            serial_number: Some(serial.into()),
        });
    }
    inventory
}

fn identify_receiver(manager: &mut SessionManager, port: &str, peer: &mut TcpStream) {
    connected(manager, port);
    peer.write_all(b"SlimeNRF Receiver\n").unwrap();
    until(|| {
        let _ = manager.poll();
        manager.session(port).unwrap().identity.role == DeviceRole::Receiver
    });
}

fn current_evidence<'a>(
    manager: &'a SessionManager,
    target: &'a CommandTargetPlan,
) -> Result<RouteEvidence<'a>, String> {
    let session = manager
        .session(&target.endpoint.port_name)
        .ok_or_else(|| tr("serial.batch.error.disconnected"))?;
    let connected = session.state == ConnectionState::Connected;
    Ok(match target.mode {
        TargetMode::ReceiverTrackers => RouteEvidence::Broadcast {
            port: &session.port,
            generation: session.generation,
            connected,
            receiver_key: &target.receiver.as_ref().unwrap().key,
            unique_receiver_route: true,
        },
        TargetMode::ConnectedTerminals | TargetMode::ReceiverConsoles => RouteEvidence::Direct {
            port: &session.port,
            generation: session.generation,
            connected,
            role: session.identity.role,
        },
    })
}

fn target_plan(
    manager: &SessionManager,
    inventory: &Inventory,
    index: usize,
    mode: TargetMode,
    lines: &[&str],
) -> CommandTargetPlan {
    let endpoint = inventory.ports[index].clone();
    let mut target = CommandTargetPlan {
        label: endpoint.label.clone(),
        generation: manager.session(&endpoint.port_name).unwrap().generation,
        endpoint,
        receiver: Some(inventory.receivers[index].clone()),
        mode,
        steps: Vec::new(),
    };
    validate_target_identity(&target.endpoint, target.receiver.as_ref(), inventory).unwrap();
    target.steps = lines
        .iter()
        .map(|line| {
            CustomCommandSnapshot::capture(line, current_evidence(manager, &target).unwrap())
        })
        .collect::<Result<_, _>>()
        .unwrap();
    target
}

fn admit(
    manager: &mut SessionManager,
    inventory: &Inventory,
    target: &CommandTargetPlan,
    snapshot: &CustomCommandSnapshot,
) -> Result<WriteReceipt, String> {
    validate_target_identity(&target.endpoint, target.receiver.as_ref(), inventory)?;
    snapshot.authorize(current_evidence(manager, target)?, true)?;
    manager.send_line(snapshot.port(), snapshot.command())
}

fn assert_payload(peer: &mut TcpStream, expected: &[u8]) {
    let mut received = Vec::new();
    peer.read_to_end(&mut received).unwrap();
    assert_eq!(received, expected);
}

#[derive(Clone, Copy)]
enum WriteBehavior {
    Complete,
    AmbiguousFailure,
}

struct WriteGate {
    ready: mpsc::Sender<()>,
    release: mpsc::Receiver<()>,
}

struct GatedSocket {
    stream: TcpStream,
    gate: Option<WriteGate>,
    behavior: WriteBehavior,
    writes: Arc<AtomicUsize>,
}

impl Read for GatedSocket {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.stream.read(bytes)
    }
}

impl Write for GatedSocket {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        if let Some(gate) = self.gate.take() {
            gate.ready.send(()).unwrap();
            gate.release.recv_timeout(Duration::from_secs(2)).unwrap();
        }
        match self.behavior {
            WriteBehavior::Complete => {
                self.stream.write_all(bytes)?;
                Ok(bytes.len())
            }
            WriteBehavior::AmbiguousFailure => {
                self.stream.write_all(&bytes[..2])?;
                Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "injected ambiguous custom write",
                ))
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

struct GatedPair {
    manager: SessionManager,
    peers: [TcpStream; 2],
    ready: [mpsc::Receiver<()>; 2],
    release: [mpsc::Sender<()>; 2],
    opens: Arc<AtomicUsize>,
    writes: [Arc<AtomicUsize>; 2],
}

impl Drop for GatedPair {
    fn drop(&mut self) {
        for release in &self.release {
            let _ = release.send(());
        }
        if let Some(worker) = self.manager.shutdown() {
            let _ = worker.join();
        }
    }
}

fn gated_pair(first_behavior: WriteBehavior) -> GatedPair {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (ready_a_tx, ready_a) = mpsc::channel();
    let (ready_b_tx, ready_b) = mpsc::channel();
    let (release_a, release_a_rx) = mpsc::channel();
    let (release_b, release_b_rx) = mpsc::channel();
    let gates = Mutex::new([
        Some(WriteGate {
            ready: ready_a_tx,
            release: release_a_rx,
        }),
        Some(WriteGate {
            ready: ready_b_tx,
            release: release_b_rx,
        }),
    ]);
    let opens = Arc::new(AtomicUsize::new(0));
    let factory_opens = Arc::clone(&opens);
    let writes = [Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0))];
    let factory_writes = writes.clone();
    let mut manager = with_transport_factory(move |port| {
        let index = match port {
            PORT_A => 0,
            PORT_B => 1,
            _ => panic!("custom commands must not fall back to another port"),
        };
        factory_opens.fetch_add(1, Ordering::SeqCst);
        let gate = gates.lock()[index]
            .take()
            .expect("custom commands must not reopen a transport");
        Ok(GatedSocket {
            stream: socket_transport(address)?,
            gate: Some(gate),
            behavior: if index == 0 {
                first_behavior
            } else {
                WriteBehavior::Complete
            },
            writes: Arc::clone(&factory_writes[index]),
        })
    });
    manager.connect(PORT_A).unwrap();
    let mut peer_a = peer(&listener);
    manager.connect(PORT_B).unwrap();
    let mut peer_b = peer(&listener);
    identify_receiver(&mut manager, PORT_A, &mut peer_a);
    identify_receiver(&mut manager, PORT_B, &mut peer_b);
    GatedPair {
        manager,
        peers: [peer_a, peer_b],
        ready: [ready_a, ready_b],
        release: [release_a, release_b],
        opens,
        writes,
    }
}

#[test]
fn accepted_direct_and_broadcast_runs_write_exact_literal_payloads_to_both_receivers() {
    for mode in [
        TargetMode::ConnectedTerminals,
        TargetMode::ReceiverConsoles,
        TargetMode::ReceiverTrackers,
    ] {
        let (mut manager, listener) = fixture();
        manager.connect(PORT_A).unwrap();
        let mut peer_a = peer(&listener);
        manager.connect(PORT_B).unwrap();
        let mut peer_b = peer(&listener);
        identify_receiver(&mut manager, PORT_A, &mut peer_a);
        identify_receiver(&mut manager, PORT_B, &mut peer_b);
        let inventory = inventory();
        let targets = (0..2)
            .map(|index| {
                target_plan(
                    &manager,
                    &inventory,
                    index,
                    mode,
                    &["  meow 中文 café  ", "info"],
                )
            })
            .collect();
        let mut run = CommandRun::new(
            RunId::new(1).unwrap(),
            CommandOrigin::SavedCommand,
            "Literal custom lines".into(),
            targets,
        );
        assert!(run.advance(|target, step| admit(&mut manager, &inventory, target, step)));
        assert_eq!(
            run.targets()[0].steps,
            [CommandStepResult::Queued, CommandStepResult::NotSubmitted]
        );
        assert_eq!(
            run.targets()[1].steps,
            [CommandStepResult::Queued, CommandStepResult::NotSubmitted]
        );
        until(|| {
            let _ = manager.poll();
            run.advance(|target, step| admit(&mut manager, &inventory, target, step));
            !run.pending()
        });
        for target in run.targets() {
            assert_eq!(
                target.steps,
                [CommandStepResult::Written, CommandStepResult::Written]
            );
        }
        manager.shutdown().unwrap().join().unwrap();
        let expected = if mode == TargetMode::ReceiverTrackers {
            "send all   meow 中文 café  \nsend all info\n".as_bytes()
        } else {
            "  meow 中文 café  \ninfo\n".as_bytes()
        };
        assert_payload(&mut peer_a, expected);
        assert_payload(&mut peer_b, expected);
    }
}

#[test]
fn each_receiver_waits_for_its_own_first_receipt_before_submitting_the_next_line() {
    let mut pair = gated_pair(WriteBehavior::Complete);
    let inventory = inventory();
    let targets = (0..2)
        .map(|index| {
            target_plan(
                &pair.manager,
                &inventory,
                index,
                TargetMode::ReceiverTrackers,
                &["info", "help"],
            )
        })
        .collect();
    let mut run = CommandRun::new(
        RunId::new(2).unwrap(),
        CommandOrigin::SavedCommand,
        "Independent receipts".into(),
        targets,
    );
    assert!(run.advance(|target, step| admit(&mut pair.manager, &inventory, target, step)));
    for ready in &pair.ready {
        ready.recv_timeout(Duration::from_secs(2)).unwrap();
    }
    assert!(!run.advance(|target, step| admit(&mut pair.manager, &inventory, target, step)));
    for target in run.targets() {
        assert_eq!(
            target.steps,
            [CommandStepResult::Queued, CommandStepResult::NotSubmitted]
        );
    }
    pair.release[0].send(()).unwrap();
    until(|| pair.manager.writes_idle(PORT_A));
    assert!(run.advance(|target, step| admit(&mut pair.manager, &inventory, target, step)));
    assert_eq!(
        run.targets()[0].steps,
        [CommandStepResult::Written, CommandStepResult::Queued]
    );
    assert_eq!(
        run.targets()[1].steps,
        [CommandStepResult::Queued, CommandStepResult::NotSubmitted]
    );
    until(|| {
        let _ = pair.manager.poll();
        run.advance(|target, step| admit(&mut pair.manager, &inventory, target, step));
        run.targets()[0].steps[1] == CommandStepResult::Written
    });
    assert!(run.pending());
    assert_eq!(pair.writes[0].load(Ordering::SeqCst), 2);
    assert_eq!(pair.writes[1].load(Ordering::SeqCst), 1);
    assert_eq!(
        run.targets()[1].steps,
        [CommandStepResult::Queued, CommandStepResult::NotSubmitted]
    );
    pair.release[1].send(()).unwrap();
    until(|| {
        let _ = pair.manager.poll();
        run.advance(|target, step| admit(&mut pair.manager, &inventory, target, step));
        !run.pending()
    });
    for target in run.targets() {
        assert_eq!(
            target.steps,
            [CommandStepResult::Written, CommandStepResult::Written]
        );
    }
    assert!(!run.advance(|_, _| panic!("finished runs must not replay accepted lines")));
    pair.manager.shutdown().unwrap().join().unwrap();
    for peer in &mut pair.peers {
        assert_payload(peer, b"send all info\nsend all help\n");
    }
    assert_eq!(pair.opens.load(Ordering::SeqCst), 2);
}

#[test]
fn ambiguous_write_failure_skips_only_that_receiver_tail_without_fallback_or_replay() {
    let mut pair = gated_pair(WriteBehavior::AmbiguousFailure);
    let inventory = inventory();
    let targets = (0..2)
        .map(|index| {
            target_plan(
                &pair.manager,
                &inventory,
                index,
                TargetMode::ReceiverTrackers,
                &["info", "help", "status"],
            )
        })
        .collect();
    let mut run = CommandRun::new(
        RunId::new(3).unwrap(),
        CommandOrigin::SavedCommand,
        "Partial failure".into(),
        targets,
    );
    assert!(run.advance(|target, step| admit(&mut pair.manager, &inventory, target, step)));
    for ready in &pair.ready {
        ready.recv_timeout(Duration::from_secs(2)).unwrap();
    }
    for release in &pair.release {
        release.send(()).unwrap();
    }
    until(|| {
        let _ = pair.manager.poll();
        run.advance(|target, step| admit(&mut pair.manager, &inventory, target, step));
        !run.pending()
    });
    assert!(matches!(
        run.targets()[0].steps.as_slice(),
        [
            CommandStepResult::WriteFailed(_),
            CommandStepResult::Skipped(_),
            CommandStepResult::Skipped(_),
        ]
    ));
    assert_eq!(
        run.targets()[1].steps,
        [
            CommandStepResult::Written,
            CommandStepResult::Written,
            CommandStepResult::Written
        ]
    );
    assert!(!run.advance(|_, _| panic!("failure must not replay or reroute the skipped tail")));
    pair.manager.shutdown().unwrap().join().unwrap();
    assert_payload(&mut pair.peers[0], b"se");
    assert_payload(
        &mut pair.peers[1],
        b"send all info\nsend all help\nsend all status\n",
    );
    assert_eq!(pair.writes[0].load(Ordering::SeqCst), 1);
    assert_eq!(pair.writes[1].load(Ordering::SeqCst), 3);
    assert_eq!(pair.opens.load(Ordering::SeqCst), 2);
}

#[derive(Clone, Copy, Debug)]
enum RouteMutation {
    Generation,
    Interface,
    ReceiverPath,
    ReceiverAssociation,
    RawCdc,
}

#[test]
fn changed_generation_interface_receiver_or_raw_cdc_rejects_the_next_line_without_replay() {
    for mutation in [
        RouteMutation::Generation,
        RouteMutation::Interface,
        RouteMutation::ReceiverPath,
        RouteMutation::ReceiverAssociation,
        RouteMutation::RawCdc,
    ] {
        let (mut manager, listener) = fixture();
        manager.connect(PORT_A).unwrap();
        let mut original_peer = peer(&listener);
        identify_receiver(&mut manager, PORT_A, &mut original_peer);
        let mut inventory = inventory();
        let target = target_plan(
            &manager,
            &inventory,
            0,
            TargetMode::ReceiverTrackers,
            &["info", "help", "status"],
        );
        let generation = target.generation;
        let mut run = CommandRun::new(
            RunId::new(4).unwrap(),
            CommandOrigin::SavedCommand,
            "Frozen route".into(),
            vec![target],
        );
        assert!(run.advance(|target, step| admit(&mut manager, &inventory, target, step)));
        until(|| manager.writes_idle(PORT_A));
        assert_eq!(
            run.targets()[0].steps,
            [
                CommandStepResult::Queued,
                CommandStepResult::NotSubmitted,
                CommandStepResult::NotSubmitted
            ]
        );
        let mut replacement_peer = None;
        match mutation {
            RouteMutation::Generation => {
                manager.disconnect(PORT_A).unwrap();
                stopped(&mut manager, PORT_A);
                manager.connect(PORT_A).unwrap();
                let mut replacement = peer(&listener);
                identify_receiver(&mut manager, PORT_A, &mut replacement);
                assert_ne!(manager.session(PORT_A).unwrap().generation, generation);
                replacement_peer = Some(replacement);
            }
            RouteMutation::Interface => {
                inventory.ports[0].usb_interface.as_mut().unwrap().number = Some(2);
            }
            RouteMutation::ReceiverPath => {
                inventory.receivers[0].path = b"replacement-hid-path".to_vec();
            }
            RouteMutation::ReceiverAssociation => {
                inventory.receivers[0].serial_number = Some("TEST-RECEIVER-B".into());
                inventory.receivers[1].serial_number = Some("TEST-RECEIVER-A".into());
            }
            RouteMutation::RawCdc => {
                inventory.ports[0]
                    .usb_interface
                    .as_mut()
                    .unwrap()
                    .description = InterfaceDescription::Value("SlimeNRF Raw Data CDC".into());
            }
        };
        assert!(run.advance(|target, step| admit(&mut manager, &inventory, target, step)));
        assert!(!run.pending(), "{mutation:?}");
        assert!(
            matches!(
                run.targets()[0].steps.as_slice(),
                [
                    CommandStepResult::Written,
                    CommandStepResult::Rejected(_),
                    CommandStepResult::Skipped(_),
                ]
            ),
            "{mutation:?}"
        );
        assert!(!run.advance(|_, _| panic!("a revoked route must not replay or fall back")));
        manager.shutdown().unwrap().join().unwrap();
        assert_payload(&mut original_peer, b"send all info\n");
        if let Some(mut replacement) = replacement_peer {
            assert_payload(&mut replacement, b"");
        }
    }
}

#[test]
fn raw_cdc_is_rejected_by_shared_identity_admission_without_opening_a_transport() {
    let mut manager = display_manager();
    let mut inventory = inventory();
    let mut raw = inventory.ports[0].clone();
    raw.port_name = "receiver-a-raw".into();
    raw.usb_interface = Some(UsbInterface {
        number: Some(1),
        description: InterfaceDescription::Value("SlimeNRF Raw Data CDC".into()),
    });
    inventory.ports.push(raw.clone());
    assert_eq!(
        validate_target_identity(
            &inventory.ports[0],
            Some(&inventory.receivers[0]),
            &inventory
        ),
        Ok(())
    );
    let evidence = RouteEvidence::Direct {
        port: &raw.port_name,
        generation: 1,
        connected: true,
        role: DeviceRole::Receiver,
    };
    let steps = ["info", "help"]
        .iter()
        .map(|line| CustomCommandSnapshot::capture(line, evidence).unwrap())
        .collect();
    let target = CommandTargetPlan {
        label: "Synthetic raw CDC".into(),
        endpoint: raw,
        receiver: Some(inventory.receivers[0].clone()),
        mode: TargetMode::ReceiverConsoles,
        generation: 1,
        steps,
    };
    let mut run = CommandRun::new(
        RunId::new(5).unwrap(),
        CommandOrigin::SavedCommand,
        "Raw interface".into(),
        vec![target],
    );
    assert!(run.advance(|target, step| admit(&mut manager, &inventory, target, step)));
    assert!(matches!(
        run.targets()[0].steps.as_slice(),
        [
            CommandStepResult::Rejected(_),
            CommandStepResult::Skipped(_)
        ]
    ));
    assert!(!run.pending());
    assert_eq!(manager.sessions().count(), 0);
    assert!(!run.advance(|_, _| panic!("raw interfaces must not be retried")));
}

#[test]
fn cancellation_retains_the_accepted_receipt_and_never_submits_the_remaining_lines() {
    let mut pair = gated_pair(WriteBehavior::Complete);
    let inventory = inventory();
    let target = target_plan(
        &pair.manager,
        &inventory,
        0,
        TargetMode::ReceiverConsoles,
        &["info", "help", "status"],
    );
    let mut run = CommandRun::new(
        RunId::new(6).unwrap(),
        CommandOrigin::SavedCommand,
        "Cancellation".into(),
        vec![target],
    );
    assert!(run.advance(|target, step| admit(&mut pair.manager, &inventory, target, step)));
    pair.ready[0].recv_timeout(Duration::from_secs(2)).unwrap();
    run.cancel();
    assert!(run.pending());
    assert!(!pair.manager.writes_idle(PORT_A));
    assert!(matches!(
        run.targets()[0].steps.as_slice(),
        [CommandStepResult::Queued, CommandStepResult::Skipped(first), CommandStepResult::Skipped(second)]
            if !first.is_empty() && !second.is_empty()
    ));
    assert!(!run.advance(|_, _| panic!("cancellation may only poll the retained receipt")));
    assert!(run.pending());
    pair.release[0].send(()).unwrap();
    until(|| {
        let _ = pair.manager.poll();
        run.advance(|_, _| panic!("cancellation must not submit the skipped tail"));
        !run.pending()
    });
    assert!(matches!(
        run.targets()[0].steps.as_slice(),
        [CommandStepResult::Written, CommandStepResult::Skipped(first), CommandStepResult::Skipped(second)]
            if !first.is_empty() && !second.is_empty()
    ));
    assert!(!run.advance(|_, _| panic!("completed cancellation must not replay")));
    pair.manager.shutdown().unwrap().join().unwrap();
    assert_payload(&mut pair.peers[0], b"info\n");
    assert_payload(&mut pair.peers[1], b"");
    assert_eq!(pair.writes[0].load(Ordering::SeqCst), 1);
    assert_eq!(pair.writes[1].load(Ordering::SeqCst), 0);
    assert_eq!(pair.opens.load(Ordering::SeqCst), 2);
}

fn saved_review() -> FrozenCommandReview {
    let inventory = inventory();
    let evidence = RouteEvidence::Direct {
        port: PORT_A,
        generation: 17,
        connected: true,
        role: DeviceRole::Receiver,
    };
    let mut library = SavedCommands::default();
    let source_id = library
        .save_draft(SavedCommandDraft {
            id: None,
            title: "Saved command".into(),
            description: String::new(),
            icon: None,
            pinned: true,
            target_mode: TargetMode::ConnectedTerminals,
            commands_text: "info\nhelp".into(),
        })
        .unwrap();
    FrozenCommandReview {
        invocation: Invocation {
            source_id,
            title: "Saved command".into(),
            mode: TargetMode::ConnectedTerminals,
            lines: parse_editor_lines("info\nhelp", TargetMode::ConnectedTerminals).unwrap(),
        },
        targets: vec![CommandTargetPlan {
            label: "Synthetic receiver".into(),
            endpoint: inventory.ports[0].clone(),
            receiver: Some(inventory.receivers[0].clone()),
            mode: TargetMode::ConnectedTerminals,
            generation: 17,
            steps: ["info", "help"]
                .iter()
                .map(|line| CustomCommandSnapshot::capture(line, evidence).unwrap())
                .collect(),
        }],
        summaries: vec![ReviewTargetSummary {
            label: "Synthetic receiver".into(),
            port: PORT_A.into(),
            lines: vec!["info".into(), "help".into()],
        }],
        excluded: Vec::new(),
    }
}

#[gpui_kit::test]
fn shutdown_rejects_custom_commands_without_consuming_the_review(
    cx: &mut gpui_kit::TestAppContext,
) {
    use gpui_kit::VisualContext as _;

    let _ui_session = crate::ui::test_support::UiTestSession::new(cx);
    let runtime = crate::app_runtime::AppRuntime::new().expect("start app runtime");
    let executor = runtime.executor();
    let cx = cx.add_empty_window();
    let workbench = cx.new_window_entity(move |window, cx| {
        crate::ui::test_support::offline_workbench(window, cx, SessionManager::new(), executor)
    });
    workbench.update(cx, |this, cx| {
        this.custom_commands.pending = Some(PendingCustomCommand::Review(saved_review()));
        this.shutting_down = true;

        assert_eq!(
            this.submit_terminal_send_all(PORT_A, "info".into(), cx)
                .err(),
            Some(tr("firmware.status.closing"))
        );
        assert_eq!(
            this.prepare_custom_invocation(saved_review().invocation, Default::default())
                .err(),
            Some(tr("firmware.status.closing"))
        );
        assert_eq!(
            this.submit_custom_review().err(),
            Some(tr("firmware.status.closing"))
        );
        assert!(matches!(
            this.custom_commands.pending,
            Some(PendingCustomCommand::Review(_))
        ));
        assert!(this.custom_commands.runs.is_empty());
        assert_eq!(this.manager.sessions().count(), 0);
        this.shutting_down = false;
    });
}

#[test]
fn connected_terminal_selection_uses_only_current_admitted_sessions_without_connecting() {
    let mut manager = display_manager();
    let mut inventory = Inventory::default();
    let ports = ["connected", "connecting", "disconnected", "raw", "reserved"];
    let mut sessions = Vec::new();
    for port in ports {
        inventory.ports.push(SerialEndpoint {
            port_name: port.into(),
            label: port.into(),
            manufacturer: None,
            product: None,
            kind: SerialPortKind::Usb,
            serial_number: Some(format!("TEST-{port}")),
            vid: Some(0xcafe),
            pid: Some(1),
            usb_interface: None,
        });
        sessions.push(display_test_session(&mut manager, port));
    }
    display_session_state(&mut manager, "connecting", ConnectionState::Connecting);
    display_session_state(&mut manager, "disconnected", ConnectionState::Disconnected);
    let raw = inventory
        .ports
        .iter_mut()
        .find(|port| port.port_name == "raw")
        .unwrap();
    raw.vid = Some(0x1209);
    raw.pid = Some(0x7690);
    raw.usb_interface = Some(UsbInterface {
        number: Some(2),
        description: InterfaceDescription::Value("SlimeNRF Raw Data CDC".into()),
    });
    let reservation = manager.reserve("reserved", Some(1)).unwrap();
    assert!(manager.command_port_reserved("reserved"));
    let mut admissions = 0;
    let snapshot = connected_command_targets(&manager, &inventory, |_, _| {
        admissions += 1;
        Ok(())
    });
    assert_eq!(admissions, 1);
    assert_eq!(
        snapshot
            .iter()
            .filter(|choice| choice.exclusion.is_none())
            .map(|choice| choice.key.as_str())
            .collect::<Vec<_>>(),
        ["connected"]
    );
    for port in ["connecting", "disconnected", "raw", "reserved"] {
        assert!(
            snapshot
                .iter()
                .find(|choice| choice.key == port)
                .unwrap()
                .exclusion
                .is_some(),
            "{port} must retain its exclusion reason"
        );
    }
    let mut late = inventory.ports[0].clone();
    late.port_name = "later-connected".into();
    late.serial_number = Some("TEST-LATER".into());
    inventory.ports.push(late);
    sessions.push(display_test_session(&mut manager, "later-connected"));
    assert!(
        !snapshot
            .iter()
            .any(|choice| choice.key == "later-connected")
    );
    let current = connected_command_targets(&manager, &inventory, |_, _| Ok(()));
    assert_eq!(
        current
            .iter()
            .filter(|choice| choice.exclusion.is_none())
            .map(|choice| choice.key.as_str())
            .collect::<Vec<_>>(),
        ["connected", "later-connected"]
    );
    assert_eq!(
        manager.session("connecting").unwrap().state,
        ConnectionState::Connecting
    );
    assert_eq!(
        manager.session("disconnected").unwrap().state,
        ConnectionState::Disconnected
    );
    manager.release(&reservation).unwrap();
    drop(sessions);
}

fn admit_terminal(
    manager: &mut SessionManager,
    inventory: &Inventory,
    target: &CommandTargetPlan,
    snapshot: &CustomCommandSnapshot,
) -> Result<WriteReceipt, String> {
    validate_target_identity(&target.endpoint, target.receiver.as_ref(), inventory)?;
    snapshot.authorize_terminal_send(current_evidence(manager, target)?)?;
    manager.send_line(snapshot.port(), snapshot.command())
}

fn poll_terminal_runs(
    state: &mut CustomCommandState,
    manager: &mut SessionManager,
    inventory: &Inventory,
) {
    let _ = manager.poll();
    state.poll_runs(|saved_reviewed, target, step| {
        assert!(!saved_reviewed);
        admit_terminal(manager, inventory, target, step)
    });
}

#[test]
fn terminal_send_all_immediately_submits_to_two_connected_consoles_without_review_or_reopen() {
    let mut pair = gated_pair(WriteBehavior::Complete);
    let inventory = inventory();
    let draft = "  meow 中文 café  \r\nhelp\r\n";
    let broadcast = capture_terminal_broadcast(
        PORT_A,
        draft.into(),
        &pair.manager,
        &inventory,
        |_, _| Ok(()),
        |_| DeviceRole::Receiver,
        |_, label| label.to_owned(),
    )
    .unwrap();
    let saved = saved_review();
    let saved_id = saved.source_id();
    let mut state = CustomCommandState {
        pending: Some(PendingCustomCommand::Review(saved)),
        error: Some("Saved review error".into()),
        ..CustomCommandState::default()
    };
    let id = state
        .submit_terminal_broadcast(broadcast, |target, step| {
            admit_terminal(&mut pair.manager, &inventory, target, step)
        })
        .unwrap();
    for ready in &pair.ready {
        ready.recv_timeout(Duration::from_secs(2)).unwrap();
    }
    let Some(PendingCustomCommand::Review(review)) = &state.pending else {
        panic!("a successful terminal send must preserve the saved review");
    };
    assert_eq!(review.source_id(), saved_id);
    assert_eq!(review.targets[0].steps[1].command(), "help");
    assert_eq!(state.error.as_deref(), Some("Saved review error"));
    assert_eq!(state.saved_runs().count(), 0);
    assert_eq!(
        state
            .terminal_runs()
            .map(|run| (run.id(), run.origin().clone()))
            .collect::<Vec<_>>(),
        [(
            id,
            CommandOrigin::Terminal {
                source_port: PORT_A.into()
            }
        )]
    );
    assert_eq!(
        state.terminal_clear.as_ref(),
        Some(&(PORT_A.into(), draft.into()))
    );
    assert_eq!(
        state.runs[0].summary(),
        CommandRunSummary {
            written: 0,
            pending: 2,
            failed: 0
        }
    );
    assert!(!state.dismiss_run(id));
    assert_eq!(
        state.runs[0].targets()[0].steps,
        [CommandStepResult::Queued, CommandStepResult::NotSubmitted]
    );
    assert_eq!(
        state.runs[0].targets()[1].steps,
        [CommandStepResult::Queued, CommandStepResult::NotSubmitted]
    );
    assert_eq!(pair.opens.load(Ordering::SeqCst), 2);
    for release in &pair.release {
        release.send(()).unwrap();
    }
    until(|| {
        poll_terminal_runs(&mut state, &mut pair.manager, &inventory);
        !state.runs[0].pending()
    });
    assert_eq!(
        state.runs[0].summary(),
        CommandRunSummary {
            written: 2,
            pending: 0,
            failed: 0
        }
    );
    for port in [PORT_A, PORT_B] {
        assert_eq!(
            state
                .pending_history
                .iter()
                .filter(|(target, _)| target == port)
                .map(|(_, command)| command.as_str())
                .collect::<Vec<_>>(),
            ["  meow 中文 café  ", "help"]
        );
    }
    assert!(state.dismiss_run(id));
    assert_eq!(state.terminal_runs().count(), 0);
    pair.manager.shutdown().unwrap().join().unwrap();
    for peer in &mut pair.peers {
        assert_payload(peer, "  meow 中文 café  \nhelp\n".as_bytes());
    }
    assert_eq!(pair.opens.load(Ordering::SeqCst), 2);
}

#[test]
fn terminal_partial_write_failure_retains_other_receipts_history_and_failure_until_dismissed() {
    let mut pair = gated_pair(WriteBehavior::AmbiguousFailure);
    let inventory = inventory();
    let broadcast = capture_terminal_broadcast(
        PORT_A,
        "info\nhelp".into(),
        &pair.manager,
        &inventory,
        |_, _| Ok(()),
        |_| DeviceRole::Receiver,
        |_, label| label.to_owned(),
    )
    .unwrap();
    let mut state = CustomCommandState::default();
    let id = state
        .submit_terminal_broadcast(broadcast, |target, step| {
            admit_terminal(&mut pair.manager, &inventory, target, step)
        })
        .unwrap();
    for ready in &pair.ready {
        ready.recv_timeout(Duration::from_secs(2)).unwrap();
    }
    assert_eq!(
        state.terminal_clear.as_ref(),
        Some(&(PORT_A.into(), "info\nhelp".into()))
    );
    assert!(!state.dismiss_run(id));
    for release in &pair.release {
        release.send(()).unwrap();
    }
    until(|| {
        poll_terminal_runs(&mut state, &mut pair.manager, &inventory);
        !state.runs[0].pending()
    });
    assert!(matches!(
        state.runs[0].targets()[0].steps.as_slice(),
        [
            CommandStepResult::WriteFailed(_),
            CommandStepResult::Skipped(_)
        ]
    ));
    assert_eq!(
        state.runs[0].targets()[1].steps,
        [CommandStepResult::Written, CommandStepResult::Written]
    );
    assert_eq!(
        state.runs[0].summary(),
        CommandRunSummary {
            written: 1,
            pending: 0,
            failed: 1
        }
    );
    assert_eq!(
        state
            .pending_history
            .iter()
            .filter(|(port, _)| port == PORT_A)
            .map(|(_, command)| command.as_str())
            .collect::<Vec<_>>(),
        ["info"]
    );
    assert_eq!(
        state
            .pending_history
            .iter()
            .filter(|(port, _)| port == PORT_B)
            .map(|(_, command)| command.as_str())
            .collect::<Vec<_>>(),
        ["info", "help"]
    );
    assert!(!state.poll_runs(|_, _, _| panic!("settled sends cannot retry")));
    pair.manager.shutdown().unwrap().join().unwrap();
    assert_payload(&mut pair.peers[0], b"in");
    assert_payload(&mut pair.peers[1], b"info\nhelp\n");
    assert_eq!(pair.opens.load(Ordering::SeqCst), 2);
    assert_eq!(pair.writes[0].load(Ordering::SeqCst), 1);
    assert_eq!(pair.writes[1].load(Ordering::SeqCst), 2);
    assert!(state.dismiss_run(id));
}

#[test]
fn terminal_exclusion_only_result_preserves_draft_and_unrelated_saved_review_and_error() {
    let mut manager = display_manager();
    let inventory = inventory();
    let sessions = [
        display_test_session(&mut manager, PORT_A),
        display_test_session(&mut manager, PORT_B),
    ];
    let saved = saved_review();
    let saved_id = saved.source_id();
    let mut state = CustomCommandState {
        pending: Some(PendingCustomCommand::Review(saved)),
        error: Some("Saved review error".into()),
        ..CustomCommandState::default()
    };
    let broadcast = capture_terminal_broadcast(
        PORT_A,
        "info".into(),
        &manager,
        &inventory,
        |_, _| Err("Command console is leased".into()),
        |_| DeviceRole::Receiver,
        |_, label| label.to_owned(),
    )
    .unwrap();
    let id = state
        .submit_terminal_broadcast(broadcast, |_, _| {
            panic!("an excluded console must never submit")
        })
        .unwrap();
    assert_eq!(state.terminal_clear, None);
    assert_eq!(state.pending_history, Vec::<(String, String)>::new());
    assert_eq!(
        state.runs[0].summary(),
        CommandRunSummary {
            written: 0,
            pending: 0,
            failed: 2
        }
    );
    assert_eq!(
        state.runs[0]
            .targets()
            .iter()
            .map(|target| target.steps.as_slice())
            .collect::<Vec<_>>(),
        [
            [CommandStepResult::Rejected(
                "Command console is leased".into()
            )]
            .as_slice(),
            [CommandStepResult::Rejected(
                "Command console is leased".into()
            )]
            .as_slice()
        ]
    );
    let Some(PendingCustomCommand::Review(review)) = &state.pending else {
        panic!("the saved review must remain pending");
    };
    assert_eq!(review.source_id(), saved_id);
    assert_eq!(review.targets[0].steps[1].command(), "help");
    assert_eq!(state.error.as_deref(), Some("Saved review error"));
    assert_eq!(state.saved_runs().count(), 0);
    assert!(state.dismiss_run(id));
    drop(sessions);
}

#[test]
fn terminal_parse_no_target_and_rejected_send_do_not_clear_source_or_replace_saved_state() {
    let mut pair = gated_pair(WriteBehavior::Complete);
    let inventory = inventory();
    let mut state = CustomCommandState {
        pending: Some(PendingCustomCommand::Review(saved_review())),
        error: Some("Saved review error".into()),
        ..CustomCommandState::default()
    };
    assert!(
        capture_terminal_broadcast(
            PORT_A,
            "info\n\nhelp".into(),
            &pair.manager,
            &inventory,
            |_, _| Ok(()),
            |_| DeviceRole::Receiver,
            |_, label| label.to_owned(),
        )
        .is_err()
    );
    let empty = display_manager();
    assert!(
        capture_terminal_broadcast(
            PORT_A,
            "info".into(),
            &empty,
            &inventory,
            |_, _| Ok(()),
            |_| DeviceRole::Receiver,
            |_, label| label.to_owned(),
        )
        .is_err()
    );
    let broadcast = capture_terminal_broadcast(
        PORT_A,
        "info".into(),
        &pair.manager,
        &inventory,
        |_, _| Ok(()),
        |_| DeviceRole::Receiver,
        |_, label| label.to_owned(),
    )
    .unwrap();
    state
        .submit_terminal_broadcast(broadcast, |_, _| Err("Submission rejected".into()))
        .unwrap();
    assert_eq!(state.terminal_clear, None);
    assert_eq!(state.pending_history, Vec::<(String, String)>::new());
    assert_eq!(
        state.runs[0].summary(),
        CommandRunSummary {
            written: 0,
            pending: 0,
            failed: 2
        }
    );
    let Some(PendingCustomCommand::Review(review)) = &state.pending else {
        panic!("failed terminal sends must preserve the saved review");
    };
    assert_eq!(review.targets[0].steps[0].command(), "info");
    assert_eq!(state.error.as_deref(), Some("Saved review error"));
    pair.manager.shutdown().unwrap().join().unwrap();
    for peer in &mut pair.peers {
        assert_payload(peer, b"");
    }
    assert_eq!(pair.opens.load(Ordering::SeqCst), 2);
}

#[test]
fn global_terminal_history_retains_each_source_and_keeps_saved_results_on_home() {
    let mut state = CustomCommandState::default();
    let (saved_id, _) = state
        .submit_run(
            CommandOrigin::SavedCommand,
            tr("serial.terminal.action.send_all"),
            Vec::new(),
            vec![Excluded {
                label: "Saved target".into(),
                reason: "Saved exclusion".into(),
            }],
            |_, _| panic!("excluded saved targets cannot submit"),
        )
        .unwrap();
    let mut terminal_ids = Vec::new();
    for port in [PORT_A, PORT_B] {
        let broadcast = super::TerminalBroadcast {
            source_port: port.into(),
            text: "info".into(),
            targets: Vec::new(),
            excluded: vec![Excluded {
                label: port.into(),
                reason: "Terminal exclusion".into(),
            }],
        };
        terminal_ids.push(
            state
                .submit_terminal_broadcast(broadcast, |_, _| {
                    panic!("excluded terminal targets cannot submit")
                })
                .unwrap(),
        );
    }
    assert_eq!(
        state.saved_runs().map(CommandRun::id).collect::<Vec<_>>(),
        [saved_id]
    );
    assert_eq!(
        state
            .terminal_runs()
            .map(|run| (run.id(), run.origin().clone()))
            .collect::<Vec<_>>(),
        [
            (
                terminal_ids[1],
                CommandOrigin::Terminal {
                    source_port: PORT_B.into()
                }
            ),
            (
                terminal_ids[0],
                CommandOrigin::Terminal {
                    source_port: PORT_A.into()
                }
            ),
        ]
    );
    assert!(state.dismiss_run(terminal_ids[0]));
    assert_eq!(
        state.saved_runs().map(CommandRun::id).collect::<Vec<_>>(),
        [saved_id]
    );
    assert_eq!(
        state
            .terminal_runs()
            .map(CommandRun::id)
            .collect::<Vec<_>>(),
        [terminal_ids[1]]
    );
}

#[test]
fn bounded_terminal_feedback_retains_failures_until_explicit_dismissal() {
    let mut state = CustomCommandState {
        pending: Some(PendingCustomCommand::Review(saved_review())),
        error: Some("Saved review error".into()),
        ..CustomCommandState::default()
    };
    let broadcast = || super::TerminalBroadcast {
        source_port: PORT_A.into(),
        text: "info".into(),
        targets: Vec::new(),
        excluded: vec![Excluded {
            label: PORT_A.into(),
            reason: "Console leased".into(),
        }],
    };
    for _ in 0..16 {
        state
            .submit_terminal_broadcast(broadcast(), |_, _| {
                panic!("excluded consoles cannot submit")
            })
            .unwrap();
    }
    assert!(
        state
            .submit_terminal_broadcast(broadcast(), |_, _| {
                panic!("a full feedback list must reject before submission")
            })
            .is_err()
    );
    assert_eq!(
        state
            .runs
            .iter()
            .map(|run| run.id().get())
            .collect::<Vec<_>>(),
        [16, 15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1]
    );
    assert_eq!(state.terminal_clear, None);
    assert_eq!(state.error.as_deref(), Some("Saved review error"));
    assert_eq!(state.pending_history, Vec::<(String, String)>::new());
    assert!(state.dismiss_run(RunId::new(1).unwrap()));
    let id = state
        .submit_terminal_broadcast(broadcast(), |_, _| {
            panic!("excluded consoles cannot submit")
        })
        .unwrap();
    assert_eq!(id.get(), 17);
    let Some(PendingCustomCommand::Review(review)) = &state.pending else {
        panic!("capacity failure and dismissal must preserve the saved review");
    };
    assert_eq!(review.targets[0].steps[0].command(), "info");
}

#[test]
fn bounded_feedback_replaces_old_success_before_older_undismissed_failure() {
    let mut state = CustomCommandState::default();
    let failed = super::TerminalBroadcast {
        source_port: PORT_A.into(),
        text: "info".into(),
        targets: Vec::new(),
        excluded: vec![Excluded {
            label: PORT_A.into(),
            reason: "Console leased".into(),
        }],
    };
    let failure_id = state
        .submit_terminal_broadcast(failed, |_, _| panic!("excluded consoles cannot submit"))
        .unwrap();
    for _ in 0..16 {
        let mut target = saved_review().targets.remove(0);
        target.steps.truncate(1);
        let (receipt, complete) = crate::sessions::test_support::write_receipt();
        let mut receipt = Some(receipt);
        let broadcast = super::TerminalBroadcast {
            source_port: PORT_A.into(),
            text: "info".into(),
            targets: vec![target],
            excluded: Vec::new(),
        };
        state
            .submit_terminal_broadcast(broadcast, |_, _| Ok(receipt.take().unwrap()))
            .unwrap();
        complete.send(Ok(())).unwrap();
        state.poll_runs(|_, _, _| panic!("a settled single line must not replay"));
    }
    assert_eq!(state.runs.len(), 16);
    assert_eq!(state.runs.back().unwrap().id(), failure_id);
    assert_eq!(
        state.runs.back().unwrap().summary(),
        CommandRunSummary {
            written: 0,
            pending: 0,
            failed: 1
        }
    );
    assert_eq!(
        state.runs.iter().filter(|run| run.id().get() == 2).count(),
        0
    );
    assert_eq!(state.runs.front().unwrap().id().get(), 17);
}

#[test]
fn full_active_feedback_rejects_before_write_and_keeps_all_accepted_receipts() {
    let mut state = CustomCommandState::default();
    let mut completions = Vec::new();
    for _ in 0..16 {
        let mut target = saved_review().targets.remove(0);
        target.steps.truncate(1);
        let (receipt, complete) = crate::sessions::test_support::write_receipt();
        let mut receipt = Some(receipt);
        let broadcast = super::TerminalBroadcast {
            source_port: PORT_A.into(),
            text: "info".into(),
            targets: vec![target],
            excluded: Vec::new(),
        };
        state
            .submit_terminal_broadcast(broadcast, |_, _| Ok(receipt.take().unwrap()))
            .unwrap();
        assert_eq!(
            state.terminal_clear.take(),
            Some((PORT_A.into(), "info".into()))
        );
        completions.push(complete);
    }
    let broadcast = super::TerminalBroadcast {
        source_port: PORT_A.into(),
        text: "help".into(),
        targets: saved_review().targets,
        excluded: Vec::new(),
    };
    assert!(
        state
            .submit_terminal_broadcast(broadcast, |_, _| {
                panic!("full active feedback must reject before a write")
            })
            .is_err()
    );
    assert_eq!(state.terminal_clear, None);
    assert_eq!(state.pending_history.len(), 16);
    assert_eq!(state.runs.iter().filter(|run| run.pending()).count(), 16);
    for completion in completions {
        completion.send(Ok(())).unwrap();
    }
    assert!(state.poll_runs(|_, _, _| panic!("accepted receipts must settle without replay")));
    assert_eq!(state.runs.iter().filter(|run| run.pending()).count(), 0);
    for run in &state.runs {
        assert_eq!(run.targets()[0].steps, [CommandStepResult::Written]);
    }
}

#[test]
fn disconnected_source_is_not_a_new_target_or_failure_but_its_old_failure_remains() {
    let mut pair = gated_pair(WriteBehavior::AmbiguousFailure);
    let inventory = inventory();
    let mut state = CustomCommandState::default();
    let first = capture_terminal_broadcast(
        PORT_A,
        "info".into(),
        &pair.manager,
        &inventory,
        |_, _| Ok(()),
        |_| DeviceRole::Receiver,
        |_, label| label.to_owned(),
    )
    .unwrap();
    let failed_id = state
        .submit_terminal_broadcast(first, |target, step| {
            admit_terminal(&mut pair.manager, &inventory, target, step)
        })
        .unwrap();
    for ready in &pair.ready {
        ready.recv_timeout(Duration::from_secs(2)).unwrap();
    }
    for release in &pair.release {
        release.send(()).unwrap();
    }
    until(|| {
        poll_terminal_runs(&mut state, &mut pair.manager, &inventory);
        !state.runs[0].pending()
    });
    assert_eq!(
        state.runs[0].summary(),
        CommandRunSummary {
            written: 1,
            pending: 0,
            failed: 1
        }
    );
    let old_results = state.runs[0].targets().to_vec();
    stopped(&mut pair.manager, PORT_A);
    pair.manager.disconnect(PORT_A).unwrap();
    assert_eq!(
        pair.manager.session(PORT_A).unwrap().state,
        ConnectionState::Disconnected
    );
    assert_eq!(
        pair.manager.session(PORT_B).unwrap().state,
        ConnectionState::Connected
    );
    let next = capture_terminal_broadcast(
        PORT_A,
        "help".into(),
        &pair.manager,
        &inventory,
        |_, _| Ok(()),
        |_| DeviceRole::Receiver,
        |_, label| label.to_owned(),
    )
    .unwrap();
    let next_id = state
        .submit_terminal_broadcast(next, |target, step| {
            admit_terminal(&mut pair.manager, &inventory, target, step)
        })
        .unwrap();
    until(|| {
        poll_terminal_runs(&mut state, &mut pair.manager, &inventory);
        !state.runs[0].pending()
    });
    assert_eq!(state.runs[0].id(), next_id);
    assert_eq!(
        state.runs[0].summary(),
        CommandRunSummary {
            written: 1,
            pending: 0,
            failed: 0
        }
    );
    assert_eq!(state.runs[0].targets().len(), 1);
    assert_eq!(state.runs[0].targets()[0].port, PORT_B);
    assert_eq!(
        state.runs[0].targets()[0].steps,
        [CommandStepResult::Written]
    );
    assert_eq!(state.runs[1].id(), failed_id);
    assert_eq!(state.runs[1].targets(), old_results);
    assert_eq!(
        state
            .terminal_runs()
            .map(CommandRun::id)
            .collect::<Vec<_>>(),
        [next_id, failed_id]
    );
    assert_eq!(pair.opens.load(Ordering::SeqCst), 2);
    assert_eq!(pair.writes[0].load(Ordering::SeqCst), 1);
    assert_eq!(pair.writes[1].load(Ordering::SeqCst), 2);
    pair.manager.shutdown().unwrap().join().unwrap();
    assert_payload(&mut pair.peers[0], b"in");
    assert_payload(&mut pair.peers[1], b"info\nhelp\n");
}
