use super::*;

impl ReplayFeature {
    pub(crate) fn open_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.stopped {
            return;
        }
        self.state.recording = None;
        self.state.processed = None;
        self.state.settings_open = false;
        self.state.reference_a = None;
        self.state.reference_b = None;
        self.state.auto_attempted = false;
        self.state.settings_dirty = false;
        self.state.edit_deadline = None;
        self.state.hover_time = None;
        self.state.hover_chart = None;
        self.state.playback_time = 0.;
        self.state.details_open = false;
        self.state.requested_range = None;
        self.state.path = None;
        self.state.cursor = 0;
        self.state.chooser = None;
        self.state.request(Request::Load(path));
        cx.notify();
    }

    pub(crate) fn shutdown(&mut self, cx: &mut Context<Self>) -> ReplayCleanup {
        self.stopped = true;
        self.state.page_visible = false;
        self.state.invalidate();
        self.state.chooser = None;
        // Quit callbacks need not have a Window. Release window-owned captures
        // after the current update, rather than re-entering its window lease.
        let scrub = self.state.scrub.take();
        let model = self.state.model.clone();
        let window = self.window;
        cx.defer(move |cx| {
            let _ = window.update(cx, |_, window, cx| {
                if let Some(scrub) = scrub
                    && window.captured_hitbox() == Some(scrub.capture)
                {
                    window.release_pointer();
                }
                if let Some(model) = model {
                    model.update(cx, |model, _| model.cancel_interaction(window));
                }
            });
        });
        ReplayCleanup {
            workers: self
                .state
                .worker
                .take()
                .map(|worker| worker.handle)
                .into_iter()
                .collect(),
            files: self.state.presets.shutdown(),
        }
    }

    pub(crate) fn poll(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.stopped {
            return;
        }
        let mut changed = false;
        if self
            .state
            .worker
            .as_ref()
            .is_some_and(|worker| worker.handle.is_finished())
        {
            let worker = self.state.worker.take().expect("finished replay worker");
            let result = if worker.handle.join().is_err() {
                Err(tr("replay.error.worker_stopped"))
            } else {
                worker
                    .receive
                    .try_recv()
                    .unwrap_or_else(|_| Err(tr("replay.error.worker_no_result")))
            };
            if worker.generation == self.state.generation && !worker.cancel.load(Ordering::Relaxed)
            {
                match result {
                    Ok(Payload::Loaded(path, recording)) => {
                        self.state.path = Some(path);
                        self.state.recording = Some(recording);
                        self.state.track = 0;
                        self.state.processed = None;
                        self.state.edit_deadline = Some(Instant::now());
                    }
                    Ok(Payload::Processed(result)) => {
                        if result.result.frames.is_empty() {
                            self.state.error = Some(tr("replay.error.no_samples"));
                        }
                        let duration = result.result.frames.last().map_or(0., |f| f.time_s);
                        let prior_time = self.state.playback_time.min(duration);
                        let remap_reference = |index: Option<usize>| {
                            let time = index
                                .and_then(|i| self.state.processed.as_ref()?.result.frames.get(i))
                                .map(|f| f.time_s)?;
                            (!result.result.frames.is_empty()).then(|| {
                                result
                                    .result
                                    .frames
                                    .partition_point(|f| f.time_s <= time)
                                    .saturating_sub(1)
                            })
                        };
                        let reference_a = remap_reference(self.state.reference_a);
                        let reference_b = remap_reference(self.state.reference_b);
                        self.state.reference_a = reference_a;
                        self.state.reference_b = reference_b;
                        if !self.state.auto_attempted {
                            self.state.auto_attempted = true;
                            match result.summary.auto_references {
                                Ok(pair) => {
                                    self.state.reference_a = Some(pair.a);
                                    self.state.reference_b = Some(pair.b);
                                }
                                Err(error) => {
                                    self.state.notice = Some(tr(auto_reference_error_key(error)));
                                }
                            }
                        }
                        if !result.summary.has_9d {
                            self.state.nine = false;
                        }
                        self.state.requested_range = Some(result.range);
                        self.state.processed = Some(result);
                        self.state.settings_dirty = false;
                        self.state.seek(prior_time);
                    }
                    Ok(Payload::Viewport(result)) => {
                        self.state.requested_range = Some(result.range);
                        self.state.processed = Some(result);
                    }
                    Ok(Payload::Exported(path)) => {
                        self.state.notice = Some(format!(
                            "{}: {}",
                            tr("replay.export.success"),
                            path.display()
                        ))
                    }
                    Err(error) => self.state.error = Some(error),
                }
            }
            self.state.start_pending();
            changed = true;
        }
        if replay_edit_due(
            self.state.edit_deadline,
            Instant::now(),
            self.state.chooser.is_some() || self.state.presets.importing(),
            self.state.busy(),
        ) {
            self.state.edit_deadline = None;
            self.compute_replay(cx);
            changed = true;
        }
        let progress = self
            .state
            .worker
            .as_ref()
            .filter(|worker| {
                worker.generation == self.state.generation && !worker.cancel.load(Ordering::Relaxed)
            })
            .map_or(0, |worker| worker.progress.load(Ordering::Relaxed));
        if progress != self.state.displayed_progress {
            self.state.displayed_progress = progress;
            changed = true;
        }
        self.reconcile_interactions(window, cx);
        let visible_change = changed && self.state.page_visible;
        if changed {
            self.state.model_dirty = true;
        }
        if visible_change {
            self.sync_replay_model(cx);
            cx.notify();
        }
    }

    pub(crate) fn activate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.stopped {
            return;
        }
        self.state.page_visible = true;
        self.initialize_view(window, cx);
        self.initialize_replay_presets(cx);
        self.reconcile_interactions(window, cx);
        if self.state.model_dirty {
            self.sync_replay_model(cx);
        }
        cx.notify();
    }

    pub(crate) fn deactivate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.state.page_visible = false;
        if self.state.playing {
            self.state.pause();
        }
        self.cancel_interactions(window, cx);
    }

    fn reconcile_interactions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.state.scrub.is_some_and(|scrub| {
            scrub.generation != self.state.generation || self.state.processed.is_none()
        }) {
            self.cancel_replay_scrub(window, cx);
        }
        if self.state.processed.is_none()
            && let Some(model) = &self.state.model
        {
            model.update(cx, |model, _| model.cancel_interaction(window));
        }
    }

    fn cancel_interactions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.cancel_replay_scrub(window, cx);
        if let Some(model) = &self.state.model {
            model.update(cx, |model, _| model.cancel_interaction(window));
        }
    }

    pub(super) fn choose_replay(&mut self, cx: &mut Context<Self>) {
        if self.state.chooser.is_some() || self.state.presets.importing() || self.stopped {
            return;
        }
        let generation = self.state.generation;
        let dialog = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(tr("replay.import.prompt").into()),
        });
        self.state.chooser = Some(cx.spawn(async move |this, cx| {
            let chosen = dialog.await;
            let _ = this.update(cx, |this, cx| {
                this.state.chooser = None;
                if this.stopped || generation != this.state.generation {
                    return;
                }
                match chosen {
                    Ok(Ok(Some(paths))) => {
                        if let Some(path) = paths.into_iter().next() {
                            this.open_path(path, cx);
                        }
                    }
                    Ok(Err(error)) => this.state.error = Some(error.to_string()),
                    Err(error) => this.state.error = Some(error.to_string()),
                    _ => {}
                }
                cx.notify();
            });
        }));
    }

    pub(super) fn export_replay(&mut self, cx: &mut Context<Self>) {
        if self.state.busy()
            || self.state.chooser.is_some()
            || self.state.presets.importing()
            || self.stopped
        {
            return;
        }
        let Some(recording) = self.state.recording.clone() else {
            return;
        };
        let generation = self.state.generation;
        let directory = self
            .state
            .path
            .as_ref()
            .and_then(|path| path.parent())
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        let dialog = cx.prompt_for_new_path(&directory, Some("recording.zip"));
        self.state.chooser = Some(cx.spawn(async move |this, cx| {
            let chosen = dialog.await;
            let _ = this.update(cx, |this, cx| {
                this.state.chooser = None;
                if this.stopped || generation != this.state.generation {
                    return;
                }
                match chosen {
                    Ok(Ok(Some(path))) => this.state.request(Request::Export(recording, path)),
                    Ok(Err(error)) => this.state.error = Some(error.to_string()),
                    Err(error) => this.state.error = Some(error.to_string()),
                    _ => {}
                }
                cx.notify();
            });
        }));
    }

    pub(super) fn compute_replay(&mut self, cx: &mut Context<Self>) {
        if self.stopped {
            return;
        }
        let Some(recording) = self.state.recording.clone() else {
            return;
        };
        let options = match self.read_replay_options(cx) {
            Ok(options) => options,
            Err(error) => {
                self.state.error = Some(error);
                self.state.settings_open = true;
                cx.notify();
                return;
            }
        };
        self.state.options = options.clone();
        self.state.edit_deadline = None;
        self.state.settings_dirty = true;
        let range = self.state.processed.as_ref().map(|data| data.range);
        self.state.request(Request::Process(
            recording,
            self.state.track,
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
