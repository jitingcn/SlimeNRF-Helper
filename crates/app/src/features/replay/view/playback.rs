use super::*;

impl ReplayFeature {
    pub(super) fn sync_replay_model(&mut self, cx: &mut Context<Self>) {
        let Some(model) = &self.state.model else {
            return;
        };
        let Some(frame) = self
            .state
            .processed
            .as_ref()
            .and_then(|data| data.result.frames.get(self.state.cursor))
        else {
            return;
        };
        let q = if self.state.nine { frame.q9 } else { frame.q6 };
        model.update(cx, |model, cx| {
            model.set_orientation(q, cx);
            model.set_frame_vectors(Some(frame), cx);
        });
        self.state.model_dirty = false;
    }

    fn schedule_replay_frame(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.state.playing || self.state.animation_pending || self.stopped {
            return;
        }
        self.state.animation_pending = true;
        let epoch = self.state.animation_epoch;
        let owner = cx.weak_entity();
        window.on_next_frame(move |window, cx| {
            let _ = owner.update(cx, |this, cx| {
                if epoch != this.state.animation_epoch {
                    return;
                }
                this.state.animation_pending = false;
                if !this.state.playing {
                    return;
                }
                if this.stopped || !this.state.page_visible {
                    this.state.pause();
                    cx.notify();
                    return;
                }
                this.advance_replay_clock(cx);
                this.schedule_replay_frame(window, cx);
            });
        });
    }

    pub(super) fn advance_replay_clock(&mut self, cx: &mut Context<Self>) {
        if self.state.advance_clock_at(Instant::now()) {
            self.sync_replay_model(cx);
            cx.notify();
        }
    }

    pub(super) fn set_speed(&mut self, speed: f64, cx: &mut Context<Self>) {
        if self.stopped {
            return;
        }
        if self.state.set_speed_at(speed, Instant::now()) {
            self.sync_replay_model(cx);
        }
        cx.notify();
    }

    pub(super) fn toggle_replay_play(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.stopped
            || !self.state.page_visible
            || self.state.settings_dirty
            || self.state.busy()
            || self.state.processed.is_none()
        {
            return;
        }
        if self.state.playing {
            self.state.pause();
        } else {
            self.state.pause();
            if self
                .state
                .processed
                .as_ref()
                .is_some_and(|data| self.state.cursor + 1 >= data.result.frames.len())
            {
                self.state.seek(0.);
            }
            self.state.clock = Instant::now();
            self.state.playing = true;
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
        if self.stopped
            || !self.state.page_visible
            || modifiers.control
            || modifiers.alt
            || modifiers.platform
            || modifiers.shift
            || self.state.chooser.is_some()
            || self.state.settings_dirty
            || self.state.busy()
            || self.state.processed.is_none()
            || self
                .state
                .fields
                .iter()
                .any(|input| input.read(cx).focus_handle(cx).is_focused(window))
            || self
                .state
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
            "up" => self.set_speed((self.state.speed * 2.).min(8.), cx),
            "down" => self.set_speed((self.state.speed * 0.5).max(0.25), cx),
            "a" => self.state.set_reference(ReferencePoint::A),
            "b" => self.state.set_reference(ReferencePoint::B),
            "x" => self.state.clear_references(),
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    pub(super) fn auto_replay_references(&mut self, cx: &mut Context<Self>) {
        if self.state.settings_dirty || self.state.busy() {
            return;
        }
        let Some(data) = &self.state.processed else {
            return;
        };
        match data.summary.auto_references {
            Ok(pair) => {
                self.state.reference_a = Some(pair.a);
                self.state.reference_b = Some(pair.b);
                self.state.notice = None;
                self.state.error = None;
            }
            Err(error) => self.state.notice = Some(tr(auto_reference_error_key(error))),
        }
        self.state.auto_attempted = true;
        cx.notify();
    }

    pub(super) fn step_replay(&mut self, forward: bool, cx: &mut Context<Self>) {
        self.state.pause();
        if let Some(data) = &self.state.processed
            && !data.result.frames.is_empty()
        {
            let next = if forward {
                (self.state.cursor + 1).min(data.result.frames.len() - 1)
            } else {
                self.state.cursor.saturating_sub(1)
            };
            let seconds = data.result.frames[next].time_s;
            self.state.seek(seconds);
        }
        self.sync_replay_model(cx);
        cx.notify();
    }
}

#[derive(Clone, Copy)]
pub(super) enum ReferencePoint {
    A,
    B,
}

impl ReplayState {
    pub(super) fn set_reference(&mut self, point: ReferencePoint) {
        match point {
            ReferencePoint::A => self.reference_a = Some(self.cursor),
            ReferencePoint::B => self.reference_b = Some(self.cursor),
        }
    }

    pub(super) fn clear_references(&mut self) {
        self.reference_a = None;
        self.reference_b = None;
    }

    pub(super) fn set_speed_at(&mut self, speed: f64, now: Instant) -> bool {
        // Settle elapsed playback under the previous speed before replacing it.
        let changed = self.advance_clock_at(now);
        self.speed = speed;
        changed
    }

    pub(super) fn advance_clock_at(&mut self, now: Instant) -> bool {
        if !self.playing {
            return false;
        }
        self.playback_time += now.duration_since(self.clock).as_secs_f64() * self.speed;
        self.clock = now;
        let previous = self.cursor;
        if let Some(data) = &self.processed {
            let frames = &data.result.frames;
            if let Some(last) = frames.last() {
                self.cursor = frames
                    .partition_point(|frame| frame.time_s <= self.playback_time)
                    .saturating_sub(1)
                    .min(frames.len() - 1);
                if self.playback_time >= last.time_s {
                    if self.looping && last.time_s > 0. {
                        self.playback_time %= last.time_s;
                        self.cursor = frames
                            .partition_point(|frame| frame.time_s <= self.playback_time)
                            .saturating_sub(1);
                    } else {
                        self.pause();
                    }
                }
            } else {
                self.pause();
            }
        } else {
            self.pause();
        }
        previous != self.cursor || !self.playing
    }
}
