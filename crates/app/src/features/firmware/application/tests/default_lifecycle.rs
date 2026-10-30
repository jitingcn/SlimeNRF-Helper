use super::support::{
    finish_internal_default, internal_default_state, join_internal_default, local_image,
    query_link, target,
};
use crate::application::flash::{Action, Mode, Payload, ota_target_key, plan_ota_groups};
use crate::domain::TrackerLink;
use slimenrf_firmware_image::{image::Uf2ImagePurpose, secure_dfu};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[test]
fn bundled_default_keeps_selection_navigation_and_previous_outcome_usable() {
    for previous_kind in [
        crate::application::flash::ActivityKind::Operation,
        crate::application::flash::ActivityKind::Query,
    ] {
        let (_directory, mut state) = internal_default_state();
        state.activity_kind = previous_kind;
        state.activity_origin = Mode::Uf2;
        state.operation_origin = Mode::Uf2;
        state.last_progress = Some(crate::update::Progress::bytes("completed copy", 10, 10));
        state.results = vec![(
            crate::application::flash::ResultTarget::generic("disk".into(), "Disk".into()),
            crate::update::UpdateOutcome::Completed,
        )];
        state.result_success = "firmware.result.copy_completed";
        state
            .result_notes
            .insert("disk".into(), "retained note".into());
        assert!(!state.busy());

        assert!(state.start_internal_default());
        assert!(
            !state.busy(),
            "selection and explicit source controls stay enabled"
        );
        assert!(!state.floating_activity(Mode::Esb));
        assert!(!state.floating_activity(Mode::Uf2));
        assert!(!state.running_operation(Mode::Esb));
        assert!(!state.shows_activity(Mode::Esb));
        assert!(state.shows_results(Mode::Uf2));
        assert!(state.ota_selection_ready().is_ok());
        state.ota_advance().unwrap();
        assert_eq!(state.ota_step, crate::application::flash::OtaStep::Firmware);
        assert!(
            state.ota_images_ready().is_err(),
            "real image is still required"
        );

        join_internal_default(&mut state);
        assert!(state.ota_images_ready().is_ok());
        state.ota_advance().unwrap();
        assert_eq!(state.ota_step, crate::application::flash::OtaStep::Mapping);
        assert_eq!(state.displayed_image(Mode::Esb).unwrap().name, "board.hex");
        assert!(state.take_internal_default().is_none());
        assert!(!state.floating_activity(Mode::Esb));
        assert_eq!(
            state.last_progress.as_ref().unwrap().message,
            "completed copy"
        );
        assert_eq!(state.results[0].0.key, "disk");
        assert_eq!(state.results[0].1, crate::update::UpdateOutcome::Completed);
        assert_eq!(state.result_success, "firmware.result.copy_completed");
        assert_eq!(state.result_notes["disk"], "retained note");
    }
}

#[test]
fn background_default_preserves_other_page_review_and_auto_copy_intent() {
    for mode in [Mode::Uf2, Mode::OpenDfu, Mode::Ble] {
        for fail in [false, true] {
            let (directory, mut state) = internal_default_state();
            if fail {
                std::fs::remove_file(directory.path().join("board.hex")).unwrap();
            }
            assert!(state.start_internal_default());
            state.transition_mode(mode);
            let image = local_image("chosen.hex");
            state.image = Some(image.clone());
            state.review = Some(match mode {
                Mode::Uf2 => {
                    std::fs::write(
                        directory.path().join("INFO_UF2.TXT"),
                        "Board-ID: nRF52840-test\n",
                    )
                    .unwrap();
                    let volume = crate::features::firmware::uf2::inspect_volume(
                        directory.path(),
                        "INFO_UF2.TXT",
                    )
                    .unwrap();
                    Action::Uf2(vec![volume], image.clone(), Uf2ImagePurpose::Application)
                }
                Mode::OpenDfu => Action::OpenDfu(
                    crate::domain::SerialEndpoint {
                        port_name: "default-review-test".into(),
                        label: "Open DFU Bootloader".into(),
                        manufacturer: None,
                        product: None,
                        kind: crate::domain::SerialPortKind::Unknown,
                        serial_number: None,
                        vid: None,
                        pid: None,
                        usb_interface: None,
                    },
                    image.clone(),
                    secure_dfu::Options::default(),
                ),
                Mode::Ble => Action::Ble(vec![], image.clone()),
                Mode::Esb => unreachable!(),
            });
            state.acknowledged = true;
            state.auto = mode == Mode::Uf2;
            state.auto_requested = mode == Mode::Uf2;
            state.auto_seen.insert("retained-copy".into());

            join_internal_default(&mut state);

            assert!(state.review.as_ref().unwrap().mode() == mode);
            assert!(state.acknowledged);
            assert_eq!(state.auto, mode == Mode::Uf2);
            assert_eq!(state.auto_requested, mode == Mode::Uf2);
            assert!(state.auto_seen.contains("retained-copy"));
            assert!(Arc::ptr_eq(
                &state.image.as_ref().unwrap().bytes,
                &image.bytes
            ));
            if fail {
                assert!(state.assignments.is_empty());
                assert!(
                    state
                        .ota_defaults
                        .values()
                        .all(|entry| entry.error.is_some())
                );
            } else {
                assert_eq!(state.displayed_image(Mode::Esb).unwrap().name, "board.hex");
            }
        }
    }
}

#[test]
fn background_default_invalidates_only_successfully_changed_ota_review() {
    for fail in [false, true] {
        let (directory, mut state) = internal_default_state();
        // Freeze a plan, then model a selected group awaiting its default.
        // A successful assignment must revoke that stale OTA confirmation.
        let destination = state.destination(Mode::Esb).unwrap();
        state
            .assign_image(&destination, local_image("old.hex"))
            .unwrap();
        let groups = plan_ota_groups(&state).unwrap();
        state.assignments.clear();
        if fail {
            std::fs::remove_file(directory.path().join("board.hex")).unwrap();
        }
        assert!(state.start_internal_default());
        state.review = Some(Action::EsbGroups(groups));
        state.acknowledged = true;

        join_internal_default(&mut state);

        assert_eq!(state.review.is_some(), fail);
        assert_eq!(state.acknowledged, fail);
        assert_eq!(state.assignments.is_empty(), fail);
    }
}

#[test]
fn bundled_defaults_load_one_selected_group_at_a_time() {
    let (_directory, mut state) = internal_default_state();
    let other = target("a", 1, "other");
    state.selected_targets.insert(ota_target_key(&other));
    state.tracker_selection.insert("tracker:a:1".into());
    state.targets.push(other);
    state.reconcile_groups(None);

    assert!(state.start_internal_default());
    for _ in 0..3 {
        assert!(!state.start_internal_default());
        assert!(!state.busy());
    }
    assert_eq!(
        state
            .ota_defaults
            .values()
            .filter(|entry| entry.attempted)
            .count(),
        1
    );
    join_internal_default(&mut state);
    assert_eq!(state.assignments.len(), 1);
    assert!(state.ota_images_ready().is_err());
    finish_internal_default(&mut state);
    assert_eq!(state.assignments.len(), 2);
    assert!(state.ota_images_ready().is_ok());
    assert!(!state.start_internal_default());
}

#[test]
fn background_default_rejects_deselected_and_recreated_destinations() {
    for reselect in [false, true] {
        let (_directory, mut state) = internal_default_state();
        let destination = state.destination(Mode::Esb).unwrap();
        let selection = state.tracker_selection.clone();
        let live = [TrackerLink {
            address: state.targets[0].address.clone(),
            ..query_link("a", 0)
        }];
        assert!(state.start_internal_default());
        assert!(!state.busy());
        state.sync_tracker_selection(&Default::default(), live.iter());
        assert!(state.ota_selection_ready().is_err());
        if reselect {
            state.sync_tracker_selection(&selection, live.iter());
            assert!(state.ota_selection_ready().is_ok());
            assert!(state.destination(Mode::Esb).as_ref() != Some(&destination));
            assert!(
                !state.start_internal_default(),
                "old worker still owns its slot"
            );
        }
        join_internal_default(&mut state);
        assert!(state.assignments.is_empty());
        assert!(
            state
                .ota_defaults
                .values()
                .all(|entry| entry.error.is_none())
        );
        if reselect {
            finish_internal_default(&mut state);
            assert_eq!(state.displayed_image(Mode::Esb).unwrap().name, "board.hex");
        } else {
            assert!(!state.start_internal_default());
        }
    }
}

#[test]
fn background_default_coexists_with_foreground_worker_and_reservation() {
    use std::sync::atomic::{AtomicBool, Ordering};
    for foreground_first in [false, true] {
        for kind in [
            crate::application::flash::ActivityKind::Query,
            crate::application::flash::ActivityKind::Operation,
        ] {
            let (_directory, mut state) = internal_default_state();
            let mut manager = crate::sessions::SessionManager::new();
            let token = manager.reserve("default-load-test", None).unwrap();
            state.leases.push(crate::application::flash::Lease::Serial {
                token,
                port: "default-load-test".into(),
            });
            state
                .operation_pins
                .ports
                .insert("default-load-test".into());
            state
                .operation_pins
                .addresses
                .insert("reserved receiver".into());
            if !foreground_first {
                assert!(state.start_internal_default());
                assert!(!state.busy());
            }
            let ended = Arc::new(AtomicBool::new(false));
            let worker_ended = ended.clone();
            state.job = Some(
                crate::update::Job::start(move |cancel, _| {
                    while !cancel.load(Ordering::Acquire) {
                        std::thread::yield_now();
                    }
                    worker_ended.store(true, Ordering::Release);
                    Ok(Payload::Update(
                        crate::features::firmware::executor::UpdateCompletion::Results(vec![]),
                    ))
                })
                .unwrap(),
            );
            state.activity_kind = kind;
            state.activity_origin = Mode::Ble;
            state.ble_activity = true;
            state.set_error(Mode::Uf2, "retained foreground error".into());
            if foreground_first {
                assert!(state.start_internal_default());
            }
            join_internal_default(&mut state);
            assert_eq!(state.displayed_image(Mode::Esb).unwrap().name, "board.hex");
            assert!(state.busy(), "real foreground work must remain exclusive");
            assert!(state.advanced_features_locked());
            assert!(state.activity_kind == kind);
            assert!(state.activity_origin == Mode::Ble);
            assert_eq!(state.error.as_deref(), Some("retained foreground error"));
            assert!(!ended.load(Ordering::Acquire));
            assert_eq!(state.leases.len(), 1);
            let crate::application::flash::Lease::Serial { token, .. } = &state.leases[0] else {
                unreachable!()
            };
            assert!(manager.reservation_ready(token).unwrap());
            assert!(manager.reserve("default-load-test", None).is_err());
            assert!(state.operation_pins.ports.contains("default-load-test"));
            assert!(state.operation_pins.addresses.contains("reserved receiver"));
            state.cancel_activity();
            assert!(state.job.is_some(), "cancel retains the foreground worker");
            state.job.take().unwrap().shutdown();
            assert!(ended.load(Ordering::Acquire));
            let crate::application::flash::Lease::Serial { token, .. } = &state.leases[0] else {
                unreachable!()
            };
            manager.release(token).unwrap();
        }
    }
}

#[test]
fn default_worker_stays_quiet_until_cancelled_and_joined_on_shutdown() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let (_directory, mut state) = internal_default_state();
    let (destination, _) = state.take_internal_default().unwrap();
    let ended = Arc::new(AtomicBool::new(false));
    let worker_ended = ended.clone();
    state.default_job = Some((
        destination,
        crate::update::Job::start(move |cancel, _| {
            while !cancel.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
            worker_ended.store(true, Ordering::Release);
            Err("cancelled default".into())
        })
        .unwrap(),
    ));
    for _ in 0..3 {
        assert!(
            !state.poll_internal_default(),
            "unfinished defaults must not repaint"
        );
    }
    state.cancel_activity();
    assert!(
        !ended.load(Ordering::Acquire),
        "foreground cancellation is independent"
    );
    assert!(!state.poll_internal_default());
    assert!(!state.busy());
    let (jobs, default_job) = state.stop_for_shutdown();
    assert!(jobs.into_iter().all(|job| job.is_none()));
    assert!(state.default_job.is_none());
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ended.load(Ordering::Acquire) {
        assert!(
            Instant::now() < deadline,
            "shutdown must request cancellation first"
        );
        std::thread::yield_now();
    }
    default_job.unwrap().shutdown();
    assert!(
        ended.load(Ordering::Acquire),
        "shutdown must join the owned default"
    );
    assert!(state.assignments.is_empty());
}
