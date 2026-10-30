use super::{
    CommandFocus, RemoteBatch, RemoteFocus, tracker_route_with_traffic, validate_tracker_focus,
};
use crate::application::devices::{
    clear_tracker_selection, live_tracker_port, tracker_selection_key,
};
use crate::{
    domain::{DeviceIdentity, DeviceRole, TrackerLink},
    features::commands::{
        batch::Outcome,
        policy::{CommandContext, CommandSnapshot, Preset, RouteEvidence, presets},
    },
    sessions::ConnectionState,
};
use std::collections::BTreeSet;

fn preset(id: &str) -> &'static Preset {
    presets(CommandContext::ForwardedTracker)
        .find(|preset| preset.id == id)
        .unwrap()
}

fn tracker(receiver: &str, slot: u8) -> TrackerLink {
    TrackerLink {
        receiver_key: receiver.into(),
        slot,
        address: Some(format!("AABBCCDDEE{slot:02X}")),
        online: true,
    }
}

fn focus(tracker: &TrackerLink) -> RemoteFocus {
    RemoteFocus::Tracker(CommandFocus::Tracker {
        receiver: tracker.receiver_key.clone(),
        slot: tracker.slot,
        address: tracker.address.clone(),
    })
}

fn route<'a>(
    tracker: &'a TrackerLink,
    port: &'a str,
    generation: u64,
) -> Result<RouteEvidence<'a>, String> {
    tracker_route_with_traffic(
        tracker,
        RouteEvidence::Forwarded {
            port,
            generation,
            connected: true,
            receiver_key: &tracker.receiver_key,
            slot: tracker.slot,
            address: tracker.address.as_deref(),
            unique_receiver_route: true,
        },
    )
}

fn capture(batch: &mut RemoteBatch, tracker: &TrackerLink, port: &str, command: &'static Preset) {
    batch.capture_target(
        focus(tracker),
        format!("{} · Tracker {}", tracker.receiver_key, tracker.slot),
        command,
        route(tracker, port, 4),
        None,
    );
}

#[test]
fn selected_tracker_batch_submits_each_slot_once_on_shared_port() {
    let roster = [tracker("receiver-a", 2), tracker("receiver-a", 7)];
    let mut batch = RemoteBatch::default();
    for tracker in &roster {
        capture(&mut batch, tracker, "COM7", preset("remote-sens"));
    }
    let mut writes = Vec::new();
    let run = batch.execute("Sensitivity".into(), Ok(()), |target| {
        let RemoteFocus::Tracker(CommandFocus::Tracker { slot, .. }) = &target.focus else {
            panic!("Expected explicit tracker");
        };
        let current = roster.iter().find(|tracker| tracker.slot == *slot).unwrap();
        target
            .snapshot
            .authorize(route(current, "COM7", 4)?, true)?;
        writes.push((
            target.snapshot.port().to_owned(),
            target.snapshot.command().to_owned(),
        ));
        Err("Queue full".into())
    });
    assert_eq!(
        writes,
        [
            ("COM7".into(), "send 2 sens 0,0,0".into()),
            ("COM7".into(), "send 7 sens 0,0,0".into())
        ]
    );
    assert_eq!(run.targets.len(), 2);
    assert!(!run.pending());
    assert!(
        run.targets
            .iter()
            .all(|target| target.outcome == Outcome::Failed("Queue full".into()))
    );
}

#[test]
fn same_slot_on_two_receivers_remains_two_independent_targets() {
    let mut batch = RemoteBatch::default();
    capture(
        &mut batch,
        &tracker("receiver-a", 2),
        "COM7",
        preset("remote-sens"),
    );
    capture(
        &mut batch,
        &tracker("receiver-b", 2),
        "COM8",
        preset("remote-sens"),
    );
    let mut writes = Vec::new();
    let run = batch.execute("Sensitivity".into(), Ok(()), |target| {
        writes.push((
            target.snapshot.port().to_owned(),
            target.snapshot.command().to_owned(),
        ));
        Err(format!("{} unavailable", target.snapshot.port()))
    });
    assert_eq!(
        writes,
        [
            ("COM7".into(), "send 2 sens 0,0,0".into()),
            ("COM8".into(), "send 2 sens 0,0,0".into())
        ]
    );
    assert_ne!(run.targets[0].label, run.targets[1].label);
    assert_ne!(run.targets[0].outcome, run.targets[1].outcome);
}

#[test]
fn parameter_batch_keeps_original_selection_roster_and_route() {
    let mut roster = vec![tracker("receiver-a", 2)];
    let mut selection = BTreeSet::from([tracker_selection_key(&roster[0])]);
    let mut batch = RemoteBatch::default();
    for tracker in roster
        .iter()
        .filter(|tracker| selection.contains(&tracker_selection_key(tracker)))
    {
        capture(&mut batch, tracker, "COM7", preset("sens-auto"));
    }
    clear_tracker_selection(&mut selection);
    roster.push(tracker("receiver-b", 8));
    roster[0].address = Some("112233445566".into());
    selection.extend(roster.iter().map(tracker_selection_key));
    let batch = batch.with_parameter("x 10").unwrap();
    assert_eq!(batch.targets.len(), 1);
    assert_eq!(batch.targets[0].snapshot.port(), "COM7");
    assert_eq!(batch.targets[0].snapshot.command(), "send 2 sens auto x 10");
    let RemoteFocus::Tracker(frozen) = &batch.targets[0].focus else {
        panic!("Tracker");
    };
    assert!(validate_tracker_focus(frozen, &roster[0]).is_err());
    assert!(
        batch.targets[0]
            .snapshot
            .authorize(route(&roster[0], "COM7", 4).unwrap(), true)
            .is_err()
    );
}

#[test]
fn parameter_batch_rejects_reconnection_without_retargeting() {
    let tracker = tracker("receiver-a", 2);
    let mut batch = RemoteBatch::default();
    capture(&mut batch, &tracker, "COM7", preset("sens-auto"));
    let batch = batch.with_parameter("x").unwrap();
    assert_eq!(batch.targets[0].snapshot.command(), "send 2 sens auto x");
    let mut writes = 0;
    let run = batch.execute("Automatic sensitivity".into(), Ok(()), |target| {
        target
            .snapshot
            .authorize(route(&tracker, "COM7", 5)?, true)?;
        writes += 1;
        Err("Should not enqueue".into())
    });
    assert_eq!(writes, 0);
    assert!(!run.pending());
    assert!(matches!(run.targets[0].outcome, Outcome::Failed(_)));
}

#[test]
fn mixed_routes_keep_exclusions_and_only_submit_available_targets() {
    let mut batch = RemoteBatch::default();
    let available = tracker("receiver-a", 2);
    capture(&mut batch, &available, "COM7", preset("remote-sens"));
    batch.capture_target(
        focus(&tracker("receiver-b", 3)),
        "Receiver B · Tracker 3".into(),
        preset("remote-sens"),
        Err("Connect the receiver to send commands".into()),
        None,
    );
    assert_eq!(batch.targets.len(), 1);
    assert_eq!(batch.excluded.len(), 1);
    let mut writes = 0;
    let run = batch.execute("Sensitivity".into(), Ok(()), |_| {
        writes += 1;
        Err("Queue full".into())
    });
    assert_eq!(writes, 1);
    assert_eq!(run.targets.len(), 2);
    assert_eq!(
        run.targets[1].outcome,
        Outcome::Failed("Excluded: Connect the receiver to send commands".into())
    );
    assert!(!run.pending());
}

#[test]
fn clear_tracker_selection_preserves_other_namespaces() {
    let unrelated = BTreeSet::from(["port:COM7".to_owned()]);
    let mut selected = unrelated.clone();
    selected.insert(tracker_selection_key(&tracker("receiver-a", 2)));
    selected.insert(tracker_selection_key(&tracker("receiver-b", 2)));
    clear_tracker_selection(&mut selected);
    assert_eq!(selected, unrelated);
}

#[test]
fn no_recent_data_excludes_wireless_candidate_from_frozen_batch() {
    let mut silent = tracker("receiver-a", 2);
    silent.online = false;
    assert!(
        route(&silent, "COM7", 4)
            .and_then(RouteEvidence::check)
            .is_err()
    );
    let mut batch = RemoteBatch::default();
    capture(&mut batch, &silent, "COM7", preset("remote-sens"));
    assert!(batch.targets.is_empty());
    assert_eq!(batch.excluded.len(), 1);
    let mut writes = 0;
    let run = batch.execute("Sensitivity".into(), Ok(()), |_| {
        writes += 1;
        Err("Should not enqueue".into())
    });
    assert_eq!(writes, 0);
    assert!(matches!(run.targets[0].outcome, Outcome::Failed(_)));
}

#[test]
fn frozen_wireless_target_rejects_expired_traffic_before_sending() {
    let mut tracker = tracker("receiver-a", 2);
    let mut batch = RemoteBatch::default();
    capture(&mut batch, &tracker, "COM7", preset("remote-sens"));
    assert_eq!(batch.targets.len(), 1);
    tracker.online = false;
    let mut writes = 0;
    let run = batch.execute("Sensitivity".into(), Ok(()), |target| {
        target
            .snapshot
            .authorize(route(&tracker, "COM7", 4)?, true)?;
        writes += 1;
        Err("Should not enqueue".into())
    });
    assert_eq!(writes, 0);
    assert!(matches!(run.targets[0].outcome, Outcome::Failed(_)));
}

#[test]
fn connected_direct_tracker_does_not_require_recent_hid_traffic() {
    let mut tracker = tracker("receiver-a", 2);
    tracker.online = false;
    let identity = DeviceIdentity {
        address: tracker.address.clone(),
        role: DeviceRole::Tracker,
        ..Default::default()
    };
    let port =
        live_tracker_port(&tracker, [("COM8", ConnectionState::Connected, &identity)]).unwrap();
    let direct = RouteEvidence::Direct {
        port,
        generation: 4,
        connected: true,
        role: DeviceRole::Tracker,
    };
    let info = presets(CommandContext::Direct(DeviceRole::Tracker))
        .find(|preset| preset.id == "info")
        .unwrap();
    let snapshot = CommandSnapshot::capture(
        info,
        "",
        tracker_route_with_traffic(&tracker, direct).unwrap(),
    )
    .unwrap();
    assert!(
        snapshot
            .authorize(tracker_route_with_traffic(&tracker, direct).unwrap(), false,)
            .is_ok()
    );
    assert_eq!(snapshot.port(), "COM8");
    assert_eq!(snapshot.command(), "info");
}

#[test]
fn receiver_wide_batch_with_no_local_active_targets_sends_one_broadcast() {
    let mut batch = RemoteBatch::default();
    batch.capture_target(
        RemoteFocus::All("receiver-a".into()),
        "Receiver A".into(),
        preset("remote-sens"),
        Ok(RouteEvidence::Broadcast {
            port: "COM7",
            generation: 4,
            connected: true,
            receiver_key: "receiver-a",
            unique_receiver_route: true,
        }),
        None,
    );
    let mut writes = Vec::new();
    batch.execute("Sensitivity".into(), Ok(()), |target| {
        writes.push((
            target.snapshot.port().to_owned(),
            target.snapshot.command().to_owned(),
        ));
        Err("Queue full".into())
    });
    assert_eq!(writes, [("COM7".into(), "send all sens 0,0,0".into())]);
}

#[test]
fn selected_tracker_batch_rejects_receiver_wide_commands_without_broadcasting() {
    let roster = [tracker("receiver-a", 2), tracker("receiver-a", 7)];
    for id in ["tracker-channel", "tracker-clearchannel"] {
        let mut batch = RemoteBatch::default();
        for tracker in &roster {
            capture(&mut batch, tracker, "COM7", preset(id));
        }
        let parameter = if id == "tracker-channel" { "100" } else { "" };
        let batch = batch.with_parameter(parameter).unwrap();
        assert!(batch.targets.is_empty());
        assert_eq!(batch.excluded.len(), roster.len());
        let mut writes = 0;
        let run = batch.execute(id.into(), Ok(()), |_| {
            writes += 1;
            Err("Should not enqueue".into())
        });
        assert_eq!(writes, 0);
        assert_eq!(run.targets.len(), roster.len());
        assert!(
            run.targets
                .iter()
                .all(|target| matches!(target.outcome, Outcome::Failed(_)))
        );
    }
}

fn broadcast<'a>(port: &'a str, generation: u64, receiver: &'a str) -> RouteEvidence<'a> {
    RouteEvidence::Broadcast {
        port,
        generation,
        connected: true,
        receiver_key: receiver,
        unique_receiver_route: true,
    }
}

#[test]
fn explicit_receiver_wide_channel_batch_keeps_scope_when_parameters_change() {
    for (id, parameter, command) in [
        ("tracker-channel", "100", "send all channel 100"),
        ("tracker-clearchannel", "", "send all clearchannel"),
    ] {
        let mut batch = RemoteBatch::default();
        batch.capture_target(
            RemoteFocus::All("receiver-a".into()),
            "Receiver A".into(),
            preset(id),
            Ok(broadcast("COM7", 4, "receiver-a")),
            None,
        );
        let batch = batch.with_parameter(parameter).unwrap();
        assert_eq!(batch.targets.len(), 1);
        assert!(batch.excluded.is_empty());
        let mut writes = Vec::new();
        let run = batch.execute(id.into(), Ok(()), |target| {
            assert!(
                matches!(&target.focus, RemoteFocus::All(receiver) if receiver == "receiver-a")
            );
            assert!(
                target
                    .snapshot
                    .authorize(broadcast("COM7", 4, "receiver-a"), false)
                    .is_err()
            );
            target
                .snapshot
                .authorize(broadcast("COM7", 4, "receiver-a"), true)?;
            writes.push(target.snapshot.command().to_owned());
            Err("Queue full".into())
        });
        assert_eq!(writes, [command]);
        assert_eq!(run.targets.len(), 1);
    }
}

#[test]
fn receiver_wide_channel_batch_reauthorizes_frozen_route_before_write() {
    let tracker = tracker("receiver-a", 2);
    for current in [
        broadcast("COM7", 5, "receiver-a"),
        broadcast("COM8", 4, "receiver-a"),
        broadcast("COM7", 4, "receiver-b"),
        RouteEvidence::Broadcast {
            port: "COM7",
            generation: 4,
            connected: false,
            receiver_key: "receiver-a",
            unique_receiver_route: true,
        },
        RouteEvidence::Broadcast {
            port: "COM7",
            generation: 4,
            connected: true,
            receiver_key: "receiver-a",
            unique_receiver_route: false,
        },
        route(&tracker, "COM7", 4).unwrap(),
    ] {
        for id in ["tracker-channel", "tracker-clearchannel"] {
            let mut batch = RemoteBatch::default();
            batch.capture_target(
                RemoteFocus::All("receiver-a".into()),
                "Receiver A".into(),
                preset(id),
                Ok(broadcast("COM7", 4, "receiver-a")),
                None,
            );
            let mut writes = 0;
            let run = batch.execute(id.into(), Ok(()), |target| {
                target.snapshot.authorize(current, true)?;
                writes += 1;
                Err("Should not enqueue".into())
            });
            assert_eq!(writes, 0);
            assert_eq!(run.targets.len(), 1);
            assert!(matches!(run.targets[0].outcome, Outcome::Failed(_)));
        }
    }
}

#[test]
fn late_calibration_conflict_rejects_whole_remote_batch_before_enqueue() {
    for blocked in [false, true] {
        let mut batch = RemoteBatch::default();
        capture(
            &mut batch,
            &tracker("receiver-a", 2),
            "COM7",
            preset("remote-sens"),
        );
        capture(
            &mut batch,
            &tracker("receiver-b", 3),
            "COM8",
            preset("remote-sens"),
        );
        let admission = batch.authorize_all(|target| {
            if blocked && target.snapshot.port() == "COM8" {
                Err("Calibration is observing receiver-b".into())
            } else {
                Ok(())
            }
        });
        let mut writes = Vec::new();
        let run = batch.execute("Sensitivity".into(), admission, |target| {
            writes.push((
                target.snapshot.port().to_owned(),
                target.snapshot.command().to_owned(),
            ));
            Err("Queue full".into())
        });
        if blocked {
            assert_eq!(writes, []);
            assert_eq!(
                run.targets
                    .iter()
                    .map(|target| &target.outcome)
                    .collect::<Vec<_>>(),
                [
                    &Outcome::Failed("Calibration is observing receiver-b".into()),
                    &Outcome::Failed("Calibration is observing receiver-b".into())
                ],
            );
        } else {
            assert_eq!(
                writes,
                [
                    ("COM7".into(), "send 2 sens 0,0,0".into()),
                    ("COM8".into(), "send 3 sens 0,0,0".into()),
                ]
            );
        }
    }
}
