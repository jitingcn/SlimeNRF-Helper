use super::*;
use crate::ui::terminal::serial_display_name;

impl Workbench {
    #[inline(never)]
    fn flash_uf2_row(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.flash.busy();
        let volume = &self.flash.volumes[index];
        let mut panel = div().flex().flex_col().gap_2();
        let identity = volume.identity.clone();
        panel = panel.child(
            Checkbox::new(SharedString::from(format!("uf2-target-{identity}")))
                .label(format!("{} · {}", volume.label, volume.mount.display()))
                .checked(self.flash.selected_volumes.contains(&identity))
                .disabled(busy || self.flash.auto)
                .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                    if *checked {
                        this.flash.selected_volumes.insert(identity.clone());
                    } else {
                        this.flash.selected_volumes.remove(&identity);
                    }
                    this.flash.review = None;
                    cx.notify();
                })),
        );
        panel = panel.child(
            div()
                .min_w_0()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(clipped(
                    format!("uf2-board-{index}"),
                    volume.board_id.clone().unwrap_or_else(|| "?".into()),
                )),
        );
        panel.into_any_element()
    }

    #[inline(never)]
    pub(super) fn flash_uf2_presets(
        &self,
        index: usize,
        preset: &board_catalog::FlashPreset,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut panel = div().flex().flex_col().gap_2();
        for (candidate_index, (_, path, target_name)) in preset.firmware_candidates().enumerate() {
            // Legacy presets may store a bootloader upgrade in `firmware`.
            // The shared image predicate, not the configuration field, decides
            // which category exposes the configured file.
            panel = panel.child(
                self.flash_configured_button(
                    (
                        SharedString::from(format!("uf2-preset-{index}")),
                        candidate_index,
                    )
                        .into(),
                    format!(
                        "{} · {target_name}",
                        tr(if self.flash.category == ImageCategory::Bootloader {
                            "firmware.source.configured_bootloader"
                        } else {
                            "firmware.source.configured"
                        })
                    )
                    .into(),
                    path,
                    cx,
                ),
            );
        }
        panel.into_any_element()
    }

    #[inline(never)]
    fn flash_dfu_port(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.flash.busy();
        let port = &self.inventory.ports[index];
        let name = port.port_name.clone();
        let label = SharedString::from(format!(
            "{} · {}",
            port.port_name,
            self.serial_device_label(
                &port.port_name,
                serial_display_name(&port.port_name, &port.label)
            )
        ));
        context_button(
            Button::new(SharedString::from(format!("dfu-port-{name}")))
                .small()
                .w_full()
                .min_w_0()
                .accessibility_label(label.clone())
                .tooltip(self.serial_device_label(
                    &port.port_name,
                    format!("{} · {}", port.port_name, port.label),
                ))
                .child(div().w_full().min_w_0().truncate().text_left().child(label))
                .disabled(busy),
            self.flash.selected_port.as_ref() == Some(&name),
            cx,
        )
        .on_click(cx.listener(move |this, _, _, cx| {
            this.flash.selected_port = Some(name.clone());
            this.flash.review = None;
            cx.notify();
        }))
        .into_any_element()
    }

    #[inline(never)]
    pub(super) fn flash_uf2_targets(&self, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.flash.busy();
        let mut panel = flash_panel(cx).child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .child(tr("firmware.step.select_devices")),
        );
        panel = panel.child(self.flash_serial_dfu(cx));
        panel = panel.child(
            Button::new("scan-uf2")
                .small()
                .icon(IconName::RefreshCw)
                .label(tr("firmware.uf2.refresh"))
                .disabled(busy)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.scan_flash_volumes();
                    cx.notify();
                })),
        );
        if self.flash.volumes.is_empty() {
            panel = panel.child(div().text_sm().child(tr("firmware.uf2.no_volume")));
        }
        for index in 0..self.flash.volumes.len() {
            panel = panel.child(self.flash_uf2_row(index, cx));
        }
        if !self.flash.volumes.is_empty() {
            panel =
                panel.child(
                    Button::new("uf2-reboot")
                        .label(tr("firmware.action.exit_dfu"))
                        .disabled(
                            busy || !self.flash.volumes.iter().any(|volume| {
                                self.flash.selected_volumes.contains(&volume.identity)
                            }),
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            let volumes = this.flash.selected_live_volumes();
                            if !volumes.is_empty() {
                                this.review_flash(Action::Reboot(volumes));
                            }
                            cx.notify();
                        })),
                );
        }
        panel.into_any_element()
    }

    #[inline(never)]
    pub(super) fn flash_dfu_targets(&self, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.flash.busy();
        let mut panel = flash_panel(cx).child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .child(tr("firmware.step.select_devices")),
        );
        panel = panel
            .child(
                div()
                    .text_sm()
                    .child(tr("firmware.opendfu.select_port_help")),
            )
            .child(
                div()
                    .min_w_0()
                    .w_full()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .whitespace_normal()
                    .child(tr("firmware.opendfu.magnet_help")),
            )
            .child(
                Button::new("dfu-refresh")
                    .small()
                    .icon(IconName::RefreshCw)
                    .label(tr("firmware.serial.refresh"))
                    .disabled(busy)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.scan_inventory(cx);
                        this.flash.selected_port = None;
                        cx.notify();
                    })),
            );
        if self.inventory.ports.is_empty() {
            panel = panel.child(
                div()
                    .text_sm()
                    .whitespace_normal()
                    .child(tr("firmware.serial.connect_help")),
            );
        }
        for index in 0..self.inventory.ports.len() {
            panel = panel.child(self.flash_dfu_port(index, cx));
        }
        panel = panel.child(
            Button::new("dfu-enter")
                .label(tr("firmware.action.enter_holyiot_dfu"))
                .disabled(busy || self.flash.selected_port.is_none())
                .on_click(cx.listener(|this, _, _, cx| {
                    match this.flash_selected_port() {
                        Ok(port) => this.review_flash(Action::Enter(port)),
                        Err(error) => this.flash.set_error(this.flash_mode(), error),
                    }
                    cx.notify();
                })),
        );
        panel.into_any_element()
    }
}
