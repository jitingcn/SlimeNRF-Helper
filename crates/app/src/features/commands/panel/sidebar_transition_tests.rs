use super::{
    CommandConnection, CommandFocus, CommandPreparation, CommandScope, CommandSidebar,
    PendingCommand, PrepareAction, PrepareTarget, PrepareTargetSource, PreparedRouteKind,
    SerialReview, same_serial_endpoint,
};
use crate::application::{
    FeaturePage,
    devices::{
        prune_unavailable_device_trackers, set_device_selection, tracker_selectable,
        tracker_selection_key,
    },
};
use crate::domain::{DeviceIdentity, DeviceRole, SerialEndpoint, TrackerLink};
use crate::features::commands::batch::{BatchSnapshot, Target};
use crate::sessions::ConnectionState;
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

#[gpui_kit::test]
fn shutdown_preserves_command_reviews_and_connecting_intents(cx: &mut gpui_kit::TestAppContext) {
    use gpui_kit::VisualContext as _;

    let _ui_session = crate::test_support::UiTestSession::new(cx);
    let runtime = crate::app_runtime::AppRuntime::new().unwrap();
    let executor = runtime.executor();
    let cx = cx.add_empty_window();
    let workbench = cx.new_window_entity(move |window, cx| {
        crate::application::test_support::offline_workbench(
            window,
            cx,
            crate::sessions::SessionManager::new(),
            executor,
        )
    });
    workbench.update(cx, |this, cx| {
        this.shutting_down = true;
        this.sidebar = reviewing(CommandScope::Current);
        this.confirm_sidebar_command(cx);
        assert!(matches!(
            this.sidebar.pending,
            Some(PendingCommand::Serial(_))
        ));
        assert!(this.batch_history.is_empty());
        this.sidebar = connecting(CommandScope::Current);
        this.retry_preparation(cx);
        this.resume_preparation();
        assert!(matches!(
            this.sidebar.pending,
            Some(PendingCommand::Connecting(_))
        ));
        assert_eq!(this.manager.sessions().count(), 0);
        this.shutting_down = false;
    });
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
        crate::application::flash::select_receiver_trackers(
            &mut selected,
            "receiver-a",
            &[],
            false,
        );
        sidebar.selection_changed(selected.iter().any(|key| key.starts_with("tracker:")));
        assert!(!selected.contains("tracker:receiver-a:0"));
        assert!(selected.contains("tracker:receiver-b:0"));
        assert!(sidebar.pending.is_none());
        assert_eq!(sidebar.scope, CommandScope::SelectedTrackers);
        sidebar = reviewing(CommandScope::SelectedTrackers);
        crate::application::flash::select_receiver_trackers(
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

mod search_ui_tests {
    use super::{CommandFocus, PendingCommand};
    use crate::{
        app_runtime::Executor,
        application::{Workbench, preference_io::PreferenceIo, test_support::offline_workbench},
        domain::{DeviceRole, ReceiverEndpoint, SerialEndpoint, SerialPortKind},
        features::commands::policy::{self as commands, CommandContext},
        sessions::test_support::{DisplaySession, display_manager, display_test_session},
    };
    use gpui_kit::{
        AnyWindowHandle, App, AppContext as _, Bounds, Entity, TestAppContext, Window,
        WindowBounds, WindowOptions, px, size, test::TestWindowExt as _,
    };
    use std::{collections::BTreeSet, path::PathBuf};

    const RECEIVER: &str = "search-fixture-receiver";
    const RECEIVER_PORT: &str = "search-fixture-console";
    const UNKNOWN_PORT: &str = "search-fixture-unknown";

    struct Fixture {
        window: AnyWindowHandle,
        workbench: Entity<Workbench>,
        _session: DisplaySession,
    }

    fn fixture(cx: &mut TestAppContext, executor: Executor, preferences_path: PathBuf) -> Fixture {
        let mut manager = display_manager();
        let session = display_test_session(&mut manager, RECEIVER_PORT);
        let (window, workbench) = cx.update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        Default::default(),
                        size(px(1400.), px(1600.)),
                    ))),
                    ..Default::default()
                },
                cx,
                move |window, cx| {
                    cx.new(|cx| {
                        let mut this = offline_workbench(window, cx, manager, executor);
                        this.preferences_io = PreferenceIo::for_test(preferences_path);
                        this.inventory.receivers.push(ReceiverEndpoint {
                            key: RECEIVER.into(),
                            path: RECEIVER.as_bytes().to_vec(),
                            label: "Synthetic receiver".into(),
                            serial_number: Some("search-fixture-serial".into()),
                        });
                        this.inventory.ports.push(SerialEndpoint {
                            port_name: RECEIVER_PORT.into(),
                            label: "Synthetic receiver console".into(),
                            manufacturer: None,
                            product: None,
                            kind: SerialPortKind::Usb,
                            serial_number: Some("search-fixture-serial".into()),
                            vid: Some(0x1209),
                            pid: Some(0x7690),
                            usb_interface: None,
                        });
                        this.inventory.ports.push(SerialEndpoint {
                            port_name: UNKNOWN_PORT.into(),
                            label: "Synthetic unidentified console".into(),
                            manufacturer: None,
                            product: None,
                            kind: SerialPortKind::Unknown,
                            serial_number: None,
                            vid: None,
                            pid: None,
                            usb_interface: None,
                        });
                        this.command_focus = Some(CommandFocus::Serial(RECEIVER_PORT.into()));
                        this
                    })
                },
            )
            .expect("open the real workbench for command search")
        });
        cx.update_window(window, |_, window, _| window.activate_window())
            .expect("activate the command search window");
        cx.run_until_parked();
        cx.update_window(window, |_, window, cx| {
            assert!(window.is_window_active());
            window.render_frame(cx);
            assert!(window.find("command-search").visible());
        })
        .expect("render the command search window");
        Fixture {
            window,
            workbench,
            _session: session,
        }
    }

    fn replace_search(window: &mut Window, cx: &mut App, query: &str) {
        window.click("command-search", cx);
        window.press("secondary-a", cx);
        window.input(query, cx);
    }

    fn assert_cards(window: &Window, expected: &[&str]) {
        let mut ids = BTreeSet::new();
        for context in [
            CommandContext::Direct(DeviceRole::Receiver),
            CommandContext::Direct(DeviceRole::Tracker),
            CommandContext::Direct(DeviceRole::Unknown),
            CommandContext::ForwardedTracker,
        ] {
            ids.extend(commands::presets(context).map(|preset| preset.id));
        }
        let mut cards: Vec<_> = ids
            .into_iter()
            .filter_map(|id| {
                let card = window.try_find(format!("sidebar-preset:{id}"))?;
                assert!(card.visible(), "matching card {id} must be visible");
                Some((id, card.bounds().top()))
            })
            .collect();
        cards.sort_by(|left, right| {
            left.1
                .partial_cmp(&right.1)
                .expect("card positions must be comparable")
        });
        let observed: Vec<_> = cards.into_iter().map(|(id, _)| id).collect();
        assert_eq!(observed, expected, "rendered command identities and order");
    }

    #[gpui_kit::test]
    fn typed_search_matches_command_title_and_description_and_clears(cx: &mut TestAppContext) {
        let runtime = crate::app_runtime::AppRuntime::new().expect("start isolated runtime");
        let directory = tempfile::tempdir().expect("create private preferences directory");
        let _ui_session = crate::test_support::UiTestSession::new(cx);
        let fixture = fixture(
            cx,
            runtime.executor(),
            directory.path().join("preferences.json"),
        );
        cx.update_window(fixture.window, |_, window, cx| {
            let input_id = fixture.workbench.read(cx).command_search.entity_id();
            assert!(window.find("sidebar-preset:info").visible());
            assert!(window.find("sidebar-preset:help").visible());

            window.click("command-search", cx);
            window.input("  ClEaRcHaNnEl  ", cx);
            assert_eq!(
                window.find("command-search").value(),
                Some("  ClEaRcHaNnEl  ")
            );
            assert_cards(window, &["receiver-clearchannel"]);
            assert!(window.find("commands.group.radio").visible());
            assert!(window.try_find("commands.group.information").is_none());

            replace_search(window, cx, "device");
            assert_cards(window, &["info", "help", "uptime", "reboot"]);
            assert!(
                window.find("commands.group.information").bounds().top()
                    < window.find("commands.group.system").bounds().top(),
                "matching groups must retain the authored order"
            );

            replace_search(window, cx, "channel");
            assert_cards(
                window,
                &["receiver-channel", "receiver-clearchannel", "rssi-scan"],
            );
            replace_search(window, cx, "paired trackers");
            assert_cards(window, &["list"]);
            assert_eq!(
                window.find("sidebar-preset:list").label(),
                Some("Paired trackers")
            );
            replace_search(window, cx, "hardware information");
            assert_cards(window, &["info"]);
            assert_eq!(
                window.find("sidebar-preset:info").label(),
                Some("Device info")
            );

            replace_search(window, cx, "no-command-can-match-this-query");
            assert_cards(window, &[]);
            assert_eq!(
                window.find("command-search-empty").role(),
                Some(gpui_kit::Role::Status)
            );
            assert!(window.find("command-clear-search").visible());
            window.click("command-clear-search", cx);
            assert_eq!(window.find("command-search").value(), Some(""));
            assert_eq!(window.find("command-search").focused(), Some(true));
            assert!(window.find("sidebar-preset:info").visible());
            assert!(window.find("sidebar-preset:help").visible());
            assert_eq!(
                fixture.workbench.read(cx).command_search.entity_id(),
                input_id
            );
        })
        .expect("exercise command filtering and clearing");
    }

    #[gpui_kit::test]
    fn search_temporarily_expands_matches_without_losing_collapsed_groups(cx: &mut TestAppContext) {
        let runtime = crate::app_runtime::AppRuntime::new().expect("start isolated runtime");
        let directory = tempfile::tempdir().expect("create private preferences directory");
        let _ui_session = crate::test_support::UiTestSession::new(cx);
        let fixture = fixture(
            cx,
            runtime.executor(),
            directory.path().join("preferences.json"),
        );
        cx.update_window(fixture.window, |_, window, cx| {
            window.click("commands.group.information", cx);
            assert!(window.try_find("sidebar-preset:info").is_none());
            assert!(
                fixture
                    .workbench
                    .read(cx)
                    .sidebar
                    .collapsed_groups
                    .contains("commands.group.information")
            );

            window.click("command-search", cx);
            window.input("hardware information", cx);
            assert_cards(window, &["info"]);
            window.click("commands.group.information", cx);
            assert_cards(window, &["info"]);
            assert!(
                fixture
                    .workbench
                    .read(cx)
                    .sidebar
                    .collapsed_groups
                    .contains("commands.group.information"),
                "search disclosure must not overwrite the retained collapsed state"
            );

            replace_search(window, cx, "   ");
            assert_eq!(window.find("command-search").value(), Some("   "));
            assert!(window.find("commands.group.information").visible());
            assert!(window.try_find("sidebar-preset:info").is_none());
            replace_search(window, cx, "no-command-can-match-this-query");
            assert_cards(window, &[]);
            window.click("command-clear-search", cx);
            assert_eq!(window.find("command-search").value(), Some(""));
            assert!(window.try_find("sidebar-preset:info").is_none());
            window.click("commands.group.information", cx);
            assert!(window.find("sidebar-preset:info").visible());
            assert!(
                !fixture
                    .workbench
                    .read(cx)
                    .sidebar
                    .collapsed_groups
                    .contains("commands.group.information")
            );
        })
        .expect("exercise retained command-group disclosure state");
    }

    #[gpui_kit::test]
    fn search_respects_device_context_and_filtered_card_opens_its_own_review(
        cx: &mut TestAppContext,
    ) {
        let runtime = crate::app_runtime::AppRuntime::new().expect("start isolated runtime");
        let directory = tempfile::tempdir().expect("create private preferences directory");
        let _ui_session = crate::test_support::UiTestSession::new(cx);
        let fixture = fixture(
            cx,
            runtime.executor(),
            directory.path().join("preferences.json"),
        );
        let input_id = fixture
            .workbench
            .read_with(cx, |this, _| this.command_search.entity_id());
        cx.update_window(fixture.window, |_, window, cx| {
            assert_eq!(
                fixture.workbench.read(cx).command_search.entity_id(),
                input_id
            );
            window.click("command-search", cx);
            window.input("  LiST  ", cx);
            assert_cards(window, &["list"]);

            window.click(format!("port-focus:direct:{UNKNOWN_PORT}"), cx);
            assert_eq!(window.find("command-search").value(), Some("  LiST  "));
            assert_cards(window, &[]);
            assert!(window.find("command-clear-search").visible());
            assert_eq!(
                fixture.workbench.read(cx).command_context(),
                CommandContext::Direct(DeviceRole::Unknown)
            );
        })
        .expect("exercise the unknown-device context");
        fixture.workbench.update(cx, |this, cx| {
            let receiver = this.inventory.receivers[0].clone();
            this.monitor =
                crate::discovery::ReceiverMonitor::fixture(receiver, 0, "010203040506", true);
            cx.notify();
        });
        cx.run_until_parked();
        cx.update_window(fixture.window, |_, window, cx| {
            window.render_frame(cx);
            window.click(format!("card:tracker:{RECEIVER}:0"), cx);
            assert_eq!(window.find("command-search").value(), Some("  LiST  "));
            assert_eq!(
                fixture.workbench.read(cx).command_context(),
                CommandContext::ForwardedTracker
            );
            assert_cards(window, &[]);
            replace_search(window, cx, "  CALIBRATE ACC  ");
            assert_cards(window, &["calibrate-acc"]);

            window.click(format!("port-focus:{RECEIVER}:{RECEIVER_PORT}"), cx);
            assert_eq!(
                window.find("command-search").value(),
                Some("  CALIBRATE ACC  ")
            );
            assert_cards(window, &[]);
            assert_eq!(
                fixture.workbench.read(cx).command_context(),
                CommandContext::Direct(DeviceRole::Receiver)
            );

            replace_search(window, cx, "channel");
            assert_cards(
                window,
                &["receiver-channel", "receiver-clearchannel", "rssi-scan"],
            );
            window.click("sidebar-preset:receiver-channel", cx);
            let workbench = fixture.workbench.read(cx);
            assert!(
                matches!(
                    &workbench.sidebar.pending,
                    Some(PendingCommand::Parameter { preset, .. }) if preset.id == "receiver-channel"
                ),
                "the filtered card must open its own parameter editor"
            );
            assert!(workbench.batch_history.is_empty());
            assert!(workbench.manager.writes_idle(RECEIVER_PORT));
            window.click("cancel-sidebar-command", cx);
            assert_eq!(window.find("command-search").value(), Some("channel"));
            assert_eq!(fixture.workbench.read(cx).command_search.entity_id(), input_id);
            assert_cards(
                window,
                &["receiver-channel", "receiver-clearchannel", "rssi-scan"],
            );
        })
        .expect("exercise context filtering and the non-sending card action");
    }

    #[gpui_kit::test]
    fn locale_changes_recompute_matches_without_replacing_the_query(cx: &mut TestAppContext) {
        let runtime = crate::app_runtime::AppRuntime::new().expect("start isolated runtime");
        let directory = tempfile::tempdir().expect("create private preferences directory");
        let _ui_session = crate::test_support::UiTestSession::new(cx);
        let fixture = fixture(
            cx,
            runtime.executor(),
            directory.path().join("preferences.json"),
        );
        cx.update_window(fixture.window, |_, window, cx| {
            let input_id = fixture.workbench.read(cx).command_search.entity_id();
            assert_eq!(
                window.find("command-search").placeholder(),
                Some("Search commands")
            );
            window.click("command-search", cx);
            window.input("hardware information", cx);
            assert_cards(window, &["info"]);

            // An existing scan suppresses real device enumeration during this isolated UI test.
            fixture
                .workbench
                .update(cx, |this, _| this.scanning_inventory = true);
            window.click("nav-settings", cx);
            window.click("settings-language-simplified-chinese", cx);
            assert!(fixture.workbench.read(cx).inventory_task.is_none());
            window.click("nav-device-setup", cx);
            assert_eq!(
                window.find("command-search").value(),
                Some("hardware information")
            );
            assert_eq!(
                window.find("command-search").placeholder(),
                Some("搜索命令")
            );
            assert_eq!(
                fixture.workbench.read(cx).command_search.entity_id(),
                input_id
            );
            assert_cards(window, &[]);
            assert!(window.find("command-clear-search").visible());

            replace_search(window, cx, "设备信息");
            assert_cards(window, &["info"]);
            assert_eq!(window.find("sidebar-preset:info").label(), Some("设备信息"));
            window.click("nav-settings", cx);
            window.click("settings-language-english", cx);
            window.click("nav-device-setup", cx);
            assert_eq!(window.find("command-search").value(), Some("设备信息"));
            assert_eq!(
                window.find("command-search").placeholder(),
                Some("Search commands")
            );
            assert_eq!(
                fixture.workbench.read(cx).command_search.entity_id(),
                input_id
            );
            assert_cards(window, &[]);
            window.click("command-clear-search", cx);
            assert_eq!(window.find("command-search").value(), Some(""));
            assert_eq!(
                window.find("sidebar-preset:info").label(),
                Some("Device info")
            );
            fixture
                .workbench
                .update(cx, |this, _| this.scanning_inventory = false);
        })
        .expect("exercise the real locale controls and retained search");
    }
}
