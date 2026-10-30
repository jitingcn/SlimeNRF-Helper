use super::{TrackerSection, Workbench};
use crate::{
    application::flash::{Action, Mode},
    discovery::ReceiverMonitor,
    domain::{ReceiverEndpoint, TrackerLink},
};
use gpui_kit::{
    AnyWindowHandle, AppContext as _, Bounds, Context, Entity, IntoElement, Render, Role,
    TestAppContext, Window, WindowBounds, WindowOptions, div, prelude::*, px, size,
    test::TestWindowExt as _,
};

const RECEIVER: &str = "ota-card-fixture";
const KEY: &str = "tracker:ota-card-fixture:2";
const BODY: &str = "ota-content-tracker:ota-card-fixture:2";
const CHECKBOX: &str = "ota-tracker:ota-card-fixture:2";
const RETRY: &str = "ota-query-tracker-ota-card-fixture-2";

struct TrackerHarness {
    workbench: Entity<Workbench>,
    tracker: TrackerLink,
}

impl Render for TrackerHarness {
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
                .child(gpui_kit::component::button::Button::new("ota-start").label("Targets"))
                .child(this.flash_esb_tracker(&self.tracker, &TrackerSection::Unknown, cx))
        })
    }
}

struct Fixture {
    window: AnyWindowHandle,
    harness: Entity<TrackerHarness>,
    workbench: Entity<Workbench>,
    receiver: ReceiverEndpoint,
    _runtime: crate::app_runtime::AppRuntime,
}

impl Fixture {
    fn new(cx: &mut TestAppContext) -> Self {
        let runtime = crate::app_runtime::AppRuntime::new().expect("create isolated runtime");
        let executor = runtime.executor();
        let receiver = ReceiverEndpoint {
            key: RECEIVER.into(),
            path: b"synthetic-ota-card".to_vec(),
            label: "Synthetic receiver".into(),
            serial_number: None,
        };
        let initial_receiver = receiver.clone();
        let tracker = TrackerLink {
            receiver_key: RECEIVER.into(),
            slot: 2,
            address: Some("112233445566".into()),
            online: true,
        };
        let (window, harness) = cx.update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        Default::default(),
                        size(px(520.), px(440.)),
                    ))),
                    ..Default::default()
                },
                cx,
                |window, cx| {
                    let workbench = cx.new(|cx| {
                        let mut this = crate::application::test_support::offline_workbench(
                            window,
                            cx,
                            crate::sessions::test_support::display_manager(),
                            executor,
                        );
                        this.active_page = crate::application::FeaturePage::Ota;
                        this.flash.mode = Mode::Esb;
                        this.inventory.receivers.push(initial_receiver.clone());
                        this.monitor =
                            ReceiverMonitor::fixture(initial_receiver, 2, "112233445566", true);
                        this
                    });
                    let focus = workbench.read(cx).page_focus.clone();
                    focus.focus(window, cx);
                    cx.new(|_| TrackerHarness { workbench, tracker })
                },
            )
            .expect("open production OTA tracker card")
        });
        let workbench = cx.update(|cx| harness.read(cx).workbench.clone());
        Self {
            window,
            harness,
            workbench,
            receiver,
            _runtime: runtime,
        }
    }
}

#[gpui_kit::test]
fn ota_content_and_checkbox_toggle_once_from_pointer_enter_and_space(cx: &mut TestAppContext) {
    let _ui_session = crate::test_support::UiTestSession::new(cx);
    let fixture = Fixture::new(cx);
    cx.update_window(fixture.window, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find(BODY).role(), Some(Role::Button));
        window.hover(BODY, cx);
        assert!(fixture.workbench.read(cx).selected.is_empty());
        window.click(BODY, cx);
        assert!(fixture.workbench.read(cx).selected.contains(KEY));
        window.click(CHECKBOX, cx);
        assert!(!fixture.workbench.read(cx).selected.contains(KEY));
        window.press("tab", cx);
        assert_eq!(window.find("ota-start").focused(), Some(true));
        window.press("tab", cx);
        assert_eq!(window.find(CHECKBOX).focused(), Some(true));
        window.press("space", cx);
    })
    .expect("toggle each pointer surface and keyboard checkbox once");
    cx.run_until_parked();
    assert!(
        fixture
            .workbench
            .read_with(cx, |this, _| this.selected.contains(KEY))
    );
    cx.update_window(fixture.window, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find(CHECKBOX).checked(), Some(true));
        assert_eq!(window.find(BODY).checked(), Some(true));
        window.hover(BODY, cx);
        assert!(fixture.workbench.read(cx).selected.contains(KEY));
        window.press("tab", cx);
        assert_eq!(window.find(BODY).focused(), Some(true));
        window.press("enter", cx);
    })
    .expect("activate the semantic content button with Enter");
    cx.run_until_parked();
    assert!(
        !fixture
            .workbench
            .read_with(cx, |this, _| this.selected.contains(KEY))
    );
    cx.update_window(fixture.window, |_, window, cx| {
        window.render_frame(cx);
        window.press("space", cx);
    })
    .expect("activate the semantic content button with Space");
    cx.run_until_parked();
    assert!(
        fixture
            .workbench
            .read_with(cx, |this, _| this.selected.contains(KEY))
    );
}

#[gpui_kit::test]
fn ota_disabled_controls_and_retry_do_not_change_selection(cx: &mut TestAppContext) {
    let _ui_session = crate::test_support::UiTestSession::new(cx);
    let fixture = Fixture::new(cx);
    fixture.workbench.update(cx, |this, cx| {
        this.flash
            .query_failures
            .insert(format!("{RECEIVER}:2"), "Synthetic query failure".into());
        cx.notify();
    });
    cx.update_window(fixture.window, |_, window, cx| {
        window.render_frame(cx);
        window.click(RETRY, cx);
        let this = fixture.workbench.read(cx);
        let Some(Action::QueryGroups(plan)) = this.flash.pending.as_ref() else {
            panic!("retry must reserve its real scoped query");
        };
        assert_eq!(plan.groups.len(), 1);
        assert_eq!(plan.groups[0].1.len(), 1);
        assert_eq!(plan.groups[0].1[0].slot, 2);
        assert!(
            this.selected.is_empty(),
            "retry must not select its surrounding card"
        );
    })
    .expect("retry without bubbling into selection");
    cx.update_window(fixture.window, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find(RETRY).disabled(), Some(true));
        assert_eq!(window.find(BODY).disabled(), Some(true));
        assert_eq!(window.find(CHECKBOX).disabled(), Some(true));
        window.click(RETRY, cx);
        window.click(CHECKBOX, cx);
        window.click(BODY, cx);
        assert!(fixture.workbench.read(cx).selected.is_empty());
    })
    .expect("disabled retry and selection controls remain independent");
    fixture.workbench.update(cx, |this, cx| {
        this.flash.pending = None;
        this.release_flash_lease();
        this.flash.finish_job_progress(None);
        this.monitor = ReceiverMonitor::fixture(fixture.receiver.clone(), 2, "112233445566", false);
        cx.notify();
    });
    fixture.harness.update(cx, |this, cx| {
        this.tracker.online = false;
        cx.notify();
    });
    cx.update_window(fixture.window, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find(BODY).disabled(), Some(true));
        assert_eq!(window.find(CHECKBOX).disabled(), Some(true));
        window.hover(BODY, cx);
        assert!(fixture.workbench.read(cx).selected.is_empty());
        window.click(BODY, cx);
        window.click(CHECKBOX, cx);
        window.press("tab", cx);
        assert_eq!(window.find("ota-start").focused(), Some(true));
        window.press("tab", cx);
        assert_eq!(window.find("ota-start").focused(), Some(true));
        window.press("space", cx);
        window.press("enter", cx);
        assert!(fixture.workbench.read(cx).selected.is_empty());
        assert!(fixture.workbench.read(cx).flash.pending.is_none());
    })
    .expect("offline unselected targets reject activation and leave no authorization");
}
