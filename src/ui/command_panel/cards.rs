use super::super::components::action_card;
use crate::{
    commands::{CommandTone, Preset},
    i18n::tr,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::{ActiveTheme, Colorize, Icon, button::Button};
use gpui_kit::{App, FontWeight, SharedString, div, prelude::*, px};

pub(super) fn command_card(id: SharedString, preset: &Preset, enabled: bool, cx: &App) -> Button {
    let theme = cx.theme();
    let title: SharedString = tr(preset.label).into();
    let description = tr(preset.description);
    // Tone is a visual hint only; confirmation remains payload/route policy.
    let severity = match preset.tone() {
        CommandTone::Normal => None,
        CommandTone::Warning => Some(("commands.tone.warning", theme.warning)),
        CommandTone::Destructive => Some(("commands.tone.destructive", theme.danger)),
    }
    .map(|(label, color)| {
        // Bright semantic fills need a darker foreground on pale card surfaces.
        let color = if theme.is_dark() {
            color
        } else {
            color.mix_oklab(theme.foreground, 0.45)
        };
        (label, color)
    });
    let icon_color = severity.map_or(theme.muted_foreground, |(_, color)| color);
    let text = div()
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
                .child(title.clone()),
        )
        .child(
            div()
                .min_w_0()
                .whitespace_normal()
                .text_xs()
                .text_color(if enabled {
                    theme.muted_foreground
                } else {
                    theme.muted_foreground.opacity(0.5)
                })
                .child(description),
        )
        .when(preset.receiver_wide_only, |text| {
            text.child(
                div()
                    .text_xs()
                    .whitespace_normal()
                    .text_color(theme.muted_foreground)
                    .child(tr("commands.review.receiver_wide_hint")),
            )
        })
        .when_some(severity, |text, (label, color)| {
            text.child(
                div().flex().child(
                    div()
                        .px_1()
                        .py_0p5()
                        .rounded_sm()
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .bg(color.opacity(if enabled { 0.1 } else { 0.05 }))
                        .text_color(if enabled { color } else { color.opacity(0.5) })
                        .child(tr(label)),
                ),
            )
        });
    action_card(id, enabled, cx)
        .accessibility_label(title)
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
                        .w(px(18.))
                        .flex_shrink_0()
                        .text_color(if enabled {
                            icon_color
                        } else {
                            icon_color.opacity(0.5)
                        })
                        .child(Icon::new(command_icon(preset)).size(px(18.))),
                )
                .child(text),
        )
}

fn command_icon(preset: &Preset) -> IconName {
    match preset.id {
        "info" => IconName::Info,
        "help" => IconName::BookOpen,
        "battery" => IconName::Battery,
        "list" => IconName::Network,
        "uptime" => IconName::Gauge,
        "scan" | "rssi-scan" => IconName::Search,
        "debug" | "debug-timed" => IconName::SquareTerminal,
        "ping" => IconName::Lightbulb,
        "range" | "range-reset" => IconName::Gauge,
        "reboot" => IconName::RotateCw,
        "dfu" | "dfu-ota" => IconName::HardDrive,
        "shutdown" => IconName::Power,
        "add" => IconName::Plus,
        "remove" => IconName::Delete,
        _ => match preset.group {
            "commands.group.information" => IconName::Info,
            "commands.group.sensor_calibration" => IconName::Axis3d,
            "commands.group.connection" | "commands.group.management" => IconName::Network,
            "commands.group.temperature_calibration" => IconName::Thermometer,
            "commands.group.magnetometer" => IconName::Compass,
            "commands.group.diagnostics" => IconName::Activity,
            "commands.group.reset" => IconName::Undo,
            "commands.group.statistics" => IconName::ChartPie,
            "commands.group.radio" => IconName::Wifi,
            _ => IconName::Cpu,
        },
    }
}
