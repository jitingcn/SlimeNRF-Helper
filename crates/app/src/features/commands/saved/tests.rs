use super::{CommandLibrary, HomeView, SavedCommandView};
use crate::{
    app_runtime::AppRuntime,
    application::{
        FeaturePage, Workbench, preference_io::PreferenceIo, test_support::offline_workbench,
    },
    features::commands::{
        model::{CommandCardIcon, SavedCommandDraft, SavedCommandId, TargetMode, ValidationReason},
        store::{ShortcutEdit, ShortcutStore},
    },
    test_support::UiTestSession,
};
use futures_util::FutureExt as _;
use gpui_kit::{
    AnyWindowHandle, AppContext as _, Bounds, Entity, TestAppContext, WindowBounds, WindowOptions,
    px, size, test::TestWindowExt as _,
};
use std::{fs, path::Path};

struct Fixture {
    window: AnyWindowHandle,
    workbench: Entity<Workbench>,
}

fn draft(title: &str) -> SavedCommandDraft {
    SavedCommandDraft {
        id: None,
        title: title.into(),
        description: "Private test command".into(),
        commands_text: "status\ninfo".into(),
        icon: Some(CommandCardIcon::Terminal),
        pinned: true,
        target_mode: TargetMode::ReceiverConsoles,
    }
}

fn seed_store(path: &Path) -> (ShortcutStore, SavedCommandId, SavedCommandId) {
    let mut store = ShortcutStore::load(path.to_path_buf()).expect("load private shortcut store");
    let first = store
        .apply(ShortcutEdit::Save(draft("First")))
        .expect("persist first private command")
        .expect("saving returns the first id");
    let second = store
        .apply(ShortcutEdit::Save(draft("Second")))
        .expect("persist second private command")
        .expect("saving returns the second id");
    (store, first, second)
}

fn fixture(
    cx: &mut TestAppContext,
    runtime: &AppRuntime,
    directory: &tempfile::TempDir,
    store: Option<ShortcutStore>,
) -> Fixture {
    let executor = runtime.executor();
    let preferences_path = directory.path().join("preferences.json");
    let store_path = directory.path().join("custom-shortcuts.json");
    cx.update(|cx| cx.set_reduce_motion(true));
    let (window, workbench) = cx.update(|cx| {
        gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    Default::default(),
                    size(px(1400.), px(1600.)),
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
                    this.preferences_io = PreferenceIo::for_test(preferences_path);
                    this.saved_commands.library = CommandLibrary::for_test(store_path, store);
                    this.open_saved_commands(window, cx);
                    this
                })
            },
        )
        .expect("open isolated saved-command window")
    });
    cx.update_window(window, |_, window, _| window.activate_window())
        .expect("activate saved-command window");
    settle(cx, window);
    Fixture { window, workbench }
}

fn settle(cx: &mut TestAppContext, window: AnyWindowHandle) {
    for _ in 0..2 {
        cx.run_until_parked();
        cx.update_window(window, |_, window, cx| {
            window.simulate_next_frame(cx);
            window.render_frame(cx);
        })
        .expect("settle saved-command frame");
    }
}

fn assert_no_store_job(this: &Workbench) {
    assert!(
        this.saved_commands
            .library
            .retained_join_for_test()
            .is_none(),
        "no store worker join"
    );
    assert!(!this.saved_commands_busy());
}

#[gpui_kit::test]
fn saved_store_owns_worker_until_foreground_completion(cx: &mut TestAppContext) {
    let runtime = AppRuntime::new().expect("start app runtime");
    let directory = tempfile::tempdir().expect("private shortcut directory");
    let path = directory.path().join("custom-shortcuts.json");
    let (store, first, second) = seed_store(&path);
    let original = fs::read(&path).expect("read original private store");
    let _session = UiTestSession::new(cx);
    let fixture = fixture(cx, &runtime, &directory, Some(store));

    let join = cx.update(|cx| {
        fixture.workbench.update(cx, |this, cx| {
            this.apply_saved_edit(
                ShortcutEdit::SetPinned {
                    id: first,
                    pinned: false,
                },
                cx,
            );
            assert!(this.saved_commands_busy());
            this.saved_commands
                .library
                .retained_join_for_test()
                .expect("accepted store worker is retained")
        })
    });
    assert!(join.clone().now_or_never().is_none());
    assert_eq!(fs::read(&path).expect("read gated store"), original);

    // Advance background work only: the real store write completes while the
    // foreground completion remains queued and still owns the worker join.
    for _ in 0..256 {
        if join.clone().now_or_never().is_some() {
            break;
        }
        assert!(cx.dispatcher.tick(true), "store worker must be queued");
    }
    assert_eq!(join.now_or_never(), Some(()), "store worker must finish");
    let written = ShortcutStore::load(path.clone())
        .expect("reload completed private write")
        .snapshot();
    assert!(!written.get(first).expect("first command").pinned);
    assert!(written.get(second).expect("second command").pinned);
    cx.update(|cx| {
        fixture.workbench.update(cx, |this, _| {
            assert!(
                this.saved_commands_busy(),
                "completion has not joined the worker"
            );
            assert!(this.saved_commands.library.card(first).unwrap().pinned);
        });
    });

    settle(cx, fixture.window);
    cx.update(|cx| {
        fixture.workbench.update(cx, |this, _| {
            assert_no_store_job(this);
            assert_eq!(
                this.saved_commands.library.snapshot(),
                Some(written.as_ref())
            );
        });
    });
}

#[gpui_kit::test]
fn shutdown_joins_accepted_store_write_and_rejects_later_edits(cx: &mut TestAppContext) {
    let runtime = AppRuntime::new().expect("start app runtime");
    let directory = tempfile::tempdir().expect("private shortcut directory");
    let path = directory.path().join("custom-shortcuts.json");
    let (store, first, second) = seed_store(&path);
    let original = fs::read(&path).expect("read original private store");
    let _session = UiTestSession::new(cx);
    let fixture = fixture(cx, &runtime, &directory, Some(store));

    let (worker, cleanup) = cx.update(|cx| {
        fixture.workbench.update(cx, |this, cx| {
            this.shutting_down = true;
            this.initialize_saved_commands(cx);
            this.apply_saved_edit(
                ShortcutEdit::SetPinned {
                    id: first,
                    pinned: false,
                },
                cx,
            );
            assert_no_store_job(this);
            this.shutting_down = false;

            this.apply_saved_edit(
                ShortcutEdit::SetPinned {
                    id: first,
                    pinned: false,
                },
                cx,
            );
            let worker = this
                .saved_commands
                .library
                .retained_join_for_test()
                .expect("accepted write is retained");
            let cleanup = this
                .saved_commands_shutdown(cx)
                .expect("shutdown joins the accepted write")
                .shared();
            assert_no_store_job(this);

            this.initialize_saved_commands(cx);
            this.apply_saved_edit(
                ShortcutEdit::SetPinned {
                    id: second,
                    pinned: false,
                },
                cx,
            );
            assert_no_store_job(this);
            (worker, cleanup)
        })
    });
    assert!(worker.clone().now_or_never().is_none());
    assert!(cleanup.clone().now_or_never().is_none());
    assert_eq!(
        fs::read(&path).expect("read queued private store"),
        original
    );

    for _ in 0..256 {
        if cleanup.clone().now_or_never().is_some() {
            break;
        }
        assert!(cx.dispatcher.tick(true), "store cleanup must be queued");
    }
    assert_eq!(cleanup.now_or_never(), Some(()));
    assert_eq!(worker.now_or_never(), Some(()));
    let written = ShortcutStore::load(path)
        .expect("reload accepted write")
        .snapshot();
    assert!(!written.get(first).expect("first command").pinned);
    assert!(written.get(second).expect("second command").pinned);
}

#[gpui_kit::test]
fn home_controls_navigate_and_focus_the_accepted_destination(cx: &mut TestAppContext) {
    let runtime = AppRuntime::new().expect("start app runtime");
    let directory = tempfile::tempdir().expect("private shortcut directory");
    let path = directory.path().join("custom-shortcuts.json");
    let (store, first, _) = seed_store(&path);
    let _session = UiTestSession::new(cx);
    let fixture = fixture(cx, &runtime, &directory, Some(store));

    cx.update_window(fixture.window, |_, window, cx| {
        window.click("saved-back-home", cx);
        let this = fixture.workbench.read(cx);
        assert!(matches!(this.active_page, FeaturePage::Home));
        assert!(matches!(this.home.view(), HomeView::Tasks));
        assert!(this.home_page_focus.is_focused(window));
    })
    .expect("return to Home");
    settle(cx, fixture.window);
    cx.update_window(fixture.window, |_, window, cx| {
        window.click("home-devices", cx);
        let this = fixture.workbench.read(cx);
        assert!(matches!(this.active_page, FeaturePage::DeviceSetup));
        assert!(this.page_focus.is_focused(window));
    })
    .expect("open devices from Home");
    settle(cx, fixture.window);
    cx.update_window(fixture.window, |_, window, cx| {
        window.click("nav-home", cx);
    })
    .expect("return to Home navigation");
    settle(cx, fixture.window);
    cx.update_window(fixture.window, |_, window, cx| {
        window.click(format!("saved-home:{}", first.get()), cx);
        let this = fixture.workbench.read(cx);
        assert!(matches!(this.home.view(), HomeView::Commands));
        assert!(matches!(
            this.saved_commands.view,
            SavedCommandView::Targets(_)
        ));
        assert!(this.home_page_focus.is_focused(window));
    })
    .expect("open the pinned command target picker");
    settle(cx, fixture.window);
    cx.update_window(fixture.window, |_, window, cx| {
        window.click("saved-targets-devices", cx);
        let this = fixture.workbench.read(cx);
        assert!(matches!(this.active_page, FeaturePage::DeviceSetup));
        assert!(this.page_focus.is_focused(window));
    })
    .expect("open devices from the target picker");
}

#[gpui_kit::test]
fn reload_recovers_the_private_store(cx: &mut TestAppContext) {
    let runtime = AppRuntime::new().expect("start app runtime");
    let directory = tempfile::tempdir().expect("private shortcut directory");
    let path = directory.path().join("custom-shortcuts.json");
    let (store, first, _) = seed_store(&path);
    let expected = store.snapshot();
    let original = fs::read(&path).expect("read original private store");
    drop(store);
    let _session = UiTestSession::new(cx);
    let fixture = fixture(cx, &runtime, &directory, None);

    fs::write(&path, b"invalid private store").expect("inject a recoverable load failure");
    cx.update(|cx| {
        fixture
            .workbench
            .update(cx, |this, cx| this.initialize_saved_commands(cx));
    });
    settle(cx, fixture.window);
    cx.update(|cx| {
        let this = fixture.workbench.read(cx);
        assert!(this.saved_commands.library.error().is_some());
        assert!(!this.saved_commands.library.has_store());
    });
    fs::write(&path, &original).expect("restore the valid private store");
    settle(cx, fixture.window);
    cx.update_window(fixture.window, |_, window, cx| {
        window.click("saved-reload", cx);
        assert!(
            fixture.workbench.read(cx).saved_commands_busy(),
            "clicking reload must start loading the store"
        );
    })
    .expect("reload private store");
    settle(cx, fixture.window);
    cx.update(|cx| {
        let this = fixture.workbench.read(cx);
        assert_no_store_job(this);
        assert!(this.saved_commands.library.error().is_none());
        assert_eq!(
            this.saved_commands.library.snapshot(),
            Some(expected.as_ref())
        );
        assert_eq!(
            this.saved_commands.library.card(first).unwrap().title,
            "First"
        );
    });
    assert_eq!(
        fs::read(&path).expect("read reloaded private store"),
        original
    );
    assert!(!directory.path().join("preferences.json").exists());
}

#[gpui_kit::test]
fn library_delete_cancellation_and_pin_persist_through_controls(cx: &mut TestAppContext) {
    let runtime = AppRuntime::new().expect("start app runtime");
    let directory = tempfile::tempdir().expect("private shortcut directory");
    let path = directory.path().join("custom-shortcuts.json");
    let (store, first, second) = seed_store(&path);
    let _session = UiTestSession::new(cx);
    let fixture = fixture(cx, &runtime, &directory, Some(store));
    cx.update_window(fixture.window, |_, window, cx| {
        window.click(format!("saved:{}:delete", first.get()), cx);
        assert_eq!(
            fixture.workbench.read(cx).saved_commands.delete,
            Some(first)
        );
    })
    .expect("open deletion confirmation");
    settle(cx, fixture.window);
    cx.update_window(fixture.window, |_, window, cx| {
        window.click(format!("saved:{}:cancel-delete", first.get()), cx);
        assert!(fixture.workbench.read(cx).saved_commands.delete.is_none());
        window.click(format!("saved:{}:pin", first.get()), cx);
        assert!(fixture.workbench.read(cx).saved_commands_busy());
    })
    .expect("unpin through the real control");
    settle(cx, fixture.window);
    let persisted = ShortcutStore::load(path)
        .expect("reload persisted unpin")
        .snapshot();
    assert!(!persisted.get(first).expect("first command remains").pinned);
    assert!(
        persisted
            .get(second)
            .expect("second command remains")
            .pinned
    );
    cx.update(|cx| {
        let this = fixture.workbench.read(cx);
        assert_no_store_job(this);
        assert_eq!(
            this.saved_commands.library.snapshot(),
            Some(persisted.as_ref())
        );
    });
    assert!(!directory.path().join("preferences.json").exists());
}

#[gpui_kit::test]
fn editor_saves_through_real_controls(cx: &mut TestAppContext) {
    let runtime = AppRuntime::new().expect("start app runtime");
    let directory = tempfile::tempdir().expect("private shortcut directory");
    let path = directory.path().join("custom-shortcuts.json");
    let (store, first, second) = seed_store(&path);
    let _session = UiTestSession::new(cx);
    let fixture = fixture(cx, &runtime, &directory, Some(store));
    cx.update_window(fixture.window, |_, window, cx| {
        window.click(format!("saved:{}:edit", first.get()), cx);
        assert!(matches!(
            fixture.workbench.read(cx).saved_commands.view,
            SavedCommandView::Editor(_)
        ));
    })
    .expect("open the real editor");
    settle(cx, fixture.window);
    let title = cx.update(|cx| {
        let this = fixture.workbench.read(cx);
        let SavedCommandView::Editor(editor) = &this.saved_commands.view else {
            panic!("the first command must be in the editor");
        };
        editor.title().entity_id()
    });
    cx.update_window(fixture.window, |_, window, cx| {
        window.click(("input", title), cx);
        window.press("secondary-a", cx);
        window.input("Renamed command", cx);
        assert_eq!(
            window.find(("input", title)).value(),
            Some("Renamed command")
        );
        window.click("saved-save", cx);
        assert!(fixture.workbench.read(cx).saved_commands_busy());
    })
    .expect("edit and save through the real controls");
    settle(cx, fixture.window);
    let persisted = ShortcutStore::load(path)
        .expect("reload saved edit")
        .snapshot();
    let saved = persisted.get(first).expect("edited command remains");
    assert_eq!(saved.title, "Renamed command");
    assert_eq!(saved.commands_text(), "status\ninfo");
    assert!(saved.pinned);
    assert_eq!(
        persisted.get(second).expect("second command remains").title,
        "Second"
    );
    cx.update(|cx| {
        let this = fixture.workbench.read(cx);
        assert_no_store_job(this);
        assert!(matches!(
            this.saved_commands.view,
            SavedCommandView::Library
        ));
        assert_eq!(
            this.saved_commands.library.snapshot(),
            Some(persisted.as_ref())
        );
    });
    assert!(!directory.path().join("preferences.json").exists());
}

#[gpui_kit::test]
fn initial_store_load_is_rejected_during_shutdown(cx: &mut TestAppContext) {
    let runtime = AppRuntime::new().expect("start app runtime");
    let directory = tempfile::tempdir().expect("private shortcut directory");
    let path = directory.path().join("custom-shortcuts.json");
    let (store, _, _) = seed_store(&path);
    let expected = store.snapshot();
    drop(store);
    let original = fs::read(&path).expect("read original private store");
    let _session = UiTestSession::new(cx);
    let fixture = fixture(cx, &runtime, &directory, None);

    cx.update(|cx| {
        fixture.workbench.update(cx, |this, cx| {
            this.shutting_down = true;
            this.initialize_saved_commands(cx);
            assert_no_store_job(this);
            assert!(this.saved_commands.library.snapshot().is_none());
            this.shutting_down = false;
            this.initialize_saved_commands(cx);
            assert!(
                this.saved_commands_busy(),
                "rejected load did not consume initialization"
            );
        });
    });
    settle(cx, fixture.window);
    cx.update(|cx| {
        let this = fixture.workbench.read(cx);
        assert_no_store_job(this);
        assert_eq!(
            this.saved_commands.library.snapshot(),
            Some(expected.as_ref())
        );
    });
    assert_eq!(
        fs::read(&path).expect("read loaded private store"),
        original
    );
    assert!(!directory.path().join("preferences.json").exists());
}

#[gpui_kit::test]
fn editor_validation_and_locale_navigation_preserve_draft_and_identity(cx: &mut TestAppContext) {
    let runtime = AppRuntime::new().expect("start app runtime");
    let directory = tempfile::tempdir().expect("private shortcut directory");
    let path = directory.path().join("custom-shortcuts.json");
    let (store, first, second) = seed_store(&path);
    let expected = store.snapshot();
    let original = fs::read(&path).expect("read original private store");
    let _session = UiTestSession::new(cx);
    let fixture = fixture(cx, &runtime, &directory, Some(store));

    cx.update_window(fixture.window, |_, window, cx| {
        window.click(format!("saved:{}:edit", first.get()), cx);
    })
    .expect("edit the first command");
    settle(cx, fixture.window);
    let ids = cx.update(|cx| {
        let this = fixture.workbench.read(cx);
        let editor = this.saved_commands.editor().expect("first command editor");
        (
            editor.title().entity_id(),
            editor.description().entity_id(),
            editor.commands().entity_id(),
        )
    });
    cx.update_window(fixture.window, |_, window, cx| {
        window.click(("input", ids.0), cx);
        window.press("secondary-a", cx);
        window.press("backspace", cx);
        window.click("saved-save", cx);
        assert!(fixture.workbench.read(cx).saved_commands_busy());
    })
    .expect("submit the invalid title through the editor");
    settle(cx, fixture.window);
    cx.update(|cx| {
        let this = fixture.workbench.read(cx);
        assert_no_store_job(this);
        let editor = this
            .saved_commands
            .editor()
            .expect("validation retains the editor");
        assert_eq!(
            editor.validation().expect("structured validation").reason,
            ValidationReason::TitleRequired
        );
        let draft = editor.draft(cx);
        assert_eq!(draft.id, Some(first));
        assert_eq!(draft.title, "");
        assert_eq!(draft.commands_text, "status\ninfo");
        assert_eq!(
            this.saved_commands.library.snapshot(),
            Some(expected.as_ref())
        );
        assert!(this.saved_commands.library.error().is_none());
    });
    assert_eq!(
        fs::read(&path).expect("read rejected private edit"),
        original
    );

    rust_i18n::set_locale("zh-CN");
    cx.update(|cx| fixture.workbench.update(cx, |_, cx| cx.notify()));
    settle(cx, fixture.window);
    cx.update_window(fixture.window, |_, window, cx| {
        assert_eq!(
            window.find("saved-save").label(),
            Some(crate::i18n::tr("shortcuts.action.save").as_str())
        );
        window.click("saved-back-home", cx);
        window.click("home-manage-commands", cx);
        let this = fixture.workbench.read(cx);
        let editor = this
            .saved_commands
            .editor()
            .expect("Home composes the retained editor");
        assert_eq!(
            (
                editor.title().entity_id(),
                editor.description().entity_id(),
                editor.commands().entity_id()
            ),
            ids
        );
        assert_eq!(editor.draft(cx).id, Some(first));
        assert_eq!(editor.draft(cx).title, "");
        assert_eq!(
            editor
                .validation()
                .expect("navigation retains validation")
                .reason,
            ValidationReason::TitleRequired
        );
        window.click(("input", ids.0), cx);
        window.input("保留的命令", cx);
        assert_eq!(window.find(("input", ids.0)).value(), Some("保留的命令"));
    })
    .expect("retain the draft across locale and Home navigation, then edit");
    // Input text changes immediately, but the owner's Change subscription runs
    // when the outer window update flushes GPUI effects.
    cx.update(|cx| {
        let this = fixture.workbench.read(cx);
        let editor = this
            .saved_commands
            .editor()
            .expect("retain the edited draft");
        assert!(
            editor.validation().is_none(),
            "the delivered input Change event clears the owner's validation"
        );
        let draft = editor.draft(cx);
        assert_eq!(draft.id, Some(first));
        assert_eq!(draft.title, "保留的命令");
        assert_eq!(editor.title().entity_id(), ids.0);
    });
    cx.update_window(fixture.window, |_, window, cx| {
        window.click("saved-save", cx);
        assert!(fixture.workbench.read(cx).saved_commands_busy());
    })
    .expect("save the corrected draft after its Change event is delivered");
    settle(cx, fixture.window);
    let persisted = ShortcutStore::load(path)
        .expect("reload corrected command")
        .snapshot();
    assert_eq!(
        persisted.cards().len(),
        2,
        "editing must not allocate a replacement id"
    );
    assert_eq!(
        persisted.get(first).expect("original id survives").title,
        "保留的命令"
    );
    assert_eq!(
        persisted.get(second).expect("other command survives").title,
        "Second"
    );
    assert!(!directory.path().join("preferences.json").exists());
}

#[gpui_kit::test]
fn persistence_failure_retains_editor_and_snapshot_until_retry(cx: &mut TestAppContext) {
    let runtime = AppRuntime::new().expect("start app runtime");
    let directory = tempfile::tempdir().expect("private shortcut directory");
    let path = directory.path().join("custom-shortcuts.json");
    let (store, first, _) = seed_store(&path);
    let expected = store.snapshot();
    let original = fs::read(&path).expect("read original private store");
    let _session = UiTestSession::new(cx);
    let fixture = fixture(cx, &runtime, &directory, Some(store));

    cx.update_window(fixture.window, |_, window, cx| {
        window.click(format!("saved:{}:edit", first.get()), cx);
    })
    .expect("edit the first command");
    settle(cx, fixture.window);
    let title = cx.update(|cx| {
        fixture
            .workbench
            .read(cx)
            .saved_commands
            .editor()
            .unwrap()
            .title()
            .entity_id()
    });
    let damaged = b"{private damaged store";
    fs::write(&path, damaged).expect("inject an external persistence failure");
    cx.update_window(fixture.window, |_, window, cx| {
        window.click(("input", title), cx);
        window.press("secondary-a", cx);
        window.input("Unsaved command", cx);
        window.click("saved-save", cx);
    })
    .expect("submit the draft while the private store is damaged");
    settle(cx, fixture.window);
    cx.update(|cx| {
        let this = fixture.workbench.read(cx);
        assert_no_store_job(this);
        let editor = this
            .saved_commands
            .editor()
            .expect("persistence failure retains editor");
        assert_eq!(editor.title().entity_id(), title);
        assert_eq!(editor.draft(cx).id, Some(first));
        assert_eq!(editor.draft(cx).title, "Unsaved command");
        assert!(editor.validation().is_none());
        assert!(this.saved_commands.library.error().is_some());
        assert_eq!(
            this.saved_commands.library.snapshot(),
            Some(expected.as_ref())
        );
    });
    assert_eq!(
        fs::read(&path).expect("damaged store is not overwritten"),
        damaged
    );

    fs::write(&path, original).expect("repair the private store");
    cx.update_window(fixture.window, |_, window, cx| {
        window.click("saved-save", cx);
        assert!(fixture.workbench.read(cx).saved_commands_busy());
    })
    .expect("retry the retained editor draft");
    settle(cx, fixture.window);
    let persisted = ShortcutStore::load(path)
        .expect("reload retried command")
        .snapshot();
    assert_eq!(persisted.cards().len(), 2);
    assert_eq!(
        persisted
            .get(first)
            .expect("edited id survives retry")
            .title,
        "Unsaved command"
    );
    cx.update(|cx| {
        let this = fixture.workbench.read(cx);
        assert_no_store_job(this);
        assert!(matches!(
            this.saved_commands.view,
            SavedCommandView::Library
        ));
        assert!(this.saved_commands.library.error().is_none());
        assert_eq!(
            this.saved_commands.library.snapshot(),
            Some(persisted.as_ref())
        );
    });
    assert!(!directory.path().join("preferences.json").exists());
}
