use super::controller::replay_edit_due;
use super::diagnostics::replay_magnetic_status;
use super::parameters::{parameter_values, set_parameter_values};
use super::render::replay_diagnostics_width_correction;
use super::scrubbing::{replay_scrub_time, viewport_range};
use super::{ReplayFeature, ReplayState, ReplayViewData, Request, Worker};
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

fn replay_data() -> (
    Arc<slimenrf_recording::capture::Recording>,
    Arc<ReplayViewData>,
) {
    use slimenrf_recording::{
        capture::{Recording, Sample, SensorMetadata, Track},
        replay::{ReplayOptions, process},
    };
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
    let result = process(
        recording.clone(),
        0,
        ReplayOptions::default(),
        &AtomicBool::new(false),
        &AtomicUsize::new(0),
    )
    .unwrap();
    let data = ReplayViewData::prepare_range(result, None, &AtomicBool::new(false)).unwrap();
    (recording, Arc::new(data))
}

#[gpui_kit::test]
fn viewport_button_space_does_not_trigger_page_playback(cx: &mut gpui_kit::TestAppContext) {
    let _ui_session = crate::test_support::UiTestSession::new(cx);
    use gpui_kit::test::{TestEventExt as _, TestWindowExt as _};
    use gpui_kit::{
        App, AppContext as _, Bounds, Context, Entity, FocusHandle, IntoElement, Render, Window,
        WindowBounds, WindowOptions, div, prelude::*, size,
    };

    struct ReplayPageFixture {
        replay: Entity<ReplayFeature>,
        focus: FocusHandle,
    }

    impl Render for ReplayPageFixture {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .track_focus(&self.focus)
                .tab_group()
                .flex()
                .flex_col()
                .child(self.replay.clone())
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

    let (recording, processed) = replay_data();
    let full_range = processed.range;
    assert!(full_range.1 > full_range.0);

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
                let replay = cx.new(|cx| {
                    let mut this = ReplayFeature::new(window, std::rc::Rc::new(|_, _, _| None));
                    // This feature fixture never accesses the user's preset storage.
                    assert!(this.state.presets.shutdown().is_empty());
                    this.state.recording = Some(recording);
                    this.state.options = processed.result.options.clone();
                    this.state.requested_range = Some(full_range);
                    this.state.processed = Some(processed);
                    this.state.looping = true;
                    this.activate(window, cx);
                    this
                });
                let focus = cx.focus_handle().tab_stop(false);
                focus.focus(window, cx);
                cx.new(|_| ReplayPageFixture { replay, focus })
            },
        )
        .unwrap()
    });
    let feature = cx.update(|cx| fixture.read(cx).replay.clone());
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(fixture.read(cx).focus.is_focused(window));
        for id in [
            "replay-pan-earlier",
            "replay-pan-later",
            "replay-zoom-out",
            "replay-fit",
        ] {
            focus_button(id, window, cx);
            let generation = feature.read(cx).state.generation;
            let animation_epoch = feature.read(cx).state.animation_epoch;
            window.key_down("space", false, cx);
            assert!(!feature.read(cx).state.playing, "keydown on {id}");
            assert_eq!(feature.read(cx).state.animation_epoch, animation_epoch);
            window.key_up("space", cx);
            let state = &feature.read(cx).state;
            assert!(!state.playing, "keyup on {id}");
            assert_eq!(state.generation, generation, "no-op viewport action {id}");
            assert_eq!(
                state.animation_epoch, animation_epoch,
                "no playback on {id}"
            );
            assert_eq!(state.requested_range, Some(full_range));
        }
        focus_button("replay-zoom-in", window, cx);
        let generation = feature.read(cx).state.generation;
        let animation_epoch = feature.read(cx).state.animation_epoch;
        window.key_down("space", false, cx);
        assert!(!feature.read(cx).state.playing);
        assert_eq!(feature.read(cx).state.generation, generation);
        assert_eq!(feature.read(cx).state.animation_epoch, animation_epoch);
        window.key_up("space", cx);
        let state = &feature.read(cx).state;
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
        cx.update_window(handle, |_, window, cx| {
            feature.update(cx, |this, cx| {
                this.poll(window, cx);
                !this.state.busy()
            })
        })
        .unwrap()
    });
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        focus_button("replay-zoom-in", window, cx);
        window.press("shift-tab", cx);
        assert!(
            feature
                .read(cx)
                .state
                .focus
                .as_ref()
                .unwrap()
                .is_focused(window)
        );
        let generation = feature.read(cx).state.generation;
        window.press("right", cx);
        assert_eq!(feature.read(cx).state.cursor, 1);
        window.press("left", cx);
        assert_eq!(feature.read(cx).state.cursor, 0);
        window.press("up", cx);
        assert_eq!(feature.read(cx).state.speed, 2.);
        window.press("down", cx);
        assert_eq!(feature.read(cx).state.speed, 1.);
        window.press("a", cx);
        assert_eq!(feature.read(cx).state.reference_a, Some(0));
        window.press("right", cx);
        window.press("b", cx);
        assert_eq!(feature.read(cx).state.reference_b, Some(1));
        window.press("x", cx);
        assert_eq!(feature.read(cx).state.reference_a, None);
        assert_eq!(feature.read(cx).state.reference_b, None);
        window.press("space", cx);
        assert!(
            feature.read(cx).state.playing,
            "chart Space starts playback"
        );
        window.press("space", cx);
        assert!(
            !feature.read(cx).state.playing,
            "chart Space pauses playback"
        );
        assert_eq!(feature.read(cx).state.generation, generation);
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
    let mut state = ReplayState::default();
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
    let mut state = ReplayState {
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
    let mut state = ReplayState {
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

#[test]
fn speed_changes_settle_elapsed_playback_under_the_previous_speed() {
    use std::time::{Duration, Instant};
    let (_, processed) = replay_data();
    let start = Instant::now();
    let mut state = ReplayState {
        processed: Some(processed),
        playing: true,
        clock: start,
        ..Default::default()
    };
    // Before the shared command, keyboard Up replaced speed without advancing
    // the clock: at 250 ms it reported 0.5 s instead of 0.375 s.
    assert!(state.set_speed_at(2., start + Duration::from_millis(125)));
    assert_eq!(state.playback_time, 0.125);
    assert_eq!(state.speed, 2.);
    assert!(state.advance_clock_at(start + Duration::from_millis(250)));
    assert_eq!(state.playback_time, 0.375);
    assert_eq!(state.cursor, 37);

    state.set_speed_at(0.5, start + Duration::from_millis(375));
    assert_eq!(state.playback_time, 0.625);
    state.advance_clock_at(start + Duration::from_millis(500));
    assert_eq!(state.playback_time, 0.6875);
    assert_eq!(state.cursor, 68);
    let epoch = state.animation_epoch;
    state.set_speed_at(0.5, start + Duration::from_millis(625));
    assert_eq!(state.playback_time, 0.75);
    assert_eq!(
        state.animation_epoch, epoch,
        "same speed does not restart playback"
    );
    assert!(state.playing);
}

#[test]
fn paused_speed_changes_do_not_advance_or_restart_the_playback_clock() {
    use std::time::Duration;
    let mut state = ReplayState {
        playback_time: 0.375,
        cursor: 37,
        animation_epoch: 9,
        ..Default::default()
    };
    let clock = state.clock;
    assert!(!state.set_speed_at(4., clock + Duration::from_secs(8)));
    assert_eq!(state.playback_time, 0.375);
    assert_eq!(state.cursor, 37);
    assert_eq!(state.clock, clock);
    assert_eq!(state.speed, 4.);
    assert_eq!(state.animation_epoch, 9);
    assert!(!state.playing);
}

#[gpui_kit::test]
fn feature_deactivation_pauses_without_canceling_work_and_shutdown_transfers_its_join(
    cx: &mut gpui_kit::TestAppContext,
) {
    use gpui_kit::VisualContext as _;
    let _ui_session = crate::test_support::UiTestSession::new(cx);
    let cx = cx.add_empty_window();
    let cancel = Arc::new(AtomicBool::new(false));
    let (release, wait) = mpsc::channel();
    let (_publish, receive) = mpsc::channel();
    let handle = thread::spawn(move || wait.recv().unwrap());
    let feature = cx.new_window_entity(|window, cx| {
        let mut this = ReplayFeature::new(window, std::rc::Rc::new(|_, _, _| None));
        assert!(this.state.presets.shutdown().is_empty());
        this.activate(window, cx);
        this.state.worker = Some(Worker {
            generation: this.state.generation,
            cancel: cancel.clone(),
            progress: Arc::new(AtomicUsize::new(0)),
            receive,
            handle,
        });
        this.state.playing = true;
        this.state.animation_pending = true;
        this.state.reference_a = Some(12);
        this.state.reference_b = Some(47);
        this
    });
    let cleanup = feature.update_in(cx, |this, window, cx| {
        let generation = this.state.generation;
        let epoch = this.state.animation_epoch;
        this.deactivate(window, cx);
        assert!(!this.state.page_visible);
        assert!(!this.state.playing);
        assert!(!this.state.animation_pending);
        assert_eq!(this.state.animation_epoch, epoch + 1);
        assert_eq!(this.state.generation, generation);
        assert_eq!(this.state.reference_a, Some(12));
        assert_eq!(this.state.reference_b, Some(47));
        assert!(this.state.worker.is_some());
        assert!(!cancel.load(Ordering::Relaxed));
        this.activate(window, cx);
        assert!(this.state.page_visible);
        assert!(!this.state.playing);
        let cleanup = this.shutdown(cx);
        assert!(this.stopped);
        assert!(!this.state.page_visible);
        assert!(this.state.worker.is_none());
        assert!(cancel.load(Ordering::Relaxed));
        this.activate(window, cx);
        assert!(!this.state.page_visible, "shutdown revokes activation");
        assert!(this.shutdown(cx).workers.is_empty());
        cleanup
    });
    assert_eq!(cleanup.workers.len(), 1);
    assert!(!cleanup.workers[0].is_finished());
    assert!(cleanup.files.is_empty());
    release.send(()).unwrap();
    for worker in cleanup.workers {
        worker.join().unwrap();
    }
}

#[gpui_kit::test]
fn poll_notifies_its_own_view_for_real_worker_progress_and_joined_results(
    cx: &mut gpui_kit::TestAppContext,
) {
    use gpui_kit::{AppContext as _, VisualContext as _};
    use std::{cell::Cell, rc::Rc};
    let _ui_session = crate::test_support::UiTestSession::new(cx);
    let cx = cx.add_empty_window();
    let feature =
        cx.new_window_entity(|window, _| ReplayFeature::new(window, Rc::new(|_, _, _| None)));
    let notifications = Rc::new(Cell::new(0));
    let observed_notifications = notifications.clone();
    let _observer = cx.new(|cx| {
        cx.observe(&feature, move |_, _, _| {
            observed_notifications.set(observed_notifications.get() + 1);
        })
        .detach();
    });
    let mut work = feature.update_in(cx, |this, window, cx| {
        super::test_support::hold_computed_result(this, window, cx)
    });
    cx.run_until_parked();
    notifications.set(0);

    feature.update_in(cx, |this, window, cx| {
        this.poll(window, cx);
        assert_eq!(this.state.displayed_progress, work.progress());
        assert!(
            this.state.processed.is_none(),
            "the native join remains held"
        );
        assert!(this.state.busy());
    });
    cx.run_until_parked();
    assert_eq!(
        notifications.get(),
        1,
        "progress redraw belongs to ReplayFeature"
    );
    feature.update_in(cx, |this, window, cx| this.poll(window, cx));
    cx.run_until_parked();
    assert_eq!(notifications.get(), 1, "unchanged progress does not redraw");

    work.release();
    crate::sessions::test_support::until(|| {
        feature.update_in(cx, |this, window, cx| {
            this.poll(window, cx);
            !this.state.busy()
        })
    });
    cx.run_until_parked();
    assert_eq!(
        notifications.get(),
        2,
        "the joined result redraws the feature once"
    );
    feature.update(cx, |this, cx| {
        assert!(
            !this
                .state
                .processed
                .as_ref()
                .unwrap()
                .result
                .frames
                .is_empty()
        );
        assert!(this.state.error.is_none());
        let cleanup = this.shutdown(cx);
        assert!(cleanup.workers.is_empty());
        assert!(cleanup.files.is_empty());
    });
}
