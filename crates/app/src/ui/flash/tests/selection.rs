use super::support::{local_image, target};
use crate::domain::TrackerLink;
use crate::esb_ota::RECEIVER_ID;
use crate::ui::flash::{
    Action, FlashState, Mode, OtaQueries, clear_known_offline_tracker_selection,
    merge_tracker_snapshot, ota_assignment_key, ota_target_key, plan_ota_groups,
    select_receiver_trackers, selected_tracker_is_known_offline, validate_live_ota_targets,
};
use std::sync::Arc;

#[test]
fn receiver_group_selection_and_clear_never_cross_equal_slots() {
    let trackers = [
        TrackerLink {
            receiver_key: "a:receiver".into(),
            slot: 0,
            address: None,
            online: true,
        },
        TrackerLink {
            receiver_key: "a:receiver".into(),
            slot: 1,
            address: None,
            online: false,
        },
        TrackerLink {
            receiver_key: "b:receiver".into(),
            slot: 0,
            address: None,
            online: true,
        },
    ];
    let mut selected = [
        "serial:COM1".to_string(),
        "tracker:b:receiver:0".to_string(),
    ]
    .into();
    select_receiver_trackers(&mut selected, "a:receiver", &trackers, true);
    assert!(selected.contains("tracker:a:receiver:0"));
    assert!(!selected.contains("tracker:a:receiver:1"));
    assert!(selected.contains("tracker:b:receiver:0"));
    select_receiver_trackers(&mut selected, "a:receiver", &[], false);
    assert_eq!(
        selected,
        [
            "serial:COM1".to_string(),
            "tracker:b:receiver:0".to_string()
        ]
        .into()
    );
}

#[test]
fn explicit_offline_clear_is_receiver_scoped_and_preserves_other_selections() {
    let link = |receiver: &str, slot, online, address| TrackerLink {
        receiver_key: receiver.into(),
        slot,
        address,
        online,
    };
    let current = [
        link("a:receiver", 0, false, Some("112233445500".into())),
        link("a:receiver", 1, true, Some("112233445501".into())),
        link("a:receiver", 3, false, None),
        link("b:receiver", 0, false, Some("112233445502".into())),
        link("a:receiver:child", 0, false, Some("112233445503".into())),
        link("a:receiver", 4, false, Some("112233445504".into())),
    ];
    let mut selected = [
        "tracker:a:receiver:0",
        "tracker:a:receiver:1",
        "tracker:a:receiver:2",
        "tracker:a:receiver:3",
        "tracker:b:receiver:0",
        "tracker:a:receiver:child:0",
        "serial:COM1",
        "tracker:a:receiver:invalid",
    ]
    .map(str::to_owned)
    .into();
    assert!(selected_tracker_is_known_offline(
        "tracker:a:receiver:0",
        "a:receiver",
        &current,
    ));
    assert!(clear_known_offline_tracker_selection(
        &mut selected,
        "a:receiver",
        &current,
    ));
    assert_eq!(
        selected,
        [
            "tracker:a:receiver:1",
            "tracker:a:receiver:2",
            "tracker:a:receiver:3",
            "tracker:b:receiver:0",
            "tracker:a:receiver:child:0",
            "serial:COM1",
            "tracker:a:receiver:invalid",
        ]
        .map(str::to_owned)
        .into(),
    );
    assert!(!clear_known_offline_tracker_selection(
        &mut selected,
        "a:receiver",
        &current,
    ));
}

#[test]
fn offline_clear_uses_current_roster_not_retained_snapshot_or_render_time_state() {
    let mut current = [TrackerLink {
        receiver_key: "a".into(),
        slot: 0,
        address: Some("112233445500".into()),
        online: true,
    }];
    let mut selected = ["tracker:a:0".to_owned()].into();
    let mut snapshot = current.to_vec();
    merge_tracker_snapshot(&mut snapshot, &[]);
    assert!(!snapshot[0].online);
    assert!(snapshot[0].address.is_some());
    assert!(!clear_known_offline_tracker_selection(
        &mut selected,
        "a",
        &[]
    ));
    current[0].address = None;
    for online in [true, false] {
        current[0].online = online;
        merge_tracker_snapshot(&mut snapshot, &current);
        assert!(!snapshot[0].online);
        assert!(snapshot[0].address.is_some());
        assert!(!clear_known_offline_tracker_selection(
            &mut selected,
            "a",
            &current,
        ));
    }
    current[0].address = Some("112233445500".into());
    assert!(selected_tracker_is_known_offline(
        "tracker:a:0",
        "a",
        &current
    ));
    // A card/action can be rendered offline, then recover before the callback runs.
    current[0].online = true;
    assert!(!clear_known_offline_tracker_selection(
        &mut selected,
        "a",
        &current,
    ));
    assert_eq!(selected, ["tracker:a:0".to_owned()].into());
    current[0].online = false;
    assert!(clear_known_offline_tracker_selection(
        &mut selected,
        "a",
        &current,
    ));
    assert!(selected.is_empty());
}

#[test]
fn known_offline_plan_retains_metadata_and_image_until_explicit_selection_clear() {
    let tracker = target("a", 0, "board");
    let current = [TrackerLink {
        receiver_key: "a".into(),
        slot: 0,
        address: tracker.address.clone(),
        online: false,
    }];
    let mut selected = ["tracker:a:0".to_owned()].into();
    let assignment = ota_assignment_key(&tracker);
    let image = local_image("board.hex");
    let mut state = FlashState {
        targets: vec![tracker.clone()],
        assignments: [(assignment.clone(), image.clone())].into(),
        ..FlashState::default()
    };
    state.sync_tracker_selection(&selected, current.iter());
    for live in [&current[..], &[][..]] {
        state.sync_tracker_selection(&selected, live.iter());
        assert_eq!(selected, ["tracker:a:0".to_owned()].into());
        assert_eq!(state.selected_targets, [ota_target_key(&tracker)].into());
        assert_eq!(state.targets.len(), 1);
        assert!(Arc::ptr_eq(
            &state.assignments[&assignment].bytes,
            &image.bytes
        ));
    }
    state.review = Some(Action::EsbGroups(vec![]));
    assert!(clear_known_offline_tracker_selection(
        &mut selected,
        "a",
        &current
    ));
    assert!(state.sync_tracker_selection(&selected, current.iter()));
    assert!(state.selected_targets.is_empty());
    assert!(state.review.is_none());
    assert_eq!(state.targets.len(), 1);
    assert_eq!(state.targets[0].address, tracker.address);
}

#[test]
fn queried_tracker_selection_follows_roster_without_second_checkbox() {
    let a = target("a", 0, "board");
    let b = target("b", 0, "board");
    let receiver = target("a", RECEIVER_ID, "receiver");
    let mut trackers = [
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
    let selected = ["tracker:a:0".to_string()].into();
    let mut state = FlashState::default();
    state.apply_queries(OtaQueries {
        targets: vec![a.clone(), b, receiver.clone()],
        failures: vec![],
    });
    state.sync_tracker_selection(&selected, trackers.iter());
    assert_eq!(state.selected_targets, [ota_target_key(&a)].into());
    assert!(!state.selected_targets.contains(&ota_target_key(&receiver)));
    state.review = Some(Action::EsbGroups(vec![]));
    trackers[0].address = Some("AABBCCDDEEFF".into());
    assert!(state.sync_tracker_selection(&selected, trackers.iter()));
    assert!(state.review.is_none());
    assert!(state.selected_targets.is_empty());
    assert!(
        !state
            .targets
            .iter()
            .any(|target| ota_target_key(target) == ota_target_key(&a))
    );
    assert!(selected.contains("tracker:a:0"));
}

#[test]
fn monitor_restart_preserves_query_selection_and_images_but_never_authorizes_unknown_routes() {
    let tracker = target("a", 0, "board");
    let selected: std::collections::BTreeSet<String> = ["tracker:a:0".to_string()].into();
    let mut live = [TrackerLink {
        receiver_key: "a".into(),
        slot: 0,
        address: tracker.address.clone(),
        online: true,
    }];
    let receivers = [tracker.receiver.clone()];
    let assignment = ota_assignment_key(&tracker);
    let image = local_image("board.hex");
    let mut state = FlashState {
        assignments: [(assignment.clone(), image.clone())].into(),
        tracker_selection: selected.clone(),
        ..FlashState::default()
    };
    state.apply_queries(OtaQueries {
        targets: vec![tracker.clone()],
        failures: vec![],
    });
    // reserve/cancel clears the monitor; release restarts it asynchronously.
    state.sync_tracker_selection(&selected, std::iter::empty());
    assert_eq!(state.targets.len(), 1);
    assert_eq!(state.selected_targets, [ota_target_key(&tracker)].into());
    let groups = plan_ota_groups(&state).unwrap();
    assert!(Arc::ptr_eq(&groups[0].image.bytes, &image.bytes));
    assert!(validate_live_ota_targets(&groups[0].targets, &receivers, std::iter::empty()).is_err());
    live[0].address = None;
    state.sync_tracker_selection(&selected, live.iter());
    assert_eq!(state.selected_targets, [ota_target_key(&tracker)].into());
    assert!(validate_live_ota_targets(&groups[0].targets, &receivers, live.iter()).is_err());
    live[0].address = tracker.address.clone();
    state.sync_tracker_selection(&selected, live.iter());
    assert!(validate_live_ota_targets(&groups[0].targets, &receivers, live.iter()).is_ok());
    assert!(Arc::ptr_eq(
        &state.assignments[&assignment].bytes,
        &image.bytes
    ));
    assert!(validate_live_ota_targets(&groups[0].targets, &[], live.iter()).is_err());
    // A real replacement must invalidate both cached targets and a frozen review.
    state.review = Some(Action::EsbGroups(groups.clone()));
    live[0].address = Some("AABBCCDDEEFF".into());
    assert!(validate_live_ota_targets(&groups[0].targets, &receivers, live.iter()).is_err());
    state.sync_tracker_selection(&selected, live.iter());
    assert!(state.targets.is_empty());
    assert!(state.selected_targets.is_empty());
    assert!(state.review.is_none());
    assert!(plan_ota_groups(&state).is_err());
}

#[test]
fn new_unqueried_selection_invalidates_review_without_removing_offline_known_targets() {
    let a = target("a", 0, "board");
    let mut trackers = [TrackerLink {
        receiver_key: "a".into(),
        slot: 0,
        address: a.address.clone(),
        online: true,
    }];
    let mut selected = ["tracker:a:0".to_string()].into();
    let mut state = FlashState {
        targets: vec![a],
        ..FlashState::default()
    };
    state.sync_tracker_selection(&selected, trackers.iter());
    state.review = Some(Action::EsbGroups(vec![]));
    selected.insert("tracker:a:1".into());
    assert!(state.sync_tracker_selection(&selected, trackers.iter()));
    assert!(state.review.is_none());
    trackers[0].online = false;
    state.sync_tracker_selection(&selected, trackers.iter());
    assert_eq!(state.selected_targets.len(), 1);
    assert_eq!(state.targets.len(), 1);
    assert!(selected.contains("tracker:a:0"));
}

#[test]
fn liveness_expiry_preserves_active_tracker_group_image_and_rejects_stale_review() {
    let receiver = target("a", RECEIVER_ID, "receiver-board");
    let tracker = target("a", 3, "tracker-board");
    let selected = ["tracker:a:3".into()].into();
    let mut live = [TrackerLink {
        receiver_key: "a".into(),
        slot: 3,
        address: tracker.address.clone(),
        online: true,
    }];
    let receivers = [receiver.receiver.clone()];
    let mut state = FlashState {
        targets: vec![receiver.clone(), tracker.clone()],
        selected_targets: [ota_target_key(&receiver)].into(),
        ..FlashState::default()
    };
    state.reconcile_groups(None);
    state.apply_images(
        state.destination(Mode::Esb).unwrap(),
        vec![local_image("receiver.hex")],
    );
    state.sync_tracker_selection(&selected, live.iter());
    let tracker_destination = state.destination(Mode::Esb).unwrap();
    let image = local_image("tracker.hex");
    state.apply_images(tracker_destination.clone(), vec![image.clone()]);
    let groups = plan_ota_groups(&state).unwrap();
    assert!(
        validate_live_ota_targets(
            groups.iter().flat_map(|group| &group.targets),
            &receivers,
            live.iter()
        )
        .is_ok()
    );

    // Silence, monitor handoff, and unknown address all retain the user's
    // plan but revoke a frozen confirmation and cannot authorize a send.
    for observation in 0..3 {
        state.review = Some(Action::EsbGroups(groups.clone()));
        state.acknowledged = true;
        live[0].online = observation != 0;
        live[0].address = if observation == 2 {
            None
        } else {
            tracker.address.clone()
        };
        let current = if observation == 1 { &[][..] } else { &live[..] };
        assert!(state.sync_tracker_selection(&selected, current.iter()));
        assert!(state.destination(Mode::Esb).as_ref() == Some(&tracker_destination));
        assert_eq!(state.selected_targets.len(), 2);
        assert_eq!(state.assignments.len(), 2);
        assert!(!state.unresolved_tracker_selection());
        assert!(Arc::ptr_eq(
            &state.displayed_image(Mode::Esb).unwrap().bytes,
            &image.bytes
        ));
        assert!(state.review.is_none());
        assert!(!state.acknowledged);
        let retained = plan_ota_groups(&state).unwrap();
        assert!(
            validate_live_ota_targets(
                retained.iter().flat_map(|group| &group.targets),
                &receivers,
                current.iter()
            )
            .is_err()
        );
        assert!(
            validate_live_ota_targets(
                groups.iter().flat_map(|group| &group.targets),
                &receivers,
                current.iter()
            )
            .is_err()
        );
    }
    live[0].online = true;
    live[0].address = tracker.address.clone();
    state.sync_tracker_selection(&selected, live.iter());
    assert!(state.destination(Mode::Esb).as_ref() == Some(&tracker_destination));
    assert!(
        validate_live_ota_targets(
            groups.iter().flat_map(|group| &group.targets),
            &receivers,
            live.iter()
        )
        .is_ok()
    );
    assert!(state.review.is_none());
    state.sync_tracker_selection(&Default::default(), live.iter());
    assert_eq!(
        state.active_group.as_ref(),
        Some(&ota_assignment_key(&receiver))
    );
    assert!(
        !state
            .assignments
            .contains_key(&ota_assignment_key(&tracker))
    );
}

#[test]
fn binding_invalidation_discards_stale_fw_info_and_tail_without_touching_an_admitted_update() {
    use crate::discovery::ReceiverMonitor;
    use crate::ui::flash::Lease;

    let old: Vec<_> = (0..10).map(|slot| target("a", slot, "board")).collect();
    let untouched = target("b", 0, "board");
    let receiver = target("a", RECEIVER_ID, "receiver-board");
    let mut state = FlashState {
        tracker_selection: (0..10)
            .map(|slot| format!("tracker:a:{slot}"))
            .chain(["tracker:b:0".into()])
            .collect(),
        ..FlashState::default()
    };
    state.apply_queries(OtaQueries {
        targets: old
            .iter()
            .cloned()
            .chain([untouched.clone(), receiver.clone()])
            .collect(),
        failures: vec![],
    });
    let image = local_image("board.hex");
    let group = ota_assignment_key(&untouched);
    state.assignments.insert(group.clone(), image.clone());
    let groups = plan_ota_groups(&state).unwrap();
    state.review = Some(Action::EsbGroups(groups.clone()));
    state.pending = Some(Action::EsbGroups(groups.clone()));
    state.operation_pins.addresses.insert("112233445500".into());
    let mut monitor = ReceiverMonitor::fixture(old[0].receiver.clone(), 0, "112233445500", true);
    let token = monitor.reserve(&old[0].receiver).unwrap();
    state.leases.push(Lease::Hid(token.clone()));
    for owner in ["a", "b"] {
        state.tracker_snapshot.insert(
            owner.into(),
            old.iter()
                .map(|target| TrackerLink {
                    receiver_key: owner.into(),
                    slot: target.id,
                    address: target.address.clone(),
                    online: false,
                })
                .collect(),
        );
    }
    state.revoke_receiver_bindings("a");
    assert_eq!(
        state.targets.iter().map(ota_target_key).collect::<Vec<_>>(),
        [ota_target_key(&untouched), ota_target_key(&receiver)]
    );
    assert_eq!(state.selected_targets, [ota_target_key(&untouched)].into());
    assert_eq!(state.tracker_selection, ["tracker:b:0".to_owned()].into());
    assert!(!state.tracker_snapshot.contains_key("a"));
    assert_eq!(state.tracker_snapshot["b"].len(), 10);
    assert!(state.review.is_none());
    assert!(
        matches!(&state.pending, Some(Action::EsbGroups(admitted)) if
        admitted.iter().flat_map(|group| &group.targets).count() == 11)
    );
    assert_eq!(state.leases.len(), 1);
    assert!(monitor.reservation_ready(&token).unwrap());
    assert!(state.operation_pins.addresses.contains("112233445500"));
    assert!(Arc::ptr_eq(&state.assignments[&group].bytes, &image.bytes));
    let fresh: Vec<_> = (0..7)
        .map(|slot| TrackerLink {
            receiver_key: "a".into(),
            slot,
            address: Some(format!("AABBCCDDEE{slot:02X}")),
            online: true,
        })
        .collect();
    merge_tracker_snapshot(
        state.tracker_snapshot.entry("a".into()).or_default(),
        &fresh,
    );
    assert_eq!(state.tracker_snapshot["a"].len(), 7);
    assert!(
        state.tracker_snapshot["a"]
            .iter()
            .all(|tracker| tracker.slot < 7)
    );
    assert_eq!(
        state
            .targets
            .iter()
            .filter(|target| target.receiver.key == "a")
            .count(),
        1
    );
}

#[test]
fn a_query_admitted_before_binding_invalidation_cannot_restore_old_tracker_fw_info() {
    use crate::ui::flash::OtaQueryPlan;

    let stale = target("a", 9, "board");
    let untouched = target("b", 9, "board");
    let receiver = target("a", RECEIVER_ID, "receiver-board");
    let mut state = FlashState {
        pending: Some(Action::QueryGroups(OtaQueryPlan {
            groups: vec![(stale.receiver.clone(), vec![], true)],
            automatic: false,
            incremental: true,
        })),
        ..FlashState::default()
    };
    state.revoke_receiver_bindings("a");
    state.apply_current_queries(
        OtaQueries {
            targets: vec![stale, untouched.clone(), receiver.clone()],
            failures: vec![
                ("a:9".into(), ("old slot".into(), Err("stale".into()))),
                ("b:9".into(), ("other slot".into(), Err("retained".into()))),
            ],
        },
        &[receiver.receiver.clone(), untouched.receiver.clone()],
    );
    assert_eq!(
        state.targets.iter().map(ota_target_key).collect::<Vec<_>>(),
        [ota_target_key(&untouched), ota_target_key(&receiver)]
    );
    assert!(!state.query_failures.contains_key("a:9"));
    assert!(state.query_failures.contains_key("b:9"));
    assert!(state.invalidated_query_receivers.is_empty());
    assert!(matches!(state.pending, Some(Action::QueryGroups(_))));
}

#[test]
fn fresh_reader_contradictions_revoke_cached_authority_but_missing_or_foreign_evidence_does_not() {
    let tracker = target("a", 0, "board");
    let state = FlashState {
        targets: vec![tracker.clone()],
        ..FlashState::default()
    };
    let mut live = [TrackerLink {
        receiver_key: "a".into(),
        slot: 0,
        address: tracker.address.clone(),
        online: false,
    }];
    assert!(!state.receiver_binding_replaced("a", &live));
    assert!(!state.receiver_binding_replaced("a", &[]));
    live[0].address = None;
    assert!(!state.receiver_binding_replaced("a", &live));
    live[0].address = Some("AABBCCDDEEFF".into());
    live[0].receiver_key = "b".into();
    assert!(!state.receiver_binding_replaced("a", &live));
    live[0].receiver_key = "a".into();
    assert!(state.receiver_binding_replaced("a", &live));
}
