use super::{TemperatureWindow, full_range, plot};
use crate::i18n::tr;
use gpui_kit::component::{ActiveTheme, Disableable, Sizable, TitleBar, button::Button};
use gpui_kit::{
    AnyElement, Context, FontWeight, IntoElement, Render, SharedString, Window, div, prelude::*, px,
};

impl Render for TemperatureWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .min_w_0()
            .min_h_0()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                TitleBar::new()
                    .text_sm()
                    .child(tr("temperature.window.title")),
            )
            .child(self.content(window, cx))
    }
}

impl TemperatureWindow {
    fn content(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.document.busy || self.chooser.is_some();
        // This window has no main navigation. Measure only its own content padding.
        let content_width = f32::from(window.viewport_size().width) - 16.;
        let columns = content_width >= 1120.;
        let mut heading = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(tr("temperature.window.title")),
            )
            .child(
                Button::new("tcal-import")
                    .small()
                    .label(tr("temperature.import.action"))
                    .tooltip(tr("temperature.import.hint.read_only"))
                    .disabled(busy)
                    .on_click(cx.listener(|this, _, _, cx| this.choose_dump(cx))),
            )
            .child(
                Button::new("tcal-svg")
                    .small()
                    .label(tr("temperature.export.action"))
                    .disabled(self.document.analysis.is_none() || self.export.is_some())
                    .on_click(cx.listener(|this, _, _, cx| this.export_svg(cx))),
            );
        if let Some(source) = &self.document.source {
            let filename = std::path::Path::new(source).file_name().map_or_else(
                || source.clone(),
                |name| name.to_string_lossy().into_owned(),
            );
            heading = heading.child(
                div()
                    .min_w_0()
                    .max_w(px(240.))
                    .truncate()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(filename),
            );
        }
        let page = div()
            .id("temperature-window-content")
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .p_2()
            .overflow_y_scroll();
        // One nonshrinking child keeps footers reachable at short window heights.
        let mut content = div()
            .w_full()
            .flex()
            .flex_col()
            .flex_shrink_0()
            .gap_2()
            .child(heading);
        if !self.trackers.is_empty() {
            let mut acquisition = div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                .child(div().text_xs().child(tr("temperature.dump.tracker_label")));
            for (port, generation, device_label) in &self.trackers {
                let port = port.clone();
                let generation = *generation;
                let active = self
                    .active
                    .iter()
                    .any(|request| request.port == port && request.generation == generation);
                let label = format!("{device_label} · tcal dump");
                acquisition = acquisition.child(
                    Button::new(SharedString::from(format!("tcal-read-{port}")))
                        .small()
                        .label(label)
                        .disabled(active)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.acquire(port.clone(), generation, cx)
                        })),
                );
            }
            content = content.child(acquisition);
        }
        for request in &self.active {
            let request = request.clone();
            let label = format!(
                "{}: {} · #{}",
                tr("temperature.dump.status.waiting"),
                request.port,
                request.generation
            );
            content = content.child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .child(div().text_xs().child(label))
                    .child(
                        Button::new(SharedString::from(format!(
                            "tcal-cancel-{}-{}",
                            request.port, request.request_id
                        )))
                        .small()
                        .label(tr("temperature.dump.action.cancel"))
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.cancel(request.clone(), cx)),
                        ),
                    ),
            );
        }
        if busy {
            content = content.child(
                div()
                    .text_xs()
                    .child(tr("temperature.import.status.reading")),
            );
        }
        if let Some(status) = &self.document.status {
            content = content.child(div().text_xs().child(status.clone()));
        }
        if let Some(error) = &self.document.error {
            content = content.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().danger)
                    .child(error.clone()),
            );
        }
        let Some(analysis) = self.document.analysis.clone() else {
            return page
                .child(content.child(div().text_sm().child(tr("temperature.empty.no_dump"))))
                .into_any_element();
        };
        let count = analysis.axes[0].raw.len();
        if count == 0 {
            return page
                .child(content.child(div().text_sm().child(tr("temperature.empty.no_points"))))
                .into_any_element();
        }
        if count == 1 {
            content = content.child(div().text_xs().child(tr("temperature.hint.single_point")));
        }
        let range = self.document.range.unwrap_or_else(|| full_range(&analysis));
        let mut controls = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .child(
                Button::new("tcal-pan-left")
                    .small()
                    .label("←")
                    .accessibility_label(tr("temperature.view.action.lower_temperature"))
                    .tooltip(tr("temperature.view.action.lower_temperature"))
                    .on_click(cx.listener(|this, _, _, cx| this.navigate(1., -0.25, cx))),
            )
            .child(
                Button::new("tcal-zoom-in")
                    .small()
                    .label(tr("temperature.view.action.zoom_in"))
                    .on_click(cx.listener(|this, _, _, cx| this.navigate(0.5, 0., cx))),
            )
            .child(
                Button::new("tcal-zoom-out")
                    .small()
                    .label(tr("temperature.view.action.zoom_out"))
                    .on_click(cx.listener(|this, _, _, cx| this.navigate(2., 0., cx))),
            )
            .child(
                Button::new("tcal-pan-right")
                    .small()
                    .label("→")
                    .accessibility_label(tr("temperature.view.action.higher_temperature"))
                    .tooltip(tr("temperature.view.action.higher_temperature"))
                    .on_click(cx.listener(|this, _, _, cx| this.navigate(1., 0.25, cx))),
            )
            .child(
                Button::new("tcal-reset")
                    .small()
                    .label(tr("temperature.view.action.reset"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(analysis) = &this.document.analysis {
                            this.document.range = Some(full_range(analysis));
                        }
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!(
                        "{:.1}…{:.1} °C · {count} {}",
                        range[0],
                        range[1],
                        tr("temperature.view.point_count_unit")
                    )),
            );
        if let Some([low, high]) = analysis.temperature_range_c {
            controls = controls.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!(
                        "{} {low:.2}…{high:.2} °C",
                        tr("temperature.view.measured_range")
                    )),
            );
        }
        // Stacked charts stay bounded; plot owns width-derived square geometry.
        let mut charts = div()
            .w_full()
            .min_w_0()
            .flex()
            .gap_4()
            .flex_shrink_0()
            .when(!columns, |row| row.flex_col().items_center());
        for axis in 0..3 {
            let chart = self.charts.get(&analysis, axis, range);
            charts = charts.child(
                div()
                    .min_w_0()
                    .when(columns, |column| column.flex_1())
                    .when(!columns, |column| column.w_full().max_w(px(520.)))
                    .child(plot::chart(chart, cx)),
            );
        }
        page.child(content.child(controls).child(charts))
            .into_any_element()
    }
}
