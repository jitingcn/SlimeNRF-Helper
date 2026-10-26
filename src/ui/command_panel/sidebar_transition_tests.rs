use super::{
    CommandConnection, CommandFocus, CommandPreparation, CommandScope, CommandSidebar,
    PendingCommand, PrepareAction, PrepareTarget, PrepareTargetSource, PreparedRouteKind,
    SerialReview, same_serial_endpoint,
};
use crate::domain::{DeviceIdentity, DeviceRole, SerialEndpoint, TrackerLink};
use crate::operations::{BatchSnapshot, Target};
use crate::sessions::ConnectionState;
use crate::ui::{
    FeaturePage,
    devices::{
        prune_unavailable_device_trackers, set_device_selection, tracker_selectable,
        tracker_selection_key,
    },
};
use std::collections::BTreeSet;

fn reviewing(scope: CommandScope) -> CommandSidebar {
    CommandSidebar {
        scope,
        history: false,
        pending: Some(PendingCommand::Serial(SerialReview {
            snapshot: BatchSnapshot {
                command: "info".into(),
                targets: vec![Target {
                    port: "COM7".into(),
                    generation: 4,
                }],
                excluded: Vec::new(),
            },
            guards: Vec::new(),
        })),
        ..CommandSidebar::default()
    }
}

fn connecting(scope: CommandScope) -> CommandSidebar {
    CommandSidebar {
        scope,
        history: false,
        pending: Some(PendingCommand::Connecting(CommandPreparation {
            action: PrepareAction::Serial {
                command: "info".into(),
            },
            targets: vec![PrepareTarget {
                source: PrepareTargetSource::Current(CommandFocus::Serial("COM7".into())),
                label: "Device A".into(),
                port: "COM7".into(),
                endpoint: None,
                route_kind: PreparedRouteKind::Direct,
            }],
            excluded: Vec::new(),
            connections: vec![CommandConnection {
                port: "COM7".into(),
                generation: Some(4),
            }],
            error: None,
        })),
        ..CommandSidebar::default()
    }
}

fn card_tracker(slot: u8, online: bool) -> TrackerLink {
    TrackerLink {
        receiver_key: "receiver-a".into(),
        slot,
        address: Some(format!("AABBCCDDEE{slot:02X}")),
        online,
    }
}

fn card_focus(tracker: &TrackerLink) -> Option<CommandFocus> {
    Some(CommandFocus::Tracker {
        receiver: tracker.receiver_key.clone(),
        slot: tracker.slot,
        address: tracker.address.clone(),
    })
}

#[test]
fn offline_card_cannot_enter_batch_selection_or_change_it() {
    let tracker = card_tracker(0, false);
    let key = tracker_selection_key(&tracker);
    let mut selected = BTreeSet::from(["port:COM7".to_owned()]);
    let before = selected.clone();
    let available = tracker_selectable(&tracker, []);
    assert!(!set_device_selection(&mut selected, &key, true, available));
    assert!(!set_device_selection(&mut selected, &key, false, available));
    assert_eq!(selected, before);
    let online = card_tracker(0, true);
    assert!(set_device_selection(
        &mut selected,
        &key,
        true,
        tracker_selectable(&online, []),
    ));
    assert!(selected.contains(&key));
}

#[test]
fn liveness_loss_prunes_only_unavailable_cards_and_cancels_pending_commands() {
    for mut sidebar in [
        reviewing(CommandScope::SelectedTrackers),
        connecting(CommandScope::SelectedTrackers),
    ] {
        let mut roster = [card_tracker(0, true), card_tracker(1, true)];
        let key = tracker_selection_key(&roster[0]);
        let retained = tracker_selection_key(&roster[1]);
        let mut selected = BTreeSet::from([key.clone(), retained.clone(), "port:COM7".to_owned()]);
        let mut focus = card_focus(&roster[0]);
        assert!(!prune_unavailable_device_trackers(
            FeaturePage::DeviceSetup,
            &mut selected,
            &mut focus,
            roster.iter(),
            |tracker| tracker_selectable(tracker, []),
        ));
        assert!(sidebar.pending.is_some());
        roster[0].online = false;
        if prune_unavailable_device_trackers(
            FeaturePage::DeviceSetup,
            &mut selected,
            &mut focus,
            roster.iter(),
            |tracker| tracker_selectable(tracker, []),
        ) {
            sidebar.selection_changed(selected.iter().any(|key| key.starts_with("tracker:")));
        }
        assert_eq!(selected, BTreeSet::from([retained, "port:COM7".to_owned()]));
        assert_eq!(focus, card_focus(&roster[0]));
        assert!(sidebar.pending.is_none());
        assert_eq!(sidebar.scope, CommandScope::SelectedTrackers);
    }
}

#[test]
fn pruning_preserves_other_focus_and_clears_missing_focused_tracker() {
    let offline = card_tracker(0, false);
    let online = card_tracker(1, true);
    for original in [
        card_focus(&online),
        Some(CommandFocus::Receiver("receiver-a".into())),
        Some(CommandFocus::Serial("COM7".into())),
    ] {
        let mut selected = BTreeSet::from([tracker_selection_key(&offline)]);
        let mut focus = original.clone();
        assert!(prune_unavailable_device_trackers(
            FeaturePage::DeviceSetup,
            &mut selected,
            &mut focus,
            [&offline, &online].into_iter(),
            |tracker| tracker_selectable(tracker, []),
        ));
        assert_eq!(focus, original);
    }
    let mut selected = BTreeSet::new();
    let mut focus = card_focus(&offline);
    assert!(prune_unavailable_device_trackers(
        FeaturePage::DeviceSetup,
        &mut selected,
        &mut focus,
        [].into_iter(),
        |tracker| tracker_selectable(tracker, []),
    ));
    assert!(focus.is_none());
}

#[test]
fn connected_direct_card_survives_silence_but_ambiguous_address_leaves_batch() {
    let tracker = card_tracker(0, false);
    let identity = DeviceIdentity {
        role: DeviceRole::Tracker,
        address: tracker.address.clone(),
        ..Default::default()
    };
    let key = tracker_selection_key(&tracker);
    let mut selected = BTreeSet::new();
    let available = tracker_selectable(&tracker, [("COM8", ConnectionState::Connected, &identity)]);
    assert!(set_device_selection(&mut selected, &key, true, available));
    let mut focus = card_focus(&tracker);
    assert!(!prune_unavailable_device_trackers(
        FeaturePage::DeviceSetup,
        &mut selected,
        &mut focus,
        [&tracker].into_iter(),
        |tracker| tracker_selectable(tracker, [("COM8", ConnectionState::Connected, &identity)],),
    ));
    assert_eq!(focus, card_focus(&tracker));
    let ambiguous = [
        ("COM8", ConnectionState::Connected, &identity),
        ("COM9", ConnectionState::Connected, &identity),
    ];
    assert!(prune_unavailable_device_trackers(
        FeaturePage::DeviceSetup,
        &mut selected,
        &mut focus,
        [&tracker].into_iter(),
        |tracker| tracker_selectable(tracker, ambiguous),
    ));
    assert!(selected.is_empty());
    assert_eq!(focus, card_focus(&tracker));
    assert!(!set_device_selection(
        &mut selected,
        &key,
        true,
        tracker_selectable(&tracker, ambiguous),
    ));
}

#[test]
fn offline_identity_focus_does_not_admit_batch_selection() {
    let tracker = card_tracker(0, false);
    let mut selected = BTreeSet::new();
    let mut focus = card_focus(&tracker);
    assert!(!prune_unavailable_device_trackers(
        FeaturePage::DeviceSetup,
        &mut selected,
        &mut focus,
        [&tracker].into_iter(),
        |tracker| tracker_selectable(tracker, []),
    ));
    assert_eq!(focus, card_focus(&tracker));
    assert!(!set_device_selection(
        &mut selected,
        &tracker_selection_key(&tracker),
        true,
        tracker_selectable(&tracker, []),
    ));
    assert!(selected.is_empty());
}

#[test]
fn replacement_at_same_receiver_slot_revokes_old_focus() {
    let original = card_tracker(2, true);
    let mut replacement = original.clone();
    replacement.address = Some("112233445566".into());
    let mut selected = BTreeSet::new();
    let mut focus = card_focus(&original);
    assert!(prune_unavailable_device_trackers(
        FeaturePage::DeviceSetup,
        &mut selected,
        &mut focus,
        [&replacement].into_iter(),
        |tracker| tracker_selectable(tracker, []),
    ));
    assert_eq!(focus, None);
    assert!(selected.is_empty());
}

#[test]
fn nickname_header_belongs_only_to_current_commands_tab() {
    let mut sidebar = CommandSidebar::default();
    assert!(sidebar.shows_current_device());
    sidebar.select_tab(true);
    assert!(!sidebar.shows_current_device());
    sidebar.select_tab(false);
    assert!(sidebar.shows_current_device());
    for scope in [
        CommandScope::SerialBatch,
        CommandScope::SelectedTrackers,
        CommandScope::AllTrackers("receiver-a".into()),
    ] {
        sidebar.change_scope(scope);
        assert!(!sidebar.shows_current_device());
    }
}

#[test]
fn device_availability_pruning_does_not_change_ota_selection_policy() {
    let tracker = card_tracker(0, false);
    let original = BTreeSet::from([tracker_selection_key(&tracker), "port:COM7".to_owned()]);
    for page in [
        FeaturePage::Home,
        FeaturePage::Ota,
        FeaturePage::Uf2,
        FeaturePage::Settings,
        FeaturePage::Tools,
    ] {
        let mut selected = original.clone();
        let mut focus = card_focus(&tracker);
        for roster in [vec![&tracker], vec![]] {
            assert!(!prune_unavailable_device_trackers(
                page,
                &mut selected,
                &mut focus,
                roster.into_iter(),
                |tracker| tracker_selectable(tracker, []),
            ));
            assert_eq!(selected, original);
            assert_eq!(focus, card_focus(&tracker));
        }
    }
}

#[test]
fn changing_focus_scope_or_selection_cancels_connection_continuation() {
    let mut sidebar = connecting(CommandScope::Current);
    sidebar.focus_changed();
    assert!(sidebar.pending.is_none());
    let mut sidebar = connecting(CommandScope::AllTrackers("receiver-a".into()));
    sidebar.change_scope(CommandScope::AllTrackers("receiver-b".into()));
    assert!(sidebar.pending.is_none());
    let mut sidebar = connecting(CommandScope::SelectedTrackers);
    sidebar.selection_changed(true);
    assert!(sidebar.pending.is_none());
    let mut sidebar = connecting(CommandScope::SerialBatch);
    sidebar.selection_changed(false);
    assert!(sidebar.pending.is_none());
}

#[test]
fn connecting_batch_history_retains_only_original_targets_and_cancel_discards_action() {
    let mut sidebar = connecting(CommandScope::SerialBatch);
    sidebar.select_tab(true);
    sidebar.select_tab(false);
    let Some(PendingCommand::Connecting(preparation)) = &sidebar.pending else {
        panic!("Expected original connection preparation");
    };
    assert_eq!(preparation.targets.len(), 1);
    assert_eq!(preparation.targets[0].port, "COM7");
    assert_eq!(preparation.connections.len(), 1);
    assert_eq!(preparation.connections[0].generation, Some(4));
    let PrepareAction::Serial { command } = &preparation.action else {
        panic!("Expected retained command");
    };
    assert_eq!(command, "info");
    // Same cancellation used by the Cancel button; no continuation remains to consume Opened.
    sidebar.pending = None;
    assert!(sidebar.pending.is_none());
}

#[test]
fn serial_endpoint_replacement_cannot_reuse_a_prepared_port_name() {
    let endpoint = SerialEndpoint {
        port_name: "COM7".into(),
        label: "Device A".into(),
        manufacturer: None,
        product: None,
        kind: crate::domain::SerialPortKind::Usb,
        serial_number: Some("A".into()),
        vid: Some(0x1209),
        pid: Some(0x7690),
        usb_interface: None,
    };
    let mut changed = endpoint.clone();
    changed.label = "Updated descriptive name".into();
    changed.manufacturer = Some("Updated manufacturer".into());
    changed.product = Some("Updated product".into());
    changed.kind = crate::domain::SerialPortKind::Unknown;
    assert!(same_serial_endpoint(Some(&endpoint), Some(&changed)));
    changed.serial_number = Some("B".into());
    assert!(!same_serial_endpoint(Some(&endpoint), Some(&changed)));
    assert!(!same_serial_endpoint(Some(&endpoint), None));
}

#[test]
fn changing_scope_discards_review_instead_of_relabeling_its_target() {
    for scope in [
        CommandScope::Current,
        CommandScope::SelectedTrackers,
        CommandScope::AllTrackers("receiver-a".into()),
    ] {
        let mut sidebar = reviewing(CommandScope::SerialBatch);
        sidebar.change_scope(scope.clone());
        assert_eq!(sidebar.scope, scope);
        assert!(sidebar.pending.is_none());
    }
    let mut sidebar = reviewing(CommandScope::AllTrackers("receiver-a".into()));
    sidebar.change_scope(CommandScope::AllTrackers("receiver-b".into()));
    assert!(sidebar.pending.is_none());
}

#[test]
fn changing_selection_cancels_review_even_within_the_same_scope() {
    let mut sidebar = reviewing(CommandScope::SelectedTrackers);
    sidebar.selection_changed(true);
    assert_eq!(sidebar.scope, CommandScope::SelectedTrackers);
    assert!(sidebar.pending.is_none());
    let mut sidebar = reviewing(CommandScope::SerialBatch);
    sidebar.selection_changed(false);
    assert!(sidebar.pending.is_none());
    let mut sidebar = reviewing(CommandScope::AllTrackers("receiver-a".into()));
    sidebar.selection_changed(true);
    assert_eq!(sidebar.scope, CommandScope::SelectedTrackers);
    assert!(sidebar.pending.is_none());
}

#[test]
fn ota_receiver_selection_changes_cancel_device_page_command_intents() {
    let tracker = crate::domain::TrackerLink {
        receiver_key: "receiver-a".into(),
        slot: 0,
        address: Some("112233445566".into()),
        online: true,
    };
    for mut sidebar in [
        reviewing(CommandScope::SelectedTrackers),
        connecting(CommandScope::SelectedTrackers),
    ] {
        let mut selected = [
            "tracker:receiver-a:0".to_string(),
            "tracker:receiver-b:0".to_string(),
        ]
        .into();
        // OTA's per-receiver clear changes the shared roster selection while
        // the command sidebar stays hidden; it must cancel, not retarget.
        crate::ui::flash::select_receiver_trackers(&mut selected, "receiver-a", &[], false);
        sidebar.selection_changed(selected.iter().any(|key| key.starts_with("tracker:")));
        assert!(!selected.contains("tracker:receiver-a:0"));
        assert!(selected.contains("tracker:receiver-b:0"));
        assert!(sidebar.pending.is_none());
        assert_eq!(sidebar.scope, CommandScope::SelectedTrackers);
        sidebar = reviewing(CommandScope::SelectedTrackers);
        crate::ui::flash::select_receiver_trackers(
            &mut selected,
            "receiver-a",
            std::slice::from_ref(&tracker),
            true,
        );
        sidebar.selection_changed(selected.iter().any(|key| key.starts_with("tracker:")));
        assert!(selected.contains("tracker:receiver-a:0"));
        assert!(sidebar.pending.is_none());
    }
}

#[test]
fn focusing_another_device_cancels_current_review() {
    let mut sidebar = reviewing(CommandScope::Current);
    sidebar.focus_changed();
    assert_eq!(sidebar.scope, CommandScope::Current);
    assert!(sidebar.pending.is_none());
}

#[test]
fn history_tab_preserves_frozen_review_without_recapturing_targets() {
    let mut sidebar = reviewing(CommandScope::SerialBatch);
    sidebar.select_tab(true);
    assert!(sidebar.history);
    sidebar.select_tab(false);
    let Some(PendingCommand::Serial(review)) = sidebar.pending else {
        panic!("History navigation must retain the pending review");
    };
    let snapshot = review.snapshot;
    assert_eq!(snapshot.command, "info");
    assert_eq!(
        snapshot.targets,
        [Target {
            port: "COM7".into(),
            generation: 4
        }]
    );
    assert_eq!(sidebar.scope, CommandScope::SerialBatch);
}

#[test]
fn command_groups_start_expanded_and_toggle_independently_per_sidebar() {
    let mut sidebar = CommandSidebar::default();
    let other_sidebar = CommandSidebar::default();
    assert!(sidebar.collapsed_groups.is_empty());
    sidebar.toggle_group("commands.group.system");
    sidebar.toggle_group("commands.group.information");
    sidebar.toggle_group("commands.group.system");
    assert_eq!(
        sidebar.collapsed_groups,
        BTreeSet::from(["commands.group.information"])
    );
    assert!(other_sidebar.collapsed_groups.is_empty());
    sidebar.toggle_group("commands.group.information");
    assert!(sidebar.collapsed_groups.is_empty());
}

#[test]
fn group_toggles_preserve_pending_review_preparation_scope_and_history() {
    for scope in [
        CommandScope::Current,
        CommandScope::SerialBatch,
        CommandScope::SelectedTrackers,
        CommandScope::AllTrackers("receiver-a".into()),
    ] {
        for history in [false, true] {
            for mut sidebar in [reviewing(scope.clone()), connecting(scope.clone())] {
                sidebar.select_tab(history);
                sidebar.toggle_group("commands.group.information");
                assert_eq!(sidebar.scope, scope);
                assert_eq!(sidebar.history, history);
                match sidebar.pending.as_ref() {
                    Some(PendingCommand::Serial(review)) => {
                        assert_eq!(review.snapshot.command, "info");
                        assert_eq!(
                            review.snapshot.targets,
                            [Target {
                                port: "COM7".into(),
                                generation: 4,
                            }]
                        );
                        assert!(review.snapshot.excluded.is_empty());
                        assert!(review.guards.is_empty());
                    }
                    Some(PendingCommand::Connecting(preparation)) => {
                        let PrepareAction::Serial { command } = &preparation.action else {
                            panic!("Group toggles must retain the prepared action");
                        };
                        assert_eq!(command, "info");
                        assert_eq!(preparation.targets.len(), 1);
                        let target = &preparation.targets[0];
                        assert!(matches!(
                            &target.source,
                            PrepareTargetSource::Current(CommandFocus::Serial(port))
                                if port == "COM7"
                        ));
                        assert_eq!(target.label, "Device A");
                        assert_eq!(target.port, "COM7");
                        assert!(target.endpoint.is_none());
                        assert!(matches!(target.route_kind, PreparedRouteKind::Direct));
                        assert_eq!(preparation.connections.len(), 1);
                        assert_eq!(preparation.connections[0].port, "COM7");
                        assert_eq!(preparation.connections[0].generation, Some(4));
                        assert!(preparation.excluded.is_empty());
                        assert!(preparation.error.is_none());
                    }
                    _ => panic!("Group toggles must retain the pending command"),
                }
            }
        }
    }
}

#[test]
fn collapsed_groups_survive_history_scope_selection_and_focus_transitions() {
    let mut sidebar = reviewing(CommandScope::Current);
    sidebar.toggle_group("commands.group.information");
    sidebar.toggle_group("commands.group.system");
    let collapsed = sidebar.collapsed_groups.clone();

    sidebar.select_tab(true);
    assert_eq!(sidebar.collapsed_groups, collapsed);
    assert!(sidebar.pending.is_some());
    sidebar.select_tab(false);
    assert_eq!(sidebar.collapsed_groups, collapsed);
    assert!(sidebar.pending.is_some());

    sidebar.change_scope(CommandScope::AllTrackers("receiver-a".into()));
    assert_eq!(sidebar.collapsed_groups, collapsed);
    assert!(sidebar.pending.is_none());
    sidebar.selection_changed(true);
    assert_eq!(sidebar.collapsed_groups, collapsed);
    assert_eq!(sidebar.scope, CommandScope::SelectedTrackers);
    sidebar.selection_changed(false);
    assert_eq!(sidebar.collapsed_groups, collapsed);
    assert_eq!(sidebar.scope, CommandScope::SerialBatch);
    sidebar.focus_changed();
    assert_eq!(sidebar.collapsed_groups, collapsed);
    assert_eq!(sidebar.scope, CommandScope::Current);

    sidebar.toggle_group("commands.group.system");
    assert_eq!(
        sidebar.collapsed_groups,
        BTreeSet::from(["commands.group.information"])
    );
}
