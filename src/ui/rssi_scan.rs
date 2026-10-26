use super::Workbench;
use crate::{i18n::tr, rssi_scan::ScanReport, sessions::RssiScanUpdate};
use gpui_kit::{
    self as gpui, AnyWindowHandle, Bounds, Context, Entity, FocusHandle, Subscription, Task,
    TitlebarOptions, WindowBounds, WindowOptions, prelude::*, px, size,
};
use std::sync::Arc;

mod files;
mod plot;
mod view;

#[derive(Default)]
pub(super) struct RssiWindowState {
    content: Option<Entity<RssiWindow>>,
    window: Option<AnyWindowHandle>,
    close_observer: Option<Subscription>,
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
    fn accept_live(&mut self, update: RssiScanUpdate) -> bool {
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

struct RssiWindow {
    document: RssiDocument,
    chooser: Option<Task<()>>,
    focus: FocusHandle,
}

impl RssiWindow {
    fn new(cx: &mut Context<Self>) -> Self {
        Self {
            document: RssiDocument::default(),
            chooser: None,
            focus: cx.focus_handle().tab_stop(false),
        }
    }
}

impl Workbench {
    fn rssi_content(&mut self, cx: &mut Context<Self>) -> Entity<RssiWindow> {
        self.rssi
            .content
            .get_or_insert_with(|| cx.new(RssiWindow::new))
            .clone()
    }

    pub(super) fn open_rssi_window(&mut self, cx: &mut Context<Self>) {
        if self.shutting_down {
            return;
        }
        if let Some(handle) = self.rssi.window {
            if handle
                .update(cx, |_, window, _| window.activate_window())
                .is_ok()
            {
                return;
            }
            self.rssi.window = None;
        }
        let content = self.rssi_content(cx);
        if self.rssi.close_observer.is_none() {
            let parent = cx.weak_entity();
            self.rssi.close_observer = Some(cx.on_window_closed(move |cx, closed_id| {
                let parent = parent.clone();
                cx.defer(move |cx| {
                    let _ = parent.update(cx, |this, _| {
                        if this
                            .rssi
                            .window
                            .is_some_and(|handle| handle.window_id() == closed_id)
                        {
                            this.rssi.window = None;
                        }
                    });
                });
            }));
        }
        let bounds = Bounds::centered(None, size(px(1060.), px(640.)), cx);
        match gpui::open_window(
            WindowOptions {
                titlebar: Some(TitlebarOptions {
                    title: Some(tr("rssi.window.title").into()),
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
                self.rssi.window = Some(handle);
                let _ = handle.update(cx, |_, window, _| window.activate_window());
            }
            Err(error) => {
                self.error = Some(format!("{}: {error}", tr("rssi.window.error.open")));
                cx.notify();
            }
        }
    }

    pub(super) fn poll_rssi_scans(&mut self, cx: &mut Context<Self>) -> bool {
        if self.shutting_down {
            return false;
        }
        for update in self.manager.take_rssi_scans() {
            let source = self.serial_generation_label(
                &update.port,
                update.generation,
                format!(
                    "{} · #{} · {}",
                    update.port, update.generation, update.snapshot.scan_id
                ),
            );
            let content = self.rssi_content(cx);
            let open = content.update(cx, |this, cx| {
                // Ignore unrelated continuing observations without scheduling redraws.
                let previous = this.document.chart.clone();
                let open = this.document.accept_live(update);
                if open {
                    this.document.source = Some(source);
                }
                if previous.as_ref().map(Arc::as_ptr)
                    != this.document.chart.as_ref().map(Arc::as_ptr)
                {
                    cx.notify();
                }
                open
            });
            if open {
                self.open_rssi_window(cx);
            }
        }
        false
    }

    pub(super) fn refresh_rssi_locale(&self, cx: &mut Context<Self>) {
        if let Some(handle) = self.rssi.window {
            let _ = handle.update(cx, |_, window, _| {
                window.set_window_title(&tr("rssi.window.title"));
            });
        }
        if let Some(content) = &self.rssi.content {
            content.update(cx, |_, cx| cx.notify());
        }
    }

    pub(super) fn shutdown_rssi(&mut self, cx: &mut Context<Self>) {
        if let Some(content) = self.rssi.content.take() {
            content.update(cx, |this, _| {
                this.document.stop();
                this.chooser = None;
            });
        }
        if let Some(handle) = self.rssi.window.take() {
            let _ = handle.update(cx, |_, window, _| window.remove_window());
        }
        self.rssi.close_observer = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rssi_scan::{Recommendation, Sample, ScanSnapshot};
    use gpui_kit::{TestAppContext, test::TestWindowExt as _};

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

    fn update(port: &str, generation: u64, scan_id: u64, first_update: bool) -> RssiScanUpdate {
        RssiScanUpdate {
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
        let report =
            crate::rssi_scan::parse_log(include_str!("../rssi_scan/fixtures/receiver_scan.log"))
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
    fn sample_inspection_has_production_keyboard_entry_and_preserves_reused_window_focus(
        cx: &mut TestAppContext,
    ) {
        let _ui_session = crate::ui::test_support::UiTestSession::new(cx);

        let runtime = crate::app_runtime::AppRuntime::new().expect("create isolated I/O runtime");
        let executor = runtime.executor();
        let (_, workbench) = cx.update(|cx| {
            gpui::open_window(WindowOptions::default(), cx, |window, cx| {
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
        let report =
            crate::rssi_scan::parse_log(include_str!("../rssi_scan/fixtures/receiver_scan.log"))
                .unwrap();
        let (handle, content) = cx.update(|cx| {
            workbench.update(cx, |this, cx| {
                let content = this.rssi_content(cx);
                content.update(cx, |this, _| this.document.set_report(report, None));
                this.open_rssi_window(cx);
                (this.rssi.window.expect("analysis window opened"), content)
            })
        });
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(content.read(cx).focus.is_focused(window));
            window.press("tab", cx);
            assert_eq!(window.find("rssi-import").focused(), Some(true));
            window.press("tab", cx);
            assert_eq!(window.find("rssi-next-sample").focused(), Some(true));
            window.press("enter", cx);
            assert_eq!(content.read(cx).document.selected_channel, Some(2));
            window.press("space", cx);
            assert_eq!(content.read(cx).document.selected_channel, Some(6));
            window.press("shift-tab", cx);
            assert_eq!(window.find("rssi-previous-sample").focused(), Some(true));
            window.press("enter", cx);
            assert_eq!(content.read(cx).document.selected_channel, Some(2));
        })
        .unwrap();
        cx.update(|cx| {
            workbench.update(cx, |this, cx| {
                this.open_rssi_window(cx);
                assert_eq!(this.rssi.window.unwrap().window_id(), handle.window_id());
            });
        });
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find("rssi-previous-sample").focused(), Some(true));
            window.remove_window();
        })
        .unwrap();
        let reopened = cx.update(|cx| {
            workbench.update(cx, |this, cx| {
                this.open_rssi_window(cx);
                this.rssi.window.expect("analysis window reopened")
            })
        });
        cx.update_window(reopened, |_, window, cx| {
            window.render_frame(cx);
            assert!(content.read(cx).focus.is_focused(window));
            assert_eq!(content.read(cx).document.selected_channel, Some(2));
            window.press("tab", cx);
            assert_eq!(window.find("rssi-import").focused(), Some(true));
        })
        .unwrap();
    }
}
