use super::transcript::ConsoleLines;
use super::worker::run_worker;
use super::{
    ConnectionState, DisplayQueue, Entry, INVENTORY_MAX_AGE, IdentitySnapshot, InventoryEntry,
    QUEUE_CAPACITY, SessionManager, SessionView, UsbIdentity, Worker, port_key,
};
use crate::domain::{DeviceIdentity, Inventory, console_port_allowed};
use crate::i18n::tr;
use crate::terminal::TerminalBuffer;
use parking_lot::Mutex;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Instant;

impl SessionManager {
    /// Apply a successful serial enumeration with its acquisition start time.
    /// An empty snapshot is removal; failures use `invalidate_inventory`.
    pub fn update_inventory(&mut self, inventory: &Inventory, scan_started_at: Instant) {
        self.update_inventory_at(inventory, scan_started_at, Instant::now());
    }

    /// Reject stale authorization without inventing a removal or arrival.
    /// The last successful snapshot remains the presence-transition baseline.
    pub fn invalidate_inventory(&mut self) {
        self.inventory_updated_at = None;
        self.inventory_scan_started_at = None;
    }

    pub(super) fn update_inventory_at(
        &mut self,
        snapshot: &Inventory,
        scan_started_at: Instant,
        now: Instant,
    ) {
        if snapshot.serial_enumeration_failed {
            self.invalidate_inventory();
            return;
        }
        self.inventory_updated_at = Some(now);
        self.inventory_scan_started_at = Some(scan_started_at);
        let mut groups = BTreeMap::<_, (usize, BTreeSet<u8>, bool)>::new();
        for endpoint in &snapshot.ports {
            if let (Some(serial), Some(vid), Some(pid)) = (
                endpoint.serial_number.as_deref(),
                endpoint.vid,
                endpoint.pid,
            ) {
                let group = groups
                    .entry((serial, vid, pid))
                    .or_insert((0, BTreeSet::new(), true));
                group.0 += 1;
                group.2 &= endpoint
                    .usb_interface
                    .as_ref()
                    .and_then(|interface| interface.number)
                    .is_some_and(|number| group.1.insert(number));
            }
        }
        let mut inventory = BTreeMap::<String, InventoryEntry>::new();
        for endpoint in &snapshot.ports {
            let console_allowed = console_port_allowed(endpoint, snapshot);
            let recovery_identity = UsbIdentity::from_endpoint(endpoint).filter(|identity| {
                console_allowed
                    && groups
                        .get(&(identity.serial.as_str(), identity.vid, identity.pid))
                        .is_some_and(|(count, _, distinct)| *count == 1 || *distinct)
            });
            inventory
                .entry(port_key(&endpoint.port_name))
                .and_modify(|existing| {
                    existing.recovery_identity = None;
                    existing.console_allowed = false;
                    existing.usb = false;
                })
                .or_insert(InventoryEntry {
                    recovery_identity,
                    console_allowed,
                    usb: endpoint.kind == crate::domain::SerialPortKind::Usb,
                });
        }
        for (key, entry) in &mut self.entries {
            let Some(reconnect) = &mut entry.reconnect else {
                continue;
            };
            let matches = inventory
                .get(key)
                .and_then(|record| record.recovery_identity.as_ref())
                == Some(&reconnect.identity);
            if !matches {
                reconnect.arrival_pending = false;
                // A transient removal/replacement during a blocking automatic
                // open must not disappear behind a later matching snapshot.
                if entry.view.state == ConnectionState::Connecting
                    && let Some(worker) = &entry.worker
                {
                    worker.cancel.store(true, Ordering::Release);
                    entry.view.state = ConnectionState::Closing;
                }
            } else if !self.inventory.contains_key(key) {
                reconnect.arrival_pending = true;
                // Only an absent canonical port is a removal. Resolving
                // incomplete/ambiguous facts or replacing a listed identity
                // must not manufacture another attempt.
            }
        }
        for (key, entry) in &mut self.entries {
            if entry.view.state == ConnectionState::Connecting
                && (inventory
                    .get(key)
                    .is_some_and(|record| !record.console_allowed)
                    || entry.opening_identity.as_ref().is_some_and(|opening| {
                        inventory
                            .get(key)
                            .and_then(|record| record.recovery_identity.as_ref())
                            != Some(opening)
                    }))
                && let Some(worker) = &entry.worker
            {
                worker.cancel.store(true, Ordering::Release);
                entry.view.state = ConnectionState::Closing;
            }
        }
        self.inventory = inventory;
    }

    pub(super) fn inventory_is_fresh(&self, now: Instant) -> bool {
        self.inventory_updated_at
            .is_some_and(|updated| now.saturating_duration_since(updated) <= INVENTORY_MAX_AGE)
    }

    /// A global COM hint funds a fresh check, not proof of device attachment.
    pub(crate) fn note_native_change(&mut self, consumed_at: Instant) {
        for entry in self.entries.values_mut() {
            if let Some(reconnect) = &mut entry.reconnect {
                reconnect.native_change_after = Some(consumed_at);
            }
        }
    }

    /// Armed waiting and in-flight recovery; a healthy connection is not waiting.
    pub fn is_reconnecting(&self, port: &str) -> bool {
        self.entries.get(&port_key(port)).is_some_and(|entry| {
            entry.reconnect.is_some() && entry.view.state != ConnectionState::Connected
        })
    }

    /// History is removable only after every owner of this generation is gone.
    pub fn can_forget(&self, port: &str) -> bool {
        let key = port_key(port);
        !self.shutting_down
            && !self.reservations.contains_key(&key)
            && self.entries.get(&key).is_some_and(|entry| {
                matches!(
                    entry.view.state,
                    ConnectionState::Disconnected | ConnectionState::Faulted
                ) && entry.worker.is_none()
                    && entry.reconnect.is_none()
                    && entry.pending_writes.load(Ordering::Acquire) == 0
            })
            && !self.thermal_dumps.lock().busy(port)
    }

    /// Explicitly discard a settled session and its transcript, never a live owner.
    pub fn forget(&mut self, port: &str) -> bool {
        if !self.can_forget(port) {
            return false;
        }
        self.entries.remove(&port_key(port)).is_some()
    }

    pub fn connect(&mut self, port: &str) -> Result<(), String> {
        if self.shutting_down {
            return Err(tr("serial.session.error.shutdown"));
        }
        if port.trim().is_empty() {
            return Err(tr("serial.session.error.empty_port"));
        }
        self.open_connection(port, false, Instant::now())
    }

    pub(super) fn open_connection(
        &mut self,
        port: &str,
        automatic: bool,
        now: Instant,
    ) -> Result<(), String> {
        let key = port_key(port);
        if self
            .entries
            .get(&key)
            .is_some_and(|entry| entry.close_requested)
        {
            return Err(format!("{port} terminal is closing"));
        }
        if self
            .inventory
            .get(&key)
            .is_some_and(|record| !record.console_allowed)
        {
            return Err(format!("{port} is not an available console interface"));
        }
        if self.reservations.contains_key(&key) {
            return Err(format!("{port} is reserved for a firmware operation"));
        }
        if self
            .entries
            .get(&key)
            .is_some_and(|entry| entry.worker.is_some())
        {
            return Err(
                rust_i18n::t!("serial.session.error.already_open", port = port).to_string(),
            );
        }
        self.next_generation = self
            .next_generation
            .checked_add(1)
            .ok_or_else(|| tr("serial.session.error.generation_exhausted"))?;
        let generation = self.next_generation;
        self.thermal_dumps
            .lock()
            .finish_port(port, super::ThermalDumpOutcome::GenerationReplaced);
        let cancel = Arc::new(AtomicBool::new(false));
        let identity = Arc::new(Mutex::new(IdentitySnapshot {
            identity: DeviceIdentity::default(),
            revision: 0,
            pairing_changed: false,
        }));
        let rssi_scan = Arc::new(Mutex::new(None));
        let worker_rssi_scan = Arc::clone(&rssi_scan);
        let (command_tx, command_rx) = mpsc::sync_channel(QUEUE_CAPACITY);
        let display = Arc::new(Mutex::new(DisplayQueue::new()));
        let worker_display = Arc::clone(&display);
        // This channel only carries Opened and Closed once, so lifecycle cannot
        // be lost or blocked by terminal backpressure.
        let (lifecycle_tx, lifecycle_rx) = mpsc::channel();
        let worker_cancel = Arc::clone(&cancel);
        let worker_identity = Arc::clone(&identity);
        let factory = Arc::clone(&self.factory);
        let worker_port = port.to_owned();
        let handle = thread::Builder::new()
            .name(format!("serial-{port}-{generation}"))
            .spawn(move || {
                run_worker(
                    &worker_port,
                    factory,
                    worker_cancel,
                    command_rx,
                    worker_display,
                    lifecycle_tx,
                    ConsoleLines::new(worker_identity, worker_rssi_scan),
                )
            });
        let (handle, spawn_error) = match handle {
            Ok(handle) => (Some(handle), None),
            Err(error) => (
                None,
                Some(
                    rust_i18n::t!("serial.session.error.connect", port = port, error = error)
                        .to_string(),
                ),
            ),
        };
        let worker = handle.map(|handle| Worker {
            generation,
            cancel,
            commands: command_tx,
            display,
            lifecycle: lifecycle_rx,
            identity,
            identity_revision: 0,
            rssi_scan,
            delivered_rssi_scan: None,
            handle: Some(handle),
            opened: false,
            closed: false,
        });
        let previous = self.entries.remove(&key);
        let (terminal, last_error, reconnect) = match previous {
            Some(entry) if automatic => {
                (entry.view.terminal, entry.view.last_error, entry.reconnect)
            }
            _ => (TerminalBuffer::new(), None, None),
        };
        let opening_identity = self
            .inventory
            .get(&key)
            .filter(|_| self.inventory_is_fresh(now))
            .and_then(|record| record.recovery_identity.clone());
        let view = SessionView {
            port: port.to_owned(),
            generation,
            state: if spawn_error.is_some() {
                ConnectionState::Faulted
            } else {
                ConnectionState::Connecting
            },
            last_error: spawn_error.clone().or(last_error),
            identity: DeviceIdentity::default(),
            terminal,
        };
        self.entries.insert(
            key,
            Entry {
                view,
                worker,
                close_requested: false,
                pending_writes: Arc::new(AtomicUsize::new(0)),
                captured_usb_identity: None,
                opening_identity,
                reconnect,
            },
        );
        match spawn_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
#[path = "tests/dismissal.rs"]
mod dismissal_tests;
