use super::diagnostics::replay_diagnostic_section;
use super::*;
use gpui_kit::base::ElementExt;

impl Render for ReplayFeature {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.render_page(cx)
    }
}

impl ReplayFeature {
    #[inline(never)]
    fn render_page(&self, cx: &mut Context<Self>) -> AnyElement {
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
        if let Some(error) = &self.state.error {
            page = page.child(
                div()
                    .text_sm()
                    .whitespace_normal()
                    .text_color(cx.theme().danger)
                    .child(error.clone()),
            );
        }
        if let Some(notice) = &self.state.notice {
            page = page.child(
                div()
                    .text_sm()
                    .whitespace_normal()
                    .text_color(cx.theme().muted_foreground)
                    .child(notice.clone()),
            );
        }
        if self.state.busy() {
            let active = self
                .state
                .worker
                .as_ref()
                .filter(|worker| !worker.cancel.load(Ordering::Relaxed));
            if active.is_some() || self.state.pending.is_some() {
                let status: SharedString =
                    (if self.state.phase == "replay.phase.computing" && active.is_some() {
                        format!(
                            "{} · {:.0}%",
                            tr(self.state.phase),
                            self.state.displayed_progress as f32 / 10.
                        )
                    } else {
                        tr(self.state.phase)
                    })
                    .into();
                let progress = div()
                    .id("replay-progress")
                    .role(gpui::Role::Status)
                    .aria_label(status.clone())
                    .child(status);
                #[cfg(test)]
                let progress = {
                    use gpui_kit::test::TestSupportExt as _;
                    progress.test_support()
                };
                page = page.child(progress);
            }
        }
        if self.state.recording.is_none() {
            return page
                .child(div().p_6().child(tr("replay.empty.import_hint")))
                .into_any_element();
        }
        let duration = self
            .state
            .processed
            .as_ref()
            .and_then(|data| data.result.frames.last())
            .map_or(0., |f| f.time_s);
        page.child(self.replay_calibration(cx))
            .when(self.state.settings_open, |page| {
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
        let Some(data) = &self.state.processed else {
            return div()
                .flex_1()
                .min_h_0()
                .p_3()
                .child(tr(if self.state.busy() {
                    "replay.phase.preparing"
                } else {
                    "replay.settings.recompute_hint"
                }))
                .into_any_element();
        };
        let mut chart_ids = ChartId::ALL;
        let mut chart_count = 0;
        for id in &self.state.charts {
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
                        self.state
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
                                        .text_color(presentation::direction_color(
                                            axis,
                                            cx.theme().is_dark(),
                                        ))
                                        .child(tr(label))
                                }),
                            ),
                    ),
            )
            .child(self.replay_diagnostics(cx))
            .when(self.state.details_open, |pane| {
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
        let layout = self.state.layout.as_ref().expect("replay layout").clone();
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
                                .track_focus(self.state.focus.as_ref().expect("replay focus"))
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
            .state
            .processed
            .as_ref()
            .map(|data| &data.result.frames);
        let at = |index: Option<usize>| {
            index
                .and_then(|i| frames.and_then(|f| f.get(i)))
                .map(|f| f.time_s)
        };
        PlotOverlay {
            cursor: self.state.playback_time,
            a: at(self.state.reference_a),
            b: at(self.state.reference_b),
            hover: self.state.hover_time,
            selection: self
                .state
                .scrub
                .and_then(|s| s.zoom_start)
                .zip(self.state.hover_time),
        }
    }

    #[inline(never)]
    fn replay_chart(&self, id: ChartId, cx: &mut Context<Self>) -> AnyElement {
        let Some(data) = &self.state.processed else {
            return div().into_any_element();
        };
        let plot = data.plot(id, self.state.nine);
        let index = self
            .state
            .hover_time
            .and_then(|time| data.nearest_index(time))
            .unwrap_or(self.state.cursor);
        let values = data.values(id, index, self.state.nine);
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
        if self.state.hover_time.is_some()
            && let Some(frame) = data.result.frames.get(index)
        {
            legend = legend.child(format!("{:.3}s", frame.time_s));
        }
        let clipping = plot.clipped(self.state.full_scale);
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
                    .child(presentation::plot_canvas(
                        id,
                        plot,
                        data.range,
                        self.replay_overlay(),
                        self.state.full_scale,
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
