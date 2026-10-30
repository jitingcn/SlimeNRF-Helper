use super::*;
use crate::{
    application::terminal::serial_display_name,
    domain::{DeviceRole, SerialPortKind},
    sessions::ConnectionState,
};

fn serial_dfu_detail(id: &'static str, label: &str, value: &str, cx: &App) -> AnyElement {
    let text = SharedString::from(format!("{}: {value}", tr(label)));
    let tooltip = text.clone();
    div()
        .id(id)
        .min_w_0()
        .flex_shrink_0()
        .w_full()
        .whitespace_normal()
        .text_color(cx.theme().muted_foreground)
        .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
        .child(text)
        .into_any_element()
}

impl Workbench {
    #[inline(never)]
    fn flash_serial_dfu_row(
        &self,
        candidate: &SerialDfuCandidate<'_>,
        locked: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let port = candidate.port.to_owned();
        let name = candidate
            .endpoint
            .and_then(|endpoint| self.serial_reported_name(endpoint))
            .unwrap_or_else(|| serial_display_name(candidate.port, candidate.label()));
        let title = if name.is_empty() || name == candidate.port {
            candidate.port.to_owned()
        } else {
            format!("{} · {name}", candidate.port)
        };
        let title = if let Some(endpoint) = candidate.endpoint {
            self.serial_endpoint_label(endpoint, title)
        } else if let Some(session) = candidate.session {
            self.serial_generation_label(candidate.port, session.generation, title)
        } else {
            title
        };
        context_button(
            Button::new(SharedString::from(format!(
                "serial-dfu-port-{port:?}-{:?}",
                candidate.session.map(|session| session.generation)
            )))
            .small()
            .h_auto()
            .flex_shrink_0()
            .w_full()
            .min_w_0()
            .py_1()
            .accessibility_label(title.clone())
            .tooltip(title.clone())
            .disabled(locked)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .w_full()
                    .min_w_0()
                    .text_left()
                    .child(div().min_w_0().w_full().truncate().child(title)),
            ),
            self.flash.serial_dfu.selected_port.as_deref() == Some(candidate.port),
            cx,
        )
        .on_click(cx.listener(move |this, _, _, cx| {
            this.flash_select_serial_dfu(port.clone());
            cx.notify();
        }))
        .into_any_element()
    }

    #[inline(never)]
    fn flash_serial_dfu_details(
        &self,
        candidate: &SerialDfuCandidate<'_>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut details = div()
            .id("serial-dfu-details")
            .flex()
            .flex_col()
            .gap_1()
            .w_full()
            .min_w_0()
            .max_h(px(168.))
            .flex_shrink_0()
            .overflow_y_scroll()
            .overflow_x_hidden()
            .text_xs();
        let kind = match candidate.endpoint.map(|endpoint| endpoint.kind) {
            Some(SerialPortKind::Usb) => "firmware.serial_dfu.port_kind.usb",
            Some(SerialPortKind::Bluetooth) => "firmware.serial_dfu.port_kind.bluetooth",
            Some(SerialPortKind::Pci) => "firmware.serial_dfu.port_kind.pci",
            Some(SerialPortKind::Unknown) | None => "common.value.unknown",
        };
        details = details.child(serial_dfu_detail(
            "serial-dfu-port-type",
            "firmware.serial_dfu.metadata.port_type",
            &tr(kind),
            cx,
        ));
        let visible_name = candidate
            .endpoint
            .and_then(|endpoint| self.serial_reported_name(endpoint))
            .unwrap_or_else(|| serial_display_name(candidate.port, candidate.label()))
            .trim();
        if let Some(endpoint) = candidate.endpoint {
            for (id, label, value) in [
                (
                    "serial-dfu-manufacturer",
                    "firmware.serial_dfu.metadata.manufacturer",
                    endpoint.manufacturer.as_deref(),
                ),
                (
                    "serial-dfu-product",
                    "firmware.serial_dfu.metadata.product",
                    endpoint
                        .product
                        .as_deref()
                        .filter(|product| product.trim() != visible_name),
                ),
                (
                    "serial-dfu-usb-serial",
                    "firmware.serial_dfu.metadata.usb_serial",
                    endpoint.serial_number.as_deref(),
                ),
            ] {
                if let Some(value) = value.filter(|value| !value.trim().is_empty()) {
                    details = details.child(serial_dfu_detail(id, label, value, cx));
                }
            }
            if endpoint.vid.is_some() || endpoint.pid.is_some() {
                let vid = endpoint
                    .vid
                    .map_or_else(|| "????".into(), |id| format!("{id:04X}"));
                let pid = endpoint
                    .pid
                    .map_or_else(|| "????".into(), |id| format!("{id:04X}"));
                details = details.child(serial_dfu_detail(
                    "serial-dfu-usb-id",
                    "firmware.serial_dfu.metadata.usb_id",
                    &format!("{vid}:{pid}"),
                    cx,
                ));
            }
        }
        // Closed/faulted sessions retain old identity for terminal history, not live evidence.
        if let Some(session) = candidate
            .session
            .filter(|session| session.state == ConnectionState::Connected)
        {
            let role = match session.identity.role {
                DeviceRole::Receiver => Some("common.device_role.receiver"),
                DeviceRole::Tracker => Some("common.device_role.tracker"),
                DeviceRole::Unknown => None,
            };
            if let Some(role) = role {
                details = details.child(serial_dfu_detail(
                    "serial-dfu-live-role",
                    "firmware.serial_dfu.metadata.role",
                    &tr(role),
                    cx,
                ));
            }
            for (id, label, value) in [
                (
                    "serial-dfu-live-target",
                    "firmware.serial_dfu.metadata.target",
                    session.identity.target.as_deref(),
                ),
                (
                    "serial-dfu-live-board",
                    "firmware.serial_dfu.metadata.board",
                    session.identity.board.as_deref(),
                ),
            ] {
                if let Some(value) = value.filter(|value| !value.trim().is_empty()) {
                    details = details.child(serial_dfu_detail(id, label, value, cx));
                }
            }
        }
        details.into_any_element()
    }

    #[inline(never)]
    pub(in crate::application::flash) fn flash_serial_dfu(
        &self,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let state = &self.flash.serial_dfu;
        let locked = state.busy();
        let blocked = self.flash_serial_dfu_blocked();
        let mut section = div()
            .flex()
            .flex_col()
            .gap_2()
            .min_w_0()
            .w_full()
            .pb_3()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(tr("firmware.serial_dfu.title")),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .whitespace_normal()
                    .child(tr("firmware.serial_dfu.help")),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .min_w_0()
                    .child(
                        Button::new("serial-dfu-refresh")
                            .small()
                            .icon(IconName::RefreshCw)
                            .label(tr("firmware.serial.refresh"))
                            .disabled(locked || blocked)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.scan_inventory(cx);
                                cx.notify();
                            })),
                    )
                    .child(
                        Checkbox::new("serial-dfu-show-other")
                            .label(tr("firmware.serial_dfu.show_other"))
                            .checked(state.show_other)
                            .disabled(locked)
                            .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                this.flash_show_other_serial_dfu(*checked);
                                cx.notify();
                            })),
                    ),
            );
        let mut candidates = div()
            .id("serial-dfu-candidates")
            .flex()
            .flex_col()
            .gap_1()
            .min_w_0()
            .w_full()
            .max_h(px(168.))
            .flex_shrink_0()
            .overflow_y_scroll()
            .overflow_x_hidden();
        let mut count = 0;
        let mut selected = None;
        for candidate in self.flash_serial_dfu_candidates() {
            count += 1;
            candidates = candidates.child(self.flash_serial_dfu_row(&candidate, locked, cx));
            if state.selected_port.as_deref() == Some(candidate.port) {
                selected = Some(candidate);
            }
        }
        if count == 0 {
            section = section.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .whitespace_normal()
                    .child(tr(if self.inventory.serial_enumeration_failed {
                        "firmware.serial_dfu.error.inventory_unavailable"
                    } else if state.show_other {
                        "firmware.serial_dfu.no_ports"
                    } else {
                        "firmware.serial_dfu.no_usb_ports"
                    })),
            );
        } else {
            section = section.child(candidates);
        }
        if let Some(candidate) = selected.as_ref() {
            section = section.child(self.flash_serial_dfu_details(candidate, cx));
        }
        if let Some(intent) = state.warning.as_ref() {
            section = section.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .min_w_0()
                    .p_2()
                    .rounded_md()
                    .border_1()
                    .border_color(cx.theme().warning)
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .whitespace_normal()
                            .child(format!(
                                "{} · {}",
                                tr("firmware.serial_dfu.unrecognized"),
                                intent.port
                            )),
                    )
                    .child(
                        div()
                            .text_xs()
                            .whitespace_normal()
                            .child(tr("firmware.serial_dfu.warning")),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_2()
                            .child(
                                Button::new("serial-dfu-continue")
                                    .primary()
                                    .when(!cx.theme().is_dark(), |button| button.outline())
                                    .label(tr("firmware.serial_dfu.confirm_send"))
                                    .disabled(locked || blocked)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.flash_confirm_serial_dfu(cx);
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("serial-dfu-cancel")
                                    .label(tr("common.action.cancel"))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.flash_cancel_serial_dfu();
                                        cx.notify();
                                    })),
                            ),
                    ),
            );
        } else {
            section = section.child(
                Button::new("serial-dfu-enter")
                    .primary()
                    .when(!cx.theme().is_dark(), |button| button.outline())
                    .label(tr("firmware.serial_dfu.enter"))
                    .disabled(locked || blocked || selected.is_none())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.flash_start_serial_dfu(cx);
                        cx.notify();
                    })),
            );
        }
        let mut status = div()
            .id("serial-dfu-status")
            .min_w_0()
            .w_full()
            .min_h(px(24.))
            .max_h(px(104.))
            .overflow_y_scroll()
            .overflow_x_hidden()
            .text_xs()
            .whitespace_normal();
        if let Some(message) = state.status() {
            status = status
                .text_color(cx.theme().muted_foreground)
                .child(tr(message));
        } else if let Some(outcome) = state.outcome.as_ref() {
            status = match outcome {
                SerialDfuOutcome::Sent => status
                    .text_color(cx.theme().muted_foreground)
                    .child(tr("firmware.serial_dfu.sent")),
                SerialDfuOutcome::Failed(error) => {
                    let message = SharedString::from(error.clone());
                    let tooltip = message.clone();
                    status
                        .flex()
                        .flex_col()
                        .gap_1()
                        .text_color(cx.theme().danger)
                        .child(tr("firmware.serial_dfu.failed"))
                        .child(
                            div()
                                .id("serial-dfu-error")
                                .min_w_0()
                                .w_full()
                                .whitespace_normal()
                                .tooltip(move |window, cx| {
                                    Tooltip::new(tooltip.clone()).build(window, cx)
                                })
                                .child(message),
                        )
                }
            };
        }
        section.child(status).into_any_element()
    }
}
