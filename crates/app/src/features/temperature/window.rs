use crate::i18n::tr;
use futures_util::{FutureExt as _, future::Shared};
use gpui_kit::component::TitleBar;
use gpui_kit::{
    self as gpui, AnyWindowHandle, App, Bounds, Context, Entity, FocusHandle, Subscription, Task,
    TitlebarOptions, WindowBounds, WindowDecorations, WindowOptions, prelude::*, px, size,
};
use slimenrf_recording::thermal_dump::{self, ThermalDump, ThermalDumpAnalysis};
use std::{rc::Rc, sync::Arc};

mod files;
mod plot;
mod view;

/// A connected tracker snapshot; the application revalidates its frozen generation before writing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TrackerTarget {
    port: String,
    generation: u64,
    label: String,
}

impl TrackerTarget {
    pub(crate) fn new(port: String, generation: u64, label: String) -> Self {
        Self {
            port,
            generation,
            label,
        }
    }
}

/// Identifies one accepted hardware request without retaining its transport or session owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DumpRequest {
    port: String,
    generation: u64,
    request_id: u64,
}

impl DumpRequest {
    pub(crate) fn new(port: String, generation: u64, request_id: u64) -> Self {
        Self {
            port,
            generation,
            request_id,
        }
    }
}

/// Hardware intent only; admission, session writes and cancellation remain application-owned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TemperatureRequest {
    Acquire {
        port: String,
        generation: u64,
    },
    Cancel {
        port: String,
        generation: u64,
        request_id: u64,
    },
}

type RequestHandler = Rc<dyn Fn(TemperatureRequest, &mut App)>;

/// Retains the independent analysis document when its native window is closed.
#[derive(Default)]
pub(crate) struct TemperatureWindowState {
    content: Option<Entity<TemperatureWindow>>,
}

impl TemperatureWindowState {
    pub(crate) fn is_initialized(&self) -> bool {
        self.content.is_some()
    }

    /// Requests are deferred until the view's listener borrow has ended.
    pub(crate) fn initialize(
        &mut self,
        on_request: impl Fn(TemperatureRequest, &mut App) + 'static,
        cx: &mut App,
    ) {
        if self.content.is_some() {
            return;
        }
        let content = cx.new(|cx| TemperatureWindow::new(Rc::new(on_request), cx));
        let weak = content.downgrade();
        let observer = cx.on_window_closed(move |cx, closed_id| {
            let weak = weak.clone();
            cx.defer(move |cx| {
                let _ = weak.update(cx, |this, _| {
                    if this
                        .window
                        .is_some_and(|handle| handle.window_id() == closed_id)
                    {
                        this.window = None;
                    }
                });
            });
        });
        content.update(cx, |this, _| this.close_observer = Some(observer));
        self.content = Some(content);
    }

    pub(crate) fn is_open(&self, cx: &App) -> bool {
        self.content
            .as_ref()
            .is_some_and(|content| content.read(cx).window.is_some())
    }

    pub(crate) fn open(&self, cx: &mut App) -> Result<(), String> {
        let content = self
            .content
            .as_ref()
            .expect("initialize the temperature document before opening its window");
        if let Some(handle) = content.read(cx).window {
            if handle
                .update(cx, |_, window, _| window.activate_window())
                .is_ok()
            {
                return Ok(());
            }
            content.update(cx, |this, _| this.window = None);
        }
        let bounds = Bounds::centered(None, size(px(1480.), px(820.)), cx);
        let opened = content.clone();
        match gpui::open_window(
            WindowOptions {
                titlebar: Some(TitlebarOptions {
                    title: Some(tr("temperature.window.title").into()),
                    ..TitleBar::title_bar_options()
                }),
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(560.), px(360.))),
                window_decorations: Some(WindowDecorations::Client),
                ..TitleBar::window_options()
            },
            cx,
            move |window, cx| {
                opened.update(cx, |this, cx| this.focus.focus(window, cx));
                opened
            },
        ) {
            Ok((handle, _)) => {
                content.update(cx, |this, _| this.window = Some(handle));
                let _ = handle.update(cx, |_, window, _| window.activate_window());
                Ok(())
            }
            Err(error) => {
                let message = format!("{}: {error}", tr("temperature.window.error.open"));
                self.set_error(message.clone(), cx);
                Err(message)
            }
        }
    }

    pub(crate) fn refresh_locale(&self, cx: &mut App) {
        if let Some(content) = &self.content {
            if let Some(handle) = content.read(cx).window {
                let _ = handle.update(cx, |_, window, _| {
                    window.set_window_title(&tr("temperature.window.title"));
                });
            }
            content.update(cx, |_, cx| cx.notify());
        }
    }

    pub(crate) fn publish_devices(
        &self,
        active: Vec<DumpRequest>,
        trackers: Vec<TrackerTarget>,
        cx: &mut App,
    ) {
        if let Some(content) = &self.content {
            content.update(cx, |this, cx| {
                if this.active != active || this.trackers != trackers {
                    this.active = active;
                    this.trackers = trackers;
                    cx.notify();
                }
            });
        }
    }

    pub(crate) fn analyze(&self, dump: ThermalDump, source: String, cx: &mut App) {
        self.content
            .as_ref()
            .expect("initialize the temperature document before delivering a dump")
            .update(cx, |this, cx| this.analyze(dump, source, cx));
    }

    pub(crate) fn set_error(&self, message: String, cx: &mut App) {
        if let Some(content) = &self.content {
            content.update(cx, |this, cx| {
                this.document.error = Some(message);
                cx.notify();
            });
        }
    }

    pub(crate) fn record_acquisition(&self, port: &str, result: Result<(), String>, cx: &mut App) {
        self.content
            .as_ref()
            .expect("initialize the temperature document before recording acquisition")
            .update(cx, |this, cx| {
                match result {
                    Ok(()) => {
                        this.document.error = None;
                        this.document.status =
                            Some(format!("{}: {port}", tr("temperature.dump.status.waiting")));
                    }
                    Err(error) => this.document.error = Some(error),
                }
                cx.notify();
            });
    }

    /// Invalidates callbacks and returns every accepted computation/file join for application cleanup.
    pub(crate) fn shutdown(&mut self, cx: &mut App) -> Vec<Shared<Task<()>>> {
        self.content.take().map_or_else(Vec::new, |content| {
            let (joins, window) = content.update(cx, |this, _| {
                let joins = this.stop();
                this.close_observer = None;
                (joins, this.window.take())
            });
            if let Some(handle) = window {
                let _ = handle.update(cx, |_, window, _| window.remove_window());
            }
            joins
        })
    }
}

#[derive(Default)]
struct TemperatureDocument {
    generation: u64,
    analysis: Option<Arc<ThermalDumpAnalysis>>,
    source: Option<String>,
    range: Option<[f64; 2]>,
    status: Option<String>,
    error: Option<String>,
    busy: bool,
    stopped: bool,
}

impl TemperatureDocument {
    fn begin_analysis(&mut self) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.busy = true;
        self.error = None;
        self.status = None;
        self.generation
    }

    /// Publishes a document only; it has no main-page or terminal action.
    fn accept_analysis(
        &mut self,
        generation: u64,
        analysis: Arc<ThermalDumpAnalysis>,
        source: String,
    ) -> bool {
        if self.stopped || generation != self.generation {
            return false;
        }
        self.range = Some(full_range(&analysis));
        self.analysis = Some(analysis);
        self.source = Some(source);
        self.busy = false;
        self.status = None;
        true
    }

    fn stop(&mut self) {
        self.stopped = true;
        self.generation = self.generation.wrapping_add(1);
        self.busy = false;
    }
}

enum AnalysisInput {
    Dump(ThermalDump),
    File(std::path::PathBuf),
}

struct AnalysisRequest {
    generation: u64,
    source: String,
    input: AnalysisInput,
}

type AnalysisResult = Result<Arc<ThermalDumpAnalysis>, String>;

struct AnalysisWork {
    join: Shared<Task<()>>,
    result: Arc<parking_lot::Mutex<Option<AnalysisResult>>>,
    generation: u64,
    source: String,
}

impl AnalysisWork {
    fn start(request: AnalysisRequest, executor: &gpui::BackgroundExecutor) -> Self {
        let AnalysisRequest {
            generation,
            source,
            input,
        } = request;
        let result = Arc::new(parking_lot::Mutex::new(None));
        let published = result.clone();
        let join = executor
            .spawn(async move {
                let dump = match input {
                    AnalysisInput::Dump(dump) => Ok(dump),
                    AnalysisInput::File(path) => files::load_dump(&path),
                };
                *published.lock() = Some(dump.map(|dump| Arc::new(thermal_dump::analyze(&dump))));
            })
            .shared();
        Self {
            join,
            result,
            generation,
            source,
        }
    }
}

#[derive(Default)]
struct TemperatureWork {
    analysis: Option<AnalysisWork>,
    pending: Option<AnalysisRequest>,
    export: Option<Shared<Task<()>>>,
}

impl TemperatureWork {
    fn queue(&mut self, request: AnalysisRequest) -> Option<AnalysisRequest> {
        if self.analysis.is_some() {
            self.pending = Some(request);
            None
        } else {
            Some(request)
        }
    }

    fn shutdown(&mut self) -> Vec<Shared<Task<()>>> {
        self.pending = None;
        let mut joins = Vec::new();
        if let Some(work) = self.analysis.take() {
            joins.push(work.join);
        }
        joins.extend(self.export.take());
        joins
    }
}

struct TemperatureWindow {
    document: TemperatureDocument,
    active: Vec<DumpRequest>,
    trackers: Vec<TrackerTarget>,
    on_request: RequestHandler,
    window: Option<AnyWindowHandle>,
    close_observer: Option<Subscription>,
    chooser: Option<Task<()>>,
    computation: Option<Task<()>>,
    work: TemperatureWork,
    export: Option<Task<()>>,
    charts: plot::ChartCache,
    focus: FocusHandle,
}

impl TemperatureWindow {
    fn new(on_request: RequestHandler, cx: &mut Context<Self>) -> Self {
        Self {
            document: TemperatureDocument::default(),
            active: Vec::new(),
            trackers: Vec::new(),
            on_request,
            window: None,
            close_observer: None,
            chooser: None,
            computation: None,
            work: TemperatureWork::default(),
            export: None,
            charts: plot::ChartCache::default(),
            focus: cx.focus_handle().tab_stop(false),
        }
    }

    fn analyze(&mut self, dump: ThermalDump, source: String, cx: &mut Context<Self>) {
        if self.document.stopped {
            return;
        }
        self.queue_analysis(AnalysisInput::Dump(dump), source, cx);
    }

    fn queue_analysis(&mut self, input: AnalysisInput, source: String, cx: &mut Context<Self>) {
        if self.document.stopped {
            return;
        }
        let request = AnalysisRequest {
            generation: self.document.begin_analysis(),
            source,
            input,
        };
        if let Some(request) = self.work.queue(request) {
            self.start_analysis(request, cx);
        }
        cx.notify();
    }

    fn start_analysis(&mut self, request: AnalysisRequest, cx: &mut Context<Self>) {
        let work = AnalysisWork::start(request, cx.background_executor());
        let join = work.join.clone();
        self.work.analysis = Some(work);
        self.computation = Some(cx.spawn(async move |this, cx| {
            join.await;
            let _ = this.update(cx, |this, cx| {
                let Some(work) = this.work.analysis.take() else {
                    return;
                };
                let result = work.result.lock().take();
                if let Some(result) = result {
                    match result {
                        Ok(analysis) => {
                            this.document
                                .accept_analysis(work.generation, analysis, work.source);
                        }
                        Err(error)
                            if !this.document.stopped
                                && work.generation == this.document.generation =>
                        {
                            this.document.error = Some(error);
                            this.document.busy = false;
                        }
                        Err(_) => {}
                    }
                }
                this.computation = None;
                if let Some(request) = this.work.pending.take() {
                    this.start_analysis(request, cx);
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn stop(&mut self) -> Vec<Shared<Task<()>>> {
        self.document.stop();
        self.chooser = None;
        self.computation = None;
        self.export = None;
        self.work.shutdown()
    }

    fn navigate(&mut self, zoom: f64, shift: f64, cx: &mut Context<Self>) {
        if let Some(range) = self.document.range {
            self.document.range = Some(moved_range(range, zoom, shift));
        }
        cx.notify();
    }

    fn acquire(&self, port: String, generation: u64, cx: &mut Context<Self>) {
        self.request(TemperatureRequest::Acquire { port, generation }, cx);
    }

    fn cancel(&self, request: DumpRequest, cx: &mut Context<Self>) {
        self.request(
            TemperatureRequest::Cancel {
                port: request.port,
                generation: request.generation,
                request_id: request.request_id,
            },
            cx,
        );
    }

    fn request(&self, request: TemperatureRequest, cx: &mut Context<Self>) {
        if self.document.stopped {
            return;
        }
        let on_request = self.on_request.clone();
        // Delivery can synchronously publish back into this retained document.
        cx.defer(move |cx| on_request(request, cx));
    }
}

fn full_range(analysis: &ThermalDumpAnalysis) -> [f64; 2] {
    analysis
        .temperature_range_c
        .map_or([-6., 6.], |[low, high]| {
            let padding = 6_f64.max(low.abs().max(high.abs()) * 1e-12);
            [low - padding, high + padding]
        })
}

fn moved_range(range: [f64; 2], zoom: f64, shift: f64) -> [f64; 2] {
    let minimum = 0.01_f64.max(range[0].abs().max(range[1].abs()) * 1e-12);
    let width = (range[1] - range[0]).clamp(minimum, 1e40);
    let center = (range[0] + range[1]) * 0.5 + width * shift;
    let half = (width * zoom).clamp(minimum, 1e40) * 0.5;
    [center - half, center + half]
}

#[cfg(test)]
mod tests {
    use super::{
        AnalysisInput, AnalysisRequest, AnalysisWork, DumpRequest, TemperatureDocument,
        TemperatureRequest, TemperatureWindowState, TemperatureWork, TrackerTarget, files,
        full_range, moved_range,
    };
    use crate::test_support::{UiTestSession, background_executor, background_executor_with_gate};
    use futures_util::FutureExt as _;
    use gpui_kit::{AppContext as _, TestAppContext, test::TestWindowExt as _};
    use slimenrf_recording::thermal_dump::{self, ThermalDump, ThermalDumpAnalysis};
    use std::{
        sync::{Arc, mpsc},
        time::Duration,
    };

    fn analysis() -> Arc<ThermalDumpAnalysis> {
        let dump = thermal_dump::parse_text("Dumping 1 collected temperature calibration points:\n7 | 24.00 | 0.01 | 0.02 | 0.03\nEnd of dump. Total points printed: 1\n").unwrap();
        Arc::new(thermal_dump::analyze(&dump))
    }

    #[test]
    fn latest_document_is_accepted_without_any_navigation_action() {
        let analysis = analysis();
        let mut document = TemperatureDocument::default();
        let old = document.begin_analysis();
        let latest = document.begin_analysis();
        assert!(!document.accept_analysis(old, analysis.clone(), "stale".into()));
        assert!(document.busy);
        assert!(document.analysis.is_none());
        assert!(document.accept_analysis(latest, analysis.clone(), "port · #2 · 8".into()));
        assert!(!document.busy);
        assert_eq!(document.source.as_deref(), Some("port · #2 · 8"));
        assert_eq!(document.range, Some([18., 30.]));
        assert!(Arc::ptr_eq(document.analysis.as_ref().unwrap(), &analysis));
        assert!(!document.accept_analysis(
            old,
            Arc::new(thermal_dump::analyze(&ThermalDump::default())),
            "stale".into()
        ));
        assert!(Arc::ptr_eq(document.analysis.as_ref().unwrap(), &analysis));
    }

    #[test]
    fn shutdown_invalidates_callbacks_and_retains_last_document() {
        let analysis = analysis();
        let mut document = TemperatureDocument::default();
        let first = document.begin_analysis();
        assert!(document.accept_analysis(first, analysis.clone(), "retained".into()));
        document.range = Some([20., 22.]);
        let pending = document.begin_analysis();
        document.stop();
        assert!(!document.accept_analysis(
            pending,
            Arc::new(thermal_dump::analyze(&ThermalDump::default())),
            "late".into()
        ));
        assert!(!document.accept_analysis(
            document.generation,
            analysis.clone(),
            "after stop".into()
        ));
        assert!(Arc::ptr_eq(document.analysis.as_ref().unwrap(), &analysis));
        assert_eq!(document.source.as_deref(), Some("retained"));
        assert_eq!(document.range, Some([20., 22.]));
        assert!(!document.busy);
    }

    #[test]
    fn navigation_changes_real_temperature_viewport_and_reset_domain_is_signed() {
        assert_eq!(moved_range([-12., 12.], 0.5, 0.), [-6., 6.]);
        assert_eq!(moved_range([-12., 12.], 1., 0.25), [-6., 18.]);
        let dump = thermal_dump::parse_text("Dumping 1 collected temperature calibration points:\n0 | -4.00 | 0.01 | 0.02 | 0.03\nEnd of dump. Total points printed: 1\n").unwrap();
        assert_eq!(full_range(&thermal_dump::analyze(&dump)), [-10., 2.]);
        let huge = moved_range([1e30 - 1e20, 1e30 + 1e20], 0.5, 0.);
        assert!(huge[0].is_finite() && huge[1].is_finite() && huge[0] < huge[1]);
    }

    fn request(generation: u64, temperature: i32) -> AnalysisRequest {
        let text = format!(
            "Dumping 1 collected temperature calibration points:\n0 | {temperature} | 0.01 | 0.02 | 0.03\nEnd of dump. Total points printed: 1\n"
        );
        AnalysisRequest {
            generation,
            source: format!("fixture-{generation}"),
            input: AnalysisInput::Dump(thermal_dump::parse_text(&text).unwrap()),
        }
    }

    #[test]
    fn running_analysis_survives_completion_drop_and_keeps_only_latest_pending() {
        let (release, gate) = mpsc::channel();
        let executor = background_executor_with_gate(Some(gate));
        let mut work = TemperatureWork {
            analysis: Some(AnalysisWork::start(request(1, 10), &executor)),
            ..Default::default()
        };
        let join = work.analysis.as_ref().unwrap().join.clone();
        let completion = executor.spawn(join);
        drop(completion);
        assert!(work.queue(request(2, 20)).is_none());
        assert!(work.queue(request(3, 30)).is_none());
        release.send(()).unwrap();
        let first = work.analysis.take().unwrap();
        join_background(first.join);
        let first_result = first.result.lock().take().unwrap().unwrap();
        assert_eq!(first_result.temperature_range_c, Some([10., 10.]));
        assert_eq!(first.source, "fixture-1");
        let latest = AnalysisWork::start(work.pending.take().unwrap(), &executor);
        join_background(latest.join);
        assert_eq!(latest.source, "fixture-3");
        assert_eq!(latest.generation, 3);
        assert_eq!(
            latest
                .result
                .lock()
                .take()
                .unwrap()
                .unwrap()
                .temperature_range_c,
            Some([30., 30.])
        );
    }

    #[test]
    fn shutdown_joins_started_analysis_and_svg_without_waiting_for_ui_completion() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("accepted.svg");
        let (release_analysis, analysis_gate) = mpsc::channel();
        let analysis_executor = background_executor_with_gate(Some(analysis_gate));
        let mut work = TemperatureWork {
            analysis: Some(AnalysisWork::start(request(1, -4), &analysis_executor)),
            ..Default::default()
        };
        let result = work.analysis.as_ref().unwrap().result.clone();
        assert!(work.queue(request(2, 99)).is_none());
        let (release_export, export_gate) = mpsc::channel();
        let executor = background_executor();
        let target = path.clone();
        let frozen = analysis();
        let (saved, saved_result) = mpsc::channel();
        let export = executor
            .spawn(async move {
                export_gate.recv().unwrap();
                saved
                    .send(files::save_svg(target, &frozen, [20., 22.]))
                    .unwrap();
            })
            .shared();
        work.export = Some(export.clone());
        let completion = executor.spawn(export);
        drop(completion);
        let joins = work.shutdown();
        assert_eq!(joins.len(), 2);
        assert!(work.pending.is_none());
        let (finished, completion) = mpsc::channel();
        let waiter = std::thread::spawn(move || {
            for join in joins {
                join_background(join);
            }
            finished.send(()).unwrap();
        });
        assert!(matches!(
            completion.recv_timeout(Duration::from_millis(30)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        release_analysis.send(()).unwrap();
        release_export.send(()).unwrap();
        completion.recv_timeout(Duration::from_secs(2)).unwrap();
        waiter.join().unwrap();
        assert_eq!(saved_result.recv().unwrap().unwrap(), path);
        assert_eq!(
            result.lock().take().unwrap().unwrap().temperature_range_c,
            Some([-4., -4.])
        );
        let svg = std::fs::read_to_string(path).unwrap();
        assert!(svg.starts_with("<svg "));
        assert!(svg.ends_with("</svg>"));
    }
    fn join_background(task: futures_util::future::Shared<gpui_kit::Task<()>>) {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("create independent test future executor")
            .block_on(task);
    }

    fn until(mut predicate: impl FnMut() -> bool) {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !predicate() {
            assert!(
                std::time::Instant::now() < deadline,
                "temperature operation timed out"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[gpui_kit::test]
    fn retained_temperature_content_imports_exports_and_stops(cx: &mut TestAppContext) {
        let _ui_session = UiTestSession::new(cx);
        let mut temperature = TemperatureWindowState::default();
        cx.update(|cx| temperature.initialize(|_, _| {}, cx));
        let content = temperature.content.clone().unwrap();
        let app = cx.clone();
        let cx = cx.add_empty_window();
        content.update(cx, |this, cx| {
            this.choose_dump(cx);
            assert!(this.chooser.is_some());
            assert!(this.window.is_none());
        });
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("dump.txt");
        std::fs::write(
            &path,
            include_str!("../../../../recording/src/thermal_dump/fixtures/signed_sparse.txt"),
        )
        .unwrap();
        cx.simulate_path_prompt_response(|_| Some(vec![path.clone()]));
        until(|| {
            cx.run_until_parked();
            content.update(cx, |this, _| {
                this.chooser.is_none() && this.work.analysis.is_none() && !this.document.busy
            })
        });
        content.update(cx, |this, _| {
            assert!(this.document.error.is_none());
            assert!(this.document.analysis.is_some());
            let source = path.display().to_string();
            assert_eq!(this.document.source.as_deref(), Some(source.as_str()));
        });
        content.update(cx, |this, cx| {
            this.export_svg(cx);
            assert!(this.export.is_some());
        });
        let svg = directory.path().join("analysis.svg");
        cx.simulate_new_path_selection(|_| Some(svg.clone()));
        until(|| {
            cx.run_until_parked();
            content.update(cx, |this, _| {
                this.export.is_none() && this.work.export.is_none()
            })
        });
        assert!(std::fs::read_to_string(svg).unwrap().starts_with("<svg "));
        let retained = content.update(cx, |this, cx| {
            this.choose_dump(cx);
            assert!(this.chooser.is_some());
            this.document.analysis.clone().unwrap()
        });
        let joins = app.update(|cx| temperature.shutdown(cx));
        assert!(joins.is_empty());
        cx.simulate_path_prompt_response(|_| Some(vec![path.clone()]));
        cx.run_until_parked();
        content.update(cx, |this, cx| {
            assert!(this.document.stopped);
            assert!(Arc::ptr_eq(
                this.document.analysis.as_ref().unwrap(),
                &retained
            ));
            this.choose_dump(cx);
            this.export_svg(cx);
            assert!(this.chooser.is_none());
            assert!(this.export.is_none());
        });
    }

    #[gpui_kit::test]
    fn temperature_navigation_has_production_keyboard_entry_and_preserves_reused_window_focus(
        cx: &mut TestAppContext,
    ) {
        let _ui_session = UiTestSession::new(cx);
        let dump = thermal_dump::parse_text(include_str!(
            "../../../../recording/src/thermal_dump/fixtures/signed_sparse.txt"
        ))
        .unwrap();
        let analysis = Arc::new(thermal_dump::analyze(&dump));
        let original_range = full_range(&analysis);
        let mut temperature = TemperatureWindowState::default();
        let (handle, content) = cx.update(|cx| {
            temperature.initialize(|_, _| {}, cx);
            let content = temperature.content.clone().unwrap();
            content.update(cx, |this, _| {
                let generation = this.document.begin_analysis();
                this.document
                    .accept_analysis(generation, analysis, "signed_sparse.txt".into());
            });
            temperature.open(cx).expect("open analysis window");
            let handle = content.read(cx).window.expect("analysis window opened");
            (handle, content)
        });
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(content.read(cx).focus.is_focused(window));
            window.press("tab", cx);
            assert_eq!(window.find("tcal-import").focused(), Some(true));
            window.press("tab", cx);
            assert_eq!(window.find("tcal-svg").focused(), Some(true));
            window.press("tab", cx);
            let left = window.find("tcal-pan-left");
            assert_eq!(left.focused(), Some(true));
            assert_eq!(
                left.label(),
                Some(crate::i18n::tr("temperature.view.action.lower_temperature").as_str())
            );
            window.press("enter", cx);
            assert_eq!(
                content.read(cx).document.range,
                Some(moved_range(original_range, 1., -0.25))
            );
            for _ in 0..3 {
                window.press("tab", cx);
            }
            let right = window.find("tcal-pan-right");
            assert_eq!(right.focused(), Some(true));
            assert_eq!(
                right.label(),
                Some(crate::i18n::tr("temperature.view.action.higher_temperature").as_str())
            );
            window.press("space", cx);
            assert_eq!(content.read(cx).document.range, Some(original_range));
        })
        .unwrap();
        cx.update(|cx| {
            temperature.open(cx).expect("reuse analysis window");
            assert_eq!(
                content.read(cx).window.unwrap().window_id(),
                handle.window_id()
            );
        });
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find("tcal-pan-right").focused(), Some(true));
            window.remove_window();
        })
        .unwrap();
        cx.run_until_parked();
        cx.update(|cx| {
            assert!(!temperature.is_open(cx));
            let retained = content.read(cx).document.analysis.clone().unwrap();
            let generation = content.read(cx).document.generation;
            temperature
                .open(cx)
                .expect("reopen retained analysis window");
            assert!(Arc::ptr_eq(
                content.read(cx).document.analysis.as_ref().unwrap(),
                &retained,
            ));
            assert_eq!(content.read(cx).document.generation, generation);
            assert_eq!(content.read(cx).document.range, Some(original_range));
            assert!(temperature.shutdown(cx).is_empty());
        });
    }

    #[gpui_kit::test]
    fn hardware_controls_emit_frozen_intent_after_releasing_the_document_borrow(
        cx: &mut TestAppContext,
    ) {
        let _ui_session = UiTestSession::new(cx);
        let requests = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let published = std::rc::Rc::new(std::cell::RefCell::new(
            None::<gpui_kit::WeakEntity<super::TemperatureWindow>>,
        ));
        let mut temperature = TemperatureWindowState::default();
        let (handle, content) = cx.update(|cx| {
            let requests = requests.clone();
            let published_callback = published.clone();
            temperature.initialize(
                move |request, cx| {
                    requests.borrow_mut().push(request);
                    published_callback
                        .borrow()
                        .as_ref()
                        .unwrap()
                        .update(cx, |this, cx| {
                            this.document.status = Some("delivered".into());
                            cx.notify();
                        })
                        .unwrap();
                },
                cx,
            );
            let content = temperature.content.clone().unwrap();
            *published.borrow_mut() = Some(content.downgrade());
            temperature.publish_devices(
                Vec::new(),
                vec![TrackerTarget::new(
                    "fixture-port".into(),
                    73,
                    "Tracker".into(),
                )],
                cx,
            );
            temperature
                .open(cx)
                .expect("open independent analysis window");
            let handle = content.read(cx).window.unwrap();
            (handle, content)
        });
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.click("tcal-read-fixture-port", cx);
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(
            *requests.borrow(),
            vec![TemperatureRequest::Acquire {
                port: "fixture-port".into(),
                generation: 73,
            }],
        );
        cx.update(|cx| {
            assert_eq!(
                content.read(cx).document.status.as_deref(),
                Some("delivered")
            );
            temperature.publish_devices(
                vec![DumpRequest::new("fixture-port".into(), 73, 19)],
                vec![TrackerTarget::new(
                    "fixture-port".into(),
                    73,
                    "Tracker".into(),
                )],
                cx,
            );
        });
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.click("tcal-cancel-fixture-port-19", cx);
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(
            requests.borrow().last(),
            Some(&TemperatureRequest::Cancel {
                port: "fixture-port".into(),
                generation: 73,
                request_id: 19,
            }),
        );
        cx.update(|cx| {
            assert!(temperature.shutdown(cx).is_empty());
            content.update(cx, |this, cx| this.acquire("fixture-port".into(), 73, cx));
        });
        cx.run_until_parked();
        assert_eq!(requests.borrow().len(), 2);
    }
}
