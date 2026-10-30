use super::{
    TabSelection, TerminalConnect, TerminalExport, consume_terminal_input_event,
    take_terminal_command,
};
use crate::application::test_support::offline_workbench;
use crate::{
    application::command_panel::CommandConnection, domain::SerialEndpoint,
    sessions::ConnectionState,
};
use futures_util::FutureExt as _;
use gpui_kit::component::input::InputEvent;
use gpui_kit::{BackgroundExecutor, PlatformDispatcher, Priority, RunnableVariant};
use std::sync::{Arc, mpsc};
use std::time::Duration;

#[test]
fn tab_selection_transitions_keep_existing_selection_when_sessions_arrive() {
    let mut tabs = TabSelection::default();
    tabs.sync([]);
    assert_eq!(tabs.active, None);
    tabs.sync(["A"]);
    assert_eq!(tabs.active.as_deref(), Some("A"));
    tabs.sync(["A", "B", "C"]);
    assert_eq!(tabs.active.as_deref(), Some("A"));
    tabs.select("C");
    tabs.sync(["A", "B", "C", "D"]);
    assert_eq!(tabs.active.as_deref(), Some("C"));
    tabs.select("B");
    tabs.sync(["A", "B", "C", "D"]);
    assert_eq!(tabs.active.as_deref(), Some("B"));
}

#[test]
fn tab_selection_retains_history_and_only_falls_back_if_session_is_removed() {
    let mut tabs = TabSelection::default();
    let mut sessions = [
        ("A", ConnectionState::Connected),
        ("B", ConnectionState::Connected),
    ];
    tabs.sync(sessions.iter().map(|(port, _)| *port));
    tabs.select("B");
    sessions[1].1 = ConnectionState::Disconnected;
    tabs.sync(sessions.iter().map(|(port, _)| *port));
    assert_eq!(tabs.active.as_deref(), Some("B"));
    tabs.select("A");
    assert_eq!(sessions[0].1, ConnectionState::Connected);
    assert_eq!(sessions[1].1, ConnectionState::Disconnected);
    tabs.sync(["B"]);
    assert_eq!(tabs.active.as_deref(), Some("B"));
    tabs.sync([]);
    assert_eq!(tabs.active, None);
}

fn terminal_intent() -> Option<TerminalConnect> {
    Some(TerminalConnect {
        connection: CommandConnection {
            port: "COM7".into(),
            generation: Some(4),
        },
        endpoint: Some(SerialEndpoint {
            port_name: "COM7".into(),
            label: "Receiver".into(),
            manufacturer: None,
            product: None,
            kind: crate::domain::SerialPortKind::Usb,
            serial_number: Some("USB-A".into()),
            vid: Some(0x1209),
            pid: Some(0x7690),
            usb_interface: None,
        }),
        command: "info".into(),
        error: None,
    })
}

#[test]
fn editing_then_restoring_terminal_input_cannot_revive_waiting_command() {
    let mut pending = terminal_intent();
    let mut status = Some("Connecting".into());
    // These are the events consumed by the actual InputState subscription, before Opened.
    consume_terminal_input_event(&InputEvent::Change, &mut pending, &mut status);
    consume_terminal_input_event(&InputEvent::Change, &mut pending, &mut status);
    // The same resume consumer receives the restored original text after Opened.
    assert!(take_terminal_command(&mut pending, &mut status, "info").is_none());
    assert!(status.is_none());
    let mut unedited = terminal_intent();
    let command = take_terminal_command(&mut unedited, &mut status, "info").unwrap();
    assert_eq!(command.command, "info");
    assert!(unedited.is_none());
}

#[test]
fn pending_terminal_send_loses_console_admission_when_unknown_sibling_arrives() {
    let pending = terminal_intent().unwrap();
    let endpoint = pending.endpoint.as_ref().unwrap().clone();
    let mut inventory = crate::domain::Inventory {
        ports: vec![endpoint.clone()],
        receivers: vec![crate::domain::ReceiverEndpoint {
            key: "receiver".into(),
            path: b"receiver".to_vec(),
            label: "Receiver".into(),
            serial_number: Some("USB-A".into()),
        }],
        ..Default::default()
    };
    assert!(
        pending
            .validate_endpoint(inventory.ports.first(), &inventory)
            .is_ok()
    );
    let mut sibling = endpoint;
    sibling.port_name = "COM8".into();
    inventory.ports.push(sibling);
    assert!(
        pending
            .validate_endpoint(inventory.ports.first(), &inventory)
            .is_err()
    );
}

#[test]
fn terminal_send_rejects_raw_ambiguous_or_replaced_interface_and_failed_inventory() {
    use crate::domain::{InterfaceDescription, Inventory, UsbInterface};
    let pending = terminal_intent().unwrap();
    for scenario in 0..4 {
        let mut endpoint = pending.endpoint.as_ref().unwrap().clone();
        endpoint.usb_interface = Some(UsbInterface {
            number: Some(2),
            description: match scenario {
                0 => InterfaceDescription::Value("SlimeNRF Raw Data CDC".into()),
                1 => InterfaceDescription::Ambiguous,
                _ => InterfaceDescription::Unavailable,
            },
        });
        let inventory = Inventory {
            ports: vec![endpoint],
            serial_enumeration_failed: scenario == 3,
            ..Default::default()
        };
        assert!(
            pending
                .validate_endpoint(inventory.ports.first(), &inventory)
                .is_err()
        );
    }
}

#[test]
fn ordinary_tracker_terminal_keeps_manual_policy_and_ignores_display_renaming() {
    let mut pending = terminal_intent().unwrap();
    let endpoint = pending.endpoint.as_mut().unwrap();
    endpoint.pid = Some(1);
    let mut current = endpoint.clone();
    current.label = "Updated tracker label".into();
    let inventory = crate::domain::Inventory {
        ports: vec![current],
        ..Default::default()
    };
    assert!(
        pending
            .validate_endpoint(inventory.ports.first(), &inventory)
            .is_ok()
    );
}

struct HeldFileDispatcher(mpsc::Sender<RunnableVariant>);

impl PlatformDispatcher for HeldFileDispatcher {
    fn is_main_thread(&self) -> bool {
        false
    }

    fn dispatch(&self, runnable: RunnableVariant, _: Priority) {
        self.0.send(runnable).unwrap();
    }

    fn dispatch_on_main_thread(&self, _: RunnableVariant, _: Priority) {
        panic!("terminal file worker must not use the UI dispatcher");
    }

    fn dispatch_after(&self, _: Duration, _: RunnableVariant) {
        panic!("terminal file worker must not need a timer");
    }

    fn spawn_realtime(&self, _: Box<dyn FnOnce() + Send>) {
        panic!("terminal file worker must not need a realtime thread");
    }
}

#[test]
fn accepted_export_finishes_after_its_ui_observer_is_cancelled_for_shutdown() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("synthetic-terminal.txt");
    let (scheduled, runnables) = mpsc::channel();
    let executor = BackgroundExecutor::new(Arc::new(HeldFileDispatcher(scheduled)));
    let mut export = TerminalExport::default();
    let (join, output) = export.write(
        path.clone(),
        "Synthetic terminal\nNo device commands\n".to_owned(),
        &executor,
    );
    export.task = Some(executor.spawn(async move {
        join.await;
    }));
    assert!(export.busy());

    let shutdown = export.shutdown().unwrap();
    drop(export);
    assert!(shutdown.clone().now_or_never().is_none());
    assert!(!path.exists());
    runnables
        .recv_timeout(Duration::from_secs(3))
        .unwrap()
        .run();
    assert_eq!(shutdown.now_or_never(), Some(()));
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "Synthetic terminal\nNo device commands\n"
    );
    assert_eq!(output.lock().take(), Some(Ok(path)));
}

fn terminal_fixture(
    cx: &mut gpui_kit::TestAppContext,
) -> (
    gpui_kit::Entity<super::Workbench>,
    &mut gpui_kit::VisualTestContext,
    crate::app_runtime::AppRuntime,
    std::net::TcpListener,
) {
    use crate::sessions::test_support::{connected, fixture, peer, stopped, until};
    use gpui_kit::VisualContext as _;
    use std::io::Write as _;

    let (mut manager, listener) = fixture();
    for port in ["A", "B"] {
        manager.connect(port).unwrap();
        let mut stream = peer(&listener);
        connected(&mut manager, port);
        stream
            .write_all(b"Synthetic terminal transcript\nNo hardware commands\n")
            .unwrap();
        until(|| {
            let _ = manager.poll();
            manager.session(port).unwrap().terminal.text()
                == "Synthetic terminal transcript\nNo hardware commands\n"
        });
        manager.disconnect(port).unwrap();
        stopped(&mut manager, port);
    }
    let runtime = crate::app_runtime::AppRuntime::new().unwrap();
    let executor = runtime.executor();

    let cx = cx.add_empty_window();
    let workbench = cx.new_window_entity(move |window, cx| {
        let mut workbench = offline_workbench(window, cx, manager, executor);
        workbench.sync_tabs(window, cx);
        workbench
    });
    (workbench, cx, runtime, listener)
}

#[gpui_kit::test]
fn dismissing_an_inactive_terminal_releases_its_ui_without_touching_the_active_terminal(
    cx: &mut gpui_kit::TestAppContext,
) {
    let _ui_session = crate::test_support::UiTestSession::new(cx);
    let (workbench, cx, _runtime, _listener) = terminal_fixture(cx);
    let (receipt, completion) = crate::sessions::test_support::write_receipt();
    let (input, transcript) = cx.update(|window, cx| {
        workbench.update(cx, |this, cx| {
            this.terminals.get_mut("A").unwrap().soft_wrap = false;
            this.terminals.get_mut("B").unwrap().soft_wrap = true;
            this.focus_serial("A");
            this.sync_tabs(window, cx);
            this.focus_serial("B");
            this.terminals["B"].transcript_focus.focus(window, cx);
            let generation = this.manager.session("B").unwrap().generation;
            this.pending_writes.push(super::PendingWrite {
                port: "B".into(),
                generation,
                receipt,
            });
            this.terminals.get_mut("B").unwrap().pending = Some(TerminalConnect {
                connection: CommandConnection {
                    port: "B".into(),
                    generation: Some(generation),
                },
                endpoint: None,
                command: "unsent B draft".into(),
                error: None,
            });
            this.terminals["B"].input.update(cx, |input, cx| {
                input.set_value("unsent B draft", window, cx);
            });
            this.terminals
                .get_mut("A")
                .unwrap()
                .history
                .record_submission("old A command", &Ok::<(), ()>(()));
            this.refresh_transcript("A", window, cx);
            assert!(!this.terminals["A"].soft_wrap);
            assert!(this.terminals["B"].soft_wrap);
            let input = this.terminals["A"].input.downgrade();
            let transcript = std::rc::Rc::downgrade(&this.terminals["A"].transcript);
            assert!(!this.can_request_terminal_close("B"));
            this.request_terminal_close("B", window, cx);
            assert!(this.can_request_terminal_close("A"));
            this.request_terminal_close("A", window, cx);
            assert_eq!(this.tabs.active.as_deref(), Some("B"));
            assert_eq!(
                this.command_focus,
                Some(crate::application::command_panel::CommandFocus::Serial(
                    "B".into()
                ))
            );
            assert_eq!(
                this.terminals["B"].input.read(cx).value().as_ref(),
                "unsent B draft"
            );
            assert_eq!(
                this.terminals["B"].pending.as_ref().unwrap().command,
                "unsent B draft"
            );
            assert_eq!(this.pending_writes.len(), 1);
            assert!(this.terminals["B"].soft_wrap);
            assert_eq!(this.manager.sessions().count(), 1);
            assert!(
                this.terminals["B"]
                    .transcript_focus
                    .contains_focused(window, cx)
            );
            (input, transcript)
        })
    });
    cx.run_until_parked();
    cx.update(|_, _| {});
    assert!(input.upgrade().is_none());
    assert!(transcript.upgrade().is_none());
    completion.send(Ok(())).unwrap();
    workbench.update(cx, |this, _| {
        assert!(this.poll_writes());
        assert_eq!(this.pending_writes.len(), 0);
        assert_eq!(this.tabs.active.as_deref(), Some("B"));
    });
}

#[gpui_kit::test]
fn pending_receipts_and_export_owners_prevent_terminal_dismissal(
    cx: &mut gpui_kit::TestAppContext,
) {
    let _ui_session = crate::test_support::UiTestSession::new(cx);
    let (workbench, cx, _runtime, _listener) = terminal_fixture(cx);
    let (receipt, completion) = crate::sessions::test_support::write_receipt();
    cx.update(|window, cx| {
        workbench.update(cx, |this, cx| {
            let generation = this.manager.session("A").unwrap().generation;
            this.pending_writes.push(super::PendingWrite {
                port: "A".into(),
                generation,
                receipt,
            });
            assert!(!this.can_request_terminal_close("A"));
            this.request_terminal_close("A", window, cx);
            assert!(!this.manager.is_close_requested("A"));
            assert_eq!(this.tabs.active.as_deref(), Some("A"));
            assert_eq!(this.terminals.len(), 2);
            assert_eq!(this.pending_writes.len(), 1);
            completion.send(Ok(())).unwrap();
            assert!(this.poll_writes());
            this.custom_commands
                .pending_history
                .push(("A".into(), "accepted custom info".into()));
            assert!(!this.can_dismiss_terminal("A"));
            this.complete_custom_terminal_clear(window, cx);
            assert_eq!(
                this.terminals
                    .get_mut("A")
                    .unwrap()
                    .history
                    .navigate(true, ""),
                Some("accepted custom info".into())
            );
            this.custom_commands.terminal_clear = Some(("A".into(), String::new()));
            assert!(!this.can_dismiss_terminal("A"));
            this.complete_custom_terminal_clear(window, cx);
            this.export_terminal_log("A", cx);
            this.export_terminal_log("A", cx);
            assert!(this.terminals["A"].export.busy());
            assert!(!this.can_dismiss_terminal("A"));
            assert!(!this.can_request_terminal_close("A"));
            this.request_terminal_close("A", window, cx);
            assert!(!this.manager.is_close_requested("A"));
        });
    });
    cx.simulate_new_path_selection(|_| None);
    cx.run_until_parked();
    cx.update(|window, cx| {
        workbench.update(cx, |this, cx| {
            assert!(!this.terminals["A"].export.busy());
            this.request_terminal_close("A", window, cx);
            assert_eq!(this.tabs.active.as_deref(), Some("B"));
            assert_eq!(this.terminals.len(), 1);
        });
    });
}

#[gpui_kit::test]
fn terminal_close_waits_for_worker_join_and_cleans_up_on_another_page(
    cx: &mut gpui_kit::TestAppContext,
) {
    let _ui_session = crate::test_support::UiTestSession::new(cx);
    use crate::sessions::test_support::{held_worker_session, stopped};

    let (workbench, cx, _runtime, _listener) = terminal_fixture(cx);
    let (display, release) = cx.update(|window, cx| {
        workbench.update(cx, |this, cx| {
            let held = held_worker_session(&mut this.manager, "A");
            this.focus_serial("B");
            this.terminals["B"].input.update(cx, |input, cx| {
                input.set_value("retained B draft", window, cx);
            });
            this.terminals["A"].input.update(cx, |input, cx| {
                input.set_value("do not send", window, cx);
            });
            let generation = this.manager.session("A").unwrap().generation;
            assert!(this.can_request_terminal_close("A"));
            this.request_terminal_close("A", window, cx);
            assert!(this.manager.is_close_requested("A"));
            assert!(!this.can_request_terminal_close("A"));
            assert!(!this.can_dismiss_terminal("A"));
            assert_eq!(
                this.manager.session("A").unwrap().state,
                ConnectionState::Closing,
            );
            this.request_terminal_close("A", window, cx);
            this.toggle_connection("A", window, cx);
            this.send("A", window, cx);
            this.send_terminal_all("A", window, cx);
            assert!(!this.queue_write("A", "help"));
            assert_eq!(this.manager.session("A").unwrap().generation, generation);
            assert_eq!(
                this.terminals["A"].input.read(cx).value().as_ref(),
                "do not send",
            );
            assert!(this.terminals["A"].pending.is_none());
            assert_eq!(this.terminal_command_runs().count(), 0);
            this.active_page = crate::application::FeaturePage::Settings;
            this.scanning_inventory = true;
            this.advance_serial_sessions(window, cx);
            assert_eq!(this.terminals.len(), 2);
            held
        })
    });
    display.push(b"Last retained line\n");
    display.close(None, 0);
    cx.update(|window, cx| {
        workbench.update(cx, |this, cx| {
            let _ = this.manager.poll();
            assert_eq!(
                this.manager.session("A").unwrap().state,
                ConnectionState::Disconnected,
            );
            this.advance_serial_sessions(window, cx);
            assert!(this.manager.is_close_requested("A"));
            assert_eq!(this.terminals.len(), 2);
            assert_eq!(
                this.manager.session("A").unwrap().terminal.text(),
                "Last retained line\n",
            );
            assert_eq!(this.tabs.active.as_deref(), Some("B"));
        });
    });
    release.send(()).unwrap();
    cx.update(|window, cx| {
        workbench.update(cx, |this, cx| {
            stopped(&mut this.manager, "A");
            assert!(this.advance_background_work(window, cx));
            assert!(this.manager.session("A").is_none());
            assert_eq!(this.terminals.len(), 1);
            assert_eq!(this.tabs.active.as_deref(), Some("B"));
            assert_eq!(
                this.terminals["B"].input.read(cx).value().as_ref(),
                "retained B draft",
            );
            assert!(!this.advance_background_work(window, cx));
            assert!(!this.advance_serial_sessions(window, cx));
        });
    });
}

#[gpui_kit::test]
fn a_replacement_generation_does_not_inherit_a_previous_terminal_close(
    cx: &mut gpui_kit::TestAppContext,
) {
    let _ui_session = crate::test_support::UiTestSession::new(cx);
    use crate::sessions::test_support::{connected, peer, stopped};

    let (workbench, cx, _runtime, listener) = terminal_fixture(cx);
    workbench.update(cx, |this, _| this.manager.connect("A").unwrap());
    let _old_peer = peer(&listener);
    let old_generation = cx.update(|window, cx| {
        workbench.update(cx, |this, cx| {
            connected(&mut this.manager, "A");
            let generation = this.manager.session("A").unwrap().generation;
            this.request_terminal_close("A", window, cx);
            assert!(this.manager.is_close_requested("A"));
            assert!(this.manager.connect("A").is_err());
            assert_eq!(this.manager.session("A").unwrap().generation, generation);
            stopped(&mut this.manager, "A");
            assert!(this.manager.forget("A"));
            this.manager.connect("A").unwrap();
            generation
        })
    });
    let _new_peer = peer(&listener);
    cx.update(|window, cx| {
        workbench.update(cx, |this, cx| {
            connected(&mut this.manager, "A");
            assert_ne!(
                this.manager.session("A").unwrap().generation,
                old_generation
            );
            assert!(!this.manager.is_close_requested("A"));
            this.sync_tabs(window, cx);
            assert_eq!(
                this.manager.session("A").unwrap().state,
                ConnectionState::Connected,
            );
            assert_eq!(this.terminals.len(), 2);
            assert_eq!(this.tabs.active.as_deref(), Some("A"));
            this.manager.disconnect("A").unwrap();
            stopped(&mut this.manager, "A");
        });
    });
}

#[gpui_kit::test]
fn future_terminal_run_steps_keep_the_target_terminal_owned_after_writes_settle(
    cx: &mut gpui_kit::TestAppContext,
) {
    let _ui_session = crate::test_support::UiTestSession::new(cx);
    use crate::features::commands::batch::CommandStepResult;
    use crate::sessions::test_support::{connected, peer, stopped, until};
    use std::io::Read as _;

    let (workbench, cx, _runtime, listener) = terminal_fixture(cx);
    workbench.update(cx, |this, _| this.manager.connect("A").unwrap());
    let mut stream = peer(&listener);
    cx.update(|window, cx| {
        workbench.update(cx, |this, cx| {
            connected(&mut this.manager, "A");
            let mut endpoint = terminal_intent().unwrap().endpoint.unwrap();
            endpoint.port_name = "A".into();
            endpoint.pid = Some(1);
            this.inventory.ports.push(endpoint);
            this.submit_terminal_send_all("B", "help\ninfo".into(), cx)
                .unwrap();
            this.complete_custom_terminal_clear(window, cx);
        });
    });
    let mut bytes = [0; 5];
    stream.read_exact(&mut bytes).unwrap();
    assert_eq!(&bytes, b"help\n");
    cx.update(|window, cx| {
        workbench.update(cx, |this, cx| {
            until(|| this.manager.writes_idle("A"));
            this.manager.disconnect("A").unwrap();
            stopped(&mut this.manager, "A");
            assert!(this.manager.can_request_close("A"));
            assert_eq!(
                this.terminal_command_runs().next().unwrap().targets()[0].steps,
                [CommandStepResult::Queued, CommandStepResult::NotSubmitted],
            );
            assert!(!this.can_request_terminal_close("A"));
            assert!(!this.can_dismiss_terminal("A"));
            this.request_terminal_close("A", window, cx);
            assert!(!this.manager.is_close_requested("A"));
            assert_eq!(this.terminals.len(), 2);
            this.request_terminal_close("B", window, cx);
            assert!(this.manager.session("B").is_none());
            assert!(this.poll_custom_commands());
            assert!(!this.terminal_command_runs().next().unwrap().pending());
            this.complete_custom_terminal_clear(window, cx);
            assert!(this.can_request_terminal_close("A"));
            this.request_terminal_close("A", window, cx);
            assert_eq!(this.terminals.len(), 0);
        });
    });
    let mut remainder = Vec::new();
    stream.read_to_end(&mut remainder).unwrap();
    assert_eq!(remainder, b"");
}

#[gpui_kit::test]
fn a_retained_export_file_join_blocks_terminal_close_without_an_observer(
    cx: &mut gpui_kit::TestAppContext,
) {
    let _ui_session = crate::test_support::UiTestSession::new(cx);
    let (workbench, cx, _runtime, _listener) = terminal_fixture(cx);
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("held-terminal.txt");
    let (scheduled, runnables) = mpsc::channel();
    let executor = BackgroundExecutor::new(Arc::new(HeldFileDispatcher(scheduled)));
    let (join, output) = cx.update(|window, cx| {
        workbench.update(cx, |this, cx| {
            let write = this.terminals.get_mut("A").unwrap().export.write(
                path.clone(),
                "retained export bytes\n".into(),
                &executor,
            );
            assert!(!this.can_request_terminal_close("A"));
            this.request_terminal_close("A", window, cx);
            assert!(!this.manager.is_close_requested("A"));
            assert_eq!(this.terminals.len(), 2);
            write
        })
    });
    runnables
        .recv_timeout(Duration::from_secs(3))
        .unwrap()
        .run();
    assert_eq!(join.now_or_never(), Some(()));
    assert_eq!(output.lock().take(), Some(Ok(path.clone())));
    assert_eq!(
        std::fs::read_to_string(path).unwrap(),
        "retained export bytes\n",
    );
    cx.update(|window, cx| {
        workbench.update(cx, |this, cx| {
            this.terminals.get_mut("A").unwrap().export = TerminalExport::default();
            this.request_terminal_close("A", window, cx);
            assert!(this.manager.session("A").is_none());
            assert_eq!(this.terminals.len(), 1);
        });
    });
}

#[gpui_kit::test]
fn closing_the_active_terminal_blurs_removed_focus_and_selects_the_remaining_tab(
    cx: &mut gpui_kit::TestAppContext,
) {
    let _ui_session = crate::test_support::UiTestSession::new(cx);
    let (workbench, cx, _runtime, _listener) = terminal_fixture(cx);
    cx.update(|window, cx| {
        workbench.update(cx, |this, cx| {
            this.focus_serial("A");
            this.command_error = Some("old A error".into());
            let removed_focus = this.terminals["A"].transcript_focus.clone();
            removed_focus.focus(window, cx);
            assert!(removed_focus.contains_focused(window, cx));
            this.request_terminal_close("A", window, cx);
            assert!(!removed_focus.contains_focused(window, cx));
            assert_eq!(this.tabs.active.as_deref(), Some("B"));
            assert_eq!(
                this.command_focus,
                Some(crate::application::command_panel::CommandFocus::Serial(
                    "B".into()
                )),
            );
            assert_eq!(this.command_error, None);
            this.request_terminal_close("B", window, cx);
            assert_eq!(this.tabs.active, None);
            assert_eq!(this.command_focus, None);
            assert_eq!(this.manager.sessions().count(), 0);
            assert_eq!(this.terminals.len(), 0);
        });
    });
}

#[gpui_kit::test]
fn terminal_chooser_rejects_generation_change_and_shutdown_but_exports_a_frozen_snapshot(
    cx: &mut gpui_kit::TestAppContext,
) {
    let _ui_session = crate::test_support::UiTestSession::new(cx);
    let (workbench, cx, _runtime, _listener) = terminal_fixture(cx);
    let directory = tempfile::tempdir().unwrap();
    let stale = directory.path().join("stale.txt");
    workbench.update(cx, |this, cx| this.export_terminal_log("A", cx));
    workbench.update(cx, |this, _| this.manager.connect("A").unwrap());
    cx.simulate_new_path_selection(|_| Some(stale.clone()));
    cx.run_until_parked();
    assert!(!stale.exists());
    workbench.update(cx, |this, _| {
        assert!(!this.terminals["A"].export.busy());
        this.manager.disconnect("A").unwrap();
    });

    let frozen = directory.path().join("frozen.txt");
    workbench.update(cx, |this, cx| this.export_terminal_log("B", cx));
    workbench.update(cx, |this, _| this.manager.clear("B"));
    cx.simulate_new_path_selection(|_| Some(frozen.clone()));
    cx.run_until_parked();
    assert_eq!(
        std::fs::read_to_string(frozen).unwrap(),
        "Synthetic terminal transcript\nNo hardware commands\n"
    );

    let after_shutdown = directory.path().join("after-shutdown.txt");
    workbench.update(cx, |this, cx| {
        this.export_terminal_log("B", cx);
        this.shutting_down = true;
        assert_eq!(this.shutdown_terminal_exports().len(), 0);
    });
    cx.simulate_new_path_selection(|_| Some(after_shutdown.clone()));
    cx.run_until_parked();
    assert!(!after_shutdown.exists());
}

#[gpui_kit::test]
fn shutdown_rejects_terminal_connect_and_send_without_losing_draft(
    cx: &mut gpui_kit::TestAppContext,
) {
    let _ui_session = crate::test_support::UiTestSession::new(cx);
    let (workbench, cx, _runtime, _listener) = terminal_fixture(cx);
    cx.update(|window, cx| {
        workbench.update(cx, |this, cx| {
            this.terminals["A"].input.update(cx, |input, cx| {
                input.set_value("unsent fixture", window, cx);
            });
            this.shutting_down = true;
            this.toggle_connection("A", window, cx);
            this.send("A", window, cx);
            this.send_terminal_all("A", window, cx);
            assert!(!this.queue_write("A", "must not be submitted"));
            assert_eq!(
                this.manager.session("A").unwrap().state,
                ConnectionState::Disconnected
            );
            assert!(this.pending_writes.is_empty());
            assert!(this.terminals["A"].pending.is_none());
            assert_eq!(
                this.terminals["A"].input.read(cx).value().as_ref(),
                "unsent fixture"
            );
        });
    });
}

struct TerminalWorkspace(gpui_kit::Entity<super::Workbench>);

impl gpui_kit::Render for TerminalWorkspace {
    fn render(
        &mut self,
        window: &mut gpui_kit::Window,
        cx: &mut gpui_kit::Context<Self>,
    ) -> impl gpui_kit::IntoElement {
        use gpui_kit::prelude::*;
        self.0.update(cx, |this, cx| {
            let workspace = this.workspace(window, cx);
            gpui_kit::div()
                .size_full()
                .track_focus(&this.page_focus)
                .tab_group()
                .flex()
                .flex_col()
                .child(workspace)
        })
    }
}

#[gpui_kit::test]
fn terminal_close_is_hidden_at_rest_visible_on_keyboard_focus_and_keeps_admission(
    cx: &mut gpui_kit::TestAppContext,
) {
    let _ui_session = crate::test_support::UiTestSession::new(cx);
    use gpui_kit::{
        VisualContext as _, point, px,
        test::{TestEventExt as _, TestWindowExt as _},
    };

    let (workbench, cx, _runtime, _listener) = terminal_fixture(cx);
    let workspace = cx.new_window_entity(|window, cx| {
        let focus = workbench.read(cx).page_focus.clone();
        focus.focus(window, cx);
        TerminalWorkspace(workbench.clone())
    });
    cx.replace_root_view(|window, cx| gpui_kit::base::Root::new(workspace, window, cx));
    let (receipt, completion) = crate::sessions::test_support::write_receipt();
    workbench.update(cx, |this, _| {
        this.pending_writes.push(super::PendingWrite {
            port: "B".into(),
            generation: this.manager.session("B").unwrap().generation,
            receipt,
        });
    });
    cx.update(|window, cx| {
        window.render_frame(cx);
        assert!(workbench.read(cx).page_focus.is_focused(window));
        window.pointer_move(point(px(600.), px(400.)), None, cx);
        assert!(!window.find("terminal-close:A").visible());
        assert_eq!(window.find("terminal-close:B").disabled(), Some(true));
        for _ in 0..32 {
            window.press("tab", cx);
            if window.find("terminal-close:A").focused() == Some(true) {
                break;
            }
        }
        let close = window.find("terminal-close:A");
        assert_eq!(close.focused(), Some(true));
        assert!(
            close.visible(),
            "keyboard focus must reveal the close action"
        );
        window.press("enter", cx);
    });
    workbench.update(cx, |this, _| {
        assert!(this.manager.session("A").is_none());
        assert_eq!(this.tabs.active.as_deref(), Some("B"));
        assert!(!this.can_request_terminal_close("B"));
    });
    cx.update(|window, cx| {
        window.render_frame(cx);
        let close = window.find("terminal-close:B").bounds().center();
        window.pointer_move(close, None, cx);
        assert!(window.find("terminal-close:B").visible());
        window.click("terminal-close:B", cx);
    });
    workbench.update(cx, |this, _| {
        assert!(this.manager.session("B").is_some());
    });
    completion.send(Ok(())).unwrap();
    workbench.update(cx, |this, _| {
        assert!(this.poll_writes());
    });
    cx.update(|window, cx| {
        window.render_frame(cx);
        window.click("terminal-close:B", cx);
    });
    workbench.update(cx, |this, _| {
        assert!(this.manager.session("B").is_none());
        assert!(this.terminals.is_empty());
    });
}

#[gpui_kit::test]
fn transcript_keyboard_selection_reaches_the_existing_copy_owner_without_sending(
    cx: &mut gpui_kit::TestAppContext,
) {
    let _ui_session = crate::test_support::UiTestSession::new(cx);
    use gpui_kit::{VisualContext as _, test::TestWindowExt as _};

    let (workbench, cx, _runtime, _listener) = terminal_fixture(cx);
    let workspace = cx.new_window_entity(|window, cx| {
        let focus = workbench.read(cx).page_focus.clone();
        focus.focus(window, cx);
        TerminalWorkspace(workbench.clone())
    });
    cx.replace_root_view(|window, cx| gpui_kit::base::Root::new(workspace, window, cx));
    cx.update(|window, cx| {
        window.render_frame(cx);
        assert!(workbench.read(cx).page_focus.is_focused(window));
        for _ in 0..32 {
            window.press("tab", cx);
            if window.find("transcript-scroll:A").focused() == Some(true) {
                break;
            }
        }
        assert_eq!(window.find("transcript-scroll:A").focused(), Some(true));
        #[cfg(target_os = "macos")]
        window.press("cmd-home", cx);
        #[cfg(not(target_os = "macos"))]
        window.press("ctrl-home", cx);
        window.press("shift-end", cx);
        window.press("secondary-c", cx);
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("Synthetic terminal transcript".into())
        );
        #[cfg(target_os = "macos")]
        window.press("cmd-a", cx);
        #[cfg(not(target_os = "macos"))]
        window.press("ctrl-a", cx);
        window.press("secondary-c", cx);
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("Synthetic terminal transcript\nNo hardware commands\n".into())
        );
    });
    workbench.update(cx, |this, cx| {
        assert!(!this.terminals["A"].follow);
        assert!(this.terminals["A"].input.read(cx).value().is_empty());
        assert!(this.pending_writes.is_empty());
        assert_eq!(
            this.manager.session("A").unwrap().state,
            ConnectionState::Disconnected
        );
    });
}

fn click_terminal_filter_option(cx: &mut gpui_kit::VisualTestContext, label: &str) {
    use gpui_kit::test::TestWindowExt as _;

    cx.update(|window, cx| {
        window.click("terminal-log-filter", cx);
        let item = window
            .within("popup-menu")
            .find_by_label(label)
            .path()
            .last()
            .unwrap()
            .clone();
        window.within("popup-menu").click(item, cx);
    });
    // PopupMenu emits DismissEvent; DropdownMenu closes its popover in the
    // subscriber after the enclosing App update completes.
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("popup-menu").is_none());
        let expected = format!("Log: {label}");
        assert_eq!(
            window.find("terminal-log-filter").label(),
            Some(expected.as_str())
        );
    });
}

fn keyboard_terminal_filter_option(cx: &mut gpui_kit::VisualTestContext, label: &str) {
    use gpui_kit::test::TestWindowExt as _;

    cx.update(|window, cx| {
        for _ in 0..32 {
            if window.find("terminal-log-filter").focused() == Some(true) {
                break;
            }
            window.press("tab", cx);
        }
        assert_eq!(window.find("terminal-log-filter").focused(), Some(true));
        window.press("enter", cx);
        for _ in 0..6 {
            if window.within("popup-menu").find_by_label(label).selected() == Some(true) {
                break;
            }
            window.press("down", cx);
        }
        assert_eq!(
            window.within("popup-menu").find_by_label(label).selected(),
            Some(true),
        );
        window.press("enter", cx);
    });
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("popup-menu").is_none());
        let expected = format!("Log: {label}");
        assert_eq!(
            window.find("terminal-log-filter").label(),
            Some(expected.as_str())
        );
        assert_eq!(window.find("terminal-log-filter").focused(), Some(true));
    });
}

fn copy_visible_terminal(window: &mut gpui_kit::Window, cx: &mut gpui_kit::App, port: &str) {
    use gpui_kit::test::TestWindowExt as _;

    window.click(format!("transcript-scroll:{port}"), cx);
    window.press("secondary-a", cx);
    window.press("secondary-c", cx);
}

fn click_terminal_tab(window: &mut gpui_kit::Window, cx: &mut gpui_kit::App, port: &str) {
    use gpui_kit::{
        MouseButton, point, px,
        test::{TestEventExt as _, TestQueryExt as _},
    };

    let bounds = window
        .find_by_label(&format!("{port} · Connected"))
        .bounds();
    let position = point(bounds.left() + px(10.), bounds.center().y);
    window.pointer_down(position, MouseButton::Left, cx);
    window.pointer_up(position, MouseButton::Left, cx);
}

#[gpui_kit::test]
fn terminal_log_filter_is_per_tab_and_changes_only_the_visible_transcript(
    cx: &mut gpui_kit::TestAppContext,
) {
    use crate::components::terminal_transcript::LogFilter;
    use crate::sessions::test_support::{display_manager, display_test_session};
    use gpui_kit::{
        ClipboardItem, MouseButton, VisualContext as _, point, px,
        test::{TestEventExt as _, TestWindowExt as _},
    };

    const SOURCE: &str = concat!(
        "<err> core: fault\n",
        "<wrn> core: caution\n",
        "<inf> core: ready\n",
        "<dbg> core: detail\n",
        "command response\n",
        "continuation\n",
    );
    const ERROR: &str = "<err> core: fault\ncommand response\ncontinuation\n";

    let runtime = crate::app_runtime::AppRuntime::new().unwrap();
    let _ui_session = crate::test_support::UiTestSession::new(cx);
    let mut manager = display_manager();
    let display_a = display_test_session(&mut manager, "A");
    let display_b = display_test_session(&mut manager, "B");
    display_a.push(SOURCE.as_bytes());
    display_b.push(b"<dbg> core: only debug");
    let _ = manager.poll();
    let executor = runtime.executor();
    let cx = cx.add_empty_window();
    let workbench = cx.new_window_entity(move |window, cx| {
        let mut this = offline_workbench(window, cx, manager, executor);
        this.sync_tabs(window, cx);
        this
    });
    let workspace = cx.new_window_entity(|window, cx| {
        let focus = workbench.read(cx).page_focus.clone();
        focus.focus(window, cx);
        TerminalWorkspace(workbench.clone())
    });
    cx.replace_root_view(|window, cx| gpui_kit::base::Root::new(workspace, window, cx));

    cx.update(|window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("terminal-log-filter").label(), Some("Log: All"));
    });
    click_terminal_filter_option(cx, "Error");
    cx.update(|window, cx| {
        copy_visible_terminal(window, cx, "A");
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some(ERROR.into())
        );
    });
    click_terminal_filter_option(cx, "Error");
    let end = cx.update(|window, cx| {
        assert_eq!(
            workbench.read(cx).terminals["A"]
                .transcript
                .borrow()
                .selected_text()
                .as_deref(),
            Some(ERROR),
            "reselecting the current level must keep the existing selection"
        );
        let bounds = window.find("transcript-scroll:A").bounds();
        let start = point(
            bounds.left() + px(10.),
            bounds.top() + window.rem_size() * 0.625,
        );
        let end = point(start.x + px(60.), start.y);
        window.pointer_down(start, MouseButton::Left, cx);
        window.pointer_move(end, Some(MouseButton::Left), cx);
        assert!(
            workbench.read(cx).terminals["A"]
                .selection
                .snapshot(cx)
                .is_some()
        );
        end
    });
    cx.run_until_parked();
    keyboard_terminal_filter_option(cx, "Warning");
    cx.update(|window, cx| {
        assert!(
            workbench.read(cx).terminals["A"]
                .selection
                .snapshot(cx)
                .is_none(),
            "changing the filter must clear Root's geometric selection"
        );
        window.pointer_up(end, MouseButton::Left, cx);
        assert!(
            workbench.read(cx).terminals["A"]
                .transcript
                .borrow()
                .selected_text()
                .is_none(),
            "changing the filter must discard the old byte range"
        );
        cx.write_to_clipboard(ClipboardItem::new_string("unchanged".into()));
        let focus = workbench.read(cx).terminals["A"].transcript_focus.clone();
        focus.focus(window, cx);
        window.render_frame(cx);
        window.press("secondary-c", cx);
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("unchanged".into())
        );
    });
    click_terminal_filter_option(cx, "Error");
    cx.update(|window, cx| {
        copy_visible_terminal(window, cx, "A");
        window.click("terminal-copy", cx);
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some(SOURCE.into()),
            "Copy all must not use the filtered projection"
        );
        window.click("terminal-export", cx);
    });

    let directory = tempfile::tempdir().unwrap();
    let exported = directory.path().join("filtered-terminal.txt");
    display_a.push(b"<err> core: later\n<inf> core: hidden\n");
    workbench.update(cx, |this, cx| {
        let _ = this.manager.poll();
        cx.notify();
    });
    cx.update(|window, cx| {
        window.render_frame(cx);
        assert_eq!(
            workbench.read(cx).terminals["A"]
                .transcript
                .borrow()
                .selected_text()
                .as_deref(),
            Some(ERROR),
            "a completed selection must remain frozen across append"
        );
    });
    cx.simulate_new_path_selection(|_| Some(exported.clone()));
    cx.run_until_parked();
    assert_eq!(
        std::fs::read_to_string(exported).unwrap(),
        SOURCE,
        "export must freeze the complete source before the chooser returns"
    );

    display_a.push(b"<i");
    workbench.update(cx, |this, cx| {
        let _ = this.manager.poll();
        cx.notify();
    });
    cx.update(|window, cx| {
        window.render_frame(cx);
        assert_eq!(
            workbench.read(cx).terminals["A"]
                .transcript
                .borrow()
                .selected_text()
                .as_deref(),
            Some(ERROR),
            "an unclassified append must not extend a completed selection"
        );
    });
    display_a.push(b"nf> core: late");
    workbench.update(cx, |this, cx| {
        let _ = this.manager.poll();
        cx.notify();
    });
    cx.update(|window, cx| {
        window.render_frame(cx);
        assert_eq!(
            workbench.read(cx).terminals["A"]
                .transcript
                .borrow()
                .selected_text()
                .as_deref(),
            Some(ERROR),
            "hiding a later row must preserve a completed selection before it"
        );
    });
    display_a.push(b"\n<i");
    workbench.update(cx, |this, cx| {
        let _ = this.manager.poll();
        cx.notify();
    });
    let drag_end = cx.update(|window, cx| {
        window.render_frame(cx);
        copy_visible_terminal(window, cx, "A");
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some(format!("{ERROR}<err> core: later\n<i")),
            "an incomplete tag is unclassified and remains visible"
        );
        let bounds = window.find("transcript-scroll:A").bounds();
        let start = point(
            bounds.left() + px(8.),
            bounds.top() + window.rem_size() * 5.625,
        );
        let end = point(start.x + px(14.), start.y);
        window.pointer_down(start, MouseButton::Left, cx);
        window.pointer_move(end, Some(MouseButton::Left), cx);
        end
    });
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.render_frame(cx);
        assert!(
            workbench.read(cx).terminals["A"]
                .selection
                .snapshot(cx)
                .is_some(),
            "the drag must create a Root geometric selection"
        );
        let selected = workbench.read(cx).terminals["A"]
            .transcript
            .borrow()
            .selected_text();
        assert!(
            selected
                .as_deref()
                .is_some_and(|text| !text.is_empty() && "<i".contains(text)),
            "the geometric selection must cover bytes in the incomplete tail: {selected:?}"
        );
    });
    display_a.push(b"nf> core: later");
    workbench.update(cx, |this, cx| {
        let _ = this.manager.poll();
        cx.notify();
    });
    cx.update(|window, cx| {
        window.render_frame(cx);
        assert!(
            workbench.read(cx).terminals["A"]
                .selection
                .snapshot(cx)
                .is_none(),
            "projection membership changes must clear Root's geometric selection"
        );
        window.pointer_up(drag_end, MouseButton::Left, cx);
        assert!(
            workbench.read(cx).terminals["A"]
                .transcript
                .borrow()
                .selected_text()
                .is_none(),
            "a partial row becoming hidden must invalidate the previous selection"
        );
        cx.write_to_clipboard(ClipboardItem::new_string("unchanged".into()));
        window.press("secondary-c", cx);
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("unchanged".into())
        );
        click_terminal_tab(window, cx, "B");
        assert_eq!(window.find("terminal-log-filter").label(), Some("Log: All"));
    });
    click_terminal_filter_option(cx, "Info");
    cx.update(|window, cx| {
        assert_eq!(
            window.find("terminal-log-filter").label(),
            Some("Log: Info")
        );
        assert!(window.try_find("transcript-scroll:B").is_some());
        assert!(
            workbench.read(cx).terminals["B"]
                .transcript
                .borrow()
                .is_empty()
        );
        assert_eq!(
            workbench.read(cx).terminals["A"]
                .transcript
                .borrow()
                .log_filter(),
            LogFilter::Error
        );
        click_terminal_tab(window, cx, "A");
        assert_eq!(
            window.find("terminal-log-filter").label(),
            Some("Log: Error")
        );
    });
    cx.update(|window, cx| {
        workbench.update(cx, |this, cx| {
            assert!(this.set_feature_page(super::FeaturePage::Settings, window, cx));
            assert!(this.set_feature_page(super::FeaturePage::DeviceSetup, window, cx));
            cx.notify();
        });
    });

    cx.update(|window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find("terminal-log-filter").label(),
            Some("Log: Error")
        );
    });
    click_terminal_filter_option(cx, "Warning");
    cx.update(|window, cx| {
        copy_visible_terminal(window, cx, "A");
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("<wrn> core: caution\ncommand response\ncontinuation\n".into())
        );
    });
    keyboard_terminal_filter_option(cx, "Info");
    cx.update(|window, cx| {
        copy_visible_terminal(window, cx, "A");
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some(
                concat!(
                    "<inf> core: ready\ncommand response\ncontinuation\n",
                    "<inf> core: hidden\n<inf> core: late\n<inf> core: later"
                )
                .into()
            )
        );
    });
    click_terminal_filter_option(cx, "Debug");
    cx.update(|window, cx| {
        copy_visible_terminal(window, cx, "A");
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("<dbg> core: detail\ncommand response\ncontinuation\n".into())
        );
    });
    keyboard_terminal_filter_option(cx, "All");
    cx.update(|window, cx| {
        copy_visible_terminal(window, cx, "A");
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some(format!(
                "{SOURCE}<err> core: later\n<inf> core: hidden\n<inf> core: late\n<inf> core: later"
            ))
        );
        for _ in 0..32 {
            if window.find("terminal-log-filter").focused() == Some(true) {
                break;
            }
            window.press("tab", cx);
        }
        assert_eq!(window.find("terminal-log-filter").focused(), Some(true));
        window.press("enter", cx);
        assert!(window.try_find("popup-menu").is_some());
        window.press("escape", cx);
    });
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("popup-menu").is_none());
        assert_eq!(window.find("terminal-log-filter").focused(), Some(true));
    });
    workbench.update(cx, |this, cx| {
        assert!(this.pending_writes.is_empty());
        assert!(this.terminals["A"].input.read(cx).value().is_empty());
        assert!(this.terminals["B"].input.read(cx).value().is_empty());
        assert_eq!(
            this.terminals["B"].transcript.borrow().log_filter(),
            LogFilter::Info
        );
    });
}

#[gpui_kit::test]
fn wheel_on_log_filter_steps_per_tab_and_preserves_transcript_scrolling(
    cx: &mut gpui_kit::TestAppContext,
) {
    use crate::components::terminal_transcript::LogFilter;
    use crate::sessions::test_support::{display_manager, display_test_session};
    use gpui_kit::{ScrollDelta, VisualContext as _, point, px, test::TestWindowExt as _};

    let runtime = crate::app_runtime::AppRuntime::new().unwrap();
    let _ui_session = crate::test_support::UiTestSession::new(cx);
    let mut manager = display_manager();
    let display_a = display_test_session(&mut manager, "A");
    let display_b = display_test_session(&mut manager, "B");
    let ordinary = "unclassified output\n".repeat(100);
    let source = format!(
        "<err> core: fault\n<wrn> core: caution\n<inf> core: ready\n\
         <dbg> core: detail\ncommand response\n{ordinary}"
    );
    display_a.push(source.as_bytes());
    display_b.push(b"B response\n");
    let _ = manager.poll();
    let executor = runtime.executor();
    let cx = cx.add_empty_window();
    let workbench = cx.new_window_entity(move |window, cx| {
        let mut this = offline_workbench(window, cx, manager, executor);
        this.sync_tabs(window, cx);
        this
    });
    let workspace = cx.new_window_entity(|window, cx| {
        let focus = workbench.read(cx).page_focus.clone();
        focus.focus(window, cx);
        TerminalWorkspace(workbench.clone())
    });
    cx.replace_root_view(|window, cx| gpui_kit::base::Root::new(workspace, window, cx));

    cx.update(|window, cx| {
        window.render_frame(cx);
        copy_visible_terminal(window, cx, "A");
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some(source)
        );
        window.scroll("terminal-log-filter", ScrollDelta::Lines(point(0., 3.)), cx);
        assert_eq!(window.find("terminal-log-filter").label(), Some("Log: All"));
        assert!(
            workbench.read(cx).terminals["A"]
                .transcript
                .borrow()
                .selected_text()
                .is_some(),
            "clamping at All must not clear the selection"
        );

        for (level, line) in [
            ("Error", "<err> core: fault\n"),
            ("Warning", "<wrn> core: caution\n"),
            ("Info", "<inf> core: ready\n"),
            ("Debug", "<dbg> core: detail\n"),
        ] {
            window.scroll(
                "terminal-log-filter",
                ScrollDelta::Lines(point(0., -3.)),
                cx,
            );
            assert_eq!(
                window.find("terminal-log-filter").label(),
                Some(format!("Log: {level}").as_str())
            );
            assert!(
                workbench.read(cx).terminals["A"]
                    .transcript
                    .borrow()
                    .selected_text()
                    .is_none(),
                "a wheel filter change must clear the old selection"
            );
            assert!(window.try_find("popup-menu").is_none());
            copy_visible_terminal(window, cx, "A");
            assert_eq!(
                cx.read_from_clipboard().and_then(|item| item.text()),
                Some(format!("{line}command response\n{ordinary}"))
            );
        }
        window.scroll(
            "terminal-log-filter",
            ScrollDelta::Lines(point(0., -3.)),
            cx,
        );
        assert_eq!(
            window.find("terminal-log-filter").label(),
            Some("Log: Debug")
        );
        assert!(
            workbench.read(cx).terminals["A"]
                .transcript
                .borrow()
                .selected_text()
                .is_some(),
            "clamping at Debug must not clear the selection"
        );
        for level in ["Info", "Warning", "Error", "All"] {
            window.scroll("terminal-log-filter", ScrollDelta::Lines(point(0., 3.)), cx);
            assert_eq!(
                window.find("terminal-log-filter").label(),
                Some(format!("Log: {level}").as_str())
            );
        }
        window.scroll(
            "terminal-log-filter",
            ScrollDelta::Lines(point(-4., 0.)),
            cx,
        );
        assert_eq!(window.find("terminal-log-filter").label(), Some("Log: All"));

        let half_step = window.rem_size() * 0.75;
        window.scroll(
            "terminal-log-filter",
            ScrollDelta::Pixels(point(px(0.), -half_step)),
            cx,
        );
        assert_eq!(window.find("terminal-log-filter").label(), Some("Log: All"));
        window.scroll(
            "terminal-log-filter",
            ScrollDelta::Pixels(point(px(0.), half_step)),
            cx,
        );
        window.scroll(
            "terminal-log-filter",
            ScrollDelta::Pixels(point(px(0.), -half_step)),
            cx,
        );
        assert_eq!(
            window.find("terminal-log-filter").label(),
            Some("Log: All"),
            "reversing direction must discard the old partial gesture"
        );
        window.scroll(
            "terminal-log-filter",
            ScrollDelta::Pixels(point(px(0.), -half_step)),
            cx,
        );
        assert_eq!(
            window.find("terminal-log-filter").label(),
            Some("Log: Error")
        );
        window.scroll(
            "terminal-log-filter",
            ScrollDelta::Pixels(point(px(0.), px(-1000.))),
            cx,
        );
        assert_eq!(
            window.find("terminal-log-filter").label(),
            Some("Log: Warning"),
            "a coalesced event may advance at most one choice"
        );

        click_terminal_tab(window, cx, "B");
        assert_eq!(window.find("terminal-log-filter").label(), Some("Log: All"));
        window.scroll(
            "terminal-log-filter",
            ScrollDelta::Lines(point(0., -3.)),
            cx,
        );
        assert_eq!(
            window.find("terminal-log-filter").label(),
            Some("Log: Error")
        );
        assert_eq!(
            workbench.read(cx).terminals["A"]
                .transcript
                .borrow()
                .log_filter(),
            LogFilter::Warning
        );
        click_terminal_tab(window, cx, "A");
        assert_eq!(
            window.find("terminal-log-filter").label(),
            Some("Log: Warning")
        );
        assert_eq!(
            workbench.read(cx).terminals["B"]
                .transcript
                .borrow()
                .log_filter(),
            LogFilter::Error
        );
        workbench.update(cx, |this, cx| {
            let terminal = this.terminals.get_mut("A").unwrap();
            terminal.follow = false;
            terminal.scroll.set_offset(point(px(0.), px(0.)));
            cx.notify();
        });
        window.render_frame(cx);
        let before = workbench.read(cx).terminals["A"].scroll.offset().y;
        window.scroll(
            "transcript-scroll:A",
            ScrollDelta::Pixels(point(px(0.), px(-100.))),
            cx,
        );
        assert!(workbench.read(cx).terminals["A"].scroll.offset().y < before);
        assert_eq!(
            window.find("terminal-log-filter").label(),
            Some("Log: Warning"),
            "scrolling the transcript must not change the filter"
        );
    });
    cx.update(|window, cx| {
        window.click("terminal-log-filter", cx);
        let item = window
            .within("popup-menu")
            .find_by_label("Info")
            .path()
            .last()
            .unwrap()
            .clone();
        window
            .within("popup-menu")
            .scroll(item, ScrollDelta::Lines(point(0., -3.)), cx);
        assert_eq!(
            workbench.read(cx).terminals["A"]
                .transcript
                .borrow()
                .log_filter(),
            LogFilter::Warning,
            "a wheel over the menu is not a wheel over the trigger"
        );
        window.press("escape", cx);
    });
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("popup-menu").is_none());
    });
    workbench.update(cx, |this, cx| {
        assert!(this.pending_writes.is_empty());
        assert_eq!(this.terminal_command_runs().count(), 0);
        assert!(this.terminals["A"].input.read(cx).value().is_empty());
        assert!(this.terminals["B"].input.read(cx).value().is_empty());
    });
}

#[gpui_kit::test]
fn wheel_on_command_input_navigates_each_ports_history_without_sending(
    cx: &mut gpui_kit::TestAppContext,
) {
    use crate::sessions::test_support::{connected, peer, stopped, until};
    use gpui_kit::{
        Modifiers, ScrollDelta, VisualContext as _, point, px,
        test::{TestEventExt as _, TestWindowExt as _},
    };
    use std::io::{ErrorKind, Read as _, Write as _};

    let _ui_session = crate::test_support::UiTestSession::new(cx);
    let (workbench, cx, _runtime, listener) = terminal_fixture(cx);
    workbench.update(cx, |this, _| {
        this.inventory.ports = ["A", "B"].into_iter().map(terminal_test_endpoint).collect();
        this.manager.connect("A").unwrap();
    });
    let mut peer_a = peer(&listener);
    workbench.update(cx, |this, _| {
        connected(&mut this.manager, "A");
        this.manager.connect("B").unwrap();
    });
    let mut peer_b = peer(&listener);
    workbench.update(cx, |this, _| {
        connected(&mut this.manager, "B");
        for (port, commands) in [("A", ["older A", "newer A"]), ("B", ["older B", "newer B"])] {
            for command in commands {
                this.terminals
                    .get_mut(port)
                    .unwrap()
                    .history
                    .record_submission(command, &Ok::<(), ()>(()));
            }
        }
    });
    // Manual reconnect creates a fresh transcript. Feed this generation enough
    // synthetic device output to exercise its real scroll viewport.
    let received = "Synthetic received output\n".repeat(100);
    peer_a.write_all(received.as_bytes()).unwrap();
    workbench.update(cx, |this, _| {
        until(|| {
            let _ = this.manager.poll();
            this.manager.session("A").unwrap().terminal.text() == received
        });
    });
    peer_a.set_nonblocking(true).unwrap();
    peer_b.set_nonblocking(true).unwrap();
    let workspace = cx.new_window_entity(|window, cx| {
        let focus = workbench.read(cx).page_focus.clone();
        focus.focus(window, cx);
        TerminalWorkspace(workbench.clone())
    });
    cx.replace_root_view(|window, cx| gpui_kit::base::Root::new(workspace, window, cx));
    cx.simulate_resize(gpui_kit::size(px(800.), px(600.)));
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    let (input_a, input_b) = workbench.read_with(cx, |this, _| {
        (
            this.terminals["A"].input.entity_id(),
            this.terminals["B"].input.entity_id(),
        )
    });

    cx.update(|window, cx| {
        window.render_frame(cx);
        window.click(("input", input_a), cx);
        window.input("draft A", cx);
        for (delta, expected) in [
            (3., "newer A"),
            (3., "older A"),
            (3., "older A"),
            (-3., "newer A"),
            (-3., "draft A"),
            (-3., "draft A"),
        ] {
            window.scroll(("input", input_a), ScrollDelta::Lines(point(0., delta)), cx);
            assert_eq!(window.find(("input", input_a)).value(), Some(expected));
        }
        window.scroll(("input", input_a), ScrollDelta::Lines(point(5., 0.)), cx);
        assert_eq!(window.find(("input", input_a)).value(), Some("draft A"));

        let half_step = window.rem_size() * 0.75;
        window.scroll(
            ("input", input_a),
            ScrollDelta::Pixels(point(px(0.), half_step)),
            cx,
        );
        assert_eq!(window.find(("input", input_a)).value(), Some("draft A"));
        window.scroll(
            ("input", input_a),
            ScrollDelta::Pixels(point(px(0.), -half_step)),
            cx,
        );
        window.scroll(
            ("input", input_a),
            ScrollDelta::Pixels(point(px(0.), half_step)),
            cx,
        );
        assert_eq!(
            window.find(("input", input_a)).value(),
            Some("draft A"),
            "reversing direction must discard the old partial gesture"
        );
        window.scroll(
            ("input", input_a),
            ScrollDelta::Pixels(point(px(0.), half_step)),
            cx,
        );
        assert_eq!(window.find(("input", input_a)).value(), Some("newer A"));
        window.scroll(
            ("input", input_a),
            ScrollDelta::Pixels(point(px(0.), px(1000.))),
            cx,
        );
        assert_eq!(window.find(("input", input_a)).value(), Some("older A"));
        window.scroll(("input", input_a), ScrollDelta::Lines(point(0., -3.)), cx);
        window.scroll(("input", input_a), ScrollDelta::Lines(point(0., -3.)), cx);
        assert_eq!(window.find(("input", input_a)).value(), Some("draft A"));

        click_terminal_tab(window, cx, "B");
        window.click(("input", input_b), cx);
        window.input("draft B", cx);
        window.scroll(("input", input_b), ScrollDelta::Lines(point(0., 3.)), cx);
        assert_eq!(window.find(("input", input_b)).value(), Some("newer B"));
        assert_eq!(
            workbench.read(cx).terminals["A"]
                .input
                .read(cx)
                .value()
                .as_ref(),
            "draft A"
        );
        click_terminal_tab(window, cx, "A");
        assert_eq!(window.find(("input", input_a)).value(), Some("draft A"));
        click_terminal_tab(window, cx, "B");
        window.scroll(("input", input_b), ScrollDelta::Lines(point(0., -3.)), cx);
        assert_eq!(window.find(("input", input_b)).value(), Some("draft B"));

        click_terminal_tab(window, cx, "A");
        window.click(("input", input_a), cx);
        let long_text = "wide text ".repeat(20);
        window.input(&long_text, cx);
        let input_entity = workbench.read(cx).terminals["A"].input.clone();
        let value = input_entity.read(cx).value();
        assert!(
            value.contains(long_text.as_str()),
            "the test must enter enough text to overflow the input"
        );
        // Start at the left edge rather than relying on caret-follow after
        // typing. The subsequent wheel must move the actual Input viewport.
        input_entity.update(cx, |input, cx| {
            input.set_scroll_offset(point(px(0.), px(0.)), cx);
        });
        window.render_frame(cx);
        let before = input_entity.read(cx).scroll_offset().x;
        assert_eq!(before, px(0.));
        let half_step = window.rem_size() * 0.75;
        window.scroll(
            ("input", input_a),
            ScrollDelta::Pixels(point(px(-1.), half_step)),
            cx,
        );
        assert_eq!(input_entity.read(cx).value(), value);
        assert_eq!(
            input_entity.read(cx).scroll_offset().x,
            before,
            "a partial vertical gesture must not pan the overflowing input"
        );
        window.scroll(
            ("input", input_a),
            ScrollDelta::Pixels(point(px(-1.), half_step)),
            cx,
        );
        assert_eq!(
            input_entity.read(cx).value().as_ref(),
            "newer A",
            "vertical trackpad motion with axis drift must recall history"
        );
        window.scroll(
            ("input", input_a),
            ScrollDelta::Pixels(point(px(-1.), -half_step * 2.)),
            cx,
        );
        assert_eq!(
            input_entity.read(cx).value(),
            value,
            "scrolling down must restore the overflowing draft"
        );
        input_entity.update(cx, |input, cx| {
            input.set_scroll_offset(point(px(0.), px(0.)), cx);
        });
        window.render_frame(cx);
        window.change_modifiers(Modifiers::control(), cx);
        window.scroll(
            ("input", input_a),
            ScrollDelta::Pixels(point(px(-20.), half_step * 2.)),
            cx,
        );
        assert_eq!(
            input_entity.read(cx).value(),
            value,
            "modified gestures must not recall a command"
        );
        assert!(
            input_entity.read(cx).scroll_offset().x < px(0.),
            "modified gestures must remain available to the native input"
        );
        window.change_modifiers(Modifiers::default(), cx);
        input_entity.update(cx, |input, cx| {
            input.set_scroll_offset(point(px(0.), px(0.)), cx);
        });
        window.render_frame(cx);
        window.scroll(
            ("input", input_a),
            ScrollDelta::Pixels(point(px(-250.), px(0.))),
            cx,
        );
        let input = workbench.read(cx).terminals["A"].input.read(cx);
        assert_eq!(input.value(), value);
        assert!(
            input.scroll_offset().x < before,
            "horizontal wheel input must still scroll the native text editor"
        );

        let before = input.value();
        workbench.update(cx, |this, cx| {
            let terminal = this.terminals.get_mut("A").unwrap();
            terminal.follow = false;
            terminal.scroll.set_offset(point(px(0.), px(0.)));
            cx.notify();
        });
        window.render_frame(cx);
        let top = workbench.read(cx).terminals["A"].scroll.offset().y;
        assert_eq!(top, px(0.));
        window.scroll(
            "transcript-scroll:A",
            ScrollDelta::Pixels(point(px(0.), px(-100.))),
            cx,
        );
        assert!(
            workbench.read(cx).terminals["A"].scroll.offset().y < top,
            "a wheel over the transcript must still scroll the transcript"
        );
        assert_eq!(
            workbench.read(cx).terminals["A"].input.read(cx).value(),
            before
        );
    });
    workbench.update(cx, |this, _| {
        assert!(this.pending_writes.is_empty());
        assert_eq!(this.terminal_command_runs().count(), 0);
        assert!(this.terminals["A"].pending.is_none());
        assert!(this.terminals["B"].pending.is_none());
        until(|| this.manager.writes_idle("A") && this.manager.writes_idle("B"));
    });
    for peer in [&mut peer_a, &mut peer_b] {
        let mut byte = [0];
        assert_eq!(
            peer.read(&mut byte).unwrap_err().kind(),
            ErrorKind::WouldBlock,
            "history scrolling must not write a command to either port"
        );
    }
    workbench.update(cx, |this, _| {
        this.manager.disconnect("A").unwrap();
        this.manager.disconnect("B").unwrap();
        stopped(&mut this.manager, "A");
        stopped(&mut this.manager, "B");
    });
}

#[gpui_kit::test]
fn closing_and_reopening_a_terminal_resets_its_log_filter(cx: &mut gpui_kit::TestAppContext) {
    use crate::components::terminal_transcript::LogFilter;
    use gpui_kit::{VisualContext as _, test::TestWindowExt as _};

    let _ui_session = crate::test_support::UiTestSession::new(cx);
    let (workbench, cx, _runtime, _listener) = terminal_fixture(cx);
    let workspace = cx.new_window_entity(|window, cx| {
        let focus = workbench.read(cx).page_focus.clone();
        focus.focus(window, cx);
        TerminalWorkspace(workbench.clone())
    });
    cx.replace_root_view(|window, cx| gpui_kit::base::Root::new(workspace, window, cx));
    cx.update(|window, cx| {
        window.render_frame(cx);
    });
    click_terminal_filter_option(cx, "Error");
    cx.update(|window, cx| {
        assert_eq!(
            window.find("terminal-log-filter").label(),
            Some("Log: Error")
        );
        workbench.update(cx, |this, cx| {
            this.request_terminal_close("A", window, cx);
            assert!(!this.terminals.contains_key("A"));
            this.manager.connect("A").unwrap();
            this.sync_tabs(window, cx);
            assert_eq!(
                this.terminals["A"].transcript.borrow().log_filter(),
                LogFilter::All
            );
            this.manager.disconnect("A").unwrap();
            crate::sessions::test_support::stopped(&mut this.manager, "A");
        });
    });
}

fn terminal_test_endpoint(port: &str) -> SerialEndpoint {
    SerialEndpoint {
        port_name: port.into(),
        label: port.into(),
        manufacturer: None,
        product: None,
        kind: crate::domain::SerialPortKind::Usb,
        serial_number: Some(format!("TEST-{port}")),
        vid: Some(0xcafe),
        pid: Some(1),
        usb_interface: None,
    }
}

fn gated_terminal_manager() -> (
    crate::sessions::SessionManager,
    std::net::TcpListener,
    mpsc::Sender<()>,
    Arc<std::sync::atomic::AtomicUsize>,
) {
    use crate::sessions::test_support::{socket_transport, with_socket_factory};
    use std::sync::atomic::{AtomicUsize, Ordering};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (opening_tx, opening_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = parking_lot::Mutex::new(release_rx);
    let opens = Arc::new(AtomicUsize::new(0));
    let worker_opens = opens.clone();
    let mut manager = with_socket_factory(move |_| {
        worker_opens.fetch_add(1, Ordering::SeqCst);
        let _ = opening_tx.send(());
        let _ = release_rx.lock().recv();
        socket_transport(address)
    });
    manager.connect("A").unwrap();
    opening_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    (manager, listener, release_tx, opens)
}

#[gpui_kit::test]
fn repeated_workbench_renders_do_not_connect_or_submit_a_ready_terminal_command(
    cx: &mut gpui_kit::TestAppContext,
) {
    use crate::sessions::test_support::{connected, peer, stopped, until};
    use gpui_kit::{VisualContext as _, px, size, test::TestWindowExt as _};
    use std::{io::Read as _, sync::atomic::Ordering};

    let _ui_session = crate::test_support::UiTestSession::new(cx);
    let runtime = crate::app_runtime::AppRuntime::new().unwrap();
    let executor = runtime.executor();
    let (manager, listener, release, opens) = gated_terminal_manager();
    let cx = cx.add_empty_window();
    let workbench = cx.new_window_entity(move |window, cx| {
        let mut this = offline_workbench(window, cx, manager, executor);
        this.inventory.ports = vec![terminal_test_endpoint("A")];
        this.advance_window_state(window, cx);
        this.terminals["A"].input.update(cx, |input, cx| {
            input.set_value("help", window, cx);
        });
        this
    });
    cx.replace_root_view(|window, cx| gpui_kit::base::Root::new(workbench.clone(), window, cx));
    cx.simulate_resize(size(px(1200.), px(800.)));
    cx.run_until_parked();
    cx.update(|window, cx| {
        workbench.update(cx, |this, cx| {
            this.send("A", window, cx);
            assert!(this.terminals["A"].pending.is_some());
        });
        window.render_frame(cx);
        window.click("sidebar-preset:info", cx);
        assert!(matches!(
            workbench.read(cx).sidebar.pending,
            Some(crate::application::command_panel::PendingCommand::Connecting(_))
        ));
    });
    release.send(()).unwrap();
    let mut stream = peer(&listener);
    workbench.update(cx, |this, _| connected(&mut this.manager, "A"));
    cx.update(|window, cx| {
        for _ in 0..4 {
            window.render_frame(cx);
        }
        let this = workbench.read(cx);
        assert!(this.terminals["A"].pending.is_some());
        assert!(matches!(
            this.sidebar.pending,
            Some(crate::application::command_panel::PendingCommand::Connecting(_))
        ));
        assert!(this.pending_writes.is_empty());
        assert_eq!(this.terminals["A"].input.read(cx).value().as_ref(), "help");
    });
    assert_eq!(opens.load(Ordering::SeqCst), 1);
    stream.set_nonblocking(true).unwrap();
    let mut byte = [0];
    assert_eq!(
        stream.read(&mut byte).unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock,
    );
    cx.update(|window, cx| {
        workbench.update(cx, |this, cx| {
            assert!(this.advance_serial_sessions(window, cx));
            assert!(this.sidebar.pending.is_none());
            assert!(this.terminals["A"].pending.is_none());
            assert!(this.terminals["A"].input.read(cx).value().is_empty());
            assert_eq!(this.batch_history.len(), 1);
            assert_eq!(this.pending_writes.len(), 1);
        });
    });
    stream.set_nonblocking(false).unwrap();
    let mut commands = [0; 10];
    stream.read_exact(&mut commands).unwrap();
    assert_eq!(&commands, b"info\nhelp\n");
    cx.update(|window, cx| {
        for _ in 0..4 {
            window.render_frame(cx);
        }
        workbench.update(cx, |this, _| {
            assert_eq!(this.batch_history.len(), 1);
            until(|| this.manager.writes_idle("A"));
            this.manager.disconnect("A").unwrap();
            stopped(&mut this.manager, "A");
        });
    });
    let mut remainder = Vec::new();
    stream.read_to_end(&mut remainder).unwrap();
    assert!(remainder.is_empty());
    assert_eq!(opens.load(Ordering::SeqCst), 1);
}

#[gpui_kit::test]
fn retained_advance_finishes_an_admitted_terminal_command_offpage_without_rendering(
    cx: &mut gpui_kit::TestAppContext,
) {
    use crate::sessions::test_support::{connected, peer, stopped, until};
    use gpui_kit::VisualContext as _;
    use std::{io::Read as _, sync::atomic::Ordering};

    let _ui_session = crate::test_support::UiTestSession::new(cx);
    let runtime = crate::app_runtime::AppRuntime::new().unwrap();
    let executor = runtime.executor();
    let (manager, listener, release, opens) = gated_terminal_manager();
    let cx = cx.add_empty_window();
    let workbench = cx.new_window_entity(move |window, cx| {
        let mut this = offline_workbench(window, cx, manager, executor);
        this.inventory.ports = vec![terminal_test_endpoint("A")];
        this.advance_window_state(window, cx);
        this.terminals["A"].input.update(cx, |input, cx| {
            input.set_value("help", window, cx);
        });
        this
    });
    cx.run_until_parked();
    cx.update(|window, cx| {
        workbench.update(cx, |this, cx| {
            this.send("A", window, cx);
            assert!(this.set_feature_page(super::FeaturePage::Settings, window, cx));
            assert!(this.terminals["A"].pending.is_some());
            assert!(this.pending_writes.is_empty());
        });
    });
    release.send(()).unwrap();
    let mut stream = peer(&listener);
    cx.update(|window, cx| {
        workbench.update(cx, |this, cx| {
            connected(&mut this.manager, "A");
            assert!(this.advance_serial_sessions(window, cx));
            assert!(this.terminals["A"].pending.is_none());
            assert!(this.terminals["A"].input.read(cx).value().is_empty());
            assert_eq!(this.pending_writes.len(), 1);
            assert_eq!(
                this.terminals
                    .get_mut("A")
                    .unwrap()
                    .history
                    .navigate(true, ""),
                Some("help".into()),
            );
        });
    });
    let mut command = [0; 5];
    stream.read_exact(&mut command).unwrap();
    assert_eq!(&command, b"help\n");
    cx.update(|window, cx| {
        workbench.update(cx, |this, cx| {
            until(|| {
                this.poll_writes();
                this.pending_writes.is_empty()
            });
            for _ in 0..4 {
                assert!(!this.advance_serial_sessions(window, cx));
            }
            this.manager.disconnect("A").unwrap();
            stopped(&mut this.manager, "A");
        });
    });
    let mut remainder = Vec::new();
    stream.read_to_end(&mut remainder).unwrap();
    assert!(
        remainder.is_empty(),
        "the admitted command is sent exactly once"
    );
    assert_eq!(opens.load(Ordering::SeqCst), 1);
}

#[gpui_kit::test]
fn full_workbench_cancels_pending_terminal_command_before_open_completes(
    cx: &mut gpui_kit::TestAppContext,
) {
    use crate::sessions::test_support::{
        connected, peer, socket_transport, stopped, with_socket_factory,
    };
    use gpui_kit::{VisualContext as _, px, size, test::TestWindowExt as _};
    use std::io::Read as _;

    let runtime = crate::app_runtime::AppRuntime::new().unwrap();
    let executor = runtime.executor();
    let _ui_session = crate::test_support::UiTestSession::new(cx);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (opening_tx, opening_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = parking_lot::Mutex::new(release_rx);
    let mut manager = with_socket_factory(move |_| {
        opening_tx.send(()).unwrap();
        // Dropping the sender also releases the worker if a test assertion fails.
        let _ = release_rx.lock().recv();
        socket_transport(address)
    });
    manager.connect("A").unwrap();
    opening_rx.recv_timeout(Duration::from_secs(3)).unwrap();

    let cx = cx.add_empty_window();
    let workbench = cx.new_window_entity(move |window, cx| {
        let mut this = offline_workbench(window, cx, manager, executor);
        this.inventory.ports = vec![terminal_test_endpoint("A")];
        this.sync_tabs(window, cx);
        this
    });
    cx.replace_root_view(|window, cx| gpui_kit::base::Root::new(workbench.clone(), window, cx));
    cx.simulate_resize(size(px(1200.), px(800.)));
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    let input_id = workbench.read_with(cx, |this, _| this.terminals["A"].input.entity_id());

    cx.update(|window, cx| {
        window.render_frame(cx);
        assert!(window.is_window_active());
        window.click(("input", input_id), cx);
        window.input("help", cx);
    });
    cx.run_until_parked();
    cx.update(|window, cx| window.click("terminal-send", cx));
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("terminal-send").disabled(), Some(true));
        assert_eq!(window.find(("input", input_id)).value(), Some("help"));
        workbench.update(cx, |this, _| {
            assert_eq!(
                this.manager.session("A").unwrap().state,
                ConnectionState::Connecting
            );
            assert_eq!(
                this.terminals["A"].pending.as_ref().unwrap().command,
                "help"
            );
            assert!(this.pending_writes.is_empty());
        });
        window.click("terminal-cancel", cx);
    });
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("terminal-cancel").is_none());
        assert_eq!(window.find(("input", input_id)).value(), Some("help"));
        workbench.update(cx, |this, _| {
            assert!(this.terminals["A"].pending.is_none());
            assert!(this.terminals["A"].status.is_none());
        });
    });

    release_tx.send(()).unwrap();
    let mut stream = peer(&listener);
    cx.update(|window, cx| {
        workbench.update(cx, |this, cx| {
            connected(&mut this.manager, "A");
            this.advance_serial_sessions(window, cx);
            cx.notify();
        });
    });
    cx.update(|window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find(("input", input_id)).value(), Some("help"));
    });
    workbench.update(cx, |this, _| {
        assert!(this.terminals["A"].pending.is_none());
        assert!(this.pending_writes.is_empty());
        this.manager.disconnect("A").unwrap();
        stopped(&mut this.manager, "A");
    });
    let mut sent = Vec::new();
    stream.read_to_end(&mut sent).unwrap();
    assert!(
        sent.is_empty(),
        "cancelled command must not be sent on open"
    );
}

#[gpui_kit::test]
fn send_all_tracks_admitted_connected_sessions_without_reserving_footer_space(
    cx: &mut gpui_kit::TestAppContext,
) {
    use crate::sessions::test_support::{
        display_manager, display_session_state, display_test_session,
    };
    use gpui_kit::{
        AppContext as _, Bounds, WindowBounds, WindowOptions, px, size, test::TestWindowExt as _,
    };

    let runtime = crate::app_runtime::AppRuntime::new().unwrap();
    let _ui_session = crate::test_support::UiTestSession::new(cx);
    let executor = runtime.executor();
    let mut manager = display_manager();
    let _display_a = display_test_session(&mut manager, "A");
    let _display_b = display_test_session(&mut manager, "B");
    display_session_state(&mut manager, "A", ConnectionState::Disconnected);
    display_session_state(&mut manager, "B", ConnectionState::Disconnected);
    let (window, workspace) = cx.update(|cx| {
        gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    Default::default(),
                    size(px(800.), px(600.)),
                ))),
                ..Default::default()
            },
            cx,
            |window, cx| {
                let workbench = cx.new(|cx| {
                    let mut this = offline_workbench(window, cx, manager, executor);
                    this.inventory.ports = ["A", "B", "not-open"]
                        .into_iter()
                        .map(terminal_test_endpoint)
                        .collect();
                    this.selected.insert("port:not-open".into());
                    this.sync_tabs(window, cx);
                    this
                });
                cx.new(|_| TerminalWorkspace(workbench))
            },
        )
        .unwrap()
    });
    let workbench = cx.update(|cx| workspace.read(cx).0.clone());
    let input_id = workbench.read_with(cx, |this, _| this.terminals["A"].input.entity_id());
    let check = |cx: &mut gpui_kit::TestAppContext, shown: bool| {
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            let send = window.find("terminal-send").bounds();
            let input = window.find(("input", input_id)).bounds();
            let clear = window.find("terminal-clear").bounds();
            assert!(input.right() <= send.left());
            assert!(
                (send.size.height - input.size.height).abs() <= px(1.),
                "Send must use the same height as the command input",
            );
            assert!((send.top() - input.top()).abs() <= px(1.));
            assert!((send.size.width - send.size.height).abs() <= px(1.));
            assert!(clear.top() >= input.bottom());
            assert!(
                clear.top() - input.bottom() <= window.rem_size(),
                "the existing toolbar must follow the input without an extra row",
            );
            match window.try_find("terminal-send-all") {
                Some(all) => {
                    assert!(shown);
                    assert_eq!(all.disabled(), Some(true));
                    let all = all.bounds();
                    assert!(all.top() >= send.bottom());
                    assert!((all.right() - send.right()).abs() <= px(1.));
                    assert!((all.top() - clear.top()).abs() <= px(1.));
                }
                None => assert!(!shown),
            }
        })
        .unwrap();
    };

    check(cx, false);
    workbench.update(cx, |this, cx| {
        display_session_state(&mut this.manager, "A", ConnectionState::Connecting);
        cx.notify();
    });
    check(cx, false);
    workbench.update(cx, |this, cx| {
        display_session_state(&mut this.manager, "A", ConnectionState::Connected);
        display_session_state(&mut this.manager, "B", ConnectionState::Connecting);
        cx.notify();
    });
    check(cx, false);
    workbench.update(cx, |this, cx| {
        display_session_state(&mut this.manager, "B", ConnectionState::Connected);
        cx.notify();
    });
    check(cx, true);

    workbench.update(cx, |this, cx| {
        let endpoint = this
            .inventory
            .ports
            .iter_mut()
            .find(|endpoint| endpoint.port_name == "B")
            .unwrap();
        endpoint.vid = Some(0x1209);
        endpoint.pid = Some(0x7690);
        endpoint.usb_interface = Some(crate::domain::UsbInterface {
            number: Some(2),
            description: crate::domain::InterfaceDescription::Value("SlimeNRF Raw Data CDC".into()),
        });
        cx.notify();
    });
    check(cx, false);
    workbench.update(cx, |this, cx| {
        *this
            .inventory
            .ports
            .iter_mut()
            .find(|endpoint| endpoint.port_name == "B")
            .unwrap() = terminal_test_endpoint("B");
        cx.notify();
    });
    check(cx, true);
    cx.simulate_window_resize(window, size(px(420.), px(600.)));
    check(cx, true);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        let all = window.find("terminal-send-all").bounds();
        let clear = window.find("terminal-clear").bounds();
        let wrap = window.find("terminal-wrap").bounds();
        assert!(
            wrap.top() > clear.top(),
            "the toolbar should wrap at narrow widths"
        );
        assert!(wrap.right() <= all.left());
    })
    .unwrap();

    workbench.update(cx, |this, cx| {
        display_session_state(&mut this.manager, "B", ConnectionState::Closing);
        cx.notify();
    });
    check(cx, false);
    workbench.update(cx, |this, cx| {
        display_session_state(&mut this.manager, "A", ConnectionState::Faulted);
        display_session_state(&mut this.manager, "B", ConnectionState::Disconnected);
        cx.notify();
    });
    check(cx, false);
}

#[gpui_kit::test]
fn terminal_send_pointer_enter_history_and_send_all_use_real_command_routes(
    cx: &mut gpui_kit::TestAppContext,
) {
    use crate::sessions::test_support::{connected, peer, stopped, until};
    use gpui_kit::{VisualContext as _, test::TestWindowExt as _};
    use std::io::Read as _;

    let _ui_session = crate::test_support::UiTestSession::new(cx);
    let (workbench, cx, _runtime, listener) = terminal_fixture(cx);
    workbench.update(cx, |this, _| {
        this.inventory.ports = ["A", "B"].into_iter().map(terminal_test_endpoint).collect();
        this.manager.connect("A").unwrap();
    });
    let mut peer_a = peer(&listener);
    workbench.update(cx, |this, _| {
        connected(&mut this.manager, "A");
        this.manager.connect("B").unwrap();
    });
    let mut peer_b = peer(&listener);
    workbench.update(cx, |this, _| connected(&mut this.manager, "B"));
    let workspace = cx.new_window_entity(|window, cx| {
        let focus = workbench.read(cx).page_focus.clone();
        focus.focus(window, cx);
        TerminalWorkspace(workbench.clone())
    });
    cx.replace_root_view(|window, cx| gpui_kit::base::Root::new(workspace, window, cx));
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    let input_id = workbench.read_with(cx, |this, _| this.terminals["A"].input.entity_id());

    cx.update(|window, cx| {
        window.render_frame(cx);
        assert!(window.is_window_active());
        window.click(("input", input_id), cx);
        window.input("help", cx);
        window.click("terminal-send", cx);
    });
    let mut command = [0; 5];
    peer_a.read_exact(&mut command).unwrap();
    assert_eq!(&command, b"help\n");
    cx.update(|window, cx| {
        window.click(("input", input_id), cx);
        window.input("info", cx);
        window.press("enter", cx);
    });
    cx.run_until_parked();
    peer_a.read_exact(&mut command).unwrap();
    assert_eq!(&command, b"info\n");

    cx.update(|window, cx| {
        window.render_frame(cx);
        window.click(("input", input_id), cx);
        window.press("up", cx);
        assert_eq!(window.find(("input", input_id)).value(), Some("info"));
        window.press("up", cx);
        assert_eq!(window.find(("input", input_id)).value(), Some("help"));
        window.press("down", cx);
        assert_eq!(window.find(("input", input_id)).value(), Some("info"));
        window.press("down", cx);
        assert_eq!(window.find(("input", input_id)).value(), Some(""));
        window.input("meow", cx);
        window.click("terminal-send-all", cx);
    });
    peer_a.read_exact(&mut command).unwrap();
    assert_eq!(&command, b"meow\n");
    peer_b.read_exact(&mut command).unwrap();
    assert_eq!(&command, b"meow\n");
    workbench.update(cx, |this, _| {
        assert_eq!(this.terminal_command_runs().count(), 1);
        until(|| this.manager.writes_idle("A") && this.manager.writes_idle("B"));
        this.manager.disconnect("A").unwrap();
        this.manager.disconnect("B").unwrap();
        stopped(&mut this.manager, "A");
        stopped(&mut this.manager, "B");
    });
    drop(workbench);
    drop(_ui_session);
}
