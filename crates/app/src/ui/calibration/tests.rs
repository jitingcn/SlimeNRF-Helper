use super::{
    AfterJoin, CalibrationUiState, Guide, RunCue, calibration_target_console_allowed, can_advance,
    can_return_to_preparation, next_axis, run_cue, sensitivity_task,
};
use crate::calibration::{
    Application, Axis, CalibrationSession, CalibrationTask, Engine, Fact, GuideSelection,
    ObservationEnd, Revolutions, RunMode, RunRequest, Target,
};
use crate::discovery::ReceiverMonitor;
use crate::domain::{
    InterfaceDescription, Inventory, ReceiverEndpoint, SerialEndpoint, SerialPortKind, UsbInterface,
};
use crate::sessions::test_support;

#[gpui_kit::test]
fn shutdown_rejects_calibration_admission_without_starting_a_worker(
    cx: &mut gpui_kit::TestAppContext,
) {
    use gpui_kit::VisualContext as _;

    let _ui_session = crate::ui::test_support::UiTestSession::new(cx);
    let runtime = crate::app_runtime::AppRuntime::new().unwrap();
    let executor = runtime.executor();
    let cx = cx.add_empty_window();
    let workbench = cx.new_window_entity(move |window, cx| {
        crate::ui::test_support::offline_workbench(
            window,
            cx,
            crate::sessions::SessionManager::new(),
            executor,
        )
    });
    workbench.update(cx, |this, cx| {
        this.shutting_down = true;
        this.inspect_calibration(cx);
        this.review_calibration(cx);
        this.start_calibration(cx);
        this.recover_calibration(cx);
        assert!(!this.calibration.session.busy());
        assert!(this.calibration.error.is_none());
        this.shutting_down = false;
        this.inspect_calibration(cx);
        assert!(this.calibration.error.is_some(), "normal admission resumes");
    });
}

fn engine(task: CalibrationTask) -> Engine {
    Engine::new(RunRequest {
        id: 7,
        target: Target::Direct {
            endpoint: SerialEndpoint {
                port_name: "fixture".into(),
                label: "Fixture".into(),
                manufacturer: None,
                product: None,
                kind: SerialPortKind::Usb,
                serial_number: Some("fixture".into()),
                vid: Some(1),
                pid: Some(2),
                usb_interface: None,
            },
            generation: None,
            address: Some("112233445566".into()),
        },
        task,
        mode: RunMode::Execute,
        recovery: None,
    })
}

#[test]
fn joined_inspection_retires_predecessor_generation_with_unchanged_identity() {
    let mut engine = engine(CalibrationTask::Stationary);
    let mut target = engine.snapshot.target.clone();
    let Target::Direct { generation, .. } = &mut target else {
        unreachable!();
    };
    *generation = Some(9);
    engine.snapshot.target = target.clone();
    engine.fact(Fact::NativeOpen(1), 0);
    engine.fact(
        Fact::Identity {
            address: "112233445566".into(),
            stationary: true,
            sensitivity: true,
        },
        10,
    );
    let mut state = CalibrationUiState {
        selection: Some(GuideSelection {
            target,
            label: "Fixture".into(),
        }),
        ..Default::default()
    };
    assert!(!state.accept_joined_selection(&engine.snapshot));
    let Target::Direct { generation, .. } = &state.selection.as_ref().unwrap().target else {
        unreachable!();
    };
    assert_eq!(*generation, Some(9));
    engine.finish(ObservationEnd::Complete, 20);
    engine.snapshot.joined = true;
    assert!(state.accept_joined_selection(&engine.snapshot));
    let Target::Direct {
        generation,
        address,
        ..
    } = &state.selection.as_ref().unwrap().target
    else {
        unreachable!();
    };
    assert_eq!(*generation, None);
    assert_eq!(address.as_deref(), Some("112233445566"));
    assert!(!state.accept_joined_selection(&engine.snapshot));
}

#[test]
fn joined_selection_retires_generation_independently_of_identity_update() {
    for identity in [None, Some("AABBCCDDEEFF")] {
        let mut engine = engine(CalibrationTask::Stationary);
        let mut target = engine.snapshot.target.clone();
        let Target::Direct { generation, .. } = &mut target else {
            unreachable!();
        };
        *generation = Some(9);
        engine.snapshot.target = target.clone();
        engine.fact(Fact::NativeOpen(1), 0);
        if let Some(address) = identity {
            engine.fact(
                Fact::Identity {
                    address: address.into(),
                    stationary: true,
                    sensitivity: true,
                },
                10,
            );
        }
        engine.finish(ObservationEnd::Complete, 20);
        engine.snapshot.joined = true;
        let mut state = CalibrationUiState {
            selection: Some(GuideSelection {
                target,
                label: "Fixture".into(),
            }),
            ..Default::default()
        };
        assert!(state.accept_joined_selection(&engine.snapshot));
        let Target::Direct {
            generation,
            address,
            ..
        } = &state.selection.as_ref().unwrap().target
        else {
            unreachable!();
        };
        assert_eq!(*generation, None);
        assert_eq!(address.as_deref(), Some(identity.unwrap_or("112233445566")));
    }
}

#[test]
fn sensitivity_form_requires_explicit_bounded_count_for_each_axis() {
    for axis in [Axis::X, Axis::Y, Axis::Z] {
        for count in [1, 5, 100] {
            assert_eq!(
                sensitivity_task(axis, &count.to_string()).unwrap(),
                CalibrationTask::Sensitivity {
                    axis,
                    revolutions: Revolutions::new(count).unwrap()
                }
            );
        }
        for count in ["", "0", "101", "-1", "2.5", "65536"] {
            assert!(sensitivity_task(axis, count).is_err());
        }
    }
}

#[test]
fn queued_measurements_never_offer_applied_finish_or_next_axis() {
    let mut engine = engine(CalibrationTask::Stationary);
    engine.fact(Fact::Application(Application::CandidateQueued), 100);
    engine.finish(ObservationEnd::Complete, 20_100);
    engine.snapshot.joined = true;
    assert!(!can_advance(&engine.snapshot));
}

#[test]
fn preparation_return_waits_for_join_and_required_acknowledgement() {
    for application in [Application::Rejected, Application::Failed] {
        let mut engine = engine(CalibrationTask::Stationary);
        engine.fact(Fact::DispatchAttempt, 0);
        engine.fact(Fact::Application(application), 10);
        assert!(!can_return_to_preparation(&engine.snapshot));
        engine.snapshot.joined = true;
        assert!(can_return_to_preparation(&engine.snapshot));
    }
    let mut engine = engine(CalibrationTask::Stationary);
    engine.fact(Fact::DispatchAttempt, 0);
    engine.fact(Fact::Application(Application::CandidateQueued), 10);
    engine.snapshot.joined = true;
    assert!(!can_return_to_preparation(&engine.snapshot));
    engine.snapshot.acknowledged = true;
    assert!(can_return_to_preparation(&engine.snapshot));
}

#[test]
fn inspection_outcomes_never_use_calibration_application_cues() {
    let mut engine = engine(CalibrationTask::Stationary);
    engine.snapshot.mode = RunMode::Inspect;
    assert_eq!(run_cue(&engine.snapshot, false), RunCue::Inspecting);
    assert!(!can_return_to_preparation(&engine.snapshot));
    engine.snapshot.joined = true;
    engine.snapshot.end = Some(ObservationEnd::PreparationFailed);
    assert_eq!(run_cue(&engine.snapshot, false), RunCue::InspectionFailed);
    assert!(can_return_to_preparation(&engine.snapshot));
    engine.snapshot.end = Some(ObservationEnd::Stopped);
    assert_eq!(run_cue(&engine.snapshot, false), RunCue::InspectionStopped);
    engine.snapshot.end = Some(ObservationEnd::Complete);
    engine.snapshot.identity = Some("112233445566".into());
    assert_eq!(run_cue(&engine.snapshot, false), RunCue::Inspected);
}

#[test]
fn next_axis_prepares_only_and_keeps_retained_session() {
    let task = sensitivity_task(Axis::X, "5").unwrap();
    let mut engine = engine(task);
    engine.fact(Fact::Application(Application::Applied), 100);
    assert!(can_advance(&engine.snapshot));
    assert!(!engine.snapshot.joined);
    assert_eq!(next_axis(task), Some(Axis::Y));
    let mut state = CalibrationUiState {
        guide: Guide::Sensitivity,
        after_join: Some(super::PendingAdvance {
            id: 7,
            action: AfterJoin::NextAxis(Axis::Y),
            persistence: engine.snapshot.persistence,
        }),
        ..Default::default()
    };
    state.invalidate_review();
    assert!(state.take_joined_action(&engine.snapshot).is_none());
    assert!(state.after_join.is_some());
    engine.snapshot.joined = true;
    assert!(matches!(
        state.take_joined_action(&engine.snapshot),
        Some(Ok(AfterJoin::NextAxis(Axis::Y)))
    ));
    assert!(state.after_join.is_none());
    assert!(state.take_joined_action(&engine.snapshot).is_none());
    assert_eq!(next_axis(sensitivity_task(Axis::Z, "5").unwrap()), None);
}

#[test]
fn sensitivity_instructions_wait_for_firmware_rotation_cue() {
    for axis in [Axis::X, Axis::Y, Axis::Z] {
        let mut engine = engine(sensitivity_task(axis, "5").unwrap());
        engine.fact(Fact::Phase(crate::calibration::Phase::WaitingStill), 10);
        assert_eq!(run_cue(&engine.snapshot, false), RunCue::HoldStill);
        engine.fact(Fact::Phase(crate::calibration::Phase::WaitingRotation), 20);
        let cue = RunCue::Rotate {
            axis,
            revolutions: Revolutions::new(5).unwrap(),
        };
        assert_eq!(run_cue(&engine.snapshot, false), cue);
        engine.fact(Fact::Phase(crate::calibration::Phase::Recording), 30);
        assert_eq!(run_cue(&engine.snapshot, false), cue);
        engine.fact(Fact::Phase(crate::calibration::Phase::Validating), 40);
        assert_eq!(run_cue(&engine.snapshot, false), RunCue::Validating);
    }
}

#[test]
fn closing_cue_replaces_live_instructions_until_join_without_rewriting_evidence() {
    for mode in [RunMode::Inspect, RunMode::Execute] {
        let mut engine = engine(CalibrationTask::Stationary);
        engine.snapshot.mode = mode;
        engine.fact(Fact::DispatchAttempt, 1);
        engine.fact(Fact::Phase(crate::calibration::Phase::WaitingStill), 2);
        assert_eq!(
            run_cue(&engine.snapshot, false),
            if mode == RunMode::Inspect {
                RunCue::Inspecting
            } else {
                RunCue::HoldStill
            }
        );
        assert_eq!(run_cue(&engine.snapshot, true), RunCue::Closing);
        assert_eq!(engine.snapshot.application, Application::Unconfirmed);
        engine.finish(ObservationEnd::Stopped, 3);
        assert_eq!(run_cue(&engine.snapshot, false), RunCue::Closing);
        engine.snapshot.joined = true;
        assert_eq!(
            run_cue(&engine.snapshot, true),
            if mode == RunMode::Inspect {
                RunCue::InspectionStopped
            } else {
                RunCue::Result(Application::Unconfirmed)
            }
        );
        assert_eq!(engine.snapshot.application, Application::Unconfirmed);
    }
}

#[test]
fn closing_applied_result_retains_late_storage_error_through_join() {
    let mut engine = engine(sensitivity_task(Axis::X, "5").unwrap());
    engine.fact(Fact::Application(Application::Applied), 1);
    assert_eq!(
        run_cue(&engine.snapshot, false),
        RunCue::Result(Application::Applied)
    );
    assert_eq!(run_cue(&engine.snapshot, true), RunCue::Closing);
    engine.fact(Fact::StorageError, 2);
    assert_eq!(run_cue(&engine.snapshot, true), RunCue::Closing);
    assert_eq!(engine.snapshot.application, Application::Applied);
    assert_eq!(
        engine.snapshot.persistence,
        crate::calibration::Persistence::StorageError
    );
    engine.finish(ObservationEnd::Stopped, 3);
    assert_eq!(run_cue(&engine.snapshot, false), RunCue::Closing);
    engine.snapshot.joined = true;
    assert_eq!(
        run_cue(&engine.snapshot, false),
        RunCue::Result(Application::Applied)
    );
    assert_eq!(
        engine.snapshot.persistence,
        crate::calibration::Persistence::StorageError
    );
}

#[test]
fn pending_finish_never_consumes_a_different_run() {
    let mut engine = engine(CalibrationTask::Stationary);
    engine.snapshot.joined = true;
    let mut state = CalibrationUiState {
        after_join: Some(super::PendingAdvance {
            id: 8,
            action: AfterJoin::Finish,
            persistence: engine.snapshot.persistence,
        }),
        ..Default::default()
    };
    assert!(state.take_joined_action(&engine.snapshot).is_none());
    assert!(state.after_join.is_some());
}

#[test]
fn late_storage_error_requires_review_before_advancing() {
    let mut engine = engine(sensitivity_task(Axis::X, "5").unwrap());
    engine.fact(Fact::Application(Application::Applied), 10);
    let mut state = CalibrationUiState {
        after_join: Some(super::PendingAdvance {
            id: 7,
            action: AfterJoin::NextAxis(Axis::Y),
            persistence: engine.snapshot.persistence,
        }),
        ..Default::default()
    };
    engine.fact(Fact::StorageError, 20);
    engine.snapshot.joined = true;
    assert!(state.take_joined_action(&engine.snapshot).unwrap().is_err());
    assert_eq!(engine.snapshot.application, Application::Applied);
    assert!(!engine.snapshot.acknowledged);
}

#[test]
fn joined_calibration_inspection_cannot_refresh_a_replaced_serial_interface() {
    let mut engine = engine(CalibrationTask::Stationary);
    engine.snapshot.joined = true;
    let mut target = engine.snapshot.target.clone();
    let Target::Direct {
        endpoint,
        generation,
        ..
    } = &mut target
    else {
        unreachable!();
    };
    *generation = Some(9);
    endpoint.usb_interface = Some(crate::domain::UsbInterface {
        number: Some(2),
        description: crate::domain::InterfaceDescription::Unavailable,
    });
    let mut state = CalibrationUiState {
        selection: Some(GuideSelection {
            target,
            label: "Fixture".into(),
        }),
        ..Default::default()
    };
    assert!(!state.accept_joined_selection(&engine.snapshot));
    let Target::Direct { generation, .. } = &state.selection.as_ref().unwrap().target else {
        unreachable!();
    };
    assert_eq!(*generation, Some(9));
}

fn composite_calibration_inventory() -> Inventory {
    let Target::Direct { mut endpoint, .. } = engine(CalibrationTask::Stationary).snapshot.target
    else {
        unreachable!();
    };
    endpoint.product = Some("SlimeNRF Tracker".into());
    let mut console = endpoint.clone();
    console.port_name = "receiver-console".into();
    console.serial_number = Some("synthetic-receiver".into());
    console.vid = Some(0x1209);
    console.pid = Some(0x7690);
    console.usb_interface = Some(UsbInterface {
        number: Some(0),
        description: InterfaceDescription::Unavailable,
    });
    let mut raw = console.clone();
    raw.port_name = "receiver-raw".into();
    raw.usb_interface = Some(UsbInterface {
        number: Some(2),
        description: InterfaceDescription::Value("SlimeNRF Raw Data CDC".into()),
    });
    Inventory {
        ports: vec![endpoint, console, raw],
        receivers: vec![ReceiverEndpoint {
            key: "synthetic-receiver-hid".into(),
            path: b"synthetic-receiver-hid".to_vec(),
            label: "Synthetic receiver".into(),
            serial_number: Some("synthetic-receiver".into()),
        }],
        ..Inventory::default()
    }
}

#[test]
fn calibration_target_admission_keeps_raw_siblings_as_exclusions_not_console_targets() {
    let inventory = composite_calibration_inventory();
    let receiver = inventory.receivers[0].clone();
    let monitor = ReceiverMonitor::fixture(receiver.clone(), 2, "112233445566", true);
    let manager = test_support::display_manager();
    let session = CalibrationSession::default();
    for target in [
        Target::Wireless {
            receiver,
            slot: 2,
            address: "112233445566".into(),
        },
        Target::Direct {
            endpoint: inventory.ports[0].clone(),
            generation: None,
            address: Some("112233445566".into()),
        },
    ] {
        let ticket = session
            .review(
                GuideSelection {
                    target,
                    label: "Synthetic tracker".into(),
                },
                CalibrationTask::Stationary,
                &inventory,
                &manager,
                &monitor,
            )
            .unwrap();
        let expected = match ticket.target() {
            Target::Wireless { .. } => vec!["receiver-console", "receiver-raw"],
            Target::Direct { .. } => vec!["fixture", "receiver-console", "receiver-raw"],
        };
        assert_eq!(
            ticket.refresh_ports(&inventory, &manager, &monitor),
            expected
        );
        assert!(!crate::domain::console_port_allowed(
            &inventory.ports[2],
            &inventory
        ));
        assert!(calibration_target_console_allowed(
            ticket.target(),
            &inventory
        ));
    }
    let raw_target = Target::Direct {
        endpoint: inventory.ports[2].clone(),
        generation: None,
        address: Some("112233445566".into()),
    };
    assert!(!calibration_target_console_allowed(&raw_target, &inventory));
    assert!(
        session
            .review(
                GuideSelection {
                    target: raw_target,
                    label: "Not a console".into(),
                },
                CalibrationTask::Stationary,
                &inventory,
                &manager,
                &monitor,
            )
            .is_err()
    );
}

#[test]
fn calibration_direct_console_admission_rechecks_current_inventory() {
    let mut inventory = composite_calibration_inventory();
    let target = Target::Direct {
        endpoint: inventory.ports[1].clone(),
        generation: None,
        address: None,
    };
    assert!(calibration_target_console_allowed(&target, &inventory));
    let mut unknown_sibling = inventory.ports[1].clone();
    unknown_sibling.port_name = "receiver-unknown".into();
    unknown_sibling.usb_interface.as_mut().unwrap().number = Some(4);
    inventory.ports.push(unknown_sibling);
    assert!(!calibration_target_console_allowed(&target, &inventory));
    inventory.ports.pop();
    inventory.serial_enumeration_failed = true;
    assert!(!calibration_target_console_allowed(&target, &inventory));
}
