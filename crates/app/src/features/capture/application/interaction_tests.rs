use super::{CaptureTrackerCard, Workbench};
use crate::{
    discovery::ReceiverMonitor,
    domain::{ReceiverEndpoint, TrackerLink},
    features::capture::{
        device::{CaptureMode, CaptureSource, CaptureTransport},
        session::CapturePreparation,
    },
};
use gpui_kit::{
    AppContext as _, Bounds, Context, Entity, IntoElement, Render, Role, SharedString,
    TestAppContext, Window, WindowBounds, WindowOptions, div, point,
    prelude::*,
    px, size,
    test::{TestQueryExt as _, TestWindowExt as _},
};

struct CaptureCardHarness {
    workbench: Entity<Workbench>,
    tracker: TrackerLink,
}

impl Render for CaptureCardHarness {
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
                .child(gpui_kit::component::button::Button::new("capture-start").label("Capture"))
                .child(this.capture_tracker_card(
                    CaptureTrackerCard {
                        tracker: &self.tracker,
                        address: self.tracker.address.clone().unwrap(),
                        tps: 0,
                    },
                    cx,
                ))
        })
    }
}

#[gpui_kit::test]
fn capture_card_has_one_whole_card_keyboard_action_and_rejects_busy_activation(
    cx: &mut TestAppContext,
) {
    let _ui_session = crate::test_support::UiTestSession::new(cx);
    let runtime = crate::app_runtime::AppRuntime::new().expect("create isolated runtime");
    let executor = runtime.executor();
    let source = CaptureSource {
        receiver: ReceiverEndpoint {
            key: "capture-card-fixture".into(),
            path: b"synthetic-capture-card".to_vec(),
            label: "Synthetic capture receiver".into(),
            serial_number: None,
        },
        transport: CaptureTransport::RawHid {
            path: b"synthetic-raw-interface".to_vec(),
        },
    };
    let tracker = TrackerLink {
        receiver_key: source.receiver.key.clone(),
        slot: 2,
        address: Some("112233445566".into()),
        online: false,
    };
    let key = format!(
        "{:?}",
        (&tracker.receiver_key, tracker.slot, "112233445566")
    );
    let card_id = SharedString::from(format!("capture-card:{key}"));
    let initial_source = source.clone();
    let (window, harness) = cx.update(|cx| {
        gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    Default::default(),
                    size(px(480.), px(360.)),
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
                    this.inventory
                        .receivers
                        .push(initial_source.receiver.clone());
                    this.monitor = ReceiverMonitor::fixture(
                        initial_source.receiver.clone(),
                        2,
                        "112233445566",
                        false,
                    );
                    this.capture.sources = vec![initial_source];
                    this.capture.source = Some(0);
                    this.capture.acknowledged = true;
                    this
                });
                let focus = workbench.read(cx).page_focus.clone();
                focus.focus(window, cx);
                cx.new(|_| CaptureCardHarness { workbench, tracker })
            },
        )
        .expect("open production capture target card")
    });
    let workbench = cx.update(|cx| harness.read(cx).workbench.clone());
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find(card_id.clone()).role(), Some(Role::Button));
        assert_eq!(window.find_all_by_role(Role::Button).len(), 2);
        assert!(window.try_find(format!("capture-target:{key}")).is_none());
        assert_eq!(window.find(card_id.clone()).checked(), Some(false));
        window.hover(card_id.clone(), cx);
        assert!(workbench.read(cx).capture.target.is_none());
        assert!(workbench.read(cx).capture.acknowledged);
        window.press("tab", cx);
        assert_eq!(window.find("capture-start").focused(), Some(true));
        window.press("tab", cx);
        assert_eq!(window.find(card_id.clone()).focused(), Some(true));
        window.press("enter", cx);
    })
    .expect("select an offline capture target through the whole-card action");
    cx.run_until_parked();
    workbench.read_with(cx, |this, _| {
        assert_eq!(this.capture.target, Some((2, "112233445566".into())));
        assert!(!this.capture.acknowledged);
        assert!(!this.capture.busy());
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find(card_id.clone()).checked(), Some(true));
        window.hover(card_id.clone(), cx);
        assert_eq!(
            workbench.read(cx).capture.target,
            Some((2, "112233445566".into()))
        );
        assert!(!workbench.read(cx).capture.acknowledged);
    })
    .expect("retain the selected card's semantic state while hovered");
    workbench.update(cx, |this, cx| {
        this.capture.target = None;
        this.capture.acknowledged = true;
        cx.notify();
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.press("space", cx);
    })
    .expect("select a capture target through Space");
    cx.run_until_parked();
    workbench.read_with(cx, |this, _| {
        assert_eq!(this.capture.target, Some((2, "112233445566".into())));
        assert!(!this.capture.acknowledged);
    });
    workbench.update(cx, |this, cx| {
        this.capture.target = None;
        this.capture.acknowledged = true;
        cx.notify();
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        let bounds = window.find(card_id.clone()).bounds();
        // The lower metadata surface must share the same semantic action as its heading.
        window.click_at(
            card_id.clone(),
            point(px(12.), bounds.size.height - px(12.)),
            cx,
        );
        assert_eq!(
            workbench.read(cx).capture.target,
            Some((2, "112233445566".into()))
        );
        assert!(!workbench.read(cx).capture.acknowledged);
    })
    .expect("select through the card's metadata surface");
    workbench.update(cx, |this, cx| {
        this.capture.acknowledged = true;
        let preparation = CapturePreparation::freeze(
            source,
            CaptureMode::AllPaired { rate_hz: 100 },
            "unused-capture-card.snr".into(),
            &this.inventory,
            &this.manager,
            &this.monitor,
        )
        .expect("freeze synthetic capture without opening hardware");
        this.capture
            .session
            .prepare(preparation, &mut this.monitor)
            .unwrap();
        cx.notify();
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find(card_id.clone()).disabled(), Some(true));
        window.hover(card_id.clone(), cx);
        window.click(card_id.clone(), cx);
        window.press("space", cx);
        window.press("enter", cx);
        assert_eq!(
            workbench.read(cx).capture.target,
            Some((2, "112233445566".into()))
        );
        assert!(workbench.read(cx).capture.acknowledged);
    })
    .expect("reject pointer and keyboard activation while capture owns its preparation");
    workbench.update(cx, |this, _| {
        this.capture
            .session
            .stop(&mut this.manager, &mut this.monitor);
    });
}
