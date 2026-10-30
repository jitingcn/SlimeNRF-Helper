use super::{PreferenceIo, SaveTarget};
use crate::{
    application::Workbench,
    features::settings::store::{AccentColor, Preferences},
};
use futures_util::FutureExt as _;
use gpui_kit::{
    BackgroundExecutor, Entity, PlatformDispatcher, Priority, RunnableVariant, TestAppContext,
    VisualContext as _, VisualTestContext,
};
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    time::Duration,
};

struct QueuedDispatcher(Sender<RunnableVariant>);

impl PlatformDispatcher for QueuedDispatcher {
    fn is_main_thread(&self) -> bool {
        false
    }

    fn dispatch(&self, runnable: RunnableVariant, _: Priority) {
        self.0.send(runnable).unwrap();
    }

    fn dispatch_on_main_thread(&self, _: RunnableVariant, _: Priority) {
        panic!("preference IO must not dispatch foreground work");
    }

    fn dispatch_after(&self, _: Duration, _: RunnableVariant) {
        panic!("preference IO does not require a timer");
    }

    fn spawn_realtime(&self, _: Box<dyn FnOnce() + Send>) {
        panic!("preference IO does not require a realtime thread");
    }
}

struct IoGate {
    executor: BackgroundExecutor,
    queued: Receiver<RunnableVariant>,
}

impl IoGate {
    fn new() -> Self {
        let (send, queued) = mpsc::channel();
        Self {
            executor: BackgroundExecutor::new(Arc::new(QueuedDispatcher(send))),
            queued,
        }
    }

    fn run_next(&self) {
        self.queued
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .run();
    }
}

fn preferences(accent: AccentColor) -> Preferences {
    Preferences {
        accent,
        ..Preferences::default()
    }
}

fn stored(path: &Path) -> Preferences {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn preference_fixture<'a>(
    cx: &'a mut TestAppContext,
    path: &Path,
) -> (
    Entity<Workbench>,
    &'a mut VisualTestContext,
    crate::app_runtime::AppRuntime,
) {
    let runtime = crate::app_runtime::AppRuntime::new().unwrap();
    let executor = runtime.executor();
    let target = SaveTarget::Path(path.to_path_buf());

    let cx = cx.add_empty_window();
    let workbench = cx.new_window_entity(move |window, cx| {
        let mut this = crate::application::test_support::offline_workbench(
            window,
            cx,
            crate::sessions::SessionManager::new(),
            executor,
        );
        this.preferences_io = PreferenceIo {
            target,
            ..PreferenceIo::new(None)
        };
        this
    });
    (workbench, cx, runtime)
}

#[gpui_kit::test]
fn preference_callback_returns_before_disk_and_coalesces_a_b_c(cx: &mut TestAppContext) {
    let _ui_session = crate::test_support::UiTestSession::new(cx);
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("preferences.json");
    let gate = IoGate::new();
    let (workbench, cx, _runtime) = preference_fixture(cx, &path);
    for accent in [AccentColor::Blue, AccentColor::Green, AccentColor::Purple] {
        workbench.update(cx, |this, cx| {
            this.preferences = preferences(accent);
            this.save_preferences_with_executor(&gate.executor, cx);
            assert_eq!(this.preferences.accent, accent);
        });
    }
    assert!(
        !path.exists(),
        "callbacks must not execute the queued disk operation"
    );
    cx.run_until_parked();
    assert!(!path.exists());

    gate.run_next();
    assert_eq!(stored(&path), preferences(AccentColor::Blue));
    assert!(matches!(
        gate.queued.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    cx.run_until_parked();
    assert_eq!(stored(&path), preferences(AccentColor::Blue));
    workbench.update(cx, |this, _| {
        assert!(this.preferences_io.active.is_some());
        assert!(this.preferences_io.pending.is_none());
    });
    gate.run_next();
    assert_eq!(stored(&path), preferences(AccentColor::Purple));
    cx.run_until_parked();
    workbench.update(cx, |this, _| {
        assert!(this.preferences_io.active.is_none());
        assert!(this.preferences_io.save_error.is_none());
    });
    assert!(matches!(
        gate.queued.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
}

#[gpui_kit::test]
fn stale_success_and_failure_cannot_change_errors_before_latest_save(cx: &mut TestAppContext) {
    let _ui_session = crate::test_support::UiTestSession::new(cx);
    let directory = tempfile::tempdir().unwrap();
    for fail_first in [false, true] {
        let path = directory
            .path()
            .join(format!("preferences-{fail_first}.json"));
        if fail_first {
            std::fs::write(&path, b"damaged existing settings").unwrap();
        }
        let gate = IoGate::new();
        let (workbench, cx, _runtime) = preference_fixture(cx, &path);
        workbench.update(cx, |this, cx| {
            this.preferences_io.load_error = Some("Original load error".into());
            this.preferences_io.save_error = Some("Previous save error".into());
            this.preferences = preferences(AccentColor::Blue);
            this.save_preferences_with_executor(&gate.executor, cx);
            this.preferences = preferences(AccentColor::Green);
            this.save_preferences_with_executor(&gate.executor, cx);
        });
        cx.run_until_parked();
        gate.run_next();
        cx.run_until_parked();
        workbench.update(cx, |this, _| {
            assert_eq!(
                this.preferences_io.load_error.as_deref(),
                Some("Original load error")
            );
            assert_eq!(
                this.preferences_io.save_error.as_deref(),
                Some("Previous save error")
            );
        });
        if fail_first {
            assert_eq!(std::fs::read(&path).unwrap(), b"damaged existing settings");
            std::fs::write(&path, br#"{"language":"en","future":{"keep":true}}"#).unwrap();
        }
        gate.run_next();
        cx.run_until_parked();
        workbench.update(cx, |this, _| {
            assert!(this.preferences_io.load_error.is_none());
            assert!(this.preferences_io.save_error.is_none());
        });
        assert_eq!(stored(&path), preferences(AccentColor::Green));
    }
}

#[gpui_kit::test]
fn failed_save_and_retry_preserve_previous_data_and_load_error(cx: &mut TestAppContext) {
    let _ui_session = crate::test_support::UiTestSession::new(cx);
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("preferences.json");
    // Compact input is valid and within the store limit, but pretty-printing its
    // unknown field exceeds the output limit before atomic replacement.
    let prefix = r#"{"language":"en","future":""}"#;
    let original = format!(
        r#"{{"language":"en","future":"{}"}}"#,
        "x".repeat(1024 * 1024 - prefix.len())
    );
    std::fs::write(&path, &original).unwrap();
    let gate = IoGate::new();
    let (workbench, cx, _runtime) = preference_fixture(cx, &path);
    workbench.update(cx, |this, cx| {
        this.preferences_io.load_error = Some("Original load error".into());
        this.preferences = preferences(AccentColor::Purple);
        this.save_preferences_with_executor(&gate.executor, cx);
    });
    cx.run_until_parked();
    gate.run_next();
    cx.run_until_parked();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    workbench.update(cx, |this, _| {
        assert!(this.preferences_io.save_error.is_some());
        assert_eq!(
            this.preferences_io.load_error.as_deref(),
            Some("Original load error")
        );
    });

    std::fs::write(&path, br#"{"language":"en","future":{"keep":true}}"#).unwrap();
    workbench.update(cx, |this, cx| {
        // Retry Save uses the current in-memory snapshot, without another edit.
        this.save_preferences_with_executor(&gate.executor, cx);
        assert!(this.preferences_io.save_error.is_some());
        assert!(this.preferences_io.load_error.is_some());
    });
    cx.run_until_parked();
    gate.run_next();
    cx.run_until_parked();
    assert_eq!(stored(&path), preferences(AccentColor::Purple));
    let document: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(document["future"]["keep"], true);
    workbench.update(cx, |this, _| {
        assert!(this.preferences_io.save_error.is_none());
        assert!(this.preferences_io.load_error.is_none());
    });
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
}

#[test]
fn shutdown_joins_active_then_flushes_only_latest_without_ui_completion() {
    let directory = tempfile::tempdir().unwrap();
    for finish_before_shutdown in [false, true] {
        let path = directory
            .path()
            .join(format!("preferences-{finish_before_shutdown}.json"));
        let gate = IoGate::new();
        let mut io = PreferenceIo {
            target: SaveTarget::Path(path.clone()),
            ..PreferenceIo::new(None)
        };
        let active = io
            .save(preferences(AccentColor::Blue), &gate.executor)
            .unwrap();
        assert!(
            io.save(preferences(AccentColor::Green), &gate.executor)
                .is_none()
        );
        assert!(
            io.save(preferences(AccentColor::Purple), &gate.executor)
                .is_none()
        );
        let mut disk = Some(gate.queued.recv_timeout(Duration::from_secs(5)).unwrap());
        let published = Arc::new(AtomicBool::new(false));
        let completion_published = published.clone();
        io.completion = Some(gate.executor.spawn(async move {
            active.await;
            completion_published.store(true, Ordering::Relaxed);
        }));
        gate.run_next();
        assert!(!published.load(Ordering::Relaxed));
        if finish_before_shutdown {
            disk.take().unwrap().run();
            assert_eq!(stored(&path), preferences(AccentColor::Blue));
        } else {
            assert!(!path.exists());
        }
        let cleanup = io.shutdown(&gate.executor).unwrap();
        assert_eq!(cleanup.clone().now_or_never(), None);
        assert!(
            io.save(preferences(AccentColor::Amber), &gate.executor)
                .is_none()
        );
        assert!(io.shutdown(&gate.executor).is_none());
        if let Some(disk) = disk {
            disk.run();
        }
        for _ in 0..5 {
            if cleanup.clone().now_or_never().is_some() {
                break;
            }
            gate.run_next();
        }
        assert_eq!(cleanup.now_or_never(), Some(()));
        assert!(!published.load(Ordering::Relaxed));
        assert_eq!(stored(&path), preferences(AccentColor::Purple));
    }
}
