use super::{FeaturePage, Workbench};
use gpui_kit::AppContext as _;
use std::sync::Arc;

static UI_TEST_LOCALE: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

struct UiTestSessionActive;

impl gpui_kit::Global for UiTestSessionActive {}

pub(super) struct UiTestSession {
    context: gpui_kit::TestAppContext,
    previous_locale: String,
    _locale: parking_lot::MutexGuard<'static, ()>,
}

impl UiTestSession {
    pub(super) fn new(cx: &mut gpui_kit::TestAppContext) -> Self {
        assert!(
            !cx.update(|cx| cx.has_global::<UiTestSessionActive>()),
            "acquire one UI test session at the outer test scope",
        );
        let locale = UI_TEST_LOCALE.lock();
        let previous_locale = rust_i18n::locale().to_string();
        // Lazy backend initialization can write the process locale before its first lookup.
        let _ = crate::_rust_i18n_backend();
        let _ = gpui_kit::component::_rust_i18n_backend();
        rust_i18n::set_locale("en");
        let session = Self {
            context: cx.clone(),
            previous_locale,
            _locale: locale,
        };
        cx.update(|cx| {
            cx.set_global(UiTestSessionActive);
            gpui_kit::init(cx);
        });
        session
    }
}

impl Drop for UiTestSession {
    fn drop(&mut self) {
        // The Kit test macro only calls App::quit, which does not free retained visual contexts.
        self.context.quit();
        self.context.run_until_parked();
        self.context.update(|cx| {
            cx.remove_global::<UiTestSessionActive>();
        });
        rust_i18n::set_locale(&self.previous_locale);
    }
}

pub(super) fn offline_workbench(
    window: &mut gpui_kit::Window,
    cx: &mut gpui_kit::Context<Workbench>,
    manager: crate::sessions::SessionManager,
    async_io: crate::app_runtime::Executor,
) -> Workbench {
    assert!(
        cx.has_global::<UiTestSessionActive>(),
        "offline Workbench fixtures require an outer UiTestSession",
    );
    Workbench {
        async_io,
        shutdown_host: Arc::default(),
        inventory: Default::default(),
        manager,
        selected: Default::default(),
        monitor: crate::discovery::ReceiverMonitor::new(),
        serial_notifications: crate::discovery::SerialNotifications::new(),
        scanning_inventory: false,
        inventory_generation: 0,
        next_enumeration: std::time::Instant::now(),
        layout_state: cx.new(|_| Default::default()),
        sidebar_layout_state: cx.new(|_| Default::default()),
        resize_handle_appearance: crate::ui::workbench_resize_handle(),
        navigation_collapsed: false,
        active_page: FeaturePage::DeviceSetup,
        home: Default::default(),
        home_page_focus: cx.focus_handle(),
        page_focus: cx.focus_handle().tab_stop(false),
        tools_page_focus: cx.focus_handle(),
        calibration_page_focus: cx.focus_handle(),
        preferences: Default::default(),
        settings: super::settings_page::SettingsUiState::new(window, cx),
        registry: Default::default(),
        preferences_io: Default::default(),
        sidebar: Default::default(),
        command_focus: None,
        command_error: None,
        inventory_error: None,
        error: None,
        terminals: Default::default(),
        tabs: Default::default(),
        pending_writes: Vec::new(),
        batch_input: cx.new(|cx| gpui_kit::component::input::InputState::new(window, cx)),
        batch_history: Default::default(),
        custom_commands: Default::default(),
        flash: Default::default(),
        capture: Default::default(),
        calibration: Default::default(),
        replay: Default::default(),
        hid_log: Default::default(),
        temperature: Default::default(),
        rssi: Default::default(),
        notices: Default::default(),
        shutting_down: false,
        shutdown_cleanup: None,
        _shutdown_task: None,
        _poll_task: gpui_kit::Task::ready(()),
        _serial_poll_task: gpui_kit::Task::ready(()),
        inventory_task: None,
    }
}

mod tests {
    use super::{UI_TEST_LOCALE, UiTestSession, Workbench, offline_workbench};
    use crate::settings::{Language, Preferences};
    use gpui_kit::{
        AnyWindowHandle, AppContext as _, MouseButton, TestAppContext, VisualContext as _,
        WeakEntity,
        test::{TestEventExt as _, TestWindowExt as _},
    };
    use std::{
        panic::{AssertUnwindSafe, catch_unwind, panic_any, resume_unwind},
        rc::Rc,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
            mpsc::{self, Sender},
        },
        thread,
        time::Duration,
    };

    #[derive(Debug, PartialEq, Eq)]
    enum LocaleAttempt {
        Waiting,
        Chinese,
    }

    struct SessionUnwind;

    struct StandaloneDispatcher(gpui_kit::TestDispatcher);

    impl Drop for StandaloneDispatcher {
        fn drop(&mut self) {
            // App globals can wake foreground tasks while they drop; retain the scheduler until then.
            self.0.drain_tasks();
        }
    }

    fn retained_registry(
        cx: &mut TestAppContext,
        executor: crate::app_runtime::Executor,
    ) -> (AnyWindowHandle, WeakEntity<Workbench>) {
        let visual = cx.add_empty_window();
        let workbench = visual.new_window_entity(|window, cx| {
            let mut this =
                offline_workbench(window, cx, crate::sessions::SessionManager::new(), executor);
            this.open_device_registry(window, cx);
            this
        });
        visual.replace_root_view(|window, cx| {
            gpui_kit::base::Root::new(workbench.clone(), window, cx)
        });
        visual.update(|window, _| window.activate_window());
        visual.run_until_parked();
        visual.update(|window, cx| {
            assert!(window.is_window_active());
            window.render_frame(cx);
        });
        (visual.window_handle(), workbench.downgrade())
    }

    fn competing_chinese_session(attempts: Sender<LocaleAttempt>, cleaned_up: Arc<AtomicBool>) {
        let runtime = crate::app_runtime::AppRuntime::new().unwrap();
        let dispatcher = StandaloneDispatcher(gpui_kit::TestDispatcher::new(0));
        let mut cx = TestAppContext::build(dispatcher.0.clone(), None);
        if UI_TEST_LOCALE.try_lock().is_none() {
            attempts.send(LocaleAttempt::Waiting).unwrap();
        }
        let _ui_session = UiTestSession::new(&mut cx);
        assert!(
            cleaned_up.load(Ordering::Acquire),
            "the English context must finish real on_quit cleanup before another locale enters",
        );
        cx.update(|cx| {
            crate::ui::theme::apply(
                &Preferences {
                    language: Language::SimplifiedChinese,
                    ..Preferences::default()
                },
                cx,
            );
        });
        let (handle, _) = retained_registry(&mut cx, runtime.executor());
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find("registry-back").label(), Some("返回"));
        })
        .unwrap();
        attempts.send(LocaleAttempt::Chinese).unwrap();
    }

    fn exercise_locale_session(unwind: bool) {
        let runtime = crate::app_runtime::AppRuntime::new().unwrap();
        let dispatcher = StandaloneDispatcher(gpui_kit::TestDispatcher::new(0));
        let mut cx = TestAppContext::build(dispatcher.0.clone(), None);
        let context_owners = Rc::strong_count(&cx.app);
        let cleaned_up = Arc::new(AtomicBool::new(false));
        let (attempts, observations) = mpsc::channel();
        let mut competitor = None;
        let mut retained = Vec::new();
        let mut first_attempt = None;
        let result = catch_unwind(AssertUnwindSafe(|| {
            let _ui_session = UiTestSession::new(&mut cx);
            let (first, first_workbench) = retained_registry(&mut cx, runtime.executor());
            retained.push(first_workbench);
            cx.update_window(first, |_, window, cx| {
                window.render_frame(cx);
                assert_eq!(window.find("registry-back").label(), Some("Back"));
            })
            .unwrap();

            cx.update(|cx| {
                crate::ui::theme::apply(
                    &Preferences {
                        language: Language::SimplifiedChinese,
                        ..Preferences::default()
                    },
                    cx,
                );
            });
            let (second, second_workbench) = retained_registry(&mut cx, runtime.executor());
            retained.push(second_workbench);
            for handle in [first, second] {
                cx.update_window(handle, |_, window, cx| {
                    window.render_frame(cx);
                    assert_eq!(
                        window.find("registry-back").label(),
                        Some("返回"),
                        "a second fixture must preserve its context's selected language",
                    );
                })
                .unwrap();
            }
            cx.update(|cx| crate::ui::theme::apply(&Preferences::default(), cx));
            let on_quit = cleaned_up.clone();
            cx.on_quit(move || on_quit.store(true, Ordering::Release));
            let position = cx
                .update_window(second, |_, window, cx| {
                    window.render_frame(cx);
                    window.press("tab", cx);
                    assert_eq!(window.find("registry-back").focused(), Some(true));
                    let position = window.find("registry-back").bounds().center();
                    window.pointer_move(position, None, cx);
                    window.pointer_down(position, MouseButton::Left, cx);
                    position
                })
                .unwrap();

            let on_quit = cleaned_up.clone();
            competitor = Some(thread::spawn(move || {
                competing_chinese_session(attempts, on_quit);
            }));
            first_attempt = Some(
                observations
                    .recv_timeout(Duration::from_secs(10))
                    .expect("competing locale session must report its acquisition attempt"),
            );
            cx.update_window(second, |_, window, cx| {
                window.render_frame(cx);
                assert_eq!(
                    window.find("registry-back").label(),
                    Some("Back"),
                    "a competing locale session must not relabel a live English pointer gesture",
                );
                let back = window.find("registry-back");
                assert_eq!(back.focused(), Some(true));
                assert!(back.visible());
                assert_eq!(back.bounds().center(), position);
                window.pointer_up(position, MouseButton::Left, cx);
                assert!(window.try_find("registry-back").is_none());
                assert!(window.find("settings-search").visible());
            })
            .unwrap();
            if unwind {
                panic_any(SessionUnwind);
            }
        }));

        assert!(
            cx.windows().is_empty(),
            "session teardown must close its real windows"
        );
        assert!(
            retained
                .iter()
                .all(|workbench| workbench.upgrade().is_none()),
            "window-root Workbenches must be released during session teardown",
        );
        assert_eq!(
            Rc::strong_count(&cx.app),
            context_owners,
            "add_empty_window must not retain cloned contexts after session teardown",
        );
        if first_attempt == Some(LocaleAttempt::Waiting) {
            assert_eq!(
                observations
                    .recv_timeout(Duration::from_secs(10))
                    .expect("Chinese session must enter after English context teardown"),
                LocaleAttempt::Chinese,
            );
        }
        if let Some(competitor) = competitor {
            competitor
                .join()
                .expect("competing locale session must finish");
        }
        match result {
            Ok(()) => assert!(!unwind, "the unwind case must exercise its destructor"),
            Err(error) if unwind && error.is::<SessionUnwind>() => {}
            Err(error) => resume_unwind(error),
        }
        assert_eq!(
            first_attempt,
            Some(LocaleAttempt::Waiting),
            "the competing locale must wait for the live English session",
        );
    }

    #[test]
    fn locale_session_preserves_english_clicks_until_retained_contexts_quit() {
        exercise_locale_session(false);
    }

    #[test]
    fn locale_session_releases_retained_contexts_during_unwind() {
        exercise_locale_session(true);
    }
}
