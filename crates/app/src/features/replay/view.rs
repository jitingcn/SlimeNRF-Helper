use crate::i18n::tr;
use gpui_kit::base::{ResizeHandleRenderer, h_resizable};
use gpui_kit::component::{
    ActiveTheme, Disableable, ResizableState, Selectable, Sizable,
    button::Button,
    checkbox::Checkbox,
    input::{Input, InputEvent, InputState},
    resizable_panel,
    tooltip::Tooltip,
};
use gpui_kit::{self as gpui, prelude::*, *};
use slimenrf_recording::{
    capture::{self, Recording},
    replay::{self, GyroCalibration, ReplayOptions, analysis, presets},
};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};
mod controller;
mod controls;
mod diagnostics;
mod model;
mod parameters;
mod playback;
mod presentation;
mod preset_io;
mod render;
mod scrubbing;

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) mod test_support;

use presentation::{ChartId, PlotOverlay, ReplayViewData};

fn auto_reference_error_key(error: analysis::AutoReferenceError) -> &'static str {
    match error {
        analysis::AutoReferenceError::InvalidTiming => "replay.reference.error.invalid_timing",
        analysis::AutoReferenceError::TooShort => "replay.reference.error.too_short",
        analysis::AutoReferenceError::IndependentSegments => {
            "replay.reference.error.independent_segments"
        }
        analysis::AutoReferenceError::NoInitialRest => "replay.reference.error.no_initial_rest",
        analysis::AutoReferenceError::NoFinalRest => "replay.reference.error.no_final_rest",
        analysis::AutoReferenceError::SameRestRun => "replay.reference.error.same_rest_run",
        analysis::AutoReferenceError::NotOrdered => "replay.reference.error.not_ordered",
        analysis::AutoReferenceError::Cancelled => "replay.reference.error.cancelled",
    }
}

enum Request {
    Load(PathBuf),
    Process(Arc<Recording>, usize, ReplayOptions, Option<(f64, f64)>),
    Export(Arc<Recording>, PathBuf),
    Viewport(
        Arc<replay::ReplayResult>,
        Arc<analysis::AnalysisSummary>,
        (f64, f64),
    ),
}

impl Request {
    fn run(self, cancel: &AtomicBool, progress: &AtomicUsize) -> Result<Payload, String> {
        match self {
            Self::Load(path) => capture::format::load(&path, cancel)
                .map(|recording| Payload::Loaded(path, Arc::new(recording))),
            Self::Process(recording, track, options, range) => {
                replay::process(recording, track, options, cancel, progress)
                    .and_then(|result| ReplayViewData::prepare_range(result, range, cancel))
                    .map(|result| Payload::Processed(Arc::new(result)))
            }
            Self::Export(recording, path) => {
                capture::format::export_csv_zip(&recording, &path, cancel)
                    .map(|()| Payload::Exported(path))
            }
            Self::Viewport(result, summary, range) => {
                ReplayViewData::for_range(result, summary, range, cancel)
                    .map(|view| Payload::Viewport(Arc::new(view)))
            }
        }
    }
}
enum Payload {
    Loaded(PathBuf, Arc<Recording>),
    Processed(Arc<ReplayViewData>),
    Viewport(Arc<ReplayViewData>),
    Exported(PathBuf),
}
struct Worker {
    generation: u64,
    cancel: Arc<AtomicBool>,
    progress: Arc<AtomicUsize>,
    receive: mpsc::Receiver<Result<Payload, String>>,
    handle: JoinHandle<()>,
}

#[derive(Clone, Copy)]
struct ReplayScrub {
    generation: u64,
    range: (f64, f64),
    bounds: Bounds<Pixels>,
    capture: HitboxId,
    zoom_start: Option<f64>,
}

/// Owns replay computation, playback, editing and the retained replay view.
pub(crate) struct ReplayFeature {
    state: ReplayState,
    resize_handle_appearance: ResizeHandleRenderer,
    window: AnyWindowHandle,
    stopped: bool,
}

/// Retained work transferred to ordinary application shutdown.
#[non_exhaustive]
pub(crate) struct ReplayCleanup {
    pub(crate) workers: Vec<JoinHandle<()>>,
    pub(crate) files: Vec<futures_util::future::Shared<Task<()>>>,
}

impl ReplayFeature {
    pub(crate) fn new(window: &mut Window, resize_handle_appearance: ResizeHandleRenderer) -> Self {
        Self {
            state: ReplayState::default(),
            resize_handle_appearance,
            window: window.window_handle(),
            stopped: false,
        }
    }
}

struct ReplayState {
    generation: u64,
    worker: Option<Worker>,
    pending: Option<Request>,
    chooser: Option<Task<()>>,
    recording: Option<Arc<Recording>>,
    path: Option<PathBuf>,
    track: usize,
    processed: Option<Arc<ReplayViewData>>,
    requested_range: Option<(f64, f64)>,
    options: ReplayOptions,
    fields: Vec<Entity<InputState>>,
    settings_open: bool,
    details_open: bool,
    charts: Vec<ChartId>,
    full_scale: bool,
    hover_time: Option<f64>,
    hover_chart: Option<ChartId>,
    reference_a: Option<usize>,
    reference_b: Option<usize>,
    auto_attempted: bool,
    settings_dirty: bool,
    edit_deadline: Option<Instant>,
    field_subscriptions: Vec<Subscription>,
    parameter_group: usize,
    presets: preset_io::PresetIo,
    preset_name: Option<Entity<InputState>>,
    layout: Option<Entity<ResizableState>>,
    focus: Option<FocusHandle>,
    looping: bool,
    nine: bool,
    playing: bool,
    animation_epoch: u64,
    animation_pending: bool,
    model: Option<Entity<model::ModelView>>,
    model_dirty: bool,
    page_visible: bool,
    displayed_progress: usize,
    speed: f64,
    cursor: usize,
    clock: Instant,
    playback_time: f64,
    scrub: Option<ReplayScrub>,
    _scrub_subscriptions: Vec<Subscription>,
    error: Option<String>,
    notice: Option<String>,
    phase: &'static str,
}
impl Default for ReplayState {
    fn default() -> Self {
        Self {
            generation: 0,
            worker: None,
            pending: None,
            chooser: None,
            recording: None,
            path: None,
            track: 0,
            processed: None,
            requested_range: None,
            options: ReplayOptions::default(),
            fields: Vec::new(),
            settings_open: false,
            details_open: false,
            charts: ChartId::ALL.to_vec(),
            full_scale: false,
            hover_time: None,
            hover_chart: None,
            reference_a: None,
            reference_b: None,
            auto_attempted: false,
            settings_dirty: false,
            edit_deadline: None,
            field_subscriptions: Vec::new(),
            parameter_group: 0,
            presets: preset_io::PresetIo::default(),
            preset_name: None,
            layout: None,
            focus: None,
            looping: false,
            nine: true,
            playing: false,
            animation_epoch: 0,
            animation_pending: false,
            model: None,
            model_dirty: true,
            page_visible: false,
            displayed_progress: 0,
            speed: 1.,
            cursor: 0,
            clock: Instant::now(),
            playback_time: 0.,
            scrub: None,
            _scrub_subscriptions: Vec::new(),
            error: None,
            notice: None,
            phase: "replay.phase.loading",
        }
    }
}
impl ReplayState {
    fn pause(&mut self) {
        self.playing = false;
        self.animation_epoch = self.animation_epoch.wrapping_add(1);
        self.animation_pending = false;
    }

    fn invalidate(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        if let Some(worker) = &self.worker {
            worker.cancel.store(true, Ordering::Relaxed);
        }
        self.pending = None;
        self.requested_range = self.processed.as_ref().map(|data| data.range);
        self.pause();
        self.error = None;
        self.notice = None;
    }
    fn request(&mut self, request: Request) {
        self.invalidate();
        self.phase = match &request {
            Request::Load(_) => "replay.phase.loading",
            Request::Process(..) => "replay.phase.computing",
            Request::Export(..) => "replay.phase.exporting",
            Request::Viewport(..) => "replay.phase.viewport",
        };
        self.pending = Some(request);
        self.start_pending();
    }
    fn start_pending(&mut self) {
        if self.worker.is_some() {
            return;
        }
        let Some(request) = self.pending.take() else {
            return;
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let progress = Arc::new(AtomicUsize::new(0));
        let worker_cancel = cancel.clone();
        let worker_progress = progress.clone();
        let (send, receive) = mpsc::channel();
        match std::thread::Builder::new()
            .name("recording-replay".into())
            .spawn(move || {
                let result = request.run(&worker_cancel, &worker_progress);
                let _ = send.send(result);
            }) {
            Ok(handle) => {
                self.worker = Some(Worker {
                    generation: self.generation,
                    cancel,
                    progress,
                    receive,
                    handle,
                })
            }
            Err(error) => self.error = Some(error.to_string()),
        }
    }
    fn busy(&self) -> bool {
        self.worker.is_some() || self.pending.is_some()
    }
    fn seek(&mut self, seconds: f64) {
        if let Some(data) = &self.processed {
            let frames = &data.result.frames;
            if !frames.is_empty() {
                self.cursor = frames
                    .partition_point(|frame| frame.time_s <= seconds)
                    .saturating_sub(1)
                    .min(frames.len() - 1);
                self.playback_time = frames[self.cursor].time_s;
            }
        }
        self.clock = Instant::now();
    }
}
