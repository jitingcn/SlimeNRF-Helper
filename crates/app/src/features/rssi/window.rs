use crate::{
    i18n::tr,
    rssi_scan::{ScanReport, ScanSnapshot},
};
use gpui_kit::component::TitleBar;
use gpui_kit::{
    App, Bounds, Context, FocusHandle, Task, TitlebarOptions, Window, WindowBounds,
    WindowDecorations, WindowOptions, px, size,
};
use std::sync::Arc;

mod files;
mod plot;
mod view;

/// One delivered receiver observation with its frozen serial and scan identity.
pub(crate) struct LiveScan {
    port: String,
    generation: u64,
    snapshot: ScanSnapshot,
    first_update: bool,
}

impl LiveScan {
    pub(crate) fn first(port: String, generation: u64, snapshot: ScanSnapshot) -> Self {
        Self {
            first_update: true,
            ..Self::continuation(port, generation, snapshot)
        }
    }

    pub(crate) fn continuation(port: String, generation: u64, snapshot: ScanSnapshot) -> Self {
        Self {
            port,
            generation,
            snapshot,
            first_update: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LiveSource {
    port: String,
    generation: u64,
    scan_id: u64,
}

#[derive(Default)]
struct RssiDocument {
    generation: u64,
    live: Option<LiveSource>,
    source: Option<String>,
    chart: Option<Arc<plot::ChartData>>,
    selected_channel: Option<u8>,
    error: Option<String>,
    importing: bool,
    stopped: bool,
}

impl RssiDocument {
    /// Only the first delivered observation may replace another source. Continuing
    /// frames update the retained document, even with no native window open.
    fn accept_live(&mut self, update: LiveScan) -> bool {
        if self.stopped {
            return false;
        }
        let source = LiveSource {
            port: update.port,
            generation: update.generation,
            scan_id: update.snapshot.scan_id,
        };
        let same = self.live.as_ref() == Some(&source);
        if !update.first_update && !same {
            return false;
        }
        let open = update.first_update && !same;
        if open {
            self.generation = self.generation.wrapping_add(1);
            self.importing = false;
        }
        if !same {
            self.source = Some(format!(
                "{} · #{} · {}",
                source.port, source.generation, source.scan_id
            ));
            self.live = Some(source);
        }
        let selection = if same { self.selected_channel } else { None };
        self.set_report(update.snapshot.report, selection);
        self.error = None;
        open
    }

    fn begin_import(&mut self) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.importing = true;
        self.error = None;
        self.generation
    }

    fn finish_import(
        &mut self,
        generation: u64,
        result: Option<Result<(Arc<ScanReport>, String), String>>,
    ) {
        if self.stopped || self.generation != generation {
            return;
        }
        self.importing = false;
        match result {
            Some(Ok((report, source))) => {
                self.live = None;
                self.source = Some(source);
                self.set_report(report, None);
                self.error = None;
            }
            Some(Err(error)) => self.error = Some(error),
            None => {}
        }
    }

    fn set_report(&mut self, report: Arc<ScanReport>, selection: Option<u8>) {
        self.selected_channel = selection
            .filter(|channel| {
                report
                    .samples
                    .binary_search_by_key(channel, |sample| sample.channel)
                    .is_ok()
            })
            .or_else(|| report.samples.first().map(|sample| sample.channel));
        self.chart = Some(Arc::new(plot::ChartData::new(report)));
    }

    fn selected_sample_index(&self) -> Option<usize> {
        let channel = self.selected_channel?;
        self.chart
            .as_ref()?
            .report
            .samples
            .binary_search_by_key(&channel, |sample| sample.channel)
            .ok()
    }

    fn inspect_sample(&mut self, offset: isize) -> bool {
        let Some(index) = self
            .selected_sample_index()
            .and_then(|index| index.checked_add_signed(offset))
        else {
            return false;
        };
        let Some(sample) = self
            .chart
            .as_ref()
            .and_then(|chart| chart.report.samples.get(index))
        else {
            return false;
        };
        self.selected_channel = Some(sample.channel);
        true
    }

    fn stop(&mut self) {
        self.stopped = true;
        self.generation = self.generation.wrapping_add(1);
        self.importing = false;
    }
}

pub(crate) struct RssiWindow {
    document: RssiDocument,
    chooser: Option<Task<()>>,
    focus: FocusHandle,
}

impl RssiWindow {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        Self {
            document: RssiDocument::default(),
            chooser: None,
            focus: cx.focus_handle().tab_stop(false),
        }
    }

    pub(crate) fn window_options(cx: &App) -> WindowOptions {
        let bounds = Bounds::centered(None, size(px(1060.), px(640.)), cx);
        WindowOptions {
            titlebar: Some(TitlebarOptions {
                title: Some(tr("rssi.window.title").into()),
                ..TitleBar::title_bar_options()
            }),
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(560.), px(360.))),
            window_decorations: Some(WindowDecorations::Client),
            ..TitleBar::window_options()
        }
    }

    pub(crate) fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus.focus(window, cx);
    }

    /// Applies an observation and returns whether its first delivery should open the window.
    pub(crate) fn accept_live(
        &mut self,
        scan: LiveScan,
        source_label: String,
        cx: &mut Context<Self>,
    ) -> bool {
        // Ignore unrelated continuing observations without scheduling redraws.
        let previous = self.document.chart.as_ref().map(Arc::as_ptr);
        let open = self.document.accept_live(scan);
        if open {
            self.document.source = Some(source_label);
        }
        if previous != self.document.chart.as_ref().map(Arc::as_ptr) {
            cx.notify();
        }
        open
    }

    pub(crate) fn stop(&mut self) {
        self.document.stop();
        self.chooser = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rssi_scan::{Recommendation, Sample};
    use gpui_kit::{
        self as gpui, AnyWindowHandle, Entity, Pixels, ScrollDelta, TestAppContext, point,
        prelude::*, test::TestWindowExt as _,
    };

    fn report(channel: u8) -> Arc<ScanReport> {
        Arc::new(ScanReport {
            channel_range: (0, 88),
            current_channel: Some(84),
            recommendation: Recommendation::Unknown,
            samples: vec![Sample {
                channel,
                rssi_dbm: -60,
            }],
            excluded_channels: vec![],
        })
    }

    fn update(port: &str, generation: u64, scan_id: u64, first_update: bool) -> LiveScan {
        LiveScan {
            port: port.into(),
            generation,
            snapshot: ScanSnapshot {
                scan_id,
                report: report(86),
            },
            first_update,
        }
    }

    #[test]
    fn sample_inspection_uses_observed_channels_without_wraparound_or_data_copies() {
        let report = crate::rssi_scan::parse_log(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/rssi_scan/fixtures/receiver_scan.log"
        )))
        .unwrap();
        let channels: Vec<_> = report.samples.iter().map(|sample| sample.channel).collect();
        let mut document = RssiDocument::default();
        document.set_report(report.clone(), None);
        assert_eq!(document.selected_channel, Some(0));
        assert!(!document.inspect_sample(-1));
        assert!(document.inspect_sample(1));
        assert_eq!(document.selected_channel, Some(2));
        assert_eq!(
            document.chart.as_ref().unwrap().report.samples
                [document.selected_sample_index().unwrap()]
            .rssi_dbm,
            -22
        );
        assert!(document.inspect_sample(1));
        assert_eq!(
            document.selected_channel,
            Some(6),
            "missing channel 4 is not a sample"
        );
        assert!(document.inspect_sample(-1));
        let mut visited = vec![0, 2];
        while document.inspect_sample(1) {
            visited.push(document.selected_channel.unwrap());
        }
        assert_eq!(visited, channels);
        assert_eq!(document.selected_channel, Some(88));
        assert!(!document.inspect_sample(1));
        assert!(Arc::ptr_eq(
            &report,
            &document.chart.as_ref().unwrap().report
        ));
    }

    #[test]
    fn new_reports_reset_inspection_but_live_continuations_preserve_channel_identity() {
        let mut document = RssiDocument::default();
        document.accept_live(update("A", 1, 1, true));
        assert_eq!(document.selected_channel, Some(86));
        let mut continuation = update("A", 1, 1, false);
        let mut continued_report = (*continuation.snapshot.report).clone();
        continued_report.samples.insert(
            0,
            Sample {
                channel: 2,
                rssi_dbm: -22,
            },
        );
        continuation.snapshot.report = Arc::new(continued_report);
        document.accept_live(continuation);
        assert_eq!(document.selected_channel, Some(86));
        assert_eq!(document.selected_sample_index(), Some(1));
        assert!(document.inspect_sample(-1));
        assert_eq!(document.selected_channel, Some(2));
        document.accept_live(update("A", 1, 2, true));
        assert_eq!(document.selected_channel, Some(86));
        let generation = document.begin_import();
        document.finish_import(generation, Some(Ok((report(88), "scan.log".into()))));
        assert_eq!(document.selected_channel, Some(88));
    }

    #[test]
    fn empty_and_single_sample_reports_have_no_neighboring_measurements() {
        let mut document = RssiDocument::default();
        let mut empty = (*report(2)).clone();
        empty.samples.clear();
        document.set_report(Arc::new(empty), Some(2));
        assert_eq!(document.selected_channel, None);
        assert_eq!(document.selected_sample_index(), None);
        assert!(!document.inspect_sample(-1));
        assert!(!document.inspect_sample(1));
        document.set_report(report(2), None);
        assert_eq!(document.selected_channel, Some(2));
        assert!(!document.inspect_sample(-1));
        assert!(!document.inspect_sample(1));
    }

    #[test]
    fn continuing_scans_never_reopen_or_replace_another_source() {
        let mut document = RssiDocument::default();
        assert!(document.accept_live(update("A", 1, 1, true)));
        // Closing the native window changes no document state.
        assert!(!document.accept_live(update("A", 1, 1, false)));
        assert!(document.accept_live(update("B", 2, 1, true)));
        let shown = document.chart.clone().unwrap();
        assert!(!document.accept_live(update("A", 1, 1, false)));
        assert!(Arc::ptr_eq(&shown, document.chart.as_ref().unwrap()));
        assert!(!document.accept_live(update("B", 1, 1, false)));
        assert!(document.accept_live(update("B", 2, 2, true)));
    }

    #[test]
    fn imported_document_is_not_overwritten_by_old_live_rows() {
        let mut document = RssiDocument::default();
        document.accept_live(update("A", 1, 1, true));
        let generation = document.begin_import();
        assert!(!document.accept_live(update("A", 1, 1, false)));
        document.finish_import(generation, Some(Ok((report(88), "scan.log".into()))));
        let imported = document.chart.clone().unwrap();
        assert!(!document.accept_live(update("A", 1, 1, false)));
        assert!(Arc::ptr_eq(&imported, document.chart.as_ref().unwrap()));
        assert!(document.accept_live(update("A", 1, 2, true)));
    }

    #[test]
    fn cancelled_or_failed_import_keeps_live_rows_received_while_choosing() {
        for result in [None, Some(Err("Cannot read scan log".into()))] {
            let mut document = RssiDocument::default();
            document.accept_live(update("A", 1, 1, true));
            let generation = document.begin_import();
            let mut continuation = update("A", 1, 1, false);
            let mut report = (*continuation.snapshot.report).clone();
            report.samples.push(Sample {
                channel: 88,
                rssi_dbm: -62,
            });
            continuation.snapshot.report = Arc::new(report);
            assert!(!document.accept_live(continuation));
            assert_eq!(document.generation, generation);
            assert!(document.importing);
            assert_eq!(document.chart.as_ref().unwrap().report.samples.len(), 2);
            document.finish_import(generation, result);
            assert!(!document.importing);
            assert_eq!(document.chart.as_ref().unwrap().report.samples.len(), 2);
            assert!(document.live.is_some());
        }
    }

    #[test]
    fn new_scan_and_shutdown_reject_stale_import_completions() {
        let mut document = RssiDocument::default();
        let generation = document.begin_import();
        assert!(document.accept_live(update("A", 1, 1, true)));
        document.finish_import(generation, Some(Ok((report(88), "old.log".into()))));
        assert!(document.live.is_some());
        let generation = document.begin_import();
        document.stop();
        document.finish_import(generation, Some(Ok((report(88), "old.log".into()))));
        assert!(!document.accept_live(update("A", 1, 2, true)));
        assert!(document.live.is_some());
    }

    #[gpui_kit::test]
    fn retained_rssi_content_imports_and_stops(cx: &mut TestAppContext) {
        use gpui_kit::VisualContext as _;

        let _ui_session = crate::test_support::UiTestSession::new(cx);
        let cx = cx.add_empty_window();
        let content = cx.new_window_entity(|_, cx| RssiWindow::new(cx));
        content.update(cx, |this, cx| {
            this.choose_log(cx);
            assert!(this.chooser.is_some());
        });
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("scan.log");
        std::fs::write(
            &path,
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/src/rssi_scan/fixtures/receiver_scan.log"
            )),
        )
        .unwrap();
        cx.simulate_path_prompt_response(|_| Some(vec![path.clone()]));
        crate::sessions::test_support::until(|| {
            cx.run_until_parked();
            content.update(cx, |this, _| this.chooser.is_none())
        });
        content.update(cx, |this, _| {
            assert!(!this.document.importing);
            assert!(this.document.error.is_none());
            assert!(this.document.chart.is_some());
            assert_eq!(this.document.source.as_deref(), Some("scan.log"));
        });
        let retained = content.update(cx, |this, cx| {
            this.choose_log(cx);
            assert!(this.chooser.is_some());
            this.document.chart.clone().unwrap()
        });
        content.update(cx, |this, _| this.stop());
        cx.simulate_path_prompt_response(|_| Some(vec![path.clone()]));
        cx.run_until_parked();

        content.update(cx, |this, cx| {
            assert!(this.document.stopped);
            assert!(Arc::ptr_eq(
                this.document.chart.as_ref().unwrap(),
                &retained
            ));
            this.choose_log(cx);
            assert!(this.chooser.is_none());
        });
    }

    const LAYOUT_SCAN: &str = concat!(
        "RSSI scan (ESB channels 0..88): samples/ch/sweep=50, sweeps=200 (total=10000 samples/ch)\n",
        "Current channel (effective): 84\n",
        "Recommended: channel 86 (-69 dBm)\n",
        "All channels sorted by RSSI (lower is better):\n",
        "0 (-42 dBm) 2 (-22 dBm) 84 (-61 dBm) 86 (-69 dBm) 88 (-66 dBm)\n",
    );

    struct LayoutFixture {
        handle: AnyWindowHandle,
        content: Entity<RssiWindow>,
    }

    fn layout_fixture(cx: &mut TestAppContext, report: Arc<ScanReport>) -> LayoutFixture {
        let (handle, content) = cx.update(|cx| {
            gpui::open_window(RssiWindow::window_options(cx), cx, |window, cx| {
                cx.new(|cx| {
                    let mut content = RssiWindow::new(cx);
                    content.document.set_report(report, None);
                    content.focus(window, cx);
                    content
                })
            })
            .expect("open RSSI window")
        });
        LayoutFixture { handle, content }
    }

    fn settle_rssi_layout(cx: &mut TestAppContext, handle: AnyWindowHandle) {
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .expect("render RSSI child window");
    }

    fn contained(viewport: Bounds<Pixels>, bounds: Bounds<Pixels>) -> bool {
        bounds.left() >= viewport.left()
            && bounds.right() <= viewport.right()
            && bounds.top() >= viewport.top()
            && bounds.bottom() <= viewport.bottom()
    }

    fn assert_rssi_visible(window: &Window, ids: &[&'static str]) {
        let viewport = window.find("rssi-window-content").bounds();
        for &id in ids {
            let element = window.find(id);
            let bounds = element.bounds();
            assert!(
                element.visible()
                    && bounds.size.width > px(0.)
                    && bounds.size.height > px(0.)
                    && contained(viewport, bounds),
                "{id} must be fully visible in the RSSI viewport: {bounds:?}, viewport {viewport:?}"
            );
        }
    }

    fn assert_unsampled_hover_target(window: &Window, id: &'static str) -> Bounds<Pixels> {
        let chart = window.find("rssi-chart").bounds();
        let plot = window.find("rssi-plot-area").bounds();
        let target = window.find(id);
        let bounds = target.bounds();
        assert!(
            target.visible()
                && bounds.size.width > px(0.)
                && bounds.size.height > px(0.)
                && contained(chart, plot)
                && contained(plot, bounds)
                && bounds.top() - plot.top() <= px(2.)
                && plot.bottom() - bounds.bottom() <= px(2.),
            "{id} hover target must span the plot without leaving its bounds: {bounds:?}, plot {plot:?}"
        );
        bounds
    }

    fn reveal_rssi(
        cx: &mut TestAppContext,
        handle: AnyWindowHandle,
        id: &'static str,
        down_if_absent: bool,
    ) -> usize {
        for steps in 0..64 {
            let visible = cx
                .update_window(handle, |_, window, cx| {
                    window.render_frame(cx);
                    let viewport = window.find("rssi-window-content").bounds();
                    let delta = if let Some(element) = window.try_find(id) {
                        let bounds = element.bounds();
                        assert!(
                            bounds.size.width > px(0.)
                                && bounds.size.height > px(0.)
                                && bounds.left() >= viewport.left()
                                && bounds.right() <= viewport.right(),
                            "{id} must not be horizontally clipped: {bounds:?}, viewport {viewport:?}"
                        );
                        if contained(viewport, bounds) {
                            assert!(element.visible(), "{id} is clipped by an ancestor");
                            return true;
                        }
                        if bounds.top() < viewport.top() {
                            viewport.top() - bounds.top() + px(2.)
                        } else {
                            viewport.bottom() - bounds.bottom() - px(2.)
                        }
                    } else if down_if_absent {
                        -viewport.size.height * 0.5
                    } else {
                        viewport.size.height * 0.5
                    };
                    window.scroll(
                        "rssi-window-content",
                        ScrollDelta::Pixels(point(px(0.), delta)),
                        cx,
                    );
                    false
                })
                .expect("scroll RSSI child window");
            cx.run_until_parked();
            if visible {
                return steps;
            }
        }
        panic!("scrolling did not reveal {id} within the RSSI viewport");
    }

    #[gpui_kit::test]
    fn rssi_child_layout_fits_default_sizes_and_grows_on_resize(cx: &mut TestAppContext) {
        let _ui_session = crate::test_support::UiTestSession::new(cx);
        let report = crate::rssi_scan::parse_log(LAYOUT_SCAN).expect("parse synthetic scan");
        assert_eq!(report.current_channel, Some(84));
        assert_eq!(
            report.recommendation,
            Recommendation::Channel(Sample {
                channel: 86,
                rssi_dbm: -69,
            })
        );
        let fixture = layout_fixture(cx, report);
        let mut short_plot_height = px(0.);

        for (width, height) in [(1114., 658.), (1060., 640.)] {
            cx.simulate_window_resize(fixture.handle, size(px(width), px(height)));
            settle_rssi_layout(cx, fixture.handle);
            short_plot_height = cx
                .update_window(fixture.handle, |_, window, cx| {
                    assert!(window.try_find("rssi-source").is_none());
                    assert_rssi_visible(
                        window,
                        &[
                            "rssi-import",
                            "rssi-no-sample-legend",
                            "rssi-inspector",
                            "rssi-previous-sample",
                            "rssi-next-sample",
                            "rssi-chart",
                            "rssi-plot-area",
                            "rssi-chart-axis-channel",
                            "rssi-chart-hint",
                            "rssi-sample-2",
                            "rssi-unsampled-4",
                            "rssi-unsampled-6",
                        ],
                    );
                    let legend = window.find("rssi-no-sample-legend").bounds();
                    let inspector = window.find("rssi-inspector").bounds();
                    let chart = window.find("rssi-chart").bounds();
                    assert!(
                        legend.bottom() <= inspector.top() && inspector.bottom() <= chart.top(),
                        "legend, inspector and chart must not overlap: \
                         {legend:?}, {inspector:?}, {chart:?}"
                    );
                    assert!(window.try_find("rssi-unsampled-2").is_none());
                    assert!(window.try_find("rssi-sample-4").is_none());
                    assert!(window.try_find("rssi-sample-6").is_none());
                    let mut previous = ("rssi-sample-2", window.find("rssi-sample-2").bounds());
                    for id in [
                        "rssi-unsampled-3",
                        "rssi-unsampled-4",
                        "rssi-unsampled-5",
                        "rssi-unsampled-6",
                    ] {
                        let target = assert_unsampled_hover_target(window, id);
                        assert!(
                            previous.1.right() < target.left(),
                            "adjacent channels {} and {id} must retain clear spacing: \
                             {:?}, {target:?}",
                            previous.0,
                            previous.1
                        );
                        previous = (id, target);
                    }
                    let plot_height = window.find("rssi-plot-area").bounds().size.height;
                    window.click("rssi-next-sample", cx);
                    assert_eq!(fixture.content.read(cx).document.selected_channel, Some(2));
                    window.click("rssi-next-sample", cx);
                    assert_eq!(fixture.content.read(cx).document.selected_channel, Some(84));
                    window.click("rssi-previous-sample", cx);
                    assert_eq!(fixture.content.read(cx).document.selected_channel, Some(2));
                    window.click("rssi-previous-sample", cx);
                    assert_eq!(fixture.content.read(cx).document.selected_channel, Some(0));
                    plot_height
                })
                .expect("inspect normal RSSI layout");
        }

        cx.simulate_window_resize(fixture.handle, size(px(1060.), px(900.)));
        settle_rssi_layout(cx, fixture.handle);
        cx.update_window(fixture.handle, |_, window, _| {
            assert_rssi_visible(
                window,
                &[
                    "rssi-inspector",
                    "rssi-no-sample-legend",
                    "rssi-chart",
                    "rssi-plot-area",
                    "rssi-chart-axis-channel",
                    "rssi-chart-hint",
                ],
            );
            assert!(
                window.find("rssi-plot-area").bounds().size.height > short_plot_height,
                "a taller window must give its surplus height to the plot"
            );
            let bar = window.find("rssi-sample-2");
            assert!(
                bar.visible() && bar.bounds().size.height > px(8.),
                "the observed sample must have a real bar interior"
            );
            assert!(
                bar.bounds().size.height > window.find("rssi-sample-86").bounds().size.height,
                "the stronger measurement must have the taller bar"
            );
            assert_unsampled_hover_target(window, "rssi-unsampled-4");
            assert_unsampled_hover_target(window, "rssi-unsampled-6");
            assert!(window.try_find("rssi-unsampled-2").is_none());
            assert!(
                window.try_find("rssi-sample-4").is_none(),
                "missing channels must not acquire synthetic bars"
            );
        })
        .expect("inspect the resized RSSI plot");
    }

    #[gpui_kit::test]
    fn complete_rssi_range_has_no_missing_legend_or_targets(cx: &mut TestAppContext) {
        let _ui_session = crate::test_support::UiTestSession::new(cx);
        let mut complete = (*report(2)).clone();
        complete.channel_range = (2, 3);
        complete.current_channel = Some(2);
        complete.samples.push(Sample {
            channel: 3,
            rssi_dbm: -62,
        });
        let fixture = layout_fixture(cx, Arc::new(complete));
        cx.simulate_window_resize(fixture.handle, size(px(1060.), px(640.)));
        settle_rssi_layout(cx, fixture.handle);

        cx.update_window(fixture.handle, |_, window, _| {
            assert_rssi_visible(
                window,
                &["rssi-plot-area", "rssi-sample-2", "rssi-sample-3"],
            );
            assert!(window.try_find("rssi-no-sample-legend").is_none());
            for id in [
                "rssi-unsampled-1",
                "rssi-unsampled-2",
                "rssi-unsampled-3",
                "rssi-unsampled-4",
            ] {
                assert!(
                    window.try_find(id).is_none(),
                    "complete range must not show {id}"
                );
            }
        })
        .expect("inspect the complete RSSI range");
    }

    #[gpui_kit::test]
    fn live_rssi_continuation_preserves_selection_and_plot_bounds(cx: &mut TestAppContext) {
        let _ui_session = crate::test_support::UiTestSession::new(cx);
        let mut decoder = crate::rssi_scan::ScanDecoder::new();
        let initial = LAYOUT_SCAN
            .lines()
            .filter_map(|line| decoder.feed_line(line))
            .last()
            .expect("decode initial scan");
        let extended = decoder
            .feed_line("6 (-127 dBm)")
            .expect("decode the continuing measurement");
        assert_eq!(extended.scan_id, initial.scan_id);
        let weak_sample = Sample {
            channel: 6,
            rssi_dbm: -127,
        };
        let mut expected_samples = initial.report.samples.clone();
        let insertion = expected_samples
            .binary_search_by_key(&weak_sample.channel, |sample| sample.channel)
            .expect_err("channel 6 was not observed in the initial scan");
        expected_samples.insert(insertion, weak_sample);
        assert_eq!(extended.report.samples, expected_samples);
        assert_eq!(
            extended.report.current_channel,
            initial.report.current_channel
        );
        assert_eq!(
            extended.report.recommendation,
            initial.report.recommendation
        );
        let inspected_sample = |document: &RssiDocument| {
            document.chart.as_ref().expect("live chart").report.samples
                [document.selected_sample_index().expect("inspected sample")]
        };
        let retained_sample = Sample {
            channel: 2,
            rssi_dbm: -22,
        };
        let fixture = layout_fixture(cx, initial.report.clone());
        cx.simulate_window_resize(fixture.handle, size(px(1060.), px(640.)));
        cx.update(|cx| {
            fixture.content.update(cx, |this, cx| {
                let mut first = update("synthetic-port", 1, 1, true);
                first.snapshot = initial;
                assert!(this.document.accept_live(first));
                cx.notify();
            });
        });
        settle_rssi_layout(cx, fixture.handle);
        let (plot_before, bar_before, missing_before) = cx
            .update_window(fixture.handle, |_, window, cx| {
                assert_rssi_visible(
                    window,
                    &[
                        "rssi-source",
                        "rssi-import",
                        "rssi-no-sample-legend",
                        "rssi-inspector",
                        "rssi-chart",
                        "rssi-plot-area",
                        "rssi-chart-axis-channel",
                        "rssi-chart-hint",
                        "rssi-sample-2",
                        "rssi-unsampled-4",
                        "rssi-unsampled-6",
                        "rssi-y-tick-0",
                        "rssi-y-tick--20",
                        "rssi-y-tick--40",
                        "rssi-y-tick--60",
                        "rssi-y-tick--80",
                    ],
                );
                assert!(window.try_find("rssi-sample-6").is_none());
                assert!(window.try_find("rssi-sample-4").is_none());
                assert!(window.try_find("rssi-unsampled-2").is_none());
                assert_unsampled_hover_target(window, "rssi-unsampled-4");
                let missing = assert_unsampled_hover_target(window, "rssi-unsampled-6");
                assert!(window.try_find("rssi-y-tick--150").is_none());
                window.press("tab", cx);
                assert_eq!(window.find("rssi-import").focused(), Some(true));
                window.press("tab", cx);
                assert_eq!(window.find("rssi-next-sample").focused(), Some(true));
                window.press("enter", cx);
                assert_eq!(
                    inspected_sample(&fixture.content.read(cx).document),
                    retained_sample
                );
                let plot = window.find("rssi-plot-area").bounds();
                let bar = window.find("rssi-sample-2").bounds();
                assert!(
                    (f32::from(bar.size.height) - f32::from(plot.size.height) * (58. / 80.)).abs()
                        < 3.,
                    "the -22 dBm bar must start at the -80 dBm baseline: {bar:?}, {plot:?}"
                );
                assert!(
                    f32::from(bar.bottom() - plot.bottom()).abs() < 3.,
                    "the bar must reach the plot baseline: {bar:?}, {plot:?}"
                );
                (plot, bar, missing)
            })
            .expect("select an existing live measurement");

        // A receiver continuation adds an observation; it cannot revise a
        // measurement already accepted for this scan.
        cx.update(|cx| {
            fixture.content.update(cx, |this, cx| {
                let mut continuation = update("synthetic-port", 1, 1, false);
                continuation.snapshot = extended;
                assert!(!this.document.accept_live(continuation));
                cx.notify();
            });
        });
        settle_rssi_layout(cx, fixture.handle);
        cx.update_window(fixture.handle, |_, window, cx| {
            assert_eq!(
                window.find("rssi-plot-area").bounds(),
                plot_before,
                "adding a sample without changing natural content dimensions must not resize the plot"
            );
            assert_rssi_visible(
                window,
                &[
                    "rssi-inspector",
                    "rssi-no-sample-legend",
                    "rssi-y-tick-0",
                    "rssi-y-tick--25",
                    "rssi-y-tick--50",
                    "rssi-y-tick--75",
                    "rssi-y-tick--100",
                    "rssi-y-tick--125",
                    "rssi-y-tick--150",
                    "rssi-sample-6",
                    "rssi-unsampled-4",
                ],
            );
            assert!(window.try_find("rssi-y-tick--20").is_none());
            assert!(window.try_find("rssi-y-tick--80").is_none());
            assert!(window.try_find("rssi-sample-4").is_none());
            assert!(window.try_find("rssi-unsampled-2").is_none());
            assert!(
                window.try_find("rssi-unsampled-6").is_none(),
                "a continuing measurement must replace its missing-channel target"
            );
            assert_unsampled_hover_target(window, "rssi-unsampled-4");
            let bar_after = window.find("rssi-sample-2").bounds();
            assert_eq!(bar_after.left(), bar_before.left());
            assert_eq!(bar_after.size.width, bar_before.size.width);
            assert!(
                bar_after.top() < bar_before.top()
                    && bar_after.size.height > bar_before.size.height,
                "the retained -22 dBm bar must grow when the lower bound falls: \
                 {bar_before:?} -> {bar_after:?}"
            );
            assert!(
                (f32::from(bar_after.size.height)
                    - f32::from(plot_before.size.height) * (128. / 150.))
                    .abs()
                    < 3.,
                "the retained bar must start at the -150 dBm baseline: {bar_after:?}"
            );
            let weak_bar = window.find("rssi-sample-6").bounds();
            assert_eq!(weak_bar.left(), missing_before.left());
            assert_eq!(weak_bar.size.width, missing_before.size.width);
            assert!(
                (f32::from(weak_bar.size.height)
                    - f32::from(plot_before.size.height) * (23. / 150.))
                    .abs()
                    < 3.,
                "the new -127 dBm bar must start at the -150 dBm baseline: {weak_bar:?}"
            );
            for bar in [bar_after, weak_bar] {
                assert!(
                    f32::from(bar.bottom() - plot_before.bottom()).abs() < 3.,
                    "each bar must reach the plot baseline: {bar:?}, {plot_before:?}"
                );
            }
            assert_eq!(
                inspected_sample(&fixture.content.read(cx).document),
                retained_sample
            );
            assert_eq!(window.find("rssi-next-sample").focused(), Some(true));
            window.press("space", cx);
            assert_eq!(
                inspected_sample(&fixture.content.read(cx).document),
                weak_sample
            );
            window.press("space", cx);
            assert_eq!(
                inspected_sample(&fixture.content.read(cx).document),
                Sample {
                    channel: 84,
                    rssi_dbm: -61,
                }
            );
            window.press("shift-tab", cx);
            assert_eq!(window.find("rssi-previous-sample").focused(), Some(true));
            window.press("enter", cx);
            assert_eq!(
                inspected_sample(&fixture.content.read(cx).document),
                weak_sample
            );
            window.press("enter", cx);
            assert_eq!(
                inspected_sample(&fixture.content.read(cx).document),
                retained_sample
            );
        })
        .expect("inspect the live continuation");
    }

    #[gpui_kit::test]
    fn rssi_axis_tick_labels_do_not_overlap_at_minimum_zoomed_height(cx: &mut TestAppContext) {
        let _ui_session = crate::test_support::UiTestSession::new(cx);
        let mut log = LAYOUT_SCAN.to_owned();
        for channel in 8..=40 {
            log.push_str(&format!(
                "Channel {channel} invalid: 1/200 visits failed; excluded\n"
            ));
        }
        let report = crate::rssi_scan::parse_log(&log).expect("parse long metadata scan");
        let fixture = layout_fixture(cx, report);
        cx.update(|cx| {
            fixture.content.update(cx, |this, cx| {
                this.document.source = Some(
                    "Synthetic receiver survey with a long descriptive offline scan log filename \
                     for the workshop and office measurements.log"
                        .into(),
                );
                cx.notify();
            });
            gpui_kit::component::Theme::update(cx, |theme| theme.font_size = px(20.));
        });
        cx.simulate_window_resize(fixture.handle, size(px(560.), px(360.)));
        settle_rssi_layout(cx, fixture.handle);
        reveal_rssi(cx, fixture.handle, "rssi-x-tick-0", true);
        reveal_rssi(cx, fixture.handle, "rssi-y-tick-0", false);

        cx.update_window(fixture.handle, |_, window, _| {
            assert_eq!(window.rem_size(), px(20.));
            assert_rssi_visible(
                window,
                &[
                    "rssi-y-tick-0",
                    "rssi-y-tick--20",
                    "rssi-y-tick--40",
                    "rssi-y-tick--60",
                    "rssi-y-tick--80",
                    "rssi-x-tick-0",
                ],
            );
            let ticks = [
                "rssi-y-tick-0",
                "rssi-y-tick--20",
                "rssi-y-tick--40",
                "rssi-y-tick--60",
                "rssi-y-tick--80",
            ];
            for pair in ticks.windows(2) {
                let upper = window.find(pair[0]).bounds();
                let lower = window.find(pair[1]).bounds();
                assert!(
                    upper.bottom() <= lower.top(),
                    "adjacent y ticks {} and {} overlap: {upper:?}, {lower:?}",
                    pair[0],
                    pair[1]
                );
            }
            let bottom = window.find("rssi-y-tick--80").bounds();
            let first = window.find("rssi-x-tick-0").bounds();
            assert!(
                bottom.right() <= first.left()
                    || first.right() <= bottom.left()
                    || bottom.bottom() <= first.top()
                    || first.bottom() <= bottom.top(),
                "bottom y tick and first x tick overlap: {bottom:?}, {first:?}"
            );
        })
        .expect("inspect minimum-height RSSI axis labels");
    }

    #[gpui_kit::test]
    fn rssi_small_zoomed_window_scrolls_to_controls_in_both_locales(cx: &mut TestAppContext) {
        let _ui_session = crate::test_support::UiTestSession::new(cx);
        let mut log = LAYOUT_SCAN.to_owned();
        for channel in 8..=40 {
            log.push_str(&format!(
                "Channel {channel} invalid: 1/200 visits failed; excluded\n"
            ));
        }
        let report = crate::rssi_scan::parse_log(&log).expect("parse long metadata scan");
        assert_eq!(report.excluded_channels.len(), 33);
        let fixture = layout_fixture(cx, report);
        cx.update(|cx| {
            fixture.content.update(cx, |this, cx| {
                this.document.source = Some(
                    "Synthetic receiver survey with a long descriptive offline scan log filename \
                     for the workshop and office measurements.log"
                        .into(),
                );
                cx.notify();
            });
        });
        cx.simulate_window_resize(fixture.handle, size(px(560.), px(360.)));

        for (language, import_label) in [
            (
                crate::features::settings::store::Language::English,
                "Import RSSI scan log",
            ),
            (
                crate::features::settings::store::Language::SimplifiedChinese,
                "导入 RSSI 扫描日志",
            ),
        ] {
            cx.update(|cx| {
                crate::features::settings::theme::apply(
                    &crate::features::settings::store::Preferences {
                        language,
                        ..Default::default()
                    },
                    cx,
                );
                gpui_kit::component::Theme::update(cx, |theme| theme.font_size = px(20.));
            });
            settle_rssi_layout(cx, fixture.handle);
            cx.update_window(fixture.handle, |_, window, cx| {
                window.scroll(
                    "rssi-window-content",
                    ScrollDelta::Pixels(point(px(0.), px(10_000.))),
                    cx,
                );
            })
            .expect("return RSSI viewport to its start");
            settle_rssi_layout(cx, fixture.handle);
            reveal_rssi(cx, fixture.handle, "rssi-import", false);
            cx.update_window(fixture.handle, |_, window, cx| {
                assert_eq!(window.rem_size(), px(20.));
                assert_rssi_visible(window, &["rssi-import"]);
                assert_eq!(window.find("rssi-import").label(), Some(import_label));
                let viewport = window.find("rssi-window-content").bounds();
                assert!(
                    window.try_find("rssi-chart-hint").is_none_or(|hint| {
                        !hint.visible() || !contained(viewport, hint.bounds())
                    }),
                    "the small zoomed window must require the scroll fallback"
                );
                window.click("rssi-import", cx);
                assert!(fixture.content.read(cx).chooser.is_some());
            })
            .expect("activate the visible import action");
            assert!(cx.did_prompt_for_paths());
            cx.simulate_path_prompt_response(|_| None);
            cx.run_until_parked();
            cx.update(|cx| {
                let content = fixture.content.read(cx);
                assert!(content.chooser.is_none());
                assert!(!content.document.importing);
            });
            settle_rssi_layout(cx, fixture.handle);
            reveal_rssi(cx, fixture.handle, "rssi-source", true);
            reveal_rssi(cx, fixture.handle, "rssi-no-sample-legend", true);
            assert!(
                reveal_rssi(cx, fixture.handle, "rssi-chart-hint", true) > 0,
                "wheel input must reveal the hint below the initial viewport"
            );
            reveal_rssi(cx, fixture.handle, "rssi-chart-axis-channel", false);
            reveal_rssi(cx, fixture.handle, "rssi-inspector", false);
            reveal_rssi(cx, fixture.handle, "rssi-next-sample", false);
            cx.update_window(fixture.handle, |_, window, cx| {
                assert_rssi_visible(window, &["rssi-next-sample"]);
                window.click("rssi-next-sample", cx);
                assert_eq!(fixture.content.read(cx).document.selected_channel, Some(2));
            })
            .expect("inspect the next sample after scrolling");
            reveal_rssi(cx, fixture.handle, "rssi-previous-sample", false);
            cx.update_window(fixture.handle, |_, window, cx| {
                assert_rssi_visible(window, &["rssi-previous-sample"]);
                window.click("rssi-previous-sample", cx);
                assert_eq!(fixture.content.read(cx).document.selected_channel, Some(0));
            })
            .expect("inspect the previous sample after scrolling");
        }
    }
}
