//! Application-owned capture admission, identity pins, and exclusive device leases.
//! The UI freezes a preparation, checks firmware conflicts, then admits it here.
use crate::discovery::{ReceiverMonitor, ReceiverReservation};
use crate::domain::{DeviceIdentity, Inventory, ReceiverEndpoint, SerialEndpoint};
use crate::features::capture::device::{
    CaptureConsolePreparation, CaptureMode, CaptureOutcome, CaptureRequest, CaptureSnapshot,
    CaptureSource, CaptureWorker,
};
use crate::i18n::tr;
use crate::serial_port::same_port_name;
use crate::sessions::{ConnectionState, SerialReservation, SessionManager};
use std::{cell::RefCell, collections::BTreeSet, path::PathBuf, sync::Arc};

type UsbIdentityBinding = (Option<u16>, Option<u16>, String);

struct CapturePin {
    receiver: ReceiverEndpoint,
    addresses: BTreeSet<String>,
    ports: BTreeSet<String>,
    // Monotonic, positive bindings survive serial identity reset on reconnect.
    learned_ports: RefCell<BTreeSet<String>>,
    learned_usb: RefCell<Vec<UsbIdentityBinding>>,
}

/// Frozen identities and request, without leases or a running worker. This lets
/// the caller finish cross-feature admission checks before reserving devices.
pub(crate) struct CapturePreparation {
    request: CaptureRequest,
    pin: CapturePin,
}

impl CapturePreparation {
    pub(crate) fn freeze(
        source: CaptureSource,
        mode: CaptureMode,
        destination: PathBuf,
        inventory: &Inventory,
        manager: &SessionManager,
        monitor: &ReceiverMonitor,
    ) -> Result<Self, String> {
        let roster: Vec<(u8, String)> = monitor
            .observation(&source.receiver.key)
            .into_iter()
            .flat_map(|o| &o.trackers)
            .filter_map(|t| {
                t.address
                    .as_deref()
                    .and_then(crate::domain::normalize_address)
                    .map(|address| (t.slot, address))
            })
            .collect();
        if roster.is_empty() {
            return Err(tr("capture.error.wait_for_identities"));
        }
        if let CaptureMode::Single { slot, rf_address } = &mode
            && !roster.iter().any(|(s, a)| s == slot && a == rf_address)
        {
            return Err(tr("capture.error.device_changed"));
        }
        let mut pin = CapturePin {
            receiver: source.receiver.clone(),
            addresses: roster.iter().map(|(_, a)| a.clone()).collect(),
            ports: BTreeSet::new(),
            learned_ports: RefCell::default(),
            learned_usb: RefCell::default(),
        };
        if let Some(port) = source.serial_port() {
            pin.ports.insert(port.to_owned());
        }
        if let Some(console) = source.console() {
            pin.ports.insert(console.port_name.clone());
        }
        for port in &inventory.ports {
            if pin_blocks_port(
                &pin,
                &port.port_name,
                manager.session(&port.port_name).map(|s| &s.identity),
                Some(port),
            ) {
                pin.ports.insert(port.port_name.clone());
            }
        }
        for session in manager.sessions() {
            if pin_blocks_port(&pin, &session.port, Some(&session.identity), None) {
                pin.ports.insert(session.port.clone());
            }
        }
        Ok(Self {
            request: CaptureRequest {
                source,
                mode,
                destination,
                roster,
            },
            pin,
        })
    }

    pub(crate) fn ports(&self) -> impl Iterator<Item = &str> {
        self.pin.ports.iter().map(String::as_str)
    }
}

#[derive(Default)]
pub(crate) struct CaptureSession {
    pending: Option<CaptureRequest>,
    worker: Option<CaptureWorker>,
    hid_lease: Option<ReceiverReservation>,
    serial_leases: Vec<SerialReservation>,
    console_lease: Option<SerialReservation>,
    pin: Option<CapturePin>,
    final_snapshot: Option<Arc<CaptureSnapshot>>,
    completion: Option<Result<CaptureOutcome, String>>,
    error: Option<String>,
}

enum CaptureHandoff {
    RawHid,
    Console(CaptureConsolePreparation),
}

impl CaptureSession {
    pub(crate) fn busy(&self) -> bool {
        self.pin.is_some()
    }

    pub(crate) fn pending(&self) -> bool {
        self.pending.is_some()
    }

    pub(crate) fn running(&self) -> bool {
        self.worker.is_some()
    }

    pub(crate) fn receiver(&self) -> Option<&ReceiverEndpoint> {
        self.pin.as_ref().map(|pin| &pin.receiver)
    }

    pub(crate) fn blocks_address(&self, address: &str) -> bool {
        self.pin
            .as_ref()
            .is_some_and(|pin| pin.addresses.contains(address))
    }

    pub(crate) fn blocks_port(
        &self,
        port: &str,
        identity: Option<&DeviceIdentity>,
        endpoint: Option<&SerialEndpoint>,
    ) -> bool {
        self.pin
            .as_ref()
            .is_some_and(|pin| pin_blocks_port(pin, port, identity, endpoint))
    }

    pub(crate) fn prepare(
        &mut self,
        preparation: CapturePreparation,
        monitor: &mut ReceiverMonitor,
    ) -> Result<(), String> {
        if self.busy() {
            return Err(tr("capture.error.receiver_busy"));
        }
        // Freeze precedes reserve(), which clears the passive roster. Installing
        // the pin gates new commands before accepted writes are drained in poll().
        self.hid_lease = Some(monitor.reserve(&preparation.pin.receiver)?);
        self.pin = Some(preparation.pin);
        self.pending = Some(preparation.request);
        self.final_snapshot = None;
        self.completion = None;
        self.error = None;
        Ok(())
    }

    pub(crate) fn snapshot(&self) -> Option<Arc<CaptureSnapshot>> {
        self.worker
            .as_ref()
            .map(CaptureWorker::snapshot)
            .or_else(|| self.final_snapshot.clone())
    }

    pub(crate) fn take_completion(&mut self) -> Option<Result<CaptureOutcome, String>> {
        self.completion.take()
    }

    pub(crate) fn take_error(&mut self) -> Option<String> {
        self.error.take()
    }

    fn stop_worker(&self) -> bool {
        if let Some(worker) = &self.worker {
            worker.stop();
            true
        } else {
            false
        }
    }

    fn join_worker(&mut self) -> Option<Result<CaptureOutcome, String>> {
        let worker = self.worker.as_mut()?;
        let result = worker.join()?;
        // Read after join establishes the final publication boundary.
        self.final_snapshot = Some(worker.snapshot());
        self.worker = None;
        Some(result)
    }

    fn release_leases(&mut self, manager: &mut SessionManager, monitor: &mut ReceiverMonitor) {
        // Cancellation alone never releases a live worker's leases.
        if self.worker.is_some() {
            return;
        }
        if let Some(token) = self.console_lease.take()
            && let Err(error) = manager.release(&token)
        {
            self.error = Some(error);
        }
        for token in self.serial_leases.drain(..) {
            if let Err(error) = manager.release(&token) {
                self.error = Some(error);
            }
        }
        if let Some(token) = self.hid_lease.take()
            && let Err(error) = monitor.release(&token)
        {
            self.error = Some(error);
        }
        self.pin = None;
    }

    pub(crate) fn stop(&mut self, manager: &mut SessionManager, monitor: &mut ReceiverMonitor) {
        if !self.stop_worker() && self.pending.take().is_some() {
            self.release_leases(manager, monitor);
        }
    }

    /// Advance handoff and completion regardless of which application page is visible.
    pub(crate) fn poll(
        &mut self,
        inventory: &Inventory,
        manager: &mut SessionManager,
        monitor: &mut ReceiverMonitor,
    ) -> bool {
        // Learn late positive identities before a reconnect can clear them.
        if self.busy() {
            for session in manager.sessions() {
                self.blocks_port(
                    &session.port,
                    Some(&session.identity),
                    inventory
                        .ports
                        .iter()
                        .find(|p| same_port_name(&p.port_name, &session.port)),
                );
            }
        }
        let mut changed = self.pending.is_some();
        if self.pending.is_some() {
            let ready = (|| -> Result<Option<CaptureHandoff>, String> {
                if let Some(console) = self.pending.as_ref().and_then(|r| r.source.console()) {
                    if manager.is_close_requested(&console.port_name) {
                        return Err(format!(
                            "{} terminal is closing; finish closing it before starting capture",
                            console.port_name
                        ));
                    }
                    if manager.thermal_dump_needs_reconnect(&console.port_name) {
                        return Err(format!(
                            "{} has an interrupted temperature dump; disconnect and reconnect before starting capture",
                            console.port_name
                        ));
                    }
                }
                let Some(token) = &self.hid_lease else {
                    return Err(tr("capture.error.source_required"));
                };
                if !monitor.reservation_ready(token)? {
                    return Ok(None);
                }
                if manager.sessions().any(|session| {
                    self.blocks_port(
                        &session.port,
                        Some(&session.identity),
                        inventory
                            .ports
                            .iter()
                            .find(|p| same_port_name(&p.port_name, &session.port)),
                    ) && !manager.writes_idle(&session.port)
                }) {
                    return Ok(None);
                }
                let source = &self.pending.as_ref().expect("pending capture").source;
                if !source.matches_inventory(inventory) {
                    return Err("Capture source changed before console initialization".into());
                }
                // Gate submissions, drain accepted writes, THEN cancel the raw
                // terminal reader. Earlier reservation could truncate a write.
                if self.serial_leases.is_empty()
                    && let Some(port) = self.pending.as_ref().and_then(|r| r.source.serial_port())
                {
                    self.serial_leases.push(manager.reserve(port, None)?);
                }
                for token in &self.serial_leases {
                    if !manager.reservation_ready(token)? {
                        return Ok(None);
                    }
                }
                let Some(console) = source.console() else {
                    return Ok(Some(CaptureHandoff::RawHid));
                };
                if manager.thermal_dump_busy(&console.port_name) {
                    return Ok(None);
                }
                if self.console_lease.is_none() && manager.command_port_reserved(&console.port_name)
                {
                    return Ok(None);
                }
                if self.console_lease.is_none() {
                    match manager.session(&console.port_name).map(|view| view.state) {
                        Some(ConnectionState::Connected) => {
                            let probe = manager.begin_capture_console_probe(console)?;
                            return Ok(Some(CaptureHandoff::Console(
                                CaptureConsolePreparation::Existing(probe),
                            )));
                        }
                        Some(ConnectionState::Connecting | ConnectionState::Closing) => {
                            return Ok(None);
                        }
                        Some(ConnectionState::Disconnected | ConnectionState::Faulted)
                            if !manager.can_forget(&console.port_name) =>
                        {
                            return Err(format!(
                                "{} console is unavailable; reconnect it before starting capture",
                                console.port_name
                            ));
                        }
                        _ => {}
                    }
                    self.console_lease = Some(manager.reserve(&console.port_name, None)?);
                }
                let token = self.console_lease.as_ref().expect("console reservation");
                if !manager.reservation_ready(token)? {
                    return Ok(None);
                }
                let source = manager.reserved_console_source(token)?;
                Ok(Some(CaptureHandoff::Console(
                    CaptureConsolePreparation::Temporary(source),
                )))
            })();
            match ready {
                Ok(None) => return false,
                Ok(Some(handoff)) => {
                    let request = self.pending.take().expect("pending capture");
                    let console = match handoff {
                        CaptureHandoff::RawHid => None,
                        CaptureHandoff::Console(console) => Some(console),
                    };
                    match CaptureWorker::start(request, console) {
                        Ok(worker) => self.worker = Some(worker),
                        Err(error) => {
                            self.error = Some(error);
                            self.release_leases(manager, monitor);
                        }
                    }
                }
                Err(error) => {
                    self.pending = None;
                    self.error = Some(error);
                    self.release_leases(manager, monitor);
                }
            }
        }
        if self
            .worker
            .as_ref()
            .is_some_and(CaptureWorker::console_closed)
        {
            changed = true;
            let result = self
                .console_lease
                .take()
                .ok_or_else(|| "Console reservation is missing".to_owned())
                .and_then(|token| manager.release(&token));
            if let Err(error) = &result {
                self.error = Some(error.clone());
            }
            if let Some(worker) = &mut self.worker {
                worker.console_released(result);
            }
        }
        if let Some(result) = self.join_worker() {
            changed = true;
            self.completion = Some(result);
            self.release_leases(manager, monitor);
        }
        changed
    }

    pub(crate) fn take_shutdown_worker(&mut self) -> Option<CaptureWorker> {
        self.pending = None;
        self.stop_worker();
        // Keep leases/pin until application background cleanup joins the worker.
        // That shutdown path permanently shuts both managers down.
        self.worker.take()
    }
}

fn pin_blocks_port(
    pin: &CapturePin,
    port: &str,
    identity: Option<&DeviceIdentity>,
    endpoint: Option<&SerialEndpoint>,
) -> bool {
    let usb_matches = endpoint.is_some_and(|endpoint| {
        endpoint
            .serial_number
            .as_deref()
            .filter(|serial| !serial.is_empty())
            .is_some_and(|serial| {
                pin.learned_usb.borrow().iter().any(|(vid, pid, known)| {
                    *vid == endpoint.vid
                        && *pid == endpoint.pid
                        && known.eq_ignore_ascii_case(serial)
                })
            })
    });
    let positive_identity = identity
        .and_then(|i| i.address.as_deref())
        .and_then(crate::domain::normalize_address)
        .is_some_and(|address| pin.addresses.contains(&address))
        || pin
            .receiver
            .serial_number
            .as_deref()
            .filter(|s| !s.is_empty())
            .is_some_and(|serial| {
                endpoint
                    .and_then(|p| p.serial_number.as_deref())
                    .is_some_and(|other| serial.eq_ignore_ascii_case(other))
            });
    let related = positive_identity
        || usb_matches
        || pin.ports.iter().any(|p| same_port_name(p, port))
        || pin
            .learned_ports
            .borrow()
            .iter()
            .any(|p| same_port_name(p, port));
    if related {
        if !pin.ports.contains(port) && !pin.learned_ports.borrow().contains(port) {
            pin.learned_ports.borrow_mut().insert(port.to_owned());
        }
        if positive_identity
            && !usb_matches
            && let Some(endpoint) = endpoint
            && let Some(serial) = endpoint
                .serial_number
                .as_deref()
                .filter(|serial| !serial.is_empty())
        {
            pin.learned_usb
                .borrow_mut()
                .push((endpoint.vid, endpoint.pid, serial.to_owned()));
        }
    }
    related
}

#[cfg(test)]
mod tests;
