use crate::{
    i18n::tr,
    rssi_scan::{Recommendation, ScanReport},
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
};

/// Numeric coordinates and cache identity are prepared only when data changes.
/// No resampling, recommendation calculation or zero-filled missing channels.
pub(super) struct ChartData {
    pub(super) report: Arc<ScanReport>,
    x: [f32; 2],
    y: [f32; 2],
    x_ticks: Vec<u8>,
    y_ticks: Vec<i16>,
    fingerprint: u64,
}

impl ChartData {
    pub(super) fn new(report: Arc<ScanReport>) -> Self {
        let (low, high) = report.channel_range;
        let x = [f32::from(low) - 0.5, f32::from(high) + 0.5];
        let minimum = report
            .samples
            .iter()
            .map(|sample| sample.rssi_dbm)
            .min()
            .unwrap_or(-100);
        let maximum = report
            .samples
            .iter()
            .map(|sample| sample.rssi_dbm)
            .max()
            .unwrap_or(-20);
        let bottom = ((minimum - 5).div_euclid(10) * 10).max(-130);
        let top = (((maximum + 5 + 9).div_euclid(10)) * 10).min(0);
        let y = [f32::from(bottom), f32::from(top.max(bottom + 10))];
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
        let y_step = if top - bottom > 80 { 20 } else { 10 };
        let y_ticks = (bottom..=top).step_by(y_step).collect();
        std::mem::discriminant(&report.recommendation).hash(&mut hash);
        match report.recommendation {
            Recommendation::Channel(sample) => {
                sample.channel.hash(&mut hash);
                sample.rssi_dbm.hash(&mut hash);
            }
            Recommendation::Unknown | Recommendation::None => {}
        }
        report.samples.len().hash(&mut hash);
        for sample in &report.samples {
            sample.channel.hash(&mut hash);
            sample.rssi_dbm.hash(&mut hash);
        }
        Self {
            report,
            x,
            y,
            x_ticks,
            y_ticks,
            fingerprint: hash.finish(),
        }
    }

    fn channel_fraction(&self, channel: u8) -> f32 {
        (f32::from(channel) - self.x[0]) / (self.x[1] - self.x[0])
    }

    fn rssi_fraction(&self, rssi: i16) -> f32 {
        (self.y[1] - f32::from(rssi)) / (self.y[1] - self.y[0])
    }

    fn path(&self, layer: usize, width: f32, height: f32) -> Option<Path<Pixels>> {
        let mut path = if layer == 2 {
            PathBuilder::fill()
        } else {
            PathBuilder::stroke(px(match layer {
                1 => 1.8,
                3 => 2.5,
                4 => 1.5,
                _ => 1.,
            }))
        };
        if layer == 3 {
            path = path.dash_array(&[px(5.), px(4.)]);
        } else if layer == 4 {
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
                for &rssi in &self.y_ticks {
                    let y = self.rssi_fraction(rssi) * height;
                    path.move_to(at(0., y));
                    path.line_to(at(width, y));
                }
            }
            1 => {
                for (index, sample) in self.report.samples.iter().enumerate() {
                    let position = at(
                        self.channel_fraction(sample.channel) * width,
                        self.rssi_fraction(sample.rssi_dbm) * height,
                    );
                    if index == 0 {
                        path.move_to(position);
                    } else {
                        path.line_to(position);
                    }
                }
            }
            2 => {
                for sample in &self.report.samples {
                    let x = self.channel_fraction(sample.channel) * width;
                    let y = self.rssi_fraction(sample.rssi_dbm) * height;
                    let r = 3.;
                    let k = r * 0.552_284_8;
                    path.move_to(at(x + r, y));
                    path.cubic_bezier_to(at(x, y + r), at(x + r, y + k), at(x + k, y + r));
                    path.cubic_bezier_to(at(x - r, y), at(x - k, y + r), at(x - r, y + k));
                    path.cubic_bezier_to(at(x, y - r), at(x - r, y - k), at(x - k, y - r));
                    path.cubic_bezier_to(at(x + r, y), at(x + k, y - r), at(x + r, y - k));
                    path.close();
                }
            }
            3 => {
                if let Some(channel) = self.report.current_channel {
                    vertical(channel);
                }
            }
            4 => {
                if let Recommendation::Channel(sample) = self.report.recommendation {
                    vertical(sample.channel);
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

pub(super) fn legend(text: String, ink: Hsla) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap_1()
        .child(div().w_4().h_0p5().bg(ink))
        .child(text)
        .into_any_element()
}

pub(super) fn chart(data: Arc<ChartData>, height: f32, cx: &App) -> AnyElement {
    let border = cx.theme().chart_grid;
    let muted = cx.theme().muted_foreground;
    let [measured, current, recommended] = series_colors(cx);
    let inks = [border, measured, measured, current, recommended];
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
            let key =
                ShapeKey::new((painted.fingerprint, width.to_bits(), height.to_bits())).finish();
            let caches = PathCaches::for_paint("rssi-channel-paths", window, cx);
            window.with_content_mask(Some(ContentMask { bounds }), |window| {
                caches.update(cx, |caches, _| {
                    for (layer, ink) in inks.iter().enumerate() {
                        if let Some(path) = caches
                            .slot(layer)
                            .get(key, bounds.origin, || painted.path(layer, width, height))
                        {
                            let mut ink = *ink;
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
    let mut plot = div()
        .relative()
        .flex_1()
        .min_w_0()
        .h(px(height))
        .border_1()
        .border_color(border)
        .child(div().absolute().inset_0().child(signal));
    for sample in &data.report.samples {
        let text = format!(
            "{} {} · {} dBm",
            tr("rssi.chart.tooltip.channel"),
            sample.channel,
            sample.rssi_dbm
        );
        plot = plot.child(
            div()
                .id(SharedString::from(format!(
                    "rssi-sample-{}",
                    sample.channel
                )))
                .absolute()
                .left(relative(data.channel_fraction(sample.channel)))
                .top(relative(data.rssi_fraction(sample.rssi_dbm)))
                .ml(px(-6.))
                .mt(px(-6.))
                .size(px(12.))
                .tooltip(move |window, cx| Tooltip::new(text.clone()).build(window, cx)),
        );
    }
    div()
        .w_full()
        .min_w_0()
        .text_xs()
        .child(
            div()
                .ml_16()
                .mb_1()
                .text_color(muted)
                .child(tr("rssi.chart.axis.signal")),
        )
        .child(
            div()
                .flex()
                .w_full()
                .pr_5()
                .child(
                    div()
                        .relative()
                        .w_16()
                        .h(px(height))
                        .flex_shrink_0()
                        .text_color(muted)
                        .children(data.y_ticks.iter().map(|&value| {
                            div()
                                .absolute()
                                .right_2()
                                .top(relative(data.rssi_fraction(value)))
                                .mt(-rems(0.5))
                                .child(value.to_string())
                        })),
                )
                .child(plot),
        )
        .child(
            div()
                .relative()
                .ml_16()
                .mr_5()
                .h_6()
                .text_color(muted)
                .children(data.x_ticks.iter().map(|&value| {
                    div()
                        .absolute()
                        .top_1()
                        .left(relative(data.channel_fraction(value)))
                        .ml(-rems(1.25))
                        .w_10()
                        .text_center()
                        .child(value.to_string())
                })),
        )
        .child(
            div()
                .ml_16()
                .mr_5()
                .text_center()
                .text_color(muted)
                .child(tr("rssi.chart.axis.channel")),
        )
        .into_any_element()
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
    fn singleton_domain_and_full_dbm_range_remain_finite() {
        let singleton = data(
            vec![Sample {
                channel: 3,
                rssi_dbm: 0,
            }],
            (3, 3),
        );
        assert_eq!(singleton.x_ticks, [3]);
        assert_eq!(singleton.channel_fraction(3), 0.5);
        let extremes = data(
            vec![
                Sample {
                    channel: 0,
                    rssi_dbm: -127,
                },
                Sample {
                    channel: 88,
                    rssi_dbm: 0,
                },
            ],
            (0, 88),
        );
        for sample in &extremes.report.samples {
            assert!((0.0..=1.0).contains(&extremes.rssi_fraction(sample.rssi_dbm)));
        }
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
