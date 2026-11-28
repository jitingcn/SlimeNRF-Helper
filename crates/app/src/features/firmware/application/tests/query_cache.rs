use super::support::{local_image, query_link, seeded_query, target};
use crate::application::flash::{
    Action, FlashState, Mode, OtaPlanGroup, OtaQueries, OtaQueryPlan, Payload,
    available_query_groups, ota_assignment_key, ota_target_key, query_failure,
    validate_live_ota_targets,
};
use crate::domain::TrackerLink;
use crate::features::firmware::esb_ota::RECEIVER_ID;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[test]
fn manual_retry_resolves_failed_target() {
    let refreshed = target("a", 1, "board");
    let mut state = FlashState {
        query_failures: [query_failure(&refreshed.receiver, 1, "timeout".into())].into(),
        ..FlashState::default()
    };
    state.prepare_action_results(&Action::QueryGroups(OtaQueryPlan {
        groups: vec![(refreshed.receiver.clone(), vec![query_link("a", 1)], false)],
        automatic: false,
    }));
    assert!(state.query_failures.contains_key("a:1"));
    state.apply_queries(OtaQueries {
        targets: vec![refreshed],
        failures: vec![],
    });
    assert!(state.query_failures.is_empty());
}

#[test]
fn manual_scoped_retry_keeps_failed_card_and_unrelated_state_until_result() {
    use crate::application::flash::{TrackerSection, tracker_section};
    let mut retried = target("a", 1, "board");
    let other = target("b", 0, "board");
    let other_group = ota_assignment_key(&other);
    let other_image = local_image("other.hex");
    let requested = TrackerLink {
        address: retried.address.clone(),
        ..query_link("a", 1)
    };
    let mut state = FlashState {
        targets: vec![other.clone()],
        selected_targets: [ota_target_key(&other)].into(),
        tracker_selection: ["tracker:b:0".into()].into(),
        assignments: [(other_group.clone(), other_image.clone())].into(),
        query_failures: [
            query_failure(&retried.receiver, 1, "retry A".into()),
            query_failure(&other.receiver, 0, "keep B".into()),
        ]
        .into(),
        ..FlashState::default()
    };
    let group = state
        .esb_auto_query
        .scoped_query_group(
            &retried.receiver,
            &retried.receiver,
            Some(&requested),
            std::slice::from_ref(&requested),
            Instant::now(),
        )
        .unwrap();
    let action = Action::QueryGroups(OtaQueryPlan {
        groups: vec![group],
        automatic: false,
    });
    state.prepare_action_results(&action);
    state.pending = Some(action.clone());
    state.activity_kind = crate::application::flash::ActivityKind::Query;
    assert!(state.querying_receiver("a"));
    assert!(!state.querying_receiver("b"));
    assert!(state.querying_tracker(&requested));
    assert!(!state.querying_tracker(&query_link("a", 2)));
    assert!(!state.querying_tracker(&query_link("b", 1)));
    assert_eq!(state.query_failures["a:1"], "retry A");
    assert_eq!(state.query_failures["b:0"], "keep B");
    assert_eq!(
        tracker_section(&requested, None, false, true),
        TrackerSection::Error
    );
    assert_eq!(
        tracker_section(
            &TrackerLink {
                online: false,
                ..requested
            },
            None,
            true,
            true,
        ),
        TrackerSection::Error,
    );

    state.pending = None;
    state.finish_job_progress(None);
    retried.info.version = "refreshed".into();
    state.apply_queries(OtaQueries {
        targets: vec![retried.clone()],
        failures: vec![],
    });
    assert!(!state.query_failures.contains_key("a:1"));
    assert_eq!(state.query_failures["b:0"], "keep B");
    assert_eq!(state.targets[0].info.version, other.info.version);
    assert_eq!(state.targets[1].info.version, "refreshed");
    assert_eq!(state.selected_targets, [ota_target_key(&other)].into());
    assert_eq!(state.tracker_selection, ["tracker:b:0".into()].into());
    assert!(Arc::ptr_eq(
        &state.assignments[&other_group].bytes,
        &other_image.bytes
    ));
    assert!(!state.selected_targets.contains(&ota_target_key(&retried)));
}

#[test]
fn late_query_results_cannot_replace_new_positive_identity_or_revoke_its_assignment() {
    for replacement in ["path", "serial", "address"] {
        let stale = target("a", 1, "old-board");
        let mut current = target("a", 1, "current-board");
        match replacement {
            "path" => current.receiver.path = b"new endpoint".to_vec(),
            "serial" => current.receiver.serial_number = Some("new serial".into()),
            _ => current.address = Some("AABBCCDDEEFF".into()),
        }
        let group = ota_assignment_key(&current);
        let image = local_image("current.hex");
        let mut state = FlashState {
            targets: vec![current.clone()],
            tracker_selection: ["tracker:a:1".into()].into(),
            selected_targets: [ota_target_key(&current)].into(),
            assignments: [(group.clone(), image.clone())].into(),
            group_generations: [(group.clone(), 7)].into(),
            ..FlashState::default()
        };
        let live = TrackerLink {
            receiver_key: "a".into(),
            slot: 1,
            address: current.address.clone(),
            online: true,
        };
        // Observation remains useful even off-page; only issuing queries
        // is page-gated. A late manual result must obey the same guard.
        state
            .esb_auto_query
            .observe(&current.receiver, &[live], Instant::now());
        state.apply_current_queries(
            OtaQueries {
                targets: vec![stale],
                failures: vec![],
            },
            std::slice::from_ref(&current.receiver),
        );
        assert_eq!(state.targets[0].receiver.path, current.receiver.path);
        assert_eq!(
            state.targets[0].receiver.serial_number,
            current.receiver.serial_number
        );
        assert_eq!(state.targets[0].address, current.address);
        assert_eq!(state.targets[0].info.board_target, "current-board");
        assert_eq!(state.selected_targets, [ota_target_key(&current)].into());
        assert!(Arc::ptr_eq(&state.assignments[&group].bytes, &image.bytes));
        assert_eq!(state.group_generations[&group], 7);
    }
}

#[test]
fn late_query_result_survives_reserved_unknown_roster_without_losing_assignment() {
    let mut refreshed = target("a", 1, "board");
    let group = ota_assignment_key(&refreshed);
    let image = local_image("board.hex");
    let mut state = FlashState {
        targets: vec![refreshed.clone()],
        tracker_selection: ["tracker:a:1".into()].into(),
        selected_targets: [ota_target_key(&refreshed)].into(),
        assignments: [(group.clone(), image.clone())].into(),
        ..FlashState::default()
    };
    let live = TrackerLink {
        receiver_key: "a".into(),
        slot: 1,
        address: refreshed.address.clone(),
        online: true,
    };
    let now = Instant::now();
    state
        .esb_auto_query
        .observe(&refreshed.receiver, std::slice::from_ref(&live), now);
    state.esb_auto_query.observe(&refreshed.receiver, &[], now);
    state.esb_auto_query.observe(
        &refreshed.receiver,
        &[TrackerLink {
            address: None,
            online: false,
            ..live
        }],
        now,
    );
    refreshed.info.version = "new information".into();
    // Missing inventory during handoff is also unknown, not a replacement.
    state.apply_current_queries(
        OtaQueries {
            targets: vec![refreshed.clone()],
            failures: vec![],
        },
        &[],
    );
    assert_eq!(state.targets[0].info.version, "new information");
    assert!(Arc::ptr_eq(&state.assignments[&group].bytes, &image.bytes));
    assert_eq!(state.selected_targets, [ota_target_key(&refreshed)].into());
}

#[test]
fn incremental_query_preserves_unrelated_failures_and_completed_flash_outcome() {
    let a = target("a", 0, "board").receiver;
    let mut b = target("b", 0, "board").receiver;
    b.label = a.label.clone(); // Labels are not receiver identities.
    let mut state = FlashState {
        results: vec![(
            crate::application::flash::ResultTarget::generic(
                "completed device".into(),
                "completed device".into(),
            ),
            crate::update::UpdateOutcome::Completed,
        )],
        result_success: "firmware.result.copy_completed",
        result_notes: [("completed device".into(), "keep power connected".into())].into(),
        last_progress: Some(crate::update::Progress::bytes("completed flash", 10, 10)),
        query_failures: [
            query_failure(&a, 0, "A timed out".into()),
            query_failure(&b, 0, "old B failure".into()),
        ]
        .into(),
        ..FlashState::default()
    };
    let action = Action::QueryGroups(OtaQueryPlan {
        groups: vec![(b.clone(), vec![query_link("b", 0)], false)],
        automatic: true,
    });
    state.prepare_action_results(&action);
    assert_eq!(state.query_failures["a:0"], "A timed out");
    assert!(!state.query_failures.contains_key("b:0"));
    assert_eq!(state.results[0].0.key, "completed device");
    assert_eq!(state.results[0].1, crate::update::UpdateOutcome::Completed);
    assert_eq!(state.result_success, "firmware.result.copy_completed");
    assert_eq!(
        state.result_notes["completed device"],
        "keep power connected"
    );
    // Read-only jobs of either kind discard their own progress, never the
    // earlier operation's final progress.
    for kind in [
        crate::application::flash::ActivityKind::Query,
        crate::application::flash::ActivityKind::Preparation,
    ] {
        state.activity_kind = kind;
        state.finish_job_progress(Some(crate::update::Progress::bytes("read only", 1, 1)));
        assert_eq!(
            state.last_progress.as_ref().unwrap().message,
            "completed flash"
        );
    }
    state.apply_queries(OtaQueries {
        targets: vec![],
        failures: vec![query_failure(&b, 0, "new B failure".into())],
    });
    assert_eq!(state.query_failures.len(), 2);
    assert_eq!(state.query_failures["a:0"], "A timed out");
    assert_eq!(state.query_failures["b:0"], "new B failure");
    assert_eq!(state.results[0].0.key, "completed device");
    assert_eq!(state.results[0].1, crate::update::UpdateOutcome::Completed);
    assert!(state.last_progress.is_some());
    assert_eq!(state.result_notes.len(), 1);
}

#[test]
fn in_flight_query_survives_rapid_page_switch_and_finishes_into_cache() {
    use std::sync::{atomic::Ordering, mpsc};
    let now = Instant::now();
    let device = target("a", 1, "board");
    let link = TrackerLink {
        address: device.address.clone(),
        ..query_link("a", 1)
    };
    let receiver = device.receiver.clone();
    let mut state = FlashState {
        esb_auto_query: seeded_query(
            std::slice::from_ref(&receiver),
            std::slice::from_ref(&link),
            now,
        ),
        activity_kind: crate::application::flash::ActivityKind::Query,
        activity_origin: Mode::Esb,
        ..FlashState::default()
    };
    let (finish, wait) = mpsc::channel();
    state.job = Some(
        crate::update::Job::start(move |cancel, _| {
            wait.recv_timeout(Duration::from_secs(2)).unwrap();
            assert!(
                !cancel.load(Ordering::Acquire),
                "navigation must not cancel readonly work"
            );
            Ok(Payload::Update(
                crate::features::firmware::executor::UpdateCompletion::Queries(
                    crate::features::firmware::executor::QueryResults {
                        targets: vec![device],
                        failures: vec![],
                    },
                ),
            ))
        })
        .unwrap(),
    );
    assert!(!state.enter_esb_page(false));
    assert!(state.busy());
    assert!(!state.shows_activity(Mode::Uf2));
    assert!(!state.enter_esb_page(true));
    let busy = state.busy();
    assert!(!state.esb_auto_query.take_ready(true, busy, now));
    assert!(
        !state
            .esb_auto_query
            .take_ready(true, busy, now + Duration::from_secs(1))
    );
    assert!(!state.enter_esb_page(false));
    finish.send(()).unwrap();
    let result = loop {
        if let Some(result) = state.job.as_mut().unwrap().poll() {
            break result;
        }
        std::thread::yield_now();
    };
    state.finish_job_progress(None);
    state.job = None;
    let Payload::Update(crate::features::firmware::executor::UpdateCompletion::Queries(query)) =
        result.unwrap()
    else {
        panic!("query payload")
    };
    state.apply_current_queries(query.into(), std::slice::from_ref(&receiver));
    assert_eq!(state.targets.len(), 1);
    state.enter_esb_page(true);
    assert!(
        !state
            .esb_auto_query
            .take_ready(true, false, now + Duration::from_secs(2))
    );
    assert!(
        state
            .esb_auto_query
            .take_ready(true, false, now + Duration::from_secs(3))
    );
    assert!(
        state
            .esb_auto_query
            .take_incremental(
                std::iter::once(&receiver),
                std::iter::once(&link),
                false,
                now + Duration::from_secs(3)
            )
            .is_empty()
    );
}

#[test]
fn successful_cache_ttl_does_not_poll_at_idle_and_failed_attempts_do_not_retry() {
    let now = Instant::now();
    let device = target("a", 1, "board");
    let receiver = device.receiver.clone();
    let link = TrackerLink {
        address: device.address.clone(),
        ..query_link("a", 1)
    };
    let mut query = seeded_query(
        std::slice::from_ref(&receiver),
        std::slice::from_ref(&link),
        now,
    );
    query.mark_queried(&device, now);
    assert!(
        query
            .take_incremental(
                std::iter::once(&receiver),
                std::iter::once(&link),
                false,
                now + Duration::from_secs(60)
            )
            .is_empty()
    );
    query.enter(false);
    query.enter(true);
    assert!(!query.take_ready(true, false, now + Duration::from_secs(60)));
    assert!(query.take_ready(true, false, now + Duration::from_secs(61)));
    let groups = query.take_incremental(
        std::iter::once(&receiver),
        std::iter::once(&link),
        false,
        now + Duration::from_secs(61),
    );
    assert_eq!(groups.len(), 1);
    assert!(
        !groups[0].2,
        "failed receiver attempt is not retried on entry"
    );
    assert_eq!(groups[0].1[0].slot, 1);
    query.enter(false);
    query.enter(true);
    query.take_ready(true, false, now + Duration::from_secs(90));
    assert!(query.take_ready(true, false, now + Duration::from_secs(91)));
    assert!(
        query
            .take_incremental(
                std::iter::once(&receiver),
                std::iter::once(&link),
                false,
                now + Duration::from_secs(91)
            )
            .is_empty()
    );
    let manual = available_query_groups(std::slice::from_ref(&receiver), std::iter::once(&link));
    assert!(manual[0].2);
    assert_eq!(manual[0].1.len(), 1);
    query.full_requested(&manual);
}

#[test]
fn cache_tracks_positive_identity_and_update_invalidates_only_affected_targets() {
    let now = Instant::now();
    let a = target("a", 1, "board");
    let b = target("a", 2, "board");
    let receiver = a.receiver.clone();
    let links = [
        TrackerLink {
            address: a.address.clone(),
            ..query_link("a", 1)
        },
        TrackerLink {
            address: b.address.clone(),
            ..query_link("a", 2)
        },
    ];
    let mut state = FlashState {
        esb_auto_query: seeded_query(std::slice::from_ref(&receiver), &links, now),
        ..FlashState::default()
    };
    state.esb_auto_query.mark_queried(&a, now);
    state.esb_auto_query.mark_queried(&b, now);
    state.esb_auto_query.observe(&receiver, &[], now);
    assert!(
        state.esb_auto_query.receivers[0]
            .trackers
            .iter()
            .all(|tracker| tracker.queried_at == Some(now))
    );
    state.prepare_action_results(&Action::EsbGroups(vec![OtaPlanGroup {
        targets: vec![a.clone()],
        image: local_image("firmware.hex"),
    }]));
    let groups = state.esb_auto_query.take_incremental(
        std::iter::once(&receiver),
        links.iter(),
        false,
        now + Duration::from_secs(2),
    );
    assert_eq!(groups.len(), 1);
    assert!(!groups[0].2);
    assert_eq!(groups[0].1.len(), 1);
    assert_eq!(groups[0].1[0].slot, a.id);
    assert_eq!(
        state.esb_auto_query.receivers[0].trackers[1].queried_at,
        Some(now)
    );
    let mut replaced = links.clone();
    replaced[1].address = Some("FFEEDDCCBBAA".into());
    state
        .esb_auto_query
        .observe(&receiver, &replaced, now + Duration::from_secs(3));
    assert!(
        state.esb_auto_query.receivers[0].trackers[1]
            .queried_at
            .is_none()
    );
    let groups = state.esb_auto_query.take_incremental(
        std::iter::once(&receiver),
        replaced.iter(),
        false,
        now + Duration::from_secs(5),
    );
    assert_eq!(groups[0].1[0].slot, b.id);
    let mut replacement_receiver = receiver;
    replacement_receiver.serial_number = Some("replacement".into());
    state
        .esb_auto_query
        .observe(&replacement_receiver, &links, now + Duration::from_secs(6));
    assert!(!state.esb_auto_query.receivers[0].attempted);
    assert!(
        state.esb_auto_query.receivers[0]
            .trackers
            .iter()
            .all(|tracker| tracker.queried_at.is_none())
    );
}

#[test]
fn fresh_cache_does_not_block_a_new_target_and_pending_leave_can_reschedule() {
    let now = Instant::now();
    let device = target("a", 1, "board");
    let receiver = device.receiver.clone();
    let first = TrackerLink {
        address: device.address.clone(),
        ..query_link("a", 1)
    };
    let mut state = FlashState {
        esb_auto_query: seeded_query(
            std::slice::from_ref(&receiver),
            std::slice::from_ref(&first),
            now,
        ),
        ..FlashState::default()
    };
    state.esb_auto_query.mark_queried(&device, now);
    let links = [first, query_link("a", 2)];
    state.esb_auto_query.observe(&receiver, &links, now);
    let groups = state.esb_auto_query.take_incremental(
        std::iter::once(&receiver),
        links.iter(),
        false,
        now + Duration::from_secs(2),
    );
    assert_eq!(groups.len(), 1);
    assert!(!groups[0].2);
    assert_eq!(groups[0].1.len(), 1);
    assert_eq!(groups[0].1[0].slot, 2);
    state.pending = Some(Action::QueryGroups(OtaQueryPlan {
        groups,
        automatic: true,
    }));
    assert!(state.enter_esb_page(false));
    assert!(state.pending.is_none());
    assert_eq!(
        state.esb_auto_query.receivers[0].trackers[0].queried_at,
        Some(now)
    );
    state.enter_esb_page(true);
    state
        .esb_auto_query
        .take_ready(true, false, now + Duration::from_secs(3));
    assert!(
        state
            .esb_auto_query
            .take_ready(true, false, now + Duration::from_secs(4))
    );
    let groups = state.esb_auto_query.take_incremental(
        std::iter::once(&receiver),
        links.iter(),
        false,
        now + Duration::from_secs(4),
    );
    assert_eq!(groups[0].1[0].slot, 2);
    assert_eq!(groups[0].1.len(), 1);
}

#[test]
fn information_query_covers_receivers_and_recent_trackers_without_selecting_updates() {
    let a = target("a", 0, "board");
    let b = target("b", 0, "board");
    let links = [
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
            online: false,
        },
        TrackerLink {
            receiver_key: "a".into(),
            slot: 1,
            address: None,
            online: true,
        },
    ];
    let receivers = [a.receiver.clone(), b.receiver.clone()];
    let groups = available_query_groups(&receivers, links.iter());
    assert_eq!(groups.len(), 2);
    assert_eq!(groups[0].1.len(), 1);
    assert_eq!(groups[0].1[0].slot, 0);
    assert!(groups[1].1.is_empty()); // Still query the receiver itself.
    let mut state = FlashState::default();
    state.apply_queries(OtaQueries {
        targets: vec![a.clone(), target("a", RECEIVER_ID, "receiver")],
        failures: vec![],
    });
    state.sync_tracker_selection(&Default::default(), links.iter());
    assert!(state.selected_targets.is_empty());
    let selected = ["tracker:a:0".to_string()].into();
    state.sync_tracker_selection(&selected, links.iter());
    assert_eq!(state.selected_targets, [ota_target_key(&a)].into());
    state.sync_tracker_selection(&Default::default(), links.iter());
    state.apply_queries(OtaQueries {
        targets: vec![a],
        failures: vec![],
    });
    assert!(state.selected_targets.is_empty()); // Refresh never rechecks a cleared target.
}

#[test]
fn failed_refresh_keeps_successful_information_selection_and_assignment() {
    let tracker = target("a", 0, "board");
    let key = ota_assignment_key(&tracker);
    let image = local_image("board.hex");
    let mut state = FlashState {
        targets: vec![tracker.clone()],
        tracker_selection: ["tracker:a:0".into()].into(),
        selected_targets: [ota_target_key(&tracker)].into(),
        assignments: [(key.clone(), image.clone())].into(),
        ..FlashState::default()
    };
    state.apply_queries(OtaQueries {
        targets: vec![target("a", RECEIVER_ID, "receiver")],
        failures: vec![("a:0".into(), "timeout".into())],
    });
    assert_eq!(state.targets.len(), 2);
    assert_eq!(state.selected_targets, [ota_target_key(&tracker)].into());
    assert!(Arc::ptr_eq(&state.assignments[&key].bytes, &image.bytes));
    assert!(
        validate_live_ota_targets(
            &state.targets,
            std::slice::from_ref(&tracker.receiver),
            std::iter::empty()
        )
        .is_err()
    );
}

#[test]
fn receiver_refresh_uses_per_route_expiry_and_directed_retry_bypasses_it() {
    let start = Instant::now();
    let expired = target("a", 0, "board");
    let fresh = target("a", 1, "board");
    let receiver = &fresh.receiver;
    let mut links = [
        TrackerLink {
            address: expired.address.clone(),
            ..query_link("a", 0)
        },
        TrackerLink {
            address: fresh.address.clone(),
            ..query_link("a", 1)
        },
        query_link("a", 2),
        TrackerLink {
            online: false,
            ..query_link("a", 3)
        },
        query_link("a", 4),
    ];
    let mut cache = seeded_query(std::slice::from_ref(receiver), &links[..4], start);
    cache.mark_queried(&expired, start);
    cache.mark_queried(&fresh, start + Duration::from_secs(2));
    cache.mark_queried(
        &target("a", RECEIVER_ID, "receiver"),
        start + Duration::from_secs(2),
    );
    // Equivalent address formatting must not invalidate an unexpired reply.
    links[1].address = Some("11:22:33:44:55:01".into());
    let now = start + Duration::from_secs(30);
    let (_, requested, include_receiver) = cache
        .scoped_query_group(receiver, receiver, None, &links, now)
        .unwrap();
    assert!(!include_receiver);
    assert_eq!(
        requested.iter().map(|link| link.slot).collect::<Vec<_>>(),
        [0, 2, 4],
        "TTL boundary, failed answer and new target are eligible; fresh and offline are not"
    );
    assert!(
        cache
            .scoped_query_group(receiver, receiver, None, &links[1..2], now)
            .is_none()
    );
    let (_, retried, include_receiver) = cache
        .scoped_query_group(receiver, receiver, Some(&links[1]), &links, now)
        .unwrap();
    assert!(!include_receiver);
    assert_eq!(retried[0].slot, fresh.id);
    links[1].address = Some("11:22:33:44:55:06".into());
    assert_eq!(
        cache
            .scoped_query_group(receiver, receiver, None, &links[1..2], now)
            .unwrap()
            .1[0]
            .slot,
        fresh.id,
        "a reused slot is not the same tracker"
    );
}

#[test]
fn new_arrival_queries_expired_reports_without_retrying_a_failure() {
    let now = Instant::now();
    let stale = target("a", 0, "board");
    let recent = target("a", 1, "board");
    let receiver = &recent.receiver;
    let links = [
        TrackerLink {
            address: stale.address.clone(),
            ..query_link("a", 0)
        },
        TrackerLink {
            address: recent.address.clone(),
            ..query_link("a", 1)
        },
        query_link("a", 2),
        query_link("a", 3),
    ];
    let mut cache = seeded_query(std::slice::from_ref(receiver), &links[..3], now);
    cache.mark_queried(&stale, now);
    cache.mark_queried(&recent, now + Duration::from_secs(4));
    cache.observe(receiver, &links, now + Duration::from_secs(29));
    let groups = cache.take_incremental(
        std::iter::once(receiver),
        links.iter(),
        false,
        now + Duration::from_secs(30),
    );
    assert_eq!(groups.len(), 1);
    assert!(!groups[0].2, "a previous receiver failure is not retried");
    assert_eq!(
        groups[0].1.iter().map(|link| link.slot).collect::<Vec<_>>(),
        [stale.id, 3]
    );
}

#[gpui_kit::test]
fn staggered_query_handoff_and_reentry_keep_fresh_reports(cx: &mut gpui_kit::TestAppContext) {
    use crate::app_runtime::AppRuntime;
    use crate::application::FeaturePage;
    use crate::application::flash::{TrackerSection, tracker_section};
    use crate::application::test_support::offline_workbench;
    use crate::discovery::ReceiverMonitor;
    use gpui_kit::VisualContext as _;

    let _ui_session = crate::test_support::UiTestSession::new(cx);
    let runtime = AppRuntime::new().unwrap();
    let executor = runtime.executor();
    let cx = cx.add_empty_window();
    let workbench = cx.new_window_entity(move |window, cx| {
        offline_workbench(window, cx, crate::sessions::SessionManager::new(), executor)
    });
    workbench.update(cx, |this, _| {
        let first = target("a", 0, "board");
        let receiver = first.receiver.clone();
        let first_link = TrackerLink {
            address: first.address.clone(),
            ..query_link("a", 0)
        };
        let failed = query_link("a", 1);
        let late = query_link("a", 2);
        this.active_page = FeaturePage::Ota;
        this.inventory.receivers.push(receiver.clone());
        this.monitor =
            ReceiverMonitor::fixture(receiver.clone(), 0, first.address.as_deref().unwrap(), true);
        this.monitor
            .fixture_observation_mut(&receiver)
            .trackers
            .push(failed.clone());
        this.sync_esb_page();
        this.sync_flash_tracker_selection();
        let now = Instant::now();
        assert!(!this.poll_esb_queries(now));
        assert!(this.poll_esb_queries(now + Duration::from_secs(1)));
        assert!(this.flash.querying_tracker(&first_link));
        assert!(this.flash.querying_tracker(&failed));
        assert!(!this.flash.querying_tracker(&late));
        let Some(Action::QueryGroups(plan)) = this.flash.pending.take() else {
            panic!("initial automatic plan must reserve the receiver");
        };
        assert!(plan.groups[0].2);
        assert_eq!(plan.groups[0].1.len(), 2);
        this.release_flash_lease();
        this.flash.finish_job_progress(None);
        this.flash.apply_current_queries(
            OtaQueries {
                targets: vec![target("a", RECEIVER_ID, "receiver"), first.clone()],
                failures: vec![query_failure(&receiver, failed.slot, "timeout".into())],
            },
            &this.inventory.receivers,
        );

        this.monitor.fixture_observation_mut(&receiver).trackers = vec![first_link.clone()];
        this.query_flash_group(&receiver, None);
        assert!(
            this.flash.pending.is_none(),
            "a fresh receiver-wide refresh takes no lease"
        );

        this.flash.enter_esb_page(false);
        this.flash.enter_esb_page(true);
        this.monitor.fixture_observation_mut(&receiver).trackers =
            vec![first_link.clone(), failed, late.clone()];
        this.sync_flash_tracker_selection();
        let reentry = Instant::now();
        assert!(!this.poll_esb_queries(reentry));
        assert!(this.poll_esb_queries(reentry + Duration::from_secs(1)));
        let Some(Action::QueryGroups(plan)) = &this.flash.pending else {
            panic!("reentry must query the newly discovered tracker");
        };
        assert!(!plan.groups[0].2);
        assert_eq!(plan.groups[0].1.len(), 1);
        assert_eq!(plan.groups[0].1[0].slot, late.slot);
        assert_eq!(plan.groups[0].1[0].address, late.address);
        this.sync_flash_tracker_selection();
        let retained = &this.flash.tracker_snapshot[&receiver.key][0];
        assert_eq!(
            tracker_section(
                retained,
                this.flash.tracker_info(retained, &receiver),
                this.flash.querying_tracker(retained),
                false,
            ),
            TrackerSection::Reported("1", "today"),
            "lease handoff must not turn the previously read tracker into a reading card"
        );
        assert!(this.flash.querying_tracker(&late));
        assert_eq!(this.flash.query_failures["a:1"], "timeout");
        assert_eq!(this.flash.targets.len(), 2);
        assert!(this.flash.enter_esb_page(false));
        this.release_flash_lease();
    });
}
