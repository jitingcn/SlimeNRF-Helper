use super::{Section, Setting, SettingsChoice};
use crate::{
    application::Workbench,
    features::settings::store::{AccentColor, Appearance, Language},
};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Bounds, Entity, MouseButton, PlatformInput, ScrollDelta,
    ScrollEvent, ScrollHandle, TestAppContext, Window, WindowBounds, WindowOptions, point, px,
    size,
    test::{TestEventExt as _, TestWindowExt as _},
};

struct SettingsFixture {
    handle: AnyWindowHandle,
    workbench: Entity<Workbench>,
    directory: tempfile::TempDir,
    _runtime: Option<crate::app_runtime::AppRuntime>,
}

fn fixture(cx: &mut TestAppContext, width: f32, height: f32) -> SettingsFixture {
    let runtime = crate::app_runtime::AppRuntime::new().unwrap();
    let executor = runtime.executor();
    fixture_with_executor(cx, width, height, executor, Some(runtime))
}

fn fixture_with_executor(
    cx: &mut TestAppContext,
    width: f32,
    height: f32,
    executor: crate::app_runtime::Executor,
    runtime: Option<crate::app_runtime::AppRuntime>,
) -> SettingsFixture {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("preferences.json");

    let (handle, workbench) = cx.update(|cx| {
        gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    Default::default(),
                    size(px(width), px(height)),
                ))),
                ..Default::default()
            },
            cx,
            move |window, cx| {
                cx.new(|cx| {
                    let mut this = crate::application::test_support::offline_workbench(
                        window,
                        cx,
                        crate::sessions::SessionManager::new(),
                        executor,
                    );
                    this.preferences_io =
                        crate::application::preference_io::PreferenceIo::for_test(path);
                    this
                })
            },
        )
        .unwrap()
    });
    // TestPlatform does not activate newly opened windows automatically.
    // Focus-in notifications require native-window activation as on the desktop.
    cx.update_window(handle, |_, window, _| window.activate_window())
        .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        assert!(
            window.is_window_active(),
            "Settings keyboard integration requires an active native window"
        );
        window.render_frame(cx);
    })
    .unwrap();
    SettingsFixture {
        handle,
        workbench,
        directory,
        _runtime: runtime,
    }
}

#[gpui_kit::test]
fn search_filters_real_rows_and_clear_keeps_the_selected_section(cx: &mut TestAppContext) {
    let _ui_session = crate::test_support::UiTestSession::new(cx);
    let fixture = fixture(cx, 1100., 760.);
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("nav-settings", cx);
        let pane_width = fixture.workbench.read(cx).settings.layout.read(cx).sizes()[0];
        assert_eq!(
            window.pixel_snap(window.find("settings-search").bounds().size.width),
            window.pixel_snap(pane_width - window.rem_size() * 1.5),
            "search must use the standard Sidebar header width, not its intrinsic minimum",
        );
        window.press("tab", cx);
        assert_eq!(window.find("settings-search").focused(), Some(true));
        window.input("Sakura", cx);
        assert!(window.find("settings-accent").visible());
        assert!(window.try_find("settings-appearance").is_none());
        assert!(window.try_find("settings-language").is_none());
        window.click("settings-section-appearance", cx);
        assert_eq!(
            window.find("settings-section-appearance").checked(),
            Some(true)
        );
        assert_eq!(window.find("settings-search").value(), Some("Sakura"));

        window.click("settings-search", cx);
        window.press("secondary-a", cx);
        window.input("no-setting-can-match-this-query", cx);
        assert!(window.find("settings-clear-search").visible());
        assert!(window.try_find("settings-accent").is_none());
        window.click("settings-clear-search", cx);
        assert_eq!(window.find("settings-search").value(), Some(""));
        assert_eq!(window.find("settings-search").focused(), Some(true));
        assert_eq!(
            window.find("settings-section-appearance").checked(),
            Some(true)
        );
        assert!(window.find("settings-language").visible());
    })
    .unwrap();
}

#[gpui_kit::test]
fn update_search_finds_the_about_section_in_english_and_chinese(cx: &mut TestAppContext) {
    let _ui_session = crate::test_support::UiTestSession::new(cx);
    let fixture = fixture(cx, 1100., 900.);
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("nav-settings", cx);
        window.click("settings-search", cx);
        window.input("prerelease", cx);
        assert!(window.find("app-update-status").visible());
        assert!(window.find("app-update-prereleases").visible());
        assert!(window.find("app-update-check").visible());
        assert!(window.try_find("settings-language").is_none());
        window.click("settings-section-about", cx);
        assert_eq!(window.find("settings-section-about").checked(), Some(true));

        window.click("settings-search", cx);
        window.press("secondary-a", cx);
        window.input("预发布", cx);
        assert!(window.find("app-update-status").visible());
        assert!(window.find("app-update-prereleases").visible());
        assert!(window.find("app-update-check").visible());
        assert!(window.try_find("settings-language").is_none());
        assert_eq!(window.find("settings-section-about").checked(), Some(true));
        assert_eq!(window.find("settings-search").value(), Some("预发布"));
    })
    .unwrap();
}

#[gpui_kit::test]
fn category_jumps_reveal_controls_and_locale_refresh_keeps_view_state(cx: &mut TestAppContext) {
    let _ui_session = crate::test_support::UiTestSession::new(cx);
    let fixture = fixture(cx, 1100., 620.);
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("nav-settings", cx);
        window.press("end", cx);
        let scroll = &fixture.workbench.read(cx).settings.scroll;
        assert!(scroll.max_offset().y > px(0.));
        assert!(
            f32::from(scroll.offset().y + scroll.max_offset().y).abs() < 0.001,
            "End must reveal the bottom of the now-longer About section"
        );
        window.press("home", cx);
        assert!(window.find("settings-language").visible());
        window.click("settings-section-about", cx);
        assert!(fixture.workbench.read(cx).page_focus.is_focused(window));
        window.simulate_next_frame(cx);
        window.render_frame(cx);
        assert!(window.find("open-notices").visible());
        assert_eq!(window.find("settings-section-about").checked(), Some(true));
        window.click("settings-section-general", cx);
        window.simulate_next_frame(cx);
        window.render_frame(cx);
        assert!(window.find("settings-language").visible());
        window.click("settings-search", cx);
        window.input("language", cx);
        let input_id = fixture.workbench.read(cx).settings.search.entity_id();
        fixture.workbench.update(cx, |this, cx| {
            let mut preferences = this.preferences.clone();
            preferences.language = Language::SimplifiedChinese;
            // Exercise the owner with inventory admission already occupied, without device IO.
            this.scanning_inventory = true;
            this.update_preferences(preferences, window, cx);
            assert!(this.inventory_task.is_none());
            this.scanning_inventory = false;
        });
        window.render_frame(cx);
        assert_eq!(
            fixture.workbench.read(cx).settings.search.entity_id(),
            input_id
        );
        assert_eq!(window.find("settings-search").value(), Some("language"));
        let placeholder = rust_i18n::t!(
            "app.settings.search.placeholder",
            locale = Language::SimplifiedChinese.code()
        );
        assert_eq!(
            window.find("settings-search").placeholder(),
            Some(placeholder.as_ref())
        );
        assert_eq!(
            window.find("settings-section-general").checked(),
            Some(true)
        );
        assert_eq!(window.find("settings-search").focused(), Some(true));
    })
    .unwrap();
    cx.run_until_parked();
}

fn settings_key(cx: &mut TestAppContext, fixture: &SettingsFixture, key: &str) {
    cx.update_window(fixture.handle, |_, window, cx| {
        window.press(key, cx);
    })
    .unwrap();
    settle_settings(cx, fixture);
}

fn assert_scrollbar_reaches_both_edges(
    window: &mut Window,
    cx: &mut App,
    scroll: &ScrollHandle,
    region: &'static str,
) {
    let viewport = scroll.bounds();
    let extent = scroll.max_offset().y;
    assert!(extent > px(0.), "{region} must overflow");
    let position = viewport.center();
    window.pointer_move(position, None, cx);
    window.dispatch_event(
        PlatformInput::Scroll(ScrollEvent {
            position,
            delta: ScrollDelta::Pixels(point(px(0.), -extent * 0.5)),
            ..Default::default()
        }),
        cx,
    );
    window.render_frame(cx);
    assert!(
        scroll.offset().y < px(0.) && scroll.offset().y > -extent,
        "{region} must start the pointer test between the scroll edges"
    );
    assert_eq!(
        scroll.bounds(),
        viewport,
        "scrolling must not move the viewport"
    );

    let x = viewport.right() - px(8.);
    let track_bottom = point(x, viewport.bottom() - px(1.));
    window.pointer_move(track_bottom, None, cx);
    window.pointer_down(track_bottom, MouseButton::Left, cx);
    window.pointer_up(track_bottom, MouseButton::Left, cx);
    assert!(
        f32::from(scroll.offset().y + extent).abs() < 0.001,
        "the track at the viewport's bottom must reach the content end within floating-point precision"
    );
    assert_eq!(scroll.bounds(), viewport);

    let thumb_bottom = point(x, viewport.bottom() - px(12.));
    let track_top = point(x, viewport.top() + px(4.));
    window.pointer_move(thumb_bottom, None, cx);
    window.pointer_down(thumb_bottom, MouseButton::Left, cx);
    window.pointer_move(track_top, Some(MouseButton::Left), cx);
    window.pointer_up(track_top, MouseButton::Left, cx);
    assert_eq!(
        scroll.offset().y,
        px(0.),
        "dragging the thumb to the viewport's top must reach the content start"
    );
    assert_eq!(scroll.bounds(), viewport);
}

#[gpui_kit::test]
fn settings_scrollbars_track_viewports_during_pointer_navigation(cx: &mut TestAppContext) {
    let _ui_session = crate::test_support::UiTestSession::new(cx);
    let fixture = fixture(cx, 760., 360.);
    cx.simulate_window_scale_factor_change(fixture.handle, 1.25);
    cx.update(|cx| {
        gpui_kit::component::Theme::update(cx, |theme| theme.font_size = px(20.));
    });
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("nav-settings", cx);
    })
    .unwrap();
    settle_settings(cx, &fixture);
    cx.update_window(fixture.handle, |_, window, cx| {
        let body = fixture.workbench.read(cx).settings.scroll.clone();
        let navigation = fixture
            .workbench
            .read(cx)
            .settings
            .navigation_scroll
            .clone();
        assert_scrollbar_reaches_both_edges(window, cx, &body, "settings body");
        assert_scrollbar_reaches_both_edges(window, cx, &navigation, "settings navigation");
        assert!(
            window
                .find((gpui_kit::ElementId::from(Section::General.id()), "anchor"))
                .visible(),
            "the first settings section must be visible at the top of the body"
        );
        assert!(window.find("settings-search").visible());
        window.click("settings-search", cx);
        assert_eq!(window.find("settings-search").focused(), Some(true));
    })
    .unwrap();
}

fn settle_settings(cx: &mut TestAppContext, fixture: &SettingsFixture) {
    // Drawing alone does not deliver section-jump or focus-reveal frame callbacks.
    for _ in 0..2 {
        cx.run_until_parked();
        cx.update_window(fixture.handle, |_, window, cx| {
            window.simulate_next_frame(cx);
            window.render_frame(cx);
        })
        .unwrap();
    }
}

fn tab_to_setting(
    cx: &mut TestAppContext,
    fixture: &SettingsFixture,
    target: &'static str,
    key: &str,
) {
    for _ in 0..32 {
        let focused = cx
            .update_window(fixture.handle, |_, window, _| {
                window.find(target).focused() == Some(true)
            })
            .unwrap();
        if focused {
            break;
        }
        settings_key(cx, fixture, key);
    }
    assert_setting_focus_visible(cx, fixture, target);
}

fn assert_setting_focus_visible(
    cx: &mut TestAppContext,
    fixture: &SettingsFixture,
    target: &'static str,
) {
    cx.update_window(fixture.handle, |_, window, cx| {
        let control = window.find(target);
        let viewport = fixture.workbench.read(cx).settings.scroll.bounds();
        assert_eq!(
            control.focused(),
            Some(true),
            "{target} must own native focus"
        );
        assert!(
            control.bounds().top() >= viewport.top(),
            "{target} is clipped above its scroll viewport"
        );
        assert!(
            control.bounds().bottom() <= viewport.bottom(),
            "{target} is clipped below its scroll viewport"
        );
        assert!(
            control.bounds().left() >= viewport.left(),
            "{target} is clipped before its scroll viewport"
        );
        assert!(
            control.bounds().right() <= viewport.right(),
            "{target} is clipped after its scroll viewport"
        );
    })
    .unwrap();
}

fn assert_section_jump_aligned(
    cx: &mut TestAppContext,
    fixture: &SettingsFixture,
    section: Section,
) -> gpui_kit::Pixels {
    cx.update_window(fixture.handle, |_, window, cx| {
        window.click(section.id(), cx);
    })
    .unwrap();
    settle_settings(cx, fixture);
    cx.update_window(fixture.handle, |_, window, cx| {
        let group = window.find((gpui_kit::ElementId::from(section.id()), "anchor"));
        let scroll = &fixture.workbench.read(cx).settings.scroll;
        let offset = scroll.offset().y;
        assert!(
            offset < gpui_kit::Pixels::ZERO,
            "middle section must not be clamped at the start"
        );
        assert!(
            offset > -scroll.max_offset().y,
            "middle section must not be clamped at the end"
        );
        assert_eq!(
            window.pixel_snap(group.bounds().top()),
            window.pixel_snap(scroll.bounds().top()),
            "section heading must align with its own scroll viewport, not the window origin",
        );
        offset
    })
    .unwrap()
}

#[gpui_kit::test]
fn middle_section_jumps_align_after_wheel_fractional_scale_and_error_header_changes(
    cx: &mut TestAppContext,
) {
    let _ui_session = crate::test_support::UiTestSession::new(cx);
    let fixture = fixture(cx, 1100., 360.);
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("nav-settings", cx);
    })
    .unwrap();
    settle_settings(cx, &fixture);
    let appearance = assert_section_jump_aligned(cx, &fixture, Section::Appearance);
    assert_section_jump_aligned(cx, &fixture, Section::Devices);
    assert_eq!(
        assert_section_jump_aligned(cx, &fixture, Section::Appearance),
        appearance
    );
    cx.update_window(fixture.handle, |_, window, cx| {
        window.scroll(
            "settings-appearance",
            gpui_kit::ScrollDelta::Pixels(gpui_kit::point(px(0.), px(-37.))),
            cx,
        );
    })
    .unwrap();
    settle_settings(cx, &fixture);
    assert_section_jump_aligned(cx, &fixture, Section::Devices);

    cx.simulate_window_scale_factor_change(fixture.handle, 1.25);
    settle_settings(cx, &fixture);
    let appearance = assert_section_jump_aligned(cx, &fixture, Section::Appearance);
    assert_section_jump_aligned(cx, &fixture, Section::Devices);
    assert_eq!(
        assert_section_jump_aligned(cx, &fixture, Section::Appearance),
        appearance
    );

    cx.update(|cx| {
        fixture.workbench.update(cx, |this, cx| {
            this.preferences_io.save_error = Some("Isolated save failure".into());
            cx.notify();
        });
    });
    settle_settings(cx, &fixture);
    assert_section_jump_aligned(cx, &fixture, Section::Appearance);
    assert_section_jump_aligned(cx, &fixture, Section::Devices);
}

#[gpui_kit::test]
fn repository_links_open_exact_urls_without_leaving_the_searched_about_section(
    cx: &mut TestAppContext,
) {
    let _ui_session = crate::test_support::UiTestSession::new(cx);
    let fixture = fixture(cx, 1100., 760.);
    let (search_id, preferences) = cx
        .update_window(fixture.handle, |_, window, cx| {
            window.render_frame(cx);
            window.click("nav-settings", cx);
            window.click("settings-search", cx);
            window.input("github", cx);
            assert!(window.find("settings-repositories").visible());
            assert!(window.try_find("settings-language").is_none());
            window.click("settings-section-about", cx);
            let workbench = fixture.workbench.read(cx);
            (
                workbench.settings.search.entity_id(),
                workbench.preferences.clone(),
            )
        })
        .unwrap();
    settle_settings(cx, &fixture);
    assert_eq!(cx.opened_url(), None);

    for (id, url) in [
        (
            "settings-repository-application",
            "https://github.com/jitingcn/SlimeNRF-Helper",
        ),
        (
            "settings-repository-receiver",
            "https://github.com/jitingcn/SlimeVR-Tracker-nRF-Receiver",
        ),
        (
            "settings-repository-tracker",
            "https://github.com/jitingcn/SlimeVR-Tracker-nRF",
        ),
        (
            "settings-repository-bootloader",
            "https://github.com/jitingcn/Adafruit_nRF52_Bootloader",
        ),
    ] {
        let previous_url = cx.opened_url();
        cx.update_window(fixture.handle, |_, window, cx| {
            assert!(window.find(id).visible());
            window.hover(id, cx);
        })
        .unwrap();
        assert_eq!(
            cx.opened_url(),
            previous_url,
            "hovering {id} must not launch a browser"
        );
        cx.update_window(fixture.handle, |_, window, cx| {
            window.click(id, cx);
        })
        .unwrap();
        settle_settings(cx, &fixture);
        assert_eq!(cx.opened_url().as_deref(), Some(url), "{id}");
        cx.update_window(fixture.handle, |_, window, cx| {
            let workbench = fixture.workbench.read(cx);
            assert_eq!(workbench.settings.search.entity_id(), search_id);
            assert_eq!(workbench.preferences, preferences);
            assert!(!workbench.registry_is_open());
            assert!(workbench.settings.selected == Section::About);
            assert_eq!(window.find("settings-search").value(), Some("github"));
            assert_eq!(window.find("settings-section-about").checked(), Some(true));
            assert!(window.find("settings-repositories").visible());
        })
        .unwrap();
    }

    cx.update_window(fixture.handle, |_, window, cx| {
        window.click("nav-home", cx);
        window.click("nav-settings", cx);
        assert_eq!(window.find("settings-search").value(), Some("github"));
        assert_eq!(window.find("settings-section-about").checked(), Some(true));
        assert!(window.find("settings-repositories").visible());
    })
    .unwrap();
}

#[gpui_kit::test]
fn minimum_height_sidebar_reveals_sections_during_keyboard_navigation(cx: &mut TestAppContext) {
    let _ui_session = crate::test_support::UiTestSession::new(cx);
    let fixture = fixture(cx, 760., 360.);
    cx.update(|cx| {
        gpui_kit::component::Theme::update(cx, |theme| theme.font_size = px(20.));
    });
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("nav-settings", cx);
        window.click("settings-search", cx);
    })
    .unwrap();
    for _ in 0..5 {
        settings_key(cx, &fixture, "tab");
    }
    cx.update_window(fixture.handle, |_, window, cx| {
        let about = window.find("settings-section-about");
        let viewport = fixture
            .workbench
            .read(cx)
            .settings
            .navigation_scroll
            .bounds();
        assert_eq!(about.focused(), Some(true));
        assert!(about.bounds().top() >= viewport.top());
        assert!(about.bounds().bottom() <= viewport.bottom());
    })
    .unwrap();
    settings_key(cx, &fixture, "enter");
    cx.update_window(fixture.handle, |_, window, cx| {
        assert!(fixture.workbench.read(cx).settings.selected == Section::About);
        assert_eq!(window.find("settings-section-about").checked(), Some(true));
    })
    .unwrap();
}

#[gpui_kit::test]
fn repository_links_reveal_and_open_with_enter_at_minimum_width_and_zoom(cx: &mut TestAppContext) {
    let _ui_session = crate::test_support::UiTestSession::new(cx);
    let fixture = fixture(cx, 760., 360.);
    let preferences = cx.update(|cx| {
        gpui_kit::component::Theme::update(cx, |theme| theme.font_size = px(20.));
        fixture.workbench.read(cx).preferences.clone()
    });
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("nav-settings", cx);
        window.scroll(
            "settings-search",
            gpui_kit::ScrollDelta::Pixels(gpui_kit::point(px(0.), px(-1000.))),
            cx,
        );
    })
    .unwrap();
    settle_settings(cx, &fixture);
    cx.update_window(fixture.handle, |_, window, cx| {
        assert!(window.find("settings-section-about").visible());
        window.click("settings-section-about", cx);
    })
    .unwrap();
    settle_settings(cx, &fixture);
    settings_key(cx, &fixture, "home");
    cx.update_window(fixture.handle, |_, window, _| {
        assert!(!window.find("settings-repository-application").visible());
    })
    .unwrap();
    tab_to_setting(cx, &fixture, "settings-repository-application", "tab");
    assert_eq!(cx.opened_url(), None);

    for (ix, (id, url)) in [
        (
            "settings-repository-application",
            "https://github.com/jitingcn/SlimeNRF-Helper",
        ),
        (
            "settings-repository-receiver",
            "https://github.com/jitingcn/SlimeVR-Tracker-nRF-Receiver",
        ),
        (
            "settings-repository-tracker",
            "https://github.com/jitingcn/SlimeVR-Tracker-nRF",
        ),
        (
            "settings-repository-bootloader",
            "https://github.com/jitingcn/Adafruit_nRF52_Bootloader",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let previous_url = cx.opened_url();
        if ix > 0 {
            settings_key(cx, &fixture, "tab");
        }
        assert_setting_focus_visible(cx, &fixture, id);
        assert_eq!(
            cx.opened_url(),
            previous_url,
            "focusing {id} must not launch a browser"
        );
        cx.update_window(fixture.handle, |_, window, cx| {
            let group = window.find("settings-repositories").bounds();
            let link = window.find(id).bounds();
            let viewport = fixture.workbench.read(cx).settings.scroll.bounds();
            assert!(group.left() >= viewport.left());
            assert!(group.right() <= viewport.right());
            assert!(link.left() >= group.left());
            assert!(link.right() <= group.right());
        })
        .unwrap();
        settings_key(cx, &fixture, "enter");
        assert_eq!(cx.opened_url().as_deref(), Some(url), "{id}");
        assert_setting_focus_visible(cx, &fixture, id);
        cx.update_window(fixture.handle, |_, window, cx| {
            let workbench = fixture.workbench.read(cx);
            assert_eq!(workbench.preferences, preferences);
            assert!(!workbench.registry_is_open());
            assert!(workbench.settings.selected == Section::About);
            assert_eq!(window.find("settings-search").value(), Some(""));
            assert_eq!(window.find("settings-section-about").checked(), Some(true));
        })
        .unwrap();
    }
}

#[gpui_kit::test]
fn tab_and_reverse_tab_reveal_real_controls_after_section_and_page_jumps(cx: &mut TestAppContext) {
    let _ui_session = crate::test_support::UiTestSession::new(cx);
    let fixture = fixture(cx, 1100., 620.);
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("nav-settings", cx);
    })
    .unwrap();
    tab_to_setting(cx, &fixture, "settings-language-english", "tab");
    for target in [
        "settings-language-simplified-chinese",
        "settings-appearance-light",
        "settings-appearance-dark",
        "settings-accent-neutral",
        "settings-accent-blue",
        "settings-accent-green",
        "settings-accent-purple",
        "settings-accent-amber",
        "settings-accent-sakura",
    ] {
        settings_key(cx, &fixture, "tab");
        assert_setting_focus_visible(cx, &fixture, target);
    }

    cx.update_window(fixture.handle, |_, window, cx| {
        window.click("settings-section-about", cx);
    })
    .unwrap();
    settings_key(cx, &fixture, "end");
    cx.update_window(fixture.handle, |_, window, _| {
        assert!(!window.find("settings-language").visible());
    })
    .unwrap();
    tab_to_setting(cx, &fixture, "settings-language-english", "tab");

    for target in [
        "settings-language-simplified-chinese",
        "settings-appearance-light",
        "settings-appearance-dark",
        "settings-accent-neutral",
        "settings-accent-blue",
        "settings-accent-green",
        "settings-accent-purple",
        "settings-accent-amber",
        "settings-accent-sakura",
        "manage-devices",
        "advanced-features",
        "settings-repository-application",
        "settings-repository-receiver",
        "settings-repository-tracker",
        "settings-repository-bootloader",
        "open-notices",
    ] {
        settings_key(cx, &fixture, "tab");
        assert_setting_focus_visible(cx, &fixture, target);
    }
    settings_key(cx, &fixture, "end");
    for target in [
        "settings-repository-bootloader",
        "settings-repository-tracker",
        "settings-repository-receiver",
        "settings-repository-application",
        "advanced-features",
        "manage-devices",
        "settings-accent-sakura",
        "settings-accent-amber",
        "settings-accent-purple",
        "settings-accent-green",
        "settings-accent-blue",
        "settings-accent-neutral",
        "settings-appearance-dark",
        "settings-appearance-light",
        "settings-language-simplified-chinese",
        "settings-language-english",
    ] {
        settings_key(cx, &fixture, "shift-tab");
        assert_setting_focus_visible(cx, &fixture, target);
    }

    settings_key(cx, &fixture, "tab");
    assert_setting_focus_visible(cx, &fixture, "settings-language-simplified-chinese");

    settings_key(cx, &fixture, "tab");
    assert_setting_focus_visible(cx, &fixture, "settings-appearance-light");
    settings_key(cx, &fixture, "enter");
    cx.update_window(fixture.handle, |_, window, cx| {
        assert!(window.try_find("popup-menu").is_none());
        assert_eq!(
            fixture.workbench.read(cx).preferences.appearance,
            Appearance::Light
        );
        assert_eq!(
            window.find("settings-appearance-light").checked(),
            Some(true)
        );
        assert_eq!(
            window.find("settings-appearance-dark").checked(),
            Some(false)
        );
    })
    .unwrap();
    assert_setting_focus_visible(cx, &fixture, "settings-appearance-light");
    settings_key(cx, &fixture, "tab");
    assert_setting_focus_visible(cx, &fixture, "settings-appearance-dark");
    settings_key(cx, &fixture, "space");
    cx.update_window(fixture.handle, |_, window, cx| {
        assert_eq!(
            fixture.workbench.read(cx).preferences.appearance,
            Appearance::Dark
        );
        assert_eq!(
            window.find("settings-appearance-dark").checked(),
            Some(true)
        );
    })
    .unwrap();
    for accent in AccentColor::ALL {
        let id = SettingsChoice::Accent(accent).id();
        settings_key(cx, &fixture, "tab");
        assert_setting_focus_visible(cx, &fixture, id);
        settings_key(cx, &fixture, "enter");
        cx.update_window(fixture.handle, |_, window, cx| {
            assert_eq!(fixture.workbench.read(cx).preferences.accent, accent);
            assert_eq!(window.find(id).checked(), Some(true));
            assert!(window.try_find("popup-menu").is_none());
        })
        .unwrap();
        assert_setting_focus_visible(cx, &fixture, id);
    }
    settings_key(cx, &fixture, "space");
    cx.update_window(fixture.handle, |_, window, cx| {
        assert_eq!(
            fixture.workbench.read(cx).preferences.accent,
            AccentColor::Sakura,
            "activating the selected choice must not clear the accent"
        );
        assert_eq!(window.find("settings-accent-sakura").checked(), Some(true));
    })
    .unwrap();
}

#[gpui_kit::test]
fn named_accent_choices_wrap_inside_the_control_area_at_minimum_width_and_zoom(
    cx: &mut TestAppContext,
) {
    let _ui_session = crate::test_support::UiTestSession::new(cx);
    let fixture = fixture(cx, 1100., 620.);
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("nav-settings", cx);
        window.click("settings-search", cx);
        window.input("sakura", cx);
    })
    .unwrap();
    cx.simulate_window_scale_factor_change(fixture.handle, 1.25);
    for width in [1100., 760.] {
        cx.simulate_window_resize(fixture.handle, size(px(width), px(620.)));
        for font_size in [16., 17.5, 20.] {
            cx.update(|cx| {
                gpui_kit::component::Theme::update(cx, |theme| theme.font_size = px(font_size));
            });
            settle_settings(cx, &fixture);
            cx.update_window(fixture.handle, |_, window, cx| {
                window.click("settings-section-all", cx);
                window.click("settings-search", cx);
            })
            .unwrap();
            settle_settings(cx, &fixture);
            let context = cx
            .update_window(fixture.handle, |_, window, cx| {
                let group = window.find("settings-accent").bounds();
                let scroll = &fixture.workbench.read(cx).settings.scroll;
                let viewport = scroll.bounds();
                let offset = scroll.offset();
                let maximum = scroll.max_offset();
                let pixel_span = |near, far| {
                    window.pixel_snap(window.pixel_snap(far) - window.pixel_snap(near))
                };
                let census = AccentColor::ALL.map(|accent| {
                    let control = window.find(SettingsChoice::Accent(accent).id());
                    let bounds = control.bounds();
                    (
                        accent,
                        bounds,
                        control.visible(),
                        (
                            pixel_span(bounds.left(), bounds.right()),
                            pixel_span(bounds.top(), bounds.bottom()),
                        ),
                    )
                });
                let context = format!(
                    "width {width}, font size {font_size}, rem {:?}, group {group:?}, viewport {viewport:?}, offset {offset:?}, maximum {maximum:?}, choices {census:?}",
                    window.rem_size()
                );
                let before = census.map(|(_, bounds, _, _)| bounds);
                let (_, first, _, (first_width, first_height)) = census[0];
                assert!(
                    before[5].top() > first.top(),
                    "all six named choices must wrap instead of overflowing: {context}"
                );
                if width == 1100. {
                    for column in 0..3 {
                        assert_eq!(
                            window.pixel_snap(before[column].top()),
                            window.pixel_snap(first.top()),
                            "normal-width choices must form three columns: {context}"
                        );
                        assert_eq!(
                            window.pixel_snap(before[column + 3].top()),
                            window.pixel_snap(before[3].top()),
                            "normal-width choices must form two rows: {context}"
                        );
                        assert_eq!(
                            window.pixel_snap(before[column].left()),
                            window.pixel_snap(before[column + 3].left()),
                            "accent rows must share their column spines: {context}"
                        );
                    }
                }
                for (accent, bounds, _, span) in census {
                    assert_eq!(
                        span.0, first_width,
                        "{accent:?} must have the same button width: {context}"
                    );
                    assert_eq!(
                        span.1, first_height,
                        "{accent:?} must have the same button height: {context}"
                    );
                    for (edge, outer, lower) in [
                        (bounds.left(), group.left(), true),
                        (bounds.right(), group.right(), false),
                        (bounds.top(), group.top(), true),
                        (bounds.bottom(), group.bottom(), false),
                    ] {
                        let edge = window.pixel_snap(edge);
                        let outer = window.pixel_snap(outer);
                        assert!(
                            if lower { edge >= outer } else { edge <= outer },
                            "{accent:?} must fit inside its control area: {context}"
                        );
                    }
                }
                context
            })
            .unwrap();
            for (ix, accent) in AccentColor::ALL.into_iter().enumerate() {
                let id = SettingsChoice::Accent(accent).id();
                if ix == 0 {
                    tab_to_setting(cx, &fixture, id, "tab");
                } else {
                    settings_key(cx, &fixture, "tab");
                    assert_setting_focus_visible(cx, &fixture, id);
                }
                let before = cx
                    .update_window(fixture.handle, |_, window, cx| {
                        assert!(
                            window.find(id).visible(),
                            "{accent:?} must be visible after keyboard reveal: {context}"
                        );
                        let before = AccentColor::ALL
                            .map(|other| window.find(SettingsChoice::Accent(other).id()).bounds());
                        window.click(id, cx);
                        before
                    })
                    .unwrap();
                settle_settings(cx, &fixture);
                cx.update_window(fixture.handle, |_, window, cx| {
                    assert_eq!(
                        fixture.workbench.read(cx).preferences.accent,
                        accent,
                        "the revealed {accent:?} button must accept a real click: {context}"
                    );
                    for (other, expected) in AccentColor::ALL.into_iter().zip(before) {
                        let control = window.find(SettingsChoice::Accent(other).id());
                        assert_eq!(
                            control.checked(),
                            Some(other == accent),
                            "selecting {accent:?} must update {other:?}: {context}"
                        );
                        assert_eq!(
                            control.bounds(),
                            expected,
                            "selecting {accent:?} must not move or resize {other:?}: {context}"
                        );
                    }
                })
                .unwrap();
            }
        }
    }
}

#[gpui_kit::test]
fn real_preference_controls_apply_immediately_and_save_only_to_the_private_store(
    cx: &mut TestAppContext,
) {
    let runtime = crate::app_runtime::AppRuntime::new().unwrap();
    let _ui_session = crate::test_support::UiTestSession::new(cx);
    let fixture = fixture_with_executor(cx, 1100., 760., runtime.executor(), None);
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("nav-settings", cx);
        assert_eq!(
            window.find("settings-language").bounds().right(),
            window.find("settings-appearance").bounds().right(),
        );
        assert_eq!(
            window.find("settings-appearance").bounds().right(),
            window.find("settings-accent").bounds().right(),
        );
    })
    .unwrap();
    for language in [Language::SimplifiedChinese, Language::English] {
        cx.update_window(fixture.handle, |_, window, cx| {
            window.click("settings-search", cx);
            window.input("language", cx);
            let input_id = fixture.workbench.read(cx).settings.search.entity_id();
            // An existing scan suppresses device IO while the real language control runs.
            fixture
                .workbench
                .update(cx, |this, _| this.scanning_inventory = true);
            window.click(SettingsChoice::Language(language).id(), cx);
            fixture.workbench.update(cx, |this, _| {
                assert!(this.inventory_task.is_none());
                this.scanning_inventory = false;
            });
            window.render_frame(cx);
            assert!(!fixture.workbench.read(cx).scanning_inventory);
            assert!(fixture.workbench.read(cx).inventory_task.is_none());
            assert_eq!(fixture.workbench.read(cx).preferences.language, language);
            assert_eq!(
                fixture.workbench.read(cx).settings.search.entity_id(),
                input_id
            );
            assert_eq!(window.find("settings-search").value(), Some("language"));
            let title = rust_i18n::t!("app.settings.language.title", locale = language.code());
            assert_eq!(
                window.find("settings-language").label(),
                Some(title.as_ref())
            );
            let english = window.find("settings-language-english").bounds();
            let chinese = window.find("settings-language-simplified-chinese").bounds();
            assert_eq!(
                window.pixel_snap(english.right()) - window.pixel_snap(english.left()),
                window.pixel_snap(chinese.right()) - window.pixel_snap(chinese.left())
            );
            assert_eq!(
                window.pixel_snap(english.bottom()) - window.pixel_snap(english.top()),
                window.pixel_snap(chinese.bottom()) - window.pixel_snap(chinese.top())
            );
            for other in Language::ALL {
                let other_choice = SettingsChoice::Language(other);
                let control = window.find(other_choice.id());
                assert!(control.visible());
                assert_eq!(control.checked(), Some(other == language));
                let label = rust_i18n::t!(other_choice.label_key(), locale = language.code());
                assert_eq!(control.label(), Some(label.as_ref()));
            }
        })
        .unwrap();
        cx.update_window(fixture.handle, |_, window, cx| {
            window.click("settings-search", cx);
            window.press("secondary-a", cx);
            window.press("backspace", cx);
        })
        .unwrap();
        settle_settings(cx, &fixture);
    }
    cx.update_window(fixture.handle, |_, window, cx| {
        for appearance in [Appearance::Light, Appearance::Dark] {
            window.click(SettingsChoice::Appearance(appearance).id(), cx);
            assert_eq!(
                fixture.workbench.read(cx).preferences.appearance,
                appearance
            );
            assert_eq!(cx.theme().is_dark(), appearance == Appearance::Dark);
            for other in Appearance::ALL {
                let choice = SettingsChoice::Appearance(other);
                let control = window.find(choice.id());
                assert_eq!(
                    control.label(),
                    Some(crate::i18n::tr(choice.label_key()).as_str())
                );
                assert_eq!(control.checked(), Some(other == appearance));
            }
            for accent in AccentColor::ALL {
                let primary = super::theme::accent_primary(appearance, accent);
                window.click(SettingsChoice::Accent(accent).id(), cx);
                assert!(window.try_find("popup-menu").is_none());
                assert_eq!(fixture.workbench.read(cx).preferences.accent, accent);
                assert_eq!(
                    cx.theme().primary,
                    primary,
                    "swatch must preview the applied primary for {appearance:?}/{accent:?}"
                );
                for other in AccentColor::ALL {
                    let choice = SettingsChoice::Accent(other);
                    let control = window.find(choice.id());
                    assert_eq!(
                        control.label(),
                        Some(crate::i18n::tr(choice.label_key()).as_str())
                    );
                    assert_eq!(control.checked(), Some(other == accent));
                }
            }
        }
    })
    .unwrap();
    settle_settings(cx, &fixture);
    cx.update_window(fixture.handle, |_, window, cx| {
        window.click("settings-search", cx);
        window.input("advanced", cx);
        let before = fixture.workbench.read(cx).preferences.advanced_features;
        window.click("advanced-features", cx);
        assert_eq!(
            fixture.workbench.read(cx).preferences.advanced_features,
            !before
        );
        assert_eq!(window.find("advanced-features").checked(), Some(!before));
    })
    .unwrap();
    cx.run_until_parked();
    let stored: crate::features::settings::store::Preferences = serde_json::from_slice(
        &std::fs::read(fixture.directory.path().join("preferences.json")).unwrap(),
    )
    .unwrap();
    cx.update(|cx| assert_eq!(fixture.workbench.read(cx).preferences, stored));
}

#[gpui_kit::test]
fn save_failure_and_retry_remain_visible_when_search_has_no_results(cx: &mut TestAppContext) {
    let _ui_session = crate::test_support::UiTestSession::new(cx);
    let fixture = fixture(cx, 1100., 760.);
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.workbench.update(cx, |this, _| {
            this.preferences_io.load_error = Some("Original load failure".into());
            this.preferences_io.save_error = Some("Previous save failure".into());
        });
        window.render_frame(cx);
        window.click("nav-settings", cx);
        window.click("settings-search", cx);
        window.input("no-settings-match", cx);
        assert!(window.find("settings-clear-search").visible());
        assert!(window.find("retry-save-preferences").visible());
        window.click("retry-save-preferences", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        assert!(
            fixture
                .workbench
                .read(cx)
                .preferences_io
                .load_error
                .is_none()
        );
        assert!(
            fixture
                .workbench
                .read(cx)
                .preferences_io
                .save_error
                .is_none()
        );
    });
    assert!(fixture.directory.path().join("preferences.json").is_file());
}

fn assert_searched_accent_geometry(
    cx: &mut TestAppContext,
    fixture: &SettingsFixture,
    language: Language,
) -> gpui_kit::Pixels {
    cx.update_window(fixture.handle, |_, window, cx| {
        let search = window.find("settings-search");
        let navigation = fixture
            .workbench
            .read(cx)
            .settings
            .navigation_scroll
            .bounds();
        let search_bounds = search.bounds();
        assert_eq!(search.value(), Some("sakura"));
        assert_eq!(search.focused(), Some(true));
        assert!(
            search.visible(),
            "the retained search must remain reachable"
        );
        assert!(search_bounds.left() >= navigation.left());
        assert!(search_bounds.right() <= navigation.right());
        assert!(search_bounds.top() >= navigation.top());
        assert!(search_bounds.bottom() <= navigation.bottom());

        let accent_group = window.find("settings-accent");
        let title = rust_i18n::t!("app.settings.accent.title", locale = language.code());
        assert_eq!(accent_group.label(), Some(title.as_ref()));
        let group = accent_group.bounds();
        let viewport = fixture.workbench.read(cx).settings.scroll.bounds();
        assert!(viewport.size.width > px(0.) && viewport.size.height > px(0.));
        assert!(window.pixel_snap(group.left()) >= window.pixel_snap(viewport.left()));
        assert!(window.pixel_snap(group.right()) <= window.pixel_snap(viewport.right()));
        for accent in AccentColor::ALL {
            let bounds = window.find(SettingsChoice::Accent(accent).id()).bounds();
            assert!(
                window.pixel_snap(bounds.left()) >= window.pixel_snap(group.left())
                    && window.pixel_snap(bounds.right()) <= window.pixel_snap(group.right()),
                "{accent:?} must fit the searched control group: {bounds:?}, group {group:?}"
            );
        }
        viewport.top()
    })
    .unwrap()
}

fn assert_save_diagnostic(cx: &mut TestAppContext, fixture: &SettingsFixture, detail: &str) {
    cx.update_window(fixture.handle, |_, window, _| {
        let alert = window.find("settings-save-error");
        let retry = window.find("retry-save-preferences");
        let bounds = alert.bounds();
        assert_eq!(alert.role(), Some(gpui_kit::Role::Alert));
        assert!(alert.label().is_some_and(|label| label.contains(detail)));
        assert!(alert.visible());
        assert!(bounds.left() >= px(0.));
        assert!(bounds.right() <= window.viewport_size().width);
        assert!(bounds.bottom() <= retry.bounds().top());
    })
    .unwrap();
}

#[gpui_kit::test]
fn searched_settings_survive_error_locale_and_narrow_resize_with_real_preference_input(
    cx: &mut TestAppContext,
) {
    const ENGLISH_DIAGNOSTIC: &str = "Cannot save the isolated preference file because the synthetic directory is unavailable; changes remain in this session.";
    const CHINESE_DIAGNOSTIC: &str = "无法写入隔离的首选项目录；请检查访问权限并重试，当前会话中的更改不会自动保存。合成诊断：目录不可用。";

    // The runtime must also outlive session cleanup if an assertion unwinds.
    let runtime = crate::app_runtime::AppRuntime::new().unwrap();
    let _ui_session = crate::test_support::UiTestSession::new(cx);
    let fixture = fixture_with_executor(cx, 1100., 760., runtime.executor(), None);
    cx.simulate_window_scale_factor_change(fixture.handle, 1.25);
    cx.update_window(fixture.handle, |_, window, cx| {
        let preferences = fixture.workbench.read(cx).preferences.clone();
        super::theme::apply(&preferences, cx);
        gpui_kit::component::Theme::update(cx, |theme| theme.font_size = px(20.));
        window.render_frame(cx);
        window.click("nav-settings", cx);
        window.click("settings-search", cx);
        window.input("sakura", cx);
    })
    .unwrap();
    settle_settings(cx, &fixture);
    let input_id = cx.update(|cx| fixture.workbench.read(cx).settings.search.entity_id());
    assert_searched_accent_geometry(cx, &fixture, Language::English);

    // This size is below the native minimum and deliberately stresses the layout.
    cx.simulate_window_resize(fixture.handle, size(px(760.), px(360.)));
    settle_settings(cx, &fixture);
    let narrow_top = assert_searched_accent_geometry(cx, &fixture, Language::English);
    cx.update(|cx| {
        fixture.workbench.update(cx, |this, cx| {
            this.preferences_io.save_error = Some(ENGLISH_DIAGNOSTIC.into());
            cx.notify();
        });
    });
    settle_settings(cx, &fixture);
    let error_top = assert_searched_accent_geometry(cx, &fixture, Language::English);
    assert_save_diagnostic(cx, &fixture, ENGLISH_DIAGNOSTIC);
    assert!(
        error_top > narrow_top,
        "the error occupies space above the viewport",
    );

    // The query hides language controls, so use their production preference owner directly.
    cx.update_window(fixture.handle, |_, window, cx| {
        fixture.workbench.update(cx, |this, cx| {
            let mut preferences = this.preferences.clone();
            preferences.language = Language::SimplifiedChinese;
            this.scanning_inventory = true;
            this.update_preferences(preferences, window, cx);
            assert!(this.inventory_task.is_none());
            this.scanning_inventory = false;
        });
        gpui_kit::component::Theme::update(cx, |theme| theme.font_size = px(20.));
    })
    .unwrap();
    // Finish the private locale save before installing the next synthetic save diagnostic.
    cx.run_until_parked();
    cx.update(|cx| {
        fixture.workbench.update(cx, |this, cx| {
            this.preferences_io.save_error = Some(CHINESE_DIAGNOSTIC.into());
            cx.notify();
        });
    });
    settle_settings(cx, &fixture);
    let translated_error_top =
        assert_searched_accent_geometry(cx, &fixture, Language::SimplifiedChinese);
    assert_save_diagnostic(cx, &fixture, CHINESE_DIAGNOSTIC);
    cx.update_window(fixture.handle, |_, window, cx| {
        let workbench = fixture.workbench.read(cx);
        assert_eq!(workbench.settings.search.entity_id(), input_id);
        assert!(!workbench.scanning_inventory);
        assert!(workbench.inventory_task.is_none());
        let retry = window.find("retry-save-preferences");
        assert!(retry.visible());
        assert!(
            window.pixel_snap(retry.bounds().bottom())
                <= window.pixel_snap(workbench.settings.navigation_scroll.bounds().top()),
            "retry must remain above the settings navigation after the diagnostic changes",
        );
        window.click("retry-save-preferences", cx);
    })
    .unwrap();
    cx.run_until_parked();
    settle_settings(cx, &fixture);
    cx.update_window(fixture.handle, |_, window, cx| {
        assert!(
            fixture
                .workbench
                .read(cx)
                .preferences_io
                .save_error
                .is_none()
        );
        assert!(window.try_find("retry-save-preferences").is_none());
        assert!(window.try_find("settings-save-error").is_none());
        window.click("settings-search", cx);
    })
    .unwrap();
    let recovered_top = assert_searched_accent_geometry(cx, &fixture, Language::SimplifiedChinese);
    assert!(
        recovered_top < translated_error_top,
        "clearing the error must return space to the scrolling content",
    );

    let choice = SettingsChoice::Accent(AccentColor::Sakura);
    tab_to_setting(cx, &fixture, choice.id(), "tab");
    settings_key(cx, &fixture, "space");
    cx.update_window(fixture.handle, |_, window, cx| {
        assert_eq!(window.find(choice.id()).checked(), Some(true));
        assert_eq!(
            fixture.workbench.read(cx).preferences.accent,
            AccentColor::Sakura
        );
        assert_eq!(
            fixture.workbench.read(cx).settings.search.entity_id(),
            input_id
        );
        assert_eq!(window.find("settings-search").value(), Some("sakura"));
        assert!(!fixture.workbench.read(cx).scanning_inventory);
        assert!(fixture.workbench.read(cx).inventory_task.is_none());
    })
    .unwrap();
    cx.run_until_parked();
    let stored: crate::features::settings::store::Preferences = serde_json::from_slice(
        &std::fs::read(fixture.directory.path().join("preferences.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(stored.accent, AccentColor::Sakura);
}

#[gpui_kit::test]
fn registry_reentry_from_navigation_and_home_keeps_keyboard_scope_and_settings_context(
    cx: &mut TestAppContext,
) {
    let _ui_session = crate::test_support::UiTestSession::new(cx);
    let fixture = fixture(cx, 1100., 760.);
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("nav-settings", cx);
        window.click("settings-search", cx);
        window.input("registry", cx);
        window.click("settings-section-devices", cx);
        window.render_frame(cx);
        window.click("manage-devices", cx);
        assert!(fixture.workbench.read(cx).registry_is_open());
        window.press("tab", cx);
        assert_eq!(window.find("registry-back").focused(), Some(true));

        window.click("nav-home", cx);
        window.click("home-settings", cx);
        window.press("tab", cx);
        assert_eq!(window.find("registry-back").focused(), Some(true));
        window.click("nav-device-setup", cx);
        window.click("nav-settings", cx);
        window.press("tab", cx);
        assert_eq!(window.find("registry-back").focused(), Some(true));

        window.click("registry-back", cx);
        assert!(fixture.workbench.read(cx).page_focus.is_focused(window));
        window.press("tab", cx);
        assert_eq!(window.find("settings-search").focused(), Some(true));
        assert_eq!(window.find("settings-search").value(), Some("registry"));
        assert_eq!(
            window.find("settings-section-devices").checked(),
            Some(true)
        );
        assert!(fixture.workbench.read(cx).settings.selected == Section::Devices);
    })
    .unwrap();
}

#[test]
fn explicit_technical_keywords_find_the_corresponding_existing_controls() {
    for (query, expected) in [
        ("sakura", Setting::Accent),
        ("sha-256", Setting::Advanced),
        ("usb", Setting::Registry),
    ] {
        let found: Vec<_> = Setting::ALL
            .into_iter()
            .filter(|setting| setting.matches(query, false))
            .collect();
        assert!(found.iter().any(|setting| std::mem::discriminant(setting) == std::mem::discriminant(&expected)));
    }
}

#[test]
fn application_update_keywords_find_the_about_setting() {
    assert!(matches!(Setting::AppUpdate.section(), Section::About));
    for query in [
        "update",
        "release",
        "prerelease",
        "version",
        "download",
        "更新",
        "发布",
        "预发布",
        "版本",
        "下载",
    ] {
        assert!(
            Setting::AppUpdate.matches(query, false),
            "application update search must match {query:?}"
        );
    }
    assert!(!Setting::AppUpdate.matches("unrelated-setting-query", false));
}
