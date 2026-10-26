use super::diagnostics::replay_diagnostic_section;
use super::*;
use gpui_kit::base::ElementExt;

impl Workbench {
    #[inline(never)]
    pub(in crate::ui) fn replay_page(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.replay.page_visible = true;
        if self.replay.scrub.is_some_and(|scrub| {
            scrub.generation != self.replay.generation || self.replay.processed.is_none()
        }) && let Some(scrub) = self.replay.scrub.take()
            && window.captured_hitbox() == Some(scrub.capture)
        {
            window.release_pointer();
        }
        if self.replay.processed.is_none()
            && let Some(model) = &self.replay.model
        {
            model.update(cx, |model, _| {
                model.cancel_interaction(window);
            });
        }
        self.replay_fields(window, cx);
        let mut page = div()
            .id("vqf-replay-page")
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .gap_1()
            .p_2()
            .on_key_down(cx.listener(Self::replay_key_down))
            .child(self.replay_toolbar(cx));
        if let Some(error) = &self.replay.error {
            page = page.child(
                div()
                    .text_sm()
                    .whitespace_normal()
                    .text_color(cx.theme().danger)
                    .child(error.clone()),
            );
        }
        if let Some(notice) = &self.replay.notice {
            page = page.child(
                div()
                    .text_sm()
                    .whitespace_normal()
                    .text_color(cx.theme().muted_foreground)
                    .child(notice.clone()),
            );
        }
        if self.replay.busy() {
            let active = self
                .replay
                .worker
                .as_ref()
                .filter(|worker| !worker.cancel.load(Ordering::Relaxed));
            if active.is_some() || self.replay.pending.is_some() {
                let status = if self.replay.phase == "replay.phase.computing" && active.is_some() {
                    let progress =
                        active.map_or(0, |worker| worker.progress.load(Ordering::Relaxed));
                    format!("{} · {:.0}%", tr(self.replay.phase), progress as f32 / 10.)
                } else {
                    tr(self.replay.phase)
                };
                page = page.child(div().child(status));
            }
        }
        if self.replay.recording.is_none() {
            return page
                .child(div().p_6().child(tr("replay.empty.import_hint")))
                .into_any_element();
        }
        let duration = self
            .replay
            .processed
            .as_ref()
            .and_then(|data| data.result.frames.last())
            .map_or(0., |f| f.time_s);
        page.child(self.replay_calibration(cx))
            .when(self.replay.settings_open, |page| {
                page.child(
                    div()
                        .id("replay-settings-scroll")
                        .h(px(226.))
                        .flex_shrink_0()
                        .overflow_y_scroll()
                        .child(self.replay_parameters(cx)),
                )
            })
            .child(self.replay_results(cx))
            .child(self.replay_timeline(duration, cx))
            .into_any_element()
    }

    #[inline(never)]
    fn replay_results(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(data) = &self.replay.processed else {
            return div()
                .flex_1()
                .min_h_0()
                .p_3()
                .child(tr(if self.replay.busy() {
                    "replay.phase.preparing"
                } else {
                    "replay.settings.recompute_hint"
                }))
                .into_any_element();
        };
        let mut chart_ids = ChartId::ALL;
        let mut chart_count = 0;
        for id in &self.replay.charts {
            if data.available(*id) {
                chart_ids[chart_count] = *id;
                chart_count += 1;
            }
        }
        let owner = cx.weak_entity();
        let chart_list = uniform_list("replay-chart-stack", chart_count, move |range, _, cx| {
            owner
                .update(cx, |this, cx| {
                    range
                        .map(|index| this.replay_chart(chart_ids[index], cx))
                        .collect::<Vec<AnyElement>>()
                })
                .unwrap_or_default()
        })
        .h_full()
        .w_full();
        let right_content = div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_2()
            .p_2()
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .pb_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        self.replay
                            .model
                            .as_ref()
                            .expect("replay model")
                            .clone()
                            .cached(
                                StyleRefinement::default()
                                    .w_full()
                                    .min_w_0()
                                    .h(px(220.))
                                    .flex_shrink_0(),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_x_2()
                            .gap_y_1()
                            .text_xs()
                            .children(
                                [
                                    "replay.axis.x",
                                    "replay.axis.y",
                                    "replay.axis.z",
                                    "replay.model.gravity",
                                    "replay.model.magnetic",
                                    "replay.model.linear_acceleration",
                                ]
                                .into_iter()
                                .enumerate()
                                .map(|(axis, label)| {
                                    div()
                                        .text_color(view::direction_color(
                                            axis,
                                            cx.theme().is_dark(),
                                        ))
                                        .child(tr(label))
                                }),
                            ),
                    ),
            )
            .child(self.replay_diagnostics(cx))
            .when(self.replay.details_open, |pane| {
                pane.child(
                    replay_diagnostic_section("replay.details.title", cx)
                        .child(self.replay_details()),
                )
            });
        let right = div()
            .id("replay-diagnostics")
            .w_full()
            .min_w_0()
            .h_full()
            .rounded_md()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().group_box)
            .overflow_y_scroll()
            .child(right_content);
        let layout = self.replay.layout.as_ref().expect("replay layout").clone();
        div()
            .relative()
            .w_full()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .child(
                h_resizable("replay-analysis-split")
                    .with_state(&layout)
                    .with_handle_appearance(self.resize_handle_appearance.clone())
                    .child(
                        resizable_panel().size_range(px(290.)..Pixels::MAX).child(
                            div()
                                .size_full()
                                .id("replay-chart-region")
                                .role(gpui::Role::Group)
                                .aria_label("Replay charts")
                                .aria_description(tr("replay.plot.interaction_hint"))
                                .track_focus(self.replay.focus.as_ref().expect("replay focus"))
                                .flex()
                                .flex_col()
                                .min_w_0()
                                .rounded_md()
                                .border_1()
                                .border_color(cx.theme().border)
                                .focus_visible(|style| style.border_color(cx.theme().ring))
                                .overflow_hidden()
                                .child(self.replay_viewport_controls(cx))
                                .child(div().flex_1().min_h_0().w_full().child(chart_list)),
                        ),
                    )
                    .child(
                        resizable_panel()
                            .size(px(300.))
                            .flex_none()
                            // The kit rescales stored widths with the window; keep
                            // diagnostics compact even after maximizing.
                            .size_range(
                                px(REPLAY_DIAGNOSTICS_MIN_WIDTH)..px(REPLAY_DIAGNOSTICS_MAX_WIDTH),
                            )
                            .child(right),
                    ),
            )
            .on_prepaint(move |_, window, cx| {
                // The kit clamps rendered bounds, but its window-resize path leaves
                // proportional widths in state. Normalize those through the public
                // sibling-redistribution API before the next drag uses them.
                if cx.has_active_drag()
                    || replay_diagnostics_width_correction(layout.read(cx).sizes()).is_none()
                {
                    return;
                }
                window.defer(cx, move |window, cx| {
                    // Never end a drag that started between prepaint and this callback.
                    if cx.has_active_drag() {
                        return;
                    }
                    layout.update(cx, |state, cx| {
                        if let Some(width) = replay_diagnostics_width_correction(state.sizes()) {
                            state.resize_panel(1, width, window, cx);
                        }
                    });
                });
            })
            .into_any_element()
    }

    pub(super) fn replay_overlay(&self) -> PlotOverlay {
        let frames = self
            .replay
            .processed
            .as_ref()
            .map(|data| &data.result.frames);
        let at = |index: Option<usize>| {
            index
                .and_then(|i| frames.and_then(|f| f.get(i)))
                .map(|f| f.time_s)
        };
        PlotOverlay {
            cursor: self.replay.playback_time,
            a: at(self.replay.reference_a),
            b: at(self.replay.reference_b),
            hover: self.replay.hover_time,
            selection: self
                .replay
                .scrub
                .and_then(|s| s.zoom_start)
                .zip(self.replay.hover_time),
        }
    }

    #[inline(never)]
    fn replay_chart(&self, id: ChartId, cx: &mut Context<Self>) -> AnyElement {
        let Some(data) = &self.replay.processed else {
            return div().into_any_element();
        };
        let plot = data.plot(id, self.replay.nine);
        let index = self
            .replay
            .hover_time
            .and_then(|time| data.nearest_index(time))
            .unwrap_or(self.replay.cursor);
        let values = data.values(id, index, self.replay.nine);
        let mut legend = div()
            .flex()
            .gap_2()
            .text_xs()
            .font_family(cx.theme().mono_font_family.clone())
            .whitespace_nowrap();
        for (channel, name) in id.legend().iter().enumerate() {
            legend = legend.child(
                div()
                    .text_color(id.color(channel, cx.theme().is_dark()))
                    .child(format!(
                        "{} {}",
                        tr(name),
                        values[channel].map_or_else(|| "—".into(), |v| format!("{v:.3}"))
                    )),
            );
        }
        if self.replay.hover_time.is_some()
            && let Some(frame) = data.result.frames.get(index)
        {
            legend = legend.child(format!("{:.3}s", frame.time_s));
        }
        let clipping = plot.clipped(self.replay.full_scale);
        let localized_unit = (id == ChartId::Orientation).then(|| tr("replay.unit.unitless"));
        div()
            .id(("replay-chart-row", id as usize))
            .h(px(222.))
            .w_full()
            .overflow_hidden()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().group_box)
            .child(
                div()
                    .flex()
                    .justify_between()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(format!(
                        "{} · {}",
                        tr(id.label()),
                        localized_unit.as_deref().unwrap_or_else(|| id.unit())
                    ))
                    .when(clipping > 0, |row| {
                        row.child(
                            div()
                                .text_xs()
                                .font_weight(FontWeight::NORMAL)
                                .text_color(cx.theme().muted_foreground)
                                .child(format!(
                                    "{}: {clipping}/{}",
                                    tr("replay.plot.clipped"),
                                    plot.finite_values
                                )),
                        )
                    }),
            )
            .child(legend)
            .child(
                div()
                    .id(("replay-chart", id as usize))
                    .w_full()
                    .text_color(cx.theme().muted_foreground)
                    .cursor_pointer()
                    .tooltip(|window, cx| {
                        Tooltip::new(tr("replay.plot.interaction_hint")).build(window, cx)
                    })
                    .child(view::plot_canvas(
                        id,
                        plot,
                        data.range,
                        self.replay_overlay(),
                        self.replay.full_scale,
                        cx.weak_entity(),
                        cx,
                    )),
            )
            .into_any_element()
    }
}

const REPLAY_DIAGNOSTICS_MIN_WIDTH: f32 = 280.;
const REPLAY_DIAGNOSTICS_MAX_WIDTH: f32 = 360.;

pub(super) fn replay_diagnostics_width_correction(sizes: &[Pixels]) -> Option<Pixels> {
    let width = *sizes.get(1)?;
    let bounded = width.clamp(
        px(REPLAY_DIAGNOSTICS_MIN_WIDTH),
        px(REPLAY_DIAGNOSTICS_MAX_WIDTH),
    );
    (bounded != width).then_some(bounded)
}
