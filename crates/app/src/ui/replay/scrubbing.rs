use super::*;

impl Workbench {
    pub(super) fn begin_replay_scrub(
        &mut self,
        x: Pixels,
        bounds: Bounds<Pixels>,
        range: (f64, f64),
        capture: HitboxId,
        shift: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.shutting_down
            || self.replay.settings_dirty
            || self.replay.busy()
            || self.replay.processed.is_none()
        {
            return false;
        }
        self.replay.pause();
        self.replay.scrub = Some(ReplayScrub {
            generation: self.replay.generation,
            range,
            bounds,
            capture,
            zoom_start: shift.then(|| {
                replay_scrub_time(
                    f64::from(f32::from(x - bounds.left()) / f32::from(bounds.size.width).max(1.)),
                    range,
                )
            }),
        });
        self.seek_replay_scrub(x, cx);
        cx.notify();
        true
    }

    fn seek_replay_scrub(&mut self, x: Pixels, cx: &mut Context<Self>) {
        let Some(scrub) = self.replay.scrub else {
            return;
        };
        let fraction = f64::from(
            f32::from(x - scrub.bounds.left()) / f32::from(scrub.bounds.size.width).max(1.),
        );
        let seconds = replay_scrub_time(fraction, scrub.range);
        if scrub.zoom_start.is_some() {
            self.replay.hover_time = Some(seconds);
            cx.notify();
            return;
        }
        let previous = self.replay.cursor;
        self.replay.seek(seconds);
        if previous != self.replay.cursor {
            self.sync_replay_model(cx);
            cx.notify();
        }
    }

    pub(super) fn continue_replay_scrub(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(scrub) = self.replay.scrub else {
            return false;
        };
        if window.captured_hitbox() != Some(scrub.capture) {
            self.cancel_replay_scrub(window, cx);
            return false;
        }
        if event.pressed_button != Some(MouseButton::Left)
            || !window.is_window_active()
            || self.active_page != FeaturePage::VqfReplay
            || scrub.generation != self.replay.generation
        {
            self.cancel_replay_scrub(window, cx);
        } else {
            self.seek_replay_scrub(event.position.x, cx);
        }
        true
    }

    pub(super) fn finish_replay_scrub(
        &mut self,
        event: &MouseUpEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(scrub) = self.replay.scrub else {
            return false;
        };
        if window.captured_hitbox() != Some(scrub.capture) {
            self.cancel_replay_scrub(window, cx);
            return false;
        }
        if event.button == MouseButton::Left
            && scrub.generation == self.replay.generation
            && self.active_page == FeaturePage::VqfReplay
        {
            self.seek_replay_scrub(event.position.x, cx);
            if let (Some(start), Some(end)) = (scrub.zoom_start, self.replay.hover_time)
                && (end - start).abs() > (scrub.range.1 - scrub.range.0) * 0.002
            {
                self.request_replay_range((start.min(end), start.max(end)), cx);
            }
        }
        self.cancel_replay_scrub(window, cx);
        true
    }

    pub(super) fn cancel_replay_scrub(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(scrub) = self.replay.scrub.take() else {
            return false;
        };
        let owned = window.captured_hitbox() == Some(scrub.capture);
        if owned {
            window.release_pointer();
        }
        cx.notify();
        owned
    }

    pub(super) fn change_replay_viewport(&mut self, zoom: f64, pan: f64, cx: &mut Context<Self>) {
        if self.shutting_down || self.replay.settings_dirty {
            return;
        }
        let Some(data) = &self.replay.processed else {
            return;
        };
        let Some(last) = data.result.frames.last() else {
            return;
        };
        let duration = last.time_s;
        let previous = self.replay.requested_range.unwrap_or(data.range);
        let range = viewport_range(previous, self.replay.playback_time, duration, zoom, pan);
        if range == data.range && !self.replay.busy() {
            return;
        }
        self.request_replay_range(range, cx);
    }

    fn request_replay_range(&mut self, range: (f64, f64), cx: &mut Context<Self>) {
        if self.replay.settings_dirty
            || (self.replay.busy() && self.replay.phase != "replay.phase.viewport")
        {
            return;
        }
        let Some(data) = &self.replay.processed else {
            return;
        };
        let result = data.result.clone();
        let summary = data.summary.clone();
        self.replay
            .request(Request::Viewport(result, summary, range));
        self.replay.requested_range = Some(range);
        cx.notify();
    }

    pub(super) fn replay_plot_hover_chart(
        &mut self,
        chart: ChartId,
        time: Option<f64>,
        cx: &mut Context<Self>,
    ) {
        if time.is_some() {
            self.replay.hover_chart = Some(chart);
            self.replay_plot_hover(time, cx);
        } else if self.replay.hover_chart == Some(chart) && self.replay.scrub.is_none() {
            self.replay.hover_chart = None;
            self.replay_plot_hover(None, cx);
        }
    }

    fn replay_plot_hover(&mut self, time: Option<f64>, cx: &mut Context<Self>) {
        if self.replay.hover_time != time {
            self.replay.hover_time = time;
            cx.notify();
        }
    }

    pub(super) fn replay_plot_wheel(&mut self, time: f64, delta: f32, cx: &mut Context<Self>) {
        if self.replay.settings_dirty {
            return;
        }
        let Some(data) = &self.replay.processed else {
            return;
        };
        let duration = data.result.frames.last().map_or(0., |f| f.time_s);
        let previous = self.replay.requested_range.unwrap_or(data.range);
        let range = viewport_range(
            previous,
            time,
            duration,
            if delta > 0. { 0.8 } else { 1.25 },
            0.,
        );
        self.request_replay_range(range, cx);
    }

    pub(super) fn reset_replay_plot(&mut self, cx: &mut Context<Self>) {
        self.change_replay_viewport(0., 0., cx);
    }
}

pub(super) fn replay_scrub_time(fraction: f64, range: (f64, f64)) -> f64 {
    range.0 + fraction.clamp(0., 1.) * (range.1 - range.0)
}

pub(super) fn viewport_range(
    previous: (f64, f64),
    cursor: f64,
    duration: f64,
    zoom: f64,
    pan: f64,
) -> (f64, f64) {
    if duration <= 0. || zoom == 0. {
        return (0., duration.max(0.));
    }
    let width = ((previous.1 - previous.0) * zoom).clamp(duration.min(0.01), duration);
    let center = if pan == 0. {
        cursor
    } else {
        (previous.0 + previous.1) * 0.5 + pan * width
    };
    let start = (center - width * 0.5).clamp(0., duration - width);
    (start, start + width)
}
