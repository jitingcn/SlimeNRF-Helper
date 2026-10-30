use super::devices::revoke_receiver_tracker_selection;
use super::{FeaturePage, Workbench};
use crate::{
    discovery,
    domain::{DeviceRole, Inventory, receiver_port},
    sessions::SessionManager,
};
use gpui_kit::Context;
use std::time::Instant;

pub(super) struct SerialPoll {
    pub(super) redraw: bool,
    pub(super) state_changed: bool,
}

/// Borrow the displayed terminal revision without allocating a canonical port key.
fn serial_visible_revision(
    manager: &SessionManager,
    terminal_visible: bool,
    active_port: Option<&str>,
) -> Option<(u64, u64)> {
    if !terminal_visible {
        return None;
    }
    let port = active_port?;
    manager
        .sessions()
        .find(|session| session.port == port)
        .map(|session| (session.generation, session.terminal.revision()))
}

fn serial_poll_result(
    changes: &crate::sessions::SessionChanges,
    before: Option<(u64, u64)>,
    after: Option<(u64, u64)>,
) -> SerialPoll {
    SerialPoll {
        redraw: changes.state || (changes.display && before != after),
        state_changed: changes.state,
    }
}

fn inventory_display_changed(before: &Inventory, after: &Inventory) -> bool {
    before.serial_enumeration_failed != after.serial_enumeration_failed
        || before.warnings != after.warnings
        || (!after.serial_enumeration_failed && before.ports.len() != after.ports.len())
        || before.receivers.len() != after.receivers.len()
        || (!after.serial_enumeration_failed
            && before.ports.iter().zip(&after.ports).any(|(a, b)| {
                a.port_name != b.port_name
                    || a.label != b.label
                    || a.manufacturer != b.manufacturer
                    || a.product != b.product
                    || a.kind != b.kind
                    || a.serial_number != b.serial_number
                    || a.vid != b.vid
                    || a.pid != b.pid
                    || a.usb_interface != b.usb_interface
            }))
        || before.receivers.iter().zip(&after.receivers).any(|(a, b)| {
            a.key != b.key
                || a.path != b.path
                || a.label != b.label
                || a.serial_number != b.serial_number
        })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum InventoryScanIntent {
    Scheduled,
    Notify,
    Rediscover,
}

impl InventoryScanIntent {
    fn rediscover_bindings(
        self,
        monitor: &mut discovery::ReceiverMonitor,
        previous_generation: u64,
    ) -> bool {
        if self != Self::Rediscover || previous_generation == 0 {
            return false;
        }
        monitor.rediscover_bindings();
        true
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum InventoryScanTrigger {
    Scheduled,
    NativeChange,
}

pub(super) fn inventory_scan_trigger(
    scanning: bool,
    next_enumeration: Instant,
    now: Instant,
    take_native_change: impl FnOnce() -> bool,
) -> Option<InventoryScanTrigger> {
    if scanning {
        return None;
    }
    let native_change = take_native_change();
    if native_change {
        Some(InventoryScanTrigger::NativeChange)
    } else if now >= next_enumeration {
        Some(InventoryScanTrigger::Scheduled)
    } else {
        None
    }
}

impl Workbench {
    pub(super) fn poll_sessions(&mut self) -> SerialPoll {
        let terminal_visible = self.active_page == FeaturePage::DeviceSetup;
        let before =
            serial_visible_revision(&self.manager, terminal_visible, self.tabs.active.as_deref());
        let changes = self.manager.poll();
        self.observe_flash_identities(&changes);
        if changes.refresh_inventory {
            self.next_enumeration = self.next_enumeration.min(Instant::now());
        }
        let binding_changed = self.poll_receiver_binding_changes();
        let mut polled = serial_poll_result(
            &changes,
            before,
            serial_visible_revision(&self.manager, terminal_visible, self.tabs.active.as_deref()),
        );
        polled.redraw |= binding_changed;
        polled.state_changed |= binding_changed;
        polled
    }

    pub(super) fn scan_inventory(&mut self, cx: &mut Context<Self>) {
        self.start_inventory_scan(cx, InventoryScanIntent::Notify);
    }

    pub(super) fn rediscover_inventory(&mut self, cx: &mut Context<Self>) {
        self.start_inventory_scan(cx, InventoryScanIntent::Rediscover);
    }

    pub(super) fn start_inventory_scan(
        &mut self,
        cx: &mut Context<Self>,
        intent: InventoryScanIntent,
    ) {
        if self.shutting_down || self.scanning_inventory {
            return;
        }
        if intent.rediscover_bindings(&mut self.monitor, self.inventory_generation) {
            self.poll_receiver_binding_changes();
        }
        self.inventory_generation += 1;
        let generation = self.inventory_generation;
        let notify_scan = intent != InventoryScanIntent::Scheduled
            || generation == 1
            || matches!(self.active_page, FeaturePage::Uf2 | FeaturePage::Ota);
        self.scanning_inventory = true;
        let scan_started_at = Instant::now();
        let scan = cx.background_executor().spawn(async { discovery::scan() });
        self.inventory_task = Some(cx.spawn(async move |this, cx| {
            let result = scan.await;
            let _ = this.update(cx, |this, cx| {
                let readiness = this.home_readiness();
                let Some(changed) =
                    this.apply_inventory_result(generation, scan_started_at, result)
                else {
                    return;
                };
                let redraw = notify_scan || changed;
                this.sync_registry_observations(cx);
                if redraw || readiness != this.home_readiness() {
                    cx.notify();
                }
            });
        }));
        if notify_scan {
            cx.notify();
        }
    }

    pub(super) fn apply_inventory_result(
        &mut self,
        generation: u64,
        scan_started_at: Instant,
        result: Result<Inventory, String>,
    ) -> Option<bool> {
        if self.shutting_down || self.inventory_generation != generation {
            return None;
        }
        self.scanning_inventory = false;
        let mut redraw = false;
        match result {
            Ok(mut inventory) => {
                redraw |= self
                    .flash
                    .reconcile_receiver_result_presence(&inventory, scan_started_at);
                if inventory.serial_enumeration_failed {
                    self.manager.invalidate_inventory();
                    inventory.ports = std::mem::take(&mut self.inventory.ports);
                } else {
                    self.manager.update_inventory(&inventory, scan_started_at);
                }
                redraw |= self.inventory_error.is_some()
                    || inventory_display_changed(&self.inventory, &inventory);
                self.inventory_error = None;
                self.monitor.reconcile(&inventory.receivers);
                self.inventory = inventory;
                self.registry_inventory_arrived();
            }
            Err(error) => {
                // Retain the roster for display, never stale USB retry eligibility.
                self.manager.invalidate_inventory();
                let error = rust_i18n::t!("devices.discovery.error.roster_retained", error = error)
                    .to_string();
                redraw |= self.inventory_error.as_deref() != Some(error.as_str());
                self.inventory_error = Some(error.into());
            }
        }
        Some(redraw)
    }

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
}

#[cfg(test)]
mod tests;

impl Workbench {
    pub(super) fn home_readiness(&self) -> crate::features::home::readiness::Readiness {
        use crate::features::home::readiness::{ReceiverState, project};
        project(
            &self.inventory,
            self.inventory_error.as_deref(),
            self.inventory_generation,
            self.scanning_inventory,
            self.inventory.receivers.iter().map(|receiver| {
                ReceiverState::new(
                    receiver,
                    self.monitor.is_reserved(receiver),
                    self.monitor.observation(&receiver.key),
                )
            }),
        )
    }
}
