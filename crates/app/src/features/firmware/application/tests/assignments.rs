use super::support::{local_image, target};
use crate::application::flash::{
    Action, FlashState, ImageDestination, Mode, OtaQueries, load_image, ota_assignment_key,
    ota_target_key, plan_ota_groups,
};
use crate::domain::TrackerLink;
use crate::features::firmware::esb_ota::RECEIVER_ID;
use std::sync::Arc;

#[test]
fn esb_focus_follows_new_tracker_without_reusing_receiver_or_global_image() {
    let receiver = target("a", RECEIVER_ID, "board");
    let tracker = target("a", 0, "board");
    let mut state = FlashState {
        targets: vec![receiver.clone(), tracker.clone()],
        selected_targets: [ota_target_key(&receiver)].into(),
        image: Some(local_image("uf2-global.hex")),
        ..FlashState::default()
    };
    state.reconcile_groups(None);
    let receiver_destination = state.destination(Mode::Esb).unwrap();
    state.apply_images(receiver_destination, vec![local_image("receiver.hex")]);
    let selected = ["tracker:a:0".into()].into();
    let live = [TrackerLink {
        receiver_key: "a".into(),
        slot: 0,
        address: tracker.address.clone(),
        online: true,
    }];
    state.sync_tracker_selection(&selected, live.iter());
    assert_eq!(
        state.active_group.as_ref(),
        Some(&ota_assignment_key(&tracker))
    );
    assert!(state.displayed_image(Mode::Esb).is_none());
    assert_eq!(
        state.displayed_image(Mode::Uf2).unwrap().name,
        "uf2-global.hex"
    );
    assert!(plan_ota_groups(&state).is_err());
    state.apply_images(
        state.destination(Mode::Esb).unwrap(),
        vec![local_image("tracker.hex")],
    );
    assert_eq!(plan_ota_groups(&state).unwrap().len(), 2);
    state.reconcile_groups(Some(ota_assignment_key(&receiver)));
    assert_eq!(
        state.displayed_image(Mode::Esb).unwrap().name,
        "receiver.hex"
    );
    state.reconcile_groups(Some(ota_assignment_key(&tracker)));
    assert_eq!(
        state.displayed_image(Mode::Esb).unwrap().name,
        "tracker.hex"
    );
}

#[test]
fn same_board_across_receivers_keeps_image_when_group_members_change() {
    let a = target("a", 0, "board");
    let b = target("b", 1, "board");
    let live = [
        TrackerLink {
            receiver_key: "a".into(),
            slot: 0,
            address: a.address.clone(),
            online: true,
        },
        TrackerLink {
            receiver_key: "b".into(),
            slot: 1,
            address: b.address.clone(),
            online: true,
        },
    ];
    let mut state = FlashState {
        targets: vec![a, b],
        ..FlashState::default()
    };
    let mut selected = ["tracker:a:0".into()].into();
    state.sync_tracker_selection(&selected, live.iter());
    let original = state.destination(Mode::Esb).unwrap();
    let image = local_image("board.hex");
    state.apply_images(original.clone(), vec![image.clone()]);
    selected.insert("tracker:b:1".into());
    state.sync_tracker_selection(&selected, live.iter());
    assert!(state.destination(Mode::Esb).as_ref() == Some(&original));
    selected.remove("tracker:a:0");
    state.sync_tracker_selection(&selected, live.iter());
    assert!(Arc::ptr_eq(
        &state.displayed_image(Mode::Esb).unwrap().bytes,
        &image.bytes
    ));
    let groups = plan_ota_groups(&state).unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].targets[0].receiver.key, "b");
}

#[test]
fn asynchronous_image_completion_uses_original_group_across_focus_and_mode_changes() {
    let a = target("a", 0, "one");
    let b = target("b", 0, "two");
    let mut state = FlashState {
        selected_targets: [ota_target_key(&a), ota_target_key(&b)].into(),
        targets: vec![a.clone(), b.clone()],
        image: Some(local_image("global.hex")),
        ..FlashState::default()
    };
    state.reconcile_groups(Some(ota_assignment_key(&a)));
    let pending = state.destination(Mode::Esb).unwrap();
    state.reconcile_groups(Some(ota_assignment_key(&b)));
    state.mode = Mode::Ble;
    state.apply_images(pending, vec![local_image("one.hex")]);
    assert!(state.displayed_image(Mode::Esb).is_none());
    assert_eq!(state.displayed_image(Mode::Ble).unwrap().name, "global.hex");
    assert_eq!(state.assignments[&ota_assignment_key(&a)].name, "one.hex");
    state.reconcile_groups(Some(ota_assignment_key(&a)));
    assert_eq!(state.displayed_image(Mode::Esb).unwrap().name, "one.hex");
}

#[test]
fn removing_and_readding_group_drops_old_async_result_and_hides_stale_image() {
    let tracker = target("a", 0, "board");
    let key = ota_target_key(&tracker);
    let mut state = FlashState {
        selected_targets: [key.clone()].into(),
        targets: vec![tracker],
        image: Some(local_image("global.hex")),
        ..FlashState::default()
    };
    state.reconcile_groups(None);
    let pending = state.destination(Mode::Esb).unwrap();
    state.apply_images(pending.clone(), vec![local_image("old.hex")]);
    state.selected_targets.clear();
    state.reconcile_groups(None);
    assert!(state.displayed_image(Mode::Esb).is_none());
    assert!(state.destination(Mode::Esb).is_none());
    assert!(state.assignments.is_empty());
    state.selected_targets.insert(key);
    state.reconcile_groups(None);
    state.apply_images(pending, vec![local_image("late.hex")]);
    assert!(state.displayed_image(Mode::Esb).is_none());
    assert_eq!(state.displayed_image(Mode::Uf2).unwrap().name, "global.hex");
}

#[test]
fn package_members_and_remote_version_selection_are_scoped_to_original_group() {
    let a = target("a", 0, "one");
    let b = target("b", 0, "two");
    let mut state = FlashState {
        selected_targets: [ota_target_key(&a), ota_target_key(&b)].into(),
        targets: vec![a.clone(), b.clone()],
        ..FlashState::default()
    };
    state.reconcile_groups(Some(ota_assignment_key(&a)));
    let destination = state.destination(Mode::Esb).unwrap();
    state.prepare_remote(Mode::Esb, &destination);
    state.select_version(123);
    state.apply_images(
        destination.clone(),
        vec![local_image("first.hex"), local_image("second.hex")],
    );
    assert!(state.displayed_image(Mode::Esb).is_none());
    state.reconcile_groups(Some(ota_assignment_key(&b)));
    assert!(state.destination(Mode::Esb).as_ref() != Some(&state.images_destination));
    assert!(state.destination(Mode::Esb).as_ref() != Some(&state.remote_destination));
    state.prepare_remote(Mode::Esb, &state.destination(Mode::Esb).unwrap());
    assert!(state.selected_version.is_none());
    state.reconcile_groups(Some(ota_assignment_key(&a)));
    assert!(state.destination(Mode::Esb).as_ref() == Some(&state.images_destination));
    state
        .choose_image(&destination, state.images[1].clone())
        .unwrap();
    assert_eq!(state.displayed_image(Mode::Esb).unwrap().name, "second.hex");
    assert!(!state.assignments.contains_key(&ota_assignment_key(&b)));
    assert!(plan_ota_groups(&state).is_err());
}

#[test]
fn archive_members_are_rematched_by_actual_filename_before_ota_choice() {
    let (_directory, mut state) = super::support::internal_default_state();
    state
        .catalog
        .as_mut()
        .unwrap()
        .legacy_targets
        .get_mut("board")
        .unwrap()
        .targets[0]
        .firmware_aliases = vec!["board.hex".into()];
    let catalog = state.catalog.as_ref().unwrap();
    let mut compatible = local_image("board-2.0.0.hex");
    compatible.hints = catalog.match_filename("other.hex").into();
    let mut incompatible = local_image("other.hex");
    incompatible.hints = catalog.match_filename("board-2.0.0.hex").into();
    let destination = state.destination(Mode::Esb).unwrap();
    state.apply_images(destination.clone(), vec![compatible, incompatible]);
    assert!(state.displayed_image(Mode::Esb).is_none());
    assert_eq!(state.images[0].hints[0].board_target, "board");
    assert_eq!(state.images[1].hints[0].board_target, "other");
    assert!(
        state
            .choose_image(&destination, state.images[1].clone())
            .is_err()
    );
    state
        .choose_image(&destination, state.images[0].clone())
        .unwrap();
    assert_eq!(
        state.displayed_image(Mode::Esb).unwrap().name,
        "board-2.0.0.hex"
    );
    assert_eq!(
        plan_ota_groups(&state).unwrap()[0].targets[0]
            .info
            .board_target,
        "board"
    );
}

#[test]
fn configured_preset_load_assigns_current_group_without_copy_step() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("slimenrf-helper.yaml");
    std::fs::write(
        &config,
        br#"{"schema_version":1,"firmware_root":".","legacy_targets":{"board":{"targets":[
        {"name":"board","firmware":"board.hex","esb_ota_kind":"tracker"}
    ]}}}"#,
    )
    .unwrap();
    std::fs::write(
        directory.path().join("board.hex"),
        b":0410000001020304E2\n:00000001FF\n",
    )
    .unwrap();
    let catalog = crate::features::firmware::board_catalog::Catalog::load(&config).unwrap();
    let tracker = target("a", 0, "board");
    let path = catalog
        .ota_target(
            crate::features::firmware::board_catalog::EsbOtaKind::Tracker,
            "board",
        )
        .unwrap()
        .image_for_bootloader(&tracker.info.bootloader)
        .unwrap()
        .to_owned();
    let mut state = FlashState {
        selected_targets: [ota_target_key(&tracker)].into(),
        targets: vec![tracker],
        catalog: Some(catalog),
        ..FlashState::default()
    };
    state.reconcile_groups(None);
    let image = load_image(path, &std::sync::atomic::AtomicBool::new(false)).unwrap();
    state.apply_images(state.destination(Mode::Esb).unwrap(), vec![image]);
    assert_eq!(state.displayed_image(Mode::Esb).unwrap().name, "board.hex");
    assert_eq!(plan_ota_groups(&state).unwrap()[0].image.name, "board.hex");
    assert!(state.image.is_none());
    assert!(state.review.is_none());
    assert!(!state.auto);
}

#[test]
fn query_refresh_preserves_assignment_but_bootloader_or_board_change_revokes_it() {
    for changed_board in [false, true] {
        let tracker = target("a", 0, "board");
        let mut state = FlashState {
            targets: vec![tracker.clone()],
            tracker_selection: ["tracker:a:0".into()].into(),
            selected_targets: [ota_target_key(&tracker)].into(),
            ..FlashState::default()
        };
        state.reconcile_groups(None);
        let destination = state.destination(Mode::Esb).unwrap();
        state.apply_images(destination.clone(), vec![local_image("board.hex")]);
        state.review = Some(Action::EsbGroups(plan_ota_groups(&state).unwrap()));
        state.apply_queries(OtaQueries {
            targets: vec![tracker.clone()],
            failures: vec![],
        });
        assert_eq!(state.displayed_image(Mode::Esb).unwrap().name, "board.hex");
        assert!(state.review.is_none());
        let mut changed = tracker;
        if changed_board {
            changed.info.board_target = "other".into();
        } else {
            // Both are non-MCUboot: even an unchanged assignment key must revoke old consent.
            changed.info.bootloader = "adafruit_uf2".into();
        }
        state.apply_queries(OtaQueries {
            targets: vec![changed],
            failures: vec![],
        });
        state.apply_images(destination, vec![local_image("late.hex")]);
        assert!(state.displayed_image(Mode::Esb).is_none());
        assert!(plan_ota_groups(&state).is_err());
    }
}

#[test]
fn unqueried_selected_tracker_blocks_partial_review_and_focuses_after_query() {
    let receiver = target("a", RECEIVER_ID, "receiver");
    let tracker = target("a", 0, "board");
    let mut state = FlashState {
        targets: vec![receiver.clone()],
        selected_targets: [ota_target_key(&receiver)].into(),
        tracker_selection: ["tracker:a:0".into()].into(),
        ..FlashState::default()
    };
    state.reconcile_groups(None);
    state.apply_images(
        state.destination(Mode::Esb).unwrap(),
        vec![local_image("receiver.hex")],
    );
    assert!(state.unresolved_tracker_selection());
    assert!(plan_ota_groups(&state).is_err());
    state.apply_queries(OtaQueries {
        targets: vec![tracker.clone()],
        failures: vec![],
    });
    assert!(!state.unresolved_tracker_selection());
    assert_eq!(
        state.active_group.as_ref(),
        Some(&ota_assignment_key(&tracker))
    );
    assert!(state.displayed_image(Mode::Esb).is_none());
    assert!(plan_ota_groups(&state).is_err());
}

#[test]
fn global_download_finishing_in_esb_does_not_assign_an_ota_group() {
    let tracker = target("a", 0, "board");
    let mut state = FlashState {
        selected_targets: [ota_target_key(&tracker)].into(),
        targets: vec![tracker],
        ..FlashState::default()
    };
    state.reconcile_groups(None);
    state.apply_images(ImageDestination::Global, vec![local_image("global.hex")]);
    assert!(state.displayed_image(Mode::Esb).is_none());
    assert_eq!(
        state.displayed_image(Mode::OpenDfu).unwrap().name,
        "global.hex"
    );
    assert!(state.destination(Mode::Esb).as_ref() != Some(&state.images_destination));
    assert!(plan_ota_groups(&state).is_err());
}

#[test]
fn address_replacement_revokes_pending_image_even_when_group_has_another_member() {
    let a = target("a", 0, "board");
    let b = target("b", 0, "board");
    let selected = ["tracker:a:0".into(), "tracker:b:0".into()].into();
    let mut live = [
        TrackerLink {
            receiver_key: "a".into(),
            slot: 0,
            address: a.address.clone(),
            online: true,
        },
        TrackerLink {
            receiver_key: "b".into(),
            slot: 0,
            address: b.address.clone(),
            online: true,
        },
    ];
    let mut state = FlashState {
        targets: vec![a, b],
        ..FlashState::default()
    };
    state.sync_tracker_selection(&selected, live.iter());
    let pending = state.destination(Mode::Esb).unwrap();
    state.apply_images(pending.clone(), vec![local_image("board.hex")]);
    live[0].address = Some("AABBCCDDEEFF".into());
    state.sync_tracker_selection(&selected, live.iter());
    assert!(state.active_group.is_some());
    state.apply_images(pending, vec![local_image("late.hex")]);
    assert!(state.displayed_image(Mode::Esb).is_none());
    assert!(state.unresolved_tracker_selection());
    assert!(plan_ota_groups(&state).is_err());
}
