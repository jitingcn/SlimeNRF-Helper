use super::*;

impl Workbench {
    pub(in crate::ui) fn poll_hid_log(&mut self) -> bool {
        let mut changed = false;
        // Bound UI work per tick even if the HID publisher is continuously busy.
        for _ in 0..4096 {
            let Some(item) = self
                .hid_log
                .subscription
                .as_mut()
                .and_then(trace::Subscription::try_recv)
            else {
                break;
            };
            match item {
                TraceItem::Record(record) => self.hid_log.retain(record),
                TraceItem::Gap { count } => {
                    self.hid_log.dropped = self.hid_log.dropped.saturating_add(count)
                }
            }
            changed = true;
        }
        if changed && self.active_page == FeaturePage::HidLog {
            self.hid_log.rebuild();
        }
        changed |= self.hid_log.poll_file_results();
        changed && self.active_page == FeaturePage::HidLog
    }

    pub(in crate::ui) fn shutdown_hid_log(&mut self) -> HidLogCleanup {
        self.hid_log.chooser = None;
        if let Some(subscription) = &mut self.hid_log.subscription {
            subscription.stop();
        }
        if let Some(recorder) = &self.hid_log.recorder {
            recorder.stop();
        }
        HidLogCleanup {
            recorder: self.hid_log.recorder.take(),
            file_worker: self.hid_log.file_worker.take(),
        }
    }

    pub(super) fn hid_fields(&mut self, cx: &mut Context<Self>) {
        if self.hid_log.subscription.is_none() {
            self.hid_log.subscription = Some(trace::subscribe());
        }
        if self.hid_log.layout.is_none() {
            self.hid_log.layout = Some(cx.new(|_| ResizableState::default()));
        }
        if self.hid_log.list_focus.is_none() {
            self.hid_log.list_focus = Some(cx.focus_handle().tab_stop(true));
        }
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
                        if !recording
                            && this
                                .hid_log
                                .recording_path
                                .as_ref()
                                .is_some_and(|active| active == &path)
                        {
                            this.hid_log.error = Some(tr("hid.error.recording_destination"));
                            cx.notify();
                            return;
                        }
                        if recording {
                            // Recorder owns a writer thread; start never opens/writes a file here.
                            match Recorder::start(path.clone()) {
                                Ok(recorder) => {
                                    this.hid_log.recorder = Some(recorder);
                                    this.hid_log.recording_path = Some(path);
                                }
                                Err(error) => this.hid_log.error = Some(error),
                            }
                        } else {
                            let job = std::thread::Builder::new()
                                .name("hid-log-snapshot".into())
                                .spawn(move || {
                                    trace::save_snapshot(&path, &snapshot, dropped)
                                        .map(|summary| (summary, path))
                                });
                            match job {
                                Ok(worker) => this.hid_log.file_worker = Some(worker),
                                Err(error) => this.hid_log.error = Some(error.to_string()),
                            }
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
