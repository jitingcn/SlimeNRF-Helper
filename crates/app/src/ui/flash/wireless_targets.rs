use super::*;
use crate::ui::tracker_cards::{TrackerCardGrid, tracker_activity_outline, tracker_card_frame};

#[inline(never)]
fn flash_firmware_metadata(
    target: &esb_ota::Target,
    advanced: bool,
    reported_header: bool,
    cx: &App,
) -> AnyElement {
    let mut panel = div()
        .min_w_0()
        .w_full()
        .flex()
        .flex_col()
        .gap_1()
        .text_xs()
        .text_color(cx.theme().muted_foreground);
    for (label, value) in [
        ("firmware.metadata.version", target.info.version.as_str()),
        (
            "firmware.metadata.build_date",
            target.info.build_date.as_str(),
        ),
        (
            "firmware.metadata.target",
            target.info.board_target.as_str(),
        ),
        (
            "firmware.metadata.bootloader",
            target.info.bootloader.as_str(),
        ),
    ] {
        if (!advanced && label == "firmware.metadata.bootloader")
            || (reported_header
                && matches!(
                    label,
                    "firmware.metadata.version" | "firmware.metadata.build_date"
                ))
        {
            continue;
        }
        let tooltip = format!("{}: {value}", tr(label));
        let text = if label == "firmware.metadata.build_date" {
            format!(
                "{}: {}",
                tr(label),
                crate::ui::date_label::display_date(value).unwrap_or_else(|| value.to_owned())
            )
        } else if label == "firmware.metadata.target" {
            value.to_owned()
        } else {
            tooltip.clone()
        };
        panel = panel.child(
            div()
                .id(SharedString::from(format!(
                    "ota-metadata-{}-{}-{label}",
                    target.receiver.key, target.id
                )))
                .min_w_0()
                .w_full()
                .whitespace_normal()
                .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                .child(text),
        );
    }
    panel.into_any_element()
}

impl Workbench {
    #[inline(never)]
    fn flash_esb_receiver(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let receiver = &self.inventory.receivers[index];
        let key = receiver.key.clone();
        let observation = self.monitor.observation(&key);
        let querying = self.flash.querying_receiver(&key);
        let refreshing = self.flash.preserves_query_layout(&key);
        let trackers = self
            .flash
            .tracker_snapshot
            .get(&key)
            .map(Vec::as_slice)
            .or_else(|| observation.map(|o| o.trackers.as_slice()))
            .unwrap_or_default();
        let receiver_name = receiver
            .serial_number
            .as_deref()
            .and_then(|serial| receiver.label.strip_suffix(serial))
            .and_then(|label| label.strip_suffix(" · "))
            .unwrap_or(&receiver.label);
        let panel = flash_panel(cx)
            .border_color(cx.theme().border.opacity(0.45))
            .child(
                div()
                    .min_w_0()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(clipped(
                        format!("ota-receiver-title-{index}"),
                        self.receiver_label(receiver, receiver_name),
                    )),
            )
            .when_some(receiver.serial_number.as_ref(), |panel, serial| {
                panel.child(
                    div()
                        .min_w_0()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(clipped(
                            format!("ota-receiver-serial-{index}"),
                            serial.clone(),
                        )),
                )
            });
        #[cfg(test)]
        let panel = panel
            .id(SharedString::from(format!("ota-receiver-card-{key}")))
            .test_support();
        let mut panel = panel;
        let online = observation.map_or(0, |o| o.trackers.iter().filter(|t| t.online).count());
        let selected = trackers
            .iter()
            .filter(|tracker| self.selected.contains(&tracker_selection_key(tracker)))
            .count();
        panel = panel.child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(if querying {
                    format!(
                        "{} · {}: {selected}",
                        tr("firmware.query.reading"),
                        tr("firmware.selection.selected")
                    )
                } else {
                    format!(
                        "{}: {online} · {}: {selected}",
                        tr("firmware.selection.online"),
                        tr("firmware.selection.selected")
                    )
                }),
        );
        let mut has_firmware_info = false;
        for (target_index, target) in self.flash.targets.iter().enumerate() {
            if target.receiver.key == receiver.key && target.id == esb_ota::RECEIVER_ID {
                has_firmware_info = true;
                panel = panel.child(self.flash_esb_row(target_index, cx));
            }
        }
        let query_error =
            self.flash
                .query_failures
                .get(&format!("{}:{}", receiver.key, esb_ota::RECEIVER_ID));
        if let Some(error) = query_error {
            let diagnostic = div()
                .text_xs()
                .text_color(cx.theme().warning)
                .child(error.clone());
            #[cfg(test)]
            let diagnostic = diagnostic
                .id(SharedString::from(format!(
                    "ota-query-error-{}-{}",
                    receiver.key,
                    esb_ota::RECEIVER_ID
                )))
                .role(Role::Label)
                .aria_label(error.clone())
                .test_support();
            panel = panel.child(diagnostic);
        }
        if !has_firmware_info || query_error.is_some() {
            panel = panel.child(
                div()
                    .text_xs()
                    .whitespace_normal()
                    .text_color(cx.theme().muted_foreground)
                    .child(tr(if query_error.is_some() {
                        "firmware.query.retry_help"
                    } else {
                        "firmware.query.wait_device"
                    })),
            );
        }
        if let Some(error) = observation.and_then(|o| o.error.as_ref()) {
            panel = panel.child(div().text_xs().child(error.clone()));
        }
        panel = panel.child(self.flash_esb_group_actions(index, cx));
        if trackers.is_empty() {
            if !querying {
                panel = panel.child(div().text_sm().child(tr("firmware.query.no_trackers")));
            }
        } else {
            let mut sections = std::collections::BTreeMap::new();
            for tracker in trackers {
                let target = self.flash.tracker_info(tracker, receiver);
                let failed = self
                    .flash
                    .query_failures
                    .contains_key(&format!("{}:{}", receiver.key, tracker.slot));
                sections
                    .entry(tracker_section(
                        tracker,
                        target,
                        querying && !refreshing,
                        failed,
                    ))
                    .or_insert_with(Vec::new)
                    .push(tracker);
            }
            for (section, mut members) in sections {
                members.sort_by_key(|tracker| tracker.slot);
                let id = format!("ota-section-{}-{}", receiver.key, section.id());
                let tooltip = section.tooltip();
                panel = panel.child(
                    div()
                        .id(SharedString::from(id.clone()))
                        .min_w_0()
                        .flex_shrink_0()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(
                            div()
                                .id(SharedString::from(format!("{id}-heading")))
                                .text_sm()
                                .whitespace_normal()
                                .font_weight(FontWeight::SEMIBOLD)
                                .tooltip(move |window, cx| {
                                    Tooltip::new(tooltip.clone()).build(window, cx)
                                })
                                .child(format!("{} ({})", section.label(), members.len())),
                        )
                        .child(TrackerCardGrid {
                            id: format!("{id}-grid").into(),
                            horizontal_padding: 0.,
                            cards: members
                                .into_iter()
                                .map(|tracker| self.flash_esb_tracker(tracker, &section, cx))
                                .collect(),
                        }),
                );
            }
        }
        panel.into_any_element()
    }

    #[inline(never)]
    fn flash_esb_group_actions(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let key = self.inventory.receivers[index].key.clone();
        let clear_key = key.clone();
        let offline_key = key.clone();
        let refresh_receiver = self.inventory.receivers[index].clone();
        let refresh_enabled = self.can_query_flash_group(&refresh_receiver, None);
        let has_selected_offline = self.monitor.observation(&key).is_some_and(|observation| {
            self.selected.iter().any(|selected| {
                selected_tracker_is_known_offline(selected, &key, &observation.trackers)
            })
        });
        div()
            .flex()
            .flex_wrap()
            .gap_2()
            .child(
                Button::new(SharedString::from(format!("ota-online-{key}")))
                    .h(px(36.))
                    .label(tr("firmware.selection.all_online"))
                    .disabled(self.flash.busy())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if this.flash.busy() {
                            return;
                        }
                        if let Some(observation) = this.monitor.observation(&key) {
                            select_receiver_trackers(
                                &mut this.selected,
                                &key,
                                &observation.trackers,
                                true,
                            );
                        }
                        this.flash.review = None;
                        this.selection_changed();
                        cx.notify();
                    })),
            )
            .child(
                Button::new(SharedString::from(format!("ota-clear-{clear_key}")))
                    .h(px(36.))
                    .label(tr("common.action.clear_selection"))
                    .disabled(self.flash.busy())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if this.flash.busy() {
                            return;
                        }
                        select_receiver_trackers(&mut this.selected, &clear_key, &[], false);
                        this.flash.review = None;
                        this.selection_changed();
                        cx.notify();
                    })),
            )
            .child(
                Button::new(SharedString::from(format!(
                    "ota-clear-offline-{offline_key}"
                )))
                .h(px(36.))
                .label(tr("firmware.selection.clear_offline"))
                .disabled(self.flash.busy() || self.flash.querying() || !has_selected_offline)
                .on_click(cx.listener(move |this, _, _, cx| {
                    if this.flash.busy() || this.flash.querying() {
                        return;
                    }
                    // Re-read live evidence: a retained display snapshot is not a roster.
                    let changed = this
                        .monitor
                        .observation(&offline_key)
                        .is_some_and(|current| {
                            clear_known_offline_tracker_selection(
                                &mut this.selected,
                                &offline_key,
                                &current.trackers,
                            )
                        });
                    if changed {
                        this.flash.review = None;
                        this.selection_changed();
                        cx.notify();
                    }
                })),
            )
            .child(
                Button::new(SharedString::from(format!(
                    "ota-query-receiver-{}",
                    refresh_receiver.key
                )))
                .h(px(36.))
                .icon(IconName::RefreshCw)
                .label(tr("firmware.query.refresh"))
                .tooltip(tr("firmware.query.refresh_receiver_help"))
                .accessibility_label(tr("firmware.query.refresh_receiver_help"))
                .disabled(!refresh_enabled)
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.query_flash_group(&refresh_receiver, None);
                    cx.notify();
                })),
            )
            .into_any_element()
    }

    #[inline(never)]
    fn flash_esb_tracker(
        &self,
        tracker: &TrackerLink,
        section: &TrackerSection<'_>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = tracker_selection_key(tracker);
        let querying = self.flash.querying_receiver(&tracker.receiver_key);
        let refreshing = self.flash.preserves_query_layout(&tracker.receiver_key);
        let selected = self.selected.contains(&key);
        let enabled =
            !self.flash.busy() && (selected || (tracker.online && tracker.address.is_some()));
        let click_key = key.clone();
        let offline = !tracker.online;
        let receiver = self
            .inventory
            .receivers
            .iter()
            .find(|receiver| receiver.key == tracker.receiver_key);
        let target = receiver.and_then(|receiver| self.flash.tracker_info(tracker, receiver));
        let failed = self
            .flash
            .query_failures
            .contains_key(&format!("{}:{}", tracker.receiver_key, tracker.slot));
        let tps = self
            .monitor
            .observation(&tracker.receiver_key)
            .map_or(0, |observation| {
                ota_tracker_activity_tps(
                    tracker,
                    &observation.trackers,
                    observation.tps(tracker.slot),
                )
            });
        let (activity, outline_width) =
            tracker_activity_outline(tps, cx.theme().is_dark(), self.preferences.accent);
        let tooltip = if enabled {
            tracker
                .address
                .clone()
                .unwrap_or_else(|| tr("firmware.metadata.address_unknown"))
        } else {
            tr("firmware.error.wait_tracker_data")
        };
        let card = div()
            .id(SharedString::from(format!("ota-card-{key}")))
            .min_w_0()
            .flex()
            .flex_col()
            .gap_1()
            .p_3()
            .border_1()
            .border_color(cx.theme().border.opacity(0.18))
            .bg(if selected {
                cx.theme().accent
            } else if offline {
                cx.theme().muted
            } else {
                cx.theme().group_box
            })
            .text_color(if offline {
                cx.theme().muted_foreground
            } else {
                cx.theme().foreground
            })
            .rounded_md()
            .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
            .when(enabled, |card| {
                card.cursor_pointer()
                    .hover(|style| style.bg(cx.theme().accent))
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.toggle_flash_tracker(&click_key);
                cx.notify();
            }));
        #[cfg(test)]
        let card = card.test_support();
        let mut card = card;
        let mut heading = div()
            .min_w_0()
            .flex()
            .items_center()
            .justify_between()
            .gap_1()
            .child(
                Checkbox::new(SharedString::from(format!("ota-{key}")))
                    .label(self.tracker_label(
                        tracker,
                        format!("{} #{}", tr("common.device_role.tracker"), tracker.slot),
                    ))
                    .checked(selected)
                    .disabled(!enabled)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.toggle_flash_tracker(&key);
                        cx.notify();
                    })),
            );
        if let Some(receiver) = receiver.filter(|_| failed) {
            let retry_receiver = receiver.clone();
            let retry_tracker = tracker.clone();
            heading = heading.child(
                div()
                    .id(SharedString::from(format!(
                        "ota-query-tracker-hit-{}-{}",
                        tracker.receiver_key, tracker.slot
                    )))
                    .flex_shrink_0()
                    // Disabled retries must not bubble into card selection either.
                    .on_click(cx.listener(|_, _, _, cx| cx.stop_propagation()))
                    .child(
                        Button::new(SharedString::from(format!(
                            "ota-query-tracker-{}-{}",
                            tracker.receiver_key, tracker.slot
                        )))
                        .small()
                        .ghost()
                        .icon(IconName::RefreshCw)
                        .tooltip(tr("firmware.query.retry_tracker"))
                        .accessibility_label(tr("firmware.query.retry_tracker"))
                        .disabled(!self.can_query_flash_group(receiver, Some(tracker)))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.query_flash_group(&retry_receiver, Some(&retry_tracker));
                            cx.notify();
                        })),
                    ),
            );
        }
        card = card.child(heading);
        if selected && offline && (!querying || refreshing) {
            card = card.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().warning)
                    .child(tr("firmware.selection.selected_offline")),
            );
        }
        card = card.child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(
                    tracker
                        .address
                        .clone()
                        .unwrap_or_else(|| tr("firmware.metadata.address_unknown")),
                ),
        );
        let mut needs_info_refresh = false;
        if let Some(target) = target {
            card = card.child(flash_firmware_metadata(
                target,
                self.preferences.advanced_features,
                matches!(section, TrackerSection::Reported(..)),
                cx,
            ));
            if let Some(guidance) = ota_update_guidance(target) {
                card = card.child(self.flash_ota_guidance(target, guidance, cx));
            }
        } else if !failed
            && (!querying || refreshing)
            && !matches!(section, TrackerSection::Offline)
        {
            card = card.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(tr(if tracker.online {
                        "firmware.query.not_queried"
                    } else {
                        "firmware.query.wait_recent_data"
                    })),
            );
        }
        if let Some(error) = self
            .flash
            .query_failures
            .get(&format!("{}:{}", tracker.receiver_key, tracker.slot))
        {
            needs_info_refresh = true;
            let diagnostic = div()
                .text_xs()
                .text_color(cx.theme().warning)
                .child(error.clone());
            #[cfg(test)]
            let diagnostic = diagnostic
                .id(SharedString::from(format!(
                    "ota-query-error-{}-{}",
                    tracker.receiver_key, tracker.slot
                )))
                .role(Role::Label)
                .aria_label(error.clone())
                .test_support();
            card = card.child(diagnostic);
        }
        if needs_info_refresh {
            card = card.child(
                div()
                    .text_xs()
                    .whitespace_normal()
                    .text_color(cx.theme().muted_foreground)
                    .child(tr("firmware.query.retry_help")),
            );
        }
        tracker_card_frame(
            card,
            if tps == 0 {
                activity.opacity(0.)
            } else {
                activity
            },
            outline_width,
        )
        .into_any_element()
    }

    #[inline(never)]
    fn flash_esb_row(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.flash.busy();
        let target = &self.flash.targets[index];
        let blocked = !ota_supported(target);
        let key = ota_target_key(target);
        let selected = self.flash.selected_targets.contains(&key);
        let mut panel = div()
            .min_w_0()
            .w_full()
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .rounded_md()
            .border_1()
            .border_color(cx.theme().border.opacity(0.45))
            .bg(if selected {
                cx.theme().accent
            } else {
                cx.theme().group_box
            })
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(tr("firmware.receiver.firmware")),
            );
        panel = panel.child(
            Checkbox::new(SharedString::from(format!("ota-target-{key}")))
                .label(tr("firmware.receiver.update_self"))
                .checked(selected)
                .disabled(busy || blocked)
                .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                    cx.stop_propagation();
                    this.flash.set_receiver_update_selected(&key, *checked);
                    cx.notify();
                })),
        );
        panel = panel.child(flash_firmware_metadata(
            target,
            self.preferences.advanced_features,
            false,
            cx,
        ));
        if let Some(guidance) = ota_update_guidance(target) {
            panel = panel.child(self.flash_ota_guidance(target, guidance, cx));
        } else {
            panel = panel.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(tr("firmware.receiver.runs_last")),
            );
        }
        panel.into_any_element()
    }

    #[inline(never)]
    fn flash_ota_guidance(
        &self,
        target: &esb_ota::Target,
        guidance: OtaUpdateGuidance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut panel = div()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_2()
            .text_xs()
            .whitespace_normal()
            .child(tr(match guidance {
                OtaUpdateGuidance::OpenDfu => "firmware.receiver.opendfu_required",
                OtaUpdateGuidance::Uf2 => "firmware.receiver.uf2_required",
                OtaUpdateGuidance::DocumentedMethod => "firmware.error.unsupported_method",
            }));
        if target.id == esb_ota::RECEIVER_ID && guidance == OtaUpdateGuidance::OpenDfu {
            panel = panel.child(tr("firmware.receiver.tracker_ota_available"));
        }
        if let Some(mode) = guidance.wired_mode() {
            let key = ota_target_key(target);
            panel = panel.child(
                Button::new(SharedString::from(format!("ota-wired-{key}")))
                    .small()
                    .label(tr(if mode == Mode::OpenDfu {
                        "firmware.receiver.open_opendfu"
                    } else {
                        "firmware.receiver.open_uf2"
                    }))
                    .disabled(self.flash.busy())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        let Some(mode) = this.flash.ota_wired_update_mode(&key) else {
                            return;
                        };
                        if !this.set_feature_page(FeaturePage::Uf2, cx) {
                            return;
                        }
                        // Navigation only: preserve explicit selections and never enqueue work.
                        this.flash.transition_mode(mode);
                        this.cancel_terminal_intents();
                        this.sidebar.pending = None;
                        this.command_error = None;
                        this.ensure_flash_catalog();
                        cx.notify();
                    })),
            );
        }
        panel.into_any_element()
    }

    #[inline(never)]
    fn flash_ble_row(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.flash.busy();
        let device = &self.flash.ble[index];
        let id = device.id.clone();
        Checkbox::new(SharedString::from(format!(
            "ble-target-{:?}-{:?}",
            device.adapter_id, id
        )))
        .label(format!("{} · {}", device.name, device.address))
        .checked(self.flash.selected_ble.contains(&id))
        .disabled(busy)
        .on_click(cx.listener(move |this, checked: &bool, _, cx| {
            if *checked {
                this.flash.selected_ble.insert(id.clone());
            } else {
                this.flash.selected_ble.remove(&id);
            }
            this.flash.review = None;
            cx.notify();
        }))
        .into_any_element()
    }

    #[inline(never)]
    pub(super) fn flash_esb_targets(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut panel = div()
            .min_w_0()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(tr("firmware.selection.tracker_help")),
            );
        if self.inventory.receivers.is_empty() {
            panel = panel.child(div().text_sm().child(tr("firmware.query.connect_receiver")));
        }
        for index in 0..self.inventory.receivers.len() {
            panel = panel.child(self.flash_esb_receiver(index, cx));
        }
        panel.into_any_element()
    }

    #[inline(never)]
    pub(super) fn flash_ble_targets(&self, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.flash.busy();
        let mut panel = flash_panel(cx).child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .child(tr("firmware.step.select_devices")),
        );
        panel = panel
            .child(div().text_sm().child(tr("firmware.ble.enter_dfu_help")))
            .child(
                Button::new("ble-scan")
                    .small()
                    .icon(IconName::RefreshCw)
                    .label(tr("firmware.ble.scan"))
                    .disabled(busy)
                    .on_click(cx.listener(|this, _, _, cx| {
                        if this.flash_mode() != Mode::Ble || !this.preferences.advanced_features {
                            return;
                        }
                        let backend = this.async_io.ble_backend();
                        this.start_flash_async(move |cancel, reporter| async move {
                            ble_ota::scan(&backend, &cancel, &mut |value| reporter.emit(value))
                                .await
                                .map(Payload::Ble)
                        });
                        cx.notify();
                    })),
            );
        for index in 0..self.flash.ble.len() {
            panel = panel.child(self.flash_ble_row(index, cx));
        }
        panel.into_any_element()
    }
}
