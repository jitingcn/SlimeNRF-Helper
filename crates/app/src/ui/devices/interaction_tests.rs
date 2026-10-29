use super::Workbench;
use crate::{
    app_runtime::{AppRuntime, Executor},
    device_registry::{DeviceKey, DeviceRecord, DeviceRegistry, ReportedMetadata},
    discovery::{ReceiverMonitor, TrackerBattery},
    domain::{
        InterfaceDescription, ReceiverEndpoint, SerialEndpoint, SerialPortKind, TrackerLink,
        UsbInterface,
    },
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
const LONG_SERIAL: &str =
    "SYNTHETIC-SERIAL-THIS-IS-DELIBERATELY-MUCH-LONGER-THAN-A-NARROW-RECEIVER-HEADER";
const CONSOLE: &str = "fixture-console";
const RAW_DATA: &str = "fixture-raw-data";

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

fn telemetry_id(part: &str, slot: u8) -> gpui_kit::SharedString {
    format!("tracker-{part}:tracker:{RECEIVER}:{slot}").into()
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

fn assert_temperature_position(window: &Window, slot: u8) {
    let card = window.find(card(slot)).bounds();
    let tps = window.find(telemetry_id("tps", slot)).bounds();
    let address = window.find(telemetry_id("address", slot));
    let temperature = window.find(telemetry_id("temperature", slot));
    let status = window.find(telemetry_id("status", slot)).bounds();
    let address_bounds = address.bounds();
    let temperature_bounds = temperature.bounds();
    assert!(
        address.visible()
            && temperature.visible()
            && address_bounds.size.width > px(0.)
            && temperature_bounds.left() >= card.left()
            && temperature_bounds.right() <= card.right()
            && address_bounds.right() <= temperature_bounds.left()
            && address_bounds.top() < temperature_bounds.bottom()
            && temperature_bounds.top() < address_bounds.bottom(),
        "slot {slot}: address and temperature must share a non-overlapping row: \
         address {address_bounds:?}, temperature {temperature_bounds:?}",
    );
    assert!(
        tps.bottom() <= temperature_bounds.top() && temperature_bounds.bottom() <= status.top(),
        "slot {slot}: temperature must sit between TPS and status: \
         TPS {tps:?}, temperature {temperature_bounds:?}, status {status:?}",
    );
    assert_eq!(
        snapped_edges(window, temperature_bounds)[2],
        snapped_edges(window, tps)[2],
        "slot {slot}: temperature and TPS must share the trailing spine",
    );
}

fn assert_battery_indicators_clustered(window: &Window, slot: u8, indicators: &[&str]) {
    let mut previous = window.find(telemetry_id("battery", slot)).bounds();
    for &indicator in indicators {
        let current = window.find(telemetry_id(indicator, slot)).bounds();
        let gap = current.left() - previous.right();
        assert!(
            gap >= px(0.)
                && gap <= window.rem_size() * 0.25 + px(1.)
                && previous.top() < current.bottom()
                && current.top() < previous.bottom(),
            "slot {slot}: {indicator} must stay close to the preceding battery indicator: \
             previous {previous:?}, current {current:?}",
        );
        previous = current;
    }
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

fn set_receiver_serial(fixture: &mut Fixture, cx: &mut TestAppContext, serial: Option<&str>) {
    fixture.receiver.serial_number = serial.map(str::to_owned);
    fixture.workbench.update(cx, |this, cx| {
        let trackers = std::mem::take(
            &mut this
                .monitor
                .fixture_observation_mut(&fixture.receiver)
                .trackers,
        );
        this.inventory.receivers[0] = fixture.receiver.clone();
        this.monitor = ReceiverMonitor::fixture(fixture.receiver.clone(), 0, FIRST_ADDRESS, true);
        this.monitor
            .fixture_observation_mut(&fixture.receiver)
            .trackers = trackers;
        cx.notify();
    });
}

fn assert_metadata_within_viewport(window: &Window, expected_online: usize, expected_total: usize) {
    let count = window.find(format!("receiver-count:{RECEIVER}"));
    let heading = window.find(format!("receiver-focus:{RECEIVER}"));
    let action = window.find(format!("all-trackers:{RECEIVER}"));
    let expected = rust_i18n::t!(
        "devices.receiver.tracker_count",
        online = expected_online,
        total = expected_total
    )
    .to_string();
    assert_eq!(count.label(), Some(expected.as_str()));
    for (name, element) in [
        ("count", &count),
        ("heading", &heading),
        ("action", &action),
    ] {
        assert!(
            element.visible()
                && element.bounds().size.width > px(0.)
                && element.bounds().left() >= px(0.)
                && element.bounds().right() <= window.viewport_size().width,
            "{name} must remain visible within the viewport: {:?}",
            element.bounds(),
        );
    }
    assert!(
        count.bounds().top() >= heading.bounds().bottom()
            && count.bounds().top() >= action.bounds().bottom(),
        "receiver metadata must remain below the name and action row",
    );
}

#[gpui_kit::test]
fn receiver_metadata_wraps_without_losing_counts_at_large_zoom_in_both_locales(
    cx: &mut TestAppContext,
) {
    let runtime = AppRuntime::new().expect("create isolated I/O runtime");
    let directory = tempfile::tempdir().expect("create private preferences directory");
    let _ui_session = crate::ui::test_support::UiTestSession::new(cx);
    let mut fixture = Fixture::new(
        cx,
        runtime.executor(),
        directory.path().join("preferences.json"),
    );
    cx.simulate_window_scale_factor_change(fixture.window, 1.25);

    for language in [
        crate::settings::Language::English,
        crate::settings::Language::SimplifiedChinese,
    ] {
        cx.update(|cx| {
            let preferences = fixture.workbench.update(cx, |this, _| {
                this.preferences.language = language;
                this.preferences.clone()
            });
            crate::ui::theme::apply(&preferences, cx);
            gpui_kit::component::Theme::update(cx, |theme| theme.font_size = px(20.));
        });
        set_receiver_serial(&mut fixture, cx, Some("SYNTHETIC-1234"));
        cx.simulate_window_resize(fixture.window, size(px(520.), px(1200.)));
        settle_roster(cx, &fixture);
        cx.update_window(fixture.window, |_, window, _| {
            assert_eq!(window.rem_size(), px(20.));
            assert_eq!(window.scale_factor(), 1.25);
            assert_metadata_within_viewport(window, 2, 4);
            let serial = window.find(format!("receiver-serial:{RECEIVER}"));
            let count = window.find(format!("receiver-count:{RECEIVER}"));
            assert_eq!(serial.label(), Some("SYNTHETIC-1234"));
            assert!(
                serial.visible()
                    && serial.bounds().top() == count.bounds().top()
                    && serial.bounds().right() <= count.bounds().left(),
                "short serial and count must share a row at a normal device-pane width",
            );
        })
        .expect("inspect receiver metadata at a normal device-pane width");

        set_receiver_serial(&mut fixture, cx, Some(LONG_SERIAL));
        cx.simulate_window_resize(fixture.window, size(px(360.), px(1200.)));
        settle_roster(cx, &fixture);
        cx.update_window(fixture.window, |_, window, _| {
            assert_metadata_within_viewport(window, 2, 4);
            let serial = window.find(format!("receiver-serial:{RECEIVER}"));
            let count = window.find(format!("receiver-count:{RECEIVER}"));
            assert_eq!(serial.label(), Some(LONG_SERIAL));
            assert!(
                serial.visible()
                    && serial.bounds().size.width > px(0.)
                    && serial.bounds().left() >= px(0.)
                    && serial.bounds().right() <= window.viewport_size().width
                    && count.bounds().top() >= serial.bounds().bottom(),
                "long serial and count must wrap without overlapping or escaping the viewport",
            );
        })
        .expect("inspect narrow receiver metadata");

        fixture.workbench.update(cx, |this, cx| {
            this.monitor
                .fixture_observation_mut(&fixture.receiver)
                .trackers[1]
                .online = false;
            cx.notify();
        });
        settle_roster(cx, &fixture);
        cx.update_window(fixture.window, |_, window, _| {
            assert_metadata_within_viewport(window, 1, 4);
        })
        .expect("inspect the updated selectable count");

        set_receiver_serial(&mut fixture, cx, None);
        settle_roster(cx, &fixture);
        cx.update_window(fixture.window, |_, window, _| {
            assert!(
                window
                    .try_find(format!("receiver-serial:{RECEIVER}"))
                    .is_none()
            );
            assert_metadata_within_viewport(window, 1, 4);
            assert!(
                window
                    .find(format!("receiver-missing-console:{RECEIVER}"))
                    .visible()
            );
        })
        .expect("inspect receiver metadata without a serial");

        fixture.workbench.update(cx, |this, cx| {
            this.monitor
                .fixture_observation_mut(&fixture.receiver)
                .trackers[1]
                .online = true;
            cx.notify();
        });
    }
}

#[test]
fn full_battery_icon_is_available_from_application_assets() {
    use gpui_kit::{AssetSource as _, assets::IconName};

    let path = IconName::BatteryFull.path();
    let svg = crate::ui::AppAssets
        .load(&path)
        .expect("read the full-battery asset")
        .expect("the full-battery icon must be bundled");
    assert!(svg.starts_with(b"<svg"), "the bundled icon must be an SVG");
}

#[gpui_kit::test]
fn tracker_telemetry_truncates_and_preserves_selection_across_states_and_locales(
    cx: &mut TestAppContext,
) {
    let runtime = AppRuntime::new().expect("create isolated I/O runtime");
    let directory = tempfile::tempdir().expect("create private preferences directory");
    let ui_session = crate::ui::test_support::UiTestSession::new(cx);
    let fixture = Fixture::new(
        cx,
        runtime.executor(),
        directory.path().join("preferences.json"),
    );
    let full = TrackerBattery {
        percent: 100,
        voltage_v: 4.31,
        charged: true,
        plugged: true,
    };
    fixture.workbench.update(cx, |this, cx| {
        this.monitor.fixture_tps(120);
        this.monitor
            .fixture_telemetry(&fixture.receiver, 0, Some(full), Some(-3.0));
        // Temperature must occupy the address row even when battery data is absent.
        this.monitor
            .fixture_telemetry(&fixture.receiver, 1, None, Some(33.0));
        // Stale data must not be rendered for an offline tracker.
        this.monitor
            .fixture_telemetry(&fixture.receiver, 2, Some(full), Some(33.0));
        // An unselected tracker with the same reading supplies a real layout
        // reference for the space consumed by the selected-state label.
        this.monitor
            .fixture_observation_mut(&fixture.receiver)
            .trackers[3]
            .online = true;
        this.monitor
            .fixture_telemetry(&fixture.receiver, 3, Some(full), None);
        cx.notify();
    });
    settle_roster(cx, &fixture);
    cx.update_window(fixture.window, |_, window, cx| {
        click_card(window, cx, 0);
        window.click(checkbox(0), cx);
        tab_to(0, window, cx);
        assert_eq!(window.find(checkbox(0)).checked(), Some(true));
        assert_eq!(window.find(card(0)).focused(), Some(true));
    })
    .expect("select and focus the live tracker");
    fixture.assert_target(cx, 0, &[0]);

    cx.simulate_window_scale_factor_change(fixture.window, 1.25);
    for language in [
        crate::settings::Language::English,
        crate::settings::Language::SimplifiedChinese,
    ] {
        cx.update(|cx| {
            let preferences = fixture.workbench.update(cx, |this, _| {
                this.preferences.language = language;
                this.preferences.clone()
            });
            crate::ui::theme::apply(&preferences, cx);
            gpui_kit::component::Theme::update(cx, |theme| theme.font_size = px(20.));
        });
        let mut wide_fixed_widths: Option<[i32; 4]> = None;
        let mut saw_truncated = false;
        for width in [360., 548., 524., 500., 488., 476., 468.] {
            cx.simulate_window_resize(fixture.window, size(px(width), px(1200.)));
            settle_roster(cx, &fixture);
            let (fixed_widths, truncated) = cx.update_window(fixture.window, |_, window, _| {
                assert_eq!(window.find(card(0)).focused(), Some(true));
                assert_eq!(window.find(checkbox(0)).checked(), Some(true));
                assert_eq!(window.find(telemetry_id("tps", 0)).label(), Some("120 TPS"));
                assert_eq!(
                    window.find(telemetry_id("battery", 0)).label(),
                    Some("100%(4.3V)")
                );
                assert_eq!(
                    window.find(telemetry_id("temperature", 0)).label(),
                    Some("-3.0°C")
                );
                assert_eq!(
                    window.find(telemetry_id("temperature", 1)).label(),
                    Some("33.0°C")
                );
                for part in ["battery", "power", "full"] {
                    assert!(
                        window.try_find(telemetry_id(part, 1)).is_none(),
                        "temperature-only telemetry must not show {part}",
                    );
                }
                let full_label = crate::i18n::tr("devices.tracker.full");
                assert_eq!(
                    window.find(telemetry_id("full", 0)).label(),
                    Some(full_label.as_str()),
                );
                let power_label = crate::i18n::tr("devices.tracker.power_indicated");
                assert_eq!(
                    window.find(telemetry_id("power", 0)).label(),
                    Some(power_label.as_str()),
                );
                let online_label = crate::i18n::tr("devices.tracker.state.online");
                let selected_label = crate::i18n::tr("devices.tracker.selection.selected");
                assert_eq!(
                    window.find(telemetry_id("status", 0)).label(),
                    Some(online_label.as_str()),
                );
                assert_eq!(
                    window.find(telemetry_id("selection", 0)).label(),
                    Some(selected_label.as_str()),
                );
                assert_temperature_position(window, 0);
                assert_temperature_position(window, 1);
                assert_battery_indicators_clustered(window, 0, &["power", "full"]);
                let card_bounds = window.find(card(0)).bounds();
                let reference_card = window.find(card(3)).bounds();
                let card_edges = snapped_edges(window, card_bounds);
                let reference_edges = snapped_edges(window, reference_card);
                assert_eq!(
                    card_edges[2] - card_edges[0],
                    reference_edges[2] - reference_edges[0],
                    "the selected and reference readings must use equally wide cards",
                );
                assert_eq!(window.find(checkbox(3)).checked(), Some(false));
                if width != 360. {
                    let peer = window.find(card(1)).bounds();
                    assert!(
                        snapped_edges(window, peer)[1] == card_edges[1]
                            && peer.left() >= card_bounds.right(),
                        "width {width} must exercise the two-column layout: \
                         first {card_bounds:?}, peer {peer:?}",
                    );
                }
                let parts = [
                    "tps",
                    "battery",
                    "power",
                    "full",
                    "temperature",
                    "status",
                    "selection",
                ];
                let bounds = parts.map(|part| {
                    let element = window.find(telemetry_id(part, 0));
                    let bounds = element.bounds();
                    assert!(
                        element.visible()
                            && bounds.size.width > px(0.)
                            && bounds.size.height > px(0.)
                            && bounds.left() >= card_bounds.left()
                            && bounds.right() <= card_bounds.right()
                            && bounds.top() >= card_bounds.top()
                            && bounds.bottom() <= card_bounds.bottom(),
                        "{part} must remain visible inside the card at width {width}: {bounds:?}",
                    );
                    bounds
                });
                let bottom_parts = [1, 2, 3, 5, 6];
                for &index in &bottom_parts {
                    assert!(
                        bounds[index].top() < bounds[1].bottom()
                            && bounds[1].top() < bounds[index].bottom(),
                        "{} must share the battery's bottom row at width {width}: {:?}",
                        parts[index],
                        bounds[index],
                    );
                }
                for pair in bottom_parts.windows(2) {
                    assert!(
                        bounds[pair[0]].right() <= bounds[pair[1]].left(),
                        "{} must precede {} on the same row at width {width}: {:?}, {:?}",
                        parts[pair[0]],
                        parts[pair[1]],
                        bounds[pair[0]],
                        bounds[pair[1]],
                    );
                }
                let reference = window.find(telemetry_id("battery", 3));
                assert_eq!(reference.label(), Some("100%(4.3V)"));
                let truncated = bounds[1].size.width < reference.bounds().size.width;
                assert!(
                    bounds[0].top() < bounds[1].top() && bounds[0].right() > card_bounds.center().x,
                    "TPS must remain in the trailing header above the battery",
                );
                assert_eq!(
                    bounds[2].size, bounds[3].size,
                    "the full indicator must have the same compact icon size as the plug",
                );
                assert!(
                    bounds[3].size.width <= window.rem_size(),
                    "the full indicator must be an icon rather than a text-sized marker",
                );
                assert_eq!(
                    snapped_edges(window, bounds[0])[2],
                    snapped_edges(window, bounds[6])[2],
                    "selection must remain aligned with the trailing header",
                );
                for (index, first) in bounds.iter().enumerate() {
                    for (other_index, second) in bounds.iter().enumerate().skip(index + 1) {
                        assert!(
                            first.right() <= second.left()
                                || second.right() <= first.left()
                                || first.bottom() <= second.top()
                                || second.bottom() <= first.top(),
                            "{} and {} must not overlap at width {width}: {first:?}, {second:?}",
                            parts[index],
                            parts[other_index],
                        );
                    }
                }
                assert_eq!(window.find(telemetry_id("tps", 2)).label(), Some("0 TPS"));
                for part in ["battery", "power", "full", "temperature"] {
                    assert!(
                        window.try_find(telemetry_id(part, 2)).is_none(),
                        "offline trackers must not show stale {part}",
                    );
                }
                assert!(window.find(telemetry_id("status", 2)).visible());
                (
                    [2, 3, 5, 6].map(|index| {
                        let edges = snapped_edges(window, bounds[index]);
                        edges[2] - edges[0]
                    }),
                    truncated,
                )
            })
            .expect("inspect single-row telemetry in the production roster");
            if let Some(wide_fixed_widths) = wide_fixed_widths {
                for ((part, narrow), wide) in ["power", "full", "status", "selection"]
                    .into_iter()
                    .zip(fixed_widths)
                    .zip(wide_fixed_widths)
                {
                    assert!(
                        (narrow - wide).abs() <= 1,
                        "{part} must retain its width at narrow card size: \
                         narrow {narrow}, wide {wide}",
                    );
                }
            } else {
                wide_fixed_widths = Some(fixed_widths);
            }
            fixture.assert_target(cx, 0, &[0]);
            fixture.assert_first_identity(cx);
            if width != 360. && truncated {
                saw_truncated = true;
                cx.update_window(fixture.window, |_, window, cx| {
                    window.hover(card(1), cx);
                    window.hover(telemetry_id("battery", 0), cx);
                })
                .expect("hover the truncated battery reading");
                cx.run_until_parked();
                cx.executor()
                    .advance_clock(std::time::Duration::from_millis(600));
                settle_roster(cx, &fixture);
                cx.update_window(fixture.window, |_, window, _| {
                    let expected = format!(
                        "100%(4.3V) · {}",
                        crate::i18n::tr("devices.tracker.power_help")
                    );
                    let tooltip = window.find("tracker-telemetry-tooltip");
                    assert!(tooltip.visible());
                    assert_eq!(tooltip.label(), Some(expected.as_str()));
                })
                .expect("inspect the complete truncated reading and its explanation");
                break;
            }
        }
        assert!(
            saw_truncated,
            "a narrow two-column card must truncate the selected reading in {language:?}",
        );
    }

    // Telemetry remains part of the target button, not the separate selection control.
    for part in ["battery", "temperature", "full"] {
        cx.update_window(fixture.window, |_, window, cx| {
            click_card(window, cx, 1);
        })
        .expect("change the current target before clicking telemetry");
        fixture.assert_target(cx, 1, &[0]);
        cx.update_window(fixture.window, |_, window, cx| {
            window.click(telemetry_id(part, 0), cx);
        })
        .expect("activate the tracker through its telemetry");
        fixture.assert_target(cx, 0, &[0]);
    }
    cx.update_window(fixture.window, |_, window, cx| {
        tab_to(0, window, cx);
        assert_eq!(window.find(checkbox(0)).checked(), Some(true));
    })
    .expect("restore keyboard focus without changing batch selection");

    fixture.workbench.update(cx, |this, cx| {
        this.monitor.fixture_telemetry(
            &fixture.receiver,
            0,
            Some(TrackerBattery {
                percent: 100,
                voltage_v: 4.3,
                charged: false,
                plugged: false,
            }),
            None,
        );
        this.monitor
            .fixture_telemetry(&fixture.receiver, 1, None, None);
        cx.notify();
    });
    settle_roster(cx, &fixture);
    cx.update_window(fixture.window, |_, window, _| {
        assert_eq!(
            window.find(telemetry_id("battery", 0)).label(),
            Some("100%(4.3V)")
        );
        for part in ["power", "full", "temperature"] {
            assert!(
                window.try_find(telemetry_id(part, 0)).is_none(),
                "ordinary 100% and missing temperature must not show {part}",
            );
        }
        for part in ["battery", "power", "full", "temperature"] {
            assert!(
                window.try_find(telemetry_id(part, 1)).is_none(),
                "missing telemetry must not show {part}",
            );
        }
        assert!(window.find(telemetry_id("status", 1)).visible());
        assert_eq!(window.find(card(0)).focused(), Some(true));
        assert_eq!(window.find(checkbox(0)).checked(), Some(true));
    })
    .expect("inspect independent missing telemetry and ordinary 100 percent");
    fixture.assert_target(cx, 0, &[0]);
    fixture.assert_first_identity(cx);

    fixture.workbench.update(cx, |this, cx| {
        this.monitor.fixture_telemetry(
            &fixture.receiver,
            0,
            Some(TrackerBattery {
                percent: 100,
                voltage_v: 4.31,
                charged: false,
                plugged: true,
            }),
            Some(-3.0),
        );
        cx.notify();
    });
    settle_roster(cx, &fixture);
    cx.update_window(fixture.window, |_, window, _| {
        assert!(window.find(telemetry_id("power", 0)).visible());
        assert_battery_indicators_clustered(window, 0, &["power"]);
        assert_temperature_position(window, 0);
        assert!(window.try_find(telemetry_id("full", 0)).is_none());
        assert_eq!(
            window.find(telemetry_id("temperature", 0)).label(),
            Some("-3.0°C")
        );
    })
    .expect("inspect indicated external power without a full marker");

    fixture.workbench.update(cx, |this, cx| {
        this.monitor.fixture_telemetry(
            &fixture.receiver,
            0,
            Some(TrackerBattery {
                percent: 100,
                voltage_v: 4.25,
                charged: true,
                plugged: false,
            }),
            Some(33.0),
        );
        cx.notify();
    });
    settle_roster(cx, &fixture);
    cx.update_window(fixture.window, |_, window, _| {
        assert!(window.find(telemetry_id("full", 0)).visible());
        assert_battery_indicators_clustered(window, 0, &["full"]);
        assert_temperature_position(window, 0);
        let full_label = crate::i18n::tr("devices.tracker.full");
        assert_eq!(
            window.find(telemetry_id("full", 0)).label(),
            Some(full_label.as_str()),
        );
        assert!(window.try_find(telemetry_id("power", 0)).is_none());
        assert_eq!(window.find(card(0)).focused(), Some(true));
        assert_eq!(window.find(checkbox(0)).checked(), Some(true));
    })
    .expect("inspect the full marker without external-power indication");
    fixture.assert_target(cx, 0, &[0]);

    fixture.workbench.update(cx, |this, cx| {
        this.monitor
            .fixture_observation_mut(&fixture.receiver)
            .trackers[0]
            .online = false;
        cx.notify();
    });
    settle_roster(cx, &fixture);
    cx.update_window(fixture.window, |_, window, _| {
        assert_eq!(window.find(telemetry_id("tps", 0)).label(), Some("0 TPS"));
        for part in ["battery", "power", "full", "temperature"] {
            assert!(window.try_find(telemetry_id(part, 0)).is_none());
        }
    })
    .expect("inspect stale telemetry after the tracker goes offline");

    fixture.workbench.update(cx, |this, cx| {
        this.monitor
            .fixture_telemetry(&fixture.receiver, 0, Some(full), Some(-3.0));
        this.monitor
            .fixture_telemetry(&fixture.receiver, 1, None, Some(33.0));
        this.monitor
            .fixture_observation_mut(&fixture.receiver)
            .trackers[0]
            .online = true;
        this.monitor
            .fixture_observation_mut(&fixture.receiver)
            .error = Some("synthetic reader failure".into());
        cx.notify();
    });
    settle_roster(cx, &fixture);
    cx.update_window(fixture.window, |_, window, _| {
        for slot in [0, 1] {
            assert_eq!(
                window.find(telemetry_id("tps", slot)).label(),
                Some("0 TPS")
            );
            for part in ["battery", "power", "full", "temperature"] {
                assert!(
                    window.try_find(telemetry_id(part, slot)).is_none(),
                    "reader errors must suppress stale {part} for slot {slot}",
                );
            }
        }
        assert_eq!(window.find(checkbox(0)).checked(), Some(true));
    })
    .expect("inspect telemetry after a reader error");
    let preferences_path = fixture.preferences_path.clone();
    drop(fixture);
    drop(ui_session);
    assert!(
        !preferences_path.exists(),
        "tracker telemetry interactions must not write preferences",
    );
}

#[gpui_kit::test]
fn tracker_telemetry_tooltips_wrap_inside_the_window(cx: &mut TestAppContext) {
    let runtime = AppRuntime::new().expect("create isolated I/O runtime");
    let directory = tempfile::tempdir().expect("create private preferences directory");
    let ui_session = crate::ui::test_support::UiTestSession::new(cx);
    let fixture = Fixture::new(
        cx,
        runtime.executor(),
        directory.path().join("preferences.json"),
    );
    fixture.workbench.update(cx, |this, cx| {
        this.monitor.fixture_telemetry(
            &fixture.receiver,
            0,
            Some(TrackerBattery {
                percent: 100,
                voltage_v: 4.31,
                charged: true,
                plugged: true,
            }),
            Some(-3.0),
        );
        this.monitor.fixture_telemetry(
            &fixture.receiver,
            1,
            Some(TrackerBattery {
                percent: 72,
                voltage_v: 4.0,
                charged: false,
                plugged: false,
            }),
            None,
        );
        cx.notify();
    });
    cx.simulate_window_scale_factor_change(fixture.window, 1.25);
    for (language, width, rem) in [
        (crate::settings::Language::English, 800., 14.),
        (crate::settings::Language::English, 360., 20.),
        (crate::settings::Language::SimplifiedChinese, 800., 14.),
        (crate::settings::Language::SimplifiedChinese, 360., 20.),
    ] {
        cx.update(|cx| {
            let preferences = fixture.workbench.update(cx, |this, _| {
                this.preferences.language = language;
                this.preferences.clone()
            });
            crate::ui::theme::apply(&preferences, cx);
            gpui_kit::component::Theme::update(cx, |theme| theme.font_size = px(rem));
        });
        cx.simulate_window_resize(fixture.window, size(px(width), px(1200.)));
        settle_roster(cx, &fixture);
        cx.update_window(fixture.window, |_, window, _| {
            assert_temperature_position(window, 0);
            assert_battery_indicators_clustered(window, 0, &["power", "full"]);
            let full = window.find(telemetry_id("full", 0));
            let power = window.find(telemetry_id("power", 0));
            let full_label = crate::i18n::tr("devices.tracker.full");
            assert_eq!(full.label(), Some(full_label.as_str()));
            assert_eq!(full.bounds().size, power.bounds().size);
            assert!(full.bounds().size.width <= window.rem_size());
        })
        .expect("inspect the icon and temperature before hovering");
        for (part, slot, key, should_wrap) in [
            ("battery", 0, "devices.tracker.power_help", true),
            ("power", 0, "devices.tracker.power_help", true),
            ("full", 0, "devices.tracker.full_help", true),
            ("temperature", 0, "devices.tracker.temperature_help", true),
            ("battery", 1, "devices.tracker.battery_help", false),
        ] {
            cx.update_window(fixture.window, |_, window, cx| {
                window.hover(telemetry_id(part, slot), cx);
            })
            .expect("hover a real telemetry value");
            cx.run_until_parked();
            // GPUI's native tooltip has a 500 ms show delay.
            cx.executor()
                .advance_clock(std::time::Duration::from_millis(600));
            settle_roster(cx, &fixture);
            cx.update_window(fixture.window, |_, window, _| {
                let tooltip = window.find("tracker-telemetry-tooltip");
                let bounds = tooltip.bounds();
                let hint = crate::i18n::tr(key);
                let expected = if part == "battery" {
                    let value = if slot == 0 { "100%(4.3V)" } else { "72%(4.0V)" };
                    format!("{value} · {hint}")
                } else {
                    hint
                };
                assert_eq!(tooltip.label(), Some(expected.as_str()));
                assert!(
                    tooltip.visible()
                        && bounds.size.width > px(0.)
                        && bounds.size.height > px(0.)
                        && bounds.left() >= px(0.)
                        && bounds.right() <= window.viewport_size().width
                        && bounds.top() >= px(0.)
                        && bounds.bottom() <= window.viewport_size().height,
                    "{part} tooltip must fit the window at width {width}, rem {rem}: {bounds:?}",
                );
                if should_wrap {
                    assert!(
                        bounds.size.height > window.rem_size() * 1.4,
                        "{part} tooltip must wrap rather than overflow: {bounds:?}",
                    );
                }
            })
            .expect("inspect the rendered tooltip geometry");
        }
    }
    let preferences_path = fixture.preferences_path.clone();
    drop(fixture);
    drop(ui_session);
    assert!(
        !preferences_path.exists(),
        "tracker telemetry tooltips must not write preferences",
    );
}

#[gpui_kit::test]
fn associated_console_and_raw_data_remain_separate_and_operable(cx: &mut TestAppContext) {
    let runtime = AppRuntime::new().expect("create isolated I/O runtime");
    let directory = tempfile::tempdir().expect("create private preferences directory");
    let _ui_session = crate::ui::test_support::UiTestSession::new(cx);
    let fixture = Fixture::new(
        cx,
        runtime.executor(),
        directory.path().join("preferences.json"),
    );
    fixture.workbench.update(cx, |this, cx| {
        let console = SerialEndpoint {
            port_name: CONSOLE.into(),
            label: "Synthetic console".into(),
            manufacturer: None,
            product: None,
            kind: SerialPortKind::Usb,
            serial_number: fixture.receiver.serial_number.clone(),
            vid: Some(0x1209),
            pid: Some(0x7690),
            usb_interface: None,
        };
        let raw_data = SerialEndpoint {
            port_name: RAW_DATA.into(),
            label: "Synthetic raw data".into(),
            usb_interface: Some(UsbInterface {
                number: Some(2),
                description: InterfaceDescription::Value("SlimeNRF Raw Data CDC".into()),
            }),
            ..console.clone()
        };
        this.inventory.ports.extend([console, raw_data]);
        cx.notify();
    });
    cx.simulate_window_resize(fixture.window, size(px(1000.), px(1200.)));
    settle_roster(cx, &fixture);
    cx.update_window(fixture.window, |_, window, cx| {
        let count = window.find(format!("receiver-count:{RECEIVER}"));
        let console = window.find(format!("port-focus:{RECEIVER}:{CONSOLE}"));
        let raw_data = window.find(format!("non-console-port:{RECEIVER}:{RAW_DATA}"));
        assert!(console.visible() && raw_data.visible());
        assert!(
            console.bounds().top() >= count.bounds().bottom()
                && console.bounds().top() - count.bounds().bottom() <= window.rem_size() * 1.75,
            "the associated console row must directly follow receiver metadata",
        );
        assert!(
            raw_data.bounds().top() >= console.bounds().bottom(),
            "raw data remains a distinct row after the command console",
        );
        assert!(
            window
                .try_find(format!("receiver-missing-console:{RECEIVER}"))
                .is_none()
        );
        assert!(
            window
                .try_find(format!("port-focus:{RECEIVER}:{RAW_DATA}"))
                .is_none()
        );
        assert!(
            window
                .try_find(format!("port-focus:direct:{CONSOLE}"))
                .is_none()
        );
        assert!(
            window
                .try_find(format!("port-focus:direct:{RAW_DATA}"))
                .is_none()
        );
        window.click(format!("check:port:{CONSOLE}"), cx);
        assert_eq!(
            window.find(format!("check:port:{CONSOLE}")).checked(),
            Some(true),
        );
        window.click(format!("port-focus:{RECEIVER}:{CONSOLE}"), cx);
    })
    .expect("operate the synthetic associated console without connecting");
    fixture.workbench.read_with(cx, |this, _| {
        assert_eq!(
            this.command_focus,
            Some(CommandFocus::Serial(CONSOLE.into())),
        );
        assert!(this.selected.contains(&format!("port:{CONSOLE}")));
        assert_eq!(this.manager.sessions().count(), 0);
    });

    fixture.workbench.update(cx, |this, cx| {
        this.inventory
            .ports
            .retain(|port| port.port_name != CONSOLE);
        cx.notify();
    });
    settle_roster(cx, &fixture);
    cx.update_window(fixture.window, |_, window, _| {
        assert!(
            window
                .find(format!("receiver-missing-console:{RECEIVER}"))
                .visible()
        );
        assert!(
            window
                .find(format!("non-console-port:{RECEIVER}:{RAW_DATA}"))
                .visible()
        );
        assert!(
            window
                .try_find(format!("port-focus:{RECEIVER}:{CONSOLE}"))
                .is_none()
        );
    })
    .expect("inspect the missing-console and raw-data paths");
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
