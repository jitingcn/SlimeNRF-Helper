use super::*;
use crate::receiver_events::EventWatchStatus;
use gpui_kit::component::popover::{Popover, PopoverState};
use std::collections::BTreeMap;

#[derive(Clone, Copy)]
enum FilterGroup {
    Receiver,
    Tracker,
    Category,
    Kind,
}

enum FilterValue {
    All,
    Receiver(String),
    Tracker(u8),
    Category(Category),
    Kind(u8),
}

struct FilterChoice {
    value: FilterValue,
    label: String,
    checked: bool,
}

impl Workbench {
    fn hid_receiver_choices(&self) -> Vec<(&str, String)> {
        let identities = receiver_identity_groups(
            self.inventory
                .receivers
                .iter()
                .map(|receiver| {
                    (
                        receiver.key.as_str(),
                        receiver
                            .serial_number
                            .as_deref()
                            .and_then(crate::device_registry::DeviceKey::receiver_serial),
                    )
                })
                .chain(self.hid_log.retained_records().map(|record| {
                    let source = &record.source;
                    let key = if source.vendor_id == 0x1209 && source.product_id == 0x7690 {
                        source
                            .serial_number
                            .as_deref()
                            .and_then(crate::device_registry::DeviceKey::receiver_serial)
                    } else {
                        None
                    };
                    (source.receiver_key.as_str(), key)
                })),
        );
        let mut choices = receiver_choices(
            self.inventory
                .receivers
                .iter()
                .map(|receiver| (receiver.key.as_str(), receiver.label.as_str()))
                .chain(self.hid_log.retained_records().map(|record| {
                    (
                        record.source.receiver_key.as_str(),
                        record.source.label.as_str(),
                    )
                })),
            self.hid_log.receiver_filter.iter().map(String::as_str),
            &tr("hid.filter.disconnected_receiver"),
        );
        for (key, label) in &mut choices {
            if identities.get(key).is_some_and(Option::is_none) {
                *label = receiver_key_label(&tr("hid.filter.unverified_receiver"), key);
                continue;
            }
            if let Some(receiver) = self
                .inventory
                .receivers
                .iter()
                .find(|receiver| receiver.key == *key)
            {
                *label = self.receiver_label(receiver, std::mem::take(label));
            } else {
                let mut records = self
                    .hid_log
                    .retained_records()
                    .filter(|record| record.source.receiver_key == *key);
                if let Some(record) = records.next()
                    && records
                        .all(|other| other.source.serial_number == record.source.serial_number)
                {
                    *label = self.hid_source_label(&record.source, std::mem::take(label));
                }
            }
        }
        choices
    }

    pub(super) fn hid_log_filters(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .child(self.hid_filter_popover(FilterGroup::Receiver, cx))
                    .child(self.hid_filter_popover(FilterGroup::Tracker, cx))
                    .child(self.hid_filter_popover(FilterGroup::Category, cx))
                    .child(self.hid_filter_popover(FilterGroup::Kind, cx)),
            )
            .child(self.hid_event_control(cx))
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(tr("hid.filter.display_only_hint")),
            )
            .into_any_element()
    }

    fn hid_filter_popover(&self, group: FilterGroup, cx: &mut Context<Self>) -> AnyElement {
        let (id, label) = match group {
            FilterGroup::Receiver => {
                let selected = &self.hid_log.receiver_filter;
                let single = selected.first().and_then(|key| {
                    self.hid_receiver_choices()
                        .into_iter()
                        .find(|(available, _)| *available == key.as_str())
                        .map(|(_, label)| label)
                });
                (
                    "hid-log-receiver",
                    filter_label(
                        "hid.filter.all_receivers",
                        "hid.filter.receivers",
                        selected.len(),
                        single,
                    ),
                )
            }
            FilterGroup::Tracker => {
                let selected = &self.hid_log.tracker_filter;
                (
                    "hid-log-tracker",
                    filter_label(
                        "hid.filter.all_trackers",
                        "hid.filter.trackers",
                        selected.len(),
                        selected
                            .first()
                            .map(|id| format!("{} {id}", tr("hid.field.tracker_id"))),
                    ),
                )
            }
            FilterGroup::Category => {
                let selected = &self.hid_log.category_filter;
                (
                    "hid-log-type",
                    filter_label(
                        "hid.filter.all_categories",
                        "hid.filter.categories",
                        selected.len(),
                        selected.first().map(|category| category.label()),
                    ),
                )
            }
            FilterGroup::Kind => {
                let selected = &self.hid_log.kind_filter;
                (
                    "hid-log-kind",
                    filter_label(
                        "hid.filter.all_event_kinds",
                        "hid.filter.event_kinds",
                        selected.len(),
                        selected.first().map(|kind| decode::event_kind_label(*kind)),
                    ),
                )
            }
        };
        let owner = cx.weak_entity();
        Popover::new(("hid-log-filter-popup", group as usize))
            .trigger(
                Button::new(id)
                    .small()
                    .label(label)
                    .icon(gpui_kit::component::IconName::ChevronDown),
            )
            .content(move |_, _, cx| {
                let popup = cx.weak_entity();
                owner
                    .update(cx, |this, cx| this.hid_filter_choices(group, popup, cx))
                    .unwrap_or_else(|_| div().into_any_element())
            })
            .into_any_element()
    }

    fn hid_filter_choices(
        &self,
        group: FilterGroup,
        popup: WeakEntity<PopoverState>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut choices = Vec::new();
        match group {
            FilterGroup::Receiver => {
                let selected = &self.hid_log.receiver_filter;
                choices.push(FilterChoice {
                    value: FilterValue::All,
                    label: tr("hid.filter.all_receivers"),
                    checked: selected.is_empty(),
                });
                choices.extend(self.hid_receiver_choices().into_iter().map(|(key, label)| {
                    FilterChoice {
                        checked: selected.contains(key),
                        value: FilterValue::Receiver(key.to_owned()),
                        label,
                    }
                }));
            }
            FilterGroup::Tracker => {
                let selected = &self.hid_log.tracker_filter;
                choices.push(FilterChoice {
                    value: FilterValue::All,
                    label: tr("hid.filter.all_trackers"),
                    checked: selected.is_empty(),
                });
                let mut ids: BTreeSet<u8> = (0..16).collect();
                ids.extend(
                    self.hid_log
                        .retained_rows()
                        .filter_map(|row| row.decoded.tracker_id),
                );
                ids.extend(selected.iter().copied());
                choices.extend(ids.into_iter().map(|id| FilterChoice {
                    value: FilterValue::Tracker(id),
                    label: format!("{} {id}", tr("hid.field.tracker_id")),
                    checked: selected.contains(&id),
                }));
            }
            FilterGroup::Category => {
                let selected = &self.hid_log.category_filter;
                choices.push(FilterChoice {
                    value: FilterValue::All,
                    label: tr("hid.filter.all_categories"),
                    checked: selected.is_empty(),
                });
                choices.extend(Category::ALL.into_iter().map(|category| FilterChoice {
                    value: FilterValue::Category(category),
                    label: category.label(),
                    checked: selected.contains(&category),
                }));
            }
            FilterGroup::Kind => {
                let selected = &self.hid_log.kind_filter;
                choices.push(FilterChoice {
                    value: FilterValue::All,
                    label: tr("hid.filter.all_event_kinds"),
                    checked: selected.is_empty(),
                });
                let mut kinds: BTreeSet<u8> = [1, 2, 3, 4, 5, 6, 7, 0x20, 0x21, 0x30, 0x31]
                    .into_iter()
                    .collect();
                kinds.extend(
                    self.hid_log
                        .retained_rows()
                        .filter_map(|row| row.decoded.event_kind),
                );
                kinds.extend(selected.iter().copied());
                choices.extend(kinds.into_iter().map(|kind| FilterChoice {
                    value: FilterValue::Kind(kind),
                    label: decode::event_kind_label(kind),
                    checked: selected.contains(&kind),
                }));
            }
        }
        let height = px((choices.len() as f32 * 36. - 4.).min(320.));
        let mut content = div()
            .id(("hid-log-filter-choices", group as usize))
            .w(px(320.))
            .h(height)
            .flex_shrink_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_1();
        for choice in choices {
            let id = SharedString::from(match &choice.value {
                FilterValue::All => format!("hid-log-filter-all:{}", group as usize),
                FilterValue::Receiver(key) => format!("hid-log-filter-receiver:{key}"),
                FilterValue::Tracker(id) => format!("hid-log-filter-tracker:{id}"),
                FilterValue::Category(category) => {
                    format!("hid-log-filter-category:{}", *category as usize)
                }
                FilterValue::Kind(kind) => format!("hid-log-filter-kind:{kind}"),
            });
            let popup = popup.clone();
            content = content.child(
                div().w_full().flex_shrink_0().p_1().child(
                    Checkbox::new(id)
                        .label(choice.label)
                        .checked(choice.checked)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            match &choice.value {
                                FilterValue::All => match group {
                                    FilterGroup::Receiver => this.hid_log.receiver_filter.clear(),
                                    FilterGroup::Tracker => this.hid_log.tracker_filter.clear(),
                                    FilterGroup::Category => this.hid_log.category_filter.clear(),
                                    FilterGroup::Kind => this.hid_log.kind_filter.clear(),
                                },
                                FilterValue::Receiver(key) => {
                                    if !this.hid_log.receiver_filter.remove(key) {
                                        this.hid_log.receiver_filter.insert(key.clone());
                                    }
                                }
                                FilterValue::Tracker(id) => {
                                    if !this.hid_log.tracker_filter.remove(id) {
                                        this.hid_log.tracker_filter.insert(*id);
                                    }
                                }
                                FilterValue::Category(category) => {
                                    this.hid_log.toggle_category(*category)
                                }
                                FilterValue::Kind(kind) => this.hid_log.toggle_kind(*kind),
                            }
                            this.hid_log.paused = false;
                            this.hid_log.rebuild();
                            cx.notify();
                            let _ = popup.update(cx, |_, cx| cx.notify());
                        })),
                ),
            );
        }
        content.into_any_element()
    }

    fn hid_event_control(&self, cx: &mut Context<Self>) -> AnyElement {
        let selected = self.hid_log.watch_receiver.clone();
        let requested = self.monitor.requested_events_count();
        let mut connected: Vec<_> = self
            .hid_receiver_choices()
            .into_iter()
            .filter(|(key, _)| {
                self.inventory
                    .receivers
                    .iter()
                    .any(|receiver| receiver.key == *key)
            })
            .map(|(key, label)| (key.to_owned(), label))
            .collect();
        let label = selected.as_ref().map_or_else(
            || tr("hid.events.select_receiver"),
            |key| {
                connected
                    .iter()
                    .find(|(available, _)| available == key)
                    .map(|(_, label)| label.clone())
                    .unwrap_or_else(|| {
                        receiver_key_label(&tr("hid.filter.disconnected_receiver"), key)
                    })
            },
        );
        for (key, label) in &mut connected {
            *label = format!(
                "{} · {}",
                label,
                watch_status_label(&self.monitor.events_status(key))
            );
        }
        let owner = cx.weak_entity();
        let receiver = Button::new("hid-log-watch-receiver")
            .small()
            .label(label)
            .disabled(connected.is_empty())
            .dropdown_menu(move |mut menu, _, _| {
                for (key, label) in &connected {
                    let checked = selected.as_ref() == Some(key);
                    let key = key.clone();
                    let owner = owner.clone();
                    menu = menu.item(PopupMenuItem::new(label.clone()).checked(checked).on_click(
                        move |_, _, cx| {
                            let _ = owner.update(cx, |this, cx| {
                                // Choosing a control target does not change subscription intent.
                                this.hid_log.watch_receiver = Some(key.clone());
                                cx.notify();
                            });
                        },
                    ));
                }
                menu
            });
        let endpoint = self.hid_log.watch_receiver.as_ref().and_then(|key| {
            self.inventory
                .receivers
                .iter()
                .find(|receiver| &receiver.key == key)
        });
        let status = self
            .hid_log
            .watch_receiver
            .as_ref()
            .map(|key| self.monitor.events_status(key));
        let enabled = self
            .hid_log
            .watch_receiver
            .as_ref()
            .is_some_and(|key| self.monitor.events_enabled(key));
        let failed = matches!(
            &status,
            Some(EventWatchStatus::Unsupported(_) | EventWatchStatus::Error(_))
        );
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .child(div().text_xs().child(tr("hid.events.heading")))
            .child(receiver)
            .child(
                Button::new("hid-log-watch")
                    .small()
                    .label(tr(if failed {
                        "hid.events.retry"
                    } else {
                        "hid.events.subscribe"
                    }))
                    .tooltip(tr("hid.events.subscribe_hint"))
                    .disabled(endpoint.is_none() || (enabled && !failed))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.hid_subscribe_events(cx);
                    })),
            )
            .child(
                Button::new("hid-log-watch-stop-all")
                    .small()
                    .label(tr("hid.events.stop_all"))
                    .tooltip(tr("hid.events.stop_all_hint"))
                    .disabled(requested == 0)
                    .on_click(cx.listener(|this, _, _, cx| this.hid_stop_all_events(cx))),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(
                        rust_i18n::t!("hid.events.requested_count", count = requested).to_string(),
                    ),
            )
            .child(
                div()
                    .max_w(px(400.))
                    .text_xs()
                    .whitespace_normal()
                    .text_color(cx.theme().muted_foreground)
                    .child(match status {
                        None => tr("hid.events.select_receiver_hint"),
                        Some(EventWatchStatus::Unsupported(error)) => {
                            format!("{} · {error}", tr("hid.events.status.unsupported"))
                        }
                        Some(EventWatchStatus::Error(error)) => {
                            format!("{} · {error}", tr("hid.events.status.error"))
                        }
                        Some(status) => watch_status_label(&status),
                    }),
            )
            .into_any_element()
    }
}

fn receiver_identity_groups<'a>(
    sources: impl IntoIterator<Item = (&'a str, Option<crate::device_registry::DeviceKey>)>,
) -> BTreeMap<&'a str, Option<crate::device_registry::DeviceKey>> {
    let mut identities = BTreeMap::new();
    for (path, key) in sources {
        identities
            .entry(path)
            .and_modify(|known| {
                if *known != key {
                    *known = None;
                }
            })
            .or_insert(key);
    }
    identities
}

// Inventory labels take precedence over retained records. The immutable key orders
// choices independently of record eviction and identifies otherwise equal labels.
fn receiver_choices<'a>(
    known: impl IntoIterator<Item = (&'a str, &'a str)>,
    selected: impl IntoIterator<Item = &'a str>,
    disconnected: &str,
) -> Vec<(&'a str, String)> {
    let mut receivers = BTreeMap::new();
    for (key, label) in known {
        receivers.entry(key).or_insert(Some(label));
    }
    for key in selected {
        receivers.entry(key).or_insert(None);
    }
    let mut counts = BTreeMap::new();
    for label in receivers.values() {
        *counts
            .entry(label.unwrap_or(disconnected))
            .or_insert(0usize) += 1;
    }
    receivers
        .into_iter()
        .map(|(key, known_label)| {
            let label = known_label.unwrap_or(disconnected);
            let label = if known_label.is_none() || counts[label] > 1 {
                receiver_key_label(label, key)
            } else {
                label.to_owned()
            };
            (key, label)
        })
        .collect()
}

fn receiver_key_label(label: &str, key: &str) -> String {
    format!("{label} · {:08X}", crc32fast::hash(key.as_bytes()))
}

fn filter_label(all: &str, group: &str, count: usize, single: Option<String>) -> String {
    match count {
        0 => tr(all),
        1 => single.unwrap_or_else(|| tr(group)),
        _ => rust_i18n::t!(
            "hid.filter.selected_count",
            filter = tr(group),
            count = count
        )
        .to_string(),
    }
}

fn watch_status_label(status: &EventWatchStatus) -> String {
    tr(match status {
        EventWatchStatus::Disabled => "hid.events.status.disabled",
        EventWatchStatus::Checking => "hid.events.status.checking",
        EventWatchStatus::Active => "hid.events.status.active",
        EventWatchStatus::Paused => "hid.events.status.paused",
        EventWatchStatus::Unsupported(_) => "hid.events.status.unsupported",
        EventWatchStatus::Error(_) => "hid.events.status.error",
    })
}

#[cfg(test)]
mod tests {
    use super::{receiver_choices, receiver_identity_groups, receiver_key_label};
    use crate::device_registry::DeviceKey;

    #[test]
    fn receiver_choices_are_key_ordered_and_disambiguate_only_when_needed() {
        let inventory = [
            ("receiver-b", "Receiver · shared serial"),
            ("receiver-a", "Receiver · shared serial"),
            ("receiver-c", "Receiver · unique serial"),
        ];
        let retained = [("retained-b", "Receiver"), ("retained-a", "Receiver")];
        let expected = receiver_choices(
            inventory.into_iter().chain(retained),
            ["missing-b", "missing-a"],
            "Disconnected receiver",
        );
        let reordered = receiver_choices(
            inventory
                .into_iter()
                .rev()
                .chain(retained.into_iter().rev()),
            ["missing-a", "missing-b"],
            "Disconnected receiver",
        );
        assert_eq!(expected, reordered);
        assert_eq!(
            expected.iter().map(|(key, _)| *key).collect::<Vec<_>>(),
            [
                "missing-a",
                "missing-b",
                "receiver-a",
                "receiver-b",
                "receiver-c",
                "retained-a",
                "retained-b",
            ]
        );
        assert_eq!(expected[4].1, "Receiver · unique serial");
        assert_eq!(
            expected[2].1,
            receiver_key_label("Receiver · shared serial", "receiver-a")
        );
        assert_ne!(expected[2].1, expected[3].1);
        assert_ne!(expected[5].1, expected[6].1);
        assert_eq!(
            expected[0].1,
            receiver_key_label("Disconnected receiver", "missing-a")
        );

        let after_clear = receiver_choices([], ["missing-a"], "Disconnected receiver");
        assert_eq!(after_clear, [expected[0].clone()]);
        let inventory_precedence = receiver_choices(
            [
                ("receiver-c", "Receiver · unique serial"),
                ("receiver-c", "Old label"),
            ],
            [],
            "Disconnected receiver",
        );
        assert_eq!(
            inventory_precedence,
            [("receiver-c", expected[4].1.clone())]
        );
    }

    #[test]
    fn one_hid_path_never_claims_a_nickname_for_mixed_or_unproven_hardware() {
        let a = DeviceKey::receiver_serial("0011223344556677").unwrap();
        let b = DeviceKey::receiver_serial("8899AABBCCDDEEFF").unwrap();
        let groups = receiver_identity_groups([
            ("reused-path", Some(b)),
            ("reused-path", Some(a)),
            ("same-hardware", Some(a)),
            ("same-hardware", Some(a)),
            ("unknown-source", Some(a)),
            ("unknown-source", None),
        ]);
        assert_eq!(groups["reused-path"], None);
        assert_eq!(groups["same-hardware"], Some(a));
        assert_eq!(groups["unknown-source"], None);
    }
}
