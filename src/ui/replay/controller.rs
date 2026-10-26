use super::*;

impl Workbench {
    pub(in crate::ui) fn replay_open_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.shutting_down {
            return;
        }
        self.replay.recording = None;
        self.replay.processed = None;
        self.replay.settings_open = false;
        self.replay.reference_a = None;
        self.replay.reference_b = None;
        self.replay.auto_attempted = false;
        self.replay.settings_dirty = false;
        self.replay.edit_deadline = None;
        self.replay.hover_time = None;
        self.replay.hover_chart = None;
        self.replay.playback_time = 0.;
        self.replay.details_open = false;
        self.replay.requested_range = None;
        self.replay.path = None;
        self.replay.cursor = 0;
        self.replay.chooser = None;
        self.replay.request(Request::Load(path));
        cx.notify();
    }

    pub(in crate::ui) fn shutdown_replay(&mut self) -> Vec<JoinHandle<()>> {
        self.replay.invalidate();
        self.replay.chooser = None;
        self.replay
            .worker
            .take()
            .map(|worker| worker.handle)
            .into_iter()
            .collect()
    }

    pub(in crate::ui) fn poll_replay(&mut self, cx: &mut Context<Self>) -> bool {
        let mut changed = false;
        if self
            .replay
            .worker
            .as_ref()
            .is_some_and(|worker| worker.handle.is_finished())
        {
            let worker = self.replay.worker.take().expect("finished replay worker");
            let result = if worker.handle.join().is_err() {
                Err(tr("replay.error.worker_stopped"))
            } else {
                worker
                    .receive
                    .try_recv()
                    .unwrap_or_else(|_| Err(tr("replay.error.worker_no_result")))
            };
            if worker.generation == self.replay.generation && !worker.cancel.load(Ordering::Relaxed)
            {
                match result {
                    Ok(Payload::Loaded(path, recording)) => {
                        self.replay.path = Some(path);
                        self.replay.recording = Some(recording);
                        self.replay.track = 0;
                        self.replay.processed = None;
                        self.replay.edit_deadline = Some(Instant::now());
                    }
                    Ok(Payload::Processed(result)) => {
                        if result.result.frames.is_empty() {
                            self.replay.error = Some(tr("replay.error.no_samples"));
                        }
                        let duration = result.result.frames.last().map_or(0., |f| f.time_s);
                        let prior_time = self.replay.playback_time.min(duration);
                        let remap_reference = |index: Option<usize>| {
                            let time = index
                                .and_then(|i| self.replay.processed.as_ref()?.result.frames.get(i))
                                .map(|f| f.time_s)?;
                            (!result.result.frames.is_empty()).then(|| {
                                result
                                    .result
                                    .frames
                                    .partition_point(|f| f.time_s <= time)
                                    .saturating_sub(1)
                            })
                        };
                        let reference_a = remap_reference(self.replay.reference_a);
                        let reference_b = remap_reference(self.replay.reference_b);
                        self.replay.reference_a = reference_a;
                        self.replay.reference_b = reference_b;
                        if !self.replay.auto_attempted {
                            self.replay.auto_attempted = true;
                            match result.summary.auto_references {
                                Ok(pair) => {
                                    self.replay.reference_a = Some(pair.a);
                                    self.replay.reference_b = Some(pair.b);
                                }
                                Err(error) => {
                                    self.replay.notice = Some(tr(auto_reference_error_key(error)));
                                }
                            }
                        }
                        if !result.summary.has_9d {
                            self.replay.nine = false;
                        }
                        self.replay.requested_range = Some(result.range);
                        self.replay.processed = Some(result);
                        self.replay.settings_dirty = false;
                        self.replay.seek(prior_time);
                    }
                    Ok(Payload::Viewport(result)) => {
                        self.replay.requested_range = Some(result.range);
                        self.replay.processed = Some(result);
                    }
                    Ok(Payload::Exported(path)) => {
                        self.replay.notice = Some(format!(
                            "{}: {}",
                            tr("replay.export.success"),
                            path.display()
                        ))
                    }
                    Err(error) => self.replay.error = Some(error),
                }
            }
            self.replay.start_pending();
            changed = true;
        }
        if replay_edit_due(
            self.replay.edit_deadline,
            Instant::now(),
            self.replay.chooser.is_some() || self.replay.presets.importing(),
            self.replay.busy(),
        ) {
            self.replay.edit_deadline = None;
            self.compute_replay(cx);
            changed = true;
        }
        let progress = self
            .replay
            .worker
            .as_ref()
            .filter(|worker| {
                worker.generation == self.replay.generation
                    && !worker.cancel.load(Ordering::Relaxed)
            })
            .map_or(0, |worker| worker.progress.load(Ordering::Relaxed));
        if progress != self.replay.displayed_progress {
            self.replay.displayed_progress = progress;
            changed = true;
        }
        let visible_change = changed && self.active_page == FeaturePage::VqfReplay;
        if changed {
            self.replay.model_dirty = true;
        }
        if visible_change {
            self.sync_replay_model(cx);
            cx.notify();
        }
        visible_change
    }

    pub(in crate::ui) fn hide_replay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.replay.page_visible {
            return;
        }
        self.replay.page_visible = false;
        if self.replay.playing {
            self.replay.pause();
        }
        if let Some(scrub) = self.replay.scrub.take()
            && window.captured_hitbox() == Some(scrub.capture)
        {
            window.release_pointer();
        }
        if let Some(model) = &self.replay.model {
            model.update(cx, |model, _| {
                model.cancel_interaction(window);
            });
        }
    }

    pub(super) fn choose_replay(&mut self, cx: &mut Context<Self>) {
        if self.replay.chooser.is_some() || self.replay.presets.importing() || self.shutting_down {
            return;
        }
        let generation = self.replay.generation;
        let dialog = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(tr("replay.import.prompt").into()),
        });
        self.replay.chooser = Some(cx.spawn(async move |this, cx| {
            let chosen = dialog.await;
            let _ = this.update(cx, |this, cx| {
                this.replay.chooser = None;
                if this.shutting_down || generation != this.replay.generation {
                    return;
                }
                match chosen {
                    Ok(Ok(Some(paths))) => {
                        if let Some(path) = paths.into_iter().next() {
                            this.replay_open_path(path, cx);
                        }
                    }
                    Ok(Err(error)) => this.replay.error = Some(error.to_string()),
                    Err(error) => this.replay.error = Some(error.to_string()),
                    _ => {}
                }
                cx.notify();
            });
        }));
    }

    pub(super) fn export_replay(&mut self, cx: &mut Context<Self>) {
        if self.replay.busy()
            || self.replay.chooser.is_some()
            || self.replay.presets.importing()
            || self.shutting_down
        {
            return;
        }
        let Some(recording) = self.replay.recording.clone() else {
            return;
        };
        let generation = self.replay.generation;
        let directory = self
            .replay
            .path
            .as_ref()
            .and_then(|path| path.parent())
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        let dialog = cx.prompt_for_new_path(&directory, Some("recording.zip"));
        self.replay.chooser = Some(cx.spawn(async move |this, cx| {
            let chosen = dialog.await;
            let _ = this.update(cx, |this, cx| {
                this.replay.chooser = None;
                if this.shutting_down || generation != this.replay.generation {
                    return;
                }
                match chosen {
                    Ok(Ok(Some(path))) => this.replay.request(Request::Export(recording, path)),
                    Ok(Err(error)) => this.replay.error = Some(error.to_string()),
                    Err(error) => this.replay.error = Some(error.to_string()),
                    _ => {}
                }
                cx.notify();
            });
        }));
    }

    pub(super) fn compute_replay(&mut self, cx: &mut Context<Self>) {
        if self.shutting_down {
            return;
        }
        let Some(recording) = self.replay.recording.clone() else {
            return;
        };
        let options = match self.read_replay_options(cx) {
            Ok(options) => options,
            Err(error) => {
                self.replay.error = Some(error);
                self.replay.settings_open = true;
                cx.notify();
                return;
            }
        };
        self.replay.options = options.clone();
        self.replay.edit_deadline = None;
        self.replay.settings_dirty = true;
        let range = self.replay.processed.as_ref().map(|data| data.range);
        self.replay.request(Request::Process(
            recording,
            self.replay.track,
            options,
            range,
        ));
        cx.notify();
    }
}

pub(super) fn replay_edit_due(
    deadline: Option<Instant>,
    now: Instant,
    chooser_open: bool,
    busy: bool,
) -> bool {
    !chooser_open && !busy && deadline.is_some_and(|deadline| now >= deadline)
}
