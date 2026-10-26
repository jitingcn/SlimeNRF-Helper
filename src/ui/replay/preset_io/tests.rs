use super::super::{ReplayUiState, Workbench};
use super::{PresetCompletion, PresetEdit, PresetIo};
use futures_util::{FutureExt as _, future::Shared};
use gpui_kit::{
    BackgroundExecutor, Entity, PlatformDispatcher, Priority, RunnableVariant, Task,
    TestAppContext, VisualContext as _, VisualTestContext,
};
use slimenrf_recording::replay::{ReplayParams, presets::PresetStore};
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
        panic!("preset IO must not schedule a foreground runnable");
    }

    fn dispatch_after(&self, _: Duration, _: RunnableVariant) {
        panic!("preset IO does not require a timer");
    }

    fn spawn_realtime(&self, _: Box<dyn FnOnce() + Send>) {
        panic!("preset IO does not require a realtime thread");
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
        let runnable = self.queued.recv_timeout(Duration::from_secs(5)).unwrap();
        // GPUI test foreground wakers must be awakened on the deterministic test thread.
        runnable.run();
    }

    fn complete(
        &self,
        io: &mut PresetIo,
        join: Shared<Task<()>>,
        generation: u64,
    ) -> PresetCompletion {
        assert_eq!(join.clone().now_or_never(), None);
        self.run_next();
        assert_eq!(join.now_or_never(), Some(()));
        io.finish(generation)
    }

    fn load(&self, path: &Path) -> PresetIo {
        let mut io = PresetIo::default();
        let join = io.load(Some(path.to_path_buf()), &self.executor).unwrap();
        assert!(matches!(
            self.complete(&mut io, join, 0),
            PresetCompletion::Quiet
        ));
        io
    }
}

fn stored_names(path: &Path) -> Vec<String> {
    PresetStore::load(Some(path.to_path_buf()))
        .unwrap()
        .presets()
        .iter()
        .map(|preset| preset.name.clone())
        .collect()
}

fn save(name: &str, tau_acc: f32) -> PresetEdit {
    PresetEdit::Save {
        name: name.into(),
        params: ReplayParams {
            tau_acc,
            ..ReplayParams::default()
        },
    }
}

fn replay_fixture(
    cx: &mut TestAppContext,
    presets: PresetIo,
) -> (
    Entity<Workbench>,
    &mut VisualTestContext,
    crate::app_runtime::AppRuntime,
) {
    let runtime = crate::app_runtime::AppRuntime::new().unwrap();
    let executor = runtime.executor();

    let cx = cx.add_empty_window();
    let workbench = cx.new_window_entity(move |window, cx| {
        let mut this = crate::ui::test_support::offline_workbench(
            window,
            cx,
            crate::sessions::SessionManager::new(),
            executor,
        );
        this.replay.presets = presets;
        this.replay.options.raw_rate_hz = Some(100.);
        this.replay.options.gyro_rate_hz = Some(100.);
        this.replay.options.accel_rate_hz = Some(100.);
        this
    });
    (workbench, cx, runtime)
}

fn settle_replay(workbench: &Entity<Workbench>, cx: &mut VisualTestContext) {
    crate::sessions::test_support::until(|| {
        workbench.update(cx, |this, cx| {
            this.poll_replay(cx);
            !this.replay.busy()
        })
    });
}

fn open_malformed_recording(
    workbench: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    directory: &Path,
) {
    let path = directory.join("malformed.csv");
    std::fs::write(
        &path,
        "seq,gx,gy,gz,ax,ay,az,mx,my,mz\n0,NaN,2,3,4,5,6,7,8,9\n",
    )
    .unwrap();
    workbench.update(cx, |this, cx| this.replay_open_path(path, cx));
    settle_replay(workbench, cx);
    workbench.update(cx, |this, _| {
        assert_eq!(
            this.replay.error.as_deref(),
            Some("Non-finite numeric input")
        );
    });
}

#[test]
fn one_operation_retains_snapshot_and_new_builtin_selection() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("presets.json");
    let mut store = PresetStore::load(Some(path.clone())).unwrap();
    store
        .save("VQF-C defaults", &ReplayParams::default())
        .unwrap();
    store.save("Second", &ReplayParams::default()).unwrap();
    let gate = IoGate::new();
    let mut io = gate.load(&path);
    io.select(Some("VQF-C defaults".into()));
    let join = io
        .edit(
            PresetEdit::Rename {
                old: "VQF-C defaults".into(),
                new: "Renamed".into(),
            },
            &gate.executor,
        )
        .unwrap();
    assert!(!io.can_mutate());
    assert_eq!(
        io.snapshot()
            .iter()
            .map(|preset| preset.name.as_str())
            .collect::<Vec<_>>(),
        ["VQF-C defaults", "Second"]
    );
    assert!(io.edit(save("Not admitted", 4.), &gate.executor).is_none());
    assert!(
        io.export(
            directory.path().join("not-admitted.yaml"),
            ReplayParams::default(),
            &gate.executor,
        )
        .is_none()
    );
    io.select(None);
    assert!(matches!(
        gate.complete(&mut io, join, 1),
        PresetCompletion::Changed
    ));
    assert_eq!(stored_names(&path), ["Renamed", "Second"]);
    assert_eq!(io.selected_custom_name(), None);
    assert!(!directory.path().join("not-admitted.yaml").exists());
}

#[test]
fn failed_mutation_returns_store_for_a_later_successful_edit() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("presets.json");
    let mut store = PresetStore::load(Some(path.clone())).unwrap();
    store.save("One", &ReplayParams::default()).unwrap();
    store.save("Two", &ReplayParams::default()).unwrap();
    let gate = IoGate::new();
    let mut io = gate.load(&path);
    io.select(Some("One".into()));
    let join = io
        .edit(
            PresetEdit::Rename {
                old: "One".into(),
                new: "Two".into(),
            },
            &gate.executor,
        )
        .unwrap();
    assert!(matches!(
        gate.complete(&mut io, join, 0),
        PresetCompletion::Error(_)
    ));
    assert!(io.can_mutate());
    assert_eq!(stored_names(&path), ["One", "Two"]);
    assert_eq!(io.selected_custom_name(), Some("One"));
    let join = io
        .edit(
            PresetEdit::Rename {
                old: "One".into(),
                new: "Renamed".into(),
            },
            &gate.executor,
        )
        .unwrap();
    assert!(matches!(
        gate.complete(&mut io, join, 0),
        PresetCompletion::Changed
    ));
    assert_eq!(stored_names(&path), ["Renamed", "Two"]);
    assert_eq!(io.selected_custom_name(), Some("Renamed"));
    let join = io
        .edit(
            PresetEdit::Delete {
                name: "Renamed".into(),
            },
            &gate.executor,
        )
        .unwrap();
    assert!(matches!(
        gate.complete(&mut io, join, 0),
        PresetCompletion::Changed
    ));
    assert_eq!(stored_names(&path), ["Two"]);
    assert_eq!(io.selected_custom_name(), None);
}

#[gpui_kit::test]
fn preset_watcher_restores_committed_store_without_clearing_a_new_recording_error(
    cx: &mut TestAppContext,
) {
    let _ui_session = crate::ui::test_support::UiTestSession::new(cx);
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("presets.json");
    let gate = IoGate::new();
    let (workbench, cx, _runtime) = replay_fixture(cx, gate.load(&path));
    workbench.update(cx, |this, cx| {
        let join = this
            .replay
            .presets
            .edit(save("Survives new replay", 7.25), &gate.executor)
            .unwrap();
        this.watch_replay_preset_io(join, cx);
        assert!(!this.replay.presets.can_mutate());
    });
    cx.run_until_parked();
    open_malformed_recording(&workbench, cx, directory.path());
    gate.run_next();
    cx.run_until_parked();
    let stored = PresetStore::load(Some(path)).unwrap();
    assert_eq!(stored.presets()[0].name, "Survives new replay");
    assert_eq!(stored.presets()[0].params.tau_acc, 7.25);
    workbench.update(cx, |this, _| {
        assert_eq!(
            this.replay.presets.snapshot()[0].name,
            "Survives new replay"
        );
        assert_eq!(this.replay.presets.snapshot()[0].params.tau_acc, 7.25);
        assert!(this.replay.presets.can_mutate());
        assert_eq!(
            this.replay.error.as_deref(),
            Some("Non-finite numeric input")
        );
        assert_eq!(this.replay.notice, None);
    });
}

#[test]
fn yaml_import_only_publishes_into_its_captured_replay_generation() {
    let directory = tempfile::tempdir().unwrap();
    let yaml = directory.path().join("parameters.yaml");
    std::fs::write(&yaml, "tauAcc: 7.25\nrestMinT: 1.5\n").unwrap();
    let gate = IoGate::new();
    let mut replay = ReplayUiState::default();
    let join = replay
        .presets
        .import(
            yaml.clone(),
            ReplayParams::default(),
            replay.generation,
            &gate.executor,
        )
        .unwrap();
    assert!(replay.presets.importing());
    replay.invalidate();
    assert!(matches!(
        gate.complete(&mut replay.presets, join, replay.generation),
        PresetCompletion::Quiet
    ));
    assert_eq!(replay.options.params.tau_acc, 1.011);
    let join = replay
        .presets
        .import(
            yaml,
            ReplayParams::default(),
            replay.generation,
            &gate.executor,
        )
        .unwrap();
    let PresetCompletion::Imported(params) =
        gate.complete(&mut replay.presets, join, replay.generation)
    else {
        panic!("the matching replay generation must receive its parsed YAML");
    };
    assert_eq!(params.tau_acc, 7.25);
    assert_eq!(params.rest_min_t, 1.5);
    assert!(!replay.presets.importing());
}

#[test]
fn shutdown_retains_the_write_join_after_canceling_ui_completion() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("presets.json");
    let gate = IoGate::new();
    let mut io = gate.load(&path);
    let join = io
        .edit(save("Accepted before shutdown", 9.5), &gate.executor)
        .unwrap();
    let published = Arc::new(AtomicBool::new(false));
    let completion_published = Arc::clone(&published);
    io.completion = Some(gate.executor.spawn(async move {
        join.await;
        completion_published.store(true, Ordering::Relaxed);
    }));
    let cleanup = io.shutdown();
    assert_eq!(cleanup.len(), 1);
    assert_eq!(cleanup[0].clone().now_or_never(), None);
    assert!(
        io.edit(save("Rejected after shutdown", 4.), &gate.executor)
            .is_none()
    );
    assert!(
        io.import(
            directory.path().join("missing.yaml"),
            ReplayParams::default(),
            0,
            &gate.executor,
        )
        .is_none()
    );
    gate.run_next();
    assert_eq!(cleanup[0].clone().now_or_never(), Some(()));
    gate.run_next();
    assert!(!published.load(Ordering::Relaxed));
    let stored = PresetStore::load(Some(path)).unwrap();
    assert_eq!(stored.presets()[0].name, "Accepted before shutdown");
    assert_eq!(stored.presets()[0].params.tau_acc, 9.5);
}

#[test]
fn damaged_existing_store_is_not_replaced_and_returned_store_can_recover() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("presets.json");
    let mut store = PresetStore::load(Some(path.clone())).unwrap();
    store.save("Original", &ReplayParams::default()).unwrap();
    let valid = std::fs::read(&path).unwrap();
    let gate = IoGate::new();
    let mut io = gate.load(&path);
    std::fs::write(&path, b"invalid-existing-preset-store").unwrap();
    let join = io.edit(save("Rejected", 7.), &gate.executor).unwrap();
    assert!(matches!(
        gate.complete(&mut io, join, 0),
        PresetCompletion::Error(_)
    ));
    assert_eq!(
        std::fs::read(&path).unwrap(),
        b"invalid-existing-preset-store"
    );
    assert_eq!(io.snapshot()[0].name, "Original");
    assert!(io.can_mutate());
    std::fs::write(&path, valid).unwrap();
    let join = io.edit(save("Recovered", 7.), &gate.executor).unwrap();
    assert!(matches!(
        gate.complete(&mut io, join, 0),
        PresetCompletion::Changed
    ));
    assert_eq!(stored_names(&path), ["Original", "Recovered"]);
}

#[test]
fn invalid_initial_store_disables_mutations_but_yaml_export_remains_available() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("presets.json");
    std::fs::write(&path, b"not a preset document").unwrap();
    let gate = IoGate::new();
    let mut io = gate.load(&path);
    assert!(io.load_error.is_some());
    assert!(io.edit(save("Forbidden", 4.), &gate.executor).is_none());
    let yaml = directory.path().join("exported.yaml");
    let join = io
        .export(
            yaml.clone(),
            ReplayParams {
                tau_acc: 8.5,
                ..ReplayParams::default()
            },
            &gate.executor,
        )
        .unwrap();
    let cleanup = io.shutdown();
    drop(join);
    assert_eq!(cleanup[0].clone().now_or_never(), None);
    gate.run_next();
    assert_eq!(cleanup[0].clone().now_or_never(), Some(()));
    let exported = std::fs::read_to_string(yaml).unwrap();
    let params =
        slimenrf_recording::replay::presets::merge_yaml(&ReplayParams::default(), &exported)
            .unwrap();
    assert_eq!(params.tau_acc, 8.5);
    assert_eq!(std::fs::read(path).unwrap(), b"not a preset document");
}

#[gpui_kit::test]
fn yaml_export_watcher_preserves_a_new_recording_error_on_success_and_failure(
    cx: &mut TestAppContext,
) {
    let _ui_session = crate::ui::test_support::UiTestSession::new(cx);
    let directory = tempfile::tempdir().unwrap();
    let gate = IoGate::new();
    let (workbench, cx, _runtime) =
        replay_fixture(cx, gate.load(&directory.path().join("presets.json")));
    let exported = directory.path().join("exported.yaml");
    for destination in [exported.clone(), directory.path().to_path_buf()] {
        workbench.update(cx, |this, cx| {
            let join = this
                .replay
                .presets
                .export(
                    destination,
                    ReplayParams {
                        tau_acc: 8.5,
                        ..ReplayParams::default()
                    },
                    &gate.executor,
                )
                .unwrap();
            this.watch_replay_preset_io(join, cx);
            assert!(this.replay.presets.busy());
        });
        cx.run_until_parked();
        open_malformed_recording(&workbench, cx, directory.path());
        gate.run_next();
        cx.run_until_parked();
        workbench.update(cx, |this, _| {
            assert!(this.replay.presets.can_mutate());
            assert_eq!(
                this.replay.error.as_deref(),
                Some("Non-finite numeric input")
            );
            assert_eq!(this.replay.notice, None);
        });
    }
    let yaml = std::fs::read_to_string(exported).unwrap();
    let params =
        slimenrf_recording::replay::presets::merge_yaml(&ReplayParams::default(), &yaml).unwrap();
    assert_eq!(params.tau_acc, 8.5);
}

fn write_valid_recording(path: &Path) {
    std::fs::write(
        path,
        "seq,gx,gy,gz,ax,ay,az,mx,my,mz\n7,1,2,3,0,0,1,0,0,0\n8,4,5,6,0,0,1,0,0,0\n",
    )
    .unwrap();
}

#[gpui_kit::test]
fn recording_choosers_wait_for_yaml_application_then_accept_recording_and_zip_paths(
    cx: &mut TestAppContext,
) {
    let _ui_session = crate::ui::test_support::UiTestSession::new(cx);
    let directory = tempfile::tempdir().unwrap();
    let recording = directory.path().join("recording.csv");
    write_valid_recording(&recording);
    let initial = directory.path().join("initial.csv");
    std::fs::write(
        &initial,
        "seq,gx,gy,gz,ax,ay,az,mx,my,mz\n1,0,0,0,0,0,1,0,0,0\n",
    )
    .unwrap();
    let yaml = directory.path().join("parameters.yaml");
    std::fs::write(&yaml, "tauAcc: 7.25\nrestMinT: 1.5\n").unwrap();
    let gate = IoGate::new();
    let (workbench, cx, _runtime) =
        replay_fixture(cx, gate.load(&directory.path().join("presets.json")));
    workbench.update(cx, |this, cx| this.replay_open_path(initial, cx));
    settle_replay(&workbench, cx);
    let generation = workbench.update(cx, |this, cx| {
        assert!(this.replay.recording.is_some());
        let generation = this.replay.generation;
        let join = this
            .replay
            .presets
            .import(
                yaml,
                this.replay.options.params.clone(),
                generation,
                &gate.executor,
            )
            .unwrap();
        this.watch_replay_preset_io(join, cx);
        this.choose_replay(cx);
        assert!(this.replay.chooser.is_none());
        this.export_replay(cx);
        assert!(this.replay.chooser.is_none());
        assert!(this.replay.presets.importing());
        generation
    });
    assert!(!cx.did_prompt_for_paths());
    cx.run_until_parked();
    gate.run_next();
    cx.run_until_parked();
    workbench.update(cx, |this, cx| {
        assert!(!this.replay.presets.importing());
        assert_eq!(this.replay.generation, generation + 1);
        assert_eq!(this.replay.options.params.tau_acc, 7.25);
        assert_eq!(this.replay.options.params.rest_min_t, 1.5);
        this.choose_replay(cx);
        assert!(this.replay.chooser.is_some());
    });
    assert!(cx.did_prompt_for_paths());
    cx.simulate_path_prompt_response(|_| Some(vec![recording.clone()]));
    cx.run_until_parked();
    settle_replay(&workbench, cx);
    workbench.update(cx, |this, cx| {
        assert_eq!(this.replay.path.as_ref(), Some(&recording));
        assert_eq!(this.replay.error, None);
        let samples = &this.replay.recording.as_ref().unwrap().tracks[0].samples;
        assert_eq!(samples.len(), 2);
        assert_eq!(samples[0].sequence, 7);
        assert_eq!(samples[1].sequence, 8);
        assert_eq!(
            samples[1].gyro,
            slimenrf_recording::capture::Gyro::RatesDps([4., 5., 6.])
        );
        this.export_replay(cx);
        assert!(this.replay.chooser.is_some());
    });
    let zip = directory.path().join("recording.zip");
    cx.simulate_new_path_selection(|_| Some(zip.clone()));
    cx.run_until_parked();
    settle_replay(&workbench, cx);
    let restored =
        slimenrf_recording::capture::format::load(&zip, &AtomicBool::new(false)).unwrap();
    assert_eq!(restored.tracks.len(), 1);
    assert_eq!(restored.tracks[0].samples.len(), 2);
    assert_eq!(restored.tracks[0].samples[0].sequence, 7);
    assert_eq!(
        restored.tracks[0].samples[1].gyro,
        slimenrf_recording::capture::Gyro::RatesDps([4., 5., 6.])
    );
}

#[gpui_kit::test]
fn preset_saves_and_yaml_exports_do_not_block_recording_choosers(cx: &mut TestAppContext) {
    let _ui_session = crate::ui::test_support::UiTestSession::new(cx);
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("presets.json");
    let recording = directory.path().join("recording.csv");
    write_valid_recording(&recording);
    let yaml = directory.path().join("parameters.yaml");
    let gate = IoGate::new();
    let (workbench, cx, _runtime) = replay_fixture(cx, gate.load(&path));
    workbench.update(cx, |this, cx| this.replay_open_path(recording, cx));
    settle_replay(&workbench, cx);
    for export in [false, true] {
        workbench.update(cx, |this, cx| {
            let join = if export {
                this.replay.presets.export(
                    yaml.clone(),
                    ReplayParams {
                        tau_acc: 9.5,
                        ..ReplayParams::default()
                    },
                    &gate.executor,
                )
            } else {
                this.replay.presets.edit(save("Saved", 9.5), &gate.executor)
            }
            .unwrap();
            this.watch_replay_preset_io(join, cx);
            this.choose_replay(cx);
            assert!(this.replay.chooser.is_some());
        });
        assert!(cx.did_prompt_for_paths());
        cx.simulate_path_prompt_response(|_| None);
        cx.run_until_parked();
        workbench.update(cx, |this, cx| {
            assert!(this.replay.presets.busy());
            this.export_replay(cx);
            assert!(this.replay.chooser.is_some());
        });
        cx.simulate_new_path_selection(|_| None);
        cx.run_until_parked();
        gate.run_next();
        cx.run_until_parked();
        workbench.update(cx, |this, _| {
            assert!(this.replay.presets.can_mutate());
            assert_eq!(
                this.replay.notice,
                Some(crate::i18n::tr(if export {
                    "replay.preset.yaml_exported"
                } else {
                    "replay.preset.saved"
                }))
            );
            assert_eq!(this.replay.error, None);
        });
    }
    let stored = PresetStore::load(Some(path)).unwrap();
    assert_eq!(stored.presets()[0].name, "Saved");
    assert_eq!(stored.presets()[0].params.tau_acc, 9.5);
    let exported = std::fs::read_to_string(yaml).unwrap();
    let params =
        slimenrf_recording::replay::presets::merge_yaml(&ReplayParams::default(), &exported)
            .unwrap();
    assert_eq!(params.tau_acc, 9.5);
}
