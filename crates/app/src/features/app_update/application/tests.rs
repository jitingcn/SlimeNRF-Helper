use super::{CHECK_INTERVAL_SECONDS, SelectedRelease, Status, TestCheck, TestDownload, check_due};
use crate::{
    app_runtime::AppRuntime,
    application::{
        FeaturePage, Workbench, preference_io::PreferenceIo, test_support::offline_workbench,
    },
    features::{
        app_update::service::{self as app_update, CheckResult},
        settings::store::{AccentColor, Preferences},
    },
    test_support::UiTestSession,
    update::Job,
};
use gpui_kit::{
    AnyWindowHandle, AppContext as _, Bounds, Entity, TestAppContext, WindowBounds, WindowOptions,
    px, size,
    test::{TestQueryExt as _, TestWindowExt as _},
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

struct Fixture {
    window: AnyWindowHandle,
    workbench: Entity<Workbench>,
}

enum Eligibility {
    Installable,
    ReleaseOnly,
}

fn fixture(
    cx: &mut TestAppContext,
    runtime: &AppRuntime,
    directory: &tempfile::TempDir,
    eligibility: Eligibility,
) -> Fixture {
    let executor = runtime.executor();
    let installable = matches!(eligibility, Eligibility::Installable);
    let path = directory.path().join("preferences.json");
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
                    this.preferences_io = PreferenceIo::for_test(path);
                    this.app_update.installable = installable;
                    this.app_update.automatic = false;
                    this
                })
            },
        )
        .expect("open isolated updater window")
    });
    cx.update_window(window, |_, window, _| window.activate_window())
        .expect("activate updater window");
    settle(cx, window);
    cx.update_window(window, |_, window, _| {
        assert!(
            window.is_window_active(),
            "keyboard interaction requires an active window"
        );
    })
    .expect("inspect updater window");
    Fixture { window, workbench }
}

fn settle(cx: &mut TestAppContext, window: AnyWindowHandle) {
    for _ in 0..2 {
        cx.run_until_parked();
        cx.update_window(window, |_, window, cx| {
            window.simulate_next_frame(cx);
            window.render_frame(cx);
        })
        .expect("settle Settings frame");
    }
}

fn search(cx: &mut TestAppContext, fixture: &Fixture, query: &str) {
    cx.update_window(fixture.window, |_, window, cx| {
        window.click("settings-search", cx);
        window.press("secondary-a", cx);
        window.input(query, cx);
    })
    .expect("search Settings");
    settle(cx, fixture.window);
}

fn open_updater(cx: &mut TestAppContext, fixture: &Fixture) {
    cx.update_window(fixture.window, |_, window, cx| {
        window.click("nav-settings", cx);
        window.click("settings-section-about", cx);
    })
    .expect("open Settings About");
    settle(cx, fixture.window);
    search(cx, fixture, "prerelease");
    cx.update_window(fixture.window, |_, window, _| {
        assert_eq!(window.find("settings-section-about").checked(), Some(true));
        assert!(window.find("app-update-check").visible());
    })
    .expect("inspect updater in About");
}

fn set_check(cx: &mut TestAppContext, fixture: &Fixture, check: TestCheck) {
    cx.update(|cx| {
        fixture.workbench.update(cx, |this, _| {
            assert!(this.app_update.job.is_none());
            this.app_update.test_check = Some(check);
        });
    });
}

fn set_download(cx: &mut TestAppContext, fixture: &Fixture, download: TestDownload) {
    cx.update(|cx| {
        fixture.workbench.update(cx, |this, _| {
            assert!(this.app_update.job.is_none());
            this.app_update.test_download = Some(download);
        });
    });
}

fn click(cx: &mut TestAppContext, fixture: &Fixture, id: &'static str) {
    cx.update_window(fixture.window, |_, window, cx| window.click(id, cx))
        .expect("activate updater control");
}

fn poll_until(cx: &mut TestAppContext, fixture: &Fixture, condition: impl Fn(&Workbench) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        // Give the real worker a chance to publish or finish before joining it.
        std::thread::sleep(Duration::from_millis(1));
        let finished = cx.update(|cx| {
            fixture.workbench.update(cx, |this, cx| {
                if this.poll_app_update(cx) {
                    cx.notify();
                }
                condition(this)
            })
        });
        cx.run_until_parked();
        if finished {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "updater worker did not reach the expected state"
        );
    }
    settle(cx, fixture.window);
}

fn wait_for_join(cx: &mut TestAppContext, fixture: &Fixture) {
    poll_until(cx, fixture, |this| this.app_update.job.is_none());
}

fn assert_status(cx: &mut TestAppContext, fixture: &Fixture, expected: &str) {
    cx.update_window(fixture.window, |_, window, _| {
        let status = window.find("app-update-status");
        assert_eq!(status.role(), Some(gpui_kit::Role::Status));
        assert_eq!(status.label(), Some(expected));
    })
    .expect("inspect updater status");
}

fn check_result(cx: &mut TestAppContext, fixture: &Fixture, result: CheckResult) {
    set_check(cx, fixture, TestCheck::Result(result));
    click(cx, fixture, "app-update-check");
    cx.update(|cx| {
        let state = &fixture.workbench.read(cx).app_update;
        assert!(matches!(state.status, Status::Checking));
        assert!(
            state.job.is_some(),
            "completed workers are retained until polled"
        );
    });
    wait_for_join(cx, fixture);
}

fn available(cx: &mut TestAppContext, fixture: &Fixture) {
    check_result(
        cx,
        fixture,
        CheckResult::Available(Box::new(app_update::test_release())),
    );
    cx.update(|cx| {
        let state = &fixture.workbench.read(cx).app_update;
        assert!(matches!(state.status, Status::Available));
        assert_eq!(
            state
                .release
                .as_ref()
                .and_then(SelectedRelease::installable),
            Some(&app_update::test_release())
        );
    });
    cx.update_window(fixture.window, |_, window, _| {
        assert!(window.try_find("app-update-prerelease").is_none());
    })
    .expect("stable release has no prerelease badge");
}

fn download_ready(cx: &mut TestAppContext, fixture: &Fixture) {
    set_download(cx, fixture, TestDownload::Success);
    click(cx, fixture, "app-update-download");
    wait_for_join(cx, fixture);
    cx.update(|cx| {
        let state = &fixture.workbench.read(cx).app_update;
        assert!(matches!(state.status, Status::Ready));
        assert_eq!(
            state.downloaded.as_ref().map(|download| &download.release),
            state
                .release
                .as_ref()
                .and_then(SelectedRelease::installable)
        );
    });
}

fn assert_no_workers(cx: &mut TestAppContext, fixture: &Fixture) {
    settle(cx, fixture.window);
    let (cleanups, downloaded_path) = cx.update(|cx| {
        fixture.workbench.update(cx, |this, cx| {
            assert!(
                this.app_update.job.is_none(),
                "updater worker must be joined"
            );
            assert!(this.flash.job.is_none(), "fixture activity must be joined");
            assert!(
                !this.preferences_io.busy(),
                "private preference saves must finish"
            );
            let downloaded_path = this
                .app_update
                .downloaded
                .as_ref()
                .map(|download| download.file.path().to_path_buf());
            (this.app_update_cleanup_files(cx), downloaded_path)
        })
    });
    cx.run_until_parked();
    for cleanup in cleanups {
        crate::application::lifecycle::join_background(cleanup);
    }
    if let Some(path) = downloaded_path {
        assert!(
            !path.exists(),
            "synthetic downloaded file must be removed after cleanup"
        );
    }
}

#[test]
fn automatic_check_interval_handles_boundaries_and_clock_reversal() {
    let now = CHECK_INTERVAL_SECONDS * 2;
    assert!(check_due(None, now));
    assert!(!check_due(Some(now - CHECK_INTERVAL_SECONDS + 1), now));
    assert!(check_due(Some(now - CHECK_INTERVAL_SECONDS), now));
    assert!(!check_due(Some(now + 1), now));
}

#[test]
fn version_label_keeps_prerelease_marker_independent_of_the_version_suffix() {
    assert_eq!(super::version_label("0.1.1", false), "0.1.1");
    assert_eq!(
        super::version_label("0.1.1+build-with-hyphens", false),
        "0.1.1+build-with-hyphens"
    );
    assert_eq!(
        super::version_label("0.1.1+abcdef0.dirty", false),
        "0.1.1+abcdef0.dirty"
    );
    for (version, marked) in [
        ("0.1.1", true),
        ("0.1.1+build-with-hyphens", true),
        ("0.1.1-rc.1+build", false),
        ("0.1.1-rc.1+abcdef0.dirty", false),
    ] {
        let label = super::version_label(version, marked);
        let prefix = format!("{version} · ");
        assert!(
            label
                .strip_prefix(prefix.as_str())
                .is_some_and(|badge| !badge.is_empty()),
            "version {version:?} with prerelease marker {marked} must carry a badge: {label:?}"
        );
    }
}

#[gpui_kit::test]
fn release_only_builds_check_preview_metadata_without_automatic_or_install_actions(
    cx: &mut TestAppContext,
) {
    let runtime = AppRuntime::new().expect("start app runtime");
    let directory = tempfile::tempdir().expect("private preferences directory");
    let _session = UiTestSession::new(cx);
    // The same capability state represents non-Windows and Windows dev/custom builds.
    let fixture = fixture(cx, &runtime, &directory, Eligibility::ReleaseOnly);
    open_updater(cx, &fixture);
    assert_status(
        cx,
        &fixture,
        &crate::i18n::tr("app.settings.update.status_idle"),
    );
    cx.update_window(fixture.window, |_, window, cx| {
        assert!(window.try_find("app-update-auto-check").is_none());
        assert!(window.find("app-update-check").visible());
        assert!(window.try_find("app-update-download").is_none());
        assert!(window.try_find("app-update-restart").is_none());
        let this = fixture.workbench.read(cx);
        assert!(!this.app_update.installable);
        assert!(!this.app_update.automatic);
        assert!(this.preferences.app_update_auto_check);
        assert!(this.preferences.app_update_last_attempt.is_none());
        assert!(this.app_update.job.is_none());
    })
    .expect("release-only builds omit automatic checking");

    // The preview is selected through the same control used by installed builds.
    click(cx, &fixture, "app-update-prereleases");
    let mut preview = app_update::test_release_info();
    preview.version = "9.0.0-rc.3".into();
    preview.tag = "v9.0.0-rc.3".into();
    preview.prerelease = true;
    preview.notes = "Release-only preview notes".into();
    preview.html_url =
        "https://github.com/jitingcn/SlimeNRF-Helper/releases/tag/v9.0.0-rc.3".into();
    set_check(
        cx,
        &fixture,
        TestCheck::ByChannel {
            stable: CheckResult::NoEligibleRelease,
            preview: CheckResult::ReleasePage(Box::new(preview.clone())),
        },
    );
    // A pending download result must stay unused, including after a check succeeds.
    set_download(cx, &fixture, TestDownload::Success);
    cx.update(|cx| {
        fixture.workbench.update(cx, |this, cx| {
            assert!(this.preferences.app_update_include_prereleases);
            assert!(!this.poll_app_update_at(Some(CHECK_INTERVAL_SECONDS), cx));
            assert!(this.app_update.job.is_none());
            assert!(this.app_update.pending_auto_attempt.is_none());
            assert!(this.app_update.test_check.is_some());
            assert!(this.preferences.app_update_last_attempt.is_none());
            this.preferences.app_update_last_attempt = Some(0);
            assert!(!this.poll_app_update_at(Some(CHECK_INTERVAL_SECONDS), cx));
            assert!(this.app_update.job.is_none());
            assert!(this.app_update.pending_auto_attempt.is_none());
            assert!(this.app_update.test_check.is_some());
            assert_eq!(this.preferences.app_update_last_attempt, Some(0));
            this.preferences.app_update_last_attempt = None;
        });
    });

    click(cx, &fixture, "app-update-check");
    cx.update(|cx| {
        let this = fixture.workbench.read(cx);
        assert!(matches!(this.app_update.status, Status::Checking));
        assert!(this.app_update.job.is_some());
        assert!(this.app_update.test_check.is_none());
        assert!(this.preferences.app_update_last_attempt.is_some());
    });
    wait_for_join(cx, &fixture);
    cx.update_window(fixture.window, |_, window, cx| {
        let state = &fixture.workbench.read(cx).app_update;
        assert!(matches!(state.status, Status::Available));
        match &state.release {
            Some(SelectedRelease::PageOnly(info)) => assert_eq!(info, &preview),
            _ => panic!("release-only check must retain metadata without an asset"),
        }
        assert!(state.downloaded.is_none());
        assert!(state.test_download.is_some());
        assert!(state.job.is_none());
        let status = window.find("app-update-status");
        assert_eq!(status.role(), Some(gpui_kit::Role::Status));
        assert!(status.visible());
        assert_eq!(
            status.label(),
            Some("Version 9.0.0-rc.3 (v9.0.0-rc.3) is available.")
        );
        let badge = window.find("app-update-prerelease");
        assert_eq!(badge.role(), Some(gpui_kit::Role::Label));
        assert_eq!(
            badge.label(),
            Some(crate::i18n::tr("app.settings.update.prerelease_badge").as_str())
        );
        assert!(badge.visible());
        let notes = window.find("app-update-release-notes");
        assert_eq!(notes.role(), Some(gpui_kit::Role::Label));
        assert_eq!(notes.label(), Some(preview.notes.as_str()));
        assert!(notes.visible());
        assert!(window.find("app-update-release-link").visible());
        assert!(window.try_find("app-update-download").is_none());
        assert!(window.try_find("app-update-restart").is_none());
    })
    .expect("inspect asset-free release metadata");
    assert_eq!(cx.opened_url(), None);
    click(cx, &fixture, "app-update-release-link");
    cx.run_until_parked();
    assert_eq!(cx.opened_url().as_deref(), Some(preview.html_url.as_str()));

    set_check(cx, &fixture, TestCheck::WaitForCancel);
    click(cx, &fixture, "app-update-check");
    cx.update_window(fixture.window, |_, window, cx| {
        assert_eq!(window.find("app-update-check").disabled(), Some(true));
        assert_eq!(window.find("app-update-prereleases").disabled(), Some(true));
        assert!(window.try_find("app-update-download").is_none());
        assert!(window.try_find("app-update-restart").is_none());
        window.click("app-update-cancel", cx);
        let state = &fixture.workbench.read(cx).app_update;
        assert!(state.job.is_some());
        assert!(state.cancelling);
        assert!(matches!(state.status, Status::Cancelling));
    })
    .expect("cancel a release-only check");
    wait_for_join(cx, &fixture);
    assert_status(
        cx,
        &fixture,
        &crate::i18n::tr("app.settings.update.status_cancelled"),
    );
    check_result(
        cx,
        &fixture,
        CheckResult::ReleasePage(Box::new(preview.clone())),
    );
    cx.update_window(fixture.window, |_, window, cx| {
        let state = &fixture.workbench.read(cx).app_update;
        assert!(matches!(state.status, Status::Available));
        assert!(matches!(
            &state.release,
            Some(SelectedRelease::PageOnly(info)) if info == &preview
        ));
        assert!(state.test_download.is_some());
        assert!(state.downloaded.is_none());
        assert!(window.find("app-update-release-link").visible());
        assert!(window.try_find("app-update-download").is_none());
        assert!(window.try_find("app-update-restart").is_none());
    })
    .expect("retry the cancelled release-only check");

    // Without an injected result the offline fixture must never fall back to network.
    click(cx, &fixture, "app-update-check");
    cx.update_window(fixture.window, |_, window, cx| {
        assert!(
            window
                .find("app-update-status")
                .label()
                .is_some_and(|text| text.contains("No offline check result was provided"))
        );
        let state = &fixture.workbench.read(cx).app_update;
        assert!(state.job.is_none());
        assert!(state.release.is_none());
        assert!(state.downloaded.is_none());
        assert!(state.test_download.is_some());
    })
    .expect("inspect offline check failure");
    check_result(
        cx,
        &fixture,
        CheckResult::ReleasePage(Box::new(preview.clone())),
    );
    cx.update_window(fixture.window, |_, window, cx| {
        let state = &fixture.workbench.read(cx).app_update;
        assert!(matches!(state.status, Status::Available));
        assert!(matches!(
            &state.release,
            Some(SelectedRelease::PageOnly(info)) if info == &preview
        ));
        assert!(state.test_download.is_some());
        assert!(state.downloaded.is_none());
        assert!(window.find("app-update-release-link").visible());
        assert!(window.try_find("app-update-download").is_none());
        assert!(window.try_find("app-update-restart").is_none());
    })
    .expect("retry the failed release-only check");
    poll_until(cx, &fixture, |this| !this.preferences_io.busy());
    assert_no_workers(cx, &fixture);
    let stored: Preferences = serde_json::from_slice(
        &std::fs::read(directory.path().join("preferences.json")).expect("private preference save"),
    )
    .expect("decode private preferences");
    cx.update(|cx| assert_eq!(stored, fixture.workbench.read(cx).preferences));
    assert!(stored.app_update_auto_check);
    assert!(stored.app_update_include_prereleases);
    assert!(stored.app_update_last_attempt.is_some());
}

#[gpui_kit::test]
fn installable_build_with_metadata_only_offers_release_page(cx: &mut TestAppContext) {
    let runtime = AppRuntime::new().expect("start app runtime");
    let directory = tempfile::tempdir().expect("private preferences directory");
    let _session = UiTestSession::new(cx);
    // A metadata-only result must never authorize installation, even for a marked build.
    let fixture = fixture(cx, &runtime, &directory, Eligibility::Installable);
    open_updater(cx, &fixture);
    let info = app_update::test_release_info();
    set_download(cx, &fixture, TestDownload::Success);
    check_result(
        cx,
        &fixture,
        CheckResult::ReleasePage(Box::new(info.clone())),
    );
    cx.update_window(fixture.window, |_, window, cx| {
        let state = &fixture.workbench.read(cx).app_update;
        assert!(state.installable);
        assert!(matches!(state.status, Status::Available));
        assert!(matches!(
            &state.release,
            Some(SelectedRelease::PageOnly(release)) if release == &info
        ));
        assert!(state.test_download.is_some());
        assert!(state.downloaded.is_none());
        assert!(state.job.is_none());
        let expected = format!("Version {} ({}) is available.", info.version, info.tag);
        let status = window.find("app-update-status");
        assert!(status.visible());
        assert_eq!(status.label(), Some(expected.as_str()));
        let notes = window.find("app-update-release-notes");
        assert!(notes.visible());
        assert_eq!(notes.label(), Some(info.notes.as_str()));
        assert!(window.find("app-update-release-link").visible());
        assert!(window.try_find("app-update-download").is_none());
        assert!(window.try_find("app-update-restart").is_none());
    })
    .expect("inspect release-only result for an installable build");

    cx.update_window(fixture.window, |_, window, cx| {
        fixture.workbench.update(cx, |this, cx| {
            this.start_app_update_download(window, cx);
            let state = &this.app_update;
            assert!(matches!(state.status, Status::Available));
            assert!(state.test_download.is_some());
            assert!(state.downloaded.is_none());
            assert!(state.job.is_none());
        });
    })
    .expect("reject download of metadata-only release");
    assert_eq!(cx.opened_url(), None);
    click(cx, &fixture, "app-update-release-link");
    cx.run_until_parked();
    assert_eq!(cx.opened_url().as_deref(), Some(info.html_url.as_str()));
    poll_until(cx, &fixture, |this| !this.preferences_io.busy());
    assert_no_workers(cx, &fixture);
}

#[gpui_kit::test]
fn stable_and_preview_checks_follow_the_controlled_preference(cx: &mut TestAppContext) {
    // Both the runtime and private store outlive UiTestSession's window teardown.
    let runtime = AppRuntime::new().expect("start app runtime");
    let directory = tempfile::tempdir().expect("private preferences directory");
    let _session = UiTestSession::new(cx);
    let fixture = fixture(cx, &runtime, &directory, Eligibility::Installable);
    open_updater(cx, &fixture);

    assert_status(
        cx,
        &fixture,
        &crate::i18n::tr("app.settings.update.status_idle"),
    );
    cx.update_window(fixture.window, |_, window, cx| {
        assert!(window.try_find("app-update-auto-check").is_none());
        assert_eq!(window.find("app-update-prereleases").checked(), Some(false));
        let this = fixture.workbench.read(cx);
        assert!(this.preferences.app_update_auto_check);
        assert!(!this.preferences.app_update_include_prereleases);
        assert!(this.preferences.app_update_last_attempt.is_none());
    })
    .expect("inspect initial updater controls");
    // Even a due timestamp cannot cause an automatic check in this explicit fixture.
    cx.update(|cx| {
        fixture.workbench.update(cx, |this, cx| {
            assert!(!this.poll_app_update(cx));
            assert!(this.app_update.job.is_none());
            assert!(this.preferences.app_update_last_attempt.is_none());
            this.preferences.app_update_last_attempt = Some(0);
            assert!(!this.poll_app_update(cx));
            assert!(this.app_update.job.is_none());
            assert_eq!(this.preferences.app_update_last_attempt, Some(0));
            this.preferences.app_update_last_attempt = None;
        });
    });

    let mut preview = app_update::test_release();
    preview.prerelease = true;
    assert_eq!(preview.tag, "v9.0.0");
    let by_channel = || TestCheck::ByChannel {
        stable: CheckResult::NoEligibleRelease,
        preview: CheckResult::Available(Box::new(preview.clone())),
    };
    set_check(cx, &fixture, by_channel());
    click(cx, &fixture, "app-update-check");
    let attempted_at = cx.update(|cx| {
        fixture
            .workbench
            .read(cx)
            .preferences
            .app_update_last_attempt
            .expect("explicit check records its attempt before the worker completes")
    });
    wait_for_join(cx, &fixture);
    assert_status(
        cx,
        &fixture,
        &crate::i18n::tr("app.settings.update.status_no_stable"),
    );
    cx.update(|cx| {
        let this = fixture.workbench.read(cx);
        assert!(matches!(this.app_update.status, Status::NoStable));
        assert!(this.app_update.release.is_none());
        assert_eq!(this.preferences.app_update_last_attempt, Some(attempted_at));
    });

    cx.update_window(fixture.window, |_, window, cx| {
        window.click("app-update-prereleases", cx);
        assert_eq!(window.find("app-update-prereleases").checked(), Some(true));
        assert!(
            fixture
                .workbench
                .read(cx)
                .preferences
                .app_update_include_prereleases
        );
        // Kit checkboxes preserve the prior focus on pointer presses.
        window.click("settings-search", cx);
    })
    .expect("change prerelease preference by pointer");
    let mut focused = false;
    for _ in 0..40 {
        cx.update_window(fixture.window, |_, window, cx| window.press("tab", cx))
            .expect("navigate to prerelease preference");
        settle(cx, fixture.window);
        if cx
            .update_window(fixture.window, |_, window, _| {
                window.find("app-update-prereleases").focused() == Some(true)
            })
            .expect("inspect prerelease focus")
        {
            focused = true;
            break;
        }
    }
    assert!(focused, "prerelease preference must be reachable by Tab");
    cx.update_window(fixture.window, |_, window, cx| {
        assert_eq!(window.find("app-update-prereleases").focused(), Some(true));
        window.press("space", cx);
        assert_eq!(window.find("app-update-prereleases").checked(), Some(false));
        assert!(
            !fixture
                .workbench
                .read(cx)
                .preferences
                .app_update_include_prereleases
        );
        window.press("space", cx);
        assert_eq!(window.find("app-update-prereleases").checked(), Some(true));
        assert!(
            fixture
                .workbench
                .read(cx)
                .preferences
                .app_update_include_prereleases
        );
        assert!(matches!(
            fixture.workbench.read(cx).app_update.status,
            Status::Idle
        ));
    })
    .expect("change prerelease preference by keyboard");
    set_check(cx, &fixture, by_channel());
    click(cx, &fixture, "app-update-check");
    wait_for_join(cx, &fixture);
    cx.update_window(fixture.window, |_, window, cx| {
        let state = &fixture.workbench.read(cx).app_update;
        assert!(matches!(state.status, Status::Available));
        assert_eq!(
            state
                .release
                .as_ref()
                .and_then(SelectedRelease::installable),
            Some(&preview)
        );
        let badge = window.find("app-update-prerelease");
        assert_eq!(badge.role(), Some(gpui_kit::Role::Label));
        assert_eq!(
            badge.label(),
            Some(crate::i18n::tr("app.settings.update.prerelease_badge").as_str())
        );
        assert!(badge.visible());
        assert!(
            window
                .find("app-update-status")
                .label()
                .is_some_and(|text| text.contains("9.0.0") && text.contains("v9.0.0"))
        );
        assert_eq!(
            window.find("app-update-release-notes").label(),
            Some("Synthetic release notes")
        );
        assert!(window.find("app-update-download").visible());
    })
    .expect("inspect selected preview release");
    assert_eq!(cx.opened_url(), None);
    click(cx, &fixture, "app-update-release-link");
    cx.run_until_parked();
    assert_eq!(
        cx.opened_url().as_deref(),
        Some("https://github.com/jitingcn/SlimeNRF-Helper/releases/tag/v9.0.0")
    );
    assert_no_workers(cx, &fixture);
    let stored: Preferences = serde_json::from_slice(
        &std::fs::read(directory.path().join("preferences.json")).expect("private preference save"),
    )
    .expect("decode private preferences");
    cx.update(|cx| assert_eq!(stored, fixture.workbench.read(cx).preferences));
    assert!(stored.app_update_include_prereleases);
    assert!(stored.app_update_last_attempt.is_some());
}

#[gpui_kit::test]
fn check_results_and_cancellation_remain_distinct(cx: &mut TestAppContext) {
    let runtime = AppRuntime::new().expect("start app runtime");
    let directory = tempfile::tempdir().expect("private preferences directory");
    let _session = UiTestSession::new(cx);
    let fixture = fixture(cx, &runtime, &directory, Eligibility::Installable);
    open_updater(cx, &fixture);

    for (result, key) in [
        (CheckResult::UpToDate, "app.settings.update.status_current"),
        (
            CheckResult::NoCompatibleRelease,
            "app.settings.update.status_no_compatible",
        ),
        (
            CheckResult::NoEligibleRelease,
            "app.settings.update.status_no_stable",
        ),
    ] {
        check_result(cx, &fixture, result);
        assert_status(cx, &fixture, &crate::i18n::tr(key));
        cx.update_window(fixture.window, |_, window, cx| {
            assert!(window.try_find("app-update-download").is_none());
            assert!(fixture.workbench.read(cx).app_update.release.is_none());
        })
        .expect("inspect empty check result");
    }
    click(cx, &fixture, "app-update-prereleases");
    check_result(cx, &fixture, CheckResult::NoEligibleRelease);
    assert_status(
        cx,
        &fixture,
        &crate::i18n::tr("app.settings.update.status_no_eligible"),
    );

    set_check(cx, &fixture, TestCheck::WaitForCancel);
    click(cx, &fixture, "app-update-check");
    cx.update_window(fixture.window, |_, window, cx| {
        let this = fixture.workbench.read(cx);
        assert!(!this.app_update_navigation_locked());
        assert!(this.app_update.job.is_some());
        window.click("nav-home", cx);
        assert!(fixture.workbench.read(cx).active_page == FeaturePage::Home);
        assert!(fixture.workbench.read(cx).app_update.job.is_some());
        window.click("nav-settings", cx);
        assert!(fixture.workbench.read(cx).active_page == FeaturePage::Settings);
    })
    .expect("navigate away and back during a real check");
    cx.update_window(fixture.window, |_, window, cx| {
        assert_eq!(window.find("app-update-check").disabled(), Some(true));
        assert_eq!(window.find("app-update-prereleases").disabled(), Some(true));
        window.click("app-update-cancel", cx);
        let state = &fixture.workbench.read(cx).app_update;
        assert!(state.job.is_some());
        assert!(state.cancelling);
        assert!(matches!(state.status, Status::Cancelling));
    })
    .expect("cancel real check worker");
    wait_for_join(cx, &fixture);
    assert_status(
        cx,
        &fixture,
        &crate::i18n::tr("app.settings.update.status_cancelled"),
    );
    available(cx, &fixture);
    set_download(cx, &fixture, TestDownload::WaitForCancel);
    click(cx, &fixture, "app-update-download");
    click(cx, &fixture, "app-update-cancel");
    wait_for_join(cx, &fixture);
    assert_status(
        cx,
        &fixture,
        &crate::i18n::tr("app.settings.update.status_cancelled"),
    );
    cx.update(|cx| assert!(fixture.workbench.read(cx).app_update.release.is_some()));
    assert_no_workers(cx, &fixture);
}

#[gpui_kit::test]
fn failed_download_retries_progress_cancel_and_ready(cx: &mut TestAppContext) {
    let runtime = AppRuntime::new().expect("start app runtime");
    let directory = tempfile::tempdir().expect("private preferences directory");
    let _session = UiTestSession::new(cx);
    let fixture = fixture(cx, &runtime, &directory, Eligibility::Installable);
    open_updater(cx, &fixture);
    available(cx, &fixture);

    set_download(
        cx,
        &fixture,
        TestDownload::Failure("synthetic transfer failure".into()),
    );
    click(cx, &fixture, "app-update-download");
    cx.update(|cx| {
        assert!(fixture.workbench.read(cx).app_update_navigation_locked());
    });
    wait_for_join(cx, &fixture);
    cx.update_window(fixture.window, |_, window, cx| {
        assert!(
            window
                .find("app-update-status")
                .label()
                .is_some_and(|text| text.contains("synthetic transfer failure"))
        );
        assert!(window.find("app-update-download").visible());
        let state = &fixture.workbench.read(cx).app_update;
        assert!(state.release.is_some());
        assert!(state.downloaded.is_none());
        assert!(!fixture.workbench.read(cx).app_update_navigation_locked());
        window.click("nav-home", cx);
        assert!(fixture.workbench.read(cx).active_page == FeaturePage::Home);
        window.click("nav-settings", cx);
        assert!(fixture.workbench.read(cx).active_page == FeaturePage::Settings);
    })
    .expect("inspect retryable download failure");

    // Dropping the sender also releases the worker if an assertion unwinds.
    let (release, wait_for_release) = mpsc::channel();
    let cancelled = Arc::new(AtomicBool::new(false));
    set_download(
        cx,
        &fixture,
        TestDownload::GatedProgress {
            cancelled: Arc::clone(&cancelled),
            release: wait_for_release,
        },
    );
    click(cx, &fixture, "app-update-download");
    poll_until(cx, &fixture, |this| {
        this.app_update.progress == Some((2, 8))
    });
    cx.update(|cx| assert!(fixture.workbench.read(cx).app_update_navigation_locked()));
    assert_status(cx, &fixture, "Downloading update: 2 / 8 bytes");
    cx.update_window(fixture.window, |_, window, cx| {
        assert_eq!(window.find("app-update-download").disabled(), Some(true));
        assert_eq!(window.find("app-update-check").disabled(), Some(true));
        assert_eq!(window.find("app-update-prereleases").disabled(), Some(true));
        window.click("app-update-cancel", cx);
        assert!(!fixture.workbench.read(cx).app_update_navigation_locked());
        assert!(fixture.workbench.read(cx).app_update.job.is_some());
    })
    .expect("cancel progressing download");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !cancelled.load(Ordering::Acquire) {
        assert!(
            Instant::now() < deadline,
            "download worker must observe cancellation"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    cx.update_window(fixture.window, |_, window, cx| {
        let state = &fixture.workbench.read(cx).app_update;
        assert!(
            state.job.is_some(),
            "cancellation must retain the unjoined worker"
        );
        assert!(state.cancelling);
        assert!(matches!(state.status, Status::Cancelling));
        assert_eq!(window.find("app-update-cancel").disabled(), Some(true));
        assert_eq!(window.find("app-update-check").disabled(), Some(true));
        window.click("app-update-check", cx);
        assert!(fixture.workbench.read(cx).app_update.job.is_some());
        assert!(!fixture.workbench.read(cx).app_update_navigation_locked());
        window.click("nav-home", cx);
        assert!(fixture.workbench.read(cx).active_page == FeaturePage::Home);
        assert!(fixture.workbench.read(cx).app_update.job.is_some());
        window.click("nav-settings", cx);
        assert!(fixture.workbench.read(cx).active_page == FeaturePage::Settings);
    })
    .expect("inspect cancellation before worker joins");
    assert_status(
        cx,
        &fixture,
        &crate::i18n::tr("app.settings.update.status_cancelling"),
    );
    release.send(()).expect("release cancelled worker");
    wait_for_join(cx, &fixture);
    assert_status(
        cx,
        &fixture,
        &crate::i18n::tr("app.settings.update.status_cancelled"),
    );
    download_ready(cx, &fixture);
    cx.update_window(fixture.window, |_, window, cx| {
        let status = window.find("app-update-status");
        assert!(status.label().is_some_and(|text| text.contains("9.0.0")
            && text.contains("v9.0.0")
            && text.contains("ready")));
        let notes = window.find("app-update-release-notes");
        assert_eq!(notes.role(), Some(gpui_kit::Role::Label));
        assert_eq!(notes.label(), Some("Synthetic release notes"));
        assert!(notes.visible());
        assert!(window.find("app-update-restart").visible());
        assert!(window.find("app-update-cancel").visible());
        assert_eq!(window.find("nav-home").disabled(), Some(true));
        assert!(fixture.workbench.read(cx).app_update_navigation_locked());
        assert!(window.try_find("app-update-download").is_none());
        assert!(matches!(
            fixture.workbench.read(cx).app_update.status,
            Status::Ready
        ));
    })
    .expect("inspect downloaded release");
    assert_no_workers(cx, &fixture);
}

#[gpui_kit::test]
fn download_locks_only_navigation_and_cancel_unlocks_before_join(cx: &mut TestAppContext) {
    let runtime = AppRuntime::new().expect("start app runtime");
    let directory = tempfile::tempdir().expect("private preferences directory");
    let _session = UiTestSession::new(cx);
    let fixture = fixture(cx, &runtime, &directory, Eligibility::Installable);
    open_updater(cx, &fixture);
    available(cx, &fixture);

    let (release, wait_for_release) = mpsc::channel();
    let cancelled = Arc::new(AtomicBool::new(false));
    set_download(
        cx,
        &fixture,
        TestDownload::GatedProgress {
            cancelled: Arc::clone(&cancelled),
            release: wait_for_release,
        },
    );
    click(cx, &fixture, "app-update-download");
    cx.update_window(fixture.window, |_, window, _| {
        assert_eq!(window.find("settings-search").value(), Some("prerelease"));
    })
    .expect("preserve a matching search when the download starts");
    poll_until(cx, &fixture, |this| {
        this.app_update.progress == Some((2, 8))
    });
    search(cx, &fixture, "no matching settings entry");
    assert_status(cx, &fixture, "Downloading update: 2 / 8 bytes");
    cx.update_window(fixture.window, |_, window, cx| {
        assert!(window.find("app-update-cancel").visible());
        assert_eq!(window.find("settings-search").focused(), Some(true));
        for id in [
            "nav-home",
            "nav-device-setup",
            "nav-uf2",
            "nav-esb-ota",
            "nav-raw-data",
            "nav-vqf-replay",
            "nav-hid-recording",
            "nav-tools",
            "nav-gyroscope",
        ] {
            assert_eq!(window.find(id).disabled(), Some(true), "{id} is locked");
        }
        let this = fixture.workbench.read(cx);
        assert!(this.app_update_navigation_locked());
        assert!(this.active_page == FeaturePage::Settings);
        assert!(this.flash.job.is_none());
        window.click("nav-uf2", cx);
        window.click("nav-raw-data", cx);
        let this = fixture.workbench.read(cx);
        assert!(this.active_page == FeaturePage::Settings);
        assert!(
            this.flash.job.is_none(),
            "blocked navigation must not load the catalog"
        );
        assert_eq!(window.find("settings-search").focused(), Some(true));
        fixture.workbench.update(cx, |this, cx| {
            assert!(this.set_feature_page(FeaturePage::Settings, window, cx));
            assert!(!this.set_feature_page(FeaturePage::Uf2, window, cx));
            assert!(!this.set_feature_page(FeaturePage::RawCapture, window, cx));
            assert!(this.active_page == FeaturePage::Settings);
        });
        assert_eq!(window.find("settings-search").focused(), Some(true));
        let collapsed = fixture.workbench.read(cx).navigation_collapsed;
        window.click("nav-toggle", cx);
        assert_eq!(fixture.workbench.read(cx).navigation_collapsed, !collapsed);
        window.click("nav-toggle", cx);
        assert_eq!(fixture.workbench.read(cx).navigation_collapsed, collapsed);
    })
    .expect("locked sidebar rejects page changes and their side effects");

    search(cx, &fixture, "registry");
    cx.update_window(fixture.window, |_, window, cx| {
        assert_eq!(window.find("manage-devices").disabled(), Some(true));
        window.click("manage-devices", cx);
        assert!(!fixture.workbench.read(cx).registry_is_open());
        fixture
            .workbench
            .update(cx, |this, cx| this.open_device_registry(window, cx));
        assert!(!fixture.workbench.read(cx).registry_is_open());
        assert_eq!(window.find("settings-search").focused(), Some(true));
        assert!(window.find("app-update-cancel").visible());
    })
    .expect("locked Settings cannot open the device registry");

    search(cx, &fixture, "sakura");
    cx.update_window(fixture.window, |_, window, cx| {
        assert_eq!(
            fixture.workbench.read(cx).preferences.accent,
            AccentColor::Neutral
        );
        window.click("settings-accent-sakura", cx);
        let this = fixture.workbench.read(cx);
        assert_eq!(this.preferences.accent, AccentColor::Sakura);
        assert!(this.app_update_navigation_locked());
        assert!(this.app_update.job.is_some());
    })
    .expect("change an unrelated preference during the download");
    cx.update(|cx| {
        fixture.workbench.update(cx, |this, cx| {
            let receiver = crate::domain::ReceiverEndpoint {
                key: "isolated-inventory-receiver".into(),
                path: b"isolated-inventory-receiver".to_vec(),
                label: "Isolated receiver".into(),
                serial_number: None,
            };
            // Enumeration is real, but reconciliation must never open host devices.
            this.monitor =
                crate::discovery::ReceiverMonitor::fixture(receiver, 0, "010203040506", false);
            let before = this.inventory_generation;
            this.scan_inventory(cx);
            assert_eq!(this.inventory_generation, before + 1);
            assert!(this.scanning_inventory);
            this.scan_inventory(cx);
            assert_eq!(this.inventory_generation, before + 1);
        });
    });
    poll_until(cx, &fixture, |this| {
        !this.scanning_inventory && !this.preferences_io.busy()
    });
    cx.update(|cx| {
        fixture.workbench.update(cx, |this, cx| {
            let before = this.inventory_generation;
            this.shutting_down = true;
            this.scan_inventory(cx);
            assert_eq!(this.inventory_generation, before);
            assert!(!this.scanning_inventory);
            this.shutting_down = false;
        });
    });
    let saved: Preferences = serde_json::from_slice(
        &std::fs::read(directory.path().join("preferences.json"))
            .expect("read private preference save"),
    )
    .expect("decode private preferences");
    assert_eq!(saved.accent, AccentColor::Sakura);

    search(cx, &fixture, "prerelease");
    cx.update_window(fixture.window, |_, window, cx| {
        window.click("app-update-cancel", cx);
        let this = fixture.workbench.read(cx);
        assert!(!this.app_update_navigation_locked());
        assert!(
            this.app_update.job.is_some(),
            "worker remains retained until join"
        );
        assert!(this.app_update.cancelling);
        window.click("nav-home", cx);
        assert!(fixture.workbench.read(cx).active_page == FeaturePage::Home);
        assert!(fixture.workbench.read(cx).app_update.job.is_some());
        window.click("nav-settings", cx);
        assert!(fixture.workbench.read(cx).active_page == FeaturePage::Settings);
    })
    .expect("navigate while the cancelled worker remains gated");
    release.send(()).expect("release cancelled download worker");
    wait_for_join(cx, &fixture);
    assert!(cancelled.load(Ordering::Acquire));
    assert_status(
        cx,
        &fixture,
        &crate::i18n::tr("app.settings.update.status_cancelled"),
    );
    let cleanups = cx.update(|cx| {
        fixture.workbench.update(cx, |this, cx| {
            assert!(this.app_update.job.is_none());
            assert!(this.flash.job.is_none());
            assert!(!this.scanning_inventory);
            assert!(!this.preferences_io.busy());
            this.app_update_cleanup_files(cx)
        })
    });
    cx.run_until_parked();
    for cleanup in cleanups {
        crate::application::lifecycle::join_background(cleanup);
    }
}

#[gpui_kit::test]
fn restart_requires_idle_work_and_keyboard_cancel_never_installs(cx: &mut TestAppContext) {
    let runtime = AppRuntime::new().expect("start app runtime");
    let directory = tempfile::tempdir().expect("private preferences directory");
    let _session = UiTestSession::new(cx);
    let fixture = fixture(cx, &runtime, &directory, Eligibility::Installable);
    open_updater(cx, &fixture);
    available(cx, &fixture);
    download_ready(cx, &fixture);
    settle(cx, fixture.window);
    search(cx, &fixture, "no matching settings entry");
    cx.update_window(fixture.window, |_, window, cx| {
        let status = window.find("app-update-status");
        assert!(status.visible());
        assert!(status.label().is_some_and(|label| label.contains("ready")));
        assert!(window.find("app-update-cancel").visible());
        assert!(fixture.workbench.read(cx).app_update_navigation_locked());
    })
    .expect("keep ready status and discard available through Settings search");
    search(cx, &fixture, "prerelease");

    // Exercise an actual retained firmware activity without touching a device.
    let activity = Job::start(|cancel, _| {
        while !cancel.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_millis(1));
        }
        Err("synthetic activity cancelled".into())
    })
    .expect("start isolated activity");
    cx.update(|cx| {
        fixture.workbench.update(cx, |this, cx| {
            this.flash.job = Some(activity);
            assert!(this.app_update_hardware_busy());
            cx.notify();
        });
    });
    settle(cx, fixture.window);
    click(cx, &fixture, "app-update-restart");
    cx.update_window(fixture.window, |_, window, cx| {
        let this = fixture.workbench.read(cx);
        assert!(matches!(this.app_update.status, Status::Busy));
        assert!(this.app_update.downloaded.is_some());
        assert!(this.app_update.job.is_none());
        assert!(
            window
                .find_all_by_role(gpui_kit::Role::AlertDialog)
                .is_empty()
        );
    })
    .expect("inspect activity-gated restart");
    assert_status(cx, &fixture, &crate::i18n::tr("app.settings.update.busy"));
    let activity = cx.update(|cx| {
        fixture.workbench.update(cx, |this, cx| {
            let activity = this.flash.job.take().expect("retained synthetic activity");
            cx.notify();
            activity
        })
    });
    activity.shutdown();
    settle(cx, fixture.window);

    cx.update_window(fixture.window, |_, window, cx| {
        window.click("settings-search", cx);
        assert_eq!(window.find("settings-search").focused(), Some(true));
        assert_eq!(window.find("app-update-restart").focused(), Some(false));
    })
    .expect("establish a different focus target before pointer activation");
    click(cx, &fixture, "app-update-restart");
    settle(cx, fixture.window);
    cx.update_window(fixture.window, |_, window, cx| {
        let dialogs = window.find_all_by_role(gpui_kit::Role::AlertDialog);
        assert_eq!(dialogs.len(), 1, "restart opens the real alert dialog");
        assert_eq!(
            dialogs[0].focused(),
            Some(true),
            "alert owns keyboard focus"
        );
        assert_eq!(
            window.find("cancel").label(),
            Some(crate::i18n::tr("app.settings.update.cancel").as_str())
        );
        assert_eq!(
            window.find("ok").label(),
            Some(crate::i18n::tr("app.settings.update.confirm_action").as_str())
        );
        window.press("escape", cx);
    })
    .expect("cancel focused restart alert by keyboard");
    settle(cx, fixture.window);
    cx.update_window(fixture.window, |_, window, cx| {
        assert!(
            window
                .find_all_by_role(gpui_kit::Role::AlertDialog)
                .is_empty()
        );
        assert_eq!(
            window.find("app-update-restart").focused(),
            Some(true),
            "focus returns to the trigger"
        );
        let this = fixture.workbench.read(cx);
        assert!(this.app_update.downloaded.is_some());
        assert!(this.app_update.job.is_none());
        assert!(!this.shutting_down);
    })
    .expect("cancelling alert retains the download without shutting down");

    // The busy check must also run when confirming an already-open dialog.
    click(cx, &fixture, "app-update-restart");
    settle(cx, fixture.window);
    cx.update_window(fixture.window, |_, window, _| {
        assert_eq!(
            window.find_all_by_role(gpui_kit::Role::AlertDialog).len(),
            1
        );
    })
    .expect("open the restart alert while idle");
    let activity = Job::start(|cancel, _| {
        while !cancel.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_millis(1));
        }
        Err("synthetic activity cancelled".into())
    })
    .expect("start activity while the alert is open");
    cx.update(|cx| {
        fixture.workbench.update(cx, |this, cx| {
            this.flash.job = Some(activity);
            cx.notify();
        });
    });
    cx.update_window(fixture.window, |_, window, cx| window.click("ok", cx))
        .expect("confirm the real restart alert");
    settle(cx, fixture.window);
    let downloaded_path = cx
        .update_window(fixture.window, |_, window, cx| {
            assert!(
                window
                    .find_all_by_role(gpui_kit::Role::AlertDialog)
                    .is_empty()
            );
            let this = fixture.workbench.read(cx);
            assert!(matches!(this.app_update.status, Status::Busy));
            assert!(!this.shutting_down);
            assert!(this.app_update_navigation_locked());
            assert!(this.flash.job.is_some());
            this.app_update
                .downloaded
                .as_ref()
                .expect("busy confirmation retains the downloaded update")
                .file
                .path()
                .to_path_buf()
        })
        .expect("inspect activity detected at confirmation");
    assert!(downloaded_path.exists());
    search(cx, &fixture, "no matching settings entry");
    assert_status(cx, &fixture, &crate::i18n::tr("app.settings.update.busy"));
    cx.update_window(fixture.window, |_, window, cx| {
        assert!(window.find("app-update-cancel").visible());
        window.click("app-update-cancel", cx);
        let this = fixture.workbench.read(cx);
        assert!(this.app_update.downloaded.is_none());
        assert!(!this.app_update_navigation_locked());
        assert!(this.flash.job.is_some());
        assert!(!this.shutting_down);
    })
    .expect("discard a ready download while other activity remains");
    let (activity, cleanups) = cx.update(|cx| {
        fixture.workbench.update(cx, |this, cx| {
            let activity = this.flash.job.take().expect("retained synthetic activity");
            (activity, this.app_update_cleanup_files(cx))
        })
    });
    activity.shutdown();
    cx.run_until_parked();
    for cleanup in cleanups {
        crate::application::lifecycle::join_background(cleanup);
    }
    assert!(
        !downloaded_path.exists(),
        "discard removes the downloaded file"
    );
    assert_no_workers(cx, &fixture);
}

#[gpui_kit::test]
fn queued_download_returns_to_about_and_closes_registry(cx: &mut TestAppContext) {
    let runtime = AppRuntime::new().expect("start app runtime");
    let directory = tempfile::tempdir().expect("private preferences directory");
    let _session = UiTestSession::new(cx);
    let fixture = fixture(cx, &runtime, &directory, Eligibility::Installable);
    open_updater(cx, &fixture);
    available(cx, &fixture);
    set_download(cx, &fixture, TestDownload::WaitForCancel);
    search(cx, &fixture, "language");
    cx.update_window(fixture.window, |_, window, cx| {
        window.click("settings-section-general", cx);
        assert_eq!(
            window.find("settings-section-general").checked(),
            Some(true)
        );
    })
    .expect("select a different Settings section");
    search(cx, &fixture, "no matching settings entry");

    // A queued download callback can arrive after the user opens the registry
    // and then leaves Settings.
    cx.update_window(fixture.window, |_, window, cx| {
        fixture.workbench.update(cx, |this, cx| {
            this.open_device_registry(window, cx);
            assert!(this.registry_is_open());
        });
    })
    .expect("open the registry before leaving Settings");
    click(cx, &fixture, "nav-home");
    cx.update_window(fixture.window, |_, window, cx| {
        fixture.workbench.update(cx, |this, cx| {
            assert!(this.active_page == FeaturePage::Home);
            assert!(this.registry_is_open());
            this.start_app_update_download(window, cx);
            assert!(this.active_page == FeaturePage::Settings);
            assert!(!this.registry_is_open());
            assert!(this.app_update_navigation_locked());
            assert!(this.app_update.job.is_some());
        });
    })
    .expect("start the queued download after leaving Settings");
    settle(cx, fixture.window);
    cx.update_window(fixture.window, |_, window, cx| {
        assert!(!fixture.workbench.read(cx).registry_is_open());
        assert_eq!(window.find("settings-section-about").checked(), Some(true));
        assert_eq!(window.find("settings-search").value(), Some(""));
        assert!(window.try_find("registry-back").is_none());
        assert!(window.find("app-update-status").visible());
        assert!(window.find("app-update-cancel").visible());
        window.click("nav-settings", cx);
        assert!(fixture.workbench.read(cx).page_focus.is_focused(window));
        window.press("tab", cx);
        assert_eq!(window.find("settings-search").focused(), Some(true));
        fixture
            .workbench
            .update(cx, |this, cx| this.open_device_registry(window, cx));
        assert_eq!(window.find("settings-search").focused(), Some(true));
        assert!(window.try_find("registry-back").is_none());
        window.click("app-update-cancel", cx);
        assert!(!fixture.workbench.read(cx).app_update_navigation_locked());
        assert!(!fixture.workbench.read(cx).registry_is_open());
        assert!(fixture.workbench.read(cx).app_update.job.is_some());
        assert!(window.find("app-update-status").visible());
    })
    .expect("keep the updater reachable after the queued download starts");
    settle(cx, fixture.window);
    cx.update_window(fixture.window, |_, window, cx| {
        assert!(window.try_find("registry-back").is_none());
        window.click("nav-home", cx);
        assert!(fixture.workbench.read(cx).active_page == FeaturePage::Home);
        window.click("nav-settings", cx);
        assert!(fixture.workbench.read(cx).active_page == FeaturePage::Settings);
        assert!(!fixture.workbench.read(cx).registry_is_open());
        assert!(window.try_find("registry-back").is_none());
    })
    .expect("navigate again without reopening the old registry");
    wait_for_join(cx, &fixture);
    cx.update(|cx| {
        assert!(matches!(
            fixture.workbench.read(cx).app_update.status,
            Status::Cancelled
        ));
    });
    assert_status(
        cx,
        &fixture,
        &crate::i18n::tr("app.settings.update.status_cancelled"),
    );
    cx.update_window(fixture.window, |_, window, _| {
        assert!(window.find("app-update-status").visible());
    })
    .expect("cancelled status remains visible after navigation unlocks");

    set_download(
        cx,
        &fixture,
        TestDownload::Failure("synthetic queued failure".into()),
    );
    search(cx, &fixture, "another unmatched setting");
    click(cx, &fixture, "nav-home");
    cx.update_window(fixture.window, |_, window, cx| {
        fixture.workbench.update(cx, |this, cx| {
            assert!(this.active_page == FeaturePage::Home);
            this.start_app_update_download(window, cx);
            assert!(this.app_update_navigation_locked());
        });
    })
    .expect("start a queued failing download from Home");
    wait_for_join(cx, &fixture);
    cx.update_window(fixture.window, |_, window, cx| {
        let this = fixture.workbench.read(cx);
        assert!(this.active_page == FeaturePage::Settings);
        assert!(!this.app_update_navigation_locked());
        assert_eq!(window.find("settings-search").value(), Some(""));
        let status = window.find("app-update-status");
        assert!(status.visible());
        assert!(
            status
                .label()
                .is_some_and(|label| label.contains("synthetic queued failure"))
        );
        window.click("nav-home", cx);
        assert!(fixture.workbench.read(cx).active_page == FeaturePage::Home);
    })
    .expect("show a fast failure after clearing an unrelated search");
    assert_no_workers(cx, &fixture);
}

#[gpui_kit::test]
fn ordinary_updater_cleanup_discards_ready_download_without_install_intent(
    cx: &mut TestAppContext,
) {
    let runtime = AppRuntime::new().expect("start app runtime");
    let directory = tempfile::tempdir().expect("private preferences directory");
    let _session = UiTestSession::new(cx);
    let fixture = fixture(cx, &runtime, &directory, Eligibility::Installable);
    open_updater(cx, &fixture);
    available(cx, &fixture);
    download_ready(cx, &fixture);
    poll_until(cx, &fixture, |this| !this.preferences_io.busy());
    let (path, cleanups) = cx.update(|cx| {
        fixture.workbench.update(cx, |this, cx| {
            let path = this
                .app_update
                .downloaded
                .as_ref()
                .expect("downloaded update is ready")
                .file
                .path()
                .to_path_buf();
            assert!(path.exists());
            let cleanups = this.app_update_cleanup_files(cx);
            assert!(this.app_update.downloaded.is_none());
            #[cfg(windows)]
            {
                let (job, restart) = this.shutdown_app_update();
                assert!(job.is_none());
                assert!(
                    restart.is_none(),
                    "ordinary quit must not install the download"
                );
            }
            #[cfg(not(windows))]
            assert!(this.shutdown_app_update().is_none());
            (path, cleanups)
        })
    });
    cx.run_until_parked();
    for cleanup in cleanups {
        crate::application::lifecycle::join_background(cleanup);
    }
    assert!(
        !path.exists(),
        "ordinary updater cleanup removes the downloaded file"
    );
}

#[gpui_kit::test]
fn narrow_zoom_keyboard_reveals_individual_updater_controls(cx: &mut TestAppContext) {
    let runtime = AppRuntime::new().expect("start app runtime");
    let directory = tempfile::tempdir().expect("private preferences directory");
    let _session = UiTestSession::new(cx);
    let fixture = fixture(cx, &runtime, &directory, Eligibility::Installable);
    open_updater(cx, &fixture);
    cx.simulate_window_resize(fixture.window, size(px(760.), px(360.)));
    cx.simulate_window_scale_factor_change(fixture.window, 1.25);
    cx.update(|cx| gpui_kit::component::Theme::update(cx, |theme| theme.font_size = px(20.)));
    settle(cx, fixture.window);
    // Start at the search field and navigate through real tab stops, not a synthetic focus.
    cx.update_window(fixture.window, |_, window, cx| {
        window.click("settings-search", cx)
    })
    .expect("focus Settings search");
    for target in ["app-update-prereleases", "app-update-check"] {
        let mut reached = false;
        for _ in 0..40 {
            if cx
                .update_window(fixture.window, |_, window, _| {
                    window.find(target).focused() == Some(true)
                })
                .expect("inspect tab focus")
            {
                reached = true;
                break;
            }
            cx.update_window(fixture.window, |_, window, cx| window.press("tab", cx))
                .expect("navigate Settings by keyboard");
            settle(cx, fixture.window);
        }
        assert!(reached, "{target} must be reachable by Tab");
        cx.update_window(fixture.window, |_, window, _| {
            let control = window.find(target);
            assert_eq!(control.focused(), Some(true));
            assert!(
                control.visible(),
                "{target} must be revealed by its own focus"
            );
            let bounds = control.bounds();
            assert!(bounds.top() >= px(0.) && bounds.bottom() <= window.viewport_size().height);
            assert!(bounds.left() >= px(0.) && bounds.right() <= window.viewport_size().width);
        })
        .expect("inspect revealed control");
        if target == "app-update-prereleases" {
            cx.update_window(fixture.window, |_, window, cx| {
                window.press("space", cx);
                assert_eq!(window.find(target).checked(), Some(true));
                assert!(
                    fixture
                        .workbench
                        .read(cx)
                        .preferences
                        .app_update_include_prereleases
                );
            })
            .expect("toggle revealed checkbox");
        }
    }
    set_check(cx, &fixture, TestCheck::Result(CheckResult::UpToDate));
    cx.update_window(fixture.window, |_, window, cx| window.press("enter", cx))
        .expect("activate revealed check button");
    wait_for_join(cx, &fixture);
    assert_status(
        cx,
        &fixture,
        &crate::i18n::tr("app.settings.update.status_current"),
    );
    assert_no_workers(cx, &fixture);
}
