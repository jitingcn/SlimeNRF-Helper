use super::*;

pub(super) fn merge_tracker_snapshot(snapshot: &mut Vec<TrackerLink>, live: &[TrackerLink]) {
    for cached in snapshot.iter_mut() {
        let current = live.iter().find(|tracker| tracker.slot == cached.slot);
        cached.online = current.is_some_and(|tracker| tracker.online && tracker.address.is_some());
        if let Some(address) = current.and_then(|tracker| tracker.address.as_ref())
            && cached.address.as_ref() != Some(address)
        {
            cached.address = Some(address.clone());
        }
    }
    let previous_len = snapshot.len();
    for tracker in live {
        if !snapshot.iter().any(|cached| cached.slot == tracker.slot) {
            snapshot.push(tracker.clone());
        }
    }
    if snapshot.len() != previous_len {
        snapshot.sort_unstable_by_key(|tracker| tracker.slot);
    }
}

pub(super) fn ota_tracker_activity_tps(
    tracker: &TrackerLink,
    current: &[TrackerLink],
    tps: u32,
) -> u32 {
    let Some(address) = tracker
        .address
        .as_deref()
        .and_then(crate::domain::address_digits)
    else {
        return 0;
    };
    // Slots can emit traffic before their RF identity is rediscovered after handoff.
    // Never attribute an unknown or reassigned sender's rate to a retained card.
    if current.iter().any(|live| {
        live.receiver_key == tracker.receiver_key
            && live.slot == tracker.slot
            && live.online
            && live
                .address
                .as_deref()
                .and_then(crate::domain::address_digits)
                == Some(address)
    }) {
        tps
    } else {
        0
    }
}

pub(super) fn selected_tracker_is_known_offline(
    key: &str,
    receiver: &str,
    current: &[TrackerLink],
) -> bool {
    let Some((owner, slot)) = key
        .strip_prefix("tracker:")
        .and_then(|key| key.rsplit_once(':'))
    else {
        return false;
    };
    if owner != receiver {
        return false;
    }
    let Ok(slot) = slot.parse::<u8>() else {
        return false;
    };
    current.iter().any(|tracker| {
        tracker.receiver_key == receiver
            && tracker.slot == slot
            && !tracker.online
            && tracker.address.is_some()
    })
}

pub(super) fn clear_known_offline_tracker_selection(
    selected: &mut BTreeSet<String>,
    receiver: &str,
    current: &[TrackerLink],
) -> bool {
    let before = selected.len();
    selected.retain(|key| !selected_tracker_is_known_offline(key, receiver, current));
    selected.len() != before
}

impl FlashState {
    pub(in crate::application) fn receiver_binding_replaced(
        &self,
        receiver: &str,
        current: &[TrackerLink],
    ) -> bool {
        current
            .iter()
            .filter(|live| live.receiver_key == receiver)
            .any(|live| {
                let Some(address) = live
                    .address
                    .as_deref()
                    .and_then(crate::domain::address_digits)
                else {
                    return false;
                };
                let cached = self
                    .tracker_snapshot
                    .get(receiver)
                    .into_iter()
                    .flatten()
                    .filter(|cached| cached.slot == live.slot)
                    .filter_map(|cached| cached.address.as_deref())
                    .chain(
                        self.targets
                            .iter()
                            .filter(|target| {
                                target.receiver.key == receiver && target.id == live.slot
                            })
                            .filter_map(|target| target.address.as_deref()),
                    );
                cached
                    .filter_map(crate::domain::address_digits)
                    .any(|previous| previous != address)
            })
    }

    pub(in crate::application) fn revoke_receiver_bindings(&mut self, receiver: &str) {
        if self.querying_receiver(receiver) {
            self.invalidated_query_receivers.insert(receiver.to_owned());
        }
        self.tracker_snapshot.remove(receiver);
        if let Some(known) = self
            .esb_auto_query
            .receivers
            .iter_mut()
            .find(|known| known.endpoint.key == receiver)
        {
            known.trackers.clear();
        }
        self.targets.retain(|target| {
            let revoked = target.receiver.key == receiver && target.id != esb_ota::RECEIVER_ID;
            if revoked {
                self.selected_targets.remove(&ota_target_key(target));
            }
            !revoked
        });
        self.tracker_selection.retain(|key| {
            key.strip_prefix("tracker:")
                .and_then(|key| key.rsplit_once(':'))
                .is_none_or(|(owner, _)| owner != receiver)
        });
        self.query_failures.retain(|key, _| {
            key.rsplit_once(':').is_none_or(|(owner, id)| {
                owner != receiver || id.parse::<u8>() == Ok(esb_ota::RECEIVER_ID)
            })
        });
        if matches!(&self.review, Some(Action::EsbGroups(groups)) if groups.iter().any(|group| {
            group.targets.iter().any(|target| {
                target.receiver.key == receiver && target.id != esb_ota::RECEIVER_ID
            })
        })) {
            self.review = None;
            self.acknowledged = false;
        }
        self.reconcile_groups(None);
    }

    pub(super) fn unresolved_tracker_selection(&self) -> bool {
        self.tracker_selection.iter().any(|key| {
            !self.targets.iter().any(|target| {
                target.id != esb_ota::RECEIVER_ID
                    && *key == format!("tracker:{}:{}", target.receiver.key, target.id)
                    && self.selected_targets.contains(&ota_target_key(target))
            })
        })
    }

    pub(super) fn apply_volumes(&mut self, volumes: Vec<uf2::Uf2Volume>) {
        self.selected_volumes
            .retain(|identity| volumes.iter().any(|volume| &volume.identity == identity));
        let stale_review = match &self.review {
            Some(Action::Uf2(frozen, _, _) | Action::Reboot(frozen)) => frozen.iter().any(|old| {
                !volumes
                    .iter()
                    .any(|live| live.identity == old.identity && live.mount == old.mount)
            }),
            _ => false,
        };
        if stale_review {
            self.review = None;
            self.acknowledged = false;
            self.auto_requested = false;
        }
        self.volumes = volumes;
    }

    pub(super) fn selected_live_volumes(&self) -> Vec<uf2::Uf2Volume> {
        self.volumes
            .iter()
            .filter(|volume| self.selected_volumes.contains(&volume.identity))
            .cloned()
            .collect()
    }

    pub(super) fn sync_receiver_identities(&mut self, receivers: &[ReceiverEndpoint]) -> bool {
        let before = self.targets.len();
        self.targets.retain(|target| {
            let replaced = receivers.iter().any(|receiver| {
                receiver.key == target.receiver.key
                    && (receiver.path != target.receiver.path
                        || receiver.serial_number != target.receiver.serial_number)
            });
            if replaced {
                self.tracker_snapshot.remove(&target.receiver.key);
                self.selected_targets.remove(&ota_target_key(target));
                let group = ota_assignment_key(target);
                self.assignments.remove(&group);
                self.group_generations.remove(&group);
            }
            !replaced
        });
        if before != self.targets.len() {
            self.review = None;
            self.acknowledged = false;
            self.reconcile_groups(None);
            true
        } else {
            false
        }
    }

    pub(super) fn sync_tracker_selection<'a>(
        &mut self,
        selected: &BTreeSet<String>,
        trackers: impl Iterator<Item = &'a TrackerLink> + Clone,
    ) -> bool {
        let selection_changed = self
            .tracker_selection
            .iter()
            .ne(selected.iter().filter(|key| key.starts_with("tracker:")));
        let preferred = if selection_changed {
            self.targets
                .iter()
                .find(|target| {
                    let key = format!("tracker:{}:{}", target.receiver.key, target.id);
                    target.id != esb_ota::RECEIVER_ID
                        && selected.contains(&key)
                        && !self.tracker_selection.contains(&key)
                })
                .map(ota_assignment_key)
        } else {
            None
        };
        if selection_changed {
            self.tracker_selection = selected
                .iter()
                .filter(|key| key.starts_with("tracker:"))
                .cloned()
                .collect();
        }
        let before = self.targets.len();
        self.targets.retain(|target| {
            // Reservation/restart temporarily empties the passive monitor. Only
            // a positively observed replacement address invalidates FW_INFO.
            let replaced = target.id != esb_ota::RECEIVER_ID
                && trackers.clone().any(|tracker| {
                    tracker.receiver_key == target.receiver.key
                        && tracker.slot == target.id
                        && tracker
                            .address
                            .as_deref()
                            .and_then(crate::domain::normalize_address)
                            .is_some_and(|address| {
                                Some(address)
                                    != target
                                        .address
                                        .as_deref()
                                        .and_then(crate::domain::normalize_address)
                            })
                });
            if replaced {
                let group = ota_assignment_key(target);
                self.assignments.remove(&group);
                self.group_generations.remove(&group);
            }
            !replaced
        });
        let mut changed = selection_changed || before != self.targets.len();
        for target in &self.targets {
            if target.id == esb_ota::RECEIVER_ID {
                continue;
            }
            let key = ota_target_key(target);
            // Selection describes the user's update plan, not current traffic.
            // Mere liveness expiry must not discard its group, image or focus.
            // Fresh route validation still gates both review and execution.
            let eligible = selected
                .contains(&format!("tracker:{}:{}", target.receiver.key, target.id))
                && ota_supported(target);
            changed |= if eligible {
                self.selected_targets.insert(key)
            } else {
                self.selected_targets.remove(&key)
            };
        }
        self.selected_targets.retain(|key| {
            self.targets
                .iter()
                .any(|target| ota_target_key(target) == *key)
        });
        changed |= matches!(&self.review, Some(Action::EsbGroups(groups)) if
            groups.iter().flat_map(|group| &group.targets).any(|target| {
                target.id != esb_ota::RECEIVER_ID && !trackers.clone().any(|tracker| {
                    tracker.receiver_key == target.receiver.key
                        && tracker.slot == target.id
                        && query_tracker_eligible(tracker)
                })
            })
        );
        if changed {
            self.review = None;
            self.acknowledged = false;
            self.reconcile_groups(preferred);
        }
        changed
    }

    pub(super) fn set_receiver_update_selected(&mut self, key: &str, checked: bool) {
        if self.busy() {
            return;
        }
        let Some(target) = self.targets.iter().find(|target| {
            ota_target_key(target) == key
                && target.id == esb_ota::RECEIVER_ID
                && ota_supported(target)
        }) else {
            return;
        };
        let group = checked.then(|| ota_assignment_key(target));
        if checked {
            self.selected_targets.insert(key.to_owned());
        } else {
            self.selected_targets.remove(key);
        }
        self.review = None;
        self.acknowledged = false;
        self.reconcile_groups(group);
    }

    pub(super) fn ota_wired_update_mode(&self, key: &str) -> Option<Mode> {
        if self.busy() {
            return None;
        }
        self.targets
            .iter()
            .find(|target| ota_target_key(target) == key)
            .and_then(ota_update_guidance)
            .and_then(OtaUpdateGuidance::wired_mode)
    }
}

impl Workbench {
    pub(in crate::application) fn sync_flash_tracker_selection(&mut self) -> bool {
        let identity_changed = self
            .flash
            .sync_receiver_identities(&self.inventory.receivers);
        for receiver in &self.inventory.receivers {
            let live = self
                .monitor
                .observation(&receiver.key)
                .map_or(&[][..], |observation| observation.trackers.as_slice());
            self.flash
                .esb_auto_query
                .observe(receiver, live, Instant::now());
            // The exclusive query lease makes live data unknown, not offline.
            // Preserve manual-refresh display geometry; authorization and selection
            // reconciliation below still consume only current monitor evidence.
            if !live.is_empty() || !self.flash.preserves_query_layout(&receiver.key) {
                merge_tracker_snapshot(
                    self.flash
                        .tracker_snapshot
                        .entry(receiver.key.clone())
                        .or_default(),
                    live,
                );
            }
        }
        let trackers = self
            .inventory
            .receivers
            .iter()
            .filter_map(|receiver| self.monitor.observation(&receiver.key))
            .flat_map(|observation| observation.trackers.iter());
        self.flash.sync_tracker_selection(&self.selected, trackers) || identity_changed
    }

    pub(super) fn toggle_flash_tracker(&mut self, key: &str) {
        if self.flash.busy() {
            return;
        }
        if !self.selected.remove(key) {
            if !self.visible_trackers().any(|tracker| {
                tracker_selection_key(tracker) == key && tracker.online && tracker.address.is_some()
            }) {
                return;
            }
            self.selected.insert(key.to_owned());
        }
        self.selection_changed();
    }
}
