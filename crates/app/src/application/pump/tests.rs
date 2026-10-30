use crate::{
    app_runtime::AppRuntime,
    application::{FeaturePage, test_support::offline_workbench},
    features::replay::test_support::hold_computed_result,
    i18n::tr,
    sessions::SessionManager,
    test_support::UiTestSession,
};
use gpui_kit::{AppContext as _, TestAppContext, VisualContext as _, test::TestWindowExt as _};
use std::{cell::Cell, rc::Rc};

#[gpui_kit::test]
fn replay_progress_notifies_its_feature_without_invalidating_the_workbench(
    cx: &mut TestAppContext,
) {
    let _ui_session = UiTestSession::new(cx);
    let runtime = AppRuntime::new().expect("start private application runtime");
    let executor = runtime.executor();
    let cx = cx.add_empty_window();
    let workbench = cx.new_window_entity(|window, cx| {
        let mut workbench = offline_workbench(window, cx, SessionManager::new(), executor);
        workbench.active_page = FeaturePage::VqfReplay;
        // This fixture observes only replay work, not native inventory enumeration.
        workbench.scanning_inventory = true;
        workbench
    });
    let replay = cx.update(|_, cx| workbench.read(cx).replay.clone());
    let mut work = cx.update(|window, cx| {
        replay.update(cx, |replay, cx| hold_computed_result(replay, window, cx))
    });
    let expected_progress = format!(
        "{} · {:.0}%",
        tr("replay.phase.computing"),
        work.progress() as f32 / 10.,
    );
    let feature_notifications = Rc::new(Cell::new(0));
    let root_notifications = Rc::new(Cell::new(0));
    let observed_feature = feature_notifications.clone();
    let observed_root = root_notifications.clone();
    let _observer = cx.new(|cx| {
        cx.observe(&replay, move |_, _, _| {
            observed_feature.set(observed_feature.get() + 1);
        })
        .detach();
        cx.observe(&workbench, move |_, _, _| {
            observed_root.set(observed_root.get() + 1);
        })
        .detach();
    });
    cx.replace_root_view(|window, cx| gpui_kit::base::Root::new(workbench.clone(), window, cx));
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find("replay-progress").label(),
            Some(format!("{} · 0%", tr("replay.phase.computing")).as_str()),
        );
    });
    feature_notifications.set(0);
    root_notifications.set(0);

    cx.update(|window, cx| {
        let changed = workbench.update(cx, |workbench, cx| {
            let changed = workbench.advance_background_work(window, cx);
            if changed {
                cx.notify();
            }
            changed
        });
        assert!(
            !changed,
            "replay progress is not a device/application change"
        );
    });
    cx.run_until_parked();
    assert_eq!(feature_notifications.get(), 1);
    assert_eq!(root_notifications.get(), 0);
    cx.update(|window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find("replay-progress").label(),
            Some(expected_progress.as_str())
        );
        assert!(!workbench.update(cx, |workbench, cx| {
            workbench.advance_background_work(window, cx)
        }));
    });
    cx.run_until_parked();
    assert_eq!(
        feature_notifications.get(),
        1,
        "unchanged progress does not redraw"
    );
    assert_eq!(
        root_notifications.get(),
        0,
        "the shell does not re-project registry state"
    );

    work.release();
    let cleanup = cx.update(|_, cx| replay.update(cx, |replay, cx| replay.shutdown(cx)));
    for worker in cleanup.workers {
        worker
            .join()
            .expect("join the real completed replay worker");
    }
    for file in cleanup.files {
        super::super::lifecycle::join_background(file);
    }
}
