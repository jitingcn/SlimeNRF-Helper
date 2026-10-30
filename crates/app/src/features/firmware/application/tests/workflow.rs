use super::support::{confirmed_ota_state, local_image, ota_workflow_state, target};
use crate::application::flash::confirmation::ota_confirmation_receivers;
use crate::application::flash::{
    Action, FlashState, Mode, OtaPlanGroup, OtaQueries, OtaQueryPlan, OtaStep, TrackerSection,
    ota_assignment_key, plan_ota_groups, tracker_section,
};
use crate::domain::TrackerLink;
use crate::features::firmware::esb_ota::RECEIVER_ID;
use std::sync::Arc;
use std::time::Duration;

#[test]
fn ota_start_consumes_authority_without_restoring_navigation() {
    let mut state = confirmed_ota_state();
    let action = state.take_confirmed_action().unwrap();
    assert!(matches!(action, Action::EsbGroups(_)));
    assert!(state.review.is_none());
    assert!(!state.acknowledged);
    assert!(state.take_confirmed_action().is_none());
    state.reconcile_ota_step(Mode::Esb);
    state.ota_back();
    state.ota_advance().unwrap();
    assert_eq!(state.ota_step, OtaStep::Confirm);
    assert_eq!(
        state.ota_presentation,
        crate::application::flash::OtaPresentation::Running
    );
    assert!(!state.ota_shows_next());
    state.ota_new_update();
    assert_eq!(
        state.ota_presentation,
        crate::application::flash::OtaPresentation::Running
    );
}

#[test]
fn ota_finished_results_and_dismissal_do_not_restore_next() {
    let mut state = confirmed_ota_state();
    let selected = state.selected_targets.clone();
    let images = state
        .assignments
        .values()
        .map(|image| image.bytes.clone())
        .collect::<Vec<_>>();
    let action = state.take_confirmed_action().unwrap();
    state.activity_origin = Mode::Esb;
    state.activity_kind = crate::application::flash::ActivityKind::Operation;
    state.prepare_action_results(&action);
    state.finish_job_progress(None);
    state.apply_joined_operation_results(
        vec![(
            crate::application::flash::ResultTarget::ota(&state.targets[0]),
            crate::update::UpdateOutcome::Completed,
        )],
        std::time::Instant::now(),
    );
    assert!(state.shows_results(Mode::Esb));
    state.dismiss_results(Mode::Esb);
    state.reconcile_ota_step(Mode::Esb);
    assert!(!state.shows_results(Mode::Esb));
    assert_eq!(state.results.len(), 1);
    assert_eq!(
        state.ota_presentation,
        crate::application::flash::OtaPresentation::Finished
    );
    assert!(!state.ota_shows_next());
    state.results.clear();
    state.reconcile_ota_step(Mode::Esb);
    assert!(!state.ota_shows_next());
    state.ota_new_update();
    assert!(state.ota_shows_next());
    assert_eq!(state.ota_step, OtaStep::Devices);
    assert_eq!(state.selected_targets, selected);
    assert!(
        state
            .assignments
            .values()
            .all(|image| images.iter().any(|bytes| Arc::ptr_eq(bytes, &image.bytes)))
    );
    assert!(state.review.is_none());
    assert!(!state.acknowledged);
    state.apply_joined_operation_results(
        vec![(
            crate::application::flash::ResultTarget::ota(&state.targets[0]),
            crate::update::UpdateOutcome::Completed,
        )],
        std::time::Instant::now(),
    );
    assert!(state.shows_results(Mode::Esb));
}

#[test]
fn ota_running_cancel_retains_finished_navigation_until_explicit_restart() {
    let mut state = confirmed_ota_state();
    let action = state.take_confirmed_action().unwrap();
    state.prepare_action_results(&action);
    state.activity_origin = Mode::Esb;
    state.activity_kind = crate::application::flash::ActivityKind::Operation;
    let (release, waiting) = std::sync::mpsc::channel();
    state.job = Some(
        crate::update::Job::start(move |cancel, _| {
            waiting.recv_timeout(Duration::from_secs(5)).unwrap();
            assert!(cancel.load(std::sync::atomic::Ordering::Acquire));
            Err("cancelled".into())
        })
        .unwrap(),
    );
    state.cancel_activity();
    state.reconcile_ota_step(Mode::Esb);
    assert!(state.job.is_some());
    assert_eq!(
        state.ota_presentation,
        crate::application::flash::OtaPresentation::Running
    );
    assert!(!state.ota_shows_next());
    state.ota_new_update();
    assert_eq!(
        state.ota_presentation,
        crate::application::flash::OtaPresentation::Running
    );
    release.send(()).unwrap();
    state.job.take().unwrap().shutdown();
    state.finish_job_progress(None);
    assert_eq!(
        state.ota_presentation,
        crate::application::flash::OtaPresentation::Finished
    );
    assert!(!state.ota_shows_next());
    state.ota_new_update();
    assert!(state.ota_shows_next());
}

#[test]
fn ota_pending_failure_cancellation_and_worker_errors_finish_presentation() {
    for cancelled in [false, true] {
        let mut state = confirmed_ota_state();
        let action = state.take_confirmed_action().unwrap();
        state.activity_origin = Mode::Esb;
        state.activity_kind = crate::application::flash::ActivityKind::Operation;
        state.prepare_action_results(&action);
        state.pending = Some(action);
        if cancelled {
            state.cancel_activity();
        } else {
            state.set_activity_error("reservation failed".into());
        }
        state.pending.take();
        state.finish_job_progress(None);
        state.reconcile_ota_step(Mode::Esb);
        assert_eq!(
            state.ota_presentation,
            crate::application::flash::OtaPresentation::Finished
        );
        assert!(!state.ota_shows_next());
        state.ota_new_update();
        assert!(state.ota_shows_next());
    }
    let mut state = confirmed_ota_state();
    state.take_confirmed_action().unwrap();
    state.activity_origin = Mode::Esb;
    state.activity_kind = crate::application::flash::ActivityKind::Operation;
    state.finish_job_progress(None);
    state.set_activity_error("worker failed".into());
    state.reconcile_ota_step(Mode::Esb);
    assert_eq!(
        state.ota_presentation,
        crate::application::flash::OtaPresentation::Finished
    );
    assert!(!state.ota_shows_next());
    assert_eq!(state.error.as_deref(), Some("worker failed"));
}

#[test]
fn ota_dismissal_and_finished_navigation_survive_queries_and_other_modes() {
    let mut state = confirmed_ota_state();
    let action = state.take_confirmed_action().unwrap();
    state.activity_origin = Mode::Esb;
    state.activity_kind = crate::application::flash::ActivityKind::Operation;
    state.prepare_action_results(&action);
    state.finish_job_progress(None);
    state.operation_origin = Mode::Uf2;
    state.apply_joined_operation_results(
        vec![(
            crate::application::flash::ResultTarget::generic("volume".into(), "Volume".into()),
            crate::update::UpdateOutcome::Completed,
        )],
        std::time::Instant::now(),
    );
    let device = state.targets[0].clone();
    let refreshed_key = format!("{}:{}", device.receiver.key, device.id);
    state
        .query_failures
        .insert(refreshed_key.clone(), "timeout".into());
    // A successful refresh must leave another receiver's failure intact.
    state
        .query_failures
        .insert("b:0".into(), "unrelated timeout".into());
    state.dismiss_results(Mode::Esb);
    assert!(state.shows_results(Mode::Uf2));
    assert!(!state.shows_results(Mode::Esb));
    assert_eq!(state.query_failures.len(), 2);
    for mode in [Mode::Uf2, Mode::Ble, Mode::OpenDfu, Mode::Esb] {
        state.transition_mode(mode);
        state.reconcile_ota_step(mode);
        assert_eq!(
            state.ota_presentation,
            crate::application::flash::OtaPresentation::Finished
        );
        assert!(!state.ota_shows_next());
        assert!(state.review.is_none());
    }
    for automatic in [true, false] {
        let query = Action::QueryGroups(OtaQueryPlan {
            groups: vec![],
            automatic,
            incremental: true,
        });
        state.prepare_action_results(&query);
        state.activity_kind = crate::application::flash::ActivityKind::Query;
        state.activity_origin = Mode::Esb;
        state.finish_job_progress(None);
        state.apply_queries(OtaQueries {
            targets: vec![device.clone()],
            failures: vec![],
        });
        assert!(!state.query_failures.contains_key(&refreshed_key));
        assert_eq!(state.query_failures["b:0"], "unrelated timeout");
        assert!(!state.shows_results(Mode::Esb));
        state.reconcile_ota_step(Mode::Esb);
        assert_eq!(
            state.ota_presentation,
            crate::application::flash::OtaPresentation::Finished
        );
        assert!(!state.ota_shows_next());
        assert!(state.shows_results(Mode::Uf2));
    }
    state.dismiss_results(Mode::Uf2);
    assert!(!state.shows_results(Mode::Esb));
    assert!(!state.shows_results(Mode::Uf2));
    assert_eq!(state.results.len(), 1);
    assert!(state.esb_auto_query.receivers.is_empty());
}

#[test]
fn wireless_unqueried_selection_is_distinct_from_empty_and_cannot_advance() {
    let tracker = target("a", 0, "board");
    let link = TrackerLink {
        receiver_key: "a".into(),
        slot: 0,
        address: tracker.address.clone(),
        online: true,
    };
    let selected = ["tracker:a:0".to_string()].into();
    let mut state = FlashState {
        mode: Mode::Esb,
        ..FlashState::default()
    };
    let empty_reason = state.ota_selection_ready().unwrap_err();
    assert!(state.ota_advance().is_err());
    state.sync_tracker_selection(&selected, std::iter::once(&link));
    assert!(state.selected_targets.is_empty());
    assert!(state.unresolved_tracker_selection());
    let unresolved_reason = state.ota_selection_ready().unwrap_err();
    assert_ne!(unresolved_reason, empty_reason);
    assert_eq!(state.ota_advance().unwrap_err(), unresolved_reason);
    assert_eq!(state.ota_step, OtaStep::Devices);
    assert!(plan_ota_groups(&state).is_err());
    assert!(state.review.is_none());

    state.apply_queries(OtaQueries {
        targets: vec![tracker],
        failures: vec![],
    });
    assert!(!state.unresolved_tracker_selection());
    state.ota_advance().unwrap();
    assert_eq!(state.ota_step, OtaStep::Firmware);
    assert!(state.ota_advance().is_err());
    assert_eq!(state.ota_step, OtaStep::Firmware);
    assert!(plan_ota_groups(&state).is_err());
    assert!(state.review.is_none());
}

#[test]
fn wireless_steps_require_resolved_selection_and_every_group_image() {
    let mut state = FlashState::default();
    assert!(state.ota_advance().is_err());
    assert_eq!(state.ota_step, OtaStep::Devices);
    state = ota_workflow_state();
    state.tracker_selection.insert("tracker:a:1".into());
    assert!(state.ota_advance().is_err());
    state.tracker_selection.remove("tracker:a:1");
    state.ota_advance().unwrap();
    assert_eq!(state.ota_step, OtaStep::Firmware);
    let tracker_group = ota_assignment_key(&state.targets[0]);
    let receiver_group = ota_assignment_key(&state.targets[1]);
    state
        .assignments
        .insert(tracker_group, local_image("application.hex"));
    assert!(state.ota_advance().is_err());
    state
        .assignments
        .insert(receiver_group, local_image("application.hex"));
    assert!(!state.reconcile_ota_step(Mode::Esb));
    assert_eq!(state.ota_step, OtaStep::Firmware);
    state.ota_advance().unwrap();
    assert_eq!(state.ota_step, OtaStep::Mapping);
    assert_eq!(
        plan_ota_groups(&state)
            .unwrap()
            .iter()
            .map(|group| group.targets.len())
            .sum::<usize>(),
        2
    );
    // State-only navigation cannot manufacture the final route-checked Action.
    state.ota_advance().unwrap();
    assert_eq!(state.ota_step, OtaStep::Mapping);
    assert!(state.review.is_none());
}

#[test]
fn wireless_back_preserves_group_intent_and_revokes_authorization() {
    let mut state = ota_workflow_state();
    for target in &state.targets {
        state
            .assignments
            .insert(ota_assignment_key(target), local_image("application.hex"));
    }
    state.active_group = Some(ota_assignment_key(&state.targets[1]));
    let focus = state.active_group.clone();
    let generations = state.group_generations.clone();
    let selected = state.selected_targets.clone();
    state.review = Some(Action::EsbGroups(plan_ota_groups(&state).unwrap()));
    state.ota_step = OtaStep::Confirm;
    state.acknowledged = true;
    state.auto_requested = true;
    for expected in [OtaStep::Mapping, OtaStep::Firmware, OtaStep::Devices] {
        state.ota_back();
        assert_eq!(state.ota_step, expected);
        assert_eq!(state.active_group, focus);
        assert_eq!(state.group_generations, generations);
        assert_eq!(state.selected_targets, selected);
        assert_eq!(state.assignments.len(), 2);
        assert!(state.review.is_none());
        assert!(!state.acknowledged);
        assert!(!state.auto_requested);
    }
}

#[test]
fn wireless_review_image_and_selection_invalidation_retreat_safely() {
    let mut state = ota_workflow_state();
    for target in &state.targets {
        state
            .assignments
            .insert(ota_assignment_key(target), local_image("application.hex"));
    }
    state.review = Some(Action::EsbGroups(plan_ota_groups(&state).unwrap()));
    state.ota_step = OtaStep::Confirm;
    state.acknowledged = true;
    state.apply_queries(OtaQueries {
        targets: vec![state.targets[0].clone()],
        failures: vec![],
    });
    assert!(state.reconcile_ota_step(Mode::Esb));
    assert_eq!(state.ota_step, OtaStep::Mapping);
    assert!(!state.acknowledged);
    state.assignments.clear();
    assert!(state.reconcile_ota_step(Mode::Esb));
    assert_eq!(state.ota_step, OtaStep::Firmware);
    state.selected_targets.clear();
    assert!(state.reconcile_ota_step(Mode::Esb));
    assert_eq!(state.ota_step, OtaStep::Devices);
    assert!(state.review.is_none());
}

#[gpui_kit::test]
fn named_firmware_workflow_update_retreats_without_rendering_and_never_auto_advances(
    cx: &mut gpui_kit::TestAppContext,
) {
    use crate::application::{FeaturePage, flash::ota_target_key, test_support::offline_workbench};
    use gpui_kit::VisualContext as _;
    use std::{cell::Cell, collections::BTreeSet, rc::Rc};

    let _ui_session = crate::test_support::UiTestSession::new(cx);
    let runtime = crate::app_runtime::AppRuntime::new().unwrap();
    let executor = runtime.executor();
    let cx = cx.add_empty_window();
    let workbench = cx.new_window_entity(move |window, cx| {
        let mut this =
            offline_workbench(window, cx, crate::sessions::SessionManager::new(), executor);
        this.active_page = FeaturePage::Ota;
        this.flash = confirmed_ota_state();
        this.flash.auto_requested = true;
        this
    });
    cx.run_until_parked();
    let notifications = Rc::new(Cell::new(0));
    let observed = notifications.clone();
    let _notification_subscription =
        cx.update(|_, cx| cx.observe(&workbench, move |_, _| observed.set(observed.get() + 1)));
    let assert_settled = |cx: &mut gpui_kit::VisualTestContext, step, count| {
        cx.run_until_parked();
        workbench.read_with(cx, |this, _| {
            assert_eq!(this.flash.ota_step, step);
            assert!(this.flash.review.is_none());
            assert!(!this.flash.acknowledged);
            assert!(!this.flash.auto_requested);
        });
        assert_eq!(notifications.get(), count);
        cx.run_until_parked();
        assert_eq!(
            notifications.get(),
            count,
            "the observer reaches a fixed point"
        );
    };

    workbench.update(cx, |_, cx| cx.notify());
    cx.run_until_parked();
    assert_eq!(
        notifications.get(),
        1,
        "a valid workflow does not notify again"
    );
    workbench.read_with(cx, |this, _| {
        assert_eq!(this.flash.ota_step, OtaStep::Confirm);
        assert!(this.flash.review.is_some());
        assert!(this.flash.acknowledged);
        assert!(this.flash.auto_requested);
    });

    notifications.set(0);
    workbench.update(cx, |this, _| this.flash.review = None);
    cx.update(|window, cx| {
        workbench.update(cx, |this, cx| {
            assert!(!this.advance_window_state(window, cx));
        });
    });
    cx.run_until_parked();
    assert_eq!(
        notifications.get(),
        0,
        "idle polling does not validate workflow state"
    );
    workbench.read_with(cx, |this, _| {
        assert_eq!(this.flash.ota_step, OtaStep::Confirm);
    });
    workbench.update(cx, |_, cx| cx.notify());
    assert_settled(cx, OtaStep::Mapping, 2);

    notifications.set(0);
    workbench.update(cx, |this, cx| {
        let destination = this.flash.destination(Mode::Esb).unwrap();
        this.flash.apply_images(destination, Vec::new());
        cx.notify();
    });
    assert_settled(cx, OtaStep::Firmware, 2);

    notifications.set(0);
    workbench.update(cx, |this, cx| {
        let receiver = this
            .flash
            .targets
            .iter()
            .find(|target| target.id == RECEIVER_ID)
            .map(ota_target_key)
            .unwrap();
        assert!(
            this.flash
                .sync_tracker_selection(&BTreeSet::new(), std::iter::empty())
        );
        this.flash.set_receiver_update_selected(&receiver, false);
        cx.notify();
    });
    assert_settled(cx, OtaStep::Devices, 2);

    notifications.set(0);
    cx.update(|window, cx| {
        workbench.update(cx, |this, cx| {
            this.flash = confirmed_ota_state();
            assert!(this.set_feature_page(FeaturePage::Settings, window, cx));
            cx.notify();
        });
    });
    assert_settled(cx, OtaStep::Mapping, 2);

    for step in [OtaStep::Firmware, OtaStep::Devices] {
        notifications.set(0);
        workbench.update(cx, |this, cx| {
            this.flash = confirmed_ota_state();
            this.flash.ota_step = step;
            cx.notify();
        });
        cx.run_until_parked();
        workbench.read_with(cx, |this, _| {
            assert_eq!(this.flash.ota_step, step);
        });
        assert_eq!(
            notifications.get(),
            1,
            "valid prerequisites never auto-advance"
        );
    }

    notifications.set(0);
    workbench.update(cx, |this, cx| {
        this.flash = confirmed_ota_state();
        this.flash.review = None;
        this.shutting_down = true;
        cx.notify();
    });
    cx.run_until_parked();
    workbench.read_with(cx, |this, _| {
        assert_eq!(this.flash.ota_step, OtaStep::Confirm);
    });
    assert_eq!(
        notifications.get(),
        1,
        "shutdown stops workflow reconciliation"
    );
}

#[test]
fn wireless_offline_review_retreat_retains_images_but_replacement_revokes_them() {
    let mut state = ota_workflow_state();
    for target in &state.targets {
        state
            .assignments
            .insert(ota_assignment_key(target), local_image("application.hex"));
    }
    let tracker_group = ota_assignment_key(&state.targets[0]);
    state.active_group = Some(tracker_group.clone());
    let generations = state.group_generations.clone();
    let selected = state.tracker_selection.clone();
    let mut link = TrackerLink {
        receiver_key: "a".into(),
        slot: 0,
        address: state.targets[0].address.clone(),
        online: false,
    };
    state.review = Some(Action::EsbGroups(plan_ota_groups(&state).unwrap()));
    state.ota_step = OtaStep::Confirm;
    state.acknowledged = true;
    state.sync_tracker_selection(&selected, std::iter::once(&link));
    state.reconcile_ota_step(Mode::Esb);
    assert_eq!(state.ota_step, OtaStep::Mapping);
    assert_eq!(state.assignments.len(), 2);
    assert_eq!(state.group_generations, generations);
    assert_eq!(state.active_group.as_ref(), Some(&tracker_group));
    assert_eq!(state.selected_targets.len(), 2);
    assert!(!state.acknowledged);
    link.address = Some("AABBCCDDEEFF".into());
    link.online = true;
    state.sync_tracker_selection(&selected, std::iter::once(&link));
    state.reconcile_ota_step(Mode::Esb);
    assert_eq!(state.ota_step, OtaStep::Devices);
    assert!(!state.assignments.contains_key(&tracker_group));
    assert!(state.unresolved_tracker_selection());
}

#[test]
fn wireless_step_reconciliation_never_changes_other_update_modes() {
    for mode in [Mode::Uf2, Mode::OpenDfu, Mode::Ble] {
        let mut state = FlashState {
            mode,
            ota_step: OtaStep::Confirm,
            review: Some(Action::Reboot(vec![])),
            acknowledged: true,
            ..FlashState::default()
        };
        state.reconcile_ota_step(mode);
        assert_eq!(state.ota_step, OtaStep::Confirm);
        assert!(matches!(state.review, Some(Action::Reboot(_))));
        assert!(state.acknowledged);
    }
}

#[test]
fn wireless_card_metadata_matches_normalized_rf_and_receiver_identity() {
    let mut device = target("a", 0, "board");
    device.address = Some("AA:BB:CC:DD:EE:FF".into());
    let receiver = device.receiver.clone();
    let mut link = TrackerLink {
        receiver_key: "a".into(),
        slot: 0,
        address: Some("aabbccddeeff".into()),
        online: true,
    };
    let mut state = FlashState::default();
    state.targets.push(device);
    assert!(state.tracker_info(&link, &receiver).is_some());
    link.address = Some("112233445566".into());
    assert!(state.tracker_info(&link, &receiver).is_none());
    link.address = None;
    assert!(state.tracker_info(&link, &receiver).is_none());
    link.address = Some("aabbccddeeff".into());
    link.receiver_key = "b".into();
    assert!(state.tracker_info(&link, &receiver).is_none());
    link.receiver_key = "a".into();
    let mut replacement = receiver;
    replacement.path = b"replacement".to_vec();
    assert!(state.tracker_info(&link, &replacement).is_none());
}

#[test]
fn wireless_sections_use_reports_not_release_age_and_neutralize_query_handoff() {
    let mut device = target("a", 0, "board");
    device.info.version = "0.1.0".into();
    device.info.build_date = "2020-01-01 00:00:00".into();
    let mut link = TrackerLink {
        receiver_key: "a".into(),
        slot: 0,
        address: device.address.clone(),
        online: true,
    };
    assert_eq!(
        tracker_section(&link, Some(&device), false, false),
        TrackerSection::Reported("0.1.0", "2020-01-01 00:00:00")
    );
    let mut different_build = device.clone();
    different_build.info.build_date = "2026-10-12 00:00:00".into();
    assert_ne!(
        tracker_section(&link, Some(&device), false, false),
        tracker_section(&link, Some(&different_build), false, false)
    );
    assert_eq!(
        tracker_section(&link, None, false, false),
        TrackerSection::Unknown
    );
    assert_eq!(
        tracker_section(&link, None, true, false),
        TrackerSection::Reading
    );
    assert_eq!(
        tracker_section(&link, Some(&device), false, true),
        TrackerSection::Error
    );
    device.info.protocol_version = 2;
    assert_eq!(
        tracker_section(&link, Some(&device), false, false),
        TrackerSection::Unsupported
    );
    link.online = false;
    assert_eq!(
        tracker_section(&link, Some(&device), false, true),
        TrackerSection::Offline
    );
    assert_eq!(
        tracker_section(&link, Some(&device), true, false),
        TrackerSection::Reading
    );
}

#[test]
fn confirmation_keeps_receivers_and_same_named_firmware_assignments_distinct() {
    let mut first_image = local_image("firmware.bin");
    first_image.digest = "first-image".into();
    let mut second_image = local_image("firmware.bin");
    second_image.digest = "second-image".into();
    let groups = vec![
        OtaPlanGroup {
            targets: vec![target("receiver-a", 0, "board-a")],
            image: first_image.clone(),
        },
        OtaPlanGroup {
            targets: vec![target("receiver-b", 0, "board-a")],
            image: first_image,
        },
        OtaPlanGroup {
            targets: vec![target("receiver-a", 1, "board-b")],
            image: second_image.clone(),
        },
        OtaPlanGroup {
            targets: vec![target("receiver-a", RECEIVER_ID, "receiver-board")],
            image: second_image,
        },
    ];
    let receivers = ota_confirmation_receivers(&groups);
    assert_eq!(receivers.len(), 2);
    let first = &receivers["receiver-a"];
    assert_eq!(first.len(), 3);
    assert!(std::ptr::eq(first[0].1, &groups[0]));
    assert!(std::ptr::eq(first[1].1, &groups[2]));
    assert!(std::ptr::eq(first[2].1, &groups[3]));
    assert_eq!(first[2].1.targets[0].id, RECEIVER_ID);
    let second = &receivers["receiver-b"];
    assert_eq!(second.len(), 1);
    assert!(std::ptr::eq(second[0].1, &groups[1]));
    assert_eq!(second[0].1.targets[0].id, first[0].1.targets[0].id);
    assert_ne!(first[0].1.image.digest, first[1].1.image.digest);
}

struct QueryCardHarness {
    workbench: gpui_kit::Entity<crate::application::Workbench>,
}

impl gpui_kit::Render for QueryCardHarness {
    fn render(
        &mut self,
        _: &mut gpui_kit::Window,
        cx: &mut gpui_kit::Context<Self>,
    ) -> impl gpui_kit::IntoElement {
        use gpui_kit::prelude::*;

        self.workbench.update(cx, |this, cx| {
            gpui_kit::div().size_full().child(this.flash_page(cx))
        })
    }
}

#[gpui_kit::test]
fn query_diagnostics_remain_on_cards_while_flash_failures_remain_in_results(
    cx: &mut gpui_kit::TestAppContext,
) {
    use crate::app_runtime::AppRuntime;
    use crate::application::flash::{ActivityKind, OtaPresentation, ResultTarget, query_failure};
    use crate::application::{FeaturePage, Workbench};
    use crate::discovery::ReceiverMonitor;
    use crate::update::UpdateOutcome;
    use crate::{application::test_support::offline_workbench, test_support::UiTestSession};
    use gpui_kit::{
        AppContext as _, Bounds, WindowBounds, WindowOptions, px, size, test::TestWindowExt as _,
    };
    use std::time::Instant;

    let runtime = AppRuntime::new().expect("create isolated I/O runtime");
    let preferences = tempfile::tempdir().expect("create isolated preference directory");
    let _ui_session = UiTestSession::new(cx);
    let first = target("a", 0, "board");
    let second = target("a", 1, "board");
    let receiver = first.receiver.clone();
    let first_fixture = first.clone();
    let second_fixture = second.clone();
    let receiver_fixture = receiver.clone();
    let executor = runtime.executor();
    let preferences_path = preferences.path().join("preferences.json");
    let (window, harness) = cx.update(|cx| {
        gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    Default::default(),
                    size(px(1280.), px(1000.)),
                ))),
                ..Default::default()
            },
            cx,
            move |window, cx| {
                let workbench = cx.new(|cx| {
                    let mut this: Workbench = offline_workbench(
                        window,
                        cx,
                        crate::sessions::SessionManager::new(),
                        executor,
                    );
                    this.preferences_io =
                        crate::application::preference_io::PreferenceIo::for_test(preferences_path);
                    this.active_page = FeaturePage::Ota;
                    this.flash.mode = Mode::Esb;
                    this.inventory.receivers.push(receiver_fixture.clone());
                    this.monitor = ReceiverMonitor::fixture(
                        receiver_fixture.clone(),
                        first_fixture.id,
                        first_fixture.address.as_deref().unwrap(),
                        true,
                    );
                    this.monitor
                        .fixture_observation_mut(&receiver_fixture)
                        .trackers
                        .push(TrackerLink {
                            receiver_key: receiver_fixture.key.clone(),
                            slot: second_fixture.id,
                            address: second_fixture.address.clone(),
                            online: true,
                        });
                    this.flash.targets.push(first_fixture);
                    this.flash.apply_queries(OtaQueries {
                        targets: vec![],
                        failures: vec![
                            query_failure(
                                &receiver_fixture,
                                RECEIVER_ID,
                                "receiver timeout".into(),
                            ),
                            query_failure(&receiver_fixture, 0, "tracker timeout".into()),
                            query_failure(&receiver_fixture, 1, "other tracker timeout".into()),
                        ],
                    });
                    this
                });
                cx.new(|_| QueryCardHarness { workbench })
            },
        )
        .expect("open the production wireless firmware page")
    });
    let workbench = cx.update(|cx| harness.read(cx).workbench.clone());
    cx.update_window(window, |_, window, _| window.activate_window())
        .expect("activate the wireless fixture");
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("flash-results-panel").is_none());
        assert!(window.try_find("flash-results-dismiss").is_none());
        assert!(window.try_find("firmware-activity-message").is_none());
        let receiver_card = window.find("ota-receiver-card-a").bounds();
        let receiver_error = window.find("ota-query-error-a-254");
        assert_eq!(receiver_error.label(), Some("receiver timeout"));
        assert!(receiver_error.bounds().top() >= receiver_card.top());
        assert!(receiver_error.bounds().bottom() <= receiver_card.bottom());
        let receiver_retry = window.find("ota-query-receiver-a");
        assert!(receiver_retry.visible());
        assert!(receiver_retry.bounds().top() >= receiver_card.top());
        assert!(receiver_retry.bounds().bottom() <= receiver_card.bottom());
        let tracker_card = window.find("ota-card-tracker:a:0").bounds();
        let tracker_error = window.find("ota-query-error-a-0");
        assert_eq!(tracker_error.label(), Some("tracker timeout"));
        assert!(tracker_error.bounds().top() >= tracker_card.top());
        assert!(tracker_error.bounds().bottom() <= tracker_card.bottom());
        let retry = window.find("ota-query-tracker-a-0");
        assert!(retry.visible());
        assert_eq!(
            retry.label(),
            Some(crate::i18n::tr("firmware.query.retry_tracker").as_str())
        );
        assert!(retry.bounds().top() >= tracker_card.top());
        assert!(retry.bounds().bottom() <= tracker_card.bottom());
        assert_eq!(
            window.find("ota-query-error-a-1").label(),
            Some("other tracker timeout")
        );
        window.click("ota-query-tracker-a-0", cx);
    })
    .expect("inspect and retry the diagnostic on the actual tracker card");

    workbench.read_with(cx, |this, _| {
        let Some(Action::QueryGroups(plan)) = this.flash.pending.as_ref() else {
            panic!("the card retry must reserve a scoped query");
        };
        assert!(!plan.automatic && plan.incremental);
        assert_eq!(plan.groups.len(), 1);
        assert_eq!(plan.groups[0].0.key, receiver.key);
        assert_eq!(plan.groups[0].1.len(), 1);
        assert_eq!(plan.groups[0].1[0].slot, first.id);
        assert!(!plan.groups[0].2);
        assert!(this.selected.is_empty(), "retry must not select the card");
        assert_eq!(this.flash.targets[0].info.version, "1");
        assert_eq!(this.flash.query_failures.len(), 3);
    });
    harness.update(cx, |_, cx| cx.notify());
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("flash-results-panel").is_none());
        assert!(window.find("firmware-activity-message").visible());
        assert_eq!(
            window.find("ota-query-error-a-0").label(),
            Some("tracker timeout")
        );
    })
    .expect("the failed card remains stable through retry handoff");

    let mut refreshed = first.clone();
    refreshed.info.version = "refreshed".into();
    workbench.update(cx, |this, cx| {
        assert!(matches!(
            this.flash.pending.take(),
            Some(Action::QueryGroups(_))
        ));
        assert!(this.flash.activity_kind == ActivityKind::Query);
        this.release_flash_lease();
        this.flash.finish_job_progress(None);
        this.flash.apply_queries(OtaQueries {
            targets: vec![refreshed],
            failures: vec![],
        });
        cx.notify();
    });
    harness.update(cx, |_, cx| cx.notify());
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("flash-results-panel").is_none());
        assert!(window.try_find("firmware-activity-message").is_none());
        assert!(window.try_find("ota-query-error-a-0").is_none());
        assert!(window.try_find("ota-query-tracker-a-0").is_none());
        assert_eq!(
            window.find("ota-query-error-a-1").label(),
            Some("other tracker timeout")
        );
        assert_eq!(
            window.find("ota-query-error-a-254").label(),
            Some("receiver timeout")
        );
    })
    .expect("inspect only the successful card clearing its query error");
    workbench.read_with(cx, |this, _| {
        assert_eq!(this.flash.targets[0].info.version, "refreshed");
        assert_eq!(this.flash.query_failures.len(), 2);
    });

    workbench.update(cx, |this, cx| {
        this.flash.ota_presentation = OtaPresentation::Finished;
        cx.notify();
    });
    harness.update(cx, |_, cx| cx.notify());
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("ota-query-error-a-1").is_none());
        assert!(window.try_find("flash-results-panel").is_none());
        assert!(window.try_find("firmware-activity-message").is_none());
    })
    .expect("an unavailable card does not promote its query diagnostic");

    workbench.update(cx, |this, cx| {
        this.flash.ota_presentation = OtaPresentation::Configuring;
        this.flash.operation_origin = Mode::Esb;
        this.flash.apply_joined_operation_results(
            vec![(
                ResultTarget::ota(&first),
                UpdateOutcome::Failed("synthetic flash failure".into()),
            )],
            Instant::now(),
        );
        cx.notify();
    });
    harness.update(cx, |_, cx| cx.notify());
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        let result_panel = window.find("flash-results-panel").bounds();
        let failure = window.find("ota-failure-a-a:0");
        assert!(failure.label().unwrap().contains("synthetic flash failure"));
        assert!(failure.bounds().top() >= result_panel.top());
        assert!(failure.bounds().bottom() <= result_panel.bottom());
        assert_eq!(
            window.find("ota-query-error-a-1").label(),
            Some("other tracker timeout")
        );
    })
    .expect("inspect actual update failure and independent card diagnostic");
}
