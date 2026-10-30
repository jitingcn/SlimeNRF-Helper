use crate::{
    application::{
        FeaturePage, Workbench,
        flash::{
            FlashState, ImageDestination, Mode, make_image, ota_assignment_key, ota_target_key,
        },
    },
    domain::ReceiverEndpoint,
    features::firmware::esb_ota::{FirmwareInfo, Target},
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
    Sources,
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
                Chooser::Sources => panel = panel.child(this.flash_source_picker(cx)),
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
                        let mut this = crate::application::test_support::offline_workbench(
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
    let _ui_session = crate::test_support::UiTestSession::new(cx);
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
    let _ui_session = crate::test_support::UiTestSession::new(cx);
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

#[gpui_kit::test]
fn firmware_sources_accept_tab_enter_and_space_without_resetting_busy_state(
    cx: &mut TestAppContext,
) {
    let _ui_session = crate::test_support::UiTestSession::new(cx);
    let fixture = Fixture::new(
        cx,
        Chooser::Sources,
        FlashState {
            pending: Some(crate::application::flash::Action::Reboot(Vec::new())),
            remote_generation: 41,
            ..FlashState::default()
        },
    );
    cx.update_window(fixture.window, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find("firmware-source-local").role(),
            Some(Role::Button)
        );
        assert_eq!(window.find("firmware-source-local").checked(), Some(true));
        window.press("tab", cx);
        assert_eq!(window.find("chooser-start").focused(), Some(true));
        window.press("tab", cx);
        assert_eq!(window.find("firmware-source-local").focused(), Some(true));
        window.press("tab", cx);
        assert_eq!(window.find("firmware-source-remote").focused(), Some(true));
        window.press("enter", cx);
    })
    .expect("switch to the online source with Enter");
    cx.run_until_parked();
    cx.update_window(fixture.window, |_, window, cx| {
        window.render_frame(cx);
        assert!(fixture.workbench.read(cx).flash.remote_view);
        assert_eq!(window.find("firmware-source-remote").checked(), Some(true));
        window.press("shift-tab", cx);
        assert_eq!(window.find("firmware-source-local").focused(), Some(true));
        window.press("space", cx);
    })
    .expect("switch back to the local source with Space");
    cx.run_until_parked();
    fixture.workbench.read_with(cx, |this, _| {
        assert!(!this.flash.remote_view);
        assert_eq!(this.flash.remote_generation, 41);
        assert!(matches!(
            this.flash.pending,
            Some(crate::application::flash::Action::Reboot(_))
        ));
        assert!(this.flash.job.is_none());
    });
}

#[gpui_kit::test]
fn uf2_preset_focus_and_activation_follow_volume_and_candidate_identity_after_reorder(
    cx: &mut TestAppContext,
) {
    use crate::{
        application::flash::Payload,
        features::firmware::{board_catalog::Catalog, bundled_resources::Resource},
    };

    let _ui_session = crate::test_support::UiTestSession::new(cx);
    let directory = tempfile::tempdir().expect("create synthetic firmware and volume directory");
    let config = directory.path().join("slimenrf-helper.yaml");
    std::fs::write(
        &config,
        br#"{"schema_version":1,"firmware_root":".","board_targets":{
            "board-a":{"targets":[{"name":"First","firmware":"first.uf2"}]},
            "board-b":{"targets":[
                {"name":"Second","firmware":"second.uf2"},
                {"name":"Shared","firmware":"second.uf2"},
                {"name":"Alternative","firmware":"alternative.uf2"}
            ]}
        }}"#,
    )
    .expect("write synthetic firmware catalog");
    let mut bytes = vec![0; 512];
    for (offset, value) in [
        (0, 0x0a32_4655u32),
        (4, 0x9e5d_5157),
        (8, 0x2000),
        (12, 0x1000),
        (16, 256),
        (20, 0),
        (24, 1),
        (28, 0xada5_2840),
        (508, 0x0ab1_6f30),
    ] {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    for filename in ["first.uf2", "second.uf2", "alternative.uf2"] {
        std::fs::write(directory.path().join(filename), &bytes).expect("write valid synthetic UF2");
    }
    let mut volumes = Vec::new();
    for board in ["board-a", "board-b"] {
        let mount = directory.path().join(board);
        std::fs::create_dir(&mount).expect("create a synthetic volume directory");
        std::fs::write(mount.join("INFO_UF2.TXT"), format!("Board-ID: {board}\n"))
            .expect("write a synthetic volume marker");
        volumes
            .push(crate::features::firmware::uf2::inspect_volume(&mount, "INFO_UF2.TXT").unwrap());
    }
    let identities: Vec<_> = volumes
        .iter()
        .map(|volume| volume.identity.clone())
        .collect();
    let expected = Resource::File(directory.path().join("second.uf2"));
    let fixture = Fixture::new(
        cx,
        Chooser::Sources,
        FlashState {
            catalog: Some(Catalog::load(&config).expect("load synthetic firmware catalog")),
            catalog_loaded: true,
            volumes,
            ..FlashState::default()
        },
    );
    let label = format!(
        "second.uf2 · {} · Second",
        crate::i18n::tr("firmware.source.configured")
    );
    let shared_label = format!(
        "second.uf2 · {} · Shared",
        crate::i18n::tr("firmware.source.configured")
    );
    cx.update_window(fixture.window, |_, window, cx| {
        window.render_frame(cx);
        for _ in 0..8 {
            window.press("tab", cx);
            if window.find_by_label(&label).focused() == Some(true) {
                break;
            }
        }
        assert_eq!(window.find_by_label(&label).focused(), Some(true));
        assert_eq!(window.find_by_label(&shared_label).focused(), Some(false));
    })
    .expect("focus the second volume's actual configured firmware choice");
    for (direction, activation, selected_label, other_label) in [
        ("tab", "space", &shared_label, &label),
        ("shift-tab", "enter", &label, &shared_label),
    ] {
        cx.update_window(fixture.window, |_, window, cx| {
            window.render_frame(cx);
            window.press(direction, cx);
            assert_eq!(window.find_by_label(selected_label).focused(), Some(true));
            assert_eq!(window.find_by_label(other_label).focused(), Some(false));
            window.press(activation, cx);
            assert!(fixture.workbench.read(cx).flash.job.is_some());
        })
        .expect("activate each distinct named choice for the shared firmware resource");
        fixture.workbench.update(cx, |this, cx| {
            let mut completed = None;
            crate::sessions::test_support::until(|| {
                completed = this.flash.poll_job();
                completed.is_some()
            });
            let (result, _) = completed.expect("named preset image worker joined");
            let Payload::Images(ImageDestination::Global, images) =
                result.expect("load named preset firmware")
            else {
                panic!("a named preset must load its configured image, not touch hardware");
            };
            assert_eq!(images.len(), 1);
            assert_eq!(images[0].resource.as_deref(), Some(&expected));
            assert!(images[0].validation_error.is_none());
            assert!(this.flash.review.is_none());
            assert!(this.flash.selected_volumes.is_empty());
            this.flash.finish_job_progress(None);
            cx.notify();
        });
    }
    fixture.workbench.update(cx, |this, cx| {
        this.flash.volumes.reverse();
        this.flash
            .catalog
            .as_mut()
            .unwrap()
            .board_targets
            .get_mut("board-b")
            .unwrap()
            .targets
            .swap(0, 1);
        cx.notify();
    });
    cx.update_window(fixture.window, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find_by_label(&label).focused(), Some(true));
        assert_eq!(window.find_by_label(&shared_label).focused(), Some(false));
        window.press("enter", cx);
        assert!(fixture.workbench.read(cx).flash.job.is_some());
    })
    .expect("activate the same candidate after both volume and candidate reorder");
    fixture.workbench.update(cx, |this, _| {
        let mut completed = None;
        crate::sessions::test_support::until(|| {
            completed = this.flash.poll_job();
            completed.is_some()
        });
        let (result, _) = completed.expect("configured image worker joined");
        let Payload::Images(ImageDestination::Global, images) =
            result.expect("load selected firmware")
        else {
            panic!("configured firmware activation must load an image, not touch hardware");
        };
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].resource.as_deref(), Some(&expected));
        assert!(images[0].validation_error.is_none());
        assert_eq!(this.flash.volumes[0].identity, identities[1]);
        assert_eq!(this.flash.volumes[1].identity, identities[0]);
        assert!(this.flash.review.is_none());
        assert!(this.flash.selected_volumes.is_empty());
    });
}
