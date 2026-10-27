use super::transcript::{drain_display, dropped_notice};
use super::{
    ConnectionState, Lifecycle, LossProbe, Reconnect, SessionChanges, SessionIdentityObservation,
    SessionManager, ThermalDumpOutcome, port_key,
};
use crate::i18n::tr;
use parking_lot::Mutex;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::thread::{self, JoinHandle};
use std::time::Instant;

impl SessionManager {
    pub fn poll(&mut self) -> SessionChanges {
        self.poll_at(Instant::now())
    }

    pub(super) fn poll_at(&mut self, now: Instant) -> SessionChanges {
        let mut changed = SessionChanges::default();
        let inventory_fresh = self.inventory_is_fresh(now);
        let inventory_scan_started_at = self.inventory_scan_started_at;
        for (key, entry) in &mut self.entries {
            let Some(worker) = entry.worker.as_mut() else {
                continue;
            };
            if worker.generation != entry.view.generation {
                self.thermal_dumps.lock().finish_generation(
                    key,
                    worker.generation,
                    ThermalDumpOutcome::GenerationReplaced,
                );
                entry.reconnect = None;
                entry.opening_identity = None;
                worker.cancel.store(true, Ordering::Release);
                if worker.handle.as_ref().is_some_and(JoinHandle::is_finished) {
                    let _ = worker.handle.take().unwrap().join();
                    entry.worker = None;
                    changed.state = true;
                }
                continue;
            }
            // Decide whether to reap before draining lifecycle events. A worker
            // finishing during this poll is left for the next poll, avoiding a
            // race between observing an empty event queue and thread exit.
            let joined = if worker.handle.as_ref().is_some_and(JoinHandle::is_finished) {
                Some(worker.handle.take().unwrap().join())
            } else {
                None
            };
            // Bound work per poll even under continuous incoming traffic.
            let display_revision = entry.view.terminal.revision();
            drain_display(worker, &mut entry.view);
            while let Ok(event) = worker.lifecycle.try_recv() {
                match event {
                    Lifecycle::Opened => {
                        if entry.view.state == ConnectionState::Connecting
                            && !worker.cancel.load(Ordering::Acquire)
                        {
                            let current_identity = self
                                .inventory
                                .get(key)
                                .filter(|_| inventory_fresh)
                                .and_then(|record| record.recovery_identity.as_ref());
                            if entry
                                .opening_identity
                                .as_ref()
                                .is_some_and(|opening| current_identity != Some(opening))
                                || entry
                                    .reconnect
                                    .as_ref()
                                    .is_some_and(|retry| current_identity != Some(&retry.identity))
                                || self
                                    .inventory
                                    .get(key)
                                    .is_some_and(|record| !record.console_allowed)
                            {
                                // Inventory may change while a blocking open is
                                // in flight. Never publish that generation.
                                worker.cancel.store(true, Ordering::Release);
                                entry.view.state = ConnectionState::Closing;
                            } else {
                                entry.view.state = ConnectionState::Connected;
                                entry.view.last_error = None;
                                worker.opened = true;
                                if let Some(identity) = entry.opening_identity.take() {
                                    if entry.reconnect.is_none() {
                                        entry.reconnect = Some(Reconnect {
                                            identity: identity.clone(),
                                            arrival_pending: false,
                                            loss_probe: LossProbe::Available,
                                            native_change_after: None,
                                        });
                                    }
                                    entry.captured_usb_identity = Some(identity);
                                }
                            }
                            changed.state = true;
                        }
                    }
                    Lifecycle::Closed { error, dropped } => {
                        self.thermal_dumps.lock().finish_generation(
                            key,
                            worker.generation,
                            ThermalDumpOutcome::Disconnected,
                        );
                        // Closed is sent after transport destruction, so all
                        // queued bytes now fit in a single bounded drain.
                        drain_display(worker, &mut entry.view);
                        entry.view.terminal.finish();
                        if dropped != 0 {
                            dropped_notice(&mut entry.view.terminal, dropped);
                        }
                        if worker.opened
                            && !worker.cancel.load(Ordering::Acquire)
                            && let Some(reconnect) = &mut entry.reconnect
                        {
                            changed.refresh_inventory |= reconnect.note_loss(now);
                        }
                        if worker.cancel.load(Ordering::Acquire) {
                            entry.view.state = ConnectionState::Disconnected;
                        } else if let Some(error) = error {
                            entry.view.last_error = Some(error);
                            entry.view.state = ConnectionState::Faulted;
                        } else {
                            entry.view.state = ConnectionState::Disconnected;
                        }
                        worker.closed = true;
                        changed.state = true;
                    }
                }
            }
            if let Some(snapshot) = worker.identity.try_lock()
                && worker.opened
                && snapshot.revision != worker.identity_revision
            {
                worker.identity_revision = snapshot.revision;
                let address_changed = entry.view.identity.address != snapshot.identity.address;
                entry.view.identity = snapshot.identity.clone();
                changed.state = true;
                if address_changed
                    && let Some(observation) = SessionIdentityObservation::from_generation(
                        &entry.view,
                        entry.captured_usb_identity.as_ref(),
                    )
                {
                    changed.identities.push(observation);
                }
            }
            if let Some(result) = joined {
                self.thermal_dumps.lock().finish_generation(
                    key,
                    worker.generation,
                    ThermalDumpOutcome::Disconnected,
                );
                if result.is_err() || !worker.closed {
                    drain_display(worker, &mut entry.view);
                    entry.view.terminal.finish();
                    entry.view.state = ConnectionState::Faulted;
                    entry.view.last_error = Some(tr("serial.session.error.unexpected_stop"));
                    if worker.opened
                        && !worker.cancel.load(Ordering::Acquire)
                        && let Some(reconnect) = &mut entry.reconnect
                    {
                        changed.refresh_inventory |= reconnect.note_loss(now);
                    }
                }
                entry.worker = None;
                changed.state = true;
            }
            // Include final UTF-8 replacement and loss notices emitted on close.
            changed.display |= display_revision != entry.view.terminal.revision();
        }
        if !self.shutting_down && inventory_fresh {
            let recoveries: Vec<_> = self
                .entries
                .iter()
                .filter_map(|(key, entry)| {
                    let retry = entry.reconnect.as_ref()?;
                    (entry.worker.is_none()
                        && !entry.close_requested
                        && inventory_scan_started_at.is_some_and(|start| retry.eligible(start))
                        && !self.reservations.contains_key(key)
                        && self
                            .inventory
                            .get(key)
                            .and_then(|record| record.recovery_identity.as_ref())
                            == Some(&retry.identity))
                    .then(|| entry.view.port.clone())
                })
                .collect();
            for port in recoveries {
                // Every source coalesces into this attempt, including an unused
                // loss fallback. Successful automatic admission cannot refill it.
                self.entries
                    .get_mut(&port_key(&port))
                    .unwrap()
                    .reconnect
                    .as_mut()
                    .unwrap()
                    .consume();
                // The previous worker has been joined. New channels, identity
                // and generation cannot replay commands or revive old pins.
                if let Err(error) = self.open_connection(&port, true, now) {
                    let entry = self.entries.get_mut(&port_key(&port)).unwrap();
                    entry.view.last_error = Some(error);
                    // Generation exhaustion is terminal, not a busy retry loop.
                    if self.next_generation == u64::MAX {
                        entry.reconnect = None;
                    }
                }
                changed.state = true;
            }
        }
        changed
    }

    /// Signal cancellation without blocking the caller. The returned reaper can
    /// be joined on a background executor at application quit to await cleanup.
    pub fn shutdown(&mut self) -> Option<JoinHandle<()>> {
        self.shutting_down = true;
        self.reservations.clear();
        for request in self.active_thermal_dumps() {
            self.thermal_dumps
                .lock()
                .finish(&request, ThermalDumpOutcome::Disconnected);
        }
        let mut handles = Vec::new();
        for entry in self.entries.values_mut() {
            entry.reconnect = None;
            entry.opening_identity = None;
            if let Some(mut worker) = entry.worker.take() {
                worker.cancel.store(true, Ordering::Release);
                if let Some(handle) = worker.handle.take() {
                    handles.push(handle);
                }
                entry.view.state = ConnectionState::Disconnected;
            }
        }
        if handles.is_empty() {
            return None;
        }
        reap_workers("serial-shutdown", handles)
    }
}

impl Drop for SessionManager {
    fn drop(&mut self) {
        if let Some(reaper) = self.shutdown() {
            let _ = reaper.join();
        }
    }
}

/// A failed reaper spawn must not silently detach live resource owners.
pub(crate) fn reap_workers(name: &str, handles: Vec<JoinHandle<()>>) -> Option<JoinHandle<()>> {
    let pending = Arc::new(Mutex::new(Some(handles)));
    let owned = Arc::clone(&pending);
    match thread::Builder::new().name(name.into()).spawn(move || {
        if let Some(handles) = owned.lock().take() {
            for handle in handles {
                let _ = handle.join();
            }
        }
    }) {
        Ok(reaper) => Some(reaper),
        Err(_) => {
            if let Some(handles) = pending.lock().take() {
                for handle in handles {
                    let _ = handle.join();
                }
            }
            None
        }
    }
}
