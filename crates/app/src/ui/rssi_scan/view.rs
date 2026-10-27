use super::{RssiWindow, plot};
use crate::{i18n::tr, rssi_scan::Recommendation};
use gpui_kit::component::{ActiveTheme, Disableable, Sizable, TitleBar, button::Button};
use gpui_kit::{AnyElement, Context, FontWeight, IntoElement, Render, Window, div, prelude::*};

impl Render for RssiWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut content = div()
            .w_full()
            .flex()
            .flex_col()
            .flex_shrink_0()
            .gap_2()
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(tr("rssi.window.title")),
                    )
                    .child(
                        Button::new("rssi-import")
                            .small()
                            .label(tr("rssi.import.action"))
                            .disabled(self.chooser.is_some())
                            .on_click(cx.listener(|this, _, _, cx| this.choose_log(cx))),
                    ),
            );
        if let Some(source) = &self.document.source {
            content = content.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(source.clone()),
            );
        }
        if self.document.importing {
            content = content.child(div().text_xs().child(tr("rssi.import.status.reading")));
        }
        if let Some(error) = &self.document.error {
            content = content.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().danger)
                    .child(error.clone()),
            );
        }
        content = if let Some(data) = self.document.chart.clone() {
            let report = &data.report;
            let mut metadata = div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_x_4()
                .gap_y_1()
                .text_xs()
                .child(format!(
                    "{}: {}",
                    tr("rssi.scan.measured_count"),
                    report.samples.len()
                ))
                .child(format!(
                    "{}: {}–{}",
                    tr("rssi.scan.channel_range"),
                    report.channel_range.0,
                    report.channel_range.1
                ));
            if !report.excluded_channels.is_empty() {
                metadata = metadata.child(format!(
                    "{}: {}",
                    tr("rssi.scan.excluded_channels"),
                    report
                        .excluded_channels
                        .iter()
                        .map(u8::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            content = content.child(metadata);
            let [measured_ink, current_ink, recommended_ink] = plot::series_colors(cx);
            let mut markers = div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_x_4()
                .gap_y_1()
                .text_xs()
                .child(plot::legend(tr("rssi.chart.legend.measured"), measured_ink));
            if let Some(channel) = report.current_channel {
                let measured = report
                    .samples
                    .iter()
                    .find(|sample| sample.channel == channel)
                    .map(|sample| format!(" ({} dBm)", sample.rssi_dbm))
                    .unwrap_or_default();
                markers = markers.child(plot::legend(
                    format!("{}: {channel}{measured}", tr("rssi.chart.legend.current")),
                    current_ink,
                ));
            }
            match report.recommendation {
                Recommendation::Unknown => {}
                Recommendation::None => {
                    markers = markers.child(
                        div()
                            .text_color(cx.theme().muted_foreground)
                            .child(tr("rssi.chart.legend.no_recommendation")),
                    );
                }
                Recommendation::Channel(sample) => {
                    markers = markers.child(plot::legend(
                        format!(
                            "{}: {} ({} dBm)",
                            tr("rssi.chart.legend.recommended"),
                            sample.channel,
                            sample.rssi_dbm
                        ),
                        recommended_ink,
                    ));
                }
            }
            content = content.child(markers);
            if report.samples.is_empty() {
                content = content.child(
                    div()
                        .py_4()
                        .text_sm()
                        .child(tr("rssi.empty.no_measurements")),
                );
            } else {
                if let Some(index) = self.document.selected_sample_index() {
                    let sample = &report.samples[index];
                    content = content.child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap_2()
                            .child(
                                Button::new("rssi-previous-sample")
                                    .small()
                                    .label(tr("rssi.inspect.action.previous"))
                                    .disabled(index == 0)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        if this.document.inspect_sample(-1) {
                                            cx.notify();
                                        }
                                    })),
                            )
                            .child(div().text_sm().child(gpui_kit::text!(
                                    id = "rssi-inspected-sample",
                                    rust_i18n::t!(
                                        "rssi.inspect.sample",
                                        channel = sample.channel,
                                        rssi = sample.rssi_dbm
                                    )
                                    .to_string()
                                )))
                            .child(
                                Button::new("rssi-next-sample")
                                    .small()
                                    .label(tr("rssi.inspect.action.next"))
                                    .disabled(index + 1 == report.samples.len())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        if this.document.inspect_sample(1) {
                                            cx.notify();
                                        }
                                    })),
                            ),
                    );
                }
                // A wide plot, not a square. Keep labels/footer reachable in small
                // native windows and let taller windows use the available height.
                let height = (f32::from(window.viewport_size().height) - 240.).clamp(160., 800.);
                content = content.child(plot::chart(data, height, cx));
            }
            content.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(tr("rssi.hint.channel_activity")),
            )
        } else {
            content.child(div().py_4().text_sm().child(tr("rssi.empty.no_scan")))
        };
        let page: AnyElement = div()
            .id("rssi-window-content")
            .w_full()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .p_3()
            .overflow_y_scroll()
            .child(content)
            .into_any_element();
        div()
            .track_focus(&self.focus)
            .size_full()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(TitleBar::new().text_sm().child(tr("rssi.window.title")))
            .child(page)
    }
}
