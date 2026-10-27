use super::Workbench;
use crate::{
    app_runtime::{AppRuntime, Executor},
    device_registry::{DeviceKey, DeviceRecord, DeviceRegistry, ReportedMetadata},
    discovery::ReceiverMonitor,
    domain::{ReceiverEndpoint, TrackerLink},
    ui::command_panel::CommandFocus,
};
use gpui_kit::{
    AnyWindowHandle, AppContext as _, Bounds, Context, Entity, IntoElement, Pixels, Render, Role,
    TestAppContext, Window, WindowBounds, WindowOptions, div, point, prelude::*, px, size,
    test::TestWindowExt as _,
};

const RECEIVER: &str = "synthetic-roster-receiver";
const LONG_RECEIVER: &str =
    "Synthetic receiver with a deliberately very long descriptive name for responsive layout";
const LONG_NICKNAME: &str = "Synthetic tracker nickname with a deliberately long display label";
const FIRST_ADDRESS: &str = "010203040506";
const SLOTS: [u8; 7] = [0, 1, 2, 3, 4, 5, 6];

struct RosterHarness {
    workbench: Entity<Workbench>,
}

impl Render for RosterHarness {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.workbench.update(cx, |this, cx| {
            div()
                .size_full()
                .track_focus(&this.page_focus)
                .tab_group()
                .flex()
                .flex_col()
                .p_3()
                .gap_2()
                .child(
                    gpui_kit::component::button::Button::new("roster-keyboard-start")
                        .label("Device roster"),
                )
                .child(this.roster(cx))
        })
    }
}

struct Fixture {
    window: AnyWindowHandle,
    workbench: Entity<Workbench>,
    receiver: ReceiverEndpoint,
    preferences_path: std::path::PathBuf,
}

impl Fixture {
    fn new(
        cx: &mut TestAppContext,
        executor: Executor,
        preferences_path: std::path::PathBuf,
    ) -> Self {
        let receiver = ReceiverEndpoint {
            key: RECEIVER.into(),
            path: RECEIVER.as_bytes().to_vec(),
            label: "Synthetic receiver".into(),
            // Registry admission requires an unambiguous receiver hardware identity.
            serial_number: Some("0011223344556677".into()),
        };

        let (window, harness) = cx.update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        Default::default(),
                        size(px(800.), px(600.)),
                    ))),
                    ..Default::default()
                },
                cx,
                |window, cx| {
                    let workbench = cx.new(|cx| {
                        let mut this = crate::ui::test_support::offline_workbench(
                            window,
                            cx,
                            crate::sessions::SessionManager::new(),
                            executor,
                        );
                        this.preferences_io = crate::ui::preferences::PreferenceIo::for_test(
                            preferences_path.clone(),
                        );
                        this.inventory.receivers.push(receiver.clone());
                        this.monitor =
                            ReceiverMonitor::fixture(receiver.clone(), 0, "010203040506", true);
                        this.monitor
                            .fixture_observation_mut(&receiver)
                            .trackers
                            .extend([
                                TrackerLink {
                                    receiver_key: RECEIVER.into(),
                                    slot: 1,
                                    address: Some("111213141516".into()),
                                    online: true,
                                },
                                TrackerLink {
                                    receiver_key: RECEIVER.into(),
                                    slot: 2,
                                    address: Some("212223242526".into()),
                                    online: false,
                                },
                                TrackerLink {
                                    receiver_key: RECEIVER.into(),
                                    slot: 3,
                                    address: None,
                                    online: false,
                                },
                            ]);
                        this.sync_registry_observations(cx);
                        assert!(
                            this.current_nickname_key(&CommandFocus::Tracker {
                                receiver: RECEIVER.into(),
                                slot: 2,
                                address: Some("212223242526".into()),
                            })
                            .is_some(),
                            "the known offline tracker must be admitted through the receiver",
                        );
                        this
                    });
                    // Match production navigation's non-tab page entry, not a card target.
                    let focus = workbench.read(cx).page_focus.clone();
                    focus.focus(window, cx);
                    cx.new(|_| RosterHarness { workbench })
                },
            )
            .expect("open production device roster")
        });
        cx.update_window(window, |_, window, _| window.activate_window())
            .expect("activate the roster test window");
        cx.run_until_parked();
        cx.update_window(window, |_, window, _| {
            assert!(
                window.is_window_active(),
                "roster keyboard integration requires an active native window",
            );
        })
        .expect("inspect the roster test window");
        let workbench = cx.update(|cx| harness.read(cx).workbench.clone());
        Self {
            window,
            workbench,
            receiver,
            preferences_path,
        }
    }

    fn assert_target(&self, cx: &TestAppContext, slot: u8, selected: &[u8]) {
        self.workbench.read_with(cx, |this, _| {
            assert!(
                matches!(
                    &this.command_focus,
                    Some(CommandFocus::Tracker { receiver, slot: current, .. })
                        if receiver == RECEIVER && *current == slot
                ),
                "current tracker should be slot {slot}: {:?}",
                this.command_focus,
            );
            assert_eq!(
                this.selected,
                selected
                    .iter()
                    .map(|slot| format!("tracker:{RECEIVER}:{slot}"))
                    .collect::<std::collections::BTreeSet<_>>(),
                "target activation and batch selection must remain independent",
            );
            assert_eq!(
                this.manager.sessions().count(),
                0,
                "no serial commands or connections"
            );
        });
    }

    fn assert_first_identity(&self, cx: &TestAppContext) {
        let focus = CommandFocus::Tracker {
            receiver: RECEIVER.into(),
            slot: 0,
            address: Some(FIRST_ADDRESS.into()),
        };
        self.workbench.read_with(cx, |this, _| {
            assert_eq!(
                this.command_focus.as_ref(),
                Some(&focus),
                "the command target must retain the original tracker address",
            );
            assert_eq!(
                this.current_nickname_key(&focus),
                DeviceKey::tracker_address(FIRST_ADDRESS),
                "the current target must resolve to its admitted registry identity",
            );
        });
    }
}

fn card(slot: u8) -> gpui_kit::SharedString {
    format!("card:tracker:{RECEIVER}:{slot}").into()
}

fn checkbox(slot: u8) -> gpui_kit::SharedString {
    format!("check:tracker:{RECEIVER}:{slot}").into()
}

fn tab_to(slot: u8, window: &mut Window, cx: &mut gpui_kit::App) {
    for _ in 0..32 {
        if window.find(card(slot)).focused() == Some(true) {
            return;
        }
        window.press("tab", cx);
    }
    panic!("tracker {slot} must be keyboard reachable");
}

fn settle_roster(cx: &mut TestAppContext, fixture: &Fixture) {
    // The grid changes its keyed column count in prepaint and schedules another frame.
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update_window(fixture.window, |_, window, cx| {
            window.simulate_next_frame(cx);
            window.render_frame(cx);
        })
        .expect("settle the roster layout");
    }
}

fn click_card(window: &mut Window, cx: &mut gpui_kit::App, slot: u8) {
    let bounds = window.find(card(slot)).bounds();
    window.click_at(
        card(slot),
        point(bounds.size.width / 2., bounds.size.height - px(5.)),
        cx,
    );
}

fn physical_coordinate(window: &Window, value: Pixels) -> i32 {
    (f32::from(window.pixel_snap(value)) * window.scale_factor()).round() as i32
}

fn snapped_edges(window: &Window, bounds: Bounds<Pixels>) -> [i32; 4] {
    [
        physical_coordinate(window, bounds.left()),
        physical_coordinate(window, bounds.top()),
        physical_coordinate(window, bounds.right()),
        physical_coordinate(window, bounds.bottom()),
    ]
}

fn assert_grid_geometry(window: &Window, slots: [u8; 7], columns: usize) {
    let cards = slots.map(|slot| window.find(card(slot)).bounds());
    let checks = slots.map(|slot| window.find(checkbox(slot)).bounds());
    let first = snapped_edges(window, cards[0]);
    let width = first[2] - first[0];
    assert!(width > 0, "tracker cards must have positive width");
    let context = format!(
        "columns {columns}, rem {:?}, viewport {:?}, cards {cards:?}, checkboxes {checks:?}",
        window.rem_size(),
        window.viewport_size(),
    );
    let first_gap = (columns > 1).then(|| snapped_edges(window, cards[1])[0] - first[2]);
    for (index, slot) in slots.into_iter().enumerate() {
        let card_edges = snapped_edges(window, cards[index]);
        let check = snapped_edges(window, checks[index]);
        let row_start = snapped_edges(window, cards[index / columns * columns]);
        let column_start = snapped_edges(window, cards[index % columns]);
        assert_eq!(
            card_edges[2] - card_edges[0],
            width,
            "slot {slot} must share the snapped card width, including partial rows: {context}",
        );
        assert_eq!(
            card_edges[1], row_start[1],
            "slot {slot} must share its row spine: {context}",
        );
        assert_eq!(
            card_edges[0], column_start[0],
            "slot {slot} must share its column spine: {context}",
        );
        assert!(
            check[0] >= card_edges[0]
                && check[1] >= card_edges[1]
                && check[2] <= card_edges[2]
                && check[3] <= card_edges[3],
            "slot {slot}'s checkbox must stay inside its card: {context}",
        );
        assert!(
            card_edges[0] >= 0
                && card_edges[2] <= physical_coordinate(window, window.viewport_size().width),
            "slot {slot} must remain inside the window horizontally: {context}",
        );
        assert!(
            window.find(card(slot)).visible() && window.find(checkbox(slot)).visible(),
            "slot {slot}'s real controls must be visible: {context}",
        );
        if index % columns != 0 {
            let previous = snapped_edges(window, cards[index - 1]);
            let gap = card_edges[0] - previous[2];
            assert!(gap > 0, "slot {slot} must not overlap: {context}");
            assert_eq!(
                Some(gap),
                first_gap,
                "slot {slot} must share the physical-pixel column gap: {context}",
            );
        }
        if index >= columns {
            let previous = snapped_edges(window, cards[index - columns]);
            assert!(
                previous[3] < card_edges[1],
                "slot {slot} must not overlap its preceding row: {context}",
            );
        }
    }
}

#[gpui_kit::test]
fn tracker_body_and_batch_checkbox_keep_independent_pointer_keyboard_and_offline_contracts(
    cx: &mut TestAppContext,
) {
    let runtime = AppRuntime::new().expect("create isolated I/O runtime");
    let directory = tempfile::tempdir().expect("create private preferences directory");
    let _ui_session = crate::ui::test_support::UiTestSession::new(cx);
    let fixture = Fixture::new(
        cx,
        runtime.executor(),
        directory.path().join("preferences.json"),
    );
    cx.update_window(fixture.window, |_, window, cx| {
        window.render_frame(cx);
        assert!(fixture.workbench.read(cx).page_focus.is_focused(window));
        let target = window.find(card(0));
        let selection = window.find(checkbox(0));
        assert_eq!(target.role(), Some(Role::Button));
        assert_eq!(selection.role(), Some(Role::CheckBox));
        window.click_at(
            card(0),
            point(
                target.bounds().size.width / 2.,
                target.bounds().size.height - px(5.),
            ),
            cx,
        );
    })
    .unwrap();
    fixture.assert_target(cx, 0, &[]);

    cx.update_window(fixture.window, |_, window, cx| {
        window.click(checkbox(1), cx);
    })
    .unwrap();
    fixture.assert_target(cx, 0, &[1]);

    cx.update_window(fixture.window, |_, window, cx| {
        window.press("tab", cx);
        assert_eq!(window.find("roster-keyboard-start").focused(), Some(true));
        tab_to(1, window, cx);
        window.press("enter", cx);
    })
    .unwrap();
    fixture.assert_target(cx, 1, &[1]);

    cx.update_window(fixture.window, |_, window, cx| {
        window.press("tab", cx);
        assert_eq!(window.find(checkbox(1)).focused(), Some(true));
        window.press("space", cx);
    })
    .unwrap();
    fixture.assert_target(cx, 1, &[]);
    cx.update_window(fixture.window, |_, window, cx| {
        window.press("space", cx);
        window.press("shift-tab", cx);
        assert_eq!(window.find(card(1)).focused(), Some(true));
    })
    .unwrap();
    fixture.assert_target(cx, 1, &[1]);

    fixture.workbench.update(cx, |this, cx| {
        this.monitor
            .fixture_observation_mut(&fixture.receiver)
            .trackers
            .reverse();
        cx.notify();
    });
    cx.update_window(fixture.window, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find(card(1)).focused(),
            Some(true),
            "focus follows tracker identity"
        );
        tab_to(0, window, cx);
        window.press("space", cx);
    })
    .unwrap();
    fixture.assert_target(cx, 0, &[1]);

    cx.update_window(fixture.window, |_, window, cx| {
        assert_ne!(window.find(card(2)).disabled(), Some(true));
        assert_eq!(window.find(checkbox(2)).disabled(), Some(true));
        window.click(checkbox(2), cx);
    })
    .unwrap();
    fixture.assert_target(cx, 0, &[1]);

    cx.update_window(fixture.window, |_, window, cx| {
        window.click(card(2), cx);
    })
    .unwrap();
    fixture.assert_target(cx, 2, &[1]);

    cx.update_window(fixture.window, |_, window, cx| {
        assert_eq!(window.find(card(3)).disabled(), Some(true));
        assert_eq!(window.find(checkbox(3)).disabled(), Some(true));
        window.click(card(3), cx);
    })
    .unwrap();
    fixture.assert_target(cx, 2, &[1]);
}

#[gpui_kit::test]
fn replacing_a_tracker_slot_does_not_transfer_keyboard_focus(cx: &mut TestAppContext) {
    let runtime = AppRuntime::new().expect("create isolated I/O runtime");
    let directory = tempfile::tempdir().expect("create private preferences directory");
    let _ui_session = crate::ui::test_support::UiTestSession::new(cx);
    let fixture = Fixture::new(
        cx,
        runtime.executor(),
        directory.path().join("preferences.json"),
    );
    cx.update_window(fixture.window, |_, window, cx| {
        window.render_frame(cx);
        assert!(fixture.workbench.read(cx).page_focus.is_focused(window));
        window.press("tab", cx);
        assert_eq!(window.find("roster-keyboard-start").focused(), Some(true));
        tab_to(0, window, cx);
        assert_eq!(window.find(card(0)).focused(), Some(true));
    })
    .unwrap();
    fixture.workbench.update(cx, |this, cx| {
        this.monitor
            .fixture_observation_mut(&fixture.receiver)
            .trackers[0]
            .address = Some("919293949596".into());
        cx.notify();
    });
    cx.update_window(fixture.window, |_, window, cx| {
        window.render_frame(cx);
        assert_ne!(window.find(card(0)).focused(), Some(true));
        window.press("enter", cx);
    })
    .unwrap();
    fixture.workbench.read_with(cx, |this, _| {
        assert!(this.command_focus.is_none());
        assert!(this.selected.is_empty());
    });
}

#[gpui_kit::test]
fn responsive_roster_preserves_controls_and_identity_through_telemetry_and_reorder(
    cx: &mut TestAppContext,
) {
    // The runtime and private directory must outlive the session's real UI cleanup.
    let runtime = AppRuntime::new().expect("create isolated I/O runtime");
    let directory = tempfile::tempdir().expect("create private preferences directory");
    let ui_session = crate::ui::test_support::UiTestSession::new(cx);
    let mut fixture = Fixture::new(
        cx,
        runtime.executor(),
        directory.path().join("preferences.json"),
    );
    fixture.receiver.label = LONG_RECEIVER.into();
    fixture.workbench.update(cx, |this, cx| {
        this.inventory.receivers[0] = fixture.receiver.clone();
        this.monitor
            .fixture_observation_mut(&fixture.receiver)
            .trackers = SLOTS
            .into_iter()
            .map(|slot| TrackerLink {
                receiver_key: RECEIVER.into(),
                slot,
                address: Some(if slot == 0 {
                    FIRST_ADDRESS.into()
                } else {
                    format!("AABBCCDDEE{slot:02X}")
                }),
                online: true,
            })
            .collect();
        let key = DeviceKey::tracker_address(FIRST_ADDRESS)
            .expect("synthetic tracker has a valid registry key");
        let snapshot = DeviceRegistry::from_records(vec![DeviceRecord {
            key,
            nickname: Some(LONG_NICKNAME.into()),
            reported: ReportedMetadata::default(),
            last_receiver: None,
        }])
        .expect("the long synthetic nickname must pass registry validation");
        this.registry.set_test_snapshot(snapshot);
        this.sync_registry_observations(cx);
        cx.notify();
    });
    cx.simulate_window_scale_factor_change(fixture.window, 1.25);
    cx.update(|cx| {
        gpui_kit::component::Theme::update(cx, |theme| theme.font_size = px(20.));
    });
    cx.simulate_window_resize(fixture.window, size(px(360.), px(1600.)));
    settle_roster(cx, &fixture);
    cx.update_window(fixture.window, |_, window, cx| {
        assert_eq!(
            window.rem_size(),
            px(20.),
            "the roster must use enlarged rem"
        );
        assert_eq!(
            window.scale_factor(),
            1.25,
            "the display scale must be fractional"
        );
        let receiver = format!("receiver-focus:{RECEIVER}");
        let heading = window.find(receiver.clone());
        assert_eq!(heading.label(), Some(LONG_RECEIVER));
        assert!(
            heading.visible()
                && heading.bounds().left() >= px(0.)
                && heading.bounds().right() <= window.viewport_size().width,
            "the long receiver heading must remain inside the narrow window",
        );
        assert!(
            window
                .find(card(0))
                .label()
                .is_some_and(|label| label.contains(LONG_NICKNAME)),
            "the real card must use the validated registry nickname",
        );
        assert!(
            window
                .find(checkbox(0))
                .label()
                .is_some_and(|label| label.contains(LONG_NICKNAME)),
            "the real selection control must name the same tracker",
        );
        assert_grid_geometry(window, SLOTS, 1);
        window.click(receiver, cx);
    })
    .expect("activate the long receiver label");
    fixture.workbench.read_with(cx, |this, _| {
        assert_eq!(
            this.command_focus,
            Some(CommandFocus::Receiver(RECEIVER.into())),
            "the long receiver heading must remain pointer-operable",
        );
    });
    cx.update_window(fixture.window, |_, window, cx| {
        click_card(window, cx, 0);
        window.click(checkbox(0), cx);
        assert_eq!(window.find(checkbox(0)).checked(), Some(true));
        tab_to(0, window, cx);
        assert_eq!(window.find(card(0)).focused(), Some(true));
    })
    .expect("select and focus the first tracker");
    fixture.assert_target(cx, 0, &[0]);
    fixture.assert_first_identity(cx);

    // Seven cards leave a partial final row in both the two- and three-column layouts.
    for (width, columns) in [(360., 1), (548., 2), (744., 3)] {
        cx.simulate_window_resize(fixture.window, size(px(width), px(1600.)));
        settle_roster(cx, &fixture);
        cx.update_window(fixture.window, |_, window, _| {
            assert_eq!(
                window.find(card(0)).focused(),
                Some(true),
                "keyboard focus must survive resize to {columns} columns",
            );
            assert_eq!(window.find(checkbox(0)).checked(), Some(true));
            assert_grid_geometry(window, SLOTS, columns);
        })
        .expect("inspect the settled responsive roster");
        fixture.assert_target(cx, 0, &[0]);
        fixture.assert_first_identity(cx);

        cx.update_window(fixture.window, |_, window, cx| {
            window.click(checkbox(6), cx);
            assert_eq!(window.find(checkbox(6)).checked(), Some(true));
        })
        .expect("select the partial-row tracker");
        fixture.assert_target(cx, 0, &[0, 6]);
        cx.update_window(fixture.window, |_, window, cx| {
            window.click(checkbox(6), cx);
            assert_eq!(window.find(checkbox(6)).checked(), Some(false));
            click_card(window, cx, 6);
        })
        .expect("activate the partial-row tracker");
        fixture.assert_target(cx, 6, &[0]);
        cx.update_window(fixture.window, |_, window, cx| {
            click_card(window, cx, 0);
            tab_to(0, window, cx);
            assert_eq!(window.find(card(0)).focused(), Some(true));
        })
        .expect("restore first-tracker focus");
        fixture.assert_target(cx, 0, &[0]);
        fixture.assert_first_identity(cx);
    }

    let before = cx
        .update_window(fixture.window, |_, window, _| {
            SLOTS.map(|slot| {
                (
                    snapped_edges(window, window.find(card(slot)).bounds()),
                    snapped_edges(window, window.find(checkbox(slot)).bounds()),
                )
            })
        })
        .expect("capture idle tracker geometry");
    fixture.workbench.update(cx, |this, cx| {
        assert_eq!(
            this.monitor
                .observation_for(&fixture.receiver)
                .unwrap()
                .tps(0),
            0,
            "the synthetic tracker must begin idle",
        );
        this.monitor.fixture_tps(120);
        assert_eq!(
            this.monitor
                .observation_for(&fixture.receiver)
                .unwrap()
                .tps(0),
            120,
            "the production roster must receive active telemetry",
        );
        cx.notify();
    });
    settle_roster(cx, &fixture);
    cx.update_window(fixture.window, |_, window, _| {
        for (slot, expected) in SLOTS.into_iter().zip(before) {
            assert_eq!(
                (
                    snapped_edges(window, window.find(card(slot)).bounds()),
                    snapped_edges(window, window.find(checkbox(slot)).bounds()),
                ),
                expected,
                "activity must not move or resize tracker {slot} or its checkbox",
            );
        }
        assert_eq!(window.find(card(0)).focused(), Some(true));
        assert_eq!(window.find(checkbox(0)).checked(), Some(true));
        assert_grid_geometry(window, SLOTS, 3);
    })
    .expect("inspect active telemetry geometry");
    fixture.assert_target(cx, 0, &[0]);
    fixture.assert_first_identity(cx);

    fixture.workbench.update(cx, |this, cx| {
        this.monitor
            .fixture_observation_mut(&fixture.receiver)
            .trackers
            .reverse();
        this.sync_registry_observations(cx);
        cx.notify();
    });
    settle_roster(cx, &fixture);
    cx.update_window(fixture.window, |_, window, _| {
        assert_eq!(
            window.find(card(0)).focused(),
            Some(true),
            "keyboard focus must follow the tracker identity after reordering",
        );
        assert_eq!(window.find(checkbox(0)).checked(), Some(true));
        assert!(
            snapped_edges(window, window.find(card(0)).bounds())[1] > before[0].0[1]
                && snapped_edges(window, window.find(card(6)).bounds())[1] < before[6].0[1],
            "the roster must really move the first and last tracker",
        );
        assert_grid_geometry(window, [6, 5, 4, 3, 2, 1, 0], 3);
    })
    .expect("inspect the reordered roster");
    fixture.assert_target(cx, 0, &[0]);
    fixture.assert_first_identity(cx);
    fixture.workbench.read_with(cx, |this, _| {
        assert_eq!(
            this.monitor
                .observation_for(&fixture.receiver)
                .unwrap()
                .tps(0),
            120,
            "reordering must retain the original tracker's active telemetry",
        );
    });

    cx.update_window(fixture.window, |_, window, cx| {
        click_card(window, cx, 6);
    })
    .expect("activate the reordered first card");
    fixture.assert_target(cx, 6, &[0]);
    cx.update_window(fixture.window, |_, window, cx| {
        tab_to(0, window, cx);
        window.press("enter", cx);
        assert_eq!(window.find(card(0)).focused(), Some(true));
    })
    .expect("activate the reordered partial-row card by keyboard");
    fixture.assert_target(cx, 0, &[0]);
    fixture.assert_first_identity(cx);
    cx.update_window(fixture.window, |_, window, cx| {
        window.press("tab", cx);
        assert_eq!(window.find(checkbox(0)).focused(), Some(true));
        window.press("space", cx);
        assert_eq!(window.find(checkbox(0)).checked(), Some(false));
    })
    .expect("clear the reordered tracker's selection by keyboard");
    fixture.assert_target(cx, 0, &[]);
    fixture.assert_first_identity(cx);
    cx.update_window(fixture.window, |_, window, cx| {
        window.press("space", cx);
        assert_eq!(window.find(checkbox(0)).checked(), Some(true));
    })
    .expect("restore the reordered tracker's selection by keyboard");
    fixture.assert_target(cx, 0, &[0]);
    let preferences_path = fixture.preferences_path.clone();
    drop(fixture);
    drop(ui_session);
    assert!(
        !preferences_path.exists(),
        "the isolated roster interactions must not write preferences",
    );
}
