use super::controller::replay_edit_due;
use super::diagnostics::replay_magnetic_status;
use super::parameters::{parameter_values, set_parameter_values};
use super::render::replay_diagnostics_width_correction;
use super::scrubbing::{replay_scrub_time, viewport_range};
use super::{ReplayUiState, Request, Worker};
use gpui_kit::px;
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    thread,
};

#[gpui_kit::test]
fn viewport_button_space_does_not_trigger_page_playback(cx: &mut gpui_kit::TestAppContext) {
    let _ui_session = crate::ui::test_support::UiTestSession::new(cx);
    use super::{ReplayViewData, Workbench};
    use gpui_kit::test::{TestEventExt as _, TestWindowExt as _};
    use gpui_kit::{
        AnyElement, App, AppContext as _, Bounds, Context, Entity, IntoElement, Render, Window,
        WindowBounds, WindowOptions, div, prelude::*, size,
    };
    use slimenrf_recording::{
        capture::{Recording, Sample, SensorMetadata, Track},
        replay::{ReplayOptions, process},
    };

    struct ReplayPageFixture {
        workbench: Entity<Workbench>,
    }

    impl Render for ReplayPageFixture {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            self.workbench.update(cx, |this, cx| {
                let page: AnyElement = this.replay_page(window, cx);
                div()
                    .size_full()
                    .track_focus(&this.page_focus)
                    .tab_group()
                    .flex()
                    .flex_col()
                    .child(page)
            })
        }
    }

    fn focus_button(id: &'static str, window: &mut Window, cx: &mut App) {
        for _ in 0..64 {
            if window.find(id).focused() == Some(true) {
                return;
            }
            window.press("tab", cx);
        }
        panic!("Button {id} was not reachable by Tab");
    }

    let recording = Arc::new(Recording {
        tracks: vec![Track {
            metadata: SensorMetadata {
                raw_tx_hz: Some(100.),
                accel_hz: Some(100.),
                ..Default::default()
            },
            samples: (0..=100)
                .map(|sequence| Sample {
                    sequence,
                    acceleration_g: [0., 0., 1.],
                    acceleration_fresh: true,
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }],
        ..Default::default()
    });
    let processed = process(
        recording.clone(),
        0,
        ReplayOptions::default(),
        &AtomicBool::new(false),
        &AtomicUsize::new(0),
    )
    .unwrap();
    let processed =
        Arc::new(ReplayViewData::prepare_range(processed, None, &AtomicBool::new(false)).unwrap());
    let full_range = processed.range;
    assert!(full_range.1 > full_range.0);
    let runtime = crate::app_runtime::AppRuntime::new().unwrap();
    let executor = runtime.executor();

    let (handle, fixture) = cx.update(|cx| {
        gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    Default::default(),
                    size(px(1100.), px(760.)),
                ))),
                ..Default::default()
            },
            cx,
            |window, cx| {
                let workbench = cx.new(|cx| {
                    let mut this = crate::ui::test_support::offline_workbench(
                        window,
                        cx,
                        crate::sessions::SessionManager::new(),
                        executor,
                    );
                    this.active_page = super::FeaturePage::VqfReplay;
                    // Keep this interaction fixture independent of the user's preset storage.
                    assert!(this.shutdown_replay_presets().is_empty());
                    this.replay.recording = Some(recording);
                    this.replay.options = processed.result.options.clone();
                    this.replay.requested_range = Some(full_range);
                    this.replay.processed = Some(processed);
                    this.replay.looping = true;
                    this
                });
                // This page slice enters through the same non-tab owner as navigation.
                let focus = workbench.read(cx).page_focus.clone();
                focus.focus(window, cx);
                cx.new(|_| ReplayPageFixture { workbench })
            },
        )
        .unwrap()
    });
    let workbench = cx.update(|cx| fixture.read(cx).workbench.clone());
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(workbench.read(cx).page_focus.is_focused(window));
        for id in [
            "replay-pan-earlier",
            "replay-pan-later",
            "replay-zoom-out",
            "replay-fit",
        ] {
            focus_button(id, window, cx);
            let generation = workbench.read(cx).replay.generation;
            let animation_epoch = workbench.read(cx).replay.animation_epoch;
            window.key_down("space", false, cx);
            assert!(!workbench.read(cx).replay.playing, "keydown on {id}");
            assert_eq!(workbench.read(cx).replay.animation_epoch, animation_epoch);
            window.key_up("space", cx);
            let state = &workbench.read(cx).replay;
            assert!(!state.playing, "keyup on {id}");
            assert_eq!(state.generation, generation, "no-op viewport action {id}");
            assert_eq!(
                state.animation_epoch, animation_epoch,
                "no playback on {id}"
            );
            assert_eq!(state.requested_range, Some(full_range));
        }
        focus_button("replay-zoom-in", window, cx);
        let generation = workbench.read(cx).replay.generation;
        let animation_epoch = workbench.read(cx).replay.animation_epoch;
        window.key_down("space", false, cx);
        assert!(!workbench.read(cx).replay.playing);
        assert_eq!(workbench.read(cx).replay.generation, generation);
        assert_eq!(workbench.read(cx).replay.animation_epoch, animation_epoch);
        window.key_up("space", cx);
        let state = &workbench.read(cx).replay;
        assert!(!state.playing);
        assert_eq!(state.generation, generation + 1, "one viewport request");
        assert_eq!(
            state.animation_epoch,
            animation_epoch + 1,
            "only request pauses"
        );
        assert_eq!(
            state.requested_range,
            Some(viewport_range(full_range, 0., full_range.1, 0.8, 0.))
        );
    })
    .unwrap();
    crate::sessions::test_support::until(|| {
        cx.update(|cx| {
            workbench.update(cx, |this, cx| {
                this.poll_replay(cx);
                !this.replay.busy()
            })
        })
    });
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        focus_button("replay-zoom-in", window, cx);
        window.press("shift-tab", cx);
        assert!(
            workbench
                .read(cx)
                .replay
                .focus
                .as_ref()
                .unwrap()
                .is_focused(window)
        );
        let generation = workbench.read(cx).replay.generation;
        window.press("right", cx);
        assert_eq!(workbench.read(cx).replay.cursor, 1);
        window.press("left", cx);
        assert_eq!(workbench.read(cx).replay.cursor, 0);
        window.press("space", cx);
        assert!(
            workbench.read(cx).replay.playing,
            "chart Space starts playback"
        );
        window.press("space", cx);
        assert!(
            !workbench.read(cx).replay.playing,
            "chart Space pauses playback"
        );
        assert_eq!(workbench.read(cx).replay.generation, generation);
    })
    .unwrap();
}

#[test]
fn replay_split_normalizes_scaled_widths_without_resetting_user_size() {
    // These are stored proportional widths; the renderer already shows
    // the RHS at its limit. Correct state so the next drag has no dead zone.
    assert_eq!(
        replay_diagnostics_width_correction(&[px(1075.), px(525.)]),
        Some(px(360.)),
    );
    assert_eq!(
        replay_diagnostics_width_correction(&[px(640.), px(240.)]),
        Some(px(280.)),
    );
    for width in [280., 315., 360.] {
        assert_eq!(
            replay_diagnostics_width_correction(&[px(1600. - width), px(width)]),
            None,
        );
    }
    assert_eq!(replay_diagnostics_width_correction(&[]), None);
}

#[test]
fn invalidation_cancels_but_retains_worker_until_join() {
    let mut state = ReplayUiState::default();
    let cancel = Arc::new(AtomicBool::new(false));
    let worker_cancel = cancel.clone();
    let (_send, receive) = mpsc::channel();
    let handle = thread::spawn(move || {
        while !worker_cancel.load(Ordering::Relaxed) {
            thread::yield_now();
        }
    });
    state.worker = Some(Worker {
        generation: 0,
        cancel: cancel.clone(),
        progress: Arc::new(AtomicUsize::new(0)),
        receive,
        handle,
    });
    state.pending = Some(Request::Load(PathBuf::from("old.slimecap")));
    state.playing = true;
    state.invalidate();
    assert_eq!(state.generation, 1);
    assert!(cancel.load(Ordering::Relaxed));
    assert!(state.worker.is_some());
    assert!(state.pending.is_none());
    assert!(!state.playing);
    state.worker.take().unwrap().handle.join().unwrap();
}

#[test]
fn viewport_zoom_centers_cursor_and_pan_fit_stay_in_recording() {
    assert_eq!(
        viewport_range((0., 7200.), 3600., 7200., 0.5, 0.),
        (1800., 5400.)
    );
    assert_eq!(
        viewport_range((1800., 5400.), 3600., 7200., 1., 0.75),
        (3600., 7200.)
    );
    assert_eq!(
        viewport_range((1800., 5400.), 3600., 7200., 1., -0.75),
        (0., 3600.)
    );
    assert_eq!(
        viewport_range((1800., 5400.), 3600., 7200., 0., 0.),
        (0., 7200.)
    );
    assert_eq!(viewport_range((0., 0.), 0., 0., 0.5, 0.), (0., 0.));
}

#[test]
fn missing_or_disabled_magnetometer_does_not_report_disturbance() {
    assert_eq!(
        replay_magnetic_status(true, false, false, true, true),
        "replay.status.magnetic_unavailable"
    );
    assert_eq!(
        replay_magnetic_status(false, true, true, true, true),
        "replay.status.magnetic_unused"
    );
    assert_eq!(
        replay_magnetic_status(true, true, true, true, true),
        "replay.status.magnetic_unsettled"
    );
    assert_eq!(
        replay_magnetic_status(true, true, true, true, false),
        "replay.status.magnetic_stable"
    );
    assert_eq!(
        replay_magnetic_status(true, true, false, true, true),
        "replay.status.magnetic_waiting"
    );
    assert_eq!(
        replay_magnetic_status(true, true, true, false, true),
        "replay.status.magnetic_rejection_disabled"
    );
}

#[test]
fn scrub_mapping_is_uniform_and_clamps_outside_the_plot() {
    assert_eq!(replay_scrub_time(-0.3, (100., 200.)), 100.);
    assert_eq!(replay_scrub_time(0.25, (100., 200.)), 125.);
    assert_eq!(replay_scrub_time(1.4, (100., 200.)), 200.);
    assert_eq!(replay_scrub_time(0.5, (7., 7.)), 7.);
}

#[test]
fn pausing_revokes_pending_animation_chain() {
    let mut state = ReplayUiState {
        playing: true,
        animation_pending: true,
        animation_epoch: 7,
        ..Default::default()
    };
    state.pause();
    assert!(!state.playing);
    assert!(!state.animation_pending);
    assert_eq!(state.animation_epoch, 8);
}

#[test]
fn parameter_editor_roundtrips_all_numeric_parameters_and_preserves_toggles() {
    let source = slimenrf_recording::replay::ReplayParams::default();
    let values = parameter_values(&source).map(Some);
    let mut target = source.clone();
    target.tau_acc = 4.;
    target.mag_rejection_factor = 2.;
    target.motion_bias_est_enabled = false;
    set_parameter_values(&mut target, &values);
    assert_eq!(parameter_values(&target), parameter_values(&source));
    assert!(!target.motion_bias_est_enabled);
}

#[test]
fn viewport_invalidation_retains_manual_references_and_auto_attempt() {
    let mut state = ReplayUiState {
        reference_a: Some(12),
        reference_b: Some(47),
        auto_attempted: true,
        cursor: 30,
        ..Default::default()
    };
    state.invalidate();
    assert_eq!(state.reference_a, Some(12));
    assert_eq!(state.reference_b, Some(47));
    assert!(state.auto_attempted);
    assert_eq!(state.cursor, 30);
}

#[test]
fn automatic_edits_wait_for_file_dialog_and_resulting_operation() {
    let now = std::time::Instant::now();
    let overdue = Some(now - std::time::Duration::from_millis(450));
    assert!(!replay_edit_due(overdue, now, true, false));
    assert!(!replay_edit_due(overdue, now, true, true));
    assert!(!replay_edit_due(overdue, now, false, true));
    assert!(replay_edit_due(overdue, now, false, false));
    assert!(!replay_edit_due(None, now, false, false));
    assert!(!replay_edit_due(
        Some(now + std::time::Duration::from_millis(450)),
        now,
        false,
        false,
    ));
}
