use std::{
    sync::{Arc, mpsc},
    time::Duration,
};

pub(crate) static UI_TEST_LOCALE: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

struct UiTestSessionActive;

impl gpui_kit::Global for UiTestSessionActive {}

pub(crate) struct UiTestSession {
    context: gpui_kit::TestAppContext,
    previous_locale: String,
    _locale: parking_lot::MutexGuard<'static, ()>,
}

impl UiTestSession {
    pub(crate) fn new(cx: &mut gpui_kit::TestAppContext) -> Self {
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

pub(crate) fn assert_ui_session(cx: &gpui_kit::App) {
    assert!(
        cx.has_global::<UiTestSessionActive>(),
        "offline Workbench fixtures require an outer UiTestSession",
    );
}

pub(crate) fn background_executor() -> gpui_kit::BackgroundExecutor {
    background_executor_with_gate(None)
}

pub(crate) fn background_executor_with_gate(
    gate: Option<mpsc::Receiver<()>>,
) -> gpui_kit::BackgroundExecutor {
    struct Dispatcher(parking_lot::Mutex<Option<mpsc::Receiver<()>>>);
    impl gpui_kit::PlatformDispatcher for Dispatcher {
        fn is_main_thread(&self) -> bool {
            false
        }

        fn dispatch(&self, runnable: gpui_kit::RunnableVariant, _: gpui_kit::Priority) {
            let gate = self.0.lock().take();
            std::thread::spawn(move || {
                if let Some(gate) = gate {
                    gate.recv().unwrap();
                }
                runnable.run();
            });
        }

        fn dispatch_on_main_thread(&self, _: gpui_kit::RunnableVariant, _: gpui_kit::Priority) {
            panic!("Background fixture must not schedule UI work");
        }

        fn dispatch_after(&self, duration: Duration, runnable: gpui_kit::RunnableVariant) {
            std::thread::spawn(move || {
                std::thread::sleep(duration);
                runnable.run();
            });
        }

        fn spawn_realtime(&self, work: Box<dyn FnOnce() + Send>) {
            std::thread::spawn(work);
        }
    }
    gpui_kit::BackgroundExecutor::new(Arc::new(Dispatcher(parking_lot::Mutex::new(gate))))
}
