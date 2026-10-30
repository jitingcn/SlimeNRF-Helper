use super::Workbench;
use crate::{
    features::rssi::{LiveScan, RssiWindow},
    i18n::tr,
};
use gpui_kit::{self as gpui, AnyWindowHandle, Context, Entity, Subscription, prelude::*};

#[derive(Default)]
pub(super) struct RssiWindowState {
    content: Option<Entity<RssiWindow>>,
    window: Option<AnyWindowHandle>,
    close_observer: Option<Subscription>,
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
        match gpui::open_window(RssiWindow::window_options(cx), cx, move |window, cx| {
            content.update(cx, |this, cx| this.focus(window, cx));
            content
        }) {
            Ok((handle, _)) => {
                self.rssi.window = Some(handle);
                let _ = handle.update(cx, |_, window, _| window.activate_window());
            }
            Err(error) => {
                self.error = Some(format!("{}: {error}", tr("rssi.window.error.open")).into());
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
            let scan = if update.first_update {
                LiveScan::first(update.port, update.generation, update.snapshot)
            } else {
                LiveScan::continuation(update.port, update.generation, update.snapshot)
            };
            let content = self.rssi_content(cx);
            let open = content.update(cx, |this, cx| this.accept_live(scan, source, cx));
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
            content.update(cx, |this, _| this.stop());
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
    use crate::rssi_scan::{Sample, ScanSnapshot};
    use gpui_kit::{TestAppContext, Window, WindowOptions, test::TestWindowExt as _};

    fn assert_selected_sample(window: &Window, sample: &Sample) {
        let expected = rust_i18n::t!(
            "rssi.inspect.sample",
            channel = sample.channel,
            rssi = sample.rssi_dbm
        )
        .to_string();
        assert_eq!(
            window.find("rssi-selected-sample").label(),
            Some(expected.as_str())
        );
    }

    #[gpui_kit::test]
    fn sample_inspection_has_production_keyboard_entry_and_preserves_reused_window_focus(
        cx: &mut TestAppContext,
    ) {
        let _ui_session = crate::test_support::UiTestSession::new(cx);

        let runtime = crate::app_runtime::AppRuntime::new().expect("create isolated I/O runtime");
        let executor = runtime.executor();
        let (_, workbench) = cx.update(|cx| {
            gpui::open_window(WindowOptions::default(), cx, |window, cx| {
                cx.new(|cx| {
                    crate::application::test_support::offline_workbench(
                        window,
                        cx,
                        crate::sessions::SessionManager::new(),
                        executor,
                    )
                })
            })
            .expect("open offline application")
        });
        let report = crate::rssi_scan::parse_log(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/rssi_scan/fixtures/receiver_scan.log"
        )))
        .unwrap();
        let (handle, content) = cx.update(|cx| {
            workbench.update(cx, |this, cx| {
                let content = this.rssi_content(cx);
                content.update(cx, |this, cx| {
                    assert!(this.accept_live(
                        LiveScan::first(
                            "synthetic-port".into(),
                            1,
                            ScanSnapshot {
                                scan_id: 1,
                                report: report.clone(),
                            },
                        ),
                        "Synthetic receiver".into(),
                        cx,
                    ));
                });
                assert!(this.rssi.window.is_none());
                this.open_rssi_window(cx);
                (this.rssi.window.expect("analysis window opened"), content)
            })
        });
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find("rssi-window").focused(), Some(true));
            window.press("tab", cx);
            assert_eq!(window.find("rssi-import").focused(), Some(true));
            window.press("tab", cx);
            assert_eq!(window.find("rssi-next-sample").focused(), Some(true));
            window.press("enter", cx);
            assert_selected_sample(window, &report.samples[1]);
            window.press("space", cx);
            assert_selected_sample(window, &report.samples[2]);
            window.press("shift-tab", cx);
            assert_eq!(window.find("rssi-previous-sample").focused(), Some(true));
            window.press("enter", cx);
            assert_selected_sample(window, &report.samples[1]);
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
            assert_eq!(window.find("rssi-window").focused(), Some(true));
            assert_selected_sample(window, &report.samples[1]);
            window.press("tab", cx);
            assert_eq!(window.find("rssi-import").focused(), Some(true));
        })
        .unwrap();
        cx.update(|cx| {
            workbench.update(cx, |this, cx| {
                this.shutdown_rssi(cx);
                assert!(this.rssi.content.is_none());
                assert!(this.rssi.window.is_none());
                assert!(this.rssi.close_observer.is_none());
            });
            content.update(cx, |this, cx| {
                assert!(!this.accept_live(
                    LiveScan::first(
                        "synthetic-port".into(),
                        1,
                        ScanSnapshot { scan_id: 2, report },
                    ),
                    "Synthetic receiver".into(),
                    cx,
                ));
            });
        });
    }
}
