use crate::discovery::ReceiverMonitor;
use crate::domain::{Inventory, ReceiverEndpoint, SerialEndpoint, SerialPortKind};
use crate::sessions::test_support::{completed, connected, fixture, peer, stopped, until};
use crate::sessions::{ConnectionState, SessionManager};
use crate::ui::flash::{Action, FlashState, Lease, OperationPins};
use crate::update::Job;
use std::collections::BTreeSet;
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::{Duration, Instant};

fn endpoint(port: &str, serial: &str) -> SerialEndpoint {
    SerialEndpoint {
        port_name: port.into(),
        label: "Synthetic USB console".into(),
        manufacturer: None,
        product: Some("Same display text for every fixture".into()),
        kind: SerialPortKind::Usb,
        serial_number: Some(serial.into()),
        vid: Some(0x1209),
        pid: Some(0x7692),
        usb_interface: None,
    }
}

fn inventory(endpoints: &[SerialEndpoint]) -> Inventory {
    Inventory {
        ports: endpoints.to_vec(),
        ..Inventory::default()
    }
}

fn selected_addresses() -> BTreeSet<String> {
    BTreeSet::from(["112233445566".into()])
}

fn connected_tracker(endpoint: &SerialEndpoint) -> (SessionManager, TcpStream, TcpListener) {
    let (mut manager, listener) = fixture();
    manager.update_inventory(&inventory(std::slice::from_ref(endpoint)), Instant::now());
    manager.connect(&endpoint.port_name).unwrap();
    let stream = peer(&listener);
    connected(&mut manager, &endpoint.port_name);
    (manager, stream, listener)
}

fn report_address(manager: &mut SessionManager, stream: &mut TcpStream, port: &str) {
    stream.write_all(b"Device address: 112233445566\n").unwrap();
    until(|| {
        let _ = manager.poll();
        manager.session(port).unwrap().identity.address.as_deref() == Some("112233445566")
    });
}

#[test]
fn initial_tracker_binding_uses_captured_usb_not_replacement_inventory() {
    let original = endpoint("COM41", "synthetic-original");
    let (mut manager, mut stream, _listener) = connected_tracker(&original);
    report_address(&mut manager, &mut stream, &original.port_name);
    let replacement = endpoint("COM41", "synthetic-replacement");
    manager.update_inventory(
        &inventory(std::slice::from_ref(&replacement)),
        Instant::now(),
    );
    manager.invalidate_inventory();
    let mut pins = OperationPins::freeze(
        selected_addresses(),
        [],
        &manager,
        std::slice::from_ref(&replacement),
    );
    let moved = endpoint("COM42", "SYNTHETIC-ORIGINAL");
    let moved_replacement = endpoint("COM43", "synthetic-replacement");
    let unrelated = endpoint("COM44", "synthetic-unrelated");
    let changed_pid = SerialEndpoint {
        port_name: "COM45".into(),
        pid: Some(0x7690),
        ..original.clone()
    };
    let changed_vid = SerialEndpoint {
        port_name: "COM46".into(),
        vid: Some(0x239a),
        ..original.clone()
    };
    assert_eq!(
        [
            pins.blocks_port("COM41", Some(&replacement)),
            pins.blocks_port("COM42", Some(&moved)),
            pins.blocks_port("COM43", Some(&moved_replacement)),
            pins.blocks_port("COM44", Some(&unrelated)),
            pins.blocks_port("COM45", Some(&changed_pid)),
            pins.blocks_port("COM46", Some(&changed_vid)),
        ],
        [true, true, false, false, false, false],
    );
    pins.observe_changes(&Default::default(), std::slice::from_ref(&moved));
    assert!(pins.blocks_port("COM42", None));
    assert!(!pins.blocks_port("COM43", Some(&moved_replacement)));
}

#[test]
fn late_tracker_binding_survives_a_new_unidentified_generation_and_renumbering() {
    let original = endpoint("COM51", "synthetic-late");
    let (mut manager, mut stream, listener) = connected_tracker(&original);
    let mut pins = OperationPins::freeze(selected_addresses(), [], &manager, &[]);
    assert!(!pins.blocks_port("COM51", Some(&original)));
    stream.write_all(b"Device address: 112233445566\n").unwrap();
    until(|| {
        let changes = manager.poll();
        pins.observe_changes(&changes, std::slice::from_ref(&original));
        pins.blocks_port("COM51", Some(&original))
    });
    let generation = manager.session("COM51").unwrap().generation;
    manager.disconnect("COM51").unwrap();
    drop(stream);
    stopped(&mut manager, "COM51");
    manager.connect("COM51").unwrap();
    let _new_stream = peer(&listener);
    connected(&mut manager, "COM51");
    assert_eq!(manager.session("COM51").unwrap().generation, generation + 1);
    assert_eq!(manager.session("COM51").unwrap().identity.address, None);
    let moved = endpoint("COM52", "synthetic-late");
    let unrelated = endpoint("COM53", "synthetic-other");
    assert_eq!(
        [
            pins.blocks_port("COM51", None),
            pins.blocks_port("COM52", Some(&moved)),
            pins.blocks_port("COM53", Some(&unrelated)),
        ],
        [true, true, false],
    );
}

#[test]
fn receiver_aliases_require_supported_usb_facts_and_do_not_block_unknown_consoles() {
    let receiver = ReceiverEndpoint {
        key: "synthetic-control".into(),
        path: b"synthetic-control".to_vec(),
        label: "Display text is not identity".into(),
        serial_number: Some("synthetic-receiver".into()),
    };
    let supported = SerialEndpoint {
        pid: Some(0x7690),
        ..endpoint("COM61", "SYNTHETIC-RECEIVER")
    };
    let wrong_pid = endpoint("COM62", "synthetic-receiver");
    let unknown = SerialEndpoint {
        serial_number: None,
        ..supported.clone()
    };
    let unknown = SerialEndpoint {
        port_name: "COM63".into(),
        ..unknown
    };
    let manager = SessionManager::new();
    let pins = OperationPins::freeze(
        selected_addresses(),
        [&receiver],
        &manager,
        std::slice::from_ref(&supported),
    );
    let state = FlashState {
        operation_pins: pins,
        ..FlashState::default()
    };
    assert!(state.blocks_address("112233445566"));
    assert!(!state.blocks_address("AABBCCDDEEFF"));
    assert_eq!(
        [
            state.operation_pins.blocks_port("COM61", None),
            state.operation_pins.blocks_port("COM62", Some(&wrong_pid)),
            state.operation_pins.blocks_port("COM63", Some(&unknown)),
        ],
        [true, false, false],
    );
}

#[test]
fn cancellation_retains_tracker_pins_and_leases_until_the_actual_worker_joins() {
    let original = endpoint("COM71", "synthetic-cancel");
    let (mut manager, mut stream, _listener) = connected_tracker(&original);
    report_address(&mut manager, &mut stream, "COM71");
    let token = manager.reserve("firmware-owner", None).unwrap();
    let mut state = FlashState {
        operation_pins: OperationPins::freeze(selected_addresses(), [], &manager, &[]),
        leases: vec![Lease::Serial {
            token,
            port: "firmware-owner".into(),
        }],
        ..FlashState::default()
    };
    let (cancelled_tx, cancelled_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    state.job = Some(
        Job::start(move |cancel, _| {
            while !cancel.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
            cancelled_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(3)).unwrap();
            Err("synthetic cancelled worker joined".into())
        })
        .unwrap(),
    );
    let moved = endpoint("COM72", "synthetic-cancel");
    let unrelated = endpoint("COM73", "synthetic-other");
    let mut monitor = ReceiverMonitor::new();
    state.cancel_activity();
    cancelled_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    assert!(state.poll_job().is_none());
    state.release_leases(&mut manager, &mut monitor);
    assert!(state.blocks_address("112233445566"));
    assert!(state.operation_pins.blocks_port("COM72", Some(&moved)));
    assert!(!state.operation_pins.blocks_port("COM73", Some(&unrelated)));
    assert!(manager.reserve("firmware-owner", None).is_err());
    release_tx.send(()).unwrap();
    let mut completion = None;
    until(|| {
        completion = state.poll_job();
        completion.is_some()
    });
    match completion.unwrap().0 {
        Err(error) => assert_eq!(error, "synthetic cancelled worker joined"),
        Ok(_) => panic!("cancelled worker unexpectedly succeeded"),
    }
    state.release_leases(&mut manager, &mut monitor);
    assert!(!state.blocks_address("112233445566"));
    assert!(!state.operation_pins.blocks_port("COM71", None));
    assert!(!state.operation_pins.blocks_port("COM72", Some(&moved)));
    let token = manager.reserve("firmware-owner", None).unwrap();
    manager.release(&token).unwrap();
}

struct ParsedSerialRead {
    stream: TcpStream,
    parsed: mpsc::Sender<()>,
    pending: bool,
}

impl std::io::Read for ParsedSerialRead {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        if std::mem::take(&mut self.pending) {
            self.parsed.send(()).unwrap();
        }
        let length = std::io::Read::read(&mut self.stream, bytes)?;
        self.pending = bytes[..length].contains(&b'\n');
        Ok(length)
    }
}

impl Write for ParsedSerialRead {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.stream.write(bytes)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.stream.flush()
    }
}

#[gpui_kit::test]
fn terminal_send_consumes_late_rf_before_admission_and_keeps_renumbered_usb_pinned(
    cx: &mut gpui_kit::TestAppContext,
) {
    let _ui_session = crate::ui::test_support::UiTestSession::new(cx);
    use crate::sessions::test_support::{socket_transport, with_transport_factory};
    use crate::ui::test_support::offline_workbench;
    use gpui_kit::VisualContext as _;
    use gpui_kit::component::input::InputEvent;
    use std::io::Read as _;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (parsed, parsed_reads) = mpsc::channel();
    let mut manager = with_transport_factory(move |_| {
        Ok(ParsedSerialRead {
            stream: socket_transport(address)?,
            parsed: parsed.clone(),
            pending: false,
        })
    });
    let original = endpoint("COM81", "synthetic-between-ticks");
    let unrelated = endpoint("COM83", "synthetic-unrelated-control");
    let initial_inventory = inventory(&[original.clone(), unrelated.clone()]);
    manager.update_inventory(&initial_inventory, Instant::now());
    manager.connect("COM81").unwrap();
    let mut original_stream = peer(&listener);
    connected(&mut manager, "COM81");
    manager.connect("COM83").unwrap();
    let mut unrelated_stream = peer(&listener);
    connected(&mut manager, "COM83");

    let runtime = crate::app_runtime::AppRuntime::new().unwrap();
    let executor = runtime.executor();

    let cx = cx.add_empty_window();
    let workbench = cx.new_window_entity(move |window, cx| {
        let mut this = offline_workbench(window, cx, manager, executor);
        this.inventory = initial_inventory;
        let receiver = ReceiverEndpoint {
            key: "synthetic-esb-owner".into(),
            path: b"synthetic-esb-owner".to_vec(),
            label: "Synthetic ESB receiver".into(),
            serial_number: Some("synthetic-esb-receiver-usb".into()),
        };
        this.monitor = ReceiverMonitor::fixture(receiver.clone(), 0, "112233445566", true);
        this.flash.operation_pins = OperationPins::freeze(
            selected_addresses(),
            [&receiver],
            &this.manager,
            &this.inventory.ports,
        );
        this.flash
            .leases
            .push(Lease::Hid(this.monitor.reserve(&receiver).unwrap()));
        this.sync_tabs(window, cx);
        this
    });
    workbench.update(cx, |this, _| {
        assert_eq!(
            this.manager.session("COM81").unwrap().identity.address,
            None
        );
        assert!(this.flash_check_port("COM81").is_ok());
        assert_eq!(this.flash.leases.len(), 1);
    });

    original_stream
        .write_all(b"Device address: 112233445566\n")
        .unwrap();
    parsed_reads.recv_timeout(Duration::from_secs(3)).unwrap();
    cx.update(|window, cx| {
        workbench.update(cx, |this, cx| {
            assert_eq!(
                this.manager.session("COM81").unwrap().identity.address,
                None
            );
            this.terminals["COM81"].input.update(cx, |input, cx| {
                input.set_value("info", window, cx);
                cx.emit(InputEvent::PressEnter {
                    secondary: false,
                    shift: false,
                });
            });
        });
    });
    cx.run_until_parked();
    workbench.update(cx, |this, _| {
        assert_eq!(
            this.manager
                .session("COM81")
                .unwrap()
                .identity
                .address
                .as_deref(),
            Some("112233445566"),
        );
        assert_eq!(
            this.terminals["COM81"].pending.as_ref().unwrap().error,
            Some(crate::i18n::tr("firmware.error.port_updating")),
        );
        assert_eq!(this.pending_writes.len(), 0);
        assert!(this.flash_check_port("COM81").is_err());
    });

    let moved = endpoint("COM82", "synthetic-between-ticks");
    workbench.update(cx, |this, cx| {
        this.inventory.ports.push(moved);
        this.manager
            .update_inventory(&this.inventory, Instant::now());
        this.toggle_connection("COM82", cx);
        assert_eq!(
            this.error,
            Some(format!(
                "COM82: {}",
                crate::i18n::tr("firmware.error.port_updating")
            )),
        );
        assert!(this.manager.session("COM82").is_none());
        assert!(this.flash_check_port("COM81").is_err());
        assert!(this.flash_check_port("COM82").is_err());
        assert!(this.flash_check_port("COM83").is_ok());
    });
    cx.update(|window, cx| {
        workbench.update(cx, |this, cx| {
            this.focus_serial("COM83");
            this.terminals["COM83"].input.update(cx, |input, cx| {
                input.set_value("help", window, cx);
                cx.emit(InputEvent::PressEnter {
                    secondary: false,
                    shift: false,
                });
            });
        });
    });
    cx.run_until_parked();
    until(|| {
        workbench.update(cx, |this, _| {
            this.poll_writes();
            this.pending_writes.is_empty()
        })
    });
    workbench.update(cx, |this, cx| {
        assert_eq!(this.terminals["COM83"].input.read(cx).value().as_ref(), "");
        assert!(this.terminals["COM83"].pending.is_none());
        assert_eq!(this.flash.leases.len(), 1);
        this.manager.shutdown().unwrap().join().unwrap();
    });
    let mut blocked_bytes = Vec::new();
    original_stream.read_to_end(&mut blocked_bytes).unwrap();
    let mut control_bytes = Vec::new();
    unrelated_stream.read_to_end(&mut control_bytes).unwrap();
    assert_eq!(blocked_bytes, b"");
    assert_eq!(control_bytes, b"help\n");
}

struct HeldPartialWrite {
    stream: TcpStream,
    ready: mpsc::Sender<()>,
    release: mpsc::Receiver<()>,
    first: bool,
}

impl std::io::Read for HeldPartialWrite {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        std::io::Read::read(&mut self.stream, bytes)
    }
}

impl Write for HeldPartialWrite {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if std::mem::take(&mut self.first) {
            self.stream.write_all(&bytes[..1])?;
            self.ready.send(()).unwrap();
            self.release.recv_timeout(Duration::from_secs(3)).unwrap();
            return Ok(1);
        }
        self.stream.write(&bytes[..bytes.len().min(1)])
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.stream.flush()
    }
}

fn serial_handoff_rejects_pending_writes(
    cx: &mut gpui_kit::TestAppContext,
    action: impl FnOnce(SerialEndpoint) -> Action,
) {
    use crate::sessions::test_support::{socket_transport, with_transport_factory};
    use crate::ui::test_support::offline_workbench;
    use gpui_kit::VisualContext as _;
    use std::io::Read as _;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (ready, held) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    let gate = parking_lot::Mutex::new(Some(gate));
    let mut manager = with_transport_factory(move |_| {
        Ok(HeldPartialWrite {
            stream: socket_transport(address)?,
            ready: ready.clone(),
            release: gate.lock().take().unwrap(),
            first: true,
        })
    });
    let original = endpoint("COM91", "synthetic-write-handoff");
    let action = action(original.clone());
    let initial_inventory = inventory(std::slice::from_ref(&original));
    manager.update_inventory(&initial_inventory, Instant::now());
    manager.connect("COM91").unwrap();
    let mut stream = peer(&listener);
    connected(&mut manager, "COM91");
    let generation = manager.session("COM91").unwrap().generation;
    let mut active = manager.send_line("COM91", "info").unwrap();
    held.recv_timeout(Duration::from_secs(3)).unwrap();
    let mut queued = manager.send_line("COM91", "help").unwrap();

    let runtime = crate::app_runtime::AppRuntime::new().unwrap();
    let executor = runtime.executor();

    let cx = cx.add_empty_window();
    let workbench = cx.new_window_entity(move |window, cx| {
        let mut this = offline_workbench(window, cx, manager, executor);
        this.inventory = initial_inventory;
        this
    });
    workbench.update(cx, |this, _| {
        assert!(!this.manager.writes_idle("COM91"));
        this.reserve_flash_action(action.clone());
        assert!(this.flash.error.is_some());
        assert!(this.flash.pending.is_none());
        assert!(this.flash.job.is_none());
        assert!(this.flash.leases.is_empty());
        assert!(!this.manager.command_port_reserved("COM91"));
        let session = this.manager.session("COM91").unwrap();
        assert_eq!(session.state, ConnectionState::Connected);
        assert_eq!(session.generation, generation);
        assert!(session.last_error.is_none());
    });
    assert!(active.poll().is_none());
    assert!(queued.poll().is_none());

    release.send(()).unwrap();
    until(|| workbench.update(cx, |this, _| this.manager.writes_idle("COM91")));
    assert_eq!(completed(&mut active), Ok(()));
    assert_eq!(completed(&mut queued), Ok(()));
    let mut bytes = [0; 10];
    stream.read_exact(&mut bytes).unwrap();
    assert_eq!(
        &bytes, b"info\nhelp\n",
        "accepted commands must finish intact"
    );

    workbench.update(cx, |this, _| {
        assert!(
            this.flash.pending.is_none(),
            "rejected actions must not retry"
        );
        assert_eq!(
            this.manager.session("COM91").unwrap().state,
            ConnectionState::Connected,
        );
        this.reserve_flash_action(action);
        assert!(this.flash.error.is_none());
        assert!(this.flash.pending.is_some());
        assert_eq!(this.flash.leases.len(), 1);
        assert!(this.manager.command_port_reserved("COM91"));
        stopped(&mut this.manager, "COM91");
        let Lease::Serial { token, .. } = &this.flash.leases[0] else {
            panic!("serial handoff must reserve the serial owner");
        };
        assert!(this.manager.reservation_ready(token).unwrap());
        // Do not poll_flash: executing the admitted action would touch hardware.
        this.flash.pending = None;
        this.release_flash_lease();
        assert!(!this.manager.command_port_reserved("COM91"));
        if let Some(join) = this.manager.shutdown() {
            join.join().unwrap();
        }
    });
    assert_eq!(stream.read(&mut [0; 1]).unwrap(), 0);
}

#[gpui_kit::test]
fn enter_dfu_admission_preserves_accepted_serial_writes(cx: &mut gpui_kit::TestAppContext) {
    let _ui_session = crate::ui::test_support::UiTestSession::new(cx);
    serial_handoff_rejects_pending_writes(cx, Action::Enter);
}

#[gpui_kit::test]
fn opendfu_admission_preserves_accepted_serial_writes(cx: &mut gpui_kit::TestAppContext) {
    let _ui_session = crate::ui::test_support::UiTestSession::new(cx);
    serial_handoff_rejects_pending_writes(cx, |port| {
        Action::OpenDfu(
            port,
            super::support::local_image("synthetic-handoff.hex"),
            crate::opendfu::Options::default(),
        )
    });
}

#[gpui_kit::test]
fn opendfu_planning_requires_unambiguous_structured_bootloader_metadata(
    cx: &mut gpui_kit::TestAppContext,
) {
    let _ui_session = crate::ui::test_support::UiTestSession::new(cx);
    use crate::domain::{InterfaceDescription, UsbInterface};
    use crate::ui::FeaturePage;
    use crate::ui::flash::Mode;
    use crate::ui::test_support::offline_workbench;
    use gpui_kit::VisualContext as _;

    let expected = SerialEndpoint {
        label: "COM10 — Open DFU Bootloader".into(),
        manufacturer: Some("Microsoft".into()),
        product: Some("USB串行设备 (COM10)".into()),
        vid: Some(0x1915),
        pid: Some(0x521f),
        usb_interface: Some(UsbInterface {
            number: Some(0),
            description: InterfaceDescription::Value("Open DFU Bootloader".into()),
        }),
        ..endpoint("COM10", "SYNTHETIC-OPENDFU")
    };
    let (manager, _listener) = fixture();
    let runtime = crate::app_runtime::AppRuntime::new().unwrap();
    let executor = runtime.executor();

    let cx = cx.add_empty_window();
    let original = expected.clone();
    let workbench = cx.new_window_entity(move |window, cx| {
        let mut this = offline_workbench(window, cx, manager, executor);
        this.active_page = FeaturePage::Uf2;
        this.inventory = inventory(std::slice::from_ref(&original));
        this.flash.mode = Mode::OpenDfu;
        this.flash.selected_port = Some(original.port_name);
        this.flash.image = Some(super::support::local_image("synthetic-application.hex"));
        this
    });
    workbench.update(cx, |this, _| {
        let Action::OpenDfu(frozen, _, _) = this.selected_flash_action().unwrap() else {
            panic!("native bootloader metadata must admit the OpenDFU action");
        };
        assert_eq!(frozen.port_name, "COM10");
        assert_eq!(frozen.product, expected.product);
        assert_eq!(frozen.manufacturer, expected.manufacturer);
        assert_eq!(frozen.usb_interface, expected.usb_interface);
        assert!(this.flash.job.is_none(), "planning must not open hardware");
        let mut relabeled = expected.clone();
        relabeled.label = "COM10 — USB串行设备".into();
        this.inventory = inventory(&[relabeled]);
        assert!(matches!(
            this.selected_flash_action(),
            Ok(Action::OpenDfu(_, _, _))
        ));
        assert!(this.flash.leases.is_empty());

        let mut invalid = Vec::new();
        let mut generic = expected.clone();
        generic.usb_interface.as_mut().unwrap().description = InterfaceDescription::Unavailable;
        invalid.push(generic.clone());
        generic.product = None;
        generic.manufacturer = Some("Open DFU Bootloader".into());
        invalid.push(generic.clone());
        generic.manufacturer = None;
        invalid.push(generic);
        for description in [
            InterfaceDescription::Ambiguous,
            InterfaceDescription::Value("SlimeNRF Raw Data CDC".into()),
        ] {
            let mut conflicting = expected.clone();
            conflicting.product = Some("Open DFU Bootloader".into());
            conflicting.usb_interface.as_mut().unwrap().description = description;
            invalid.push(conflicting);
        }
        for endpoint in invalid {
            this.inventory = inventory(std::slice::from_ref(&endpoint));
            assert!(
                this.selected_flash_action().is_err(),
                "must reject unsupported structured identity: {endpoint:?}"
            );
        }

        let mut fallback = expected.clone();
        fallback.product = Some("Open DFU Bootloader".into());
        fallback.usb_interface.as_mut().unwrap().description = InterfaceDescription::Unavailable;
        this.inventory = inventory(std::slice::from_ref(&fallback));
        assert!(matches!(
            this.selected_flash_action(),
            Ok(Action::OpenDfu(_, _, _))
        ));

        this.inventory = inventory(std::slice::from_ref(&expected));
        this.inventory.serial_enumeration_failed = true;
        assert!(this.selected_flash_action().is_err());
        this.inventory = inventory(&[expected.clone(), expected.clone()]);
        assert!(this.selected_flash_action().is_err());
        let mut moved = expected.clone();
        moved.port_name = "COM11".into();
        this.inventory = inventory(&[moved]);
        assert!(this.selected_flash_action().is_err());
        this.inventory.ports.clear();
        assert!(this.selected_flash_action().is_err());
        assert!(this.flash.job.is_none());
        assert!(this.flash.leases.is_empty());
    });
}
