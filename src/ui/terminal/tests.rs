use super::{
    TabSelection, TerminalConnect, TerminalExport, consume_terminal_input_event,
    take_terminal_command,
};
use crate::ui::test_support::offline_workbench;
use crate::{
    domain::SerialEndpoint, sessions::ConnectionState, ui::command_panel::CommandConnection,
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
    let _ui_session = crate::ui::test_support::UiTestSession::new(cx);
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
                Some(crate::ui::command_panel::CommandFocus::Serial("B".into()))
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
    let _ui_session = crate::ui::test_support::UiTestSession::new(cx);
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
    let _ui_session = crate::ui::test_support::UiTestSession::new(cx);
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
            this.toggle_connection("A", cx);
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
            this.active_page = crate::ui::FeaturePage::Settings;
            this.sync_tabs(window, cx);
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
            this.sync_tabs(window, cx);
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
            this.sync_tabs(window, cx);
            assert!(this.manager.session("A").is_none());
            assert_eq!(this.terminals.len(), 1);
            assert_eq!(this.tabs.active.as_deref(), Some("B"));
            assert_eq!(
                this.terminals["B"].input.read(cx).value().as_ref(),
                "retained B draft",
            );
        });
    });
}

#[gpui_kit::test]
fn a_replacement_generation_does_not_inherit_a_previous_terminal_close(
    cx: &mut gpui_kit::TestAppContext,
) {
    let _ui_session = crate::ui::test_support::UiTestSession::new(cx);
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
    let _ui_session = crate::ui::test_support::UiTestSession::new(cx);
    use crate::operations::CommandStepResult;
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
    let _ui_session = crate::ui::test_support::UiTestSession::new(cx);
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
    let _ui_session = crate::ui::test_support::UiTestSession::new(cx);
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
                Some(crate::ui::command_panel::CommandFocus::Serial("B".into())),
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
    let _ui_session = crate::ui::test_support::UiTestSession::new(cx);
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
    let _ui_session = crate::ui::test_support::UiTestSession::new(cx);
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
    let _ui_session = crate::ui::test_support::UiTestSession::new(cx);
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
        window.click("terminal-copy-selection", cx);
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("Synthetic terminal transcript".into())
        );
        #[cfg(target_os = "macos")]
        window.press("cmd-a", cx);
        #[cfg(not(target_os = "macos"))]
        window.press("ctrl-a", cx);
        window.click("terminal-copy-selection", cx);
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
