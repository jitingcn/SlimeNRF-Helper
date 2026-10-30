use super::support::{
    finish_internal_default, internal_default_state, join_internal_default, local_image, target,
};
use crate::application::flash::{
    Action, ImageDestination, Mode, OtaQueries, ota_target_key, plan_ota_groups,
    validate_live_ota_targets,
};
use crate::domain::TrackerLink;
use crate::features::firmware::bundled_resources::Resource;
use std::sync::Arc;

#[test]
fn available_additional_version_is_never_promoted_to_missing_exact_default() {
    let (directory, mut state) = internal_default_state();
    let resource = Resource::File(directory.path().join("board.hex"));
    let target = &mut state
        .catalog
        .as_mut()
        .unwrap()
        .legacy_targets
        .get_mut("board")
        .unwrap()
        .targets[0];
    target.firmware = None;
    target.firmware_aliases = vec!["board.hex".into()];
    target.additional_firmware = vec![resource.clone()];
    let key = state.active_group.clone().unwrap();
    assert_eq!(
        state
            .internal_image_target(&key)
            .unwrap()
            .additional_firmware
            .len(),
        1
    );
    assert!(state.internal_image_resource(&key).is_err());
    finish_internal_default(&mut state);
    assert!(state.assignments.is_empty());
    let destination = state.destination(Mode::Esb).unwrap();
    state.explicit_image_intent(&destination);
    let image = crate::application::flash::load_image(
        resource.clone(),
        &std::sync::atomic::AtomicBool::new(false),
    )
    .unwrap();
    state.apply_images(destination, vec![image]);
    assert_eq!(
        state
            .displayed_image(Mode::Esb)
            .unwrap()
            .resource
            .as_deref(),
        Some(&resource)
    );
    assert!(state.take_internal_default().is_none());
}

#[test]
fn configured_versions_preserve_exact_default_and_explicit_resource_identity() {
    for pending_default in [false, true] {
        let (directory, mut state) = internal_default_state();
        let config = directory.path().join("slimenrf-helper.yaml");
        std::fs::write(
            &config,
            br#"{"schema_version":1,"firmware_root":".","legacy_targets":{"board":{"targets":[
                {"name":"board","firmware":"board.hex","firmware_aliases":["board.hex"],"esb_ota_kind":"tracker"}
            ]}}}"#,
        )
        .unwrap();
        for name in ["board-1.0.0.hex", "board-2.0.0.hex"] {
            std::fs::write(
                directory.path().join(name),
                b":0410000001020304E2\n:00000001FF\n",
            )
            .unwrap();
        }
        state.catalog =
            Some(crate::features::firmware::board_catalog::Catalog::load(&config).unwrap());
        let key = state.active_group.clone().unwrap();
        let configured = state.internal_image_target(&key).unwrap();
        assert_eq!(configured.additional_firmware.len(), 2);
        let selected = configured.additional_firmware[0].clone();
        assert_eq!(selected.filename(), Some("board-1.0.0.hex"));
        assert_eq!(
            state.internal_image_resource(&key).unwrap().filename(),
            Some("board.hex")
        );
        let destination = state.destination(Mode::Esb).unwrap();
        assert!(state.start_internal_default());
        if !pending_default {
            join_internal_default(&mut state);
            assert_eq!(state.displayed_image(Mode::Esb).unwrap().name, "board.hex");
            state.review = Some(Action::EsbGroups(plan_ota_groups(&state).unwrap()));
            state.acknowledged = true;
            state.auto = true;
            state.auto_requested = true;
        }
        state.explicit_image_intent(&destination);
        let image = crate::application::flash::load_image(
            selected.clone(),
            &std::sync::atomic::AtomicBool::new(false),
        )
        .unwrap();
        state.apply_images(destination.clone(), vec![image]);
        if pending_default {
            join_internal_default(&mut state);
        }
        state.apply_internal_default(destination, Ok(local_image("late.hex")));
        let chosen = state.displayed_image(Mode::Esb).unwrap();
        assert_eq!(chosen.name, "board-1.0.0.hex");
        assert_eq!(chosen.resource.as_deref(), Some(&selected));
        assert_eq!(chosen.hints.len(), 1);
        assert_eq!(chosen.hints[0].board_target, "board");
        assert_eq!(
            chosen.hints[0].kind,
            Some(crate::features::firmware::board_catalog::EsbOtaKind::Tracker)
        );
        assert!(!chosen.hints[0].bootloader);
        assert_eq!(plan_ota_groups(&state).unwrap()[0].image.name, chosen.name);
        assert!(state.review.is_none());
        assert!(!state.acknowledged);
        assert!(!state.auto);
        assert!(!state.auto_requested);
        assert!(state.take_internal_default().is_none());
    }
}

#[test]
fn internal_default_assigns_compatible_preset_once_without_affecting_global_image() {
    let (_directory, mut state) = internal_default_state();
    state.image = Some(local_image("global.hex"));
    finish_internal_default(&mut state);
    let image = state.displayed_image(Mode::Esb).unwrap().clone();
    assert_eq!(image.name, "board.hex");
    assert_eq!(image.hints.len(), 1);
    assert_eq!(plan_ota_groups(&state).unwrap()[0].image.name, "board.hex");
    assert_eq!(state.image.as_ref().unwrap().name, "global.hex");
    assert!(state.review.is_none());
    assert!(!state.acknowledged);
    for _ in 0..3 {
        state.apply_queries(OtaQueries {
            targets: state.targets.clone(),
            failures: vec![],
        });
        state.reconcile_groups(None);
        assert!(state.take_internal_default().is_none());
        assert!(Arc::ptr_eq(
            &image.bytes,
            &state.displayed_image(Mode::Esb).unwrap().bytes
        ));
    }
}

#[test]
fn explicit_local_remote_and_preset_intents_win_over_pending_defaults() {
    for (intent, choose_before_completion) in [
        ("local", false),
        ("remote", false),
        ("preset", false),
        ("local", true),
        ("remote", true),
        ("preset", true),
    ] {
        let (_directory, mut state) = internal_default_state();
        let destination = state.destination(Mode::Esb).unwrap();
        assert!(state.start_internal_default());
        assert!(!state.busy());
        if intent == "remote" {
            state.prepare_remote(Mode::Esb, &destination);
        } else {
            // Both the file dialog and configured-image click record intent
            // before their asynchronous file load can complete.
            state.explicit_image_intent(&destination);
        }
        if choose_before_completion {
            state.apply_images(destination.clone(), vec![local_image("chosen.hex")]);
        }
        join_internal_default(&mut state);
        if !choose_before_completion {
            assert!(state.assignments.is_empty());
            state.apply_images(destination.clone(), vec![local_image("chosen.hex")]);
        }
        assert!(!state.start_internal_default());
        state.apply_internal_default(destination, Err("late failure".into()));
        assert_eq!(state.displayed_image(Mode::Esb).unwrap().name, "chosen.hex");
        assert!(
            state
                .ota_defaults
                .values()
                .all(|state| state.error.is_none())
        );
    }
}

#[test]
fn explicit_choice_before_default_scheduling_is_never_replaced() {
    for remote_intent in [false, true] {
        let (_directory, mut state) = internal_default_state();
        let destination = state.destination(Mode::Esb).unwrap();
        if remote_intent {
            state.prepare_remote(Mode::Esb, &destination);
        } else {
            state.apply_images(destination, vec![local_image("chosen.hex")]);
        }
        assert!(state.take_internal_default().is_none());
        state.apply_queries(OtaQueries {
            targets: state.targets.clone(),
            failures: vec![],
        });
        assert!(state.take_internal_default().is_none());
    }
}

#[test]
fn unavailable_internal_defaults_do_not_assign_or_retry_and_allow_manual_choice() {
    for failure in ["missing", "corrupt", "incompatible", "unmapped"] {
        let (directory, mut state) = internal_default_state();
        match failure {
            "missing" => std::fs::remove_file(directory.path().join("board.hex")).unwrap(),
            "corrupt" => std::fs::write(directory.path().join("board.hex"), b"broken").unwrap(),
            "incompatible" => {
                // A configured path exists, but HEX cannot update MCUboot.
                state.targets[0].info.bootloader = "mcuboot".into();
                state
                    .catalog
                    .as_mut()
                    .unwrap()
                    .legacy_targets
                    .get_mut("board")
                    .unwrap()
                    .targets[0]
                    .esb_ota_mcuboot_firmware =
                    Some(Resource::File(directory.path().join("board.hex")));
                state.reconcile_groups(None);
            }
            "unmapped" => {
                let key = ota_target_key(&state.targets[0]);
                state.targets[0].info.board_target = "unconfigured".into();
                state.selected_targets = [key].into();
                state.reconcile_groups(None);
            }
            _ => unreachable!(),
        }
        finish_internal_default(&mut state);
        assert!(state.assignments.is_empty(), "{failure}");
        assert!(
            state
                .ota_defaults
                .values()
                .any(|state| state.error.is_some())
        );
        for _ in 0..3 {
            state.reconcile_groups(None);
            assert!(state.take_internal_default().is_none(), "{failure}");
        }
        if failure == "incompatible" {
            state.targets[0].info.bootloader = "none".into();
            state.reconcile_groups(None);
        }
        let destination = state.destination(Mode::Esb).unwrap();
        state.prepare_remote(Mode::Esb, &destination);
        assert!(
            state
                .ota_defaults
                .values()
                .all(|state| state.error.is_none())
        );
        state.apply_images(destination, vec![local_image("manual.hex")]);
        assert_eq!(state.displayed_image(Mode::Esb).unwrap().name, "manual.hex");
    }
}

#[cfg(not(feature = "embed-firmware"))]
#[test]
fn intentionally_omitted_defaults_do_not_start_workers_or_errors_and_allow_manual_remote_choice() {
    for remote_intent in [false, true] {
        let (_directory, mut state) = internal_default_state();
        state
            .catalog
            .as_mut()
            .unwrap()
            .legacy_targets
            .get_mut("board")
            .unwrap()
            .targets[0]
            .firmware = Some(Resource::bundled("firmwares/board.hex").unwrap());
        let destination = state.destination(Mode::Esb).unwrap();
        for _ in 0..3 {
            assert!(!state.start_internal_default());
            state.reconcile_groups(None);
            assert!(state.default_job.is_none());
            assert!(state.assignments.is_empty());
            assert!(
                state
                    .ota_defaults
                    .values()
                    .all(|state| state.error.is_none())
            );
            assert!(!state.busy());
        }
        if remote_intent {
            state.prepare_remote(Mode::Esb, &destination);
        } else {
            state.explicit_image_intent(&destination);
        }
        state.apply_images(destination, vec![local_image("manual.hex")]);
        assert_eq!(state.displayed_image(Mode::Esb).unwrap().name, "manual.hex");
    }
}

#[cfg(not(feature = "embed-firmware"))]
#[test]
fn omitted_default_does_not_block_an_external_default_in_another_group() {
    let (_directory, mut state) = internal_default_state();
    state
        .catalog
        .as_mut()
        .unwrap()
        .legacy_targets
        .get_mut("board")
        .unwrap()
        .targets[0]
        .firmware = Some(Resource::bundled("firmwares/board.hex").unwrap());
    let other = target("a", 1, "other");
    state.selected_targets.insert(ota_target_key(&other));
    state.targets.push(other.clone());
    state.reconcile_groups(None);
    finish_internal_default(&mut state);
    assert_eq!(state.assignments.len(), 1);
    let other_key = crate::application::flash::ota_assignment_key(&other);
    let image = &state.assignments[&other_key];
    assert_eq!(image.name, "other.hex");
    assert!(matches!(image.resource.as_deref(), Some(Resource::File(_))));
    assert!(
        state
            .ota_defaults
            .values()
            .all(|state| state.error.is_none())
    );
    assert!(!state.start_internal_default());
}

#[test]
fn late_query_and_catalog_default_new_groups_without_resetting_existing_group() {
    let (_directory, mut state) = internal_default_state();
    let catalog = state.catalog.take();
    assert!(state.take_internal_default().is_none());
    state.catalog = catalog;
    finish_internal_default(&mut state);
    let original = state.destination(Mode::Esb).unwrap();
    let original_image = state.displayed_image(Mode::Esb).unwrap().clone();
    state.tracker_selection.insert("tracker:a:1".into());
    assert!(state.unresolved_tracker_selection());
    assert!(state.take_internal_default().is_none());
    assert!(plan_ota_groups(&state).is_err());
    state.apply_queries(OtaQueries {
        targets: vec![target("a", 1, "other")],
        failures: vec![],
    });
    assert!(!state.unresolved_tracker_selection());
    let new_group = state.active_group.clone();
    finish_internal_default(&mut state);
    assert_eq!(state.displayed_image(Mode::Esb).unwrap().name, "other.hex");
    let ImageDestination::Ota { key, .. } = &original else {
        unreachable!()
    };
    state.reconcile_groups(Some(key.clone()));
    assert!(state.destination(Mode::Esb).as_ref() == Some(&original));
    assert!(Arc::ptr_eq(
        &original_image.bytes,
        &state.displayed_image(Mode::Esb).unwrap().bytes
    ));
    state.reconcile_groups(new_group);
    assert!(state.take_internal_default().is_none());
    assert_eq!(state.assignments.len(), 2);
}

#[test]
fn default_survives_liveness_loss_but_reselection_rejects_old_completion() {
    let (_directory, mut state) = internal_default_state();
    finish_internal_default(&mut state);
    let destination = state.destination(Mode::Esb).unwrap();
    let image = state.displayed_image(Mode::Esb).unwrap().clone();
    let selected = state.tracker_selection.clone();
    let tracker = state.targets[0].clone();
    let mut live = [TrackerLink {
        receiver_key: "a".into(),
        slot: 0,
        address: tracker.address.clone(),
        online: false,
    }];
    let groups = plan_ota_groups(&state).unwrap();
    for observation in 0..3 {
        state.review = Some(Action::EsbGroups(groups.clone()));
        state.acknowledged = true;
        live[0].online = observation == 2;
        live[0].address = if observation == 2 {
            None
        } else {
            tracker.address.clone()
        };
        let current = if observation == 1 { &[][..] } else { &live[..] };
        state.sync_tracker_selection(&selected, current.iter());
        assert!(state.destination(Mode::Esb).as_ref() == Some(&destination));
        assert!(Arc::ptr_eq(
            &image.bytes,
            &state.displayed_image(Mode::Esb).unwrap().bytes
        ));
        assert!(state.take_internal_default().is_none());
        assert!(state.review.is_none());
        assert!(!state.acknowledged);
        assert!(
            validate_live_ota_targets(
                &groups[0].targets,
                std::slice::from_ref(&tracker.receiver),
                current.iter(),
            )
            .is_err()
        );
    }
    state.sync_tracker_selection(&Default::default(), live.iter());
    state.sync_tracker_selection(&selected, live.iter());
    assert!(state.destination(Mode::Esb).as_ref() != Some(&destination));
    state.apply_internal_default(destination, Ok(image));
    assert!(state.assignments.is_empty());
    finish_internal_default(&mut state);
    assert_eq!(state.displayed_image(Mode::Esb).unwrap().name, "board.hex");
}
