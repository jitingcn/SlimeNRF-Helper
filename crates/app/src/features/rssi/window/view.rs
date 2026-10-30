use super::{RssiWindow, plot};
use crate::{i18n::tr, rssi_scan::Recommendation};
use gpui_kit::component::{ActiveTheme, Disableable, Sizable, TitleBar, button::Button};
use gpui_kit::{AnyElement, Context, FontWeight, IntoElement, Render, Window, div, prelude::*};

#[cfg(test)]
use gpui_kit::test::TestSupportExt as _;

impl Render for RssiWindow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut content = div()
            .w_full()
            .min_h_full()
            .flex()
            .flex_col()
            .flex_shrink_0()
            .p_3()
            .gap_2()
            .child(
                div()
                    .flex()
                    .flex_shrink_0()
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
            let source = div()
                .id("rssi-source")
                .min_w_0()
                .flex_shrink_0()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .whitespace_normal()
                .child(source.clone());
            #[cfg(test)]
            let source = source.test_support();
            content = content.child(source);
        }
        if self.document.importing {
            content = content.child(
                div()
                    .flex_shrink_0()
                    .text_xs()
                    .child(tr("rssi.import.status.reading")),
            );
        }
        if let Some(error) = &self.document.error {
            content = content.child(
                div()
                    .flex_shrink_0()
                    .min_w_0()
                    .text_sm()
                    .text_color(cx.theme().danger)
                    .whitespace_normal()
                    .child(error.clone()),
            );
        }
        content = if let Some(data) = self.document.chart.clone() {
            let report = &data.report;
            let mut metadata = div()
                .min_w_0()
                .flex_shrink_0()
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
                metadata = metadata.child(div().min_w_0().whitespace_normal().child(format!(
                        "{}: {}",
                        tr("rssi.scan.excluded_channels"),
                        report
                            .excluded_channels
                            .iter()
                            .map(u8::to_string)
                            .collect::<Vec<_>>()
                            .join(", ")
                    )));
            }
            content = content.child(metadata);
            let [measured_ink, current_ink, recommended_ink] = plot::series_colors(cx);
            let mut markers = div()
                .flex_shrink_0()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_x_4()
                .gap_y_1()
                .text_xs()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(div().w_4().h_2().bg(measured_ink))
                        .child(tr("rssi.chart.legend.measured")),
                );
            if data.has_missing() {
                markers =
                    markers.child(plot::missing_legend(tr("rssi.chart.legend.no_sample"), cx));
            }
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
                    let sample_text = rust_i18n::t!(
                        "rssi.inspect.sample",
                        channel = sample.channel,
                        rssi = sample.rssi_dbm
                    )
                    .to_string();
                    let inspected_sample = div().text_sm();
                    #[cfg(test)]
                    let inspected_sample = inspected_sample
                        .id("rssi-selected-sample")
                        .aria_label(sample_text.clone())
                        .test_support();
                    let inspected_sample = inspected_sample
                        .child(gpui_kit::text!(id = "rssi-inspected-sample", sample_text));
                    let inspector = div()
                        .id("rssi-inspector")
                        .flex()
                        .flex_wrap()
                        .flex_shrink_0()
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
                        .child(inspected_sample)
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
                        );
                    #[cfg(test)]
                    let inspector = inspector.test_support();
                    content = content.child(inspector);
                }
                content = content.child(plot::chart(data, cx));
            }
            let hint = div()
                .id("rssi-chart-hint")
                .min_w_0()
                .flex_shrink_0()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .whitespace_normal()
                .child(tr("rssi.hint.channel_activity"));
            #[cfg(test)]
            let hint = hint.test_support();
            content.child(hint)
        } else {
            content.child(div().py_4().text_sm().child(tr("rssi.empty.no_scan")))
        };
        let page = div()
            .id("rssi-window-content")
            .w_full()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .overflow_y_scroll()
            .child(content);
        #[cfg(test)]
        let page = page.test_support();
        let page: AnyElement = page.into_any_element();
        let root = div();
        #[cfg(test)]
        let root = root.id("rssi-window").test_support();
        root.track_focus(&self.focus)
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
