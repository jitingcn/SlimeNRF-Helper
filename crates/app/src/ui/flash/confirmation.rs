use super::*;

// Preserve every frozen assignment, including different images with identical filenames.
// Only the receiver presentation is regrouped; execution order stays in OtaPlanGroup.
pub(super) fn ota_confirmation_receivers(
    groups: &[OtaPlanGroup],
) -> std::collections::BTreeMap<&str, Vec<(usize, &OtaPlanGroup)>> {
    let mut receivers = std::collections::BTreeMap::<_, Vec<_>>::new();
    for (index, group) in groups.iter().enumerate() {
        if let Some(target) = group.targets.first() {
            receivers
                .entry(target.receiver.key.as_str())
                .or_default()
                .push((index, group));
        }
    }
    receivers
}

impl FlashState {
    pub(super) fn take_confirmed_action(&mut self) -> Option<Action> {
        if self.busy() || !self.acknowledged {
            return None;
        }
        let action = self.review.take()?;
        self.acknowledged = false;
        if matches!(action, Action::EsbGroups(_)) {
            self.ota_presentation = OtaPresentation::Running;
        }
        Some(action)
    }
}

impl Workbench {
    pub(super) fn review_flash(&mut self, action: Action) {
        if matches!(action, Action::Ble(..)) && !self.preferences.advanced_features {
            return;
        }
        self.flash.review = Some(action);
        self.flash.auto_requested = false;
        self.flash.acknowledged = false;
        self.flash.error = None;
    }

    #[inline(never)]
    pub(super) fn flash_review_controls(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.flash.review.is_some() {
            return div().into_any_element();
        }
        let busy = self.flash.busy();
        let mut page = div().flex().flex_col().gap_3();
        let readiness = self.selected_flash_action();
        let disabled_reason = if self.flash.querying() {
            Some(tr("firmware.query.reading"))
        } else {
            readiness.as_ref().err().cloned()
        };
        page = page.child(
            Button::new("flash-review")
                .label(tr("firmware.action.review"))
                .primary()
                .when(!cx.theme().is_dark(), |button| button.outline())
                .disabled(busy || readiness.is_err())
                .on_click(cx.listener(|this, _, _, cx| {
                    match this.selected_flash_action() {
                        Ok(action) => this.review_flash(action),
                        Err(error) => this.flash.set_error(this.flash_mode(), error),
                    }
                    cx.notify();
                })),
        );
        if let Some(reason) = disabled_reason {
            page = page.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(reason),
            );
        }
        page.into_any_element()
    }

    #[inline(never)]
    pub(super) fn flash_confirmation(&self, action: &Action, cx: &mut Context<Self>) -> AnyElement {
        let esb = matches!(action, Action::EsbGroups(_));
        let mut panel = flash_panel(cx)
            .when(!esb, |panel| {
                panel.child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(tr("firmware.confirmation.title")),
                )
            })
            .child(
                div()
                    .text_sm()
                    .when(!esb, |text| text.text_color(cx.theme().warning))
                    .whitespace_normal()
                    .child(tr(if esb {
                        "firmware.confirmation.esb_power"
                    } else {
                        "firmware.confirmation.wired_power"
                    })),
            )
            .when(esb, |panel| {
                panel.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .whitespace_normal()
                        .child(tr("firmware.confirmation.wireless_recovery")),
                )
            });
        panel = panel.child(self.flash_confirmation_targets(action, cx));
        let image = match action {
            Action::Uf2(_, image, _) | Action::OpenDfu(_, image, _) | Action::Ble(_, image) => {
                Some(image)
            }
            Action::Reboot(_)
            | Action::Enter(_)
            | Action::QueryGroups(_)
            | Action::EsbGroups(_) => None,
        };
        if let Some(image) = image {
            panel = panel.child(self.flash_image_identity(image, "confirmation", cx));
            if image_category(&image.name, &image.hints) == ImageCategory::Bootloader {
                panel = panel.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().warning)
                        .whitespace_normal()
                        .child(tr("firmware.confirmation.bootloader_risk")),
                );
            }
        }
        panel = panel.child(self.flash_confirmation_ack(cx));
        panel = panel.child(self.flash_confirmation_actions(cx));
        panel.into_any_element()
    }

    #[inline(never)]
    fn flash_confirmation_targets(&self, action: &Action, cx: &App) -> AnyElement {
        let mut targets = div().min_w_0().w_full().flex().flex_col().gap_2();
        match action {
            Action::Uf2(volumes, _, _) => {
                for volume in volumes {
                    targets = targets.child(flash_target_text(format!(
                        "{} · {} · {}",
                        volume.label,
                        volume.mount.display(),
                        volume.board_id.as_deref().unwrap_or("?")
                    )));
                }
            }
            Action::Reboot(volumes) => {
                for volume in volumes {
                    targets = targets.child(flash_target_text(format!(
                        "{} · {}",
                        tr("firmware.action.exit_dfu"),
                        volume.mount.display()
                    )));
                }
            }
            Action::Enter(port) => {
                targets = targets.child(flash_target_text(format!(
                    "{} · {} · {}",
                    tr("firmware.action.enter_holyiot_dfu"),
                    port.port_name,
                    self.frozen_serial_endpoint_label(port, port.label.clone())
                )));
            }
            Action::OpenDfu(port, _, _) => {
                targets = targets.child(flash_target_text(format!(
                    "{} · {}",
                    port.port_name,
                    self.frozen_serial_endpoint_label(port, port.label.clone())
                )));
            }
            Action::EsbGroups(groups) => {
                for receiver_groups in ota_confirmation_receivers(groups).into_values() {
                    targets = targets.child(self.flash_confirmation_receiver(&receiver_groups, cx));
                }
                return div()
                    .id("firmware-confirmation-targets")
                    .min_w_0()
                    .w_full()
                    .max_h(px(320.))
                    .overflow_y_scroll()
                    .child(targets.flex_shrink_0())
                    .into_any_element();
            }
            Action::Ble(devices, _) => {
                for device in devices {
                    targets = targets.child(flash_target_text(format!(
                        "{} · {}",
                        device.name, device.address
                    )));
                }
            }
            Action::QueryGroups(_) => {}
        }
        targets.into_any_element()
    }

    #[inline(never)]
    fn flash_confirmation_ack(&self, cx: &mut Context<Self>) -> AnyElement {
        Checkbox::new("firmware-ack")
            .label(tr("firmware.confirmation.acknowledge"))
            .checked(self.flash.acknowledged)
            .disabled(self.flash.busy())
            .on_click(cx.listener(|this, checked: &bool, _, cx| {
                this.flash.acknowledged = *checked;
                cx.notify();
            }))
            .into_any_element()
    }

    #[inline(never)]
    fn flash_confirmation_start(&self, cx: &mut Context<Self>) -> AnyElement {
        Button::new("firmware-confirm")
            .label(tr("firmware.action.confirm_start"))
            .primary()
            .when(!cx.theme().is_dark(), |button| button.outline())
            .disabled(!self.flash.acknowledged || self.flash.busy())
            .on_click(cx.listener(|this, _, _, cx| {
                if this.shutting_down {
                    return;
                }
                if let Some(action) = this.flash.take_confirmed_action() {
                    this.flash.auto =
                        this.flash.auto_requested && matches!(&action, Action::Uf2(..));
                    this.flash.auto_requested = false;
                    if let Action::Uf2(volumes, image, _) = &action {
                        for volume in volumes {
                            this.flash
                                .auto_seen
                                .insert(format!("{}:{}", volume.identity, image.digest));
                        }
                    }
                    this.reserve_flash_action(action);
                }
                cx.notify();
            }))
            .into_any_element()
    }

    #[inline(never)]
    fn flash_confirmation_back(&self, cx: &mut Context<Self>) -> AnyElement {
        Button::new("firmware-dismiss")
            .label(tr("common.action.back"))
            .disabled(self.flash.busy())
            .on_click(cx.listener(|this, _, _, cx| {
                this.flash.review = None;
                this.flash.acknowledged = false;
                if this.flash_mode() == Mode::Esb {
                    this.flash.ota_back();
                }
                this.flash.auto = false;
                this.flash.auto_requested = false;
                cx.notify();
            }))
            .into_any_element()
    }

    #[inline(never)]
    fn flash_confirmation_actions(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex()
            .gap_2()
            .child(self.flash_confirmation_start(cx))
            .child(self.flash_confirmation_back(cx))
            .into_any_element()
    }

    #[inline(never)]
    fn flash_confirmation_receiver(
        &self,
        groups: &[(usize, &OtaPlanGroup)],
        cx: &App,
    ) -> AnyElement {
        let receiver = &groups[0].1.targets[0].receiver;
        let receiver_name = receiver
            .serial_number
            .as_deref()
            .and_then(|serial| receiver.label.strip_suffix(serial))
            .and_then(|label| label.strip_suffix(" · "))
            .unwrap_or(&receiver.label);
        let mut panel = div()
            .min_w_0()
            .w_full()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .rounded_md()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .child(
                div()
                    .min_w_0()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(clipped(
                        format!("review-receiver-{}", receiver.key),
                        self.frozen_receiver_label(receiver, receiver_name),
                    )),
            )
            .child(
                div()
                    .min_w_0()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(clipped(
                        format!("review-receiver-id-{}", receiver.key),
                        receiver
                            .serial_number
                            .as_ref()
                            .unwrap_or(&receiver.key)
                            .clone(),
                    )),
            );
        for &(index, group) in groups {
            panel = panel.child(self.flash_confirmation_ota_group(index, group, cx));
        }
        panel.into_any_element()
    }

    #[inline(never)]
    fn flash_confirmation_ota_group(
        &self,
        index: usize,
        group: &OtaPlanGroup,
        cx: &App,
    ) -> AnyElement {
        let mut chips = div().min_w_0().flex().flex_wrap().gap_1();
        let tracker_count = group
            .targets
            .iter()
            .filter(|target| target.id != esb_ota::RECEIVER_ID)
            .count();
        for target in &group.targets {
            let label = if target.id == esb_ota::RECEIVER_ID {
                tr("common.device_role.receiver")
            } else {
                rust_i18n::t!("firmware.confirmation.tracker_slot", slot = target.id).to_string()
            };
            let label = if target.id == esb_ota::RECEIVER_ID {
                self.frozen_receiver_label(&target.receiver, label)
            } else {
                self.address_label(target.address.as_deref(), label)
            };
            let details = format!(
                "{}\n{}: {}\n{}: {}",
                label,
                tr("firmware.metadata.address"),
                target.address.as_deref().unwrap_or({
                    if target.id == esb_ota::RECEIVER_ID {
                        "—"
                    } else {
                        "?"
                    }
                }),
                tr("firmware.metadata.target"),
                target.info.board_target,
            );
            chips = chips.child(
                div()
                    .id(SharedString::from(format!(
                        "review-target-{index}-{}-{}",
                        target.receiver.key, target.id
                    )))
                    .flex_shrink_0()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().muted)
                    .text_xs()
                    .tooltip(move |window, cx| Tooltip::new(details.clone()).build(window, cx))
                    .child(label),
            );
        }
        div()
            .min_w_0()
            .w_full()
            .flex()
            .flex_col()
            .gap_2()
            .pt_2()
            .border_t_1()
            .border_color(cx.theme().border)
            .child(self.flash_image_identity(&group.image, &format!("review-{index}"), cx))
            .when(tracker_count > 0, |panel| {
                panel.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(
                            rust_i18n::t!(
                                "firmware.confirmation.tracker_count",
                                count = tracker_count
                            )
                            .to_string(),
                        ),
                )
            })
            .child(chips)
            .into_any_element()
    }
}
