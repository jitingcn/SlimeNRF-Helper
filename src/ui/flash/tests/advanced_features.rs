use super::support::local_image;
use crate::ui::FeaturePage;
use crate::ui::flash::{Action, FlashState, Mode};

#[test]
fn disabling_advanced_features_revokes_ble_review_even_from_settings() {
    let mut state = FlashState {
        mode: Mode::Ble,
        review: Some(Action::Ble(vec![], local_image("legacy.zip"))),
        acknowledged: true,
        auto: true,
        auto_requested: true,
        ..FlashState::default()
    };
    assert!(state.effective_mode(FeaturePage::Settings, true) == Mode::Ble);
    assert!(state.prepare_advanced_features(false));
    assert!(state.mode == Mode::Esb);
    assert!(state.effective_mode(FeaturePage::Settings, false) == Mode::Esb);
    assert!(state.effective_mode(FeaturePage::Ota, false) == Mode::Esb);
    assert!(state.review.is_none());
    assert!(!state.acknowledged && !state.auto && !state.auto_requested);
    assert!(!state.busy());
}

#[test]
fn pending_ble_rejects_disable_without_dropping_the_operation() {
    let mut state = FlashState {
        mode: Mode::Ble,
        pending: Some(Action::Ble(vec![], local_image("legacy.zip"))),
        acknowledged: true,
        ..FlashState::default()
    };
    assert!(state.advanced_features_locked());
    assert!(!state.prepare_advanced_features(false));
    assert!(state.mode == Mode::Ble);
    assert!(matches!(state.pending, Some(Action::Ble(..))));
    assert!(state.acknowledged);
}

#[test]
fn ble_worker_and_file_chooser_lock_disable_until_completion() {
    let mut state = FlashState {
        mode: Mode::Ble,
        ble_activity: true,
        chooser: Some(gpui_kit::gpui::Task::ready(())),
        ..FlashState::default()
    };
    assert!(!state.prepare_advanced_features(false));
    assert!(state.chooser.is_some());
    state.chooser = None;
    state.job =
        Some(crate::update::Job::start(|_, _| Ok(crate::ui::flash::Payload::Ble(vec![]))).unwrap());
    // A result not yet consumed by the UI still belongs to the active operation.
    assert!(state.advanced_features_locked());
    assert!(!state.prepare_advanced_features(false));
    assert!(state.job.is_some());
    state.job.take().unwrap().shutdown();
    assert!(!state.advanced_features_locked());
    assert!(state.prepare_advanced_features(false));
    assert!(state.mode == Mode::Esb);
}

#[test]
fn unrelated_flash_activity_does_not_lock_the_advanced_setting() {
    let image = local_image("esb.hex");
    let mut state = FlashState {
        mode: Mode::Esb,
        pending: Some(Action::Reboot(vec![])),
        review: Some(Action::Reboot(vec![])),
        active_group: Some("board".into()),
        assignments: [("board".into(), image)].into(),
        ..FlashState::default()
    };
    assert!(state.busy());
    assert!(!state.advanced_features_locked());
    assert!(state.prepare_advanced_features(true));
    assert!(state.prepare_advanced_features(false));
    assert!(matches!(state.pending, Some(Action::Reboot(..))));
    assert!(matches!(state.review, Some(Action::Reboot(..))));
    assert_eq!(state.active_group.as_deref(), Some("board"));
    assert_eq!(state.assignments["board"].name, "esb.hex");
    state.pending = None;
    state.chooser = Some(gpui_kit::gpui::Task::ready(()));
    assert!(state.prepare_advanced_features(false));
    assert!(state.chooser.is_some());
}

#[test]
fn advanced_toggle_alone_never_selects_ble_or_starts_work() {
    let mut state = FlashState {
        mode: Mode::Esb,
        review: Some(Action::Reboot(vec![])),
        ..FlashState::default()
    };
    assert!(state.prepare_advanced_features(true));
    assert!(state.mode == Mode::Esb);
    assert!(matches!(state.review, Some(Action::Reboot(..))));
    assert!(!state.busy());
    assert!(!state.esb_auto_query.queued);
    assert!(state.ble.is_empty());
    assert!(state.selected_ble.is_empty());
    // Even a stale stored BLE mode cannot expose BLE while the preference is off.
    state.transition_mode(Mode::Ble);
    assert!(state.effective_mode(FeaturePage::Ota, false) == Mode::Esb);
    assert!(state.effective_mode(FeaturePage::Settings, false) == Mode::Esb);
    assert!(state.effective_mode(FeaturePage::Ota, true) == Mode::Ble);
    assert!(!state.busy());
}

#[gpui_kit::test]
fn locked_advanced_switch_rejects_pointer_and_keyboard_without_revoking_ble(
    cx: &mut gpui_kit::TestAppContext,
) {
    let _ui_session = crate::ui::test_support::UiTestSession::new(cx);
    use gpui_kit::{
        AppContext as _, Bounds, WindowBounds, WindowOptions, px, size, test::TestWindowExt as _,
    };

    let directory = tempfile::tempdir().unwrap();
    let preferences = directory.path().join("preferences.json");
    let runtime = crate::app_runtime::AppRuntime::new().unwrap();
    let executor = runtime.executor();

    let (handle, workbench) = cx.update(|cx| {
        gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    Default::default(),
                    size(px(1100.), px(760.)),
                ))),
                ..Default::default()
            },
            cx,
            move |window, cx| {
                cx.new(|cx| {
                    let mut this = crate::ui::test_support::offline_workbench(
                        window,
                        cx,
                        crate::sessions::SessionManager::new(),
                        executor,
                    );
                    this.preferences_io =
                        crate::ui::preferences::PreferenceIo::for_test(preferences);
                    this.preferences.advanced_features = true;
                    this
                })
            },
        )
        .unwrap()
    });
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("nav-settings", cx);
        window.click("settings-search", cx);
        window.input("advanced", cx);
        for _ in 0..12 {
            if window.find("advanced-features").focused() == Some(true) {
                break;
            }
            window.press("tab", cx);
        }
        assert_eq!(window.find("advanced-features").focused(), Some(true));
        workbench.update(cx, |this, cx| {
            this.flash.pending = Some(Action::Ble(vec![], local_image("legacy.zip")));
            cx.notify();
        });
        window.render_frame(cx);
        assert_eq!(window.find("advanced-features").disabled(), Some(true));
        assert_eq!(window.find("advanced-features").checked(), Some(true));
        window.click("advanced-features", cx);
        window.press("space", cx);
        assert!(workbench.read(cx).preferences.advanced_features);
        assert!(workbench.read(cx).flash.advanced_features_locked());
        assert!(matches!(
            workbench.read(cx).flash.pending,
            Some(Action::Ble(..))
        ));
        workbench.update(cx, |this, cx| {
            this.flash.pending = None;
            cx.notify();
        });
        window.render_frame(cx);
        window.click("advanced-features", cx);
        assert!(!workbench.read(cx).preferences.advanced_features);
    })
    .unwrap();
    cx.run_until_parked();
}
