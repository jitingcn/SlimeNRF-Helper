use super::{Workbench, components::action_card, page_scroll::page_scroll};
use crate::i18n::tr;
use gpui_kit::assets::IconName;
use gpui_kit::component::{ActiveTheme, Icon, button::Button};
use gpui_kit::{AnyElement, App, Context, FontWeight, SharedString, div, prelude::*, rems};

impl Workbench {
    pub(super) fn tools_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let temperature_title = tr("tools.temperature.title");
        let rssi_title = tr("tools.rssi.title");
        page_scroll(
            "tools-page",
            &self.tools_page_focus,
            div()
                .w_full()
                .flex_shrink_0()
                .flex()
                .flex_col()
                .gap_4()
                .p_4()
                .child(
                    div()
                        .text_lg()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(tr("tools.title")),
                )
                .child(
                    div()
                        .w_full()
                        .flex_shrink_0()
                        .min_w_0()
                        .max_w(rems(25.))
                        .child(
                            tool_card(
                                "open-tcal-analysis",
                                temperature_title,
                                tr("tools.temperature.description"),
                                IconName::Thermometer,
                                cx,
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.open_temperature_window(cx);
                            })),
                        ),
                )
                .child(
                    div()
                        .w_full()
                        .flex_shrink_0()
                        .min_w_0()
                        .max_w(rems(25.))
                        .child(
                            tool_card(
                                "open-rssi-analysis",
                                rssi_title,
                                tr("tools.rssi.description"),
                                IconName::Activity,
                                cx,
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.open_rssi_window(cx);
                            })),
                        ),
                ),
        )
        .into_any_element()
    }
}

fn tool_card(
    id: &'static str,
    title: String,
    description: String,
    icon: IconName,
    cx: &App,
) -> Button {
    let title: SharedString = title.into();
    action_card(id.into(), true, cx)
        .accessibility_label(title.clone())
        .child(
            div()
                .flex()
                .items_start()
                .w_full()
                .min_w_0()
                .h_auto()
                .gap_2()
                .whitespace_normal()
                .child(
                    div()
                        .w_5()
                        .flex_shrink_0()
                        .text_color(cx.theme().muted_foreground)
                        .child(Icon::new(icon).size_5()),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .h_auto()
                        .gap_1()
                        .text_left()
                        .whitespace_normal()
                        .child(
                            div()
                                .min_w_0()
                                .whitespace_normal()
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(title),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .whitespace_normal()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(description),
                        ),
                ),
        )
}
