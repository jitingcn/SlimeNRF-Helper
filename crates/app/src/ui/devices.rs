#[cfg(test)]
mod interaction_tests;
#[cfg(test)]
mod tests;

use super::{
    FeaturePage, Workbench,
    command_panel::{CommandFocus, CommandScope},
    components::{
        CONNECTION_ACTION_WIDTH, clipped, connection_action, connection_status, context_button,
    },
    terminal::serial_display_name,
    tracker_cards::{
        TRACKER_GRID_PADDING, TrackerCardGrid, tracker_activity_outline, tracker_card_frame,
    },
};
use crate::{
    domain::{
        DeviceIdentity, DeviceRole, Inventory, SerialEndpoint, SerialRole, TrackerLink,
        console_port_allowed, receiver_port, receiver_serial_ports, tracker_port,
    },
    i18n::tr,
    sessions::ConnectionState,
};
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    ActiveTheme, Disableable, Selectable, Sizable,
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    tooltip::Tooltip,
};
use gpui_kit::{
    AnyElement, Context, FontWeight, IntoElement, SharedString, div, prelude::*, px, rems,
};
use std::{collections::BTreeSet, fmt::Write as _};

impl Workbench {
    pub(super) fn poll_receiver_binding_changes(&mut self) -> bool {
        for (port, generation) in self.manager.take_receiver_pairing_changes() {
            let receiver = self.inventory.receivers.iter().find(|receiver| {
                receiver_port(receiver, &self.inventory).is_some_and(|endpoint| {
                    crate::serial_port::same_port_name(&endpoint.port_name, &port)
                        && self
                            .manager
                            .connected_identity_for(endpoint, generation)
                            .is_some_and(|identity| identity.role == DeviceRole::Receiver)
                })
            });
            if let Some(receiver) = receiver {
                self.monitor.observe_pairing_mutation(receiver);
            }
        }
        let mut invalidations = self.monitor.take_binding_invalidations();
        for receiver in &self.inventory.receivers {
            if !invalidations
                .iter()
                .any(|(changed, _)| changed.key == receiver.key)
                && self
                    .monitor
                    .observation_for(receiver)
                    .is_some_and(|observation| {
                        self.flash
                            .receiver_binding_replaced(&receiver.key, &observation.trackers)
                    })
            {
                // A fully removed reader may have dropped its earlier fingerprints.
                invalidations.push((
                    receiver.clone(),
                    crate::discovery::BindingResetCause::AddressReplacement,
                ));
            }
        }
        for (receiver, _) in &invalidations {
            revoke_receiver_tracker_selection(
                &receiver.key,
                &mut self.selected,
                &mut self.command_focus,
            );
            self.flash.revoke_receiver_bindings(&receiver.key);
            self.capture.revoke_receiver_bindings(&receiver.key);
        }
        if !invalidations.is_empty() {
            self.selection_changed();
        }
        !invalidations.is_empty()
    }

    pub(super) fn visible_trackers(&self) -> impl Iterator<Item = &TrackerLink> {
        self.inventory
            .receivers
            .iter()
            .filter_map(|receiver| self.monitor.observation(&receiver.key))
            .flat_map(|observation| &observation.trackers)
    }

    pub(super) fn selected_tracker(&self, key: &str) -> Option<&TrackerLink> {
        let (receiver, slot) = key.strip_prefix("tracker:")?.rsplit_once(':')?;
        let slot = slot.parse::<u8>().ok()?;
        self.visible_trackers()
            .find(|tracker| tracker.receiver_key == receiver && tracker.slot == slot)
    }

    pub(super) fn sync_device_tracker_availability(&mut self) {
        let trackers = self
            .inventory
            .receivers
            .iter()
            .filter_map(|receiver| self.monitor.observation(&receiver.key))
            .flat_map(|observation| &observation.trackers);
        if prune_unavailable_device_trackers(
            self.active_page,
            &mut self.selected,
            &mut self.command_focus,
            trackers,
            |tracker| {
                tracker_selectable(
                    tracker,
                    self.manager
                        .sessions()
                        .map(|session| (session.port.as_str(), session.state, &session.identity)),
                )
            },
        ) {
            self.selection_changed();
        }
    }

    fn selection_box(&self, key: String, label: String, cx: &mut Context<Self>) -> Checkbox {
        Checkbox::new(SharedString::from(format!("check:{key}")))
            .accessibility_label(format!("Select {label}"))
            .checked(self.selected.contains(&key))
            .on_click(cx.listener(move |this, checked, _, cx| {
                cx.stop_propagation();
                let available = !key.starts_with("tracker:")
                    || this.selected_tracker(&key).is_some_and(|tracker| {
                        tracker_selectable(
                            tracker,
                            this.manager.sessions().map(|session| {
                                (session.port.as_str(), session.state, &session.identity)
                            }),
                        )
                    });
                if !set_device_selection(&mut this.selected, &key, *checked, available) {
                    return;
                }
                this.selection_changed();
                cx.notify();
            }))
    }

    fn port_row(
        &self,
        port: &str,
        label: &str,
        context: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if let Some(endpoint) = self.port_endpoint(port)
            && !console_port_allowed(endpoint, &self.inventory)
        {
            let role = if endpoint.role() == SerialRole::ReceiverRawData {
                "devices.serial.role.raw_data"
            } else {
                "devices.serial.role.unavailable"
            };
            return div()
                .id(SharedString::from(format!(
                    "non-console-port:{context}:{port}"
                )))
                .test_support()
                .flex()
                .items_center()
                .gap_3()
                .px_3()
                .py_2()
                .border_b_1()
                .border_color(cx.theme().border)
                .child(div().text_sm().child(port.to_owned()))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .truncate()
                        .child(self.serial_device_label(port, serial_display_name(port, label))),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(tr(role)),
                )
                .into_any_element();
        }
        let session = self.manager.session(port);
        let state = session.map(|session| session.state);
        let reconnecting = self.manager.is_reconnecting(port);
        let name = port.to_owned();
        let focus_port = port.to_owned();
        let metadata = self
            .port_endpoint(port)
            .and_then(|endpoint| self.serial_reported_name(endpoint))
            .unwrap_or(label);
        let mut details = format!("{port}\n{label}");
        if metadata != label {
            let _ = write!(details, "\n{metadata}");
        }
        if let Some(endpoint) = self
            .inventory
            .ports
            .iter()
            .find(|endpoint| endpoint.port_name == port)
        {
            if let Some(vid) = endpoint.vid {
                let _ = write!(details, "\nUSB VID {vid:04X}");
            }
            if let Some(pid) = endpoint.pid {
                let _ = write!(details, "\nUSB PID {pid:04X}");
            }
        }
        let mut row = div()
            .flex()
            .flex_shrink_0()
            .items_center()
            .min_w_0()
            .gap_3()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().sidebar)
            .child(self.selection_box(
                format!("port:{port}"),
                format!(
                    "{} ({port})",
                    self.serial_device_label(port, serial_display_name(port, metadata))
                ),
                cx,
            ))
            .child(
                Button::new(SharedString::from(format!("port-focus:{context}:{port}")))
                    .small()
                    .label(port.to_owned())
                    .map(|button| context_button(
                        button,
                        matches!(&self.command_focus, Some(CommandFocus::Serial(focus)) if focus == port),
                        cx,
                    ))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.focus_serial(&focus_port);
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(
                        div()
                            .id(SharedString::from(format!("port-label:{context}:{port}")))
                            .min_w_0()
                            .w_full()
                            .truncate()
                            .child(self.serial_device_label(port, serial_display_name(port, metadata)))
                            .tooltip(move |window, cx| {
                                Tooltip::new(details.clone()).build(window, cx)
                            }),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .flex_shrink_0()
                            .child(connection_status(state, reconnecting, cx)),
                    )
                    .child(
                        Button::new(SharedString::from(format!("connect:{context}:{port}")))
                            .small()
                            .w(px(CONNECTION_ACTION_WIDTH))
                            .label(connection_action(state, reconnecting))
                            .disabled(!reconnecting && state == Some(ConnectionState::Closing))
                            .on_click(cx.listener(move |this, _, _, cx| this.toggle_connection(&name, cx))),
                    ),
            );
        if let Some(error) = session.and_then(|session| session.last_error.as_ref()) {
            row = row.child(
                div()
                    .w(px(160.))
                    .min_w_0()
                    .text_xs()
                    .text_color(cx.theme().danger)
                    .child(clipped(
                        format!("port-error:{context}:{port}"),
                        error.clone(),
                    )),
            );
        }
        row.into_any_element()
    }

    pub(super) fn roster(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut roster = div()
            .id("device-roster")
            .flex_1()
            .flex()
            .flex_col()
            .gap_3()
            .min_h_0()
            .min_w_0()
            .overflow_y_scroll();
        let mut associated = BTreeSet::new();
        for receiver in &self.inventory.receivers {
            let key = receiver.key.clone();
            let observation = self.monitor.observation_for(receiver);
            let all_receiver = key.clone();
            let receiver_focus = CommandFocus::Receiver(key.clone());
            let receiver_name = receiver
                .serial_number
                .as_deref()
                .and_then(|serial| receiver.label.strip_suffix(serial))
                .and_then(|label| label.strip_suffix(" · "))
                .unwrap_or(&receiver.label);
            let group =
                div()
                    .flex()
                    .flex_col()
                    .min_w_0()
                    .gap_1()
                    .px_3()
                    .py_2()
                    .bg(cx.theme().muted)
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .min_w_0()
                            .gap_3()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(180.))
                                    .flex()
                                    .child(
                                        Button::new(SharedString::from(format!("receiver-focus:{key}")))
                                            .small()
                                            .ghost()
                                            .self_start()
                                            .max_w_full()
                                            .min_w_0()
                                            .px_0()
                                            .border_0()
                                            .accessibility_label(self.receiver_label(receiver, receiver.label.clone()))
                                            .tooltip(self.receiver_label(receiver, receiver.label.clone()))
                                            .child(
                                                div()
                                                    .min_w_0()
                                                    .truncate()
                                                    .text_left()
                                                    .font_weight(FontWeight::SEMIBOLD)
                                                    .child(self.receiver_label(receiver, receiver_name)),
                                            )
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                cx.stop_propagation();
                                                this.focus_command_target(receiver_focus.clone());
                                                cx.notify();
                                            })),
                                    ),
                            )
                            .child(
                                Button::new(SharedString::from(format!("all-trackers:{key}")))
                                    .small()
                                    .map(|button| context_button(
                                        button,
                                        matches!(&self.sidebar.scope, CommandScope::AllTrackers(receiver) if receiver == &key),
                                        cx,
                                    ))
                                    .min_w(px(156.))
                                    .label(tr("devices.receiver.select_all_online"))
                                    .tooltip(tr("devices.receiver.select_all_online_hint"))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.change_command_scope(CommandScope::AllTrackers(
                                            all_receiver.clone(),
                                        ));
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .min_w_0()
                            .gap_x_3()
                            .gap_y_1()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .when_some(receiver.serial_number.as_ref(), |row, serial| {
                                row.child(receiver_metadata(
                                    format!("receiver-serial:{key}"),
                                    serial,
                                ))
                            })
                            .child(receiver_metadata(
                                format!("receiver-count:{key}"),
                                rust_i18n::t!(
                                    "devices.receiver.tracker_count",
                                    online = observation.map_or(0, |value| value
                                        .trackers
                                        .iter()
                                        .filter(|tracker| tracker_selectable(
                                            tracker,
                                            self.manager.sessions().map(|session| (
                                                session.port.as_str(),
                                                session.state,
                                                &session.identity,
                                            )),
                                        ))
                                        .count()),
                                    total =
                                        observation.map_or(0, |value| value.trackers.len())
                                )
                                .as_ref(),
                            )),
                    );
            let mut receiver_group = div()
                .flex()
                .flex_col()
                .flex_shrink_0()
                .min_w_0()
                .rounded_md()
                .border_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().sidebar)
                .overflow_hidden()
                .child(group);
            let serial_ports = receiver_serial_ports(receiver, &self.inventory);
            if let Some(port) = serial_ports.as_ref().and_then(|ports| ports.console) {
                associated.insert(port.port_name.clone());
                receiver_group =
                    receiver_group.child(self.port_row(&port.port_name, &port.label, &key, cx));
            } else {
                receiver_group = receiver_group.child(
                    div()
                        .id(SharedString::from(format!(
                            "receiver-missing-console:{key}"
                        )))
                        .test_support()
                        .px_3()
                        .py_1()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(tr("devices.receiver.error.serial_unidentified")),
                );
            }
            if let Some(port) = serial_ports.as_ref().and_then(|ports| ports.raw_data) {
                associated.insert(port.port_name.clone());
                receiver_group =
                    receiver_group.child(self.port_row(&port.port_name, &port.label, &key, cx));
            }
            if let Some(observation) = observation {
                if let Some(error) = &observation.error {
                    receiver_group = receiver_group.child(
                        div()
                            .px_3()
                            .py_1()
                            .text_xs()
                            .text_color(cx.theme().danger)
                            .child(clipped(format!("scan-error:{key}"), error.clone())),
                    );
                }
                let mut cards = Vec::with_capacity(observation.trackers.len());
                for tracker in &observation.trackers {
                    let tracker_key = tracker_selection_key(tracker);
                    let focus = CommandFocus::Tracker {
                        receiver: key.clone(),
                        slot: tracker.slot,
                        address: tracker.address.clone(),
                    };
                    let focused = self.command_focus.as_ref() == Some(&focus);
                    let selected = self.selected.contains(&tracker_key);
                    let available = tracker_selectable(
                        tracker,
                        self.manager.sessions().map(|session| {
                            (session.port.as_str(), session.state, &session.identity)
                        }),
                    );
                    let focusable = available || self.current_nickname_key(&focus).is_some();
                    let tracker_label = self.tracker_label(
                        tracker,
                        rust_i18n::t!("devices.tracker.slot_label", slot = tracker.slot)
                            .to_string(),
                    );
                    let status = tr(if available {
                        "devices.tracker.state.online"
                    } else {
                        "devices.tracker.state.offline"
                    });
                    let subdued = !available;
                    let tps = observation.tps(tracker.slot);
                    let battery = observation.battery(tracker.slot).filter(|_| tracker.online);
                    let (activity, outline_width) = tracker_activity_outline(
                        tps,
                        cx.theme().is_dark(),
                        self.preferences.accent,
                    );
                    let card = Button::new(SharedString::from(format!("card:{tracker_key}")))
                        .ghost()
                        .w_full()
                        .min_w_0()
                        .h_auto()
                        .p_3()
                        .text_left()
                        .accessibility_label(format!("Set current target: {tracker_label}"))
                        .selected(selected || focused)
                        .toggled(focused)
                        .disabled(!focusable)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let CommandFocus::Tracker {
                                receiver,
                                slot,
                                address,
                            } = &focus
                            else {
                                return;
                            };
                            if this.current_nickname_key(&focus).is_none()
                                && !this.visible_trackers().any(|tracker| {
                                    tracker.receiver_key == *receiver
                                        && tracker.slot == *slot
                                        && tracker.address == *address
                                        && tracker_selectable(
                                            tracker,
                                            this.manager.sessions().map(|session| {
                                                (
                                                    session.port.as_str(),
                                                    session.state,
                                                    &session.identity,
                                                )
                                            }),
                                        )
                                })
                            {
                                return;
                            }
                            this.focus_command_target(focus.clone());
                            cx.notify();
                        }))
                        .rounded_md()
                        .border_1()
                        .border_color(if selected {
                            cx.theme().primary
                        } else if focused {
                            cx.theme().ring
                        } else {
                            cx.theme().border.opacity(0.)
                        })
                        .bg(if selected || focused {
                            cx.theme().accent
                        } else if subdued {
                            cx.theme().sidebar
                        } else {
                            cx.theme().group_box
                        })
                        .child(
                            div()
                                .w_full()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .h_5()
                                        .gap_2()
                                        .child(div().w_4().flex_shrink_0())
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .text_sm()
                                                .line_height(rems(1.25))
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .text_color(if selected || focused {
                                                    cx.theme().accent_foreground
                                                } else if subdued {
                                                    cx.theme().muted_foreground
                                                } else {
                                                    cx.theme().foreground
                                                })
                                                .child(clipped(
                                                    format!("tracker-label:{tracker_key}"),
                                                    tracker_label.clone(),
                                                )),
                                        )
                                        .when_some(battery, |header, battery| {
                                            header.child(
                                                div()
                                                    .flex_shrink_0()
                                                    .text_xs()
                                                    .line_height(rems(1.))
                                                    .font_family(
                                                        cx.theme().mono_font_family.clone(),
                                                    )
                                                    .text_color(if selected || focused {
                                                        cx.theme().accent_foreground
                                                    } else {
                                                        cx.theme().muted_foreground
                                                    })
                                                    .child(format!("{}%", battery.percent)),
                                            )
                                        }),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .h_4()
                                        .gap_2()
                                        .text_xs()
                                        .line_height(rems(1.))
                                        .font_family(cx.theme().mono_font_family.clone())
                                        .text_color(if selected || focused {
                                            cx.theme().accent_foreground
                                        } else {
                                            cx.theme().muted_foreground
                                        })
                                        .child(div().flex_1().min_w_0().child(clipped(
                                            format!("address:{tracker_key}"),
                                            tracker.address.clone().unwrap_or_else(|| {
                                                tr("devices.tracker.address_unobserved")
                                            }),
                                        )))
                                        .when_some(battery, |row, battery| {
                                            row.child(
                                                div()
                                                    .flex_shrink_0()
                                                    .child(format!("{:.2} V", battery.voltage_v)),
                                            )
                                        }),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .h_4()
                                        .gap_1()
                                        .text_xs()
                                        .line_height(rems(1.))
                                        .whitespace_nowrap()
                                        .overflow_hidden()
                                        .text_color(if selected || focused {
                                            cx.theme().accent_foreground
                                        } else {
                                            cx.theme().muted_foreground
                                        })
                                        .child(
                                            div()
                                                .flex()
                                                .flex_1()
                                                .min_w_0()
                                                .items_center()
                                                .gap_1()
                                                .child(
                                                    div()
                                                        .size_1p5()
                                                        .flex_shrink_0()
                                                        .rounded_md()
                                                        .bg(if tps > 0 {
                                                            activity
                                                        } else {
                                                            cx.theme().muted_foreground
                                                        }),
                                                )
                                                .child(
                                                    div().flex_1().min_w_0().child(clipped(
                                                        format!("traffic:{tracker_key}"),
                                                        rust_i18n::t!(
                                                            "devices.tracker.traffic_rate",
                                                            tps = tps
                                                        )
                                                        .to_string(),
                                                    )),
                                                ),
                                        )
                                        .child(
                                            div().min_w_0().child(clipped(
                                                format!("route:{tracker_key}"),
                                                status,
                                            )),
                                        )
                                        .when(selected || focused, |row| {
                                            row.child(
                                                div()
                                                    .flex_shrink_0()
                                                    .text_xs()
                                                    .line_height(rems(1.))
                                                    .px_1()
                                                    .rounded_sm()
                                                    .bg(if selected {
                                                        cx.theme().primary
                                                    } else {
                                                        cx.theme().group_box
                                                    })
                                                    .text_color(if selected {
                                                        cx.theme().primary_foreground
                                                    } else {
                                                        cx.theme().accent_foreground
                                                    })
                                                    .child(if selected {
                                                        tr("devices.tracker.selection.selected")
                                                    } else {
                                                        tr("devices.tracker.selection.current")
                                                    }),
                                            )
                                        }),
                                ),
                        );
                    // The checkbox is a sibling, not an interactive child of the target button.
                    // Its header lane shares the button's padding plus the one-pixel border.
                    let card = div()
                        .id(SharedString::from(format!(
                            "tracker-controls:{tracker_key}:{:?}",
                            tracker.address
                        )))
                        .relative()
                        .child(card)
                        .child(
                            div()
                                .absolute()
                                .top_3()
                                .left_3()
                                .m_px()
                                .h_5()
                                .flex()
                                .items_center()
                                // Even a disabled checkbox owns this lane, not the card behind it.
                                .block_mouse_except_scroll()
                                .child(
                                    self.selection_box(tracker_key.clone(), tracker_label, cx)
                                        .disabled(!available),
                                ),
                        );
                    cards
                        .push(tracker_card_frame(card, activity, outline_width).into_any_element());
                }
                if cards.is_empty() {
                    if observation.error.is_none() {
                        receiver_group = receiver_group.child(
                            div()
                                .p_2()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(tr("devices.receiver.listening_hint")),
                        );
                    }
                } else {
                    receiver_group = receiver_group.child(TrackerCardGrid {
                        id: SharedString::from(format!("tracker-grid:{key}")),
                        horizontal_padding: TRACKER_GRID_PADDING,
                        cards,
                    });
                }
            } else {
                receiver_group = receiver_group.child(
                    div()
                        .px_3()
                        .py_2()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(tr("devices.receiver.listener_starting")),
                );
            }
            roster = roster.child(receiver_group);
        }
        let mut has_other = false;
        for port in &self.inventory.ports {
            if associated.contains(&port.port_name) {
                continue;
            }
            if !has_other {
                roster = roster.child(
                    div()
                        .flex_shrink_0()
                        .px_3()
                        .py_2()
                        .bg(cx.theme().muted)
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(tr("devices.roster.wired_title")),
                );
                has_other = true;
            }
            roster = roster.child(self.port_row(&port.port_name, &port.label, "direct", cx));
        }
        if self.inventory.receivers.is_empty() && !has_other {
            roster = roster.child(
                div()
                    .p_3()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(tr("devices.roster.empty_hint")),
            );
        }
        for (index, warning) in self.inventory.warnings.iter().enumerate() {
            roster = roster.child(
                div()
                    .px_3()
                    .text_xs()
                    .text_color(cx.theme().warning)
                    .child(clipped(format!("warning:{index}"), warning.clone())),
            );
        }
        roster.into_any_element()
    }
}

fn receiver_metadata(id: String, text: &str) -> AnyElement {
    let text = SharedString::from(text);
    let tooltip = text.clone();
    div()
        .id(SharedString::from(id))
        .role(gpui_kit::Role::Label)
        .aria_label(text.clone())
        .test_support()
        .min_w_0()
        .max_w_full()
        .truncate()
        .child(text)
        .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
        .into_any_element()
}

pub(super) fn receiver_command_port<'a>(
    key: &str,
    inventory: &'a Inventory,
) -> Result<&'a SerialEndpoint, String> {
    let receiver = inventory
        .receivers
        .iter()
        .find(|receiver| receiver.key == key)
        .ok_or_else(|| tr("devices.receiver.error.not_present"))?;
    receiver_port(receiver, inventory)
        .ok_or_else(|| tr("devices.receiver.error.serial_unidentified"))
}

// Retained terminal history is not evidence about the device currently occupying a COM name.
pub(super) fn live_tracker_port<'a>(
    tracker: &TrackerLink,
    sessions: impl IntoIterator<Item = (&'a str, ConnectionState, &'a DeviceIdentity)>,
) -> Option<&'a str> {
    tracker_port(
        tracker,
        sessions
            .into_iter()
            .filter(|(_, state, identity)| {
                *state == ConnectionState::Connected && identity.role != DeviceRole::Receiver
            })
            .map(|(port, _, identity)| (port, identity)),
    )
}

pub(super) fn tracker_selectable<'a>(
    tracker: &TrackerLink,
    sessions: impl IntoIterator<Item = (&'a str, ConnectionState, &'a DeviceIdentity)>,
) -> bool {
    tracker.online || live_tracker_port(tracker, sessions).is_some()
}

pub(super) fn set_device_selection(
    selected: &mut BTreeSet<String>,
    key: &str,
    checked: bool,
    available: bool,
) -> bool {
    if !available {
        return false;
    }
    if checked {
        selected.insert(key.to_owned())
    } else {
        selected.remove(key)
    }
}

// Offline identities can keep focus, but never actionable batch selection.
// OTA retains its own queried targets across monitor handoff.
pub(super) fn prune_unavailable_device_trackers<'a>(
    page: FeaturePage,
    selected: &mut BTreeSet<String>,
    focus: &mut Option<CommandFocus>,
    trackers: impl Iterator<Item = &'a TrackerLink> + Clone,
    available: impl Fn(&TrackerLink) -> bool,
) -> bool {
    if page != FeaturePage::DeviceSetup {
        return false;
    }
    let before = selected.len();
    selected.retain(|key| {
        let Some(tracker_key) = key.strip_prefix("tracker:") else {
            return true;
        };
        let Some((receiver, slot)) = tracker_key.rsplit_once(':') else {
            return false;
        };
        let Ok(slot) = slot.parse::<u8>() else {
            return false;
        };
        trackers.clone().any(|tracker| {
            tracker.receiver_key == receiver && tracker.slot == slot && available(tracker)
        })
    });
    let focus_unavailable = match focus {
        Some(CommandFocus::Tracker {
            receiver,
            slot,
            address,
        }) => !trackers.clone().any(|tracker| {
            tracker.receiver_key == *receiver
                && tracker.slot == *slot
                && tracker.address == *address
        }),
        _ => false,
    };
    if focus_unavailable {
        *focus = None;
    }
    selected.len() != before || focus_unavailable
}

pub(super) fn tracker_selection_key(tracker: &TrackerLink) -> String {
    format!("tracker:{}:{}", tracker.receiver_key, tracker.slot)
}

pub(super) fn clear_tracker_selection(selected: &mut BTreeSet<String>) {
    selected.retain(|key| !key.starts_with("tracker:"));
}

pub(super) fn revoke_receiver_tracker_selection(
    receiver: &str,
    selected: &mut BTreeSet<String>,
    focus: &mut Option<CommandFocus>,
) {
    selected.retain(|key| {
        key.strip_prefix("tracker:")
            .and_then(|key| key.rsplit_once(':'))
            .is_none_or(|(owner, _)| owner != receiver)
    });
    if matches!(focus, Some(CommandFocus::Tracker { receiver: owner, .. }) if owner == receiver) {
        *focus = None;
    }
}
