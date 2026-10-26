use super::Workbench;
use crate::{
    discovery::ReceiverMonitor,
    domain::{ReceiverEndpoint, TrackerLink},
    ui::command_panel::CommandFocus,
};
use gpui_kit::{
    AnyWindowHandle, AppContext as _, Bounds, Context, Entity, IntoElement, Render, Role,
    TestAppContext, Window, WindowBounds, WindowOptions, div, point, prelude::*, px, size,
    test::TestWindowExt as _,
};

const RECEIVER: &str = "synthetic-roster-receiver";

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
    _runtime: crate::app_runtime::AppRuntime,
}

impl Fixture {
    fn new(cx: &mut TestAppContext) -> Self {
        let runtime = crate::app_runtime::AppRuntime::new().expect("create isolated I/O runtime");
        let executor = runtime.executor();
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
        let workbench = cx.update(|cx| harness.read(cx).workbench.clone());
        Self {
            window,
            workbench,
            receiver,
            _runtime: runtime,
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
}

fn card(slot: u8) -> gpui_kit::SharedString {
    format!("card:tracker:{RECEIVER}:{slot}").into()
}

fn checkbox(slot: u8) -> gpui_kit::SharedString {
    format!("check:tracker:{RECEIVER}:{slot}").into()
}

fn tab_to(slot: u8, window: &mut Window, cx: &mut gpui_kit::App) {
    for _ in 0..16 {
        if window.find(card(slot)).focused() == Some(true) {
            return;
        }
        window.press("tab", cx);
    }
    panic!("tracker {slot} must be keyboard reachable");
}

#[gpui_kit::test]
fn tracker_body_and_batch_checkbox_keep_independent_pointer_keyboard_and_offline_contracts(
    cx: &mut TestAppContext,
) {
    let _ui_session = crate::ui::test_support::UiTestSession::new(cx);
    let fixture = Fixture::new(cx);
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
    let _ui_session = crate::ui::test_support::UiTestSession::new(cx);
    let fixture = Fixture::new(cx);
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
