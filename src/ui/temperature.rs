use super::Workbench;
use crate::{
    commands::{self, CommandContext, CommandSnapshot, RouteEvidence},
    domain::DeviceRole,
    i18n::tr,
    sessions::{ConnectionState, ThermalDumpOutcome, ThermalDumpRequest},
};
use futures_util::{FutureExt as _, future::Shared};
use gpui_kit::{
    self as gpui, AnyWindowHandle, Bounds, Context, Entity, FocusHandle, Subscription, Task,
    TitlebarOptions, WeakEntity, WindowBounds, WindowOptions, prelude::*, px, size,
};
use slimenrf_recording::thermal_dump::{self, ThermalDump, ThermalDumpAnalysis};
use std::sync::Arc;

mod files;
mod plot;
mod view;

/// The native view may close while its document and computation remain alive.
#[derive(Default)]
pub(super) struct TemperatureWindowState {
    content: Option<Entity<TemperatureWindow>>,
    window: Option<AnyWindowHandle>,
    close_observer: Option<Subscription>,
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
    parent: WeakEntity<Workbench>,
    document: TemperatureDocument,
    active: Vec<ThermalDumpRequest>,
    trackers: Vec<(String, u64, String)>,
    chooser: Option<Task<()>>,
    computation: Option<Task<()>>,
    work: TemperatureWork,
    export: Option<Task<()>>,
    charts: plot::ChartCache,
    focus: FocusHandle,
}

impl TemperatureWindow {
    fn new(parent: WeakEntity<Workbench>, cx: &mut Context<Self>) -> Self {
        Self {
            parent,
            document: TemperatureDocument::default(),
            active: Vec::new(),
            trackers: Vec::new(),
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
        let parent = self.parent.clone();
        // Parent publishes back to this entity. End the listener's borrow first.
        cx.defer(move |cx| {
            let _ = parent.update(cx, |this, cx| {
                if !this.shutting_down {
                    this.acquire_temperature_dump(&port, generation, cx);
                }
            });
        });
    }

    fn cancel(&self, request: ThermalDumpRequest, cx: &mut Context<Self>) {
        let parent = self.parent.clone();
        cx.defer(move |cx| {
            let _ = parent.update(cx, |this, cx| {
                if !this.shutting_down {
                    this.manager.cancel_thermal_dump(&request);
                    this.poll_temperature(cx);
                }
            });
        });
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

impl Workbench {
    fn temperature_content(&mut self, cx: &mut Context<Self>) -> Entity<TemperatureWindow> {
        self.temperature
            .content
            .get_or_insert_with(|| {
                let parent = cx.weak_entity();
                cx.new(|cx| TemperatureWindow::new(parent, cx))
            })
            .clone()
    }

    pub(super) fn open_temperature_window(&mut self, cx: &mut Context<Self>) {
        if self.shutting_down {
            return;
        }
        if let Some(handle) = self.temperature.window {
            if handle
                .update(cx, |_, window, _| window.activate_window())
                .is_ok()
            {
                self.publish_temperature_devices(cx);
                return;
            }
            self.temperature.window = None;
        }
        let content = self.temperature_content(cx);
        if self.temperature.close_observer.is_none() {
            let parent = cx.weak_entity();
            self.temperature.close_observer = Some(cx.on_window_closed(move |cx, closed_id| {
                // Also safe when removal originates in parent shutdown.
                let parent = parent.clone();
                cx.defer(move |cx| {
                    let _ = parent.update(cx, |this, _| {
                        if this
                            .temperature
                            .window
                            .is_some_and(|handle| handle.window_id() == closed_id)
                        {
                            this.temperature.window = None;
                        }
                    });
                });
            }));
        }
        let bounds = Bounds::centered(None, size(px(1480.), px(820.)), cx);
        match gpui::open_window(
            WindowOptions {
                titlebar: Some(TitlebarOptions {
                    title: Some(tr("temperature.window.title").into()),
                    ..Default::default()
                }),
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(560.), px(360.))),
                ..Default::default()
            },
            cx,
            move |window, cx| {
                content.update(cx, |this, cx| this.focus.focus(window, cx));
                content
            },
        ) {
            Ok((handle, _)) => {
                self.temperature.window = Some(handle);
                self.publish_temperature_devices(cx);
                let _ = handle.update(cx, |_, window, _| window.activate_window());
            }
            Err(error) => {
                let message = format!("{}: {error}", tr("temperature.window.error.open"));
                if let Some(content) = &self.temperature.content {
                    content.update(cx, |this, cx| {
                        this.document.error = Some(message.clone());
                        cx.notify();
                    });
                }
                self.error = Some(message);
                cx.notify();
            }
        }
    }

    pub(super) fn refresh_temperature_locale(&self, cx: &mut Context<Self>) {
        if let Some(handle) = self.temperature.window {
            let _ = handle.update(cx, |_, window, _| {
                window.set_window_title(&tr("temperature.window.title"));
            });
        }
        if let Some(content) = &self.temperature.content {
            content.update(cx, |_, cx| cx.notify());
        }
    }

    pub(super) fn shutdown_temperature(&mut self, cx: &mut Context<Self>) -> Vec<Shared<Task<()>>> {
        let joins = self
            .temperature
            .content
            .take()
            .map_or_else(Vec::new, |content| {
                content.update(cx, |this, _| this.stop())
            });
        if let Some(handle) = self.temperature.window.take() {
            let _ = handle.update(cx, |_, window, _| window.remove_window());
        }
        self.temperature.close_observer = None;
        joins
    }

    pub(super) fn publish_temperature_devices(&self, cx: &mut Context<Self>) {
        if self.temperature.window.is_none() {
            return;
        }
        let Some(content) = &self.temperature.content else {
            return;
        };
        let active = self.manager.active_thermal_dumps();
        let trackers = self
            .manager
            .sessions()
            .filter(|session| {
                session.state == ConnectionState::Connected
                    && session.identity.role == DeviceRole::Tracker
            })
            .map(|session| {
                (
                    session.port.clone(),
                    session.generation,
                    self.serial_generation_label(
                        &session.port,
                        session.generation,
                        session.port.clone(),
                    ),
                )
            })
            .collect::<Vec<_>>();
        content.update(cx, |this, cx| {
            if this.active != active || this.trackers != trackers {
                this.active = active;
                this.trackers = trackers;
                cx.notify();
            }
        });
    }

    pub(super) fn poll_temperature(&mut self, cx: &mut Context<Self>) -> bool {
        if self.shutting_down {
            return false;
        }
        for event in self.manager.drain_thermal_dump_events() {
            let source = self.serial_generation_label(
                &event.request.port,
                event.request.generation,
                format!(
                    "{} · #{} · {}",
                    event.request.port, event.request.generation, event.request.request_id
                ),
            );
            let content = self.temperature_content(cx);
            match event.outcome {
                ThermalDumpOutcome::Complete(dump) => {
                    content.update(cx, |this, cx| this.analyze(dump, source, cx));
                    self.open_temperature_window(cx);
                }
                outcome => {
                    let message = match outcome {
                        ThermalDumpOutcome::Failed(error) => error.to_string(),
                        ThermalDumpOutcome::WriteFailed(error) => {
                            format!("{}: {error}", tr("temperature.dump.error.write"))
                        }
                        ThermalDumpOutcome::Cancelled => tr("temperature.dump.status.cancelled"),
                        ThermalDumpOutcome::Disconnected => {
                            tr("temperature.dump.error.disconnected")
                        }
                        ThermalDumpOutcome::Handoff => tr("temperature.dump.error.handoff"),
                        ThermalDumpOutcome::TimedOut => tr("temperature.dump.error.timeout"),
                        ThermalDumpOutcome::GenerationReplaced => {
                            tr("temperature.dump.error.connection_changed")
                        }
                        ThermalDumpOutcome::Complete(_) => unreachable!(),
                    };
                    let reconnect = if self
                        .manager
                        .thermal_dump_needs_reconnect(&event.request.port)
                    {
                        format!(" {}", tr("temperature.dump.hint.reconnect"))
                    } else {
                        String::new()
                    };
                    content.update(cx, |this, cx| {
                        this.document.error = Some(format!("{source}: {message}{reconnect}"));
                        cx.notify();
                    });
                    self.open_temperature_window(cx);
                }
            }
        }
        self.publish_temperature_devices(cx);
        // Child redraws are independent of the main page and terminal.
        false
    }

    fn acquire_temperature_dump(&mut self, port: &str, generation: u64, cx: &mut Context<Self>) {
        self.poll_sessions();
        let result = (|| {
            let session = self
                .manager
                .session(port)
                .ok_or_else(|| tr("temperature.dump.error.connect_tracker"))?;
            if session.generation != generation
                || session.state != ConnectionState::Connected
                || session.identity.role != DeviceRole::Tracker
            {
                return Err(tr("temperature.dump.error.target_changed"));
            }
            let preset = commands::presets(CommandContext::Direct(DeviceRole::Tracker))
                .find(|preset| preset.command == "tcal dump")
                .ok_or_else(|| tr("temperature.dump.error.command_unavailable"))?;
            let snapshot = CommandSnapshot::capture(
                preset,
                "",
                RouteEvidence::Direct {
                    port,
                    generation,
                    connected: true,
                    role: session.identity.role,
                },
            )?;
            self.flash_send_line(port, snapshot.command()).map(|_| ())
        })();
        let content = self.temperature_content(cx);
        content.update(cx, |this, cx| {
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
        self.publish_temperature_devices(cx);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AnalysisInput, AnalysisRequest, AnalysisWork, TemperatureDocument, TemperatureWork, files,
        full_range, moved_range,
    };
    use crate::ui::polling_tests::{
        background_executor, background_executor_with_gate, join_background,
    };
    use futures_util::FutureExt as _;
    use gpui_kit::{AppContext as _, TestAppContext, WindowOptions, test::TestWindowExt as _};
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

    #[gpui_kit::test]
    fn temperature_navigation_has_production_keyboard_entry_and_preserves_reused_window_focus(
        cx: &mut TestAppContext,
    ) {
        let _ui_session = crate::ui::test_support::UiTestSession::new(cx);

        let runtime = crate::app_runtime::AppRuntime::new().expect("create isolated I/O runtime");
        let executor = runtime.executor();
        let (_, workbench) = cx.update(|cx| {
            gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
                cx.new(|cx| {
                    crate::ui::test_support::offline_workbench(
                        window,
                        cx,
                        crate::sessions::SessionManager::new(),
                        executor,
                    )
                })
            })
            .expect("open offline application")
        });
        let dump = thermal_dump::parse_text(include_str!(
            "../../crates/recording/src/thermal_dump/fixtures/signed_sparse.txt"
        ))
        .unwrap();
        let analysis = Arc::new(thermal_dump::analyze(&dump));
        let original_range = full_range(&analysis);
        let (handle, content) = cx.update(|cx| {
            workbench.update(cx, |this, cx| {
                let content = this.temperature_content(cx);
                content.update(cx, |this, _| {
                    let generation = this.document.begin_analysis();
                    this.document
                        .accept_analysis(generation, analysis, "signed_sparse.txt".into());
                });
                this.open_temperature_window(cx);
                (
                    this.temperature.window.expect("analysis window opened"),
                    content,
                )
            })
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
            workbench.update(cx, |this, cx| {
                this.open_temperature_window(cx);
                assert_eq!(
                    this.temperature.window.unwrap().window_id(),
                    handle.window_id()
                );
            });
        });
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find("tcal-pan-right").focused(), Some(true));
        })
        .unwrap();
    }
}
