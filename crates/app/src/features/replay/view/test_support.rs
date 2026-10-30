use super::{Payload, Recording, ReplayFeature, ReplayOptions, Request, Worker};
use gpui_kit::{Context, Window};
use std::{
    fmt::Write as _,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{self, Sender},
    },
    time::Duration,
};

pub(crate) struct ComputedReplayWork {
    release: Option<Sender<()>>,
    progress: usize,
    _directory: tempfile::TempDir,
}

impl ComputedReplayWork {
    pub(crate) fn progress(&self) -> usize {
        self.progress
    }

    pub(crate) fn release(&mut self) {
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
    }
}

impl Drop for ComputedReplayWork {
    fn drop(&mut self) {
        self.release();
    }
}

/// Holds a real parsed and computed replay result before its native worker exits.
pub(crate) fn hold_computed_result(
    feature: &mut ReplayFeature,
    window: &mut Window,
    cx: &mut Context<ReplayFeature>,
) -> ComputedReplayWork {
    assert!(!feature.stopped);
    assert!(!feature.state.busy());
    assert!(feature.state.recording.is_none());
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("poll-progress.csv");
    let mut csv = String::from("seq,gx,gy,gz,ax,ay,az,mx,my,mz\n");
    for sequence in 0..=1024 {
        writeln!(csv, "{sequence},0,0,0,0,0,1,0,0,0").unwrap();
    }
    std::fs::write(&path, csv).unwrap();
    let options = ReplayOptions {
        raw_rate_hz: Some(100.),
        gyro_rate_hz: Some(100.),
        accel_rate_hz: Some(100.),
        ..ReplayOptions::default()
    };
    assert!(feature.state.presets.shutdown().is_empty());
    feature.state.options = options.clone();
    feature.activate(window, cx);
    feature.state.invalidate();
    feature.state.displayed_progress = 0;
    feature.state.phase = "replay.phase.computing";
    let cancel = Arc::new(AtomicBool::new(false));
    let progress = Arc::new(AtomicUsize::new(0));
    let worker_cancel = cancel.clone();
    let worker_progress = progress.clone();
    let (send, receive) = mpsc::channel();
    let (release, wait) = mpsc::channel();
    let (prepared, ready) = mpsc::channel::<Result<(PathBuf, Arc<Recording>, usize), String>>();
    let handle = std::thread::Builder::new()
        .name("replay-poll-fixture".into())
        .spawn(move || {
            let loaded = Request::Load(path).run(&worker_cancel, &worker_progress);
            let (path, recording) = match loaded {
                Ok(Payload::Loaded(path, recording)) => (path, recording),
                Ok(_) => unreachable!("load requests return a recording"),
                Err(error) => {
                    let _ = prepared.send(Err(error.clone()));
                    let _ = send.send(Err(error));
                    return;
                }
            };
            let result = Request::Process(recording.clone(), 0, options, None)
                .run(&worker_cancel, &worker_progress);
            let readiness = result
                .as_ref()
                .map(|_| (path, recording, worker_progress.load(Ordering::Relaxed)))
                .map_err(Clone::clone);
            let _ = prepared.send(readiness);
            let _ = wait.recv();
            let _ = send.send(result);
        })
        .unwrap();
    let (path, recording, computed_progress) = ready
        .recv_timeout(Duration::from_secs(10))
        .expect("the native replay worker must finish computing")
        .expect("the synthetic recording must parse and compute");
    assert!(computed_progress > 0);
    feature.state.path = Some(path);
    feature.state.recording = Some(recording);
    feature.state.worker = Some(Worker {
        generation: feature.state.generation,
        cancel,
        progress,
        receive,
        handle,
    });
    cx.notify();
    ComputedReplayWork {
        release: Some(release),
        progress: computed_progress,
        _directory: directory,
    }
}
