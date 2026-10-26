use super::*;

impl Workbench {
    pub(super) fn sync_replay_model(&mut self, cx: &mut Context<Self>) {
        let Some(model) = &self.replay.model else {
            return;
        };
        let Some(frame) = self
            .replay
            .processed
            .as_ref()
            .and_then(|data| data.result.frames.get(self.replay.cursor))
        else {
            return;
        };
        let q = if self.replay.nine { frame.q9 } else { frame.q6 };
        model.update(cx, |model, cx| {
            model.set_orientation(q, cx);
            model.set_frame_vectors(Some(frame), cx);
        });
        self.replay.model_dirty = false;
    }

    fn schedule_replay_frame(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.replay.playing || self.replay.animation_pending || self.shutting_down {
            return;
        }
        self.replay.animation_pending = true;
        let epoch = self.replay.animation_epoch;
        let owner = cx.weak_entity();
        window.on_next_frame(move |window, cx| {
            let _ = owner.update(cx, |this, cx| {
                if epoch != this.replay.animation_epoch {
                    return;
                }
                this.replay.animation_pending = false;
                if !this.replay.playing {
                    return;
                }
                if this.shutting_down || this.active_page != FeaturePage::VqfReplay {
                    this.replay.pause();
                    cx.notify();
                    return;
                }
                this.advance_replay_clock(cx);
                this.schedule_replay_frame(window, cx);
            });
        });
    }

    pub(super) fn advance_replay_clock(&mut self, cx: &mut Context<Self>) {
        if !self.replay.playing {
            return;
        }
        let now = Instant::now();
        self.replay.playback_time +=
            now.duration_since(self.replay.clock).as_secs_f64() * self.replay.speed;
        self.replay.clock = now;
        let previous = self.replay.cursor;
        if let Some(data) = &self.replay.processed {
            let frames = &data.result.frames;
            if let Some(last) = frames.last() {
                self.replay.cursor = frames
                    .partition_point(|frame| frame.time_s <= self.replay.playback_time)
                    .saturating_sub(1)
                    .min(frames.len() - 1);
                if self.replay.playback_time >= last.time_s {
                    if self.replay.looping && last.time_s > 0. {
                        self.replay.playback_time %= last.time_s;
                        self.replay.cursor = frames
                            .partition_point(|frame| frame.time_s <= self.replay.playback_time)
                            .saturating_sub(1);
                    } else {
                        self.replay.pause();
                    }
                }
            } else {
                self.replay.pause();
            }
        } else {
            self.replay.pause();
        }
        if previous != self.replay.cursor || !self.replay.playing {
            self.sync_replay_model(cx);
            cx.notify();
        }
    }

    pub(super) fn toggle_replay_play(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.replay.settings_dirty || self.replay.busy() || self.replay.processed.is_none() {
            return;
        }
        if self.replay.playing {
            self.replay.pause();
        } else {
            self.replay.pause();
            if self
                .replay
                .processed
                .as_ref()
                .is_some_and(|data| self.replay.cursor + 1 >= data.result.frames.len())
            {
                self.replay.seek(0.);
            }
            self.replay.clock = Instant::now();
            self.replay.playing = true;
            self.schedule_replay_frame(window, cx);
        }
        self.sync_replay_model(cx);
        cx.notify();
    }

    pub(super) fn replay_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let modifiers = event.keystroke.modifiers;
        if modifiers.control
            || modifiers.alt
            || modifiers.platform
            || modifiers.shift
            || self.replay.chooser.is_some()
            || self.replay.settings_dirty
            || self.replay.busy()
            || self.replay.processed.is_none()
            || self
                .replay
                .fields
                .iter()
                .any(|input| input.read(cx).focus_handle(cx).is_focused(window))
            || self
                .replay
                .preset_name
                .as_ref()
                .is_some_and(|input| input.read(cx).focus_handle(cx).is_focused(window))
        {
            return;
        }
        match event.keystroke.key.as_str() {
            "space" => self.toggle_replay_play(window, cx),
            "left" => self.step_replay(false, cx),
            "right" => self.step_replay(true, cx),
            "up" => {
                self.replay.speed = (self.replay.speed * 2.).min(8.);
            }
            "down" => {
                self.replay.speed = (self.replay.speed * 0.5).max(0.25);
            }
            "a" => self.replay.reference_a = Some(self.replay.cursor),
            "b" => self.replay.reference_b = Some(self.replay.cursor),
            "x" => {
                self.replay.reference_a = None;
                self.replay.reference_b = None;
            }
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    pub(super) fn auto_replay_references(&mut self, cx: &mut Context<Self>) {
        if self.replay.settings_dirty || self.replay.busy() {
            return;
        }
        let Some(data) = &self.replay.processed else {
            return;
        };
        match data.summary.auto_references {
            Ok(pair) => {
                self.replay.reference_a = Some(pair.a);
                self.replay.reference_b = Some(pair.b);
                self.replay.notice = None;
                self.replay.error = None;
            }
            Err(error) => self.replay.notice = Some(tr(auto_reference_error_key(error))),
        }
        self.replay.auto_attempted = true;
        cx.notify();
    }

    pub(super) fn step_replay(&mut self, forward: bool, cx: &mut Context<Self>) {
        self.replay.pause();
        if let Some(data) = &self.replay.processed
            && !data.result.frames.is_empty()
        {
            let next = if forward {
                (self.replay.cursor + 1).min(data.result.frames.len() - 1)
            } else {
                self.replay.cursor.saturating_sub(1)
            };
            let seconds = data.result.frames[next].time_s;
            self.replay.seek(seconds);
        }
        self.sync_replay_model(cx);
        cx.notify();
    }
}
