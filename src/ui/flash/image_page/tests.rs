use crate::{
    domain::ReceiverEndpoint,
    esb_ota::{FirmwareInfo, Target},
    ui::{
        FeaturePage, Workbench,
        flash::{
            FlashState, ImageDestination, Mode, make_image, ota_assignment_key, ota_target_key,
        },
    },
};
use gpui_kit::{
    AnyWindowHandle, AppContext as _, Bounds, Context, Entity, IntoElement, Render, Role,
    SharedString, TestAppContext, Window, WindowBounds, WindowOptions, div,
    prelude::*,
    px, size,
    test::{TestQueryExt as _, TestWindowExt as _},
};

#[derive(Clone, Copy)]
enum Chooser {
    Groups,
    Images,
}

struct ChooserHarness {
    workbench: Entity<Workbench>,
    chooser: Chooser,
    reversed: bool,
}

impl Render for ChooserHarness {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.workbench.update(cx, |this, cx| {
            let mut panel = div()
                .size_full()
                .track_focus(&this.page_focus)
                .tab_group()
                .flex()
                .flex_col()
                .p_3()
                .gap_2()
                .child(gpui_kit::component::button::Button::new("chooser-start").label("Firmware"));
            match self.chooser {
                Chooser::Groups => {
                    let mut groups = this.ota_assignment_groups();
                    if self.reversed {
                        groups.reverse();
                    }
                    for group in groups {
                        panel = panel.child(this.flash_ota_assignment_row(&group, cx));
                    }
                }
                Chooser::Images => {
                    for index in 0..this.flash.images.len() {
                        panel = panel.child(this.flash_image_variant(index, cx));
                    }
                }
            }
            panel
        })
    }
}

struct Fixture {
    window: AnyWindowHandle,
    harness: Entity<ChooserHarness>,
    workbench: Entity<Workbench>,
    _runtime: crate::app_runtime::AppRuntime,
}

impl Fixture {
    fn new(cx: &mut TestAppContext, chooser: Chooser, flash: FlashState) -> Self {
        let runtime = crate::app_runtime::AppRuntime::new().expect("create isolated I/O runtime");
        let executor = runtime.executor();

        let (window, harness) = cx.update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        Default::default(),
                        size(px(640.), px(480.)),
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
                        this.active_page = if flash.mode == Mode::Esb {
                            FeaturePage::Ota
                        } else {
                            FeaturePage::Uf2
                        };
                        this.flash = flash;
                        this
                    });
                    // Root's Tab binding needs the rendered page's initial focus path.
                    let focus = workbench.read(cx).page_focus.clone();
                    focus.focus(window, cx);
                    cx.new(|_| ChooserHarness {
                        workbench,
                        chooser,
                        reversed: false,
                    })
                },
            )
            .expect("open actual firmware chooser components")
        });
        let workbench = cx.update(|cx| harness.read(cx).workbench.clone());
        Self {
            window,
            harness,
            workbench,
            _runtime: runtime,
        }
    }
}

fn tracker(board: &str, slot: u8) -> Target {
    Target {
        receiver: ReceiverEndpoint {
            key: "synthetic-chooser-receiver".into(),
            path: b"synthetic-chooser-receiver".to_vec(),
            label: "Synthetic receiver".into(),
            serial_number: None,
        },
        id: slot,
        address: Some(format!("1122334455{slot:02X}")),
        info: FirmwareInfo {
            version: "1".into(),
            build_date: "2026-01-01".into(),
            firmware_size: 4,
            bootloader: "none".into(),
            protocol_version: 1,
            board_target: board.into(),
            flash_base: 0x1000,
        },
    }
}

#[gpui_kit::test]
fn assignment_groups_are_keyboard_operable_and_keep_focus_when_reordered(cx: &mut TestAppContext) {
    let _ui_session = crate::ui::test_support::UiTestSession::new(cx);
    let first = tracker("alpha", 0);
    let second = tracker("beta", 1);
    let first_key = ota_assignment_key(&first);
    let second_key = ota_assignment_key(&second);
    let first_id = SharedString::from(format!("ota-group-{first_key}"));
    let second_id = SharedString::from(format!("ota-group-{second_key}"));
    let mut flash = FlashState {
        mode: Mode::Esb,
        targets: vec![first.clone(), second.clone()],
        selected_targets: [ota_target_key(&first), ota_target_key(&second)].into(),
        ..FlashState::default()
    };
    flash.reconcile_groups(Some(first_key.clone()));
    let generations = flash.group_generations.clone();
    let fixture = Fixture::new(cx, Chooser::Groups, flash);

    cx.update_window(fixture.window, |_, window, cx| {
        window.render_frame(cx);
        assert!(fixture.workbench.read(cx).page_focus.is_focused(window));
        assert_eq!(window.find(first_id.clone()).role(), Some(Role::Button));
        assert!(
            window
                .find(second_id.clone())
                .label()
                .unwrap()
                .contains("beta")
        );
        assert_eq!(window.find(first_id.clone()).checked(), Some(true));
        assert_eq!(window.find(second_id.clone()).checked(), Some(false));
        window.press("tab", cx);
        assert_eq!(window.find("chooser-start").focused(), Some(true));
        window.press("tab", cx);
        assert_eq!(window.find(first_id.clone()).focused(), Some(true));
        window.press("tab", cx);
        assert_eq!(window.find(second_id.clone()).focused(), Some(true));
        window.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();
    fixture.workbench.read_with(cx, |this, _| {
        assert_eq!(this.flash.active_group.as_ref(), Some(&second_key));
        assert_eq!(this.flash.group_generations, generations);
        assert!(
            this.flash.assignments.is_empty(),
            "selection must not assign firmware"
        );
        assert!(
            this.flash.review.is_none(),
            "selection must not authorize an update"
        );
    });

    fixture.harness.update(cx, |this, cx| {
        this.reversed = true;
        cx.notify();
    });
    cx.update_window(fixture.window, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find(second_id.clone()).focused(), Some(true));
        assert_eq!(window.find(second_id.clone()).checked(), Some(true));
        assert!(
            window.find(second_id.clone()).bounds().top()
                < window.find(first_id.clone()).bounds().top()
        );
        window.press("tab", cx);
        assert_eq!(window.find(first_id.clone()).focused(), Some(true));
        window.press("space", cx);
    })
    .unwrap();
    cx.run_until_parked();
    fixture.workbench.read_with(cx, |this, _| {
        assert_eq!(this.flash.active_group.as_ref(), Some(&first_key));
        assert!(matches!(
            this.flash.destination(Mode::Esb),
            Some(ImageDestination::Ota { key, generation })
                if key == first_key && generation == generations[&first_key]
        ));
        assert_eq!(this.flash.group_generations, generations);
    });
}

#[gpui_kit::test]
fn image_choices_keep_focused_identity_on_reorder_and_reject_disabled_activation(
    cx: &mut TestAppContext,
) {
    let _ui_session = crate::ui::test_support::UiTestSession::new(cx);
    let image = |name: &str, digest: &str| {
        make_image(
            name.into(),
            b":0410000001020304E2\n:00000001FF\n".to_vec(),
            "synthetic chooser".into(),
            digest.into(),
            true,
        )
    };
    let mut invalid = image("invalid.hex", "invalid");
    invalid.validation_error = Some("Invalid synthetic image".into());
    let fixture = Fixture::new(
        cx,
        Chooser::Images,
        FlashState {
            images: vec![
                image("first.hex", "first"),
                image("second.hex", "second"),
                invalid,
            ],
            images_destination: ImageDestination::Global,
            ..FlashState::default()
        },
    );
    cx.update_window(fixture.window, |_, window, cx| {
        window.render_frame(cx);
        assert!(fixture.workbench.read(cx).page_focus.is_focused(window));
        window.press("tab", cx);
        assert_eq!(window.find("chooser-start").focused(), Some(true));
        window.press("tab", cx);
        window.press("tab", cx);
        window.press("enter", cx);
        assert_eq!(
            window.find_by_label("Choose firmware second.hex").focused(),
            Some(true)
        );
    })
    .unwrap();
    fixture.workbench.read_with(cx, |this, _| {
        assert_eq!(this.flash.image.as_ref().unwrap().name, "second.hex");
    });

    fixture.workbench.update(cx, |this, cx| {
        this.flash.images.swap(0, 1);
        this.flash.image = None;
        cx.notify();
    });
    cx.update_window(fixture.window, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find_by_label("Choose firmware second.hex").focused(),
            Some(true)
        );
        window.press("enter", cx);
        window.press("tab", cx);
        assert_eq!(
            window.find_by_label("Choose firmware first.hex").focused(),
            Some(true)
        );
        window.press("tab", cx);
        assert_eq!(window.find("chooser-start").focused(), Some(true));
    })
    .unwrap();
    cx.run_until_parked();
    fixture.workbench.read_with(cx, |this, _| {
        assert_eq!(this.flash.image.as_ref().unwrap().name, "second.hex");
    });
    fixture.workbench.update(cx, |this, cx| {
        this.flash.image = None;
        cx.notify();
    });
    cx.update_window(fixture.window, |_, window, cx| {
        window.render_frame(cx);
        let invalid = window.find_by_label("Choose firmware invalid.hex");
        assert_eq!(invalid.disabled(), Some(true));
        window.click(invalid.path().last().unwrap().clone(), cx);
        assert_eq!(window.find("chooser-start").focused(), Some(true));
        window.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();
    fixture.workbench.read_with(cx, |this, _| {
        assert!(
            this.flash.image.is_none(),
            "disabled image must not be selected"
        );
        assert!(this.flash.review.is_none());
    });
}
