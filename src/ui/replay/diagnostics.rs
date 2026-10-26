use super::*;

impl Workbench {
    #[inline(never)]
    pub(super) fn replay_diagnostics(&self, cx: &App) -> AnyElement {
        let Some(data) = &self.replay.processed else {
            return div().into_any_element();
        };
        let Some(frame) = data.result.frames.get(self.replay.cursor) else {
            return div().into_any_element();
        };
        let diagnostic = analysis::frame_diagnostics(frame);
        let metrics = analysis::reference_metrics(
            &data.result,
            self.replay.reference_a,
            self.replay.reference_b,
        );
        let number = |v: Option<f64>| v.map_or_else(|| "—".into(), |v| format!("{v:.3}"));
        let dark = cx.theme().is_dark();
        let success = if dark {
            cx.theme().success
        } else {
            view::direction_color(1, false)
        };
        let warning = if dark {
            cx.theme().warning
        } else {
            view::direction_color(3, false)
        };
        let danger = if dark {
            cx.theme().danger
        } else {
            view::direction_color(0, false)
        };
        let (rest, rest_color) = if !data.result.options.params.rest_bias_est_enabled {
            ("replay.status.rest_disabled", cx.theme().muted_foreground)
        } else if frame.rest {
            ("replay.status.at_rest", success)
        } else {
            ("replay.status.moving", warning)
        };
        let magnetic = replay_magnetic_status(
            data.result.options.use_magnetometer,
            data.summary.has_mag,
            frame.heading9_valid,
            data.result.options.params.mag_dist_rejection_enabled,
            frame.mag_disturbed,
        );
        let magnetic_color = match magnetic {
            "replay.status.magnetic_unsettled" => danger,
            "replay.status.magnetic_stable" => success,
            "replay.status.magnetic_waiting" => warning,
            _ => cx.theme().muted_foreground,
        };
        let mut status = div().flex().flex_col().gap_2().child(
            div()
                .flex()
                .flex_wrap()
                .gap_1()
                .child(replay_status_pill(rest, rest_color))
                .child(replay_status_pill(magnetic, magnetic_color)),
        );
        if data.summary.has_temperature {
            status = status.child(replay_metric_row(
                "replay.diagnostic.temperature",
                replay_diagnostic_value(diagnostic.temperature_c, 2, " °C"),
                cx,
            ));
        }
        let mut orientation = replay_diagnostic_section("replay.diagnostic.orientation", cx)
            .child(replay_component_header(
                ["replay.axis.roll", "replay.axis.pitch", "replay.axis.yaw"],
                cx,
            ))
            .child(replay_component_values(
                "6D",
                Some(diagnostic.euler6_deg),
                2,
                cx,
            ));
        if data.summary.has_9d {
            orientation = orientation
                .child(replay_component_values("9D", diagnostic.euler9_deg, 2, cx))
                .child(replay_metric_row(
                    "replay.diagnostic.heading_difference",
                    replay_diagnostic_value(diagnostic.heading_difference_deg, 2, "°"),
                    cx,
                ));
        }
        let mut bias = replay_diagnostic_section("replay.diagnostic.bias", cx)
            .child(replay_component_header(
                ["replay.axis.x", "replay.axis.y", "replay.axis.z"],
                cx,
            ))
            .child(replay_component_values(
                "°/s",
                Some(diagnostic.bias_deg_s),
                4,
                cx,
            ));
        if data.summary.has_thermal {
            bias = bias
                .child(
                    div()
                        .pt_1()
                        .text_color(cx.theme().muted_foreground)
                        .child(tr("replay.diagnostic.thermal_compensation")),
                )
                .child(replay_component_values(
                    "°/s",
                    diagnostic.thermal_correction_deg_s,
                    4,
                    cx,
                ));
        }
        let mut panel = div()
            .w_full()
            .min_w_0()
            .whitespace_normal()
            .flex()
            .flex_col()
            .gap_2()
            .text_xs()
            .child(status)
            .child(orientation)
            .child(bias);
        if data.summary.has_mag {
            panel = panel.child(
                replay_diagnostic_section("replay.diagnostic.magnetic", cx)
                    .child(replay_metric_row(
                        "replay.diagnostic.magnetic_norm",
                        replay_diagnostic_value(diagnostic.magnetic_norm_gauss, 4, " G"),
                        cx,
                    ))
                    .child(replay_metric_row(
                        "replay.diagnostic.magnetic_inclination",
                        replay_diagnostic_value(diagnostic.magnetic_inclination_deg, 2, "°"),
                        cx,
                    )),
            );
        }
        if data.summary.has_accel {
            panel = panel.child(
                replay_diagnostic_section("replay.diagnostic.acceleration", cx)
                    .child(replay_metric_row(
                        "replay.diagnostic.acceleration_deviation",
                        replay_diagnostic_value(diagnostic.accel_norm_deviation_g, 5, " g"),
                        cx,
                    ))
                    .child(replay_metric_row(
                        "replay.diagnostic.linear_acceleration_norm",
                        replay_diagnostic_value(diagnostic.linear_accel_magnitude_g, 5, " g"),
                        cx,
                    )),
            );
        }
        if metrics.a.is_some() || metrics.b.is_some() {
            let mut references = replay_diagnostic_section("replay.diagnostic.heading_drift", cx);
            for (index, (name, endpoint)) in [
                ("replay.reference.a", metrics.a),
                ("replay.reference.b", metrics.b),
            ]
            .into_iter()
            .enumerate()
            {
                if let Some(point) = endpoint {
                    references = references
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(replay_status_pill(name, view::reference_color(index, dark)))
                                .child(
                                    div()
                                        .font_family(cx.theme().mono_font_family.clone())
                                        .child(format!("{:.3} s", point.time_s)),
                                ),
                        )
                        .child(replay_metric_row(
                            "replay.diagnostic.heading_pair",
                            format!(
                                "{}° / {}°",
                                number(point.heading6_deg),
                                number(point.heading9_deg)
                            ),
                            cx,
                        ));
                }
            }
            if let Some(interval) = metrics.interval {
                references = references.child(replay_metric_row(
                    "replay.diagnostic.reference_interval",
                    format!("{:.3} s", interval.duration_s),
                    cx,
                ));
                if interval.continuous {
                    for (label, drift, rate) in [
                        (
                            "replay.diagnostic.drift_6d",
                            interval.drift6_deg,
                            interval.rate6_deg_min,
                        ),
                        (
                            "replay.diagnostic.drift_9d",
                            interval.drift9_deg,
                            interval.rate9_deg_min,
                        ),
                    ] {
                        references = references.child(replay_metric_row(
                            label,
                            format!("{}° · {}°/min", number(drift), number(rate)),
                            cx,
                        ));
                    }
                } else {
                    references = references.child(
                        div()
                            .text_color(warning)
                            .child(tr("replay.reference.crosses_reset")),
                    );
                }
            }
            panel = panel.child(
                references.child(
                    div()
                        .text_color(cx.theme().muted_foreground)
                        .child(tr("replay.reference.baseline_note")),
                ),
            );
        }
        panel.into_any_element()
    }

    #[inline(never)]
    pub(super) fn replay_details(&self) -> AnyElement {
        let mut panel = div()
            .w_full()
            .min_w_0()
            .whitespace_normal()
            .flex()
            .flex_col()
            .gap_2()
            .text_sm()
            .child(tr("replay.details.gyro_quaternion_note"))
            .child(tr("replay.details.reconstructed_note"));
        if let Some(path) = &self.replay.path {
            panel = panel.child(path.display().to_string());
        }
        if let Some(recording) = &self.replay.recording {
            for source in &recording.provenance {
                panel = panel.child(source.clone());
            }
            if self.replay.processed.is_none() {
                for warning in &recording.warnings {
                    panel = panel.child(warning.clone());
                }
            }
        }
        if let Some(data) = &self.replay.processed {
            if let Some(reference) = data.summary.magnetic_reference {
                panel = panel.child(format!(
                    "{}: {:.4} G / {:.2}°",
                    tr("replay.details.first_mean"),
                    reference.norm_gauss,
                    reference.inclination_deg
                ));
            }
            panel = panel.child(data.result.profile.clone());
            panel = panel.child(format!(
                "{}: #{} · {}",
                tr("replay.details.target"),
                data.result.identity.slot,
                data.result
                    .identity
                    .rf_address
                    .as_deref()
                    .unwrap_or(&data.result.identity.receiver),
            ));
            for warning in &data.result.warnings {
                panel = panel.child(warning.clone());
            }
            if self
                .replay
                .charts
                .iter()
                .any(|id| data.available(*id) && data.plot(*id, self.replay.nine).dense_boundaries)
            {
                panel = panel.child(tr("replay.details.dense_segments"));
            }
            if let Some(frame) = data.result.frames.get(self.replay.cursor) {
                panel = panel
                    .child(format!(
                        "{}: {} / {} · {}: {}",
                        tr("replay.details.segment"),
                        frame.segment + 1,
                        data.result.segments.len(),
                        tr("replay.details.observed_sample"),
                        frame.observed
                    ))
                    .child(format!(
                        "q wxyz: {:.5?}",
                        if self.replay.nine { frame.q9 } else { frame.q6 }
                    ))
                    .child(format!(
                        "{} rad/s: {:.5?}",
                        tr("replay.details.gyro_bias"),
                        frame.bias_rad_s
                    ))
                    .child(format!(
                        "{}: {:.5} / {:.5}",
                        tr("replay.details.magnetic_norm_reference"),
                        frame.mag_norm,
                        frame.mag_ref_norm
                    ))
                    .child(format!(
                        "{}: {:.2} / {:.2}",
                        tr("replay.details.magnetic_dip_reference"),
                        frame.mag_dip_rad.to_degrees(),
                        frame.mag_ref_dip_rad.to_degrees()
                    ));
                if data.result.options.use_magnetometer && data.summary.has_mag {
                    panel = panel.child(format!(
                        "{}: {:.5}",
                        tr("replay.details.magnetic_initialization_gain"),
                        frame.k_mag_init,
                    ));
                }
                if let Some(segment) = data.result.segments.get(frame.segment)
                    && let (Some(first), Some(last)) = (
                        data.result.frames.get(segment.start),
                        data.result.frames.get(segment.end.saturating_sub(1)),
                    )
                {
                    panel = panel.child(format!(
                        "{}: {:.3}–{:.3} s · {} {}",
                        tr("replay.details.segment"),
                        first.time_s,
                        last.time_s,
                        segment.end.saturating_sub(segment.start),
                        tr("replay.samples")
                    ));
                }
            }
        }
        panel.into_any_element()
    }
}

pub(super) fn replay_diagnostic_section(title: &str, cx: &App) -> Div {
    div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_1()
        .pt_2()
        .border_t_1()
        .border_color(cx.theme().border)
        .child(
            div()
                .pb_1()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(cx.theme().foreground)
                .child(tr(title)),
        )
}

fn replay_metric_row(label: &str, value: String, cx: &App) -> Div {
    div()
        .w_full()
        .min_w_0()
        .flex()
        .items_start()
        .justify_between()
        .gap_2()
        .child(
            div()
                .min_w_0()
                .text_color(cx.theme().muted_foreground)
                .child(tr(label)),
        )
        .child(
            div()
                .flex_shrink_0()
                .font_family(cx.theme().mono_font_family.clone())
                .text_color(cx.theme().foreground)
                .child(value),
        )
}

fn replay_component_header(labels: [&str; 3], cx: &App) -> Div {
    div()
        .flex()
        .gap_1()
        .text_color(cx.theme().muted_foreground)
        .child(div().w(px(28.)).flex_shrink_0())
        .children(labels.into_iter().enumerate().map(|(axis, label)| {
            div()
                .flex_1()
                .min_w_0()
                .text_right()
                .text_color(view::direction_color(axis, cx.theme().is_dark()))
                .child(tr(label))
        }))
}

fn replay_component_values(
    label: &str,
    values: Option<[f64; 3]>,
    precision: usize,
    cx: &App,
) -> Div {
    div()
        .flex()
        .gap_1()
        .font_family(cx.theme().mono_font_family.clone())
        .whitespace_nowrap()
        .child(
            div()
                .w(px(28.))
                .flex_shrink_0()
                .text_color(cx.theme().muted_foreground)
                .child(label.to_owned()),
        )
        .children((0..3).map(|axis| {
            div()
                .flex_1()
                .min_w_0()
                .text_right()
                .text_color(view::direction_color(axis, cx.theme().is_dark()))
                .child(replay_diagnostic_value(
                    values.as_ref().map(|v| v[axis]),
                    precision,
                    "",
                ))
        }))
}

fn replay_diagnostic_value(value: Option<f64>, precision: usize, unit: &str) -> String {
    value
        .filter(|value| value.is_finite())
        .map_or_else(|| "—".into(), |value| format!("{value:.precision$}{unit}"))
}

fn replay_status_pill(label: &str, color: Hsla) -> Div {
    div()
        .px_2()
        .py(px(2.))
        .rounded_md()
        .border_1()
        .border_color(color.opacity(0.35))
        .bg(color.opacity(0.10))
        .text_color(color)
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .child(tr(label))
}

pub(super) fn replay_magnetic_status(
    enabled: bool,
    has_observations: bool,
    heading_valid: bool,
    rejection_enabled: bool,
    disturbed: bool,
) -> &'static str {
    if !enabled {
        "replay.status.magnetic_unused"
    } else if !has_observations {
        "replay.status.magnetic_unavailable"
    } else if !heading_valid {
        "replay.status.magnetic_waiting"
    } else if !rejection_enabled {
        "replay.status.magnetic_rejection_disabled"
    } else if disturbed {
        "replay.status.magnetic_unsettled"
    } else {
        "replay.status.magnetic_stable"
    }
}
