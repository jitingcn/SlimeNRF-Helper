use crate::{commands, i18n::tr, sessions::ConnectionState};
use gpui_kit::component::{
    ActiveTheme, Colorize, Disableable, Selectable,
    button::{Button, ButtonCustomVariant, ButtonVariant, ButtonVariants},
    tooltip::Tooltip,
};
use gpui_kit::{AnyElement, App, Div, SharedString, div, prelude::*, px};

pub(super) fn clipped(id: String, text: String) -> AnyElement {
    let full = text.clone();
    div()
        .id(SharedString::from(id))
        .min_w_0()
        .w_full()
        .truncate()
        .child(text)
        .tooltip(move |window, cx| Tooltip::new(full.clone()).build(window, cx))
        .into_any_element()
}

pub(super) fn command_button_tone(button: Button, tone: commands::CommandTone, cx: &App) -> Button {
    match tone {
        commands::CommandTone::Normal => button,
        commands::CommandTone::Destructive => button.danger().outline(),
        commands::CommandTone::Warning => button
            .custom(
                ButtonCustomVariant::new(cx)
                    .color(cx.theme().warning)
                    .foreground(cx.theme().warning_foreground)
                    .hover(cx.theme().warning_hover)
                    .active(cx.theme().warning_active),
            )
            .outline(),
    }
}

pub(super) fn context_button(button: Button, selected: bool, cx: &App) -> Button {
    button
        .selected(selected)
        .toggled(selected)
        .when(!cx.theme().is_dark(), |button| {
            button.with_variant(ButtonVariant::Default)
        })
        .when(selected, |button| {
            button
                .custom(
                    ButtonCustomVariant::new(cx)
                        .color(if cx.theme().is_dark() {
                            cx.theme().accent
                        } else {
                            cx.theme().primary
                        })
                        .foreground(cx.theme().accent_foreground)
                        .hover(cx.theme().accent)
                        .active(cx.theme().accent),
                )
                .when(!cx.theme().is_dark(), |button| button.outline())
        })
}

pub(super) fn action_card(id: SharedString, enabled: bool, cx: &App) -> Button {
    let theme = cx.theme();
    Button::new(id)
        .custom(
            ButtonCustomVariant::new(cx)
                .color(theme.group_box)
                .foreground(theme.foreground)
                .hover(theme.group_box.mix_oklab(theme.accent, 0.35))
                .active(theme.group_box.mix_oklab(theme.accent, 0.6)),
        )
        // Button's default height and label wrapper are single-line; the custom
        // child must opt into intrinsic height and wrapping at every text level.
        .h_auto()
        .w_full()
        .min_w_0()
        .p_2p5()
        .border_1()
        .border_color(if enabled {
            theme.border
        } else {
            theme.border.opacity(0.5)
        })
        .whitespace_normal()
        .disabled(!enabled)
}

pub(super) const CONNECTION_ACTION_WIDTH: f32 = 144.;

pub(super) fn connection_status(
    state: Option<ConnectionState>,
    reconnecting: bool,
    cx: &App,
) -> Div {
    let color = if reconnecting {
        Some(cx.theme().warning)
    } else {
        match state {
            Some(ConnectionState::Connected) => Some(cx.theme().success),
            Some(ConnectionState::Connecting) => Some(cx.theme().warning),
            Some(ConnectionState::Closing) => Some(cx.theme().muted_foreground),
            Some(ConnectionState::Faulted) => Some(cx.theme().danger),
            _ => None,
        }
    };
    // Reserve the row slot, not the badge background.
    div()
        .flex()
        .flex_shrink_0()
        .justify_end()
        .items_center()
        .w(px(128.))
        .whitespace_nowrap()
        .child(
            div()
                .flex()
                .flex_shrink_0()
                .items_center()
                .gap_1()
                .px_1()
                .rounded_sm()
                .text_color(cx.theme().foreground)
                .when_some(color, |badge, color| {
                    badge
                        .bg(color.opacity(0.12))
                        .child(div().size(px(5.)).flex_shrink_0().rounded_sm().bg(color))
                })
                .child(if reconnecting {
                    reconnecting_label(state)
                } else {
                    state.map_or_else(|| tr("serial.connection.state.not_connected"), state_label)
                }),
        )
}

fn reconnecting_label(state: Option<ConnectionState>) -> String {
    tr(if state == Some(ConnectionState::Connecting) {
        "serial.connection.state.reconnecting"
    } else {
        "serial.connection.state.waiting_for_device"
    })
}

pub(super) fn connection_action(state: Option<ConnectionState>, reconnecting: bool) -> String {
    if reconnecting {
        return tr("serial.connection.action.stop_reconnecting");
    }
    tr(match state {
        Some(ConnectionState::Connected) => "serial.connection.action.disconnect",
        Some(ConnectionState::Connecting) => "common.action.cancel",
        Some(ConnectionState::Closing) => "serial.connection.action.closing",
        _ => "serial.connection.action.connect",
    })
}

pub(super) fn state_label(state: ConnectionState) -> String {
    tr(match state {
        ConnectionState::Connecting => "serial.connection.state.connecting",
        ConnectionState::Connected => "serial.connection.state.connected",
        ConnectionState::Closing => "serial.connection.state.closing",
        ConnectionState::Disconnected => "serial.connection.state.disconnected",
        ConnectionState::Faulted => "serial.connection.state.error",
    })
}
