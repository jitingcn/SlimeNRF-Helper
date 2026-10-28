use crate::{
    i18n::tr,
    rssi_scan::{Recommendation, Sample, ScanReport},
};
use gpui_kit::base::plot::{PathCaches, ShapeKey};
use gpui_kit::component::{ActiveTheme, tooltip::Tooltip};
use gpui_kit::{
    AnyElement, App, ContentMask, Hsla, IntoElement, Path, PathBuilder, Pixels, SharedString,
    canvas, div, point, prelude::*, px, relative, rems,
};
use std::{
    hash::{DefaultHasher, Hash, Hasher},
    sync::Arc,
    time::Duration,
};

#[cfg(test)]
use gpui_kit::test::TestSupportExt as _;

const BAR_CHANNEL_WIDTH: f32 = 0.8;
const MISSING_MARK_HALF_REM: f32 = 0.25;
const MISSING_MARK_INSET_REM: f32 = 0.12;
// Seven ticks fit at the chart's minimum height with a one-rem line height.
const MAX_RSSI_INTERVALS: i32 = 6;

struct RssiAxis {
    bottom: i32,
    top: i32,
    step: i32,
}

impl RssiAxis {
    fn new(extent: Option<(i32, i32)>) -> Self {
        // Empty reports do not render a chart; retain a finite default until
        // the first real measurement arrives.
        let (minimum, maximum) = extent.unwrap_or((0, 0));
        let padding = ((maximum - minimum + 9) / 10).max(5);
        let low = minimum - padding;
        let high = maximum + padding;
        let mut magnitude = 1;
        loop {
            for step in [
                magnitude,
                2 * magnitude,
                5 * magnitude / 2,
                3 * magnitude,
                5 * magnitude,
            ] {
                let bottom = low.div_euclid(step) * step;
                let top = -(-high).div_euclid(step) * step;
                if (top - bottom) / step <= MAX_RSSI_INTERVALS {
                    return Self { bottom, top, step };
                }
            }
            magnitude *= 10;
        }
    }

    fn ticks(&self) -> impl Iterator<Item = i32> {
        (self.bottom..=self.top).step_by(self.step as usize)
    }

    fn fraction(&self, rssi: i32) -> f32 {
        (self.top - rssi) as f32 / (self.top - self.bottom) as f32
    }
}

struct Bar {
    left: f32,
    width: f32,
    top: f32,
}

fn append_cross(path: &mut PathBuilder, x: f32, y: f32, half: f32) {
    path.move_to(point(px(x - half), px(y - half)));
    path.line_to(point(px(x + half), px(y + half)));
    path.move_to(point(px(x - half), px(y + half)));
    path.line_to(point(px(x + half), px(y - half)));
}

/// Numeric coordinates and cache identity are prepared only when data changes.
/// No resampling, recommendation calculation or zero-filled missing channels.
pub(super) struct ChartData {
    pub(super) report: Arc<ScanReport>,
    x: [f32; 2],
    x_scale: f32,
    y: RssiAxis,
    x_ticks: Vec<u8>,
    missing: Vec<u8>,
    fingerprint: u64,
}

impl ChartData {
    pub(super) fn new(report: Arc<ScanReport>) -> Self {
        let (low, high) = report.channel_range;
        let x = [f32::from(low) - 0.5, f32::from(high) + 0.5];
        let x_scale = 1. / (x[1] - x[0]);
        let step = match high.saturating_sub(low) {
            0..=8 => 1,
            9..=20 => 5,
            21..=60 => 10,
            61..=120 => 20,
            _ => 50,
        };
        let mut hash = DefaultHasher::new();
        report.channel_range.hash(&mut hash);
        report.current_channel.hash(&mut hash);
        let mut x_ticks: Vec<_> = (low..=high).filter(|channel| channel % step == 0).collect();
        if x_ticks.is_empty() {
            x_ticks.push(low);
        }
        std::mem::discriminant(&report.recommendation).hash(&mut hash);
        match report.recommendation {
            Recommendation::Channel(sample) => {
                sample.channel.hash(&mut hash);
                sample.rssi_dbm.hash(&mut hash);
            }
            Recommendation::Unknown | Recommendation::None => {}
        }
        report.samples.len().hash(&mut hash);
        let mut observed = [false; 256];
        let mut extent: Option<(i32, i32)> = None;
        for sample in &report.samples {
            sample.channel.hash(&mut hash);
            sample.rssi_dbm.hash(&mut hash);
            observed[usize::from(sample.channel)] = true;
            let rssi = i32::from(sample.rssi_dbm);
            extent = Some(match extent {
                Some((minimum, maximum)) => (minimum.min(rssi), maximum.max(rssi)),
                None => (rssi, rssi),
            });
        }
        let channel_count = usize::from(high) - usize::from(low) + 1;
        let mut missing = Vec::with_capacity(channel_count.saturating_sub(report.samples.len()));
        missing.extend((low..=high).filter(|&channel| !observed[usize::from(channel)]));
        Self {
            report,
            x,
            x_scale,
            y: RssiAxis::new(extent),
            missing,
            x_ticks,
            fingerprint: hash.finish(),
        }
    }

    pub(super) fn has_missing(&self) -> bool {
        !self.missing.is_empty()
    }

    fn channel_fraction(&self, channel: u8) -> f32 {
        (f32::from(channel) - self.x[0]) * self.x_scale
    }

    fn rssi_fraction(&self, rssi: i32) -> f32 {
        self.y.fraction(rssi)
    }

    fn channel_slot(&self, channel: u8) -> (f32, f32) {
        let width = BAR_CHANNEL_WIDTH * self.x_scale;
        (self.channel_fraction(channel) - width / 2., width)
    }

    fn bar(&self, sample: &Sample) -> Bar {
        let (left, width) = self.channel_slot(sample.channel);
        Bar {
            left,
            width,
            top: self.rssi_fraction(i32::from(sample.rssi_dbm)),
        }
    }

    fn path(&self, layer: usize, width: f32, height: f32, rem: f32) -> Option<Path<Pixels>> {
        let slot_width = BAR_CHANNEL_WIDTH * self.x_scale * width;
        let missing_stroke = (slot_width * 0.35).min(1.);
        let mut path = if layer == 1 {
            PathBuilder::fill()
        } else {
            PathBuilder::stroke(px(match layer {
                2 => 2.5,
                3 => 1.5,
                4 => missing_stroke,
                _ => 1.,
            }))
        };
        if layer == 2 {
            path = path.dash_array(&[px(5.), px(4.)]);
        } else if layer == 3 {
            path = path.dash_array(&[px(1.5), px(3.)]);
        }
        let at = |x, y| point(px(x), px(y));
        let mut vertical = |channel| {
            let x = self.channel_fraction(channel) * width;
            path.move_to(at(x, 0.));
            path.line_to(at(x, height));
        };
        match layer {
            0 => {
                for &channel in &self.x_ticks {
                    vertical(channel);
                }
                for rssi in self.y.ticks() {
                    let y = self.rssi_fraction(rssi) * height;
                    path.move_to(at(0., y));
                    path.line_to(at(width, y));
                }
            }
            1 => {
                for sample in &self.report.samples {
                    let bar = self.bar(sample);
                    let left = bar.left * width;
                    let right = (bar.left + bar.width) * width;
                    let top = bar.top * height;
                    path.move_to(at(left, height));
                    path.line_to(at(left, top));
                    path.line_to(at(right, top));
                    path.line_to(at(right, height));
                    path.close();
                }
            }
            2 => {
                if let Some(channel) = self.report.current_channel {
                    vertical(channel);
                }
            }
            3 => {
                if let Recommendation::Channel(sample) = self.report.recommendation {
                    vertical(sample.channel);
                }
            }
            4 => {
                let half = ((slot_width - missing_stroke) / 2.)
                    .min(rem * MISSING_MARK_HALF_REM)
                    .min((height - missing_stroke).max(0.) / 2.);
                let inset = (rem * MISSING_MARK_INSET_REM)
                    .min((height - 2. * half - missing_stroke).max(0.));
                let y = height - inset - half - missing_stroke / 2.;
                if half > 0. {
                    for &channel in &self.missing {
                        append_cross(&mut path, self.channel_fraction(channel) * width, y, half);
                    }
                }
            }
            _ => unreachable!(),
        }
        path.build().ok()
    }
}

/// Semantic colors remain distinct even when the toolkit chart palette is monochrome.
pub(super) fn series_colors(cx: &App) -> [Hsla; 3] {
    if cx.theme().is_dark() {
        [cx.theme().info, cx.theme().warning, cx.theme().success]
    } else {
        [
            cx.theme().info_active,
            cx.theme().warning_active,
            cx.theme().success_active,
        ]
    }
}

fn missing_ink(cx: &App) -> Hsla {
    let mut ink = cx.theme().muted_foreground;
    ink.a *= 0.40;
    ink
}

pub(super) fn missing_legend(text: String, cx: &App) -> AnyElement {
    let ink = missing_ink(cx);
    let swatch = canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let mut path = PathBuilder::stroke(px(1.));
            let width = f32::from(bounds.size.width);
            let height = f32::from(bounds.size.height);
            let half = ((width.min(height) - 1.) / 2.)
                .max(0.)
                .min(f32::from(window.rem_size()) * MISSING_MARK_HALF_REM);
            append_cross(&mut path, width / 2., height / 2., half);
            path.translate(bounds.origin);
            if let Ok(path) = path.build() {
                window.paint_path(path, ink);
            }
        },
    )
    .size_full();
    let legend = div()
        .id("rssi-no-sample-legend")
        .flex()
        .items_center()
        .gap_1()
        .child(div().w_4().h_2().child(swatch))
        .child(text);
    #[cfg(test)]
    let legend = legend.test_support();
    legend.into_any_element()
}

pub(super) fn legend(text: String, ink: Hsla) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap_1()
        .child(div().w_4().h_0p5().bg(ink))
        .child(text)
        .into_any_element()
}

pub(super) fn chart(data: Arc<ChartData>, cx: &App) -> AnyElement {
    let border = cx.theme().chart_grid;
    let muted = cx.theme().muted_foreground;
    let [measured, current, recommended] = series_colors(cx);
    let inks = [border, measured, current, recommended, missing_ink(cx)];
    let painted = data.clone();
    let signal = canvas(
        |_, _, _| (),
        move |bounds, _, window, cx| {
            let clip = window.content_mask().bounds;
            if bounds.right() <= clip.left()
                || bounds.left() >= clip.right()
                || bounds.bottom() <= clip.top()
                || bounds.top() >= clip.bottom()
            {
                return;
            }
            let width = f32::from(bounds.size.width);
            let height = f32::from(bounds.size.height);
            if width <= 0. || height <= 0. {
                return;
            }
            let rem = f32::from(window.rem_size());
            let key = ShapeKey::new((
                painted.fingerprint,
                width.to_bits(),
                height.to_bits(),
                rem.to_bits(),
            ))
            .finish();
            let caches = PathCaches::for_paint("rssi-channel-paths", window, cx);
            window.with_content_mask(Some(ContentMask { bounds }), |window| {
                caches.update(cx, |caches, _| {
                    for layer in [0, 4, 1, 2, 3] {
                        if layer == 4 && painted.missing.is_empty() {
                            continue;
                        }
                        if let Some(path) = caches.slot(layer).get(key, bounds.origin, || {
                            painted.path(layer, width, height, rem)
                        }) {
                            let mut ink = inks[layer];
                            if layer == 0 {
                                ink.a *= 0.55;
                            }
                            window.paint_path(path, ink);
                        }
                    }
                });
            });
        },
    )
    .size_full();
    let plot = div()
        .relative()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .border_1()
        .border_color(border)
        .child(div().absolute().inset_0().child(signal));
    #[cfg(test)]
    let plot = plot.id("rssi-plot-area").test_support();
    let mut plot = plot;
    for &channel in &data.missing {
        let text = format!(
            "{} {} · {}",
            tr("rssi.chart.tooltip.channel"),
            channel,
            tr("rssi.chart.legend.no_sample")
        );
        let (left, width) = data.channel_slot(channel);
        let target = div()
            .id(SharedString::from(format!("rssi-unsampled-{channel}")))
            .absolute()
            .left(relative(left))
            .top_0()
            .w(relative(width))
            .h_full()
            .tooltip(move |window, cx| Tooltip::new(text.clone()).build(window, cx))
            .tooltip_show_delay(Duration::ZERO);
        #[cfg(test)]
        let target = target.test_support();
        plot = plot.child(target);
    }
    for sample in &data.report.samples {
        let text = format!(
            "{} {} · {} dBm",
            tr("rssi.chart.tooltip.channel"),
            sample.channel,
            sample.rssi_dbm
        );
        let bar = data.bar(sample);
        let target = div()
            .id(SharedString::from(format!(
                "rssi-sample-{}",
                sample.channel
            )))
            .absolute()
            .left(relative(bar.left))
            .top(relative(bar.top))
            .w(relative(bar.width))
            .h(relative(1. - bar.top))
            .tooltip(move |window, cx| Tooltip::new(text.clone()).build(window, cx))
            .tooltip_show_delay(Duration::ZERO);
        #[cfg(test)]
        let target = target.test_support();
        plot = plot.child(target);
    }
    let channel_axis = div()
        .id("rssi-chart-axis-channel")
        .flex_shrink_0()
        .ml_16()
        .mr_5()
        .text_center()
        .text_color(muted)
        .child(tr("rssi.chart.axis.channel"));
    #[cfg(test)]
    let channel_axis = channel_axis.test_support();
    let chart = div()
        .id("rssi-chart")
        .w_full()
        .min_w_0()
        .min_h(rems(12.))
        .flex()
        .flex_col()
        .flex_1()
        .text_xs()
        .child(
            div()
                .flex_shrink_0()
                .ml_16()
                .mb_1()
                .text_color(muted)
                .child(tr("rssi.chart.axis.signal")),
        )
        .child(
            div()
                .flex()
                .w_full()
                .flex_1()
                .min_h(rems(7.5))
                .pr_5()
                .child(
                    div()
                        .relative()
                        .w_16()
                        .h_full()
                        .flex_shrink_0()
                        .line_height(rems(1.))
                        .text_color(muted)
                        .children(data.y.ticks().map(|value| {
                            // Reserve room at the baseline for the first x-axis label.
                            let label = div()
                                .absolute()
                                .right_6()
                                .top(relative(data.rssi_fraction(value)))
                                .mt(-rems(0.5))
                                .child(value.to_string());
                            #[cfg(test)]
                            let label = label
                                .id(SharedString::from(format!("rssi-y-tick-{value}")))
                                .test_support();
                            label
                        })),
                )
                .child(plot),
        )
        .child(
            div()
                .relative()
                .flex_shrink_0()
                .ml_16()
                .mr_5()
                .h_6()
                .text_color(muted)
                .children(data.x_ticks.iter().map(|&value| {
                    let label = div()
                        .absolute()
                        .top_1()
                        .left(relative(data.channel_fraction(value)))
                        .ml(-rems(1.25))
                        .w_10()
                        .text_center()
                        .child(value.to_string());
                    #[cfg(test)]
                    let label = label
                        .id(SharedString::from(format!("rssi-x-tick-{value}")))
                        .test_support();
                    label
                })),
        )
        .child(channel_axis);
    #[cfg(test)]
    let chart = chart.test_support();
    chart.into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rssi_scan::Sample;

    fn data(samples: Vec<Sample>, range: (u8, u8)) -> ChartData {
        ChartData::new(Arc::new(ScanReport {
            channel_range: range,
            current_channel: Some(84),
            recommendation: Recommendation::Channel(Sample {
                channel: 86,
                rssi_dbm: -69,
            }),
            samples,
            excluded_channels: vec![4],
        }))
    }

    fn path_contains(path: &Path<Pixels>, x: f32, y: f32) -> bool {
        path.vertices.as_chunks::<3>().0.iter().any(|triangle| {
            let side = |a: usize, b: usize| {
                let a = triangle[a].xy_position;
                let b = triangle[b].xy_position;
                (f32::from(b.x) - f32::from(a.x)) * (y - f32::from(a.y))
                    - (f32::from(b.y) - f32::from(a.y)) * (x - f32::from(a.x))
            };
            let sides = [side(0, 1), side(1, 2), side(2, 0)];
            sides.iter().all(|value| *value >= 0.) || sides.iter().all(|value| *value <= 0.)
        })
    }

    #[test]
    fn unsampled_marks_are_small_and_confined_to_missing_slots_near_baseline() {
        let data = data(
            vec![
                Sample {
                    channel: 0,
                    rssi_dbm: -100,
                },
                Sample {
                    channel: 2,
                    rssi_dbm: -40,
                },
                Sample {
                    channel: 8,
                    rssi_dbm: -127,
                },
            ],
            (0, 8),
        );
        assert_eq!(data.missing, [1, 3, 4, 5, 6, 7]);
        assert!(data.has_missing());
        assert_eq!(data.report.samples.len(), 3);
        let marks = data.path(4, 900., 260., 16.).expect("unsampled marks");
        assert!(!marks.vertices.is_empty());
        assert!(marks.bounds.top() >= px(260. - 16.));
        assert!(marks.bounds.bottom() <= px(260.));
        assert!(marks.bounds.size.height <= px(16. * 2. * MISSING_MARK_HALF_REM + 1.));
        let mark_y = f32::from(marks.bounds.center().y);
        for &channel in &data.missing {
            let x = data.channel_fraction(channel) * 900.;
            assert!(
                path_contains(&marks, x, mark_y),
                "missing channel {channel}"
            );
            assert!(!path_contains(&marks, x, 130.), "mark has no RSSI height");
        }
        for channel in [0, 2, 8] {
            let x = data.channel_fraction(channel) * 900.;
            assert!(
                !path_contains(&marks, x, mark_y),
                "measured channel {channel}"
            );
        }
        assert!(!path_contains(&marks, 100., mark_y), "ordinary slot gap");
        for vertex in &marks.vertices {
            let x = f32::from(vertex.xy_position.x);
            assert!(
                data.missing.iter().any(|&channel| {
                    let (left, width) = data.channel_slot(channel);
                    x >= left * 900. - 0.001 && x <= (left + width) * 900. + 0.001
                }),
                "mark must stay within a missing slot: {x}"
            );
        }
        let zoomed = data.path(4, 900., 260., 20.).expect("zoomed missing marks");
        assert!(zoomed.bounds.size.height > marks.bounds.size.height);
        assert!(zoomed.bounds.size.height <= px(20. * 2. * MISSING_MARK_HALF_REM + 1.));
    }

    #[test]
    fn new_measurement_replaces_its_missing_mark_and_dense_range_remains_bounded() {
        let first = Sample {
            channel: 0,
            rssi_dbm: -60,
        };
        let last = Sample {
            channel: 2,
            rssi_dbm: -40,
        };
        let initial = data(vec![first, last], (0, 2));
        let completed = data(
            vec![
                first,
                Sample {
                    channel: 1,
                    rssi_dbm: -50,
                },
                last,
            ],
            (0, 2),
        );
        assert_eq!(initial.missing, [1]);
        assert!(completed.missing.is_empty());
        assert!(!completed.has_missing());
        assert_ne!(initial.fingerprint, completed.fingerprint);

        let dense = data(vec![first], (0, 255));
        assert_eq!(dense.missing.len(), 255);
        let marks = dense
            .path(4, 400., 4000., 16.)
            .expect("dense missing marks");
        assert!(!marks.vertices.is_empty());
        assert!(marks.bounds.top() >= px(4000. - 16.));
    }

    #[test]
    fn sparse_channels_keep_numeric_spacing_and_no_invented_measurements() {
        let data = data(
            vec![
                Sample {
                    channel: 0,
                    rssi_dbm: -42,
                },
                Sample {
                    channel: 8,
                    rssi_dbm: -43,
                },
                Sample {
                    channel: 86,
                    rssi_dbm: -69,
                },
            ],
            (0, 88),
        );
        let short = data.channel_fraction(8) - data.channel_fraction(0);
        let long = data.channel_fraction(86) - data.channel_fraction(8);
        assert!((long / short - 78. / 8.).abs() < 0.001);
        assert_eq!(data.report.samples.len(), 3);
        assert!(
            !data
                .report
                .samples
                .iter()
                .any(|sample| sample.channel == 84)
        );
        assert!(data.rssi_fraction(-69) > data.rssi_fraction(-42));
    }

    #[test]
    fn auto_scale_pads_extremes_and_equal_values_with_bounded_ticks() {
        let singleton = data(
            vec![Sample {
                channel: 3,
                rssi_dbm: 0,
            }],
            (3, 3),
        );
        assert_eq!(singleton.x_ticks, [3]);
        assert_eq!(singleton.channel_fraction(3), 0.5);

        for values in [
            &[-127][..],
            &[0],
            &[-69, -69],
            &[-127, 0],
            &[-100, -99],
            &[-85, -40],
        ] {
            let samples = values
                .iter()
                .enumerate()
                .map(|(channel, &rssi_dbm)| Sample {
                    channel: channel as u8,
                    rssi_dbm,
                })
                .collect();
            let data = data(samples, (0, 88));
            let minimum = i32::from(*values.iter().min().unwrap());
            let maximum = i32::from(*values.iter().max().unwrap());
            let padding = ((maximum - minimum + 9) / 10).max(5);
            assert!(data.y.bottom < minimum && data.y.top > maximum);
            assert!(minimum - data.y.bottom <= padding + data.y.step);
            assert!(data.y.top - maximum <= padding + data.y.step);
            let ticks: Vec<_> = data.y.ticks().collect();
            assert!(
                (2..=7).contains(&ticks.len()),
                "ticks for {values:?}: {ticks:?}"
            );
            assert_eq!(ticks.first(), Some(&data.y.bottom));
            assert_eq!(ticks.last(), Some(&data.y.top));
            assert_eq!(data.rssi_fraction(data.y.bottom), 1.);
            assert_eq!(data.rssi_fraction(data.y.top), 0.);
            for sample in &data.report.samples {
                let top = data.rssi_fraction(i32::from(sample.rssi_dbm));
                assert!((0.0..1.0).contains(&top) && top > 0., "{sample:?}");
            }
        }
        let empty = data(Vec::new(), (0, 8));
        assert!(empty.report.samples.is_empty());
        assert!(empty.y.bottom < empty.y.top);
    }

    #[test]
    fn bar_geometry_tracks_auto_scaled_true_dbm_endpoints() {
        let initial = data(
            vec![
                Sample {
                    channel: 2,
                    rssi_dbm: -100,
                },
                Sample {
                    channel: 6,
                    rssi_dbm: -90,
                },
            ],
            (0, 8),
        );
        let within_extent = data(
            vec![
                initial.report.samples[0],
                Sample {
                    channel: 4,
                    rssi_dbm: -95,
                },
                initial.report.samples[1],
            ],
            (0, 8),
        );
        let updated = data(
            vec![
                Sample {
                    channel: 0,
                    rssi_dbm: -127,
                },
                initial.report.samples[0],
                initial.report.samples[1],
                Sample {
                    channel: 8,
                    rssi_dbm: 0,
                },
            ],
            (0, 8),
        );
        let original = initial.bar(&initial.report.samples[0]);
        let unchanged = within_extent.bar(&within_extent.report.samples[0]);
        let retained = updated.bar(&updated.report.samples[1]);
        assert_eq!(original.left, retained.left);
        assert_eq!(original.width, retained.width);
        assert_eq!(original.top, unchanged.top);
        assert_ne!(original.top, retained.top);
        assert_ne!(initial.fingerprint, updated.fingerprint);
        for sample in &updated.report.samples {
            let top = updated.bar(sample).top;
            let expected = (updated.y.top - i32::from(sample.rssi_dbm)) as f32
                / (updated.y.top - updated.y.bottom) as f32;
            assert!((top - expected).abs() < 0.000_001);
        }
        assert!(updated.bar(&updated.report.samples[3]).top < retained.top);
        assert!(updated.bar(&updated.report.samples[0]).top > retained.top);
    }

    #[test]
    fn batched_bar_path_fills_only_measured_channels_and_preserves_gaps() {
        let data = data(
            vec![
                Sample {
                    channel: 0,
                    rssi_dbm: -100,
                },
                Sample {
                    channel: 2,
                    rssi_dbm: -40,
                },
                Sample {
                    channel: 8,
                    rssi_dbm: -127,
                },
            ],
            (0, 8),
        );
        let path = data.path(1, 900., 260., 16.).expect("filled bar path");
        let contains = |x, y| path_contains(&path, x, y);
        for sample in &data.report.samples {
            let bar = data.bar(sample);
            let x = (bar.left + bar.width / 2.) * 900.;
            let y = (bar.top + 1.) * 130.;
            assert!(contains(x, y), "missing bar for channel {}", sample.channel);
            assert!(
                !contains(x, bar.top * 260. - 1.),
                "bar exceeds its measured endpoint"
            );
        }
        for channel in [1, 4, 7] {
            let x = data.channel_fraction(channel) * 900.;
            assert!(!contains(x, 259.), "fabricated bar at channel {channel}");
        }
        let first = data.bar(&data.report.samples[0]);
        let second = data.bar(&data.report.samples[1]);
        assert!((first.width * 900. - 80.).abs() < 0.001);
        assert!((second.left - first.left - 2. / 9.).abs() < 0.000_001);
        assert!(
            !contains(50., first.top * 260. / 2.),
            "bar extends above its dBm endpoint"
        );
    }

    #[test]
    fn current_and_recommended_paths_are_independent_of_sample_bars() {
        let data = data(
            vec![Sample {
                channel: 2,
                rssi_dbm: -70,
            }],
            (0, 88),
        );
        let bars = data.path(1, 890., 260., 16.).expect("measured path");
        let current = data.path(2, 890., 260., 16.).expect("current marker");
        let recommended = data
            .path(3, 890., 260., 16.)
            .expect("recommendation marker");
        assert!(!bars.vertices.is_empty());
        assert!(!current.vertices.is_empty());
        assert!(!recommended.vertices.is_empty());
        assert!(
            current.bounds.origin.x > bars.bounds.origin.x + bars.bounds.size.width,
            "current marker must remain visible without a measured bar"
        );
        assert!(
            recommended.bounds.origin.x > current.bounds.origin.x,
            "recommendation has its own channel position"
        );
    }

    #[test]
    fn parsed_recommendation_and_samples_have_distinct_same_viewport_cache_keys() {
        let prefix = "RSSI scan (ESB channels 0..88): samples/ch/sweep=50, sweeps=200 (total=10000 samples/ch)\nCurrent channel (effective): 84\n";
        let recommended = crate::rssi_scan::parse_log(&format!(
            "{prefix}Recommended: channel 2 (-69 dBm)\nAll channels sorted by RSSI (lower is better):\n86 (-66 dBm)\n"
        )).unwrap();
        let unknown = crate::rssi_scan::parse_log(&format!(
            "{prefix}All channels sorted by RSSI (lower is better):\n2 (-69 dBm) 86 (-66 dBm)\n"
        ))
        .unwrap();
        let recommended = ChartData::new(recommended);
        let unknown = ChartData::new(unknown);
        assert_ne!(recommended.fingerprint, unknown.fingerprint);
        let key = |data: &ChartData| {
            ShapeKey::new((data.fingerprint, 800_f32.to_bits(), 400_f32.to_bits())).finish()
        };
        assert_ne!(key(&recommended), key(&unknown));
    }
}
