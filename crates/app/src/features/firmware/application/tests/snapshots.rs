use super::support::target;
use crate::application::flash::{
    Action, FlashState, merge_tracker_snapshot, ota_target_key, ota_tracker_activity_tps,
};
use crate::domain::TrackerLink;

#[test]
fn display_snapshot_survives_monitor_handoff_without_claiming_live_routes() {
    let original = TrackerLink {
        receiver_key: "a".into(),
        slot: 0,
        address: Some("112233445566".into()),
        online: true,
    };
    let mut snapshot = vec![original.clone()];
    merge_tracker_snapshot(&mut snapshot, &[]);
    assert_eq!(snapshot[0].address, original.address);
    assert!(!snapshot[0].online);
    let mut unknown = original.clone();
    unknown.address = None;
    merge_tracker_snapshot(&mut snapshot, &[unknown]);
    assert_eq!(snapshot[0].address, original.address);
    assert!(!snapshot[0].online);
    let mut replacement = original;
    replacement.address = Some("AABBCCDDEEFF".into());
    merge_tracker_snapshot(&mut snapshot, &[replacement.clone()]);
    assert_eq!(snapshot.len(), 1);
    assert_eq!(snapshot[0].address, replacement.address);
    assert!(snapshot[0].online);
}

#[test]
fn ota_activity_requires_rediscovered_identity_after_monitor_handoff() {
    let original = TrackerLink {
        receiver_key: "a:receiver".into(),
        slot: 0,
        address: Some("AABBCCDDEEFF".into()),
        online: true,
    };
    let mut snapshot = vec![original.clone()];
    let mut current = [original.clone()];
    merge_tracker_snapshot(&mut snapshot, &[]);
    assert_eq!(ota_tracker_activity_tps(&snapshot[0], &[], 90), 0);

    // Sensor traffic resumes before discovery has supplied the RF address.
    current[0].address = None;
    merge_tracker_snapshot(&mut snapshot, &current);
    assert!(!snapshot[0].online);
    assert_eq!(snapshot[0].address, original.address);
    assert_eq!(ota_tracker_activity_tps(&snapshot[0], &current, 90), 0);

    // Positive rediscovery may use a different accepted address spelling.
    current[0].address = Some("aa:bb:cc:dd:ee:ff".into());
    assert_eq!(ota_tracker_activity_tps(&snapshot[0], &current, 90), 90);
    merge_tracker_snapshot(&mut snapshot, &current);
    assert!(snapshot[0].online);
    assert_eq!(ota_tracker_activity_tps(&snapshot[0], &current, 45), 45);

    // Reassignment cannot animate the still-retained old identity.
    current[0].address = Some("112233445566".into());
    assert_eq!(ota_tracker_activity_tps(&snapshot[0], &current, 75), 0);
    merge_tracker_snapshot(&mut snapshot, &current);
    assert_eq!(snapshot[0].address, current[0].address);
    assert_eq!(ota_tracker_activity_tps(&snapshot[0], &current, 75), 75);

    current[0].online = false;
    assert_eq!(ota_tracker_activity_tps(&snapshot[0], &current, 75), 0);
    current[0].online = true;
    current[0].receiver_key = "b:receiver".into();
    assert_eq!(ota_tracker_activity_tps(&snapshot[0], &current, 75), 0);
    current[0].receiver_key = original.receiver_key;
    current[0].slot = 1;
    assert_eq!(ota_tracker_activity_tps(&snapshot[0], &current, 75), 0);
    current[0].slot = 0;
    current[0].address = Some("not an RF address".into());
    snapshot[0].address = current[0].address.clone();
    assert_eq!(ota_tracker_activity_tps(&snapshot[0], &current, 75), 0);
    snapshot[0].address = None;
    assert_eq!(ota_tracker_activity_tps(&snapshot[0], &current, 75), 0);
}

#[test]
fn snapshot_merge_keeps_slot_order_across_partial_discovery_and_restart() {
    let link = |slot| TrackerLink {
        receiver_key: "a".into(),
        slot,
        address: Some(format!("1122334455{slot:02X}")),
        online: true,
    };
    let mut snapshot = Vec::new();
    merge_tracker_snapshot(&mut snapshot, &[link(6), link(4), link(7), link(5)]);
    assert_eq!(
        snapshot
            .iter()
            .map(|tracker| tracker.slot)
            .collect::<Vec<_>>(),
        [4, 5, 6, 7]
    );
    merge_tracker_snapshot(
        &mut snapshot,
        &[link(0), link(1), link(2), link(8), link(3)],
    );
    let expected = (0..=8).collect::<Vec<_>>();
    assert_eq!(
        snapshot
            .iter()
            .map(|tracker| tracker.slot)
            .collect::<Vec<_>>(),
        expected
    );
    merge_tracker_snapshot(&mut snapshot, &[]);
    merge_tracker_snapshot(&mut snapshot, &[link(8), link(3), link(0)]);
    assert_eq!(
        snapshot
            .iter()
            .map(|tracker| tracker.slot)
            .collect::<Vec<_>>(),
        expected
    );
    assert_eq!(
        snapshot
            .iter()
            .filter(|tracker| tracker.online)
            .map(|tracker| tracker.slot)
            .collect::<Vec<_>>(),
        [0, 3, 8]
    );
}

#[test]
fn positive_receiver_identity_change_invalidates_cached_information_and_review() {
    let tracker = target("a", 0, "board");
    let mut state = FlashState {
        targets: vec![tracker.clone()],
        selected_targets: [ota_target_key(&tracker)].into(),
        tracker_snapshot: [("a".into(), Vec::new())].into(),
        review: Some(Action::EsbGroups(vec![])),
        ..FlashState::default()
    };
    assert!(!state.sync_receiver_identities(&[])); // Discovery handoff is not replacement.
    let mut changed = tracker.receiver.clone();
    changed.path.push(1);
    assert!(state.sync_receiver_identities(&[changed]));
    assert!(state.targets.is_empty());
    assert!(state.selected_targets.is_empty());
    assert!(state.tracker_snapshot.is_empty());
    assert!(state.review.is_none());
}
