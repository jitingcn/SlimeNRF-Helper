use super::*;

impl Workbench {
    pub(in crate::application) fn activate_hid_log(&mut self, cx: &mut Context<Self>) {
        if !self.shutting_down {
            self.hid_log.activate(cx);
        }
    }

    pub(in crate::application) fn poll_hid_log(&mut self) -> bool {
        let changed = self.hid_log.poll();
        if self.active_page == FeaturePage::HidLog {
            self.hid_log.refresh_visible();
            changed
        } else {
            false
        }
    }

    pub(in crate::application) fn shutdown_hid_log(&mut self) -> HidLogCleanup {
        self.hid_log.shutdown()
    }

    pub(super) fn choose_hid_file(&mut self, recording: bool, cx: &mut Context<Self>) {
        if self.shutting_down
            || self.hid_log.chooser.is_some()
            || self.hid_log.file_worker.is_some()
            || (recording && !self.hid_log.can_start_recording())
        {
            return;
        }
        // Freeze original reports before the native dialog; never apply display filters.
        let snapshot = if recording {
            Vec::new()
        } else {
            self.hid_log.snapshot()
        };
        let dropped = self.hid_log.dropped;
        let directory = directories::BaseDirs::new()
            .map(|dirs| dirs.home_dir().to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        let filename = format!(
            "hid-{}.hidlog",
            chrono::Local::now().format("%Y%m%d-%H%M%S")
        );
        let dialog = cx.prompt_for_new_path(&directory, Some(&filename));
        self.hid_log.error = None;
        self.hid_log.notice = None;
        self.hid_log.chooser = Some(cx.spawn(async move |this, cx| {
            let result = dialog.await;
            let _ = this.update(cx, |this, cx| {
                this.hid_log.chooser = None;
                if this.shutting_down {
                    return;
                }
                match result {
                    Ok(Ok(Some(path))) => {
                        if recording {
                            this.hid_log.start_recording(path);
                        } else {
                            this.hid_log.save_snapshot(path, snapshot, dropped);
                        }
                    }
                    Ok(Ok(None)) => {}
                    Ok(Err(error)) => this.hid_log.error = Some(error.to_string()),
                    Err(error) => this.hid_log.error = Some(error.to_string()),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    pub(super) fn hid_stop_recording(&mut self, cx: &mut Context<Self>) {
        if self.hid_log.stop_recording() {
            cx.notify();
        }
    }

    pub(super) fn hid_stop_all_events(&mut self, cx: &mut Context<Self>) {
        if self.monitor.stop_all_events() > 0 {
            cx.notify();
        }
    }

    pub(super) fn hid_subscribe_events(&mut self, cx: &mut Context<Self>) {
        if self.shutting_down {
            return;
        }
        let Some(key) = &self.hid_log.watch_receiver else {
            return;
        };
        let Some(receiver) = self
            .inventory
            .receivers
            .iter()
            .find(|receiver| &receiver.key == key)
        else {
            return;
        };
        self.hid_log.error = self.monitor.set_events_enabled(receiver, true).err();
        cx.notify();
    }
}

pub(super) fn saved_notice(summary: &Summary, path: &std::path::Path) -> String {
    rust_i18n::t!(
        "hid.notice.saved",
        reports = summary.reports,
        dropped = summary.dropped,
        path = path.display().to_string()
    )
    .to_string()
}
