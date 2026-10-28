use super::{
    AppUpdateState, CHECK_INTERVAL_SECONDS, SelectedRelease, Status, TestCheck, TestDownload,
};
use crate::{
    app_runtime::AppRuntime,
    app_update::{self, CheckResult},
    settings::{AccentColor, Preferences},
    ui::{
        Workbench,
        preferences::PreferenceIo,
        test_support::{UiTestSession, offline_workbench},
    },
};
use gpui_kit::{
    AnyWindowHandle, AppContext as _, BackgroundExecutor, Bounds, Entity, PlatformDispatcher,
    Priority, RunnableVariant, TestAppContext, WindowBounds, WindowOptions, px, size,
    test::{TestQueryExt as _, TestWindowExt as _},
};
use std::{
    path::Path,
    sync::{
        Arc,
        mpsc::{self, Receiver, Sender},
    },
    time::{Duration, Instant},
};

const FIRST_CHECK: u64 = CHECK_INTERVAL_SECONDS * 10;

struct QueuedDispatcher(Sender<RunnableVariant>);

impl PlatformDispatcher for QueuedDispatcher {
    fn is_main_thread(&self) -> bool {
        false
    }

    fn dispatch(&self, runnable: RunnableVariant, _: Priority) {
        self.0.send(runnable).expect("queue preference save");
    }

    fn dispatch_on_main_thread(&self, _: RunnableVariant, _: Priority) {
        panic!("preference save executor must not dispatch UI work");
    }

    fn dispatch_after(&self, _: Duration, _: RunnableVariant) {
        panic!("preference saves do not use a timer");
    }

    fn spawn_realtime(&self, _: Box<dyn FnOnce() + Send>) {
        panic!("preference saves do not use a realtime thread");
    }
}

struct SaveGate {
    executor: BackgroundExecutor,
    queued: Receiver<RunnableVariant>,
}

impl SaveGate {
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
            .expect("expected a retained preference save")
            .run();
    }

    fn assert_idle(&self) {
        assert!(matches!(
            self.queued.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
    }
}

struct Fixture {
    window: AnyWindowHandle,
    workbench: Entity<Workbench>,
}

fn fixture(
    cx: &mut TestAppContext,
    runtime: &AppRuntime,
    path: &Path,
    gate: &SaveGate,
    preferences: Preferences,
) -> Fixture {
    let executor = runtime.executor();
    let io = PreferenceIo::for_test_with_executor(path.to_path_buf(), gate.executor.clone());
    cx.update(|cx| cx.set_reduce_motion(true));
    let (window, workbench) = cx.update(|cx| {
        gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    Default::default(),
                    size(px(1100.), px(900.)),
                ))),
                ..Default::default()
            },
            cx,
            move |window, cx| {
                cx.new(|cx| {
                    let mut this = offline_workbench(
                        window,
                        cx,
                        crate::sessions::SessionManager::new(),
                        executor,
                    );
                    this.preferences = preferences;
                    this.preferences_io = io;
                    // This explicitly injected offline backend exercises the same
                    // admission path as an eligible official Windows installation.
                    this.app_update.installable = true;
                    this.app_update.automatic = true;
                    this
                })
            },
        )
        .expect("open isolated updater window")
    });
    cx.update_window(window, |_, window, _| window.activate_window())
        .expect("activate updater window");
    let fixture = Fixture { window, workbench };
    settle(cx, &fixture);
    cx.update_window(window, |_, window, cx| {
        window.click("nav-settings", cx);
        window.click("settings-section-about", cx);
    })
    .expect("open About settings");
    settle(cx, &fixture);
    cx.update_window(window, |_, window, cx| {
        window.click("settings-search", cx);
        window.press("secondary-a", cx);
        window.input("prerelease", cx);
    })
    .expect("show updater controls");
    settle(cx, &fixture);
    fixture
}

fn settle(cx: &mut TestAppContext, fixture: &Fixture) {
    for _ in 0..2 {
        cx.run_until_parked();
        cx.update_window(fixture.window, |_, window, cx| {
            window.simulate_next_frame(cx);
            window.render_frame(cx);
        })
        .expect("settle updater frame");
    }
}

fn poll_at(cx: &mut TestAppContext, fixture: &Fixture, now: u64) -> bool {
    cx.update(|cx| {
        fixture.workbench.update(cx, |this, cx| {
            let changed = this.poll_app_update_at(Some(now), cx);
            if changed {
                cx.notify();
            }
            changed
        })
    })
}

fn wait_for_job(cx: &mut TestAppContext, fixture: &Fixture, now: u64) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        std::thread::sleep(Duration::from_millis(1));
        poll_at(cx, fixture, now);
        cx.run_until_parked();
        if cx.update(|cx| fixture.workbench.read(cx).app_update.job.is_none()) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "updater job must finish and join"
        );
    }
    settle(cx, fixture);
}

fn click(cx: &mut TestAppContext, fixture: &Fixture, id: &'static str) {
    cx.update_window(fixture.window, |_, window, cx| window.click(id, cx))
        .expect("activate updater control");
}

fn saved(path: &Path) -> (Preferences, serde_json::Value) {
    let bytes = std::fs::read(path).expect("read private preference file");
    (
        serde_json::from_slice(&bytes).expect("decode private preferences"),
        serde_json::from_slice(&bytes).expect("decode private preference document"),
    )
}

fn assert_quiescent(cx: &mut TestAppContext, fixture: &Fixture, gate: &SaveGate) {
    cx.update(|cx| {
        let this = fixture.workbench.read(cx);
        assert!(
            !this.app_update.busy(),
            "updater admission and worker must finish"
        );
        assert!(!this.preferences_io.busy(), "accepted saves must finish");
    });
    gate.assert_idle();
}

#[test]
fn automatic_checks_require_an_official_installable_windows_build() {
    // Test executables are not official packaged builds, even on Windows.
    let development = AppUpdateState::new();
    #[cfg(windows)]
    let official_windows = app_update::windows::install_eligible();
    #[cfg(not(windows))]
    let official_windows = false;
    assert_eq!(development.installable, official_windows);
    assert_eq!(development.automatic, official_windows);
    assert!(!development.automatic);
}

#[gpui_kit::test]
fn automatic_check_waits_for_latest_snapshot_and_observes_24_hour_spacing(cx: &mut TestAppContext) {
    let runtime = AppRuntime::new().expect("start private app runtime");
    let directory = tempfile::tempdir().expect("private preferences directory");
    let path = directory.path().join("preferences.json");
    std::fs::write(&path, br#"{"language":"en","future":{"keep":true}}"#)
        .expect("write readable preferences");
    let initial = saved(&path).0;
    let gate = SaveGate::new();
    let _session = UiTestSession::new(cx);
    let fixture = fixture(cx, &runtime, &path, &gate, initial);

    cx.update(|cx| {
        fixture.workbench.update(cx, |this, cx| {
            this.app_update.test_check = Some(TestCheck::ByChannel {
                stable: CheckResult::UpToDate,
                preview: CheckResult::NoEligibleRelease,
            });
            // A pre-existing accepted save must not admit the automatic check.
            this.preferences.accent = AccentColor::Blue;
            this.save_preferences(cx);
        });
    });
    assert!(poll_at(cx, &fixture, FIRST_CHECK));
    assert!(!poll_at(cx, &fixture, FIRST_CHECK));
    settle(cx, &fixture);
    cx.update_window(fixture.window, |_, window, cx| {
        let this = fixture.workbench.read(cx);
        assert_eq!(this.app_update.pending_auto_attempt, Some(FIRST_CHECK));
        assert!(this.app_update.job.is_none());
        assert!(matches!(this.app_update.status, Status::SavingAttempt));
        assert!(!this.app_update_navigation_locked());
        assert_eq!(window.find("app-update-check").disabled(), Some(true));
        assert!(window.find("app-update-cancel").visible());
        assert_eq!(
            window.find("app-update-status").label(),
            Some(crate::i18n::tr("app.settings.update.status_saving_attempt").as_str())
        );
        window.click("app-update-prereleases", cx);
    })
    .expect("change channel while admission is pending");
    cx.update(|cx| {
        let this = fixture.workbench.read(cx);
        assert!(this.preferences.app_update_include_prereleases);
        assert!(matches!(this.app_update.status, Status::SavingAttempt));
        assert!(this.app_update.job.is_none());
    });

    gate.run_next();
    let (first, _) = saved(&path);
    assert_eq!(first.accent, AccentColor::Blue);
    assert_eq!(first.app_update_last_attempt, None);
    cx.run_until_parked();
    assert!(!poll_at(cx, &fixture, FIRST_CHECK));
    cx.update(|cx| assert!(fixture.workbench.read(cx).app_update.job.is_none()));

    gate.run_next();
    let (latest, document) = saved(&path);
    assert_eq!(latest.app_update_last_attempt, Some(FIRST_CHECK));
    assert!(latest.app_update_include_prereleases);
    assert_eq!(document["future"]["keep"], true);
    cx.run_until_parked();
    cx.update(|cx| {
        let this = fixture.workbench.read(cx);
        assert_eq!(latest, this.preferences);
        assert!(
            this.app_update.job.is_none(),
            "save completion does not itself start a check"
        );
    });
    assert!(poll_at(cx, &fixture, FIRST_CHECK));
    cx.update(|cx| {
        let this = fixture.workbench.read(cx);
        assert!(this.app_update.job.is_some());
        assert!(matches!(this.app_update.status, Status::Checking));
        assert!(!this.app_update_navigation_locked());
    });
    wait_for_job(cx, &fixture, FIRST_CHECK);
    cx.update(|cx| {
        let this = fixture.workbench.read(cx);
        assert!(matches!(this.app_update.status, Status::NoEligible));
        assert!(this.preferences.app_update_include_prereleases);
    });

    assert_quiescent(cx, &fixture, &gate);
    // A new Workbench receives the preferences read from the previous launch.
    let restart = self::fixture(cx, &runtime, &path, &gate, saved(&path).0);
    cx.update(|cx| {
        restart.workbench.update(cx, |this, _| {
            this.app_update.test_check = Some(TestCheck::Result(CheckResult::UpToDate));
        });
    });
    assert!(!poll_at(
        cx,
        &restart,
        FIRST_CHECK + CHECK_INTERVAL_SECONDS - 1
    ));
    cx.update(|cx| {
        let this = restart.workbench.read(cx);
        assert!(this.app_update.job.is_none());
        assert!(this.app_update.test_check.is_some());
    });
    let next = FIRST_CHECK + CHECK_INTERVAL_SECONDS;
    assert!(poll_at(cx, &restart, next));
    assert_eq!(saved(&path).0.app_update_last_attempt, Some(FIRST_CHECK));
    gate.run_next();
    assert_eq!(saved(&path).0.app_update_last_attempt, Some(next));
    cx.run_until_parked();
    assert!(poll_at(cx, &restart, next));
    wait_for_job(cx, &restart, next);
    cx.update(|cx| {
        assert!(matches!(
            restart.workbench.read(cx).app_update.status,
            Status::Current
        ));
    });
    assert_quiescent(cx, &restart, &gate);
}

#[gpui_kit::test]
fn cancelling_admission_keeps_the_save_and_allows_a_manual_check(cx: &mut TestAppContext) {
    let runtime = AppRuntime::new().expect("start private app runtime");
    let directory = tempfile::tempdir().expect("private preferences directory");
    let path = directory.path().join("preferences.json");
    let gate = SaveGate::new();
    let _session = UiTestSession::new(cx);
    let fixture = fixture(cx, &runtime, &path, &gate, Preferences::default());
    cx.update(|cx| {
        fixture.workbench.update(cx, |this, _| {
            this.app_update.release =
                Some(SelectedRelease::Installable(app_update::test_release()));
            this.app_update.test_check = Some(TestCheck::WaitForCancel);
            this.app_update.test_download = Some(TestDownload::WaitForCancel);
        });
    });
    assert!(poll_at(cx, &fixture, FIRST_CHECK));
    settle(cx, &fixture);
    cx.update_window(fixture.window, |_, window, cx| {
        assert_eq!(window.find("app-update-check").disabled(), Some(true));
        assert_eq!(window.find("app-update-download").disabled(), Some(true));
        window.click("app-update-check", cx);
        window.click("app-update-download", cx);
    })
    .expect("pending admission rejects updater buttons");
    cx.update_window(fixture.window, |_, window, cx| {
        fixture.workbench.update(cx, |this, cx| {
            this.start_app_update_check(cx);
            this.start_app_update_download(window, cx);
            assert!(this.app_update.job.is_none());
            assert!(this.app_update.test_check.is_some());
            assert!(this.app_update.test_download.is_some());
        });
    })
    .expect("reject direct download while admission is pending");
    click(cx, &fixture, "app-update-cancel");
    cx.update(|cx| {
        let this = fixture.workbench.read(cx);
        assert!(this.app_update.pending_auto_attempt.is_none());
        assert!(matches!(this.app_update.status, Status::Cancelled));
        assert!(!this.app_update_navigation_locked());
        assert!(
            this.preferences_io.busy(),
            "cancelling admission must retain its save"
        );
    });
    click(cx, &fixture, "app-update-check");
    let manual_attempt = cx.update(|cx| {
        let this = fixture.workbench.read(cx);
        assert!(
            this.app_update.job.is_some(),
            "manual checks remain available"
        );
        assert!(this.preferences_io.busy());
        assert!(!this.app_update_navigation_locked());
        assert!(this.app_update.pending_auto_attempt.is_none());
        this.preferences
            .app_update_last_attempt
            .expect("manual attempt time")
    });
    assert!(!poll_at(
        cx,
        &fixture,
        manual_attempt + CHECK_INTERVAL_SECONDS
    ));
    gate.run_next();
    cx.run_until_parked();
    gate.run_next();
    cx.run_until_parked();
    click(cx, &fixture, "app-update-cancel");
    wait_for_job(cx, &fixture, FIRST_CHECK);
    cx.update(|cx| {
        assert!(matches!(
            fixture.workbench.read(cx).app_update.status,
            Status::Cancelled
        ));
    });
    assert_quiescent(cx, &fixture, &gate);
}

#[gpui_kit::test]
fn disabling_automatic_check_revokes_admission_without_cancelling_coalesced_saves(
    cx: &mut TestAppContext,
) {
    let runtime = AppRuntime::new().expect("start private app runtime");
    let directory = tempfile::tempdir().expect("private preferences directory");
    let path = directory.path().join("preferences.json");
    let gate = SaveGate::new();
    let _session = UiTestSession::new(cx);
    let fixture = fixture(cx, &runtime, &path, &gate, Preferences::default());
    cx.update(|cx| {
        fixture.workbench.update(cx, |this, _| {
            this.app_update.test_check = Some(TestCheck::Result(CheckResult::UpToDate));
        });
    });
    assert!(poll_at(cx, &fixture, FIRST_CHECK));
    settle(cx, &fixture);
    click(cx, &fixture, "app-update-auto-check");
    cx.update(|cx| {
        let this = fixture.workbench.read(cx);
        assert!(!this.preferences.app_update_auto_check);
        assert!(this.app_update.pending_auto_attempt.is_none());
        assert!(this.app_update.job.is_none());
        assert!(matches!(this.app_update.status, Status::Cancelled));
        assert!(this.preferences_io.busy());
    });
    gate.run_next();
    cx.run_until_parked();
    assert!(!poll_at(
        cx,
        &fixture,
        FIRST_CHECK + CHECK_INTERVAL_SECONDS * 2
    ));
    gate.run_next();
    cx.run_until_parked();
    let (stored, _) = saved(&path);
    assert!(!stored.app_update_auto_check);
    assert_eq!(stored.app_update_last_attempt, Some(FIRST_CHECK));
    assert!(!poll_at(
        cx,
        &fixture,
        FIRST_CHECK + CHECK_INTERVAL_SECONDS * 2
    ));
    cx.update(|cx| {
        let this = fixture.workbench.read(cx);
        assert!(this.app_update.job.is_none());
        assert!(this.app_update.test_check.is_some());
    });
    assert_quiescent(cx, &fixture, &gate);
}

#[gpui_kit::test]
fn admission_requires_the_exact_latest_saved_snapshot(cx: &mut TestAppContext) {
    let runtime = AppRuntime::new().expect("start private app runtime");
    let directory = tempfile::tempdir().expect("private preferences directory");
    let path = directory.path().join("preferences.json");
    let gate = SaveGate::new();
    let _session = UiTestSession::new(cx);
    let fixture = fixture(cx, &runtime, &path, &gate, Preferences::default());
    cx.update(|cx| {
        fixture.workbench.update(cx, |this, _| {
            this.app_update.test_check = Some(TestCheck::Result(CheckResult::UpToDate));
        });
    });
    assert!(poll_at(cx, &fixture, FIRST_CHECK));
    cx.update(|cx| {
        fixture.workbench.update(cx, |this, cx| {
            // Model a superseding stale snapshot, followed by an in-memory change.
            this.preferences.app_update_last_attempt = None;
            this.save_preferences(cx);
            this.preferences.app_update_last_attempt = Some(FIRST_CHECK);
        });
    });
    gate.run_next();
    cx.run_until_parked();
    assert!(!poll_at(cx, &fixture, FIRST_CHECK));
    gate.run_next();
    cx.run_until_parked();
    assert_eq!(saved(&path).0.app_update_last_attempt, None);
    assert!(poll_at(cx, &fixture, FIRST_CHECK));
    cx.update(|cx| {
        let this = fixture.workbench.read(cx);
        assert!(matches!(this.app_update.status, Status::Cancelled));
        assert!(this.app_update.job.is_none());
        assert!(this.app_update.test_check.is_some());
        assert!(this.preferences_io.save_error.is_none());
    });
    assert_quiescent(cx, &fixture, &gate);
}

fn oversized_readable_document() -> String {
    let prefix = r#"{"language":"en","future":""}"#;
    format!(
        r#"{{"language":"en","future":"{}"}}"#,
        "x".repeat(1024 * 1024 - prefix.len())
    )
}

#[gpui_kit::test]
fn readable_but_unsavable_preferences_never_start_automatic_network(cx: &mut TestAppContext) {
    let runtime = AppRuntime::new().expect("start private app runtime");
    let directory = tempfile::tempdir().expect("private preferences directory");
    let path = directory.path().join("preferences.json");
    let original = oversized_readable_document();
    std::fs::write(&path, &original).expect("write valid readable preference document");
    let initial = saved(&path).0;
    let _session = UiTestSession::new(cx);
    // Each fresh Workbench represents a new launch reading the same intact file.
    for _ in 0..2 {
        let gate = SaveGate::new();
        let fixture = fixture(cx, &runtime, &path, &gate, initial.clone());
        cx.update(|cx| {
            fixture.workbench.update(cx, |this, _| {
                this.app_update.test_check = Some(TestCheck::Result(CheckResult::UpToDate));
            });
        });
        assert!(poll_at(cx, &fixture, FIRST_CHECK));
        gate.run_next();
        cx.run_until_parked();
        assert!(poll_at(cx, &fixture, FIRST_CHECK));
        settle(cx, &fixture);
        assert_eq!(
            std::fs::read_to_string(&path).expect("read original preferences"),
            original
        );
        cx.update_window(fixture.window, |_, window, cx| {
            let this = fixture.workbench.read(cx);
            assert!(matches!(this.app_update.status, Status::AutoSaveError));
            assert!(this.preferences_io.save_error.is_some());
            assert!(this.app_update.job.is_none());
            assert!(this.app_update.test_check.is_some());
            assert_eq!(
                window.find("settings-save-error").role(),
                Some(gpui_kit::Role::Alert)
            );
            assert_eq!(
                window.find("app-update-status").label(),
                Some(crate::i18n::tr("app.settings.update.auto_save_error").as_str())
            );
        })
        .expect("show save and skipped-check errors");
        for now in [FIRST_CHECK, FIRST_CHECK + CHECK_INTERVAL_SECONDS * 3] {
            assert!(
                !poll_at(cx, &fixture, now),
                "failed persistence must not retry automatically"
            );
        }
        assert_quiescent(cx, &fixture, &gate);
    }
}

#[gpui_kit::test]
fn unopenable_preferences_block_auto_but_not_a_manual_check(cx: &mut TestAppContext) {
    let runtime = AppRuntime::new().expect("start private app runtime");
    let directory = tempfile::tempdir().expect("private preferences directory");
    let parent = directory.path().join("not-a-directory");
    std::fs::write(&parent, b"private fixture").expect("create non-directory parent");
    let path = parent.join("preferences.json");
    let gate = SaveGate::new();
    let _session = UiTestSession::new(cx);
    let fixture = fixture(cx, &runtime, &path, &gate, Preferences::default());
    cx.update(|cx| {
        fixture.workbench.update(cx, |this, _| {
            this.app_update.test_check = Some(TestCheck::Result(CheckResult::UpToDate));
        });
    });
    assert!(poll_at(cx, &fixture, FIRST_CHECK));
    gate.run_next();
    cx.run_until_parked();
    assert!(poll_at(cx, &fixture, FIRST_CHECK));
    settle(cx, &fixture);
    cx.update(|cx| {
        let this = fixture.workbench.read(cx);
        assert!(matches!(this.app_update.status, Status::AutoSaveError));
        assert!(this.preferences_io.save_error.is_some());
        assert!(this.app_update.test_check.is_some());
        assert!(this.app_update.job.is_none());
    });
    assert!(!poll_at(
        cx,
        &fixture,
        FIRST_CHECK + CHECK_INTERVAL_SECONDS * 2
    ));
    click(cx, &fixture, "app-update-check");
    cx.update(|cx| {
        let this = fixture.workbench.read(cx);
        assert!(
            this.app_update.job.is_some(),
            "explicit check must not wait for saving"
        );
        assert!(this.preferences_io.busy());
    });
    gate.run_next();
    cx.run_until_parked();
    wait_for_job(cx, &fixture, FIRST_CHECK);
    cx.update(|cx| {
        let this = fixture.workbench.read(cx);
        assert!(matches!(this.app_update.status, Status::Current));
        assert!(this.preferences_io.save_error.is_some());
    });
    assert_eq!(
        std::fs::read(&parent).expect("read fixture parent"),
        b"private fixture"
    );
    assert_quiescent(cx, &fixture, &gate);
}

#[gpui_kit::test]
fn a_successful_older_save_cannot_hide_a_failed_latest_snapshot(cx: &mut TestAppContext) {
    let runtime = AppRuntime::new().expect("start private app runtime");
    let directory = tempfile::tempdir().expect("private preferences directory");
    let path = directory.path().join("preferences.json");
    std::fs::write(&path, br#"{"language":"en","future":{"keep":true}}"#)
        .expect("write private preferences");
    let initial = saved(&path).0;
    let gate = SaveGate::new();
    let _session = UiTestSession::new(cx);
    let fixture = fixture(cx, &runtime, &path, &gate, initial);
    cx.update(|cx| {
        fixture.workbench.update(cx, |this, cx| {
            this.preferences.accent = AccentColor::Blue;
            this.save_preferences(cx);
            this.app_update.test_check = Some(TestCheck::Result(CheckResult::UpToDate));
        });
    });
    assert!(poll_at(cx, &fixture, FIRST_CHECK));
    gate.run_next();
    assert_eq!(saved(&path).0.app_update_last_attempt, None);
    cx.run_until_parked();
    assert!(!poll_at(cx, &fixture, FIRST_CHECK));
    let original = oversized_readable_document();
    std::fs::write(&path, &original).expect("replace with readable but unsavable document");
    gate.run_next();
    cx.run_until_parked();
    assert!(poll_at(cx, &fixture, FIRST_CHECK));
    cx.update(|cx| {
        let this = fixture.workbench.read(cx);
        assert!(matches!(this.app_update.status, Status::AutoSaveError));
        assert!(this.preferences_io.save_error.is_some());
        assert!(this.app_update.test_check.is_some());
        assert!(this.app_update.job.is_none());
    });
    assert_eq!(
        std::fs::read_to_string(&path).expect("read intact document"),
        original
    );
    assert_quiescent(cx, &fixture, &gate);
}

#[gpui_kit::test]
fn pending_preference_save_does_not_block_restart_confirmation(cx: &mut TestAppContext) {
    let runtime = AppRuntime::new().expect("start private app runtime");
    let directory = tempfile::tempdir().expect("private preferences directory");
    let path = directory.path().join("preferences.json");
    let gate = SaveGate::new();
    let _session = UiTestSession::new(cx);
    let fixture = fixture(cx, &runtime, &path, &gate, Preferences::default());
    cx.update(|cx| {
        fixture.workbench.update(cx, |this, _| {
            this.app_update.automatic = false;
            this.app_update.test_check = Some(TestCheck::Result(CheckResult::Available(Box::new(
                app_update::test_release(),
            ))));
        });
    });
    click(cx, &fixture, "app-update-check");
    wait_for_job(cx, &fixture, FIRST_CHECK);
    cx.update(|cx| {
        fixture.workbench.update(cx, |this, _| {
            assert!(
                this.preferences_io.busy(),
                "the accepted save remains gated"
            );
            this.app_update.test_download = Some(TestDownload::Success);
        });
    });
    click(cx, &fixture, "app-update-download");
    wait_for_job(cx, &fixture, FIRST_CHECK);
    let downloaded_path = cx.update(|cx| {
        let this = fixture.workbench.read(cx);
        assert!(matches!(this.app_update.status, Status::Ready));
        assert!(this.preferences_io.busy());
        assert!(this.app_update_navigation_locked());
        this.app_update
            .downloaded
            .as_ref()
            .expect("downloaded update is ready")
            .file
            .path()
            .to_path_buf()
    });
    assert!(downloaded_path.exists());

    click(cx, &fixture, "app-update-restart");
    settle(cx, &fixture);
    cx.update_window(fixture.window, |_, window, cx| {
        assert_eq!(
            window.find_all_by_role(gpui_kit::Role::AlertDialog).len(),
            1,
            "a pending preference save must not reject restart confirmation"
        );
        assert!(fixture.workbench.read(cx).preferences_io.busy());
        window.press("escape", cx);
    })
    .expect("cancel the restart alert while the save remains gated");
    settle(cx, &fixture);
    cx.update_window(fixture.window, |_, window, cx| {
        assert!(
            window
                .find_all_by_role(gpui_kit::Role::AlertDialog)
                .is_empty()
        );
        assert!(fixture.workbench.read(cx).app_update.downloaded.is_some());
        assert!(!fixture.workbench.read(cx).shutting_down);
        window.click("app-update-cancel", cx);
        assert!(!fixture.workbench.read(cx).app_update_navigation_locked());
        assert!(fixture.workbench.read(cx).preferences_io.busy());
    })
    .expect("discard the update without cancelling the pending save");
    let cleanups = cx.update(|cx| {
        fixture
            .workbench
            .update(cx, |this, cx| this.app_update_cleanup_files(cx))
    });
    gate.run_next();
    cx.run_until_parked();
    settle(cx, &fixture);
    for cleanup in cleanups {
        crate::ui::lifecycle::join_background(cleanup);
    }
    assert!(!downloaded_path.exists());
    assert_quiescent(cx, &fixture, &gate);
}
