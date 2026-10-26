use super::support::{confirmed_ota_state, local_image, ota_workflow_state, target};
use crate::domain::TrackerLink;
use crate::esb_ota::RECEIVER_ID;
use crate::ui::flash::confirmation::ota_confirmation_receivers;
use crate::ui::flash::{
    Action, FlashState, Mode, OtaPlanGroup, OtaQueries, OtaQueryPlan, OtaStep, TrackerSection,
    ota_assignment_key, plan_ota_groups, tracker_section,
};
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
        crate::ui::flash::OtaPresentation::Running
    );
    assert!(!state.ota_shows_next());
    state.ota_new_update();
    assert_eq!(
        state.ota_presentation,
        crate::ui::flash::OtaPresentation::Running
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
    state.activity_kind = crate::ui::flash::ActivityKind::Operation;
    state.prepare_action_results(&action);
    state.finish_job_progress(None);
    state.apply_joined_operation_results(
        vec![(
            crate::ui::flash::ResultTarget::ota(&state.targets[0]),
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
        crate::ui::flash::OtaPresentation::Finished
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
            crate::ui::flash::ResultTarget::ota(&state.targets[0]),
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
    state.activity_kind = crate::ui::flash::ActivityKind::Operation;
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
        crate::ui::flash::OtaPresentation::Running
    );
    assert!(!state.ota_shows_next());
    state.ota_new_update();
    assert_eq!(
        state.ota_presentation,
        crate::ui::flash::OtaPresentation::Running
    );
    release.send(()).unwrap();
    state.job.take().unwrap().shutdown();
    state.finish_job_progress(None);
    assert_eq!(
        state.ota_presentation,
        crate::ui::flash::OtaPresentation::Finished
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
        state.activity_kind = crate::ui::flash::ActivityKind::Operation;
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
            crate::ui::flash::OtaPresentation::Finished
        );
        assert!(!state.ota_shows_next());
        state.ota_new_update();
        assert!(state.ota_shows_next());
    }
    let mut state = confirmed_ota_state();
    state.take_confirmed_action().unwrap();
    state.activity_origin = Mode::Esb;
    state.activity_kind = crate::ui::flash::ActivityKind::Operation;
    state.finish_job_progress(None);
    state.set_activity_error("worker failed".into());
    state.reconcile_ota_step(Mode::Esb);
    assert_eq!(
        state.ota_presentation,
        crate::ui::flash::OtaPresentation::Finished
    );
    assert!(!state.ota_shows_next());
    assert_eq!(state.error.as_deref(), Some("worker failed"));
}

#[test]
fn ota_dismissal_and_finished_navigation_survive_queries_and_other_modes() {
    let mut state = confirmed_ota_state();
    let action = state.take_confirmed_action().unwrap();
    state.activity_origin = Mode::Esb;
    state.activity_kind = crate::ui::flash::ActivityKind::Operation;
    state.prepare_action_results(&action);
    state.finish_job_progress(None);
    state.operation_origin = Mode::Uf2;
    state.apply_joined_operation_results(
        vec![(
            crate::ui::flash::ResultTarget::generic("volume".into(), "Volume".into()),
            crate::update::UpdateOutcome::Completed,
        )],
        std::time::Instant::now(),
    );
    let device = state.targets[0].clone();
    let refreshed_key = format!("{}:{}", device.receiver.key, device.id);
    state.query_failures.insert(
        refreshed_key.clone(),
        ("Tracker".into(), Err("timeout".into())),
    );
    // The refreshed target's failure must resolve on success. Keep a different
    // receiver's failure to exercise dismissed-result visibility independently.
    state.query_failures.insert(
        "b:0".into(),
        ("Other tracker".into(), Err("unrelated timeout".into())),
    );
    state.dismiss_results(Mode::Esb);
    assert!(state.shows_operation_results(Mode::Uf2));
    assert!(!state.shows_query_results(Mode::Esb));
    assert_eq!(state.query_failures.len(), 2);
    for mode in [Mode::Uf2, Mode::Ble, Mode::OpenDfu, Mode::Esb] {
        state.transition_mode(mode);
        state.reconcile_ota_step(mode);
        assert_eq!(
            state.ota_presentation,
            crate::ui::flash::OtaPresentation::Finished
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
        assert!(!state.shows_query_results(Mode::Esb));
        state.activity_kind = crate::ui::flash::ActivityKind::Query;
        state.activity_origin = Mode::Esb;
        state.finish_job_progress(None);
        state.apply_queries(OtaQueries {
            targets: vec![device.clone()],
            failures: vec![],
        });
        assert!(!state.query_failures.contains_key(&refreshed_key));
        assert_eq!(
            state.query_failures["b:0"].1.as_ref().unwrap_err(),
            "unrelated timeout"
        );
        assert_eq!(state.shows_query_results(Mode::Esb), !automatic);
        state.reconcile_ota_step(Mode::Esb);
        assert_eq!(
            state.ota_presentation,
            crate::ui::flash::OtaPresentation::Finished
        );
        assert!(!state.ota_shows_next());
        assert!(state.shows_operation_results(Mode::Uf2));
    }
    state.dismiss_results(Mode::Uf2);
    assert!(state.shows_query_results(Mode::Esb));
    assert!(!state.shows_operation_results(Mode::Uf2));
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
    state.reconcile_ota_step(Mode::Esb);
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
    state.reconcile_ota_step(Mode::Esb);
    assert_eq!(state.ota_step, OtaStep::Mapping);
    assert!(!state.acknowledged);
    state.assignments.clear();
    state.reconcile_ota_step(Mode::Esb);
    assert_eq!(state.ota_step, OtaStep::Firmware);
    state.selected_targets.clear();
    state.reconcile_ota_step(Mode::Esb);
    assert_eq!(state.ota_step, OtaStep::Devices);
    assert!(state.review.is_none());
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
