use super::{
    CalibrationTask, CalibrationWorker, Engine, ObservationEnd, RunMode, RunRequest, Snapshot,
    Target,
};
use crate::discovery::{ReceiverMonitor, ReceiverReservation};
use crate::domain::{
    DeviceIdentity, DeviceRole, Inventory, ReceiverEndpoint, SerialEndpoint, SerialPortKind,
    address_digits, console_port_allowed, normalize_address,
};
use crate::serial_port::same_port_name;
use crate::sessions::{ConnectionState, SerialReservation, SessionManager};
use std::{
    cell::RefCell,
    collections::BTreeSet,
    sync::Arc,
    time::{Duration, Instant},
};

#[derive(Clone, Debug)]
pub(crate) struct GuideSelection {
    pub target: Target,
    pub label: String,
}

pub(crate) struct ReviewTicket {
    target: Target,
    task: CalibrationTask,
    pin: Pin,
}
impl ReviewTicket {
    pub(crate) fn target(&self) -> &Target {
        &self.target
    }
    pub(crate) fn task(&self) -> CalibrationTask {
        self.task
    }
    pub(crate) fn address(&self) -> Option<&str> {
        self.pin.address.as_deref()
    }
    pub(crate) fn ports(&self) -> impl Iterator<Item = &str> {
        self.pin.ports.iter().map(String::as_str)
    }
    pub(crate) fn refresh_ports(
        &self,
        inventory: &Inventory,
        manager: &SessionManager,
        monitor: &ReceiverMonitor,
    ) -> Vec<String> {
        learn_aliases(&self.pin, inventory, manager, monitor);
        for endpoint in &inventory.ports {
            self.pin.blocks_port(
                &endpoint.port_name,
                manager
                    .session(&endpoint.port_name)
                    .map(|session| &session.identity),
                Some(endpoint),
            );
        }
        let aliases = self.pin.aliases.borrow();
        let mut ports = Vec::with_capacity(self.pin.ports.len() + aliases.ports.len());
        ports.extend(self.ports().map(str::to_owned));
        for port in &aliases.ports {
            if !ports.iter().any(|known| same_port_name(known, port)) {
                ports.push(port.clone());
            }
        }
        ports
    }
}

#[derive(Clone, Default)]
struct Aliases {
    ports: BTreeSet<String>,
    usb: BTreeSet<(Option<u16>, Option<u16>, String)>,
    receivers: BTreeSet<String>,
    receiver_serials: BTreeSet<String>,
}
#[derive(Clone)]
struct Pin {
    address: Option<String>,
    ports: BTreeSet<String>,
    aliases: RefCell<Aliases>,
}
impl Pin {
    fn blocks_port(
        &self,
        port: &str,
        identity: Option<&DeviceIdentity>,
        endpoint: Option<&SerialEndpoint>,
    ) -> bool {
        let positive = self.address.as_ref().is_some_and(|address| {
            identity
                .filter(|identity| identity.role == DeviceRole::Tracker)
                .and_then(|identity| identity.address.as_deref())
                .and_then(normalize_address)
                .as_ref()
                == Some(address)
        });
        let aliases = self.aliases.borrow();
        let usb_matches = endpoint.is_some_and(|endpoint| {
            endpoint
                .serial_number
                .as_deref()
                .filter(|serial| !serial.trim().is_empty())
                .is_some_and(|serial| {
                    aliases.usb.iter().any(|(vid, pid, known)| {
                        *vid == endpoint.vid
                            && *pid == endpoint.pid
                            && known.eq_ignore_ascii_case(serial.trim())
                    })
                })
        });
        let related = positive
            || self
                .ports
                .iter()
                .chain(&aliases.ports)
                .any(|known| same_port_name(known, port))
            || usb_matches;
        drop(aliases);
        if related {
            let mut aliases = self.aliases.borrow_mut();
            if !aliases.ports.contains(port) {
                aliases.ports.insert(port.to_owned());
            }
            if positive
                && !usb_matches
                && let Some(binding) = endpoint.and_then(usb_binding)
            {
                aliases.usb.insert(binding);
            }
        }
        related
    }
}
fn usb_binding(endpoint: &SerialEndpoint) -> Option<(Option<u16>, Option<u16>, String)> {
    let serial = endpoint.serial_number.as_deref()?.trim();
    if serial.is_empty() {
        return None;
    }
    Some((endpoint.vid, endpoint.pid, serial.to_ascii_uppercase()))
}

pub(crate) fn same_endpoint(a: &SerialEndpoint, b: &SerialEndpoint) -> bool {
    same_port_name(&a.port_name, &b.port_name)
        && a.kind == b.kind
        && a.vid == b.vid
        && a.pid == b.pid
        && a.serial_number == b.serial_number
        && a.same_usb_interface(b)
        && a.product == b.product
        && a.manufacturer == b.manufacturer
}
fn known_tracker(endpoint: &SerialEndpoint, manager: &SessionManager) -> bool {
    manager.session(&endpoint.port_name).is_some_and(|session| {
        session.state == ConnectionState::Connected
            && session.identity.role == DeviceRole::Tracker
            && session
                .identity
                .address
                .as_deref()
                .and_then(normalize_address)
                .is_some()
    }) || endpoint.product.as_deref().is_some_and(|product| {
        product
            .split(|c: char| !c.is_ascii_alphanumeric())
            .any(|word| word.eq_ignore_ascii_case("tracker"))
            && product
                .split(|c: char| !c.is_ascii_alphanumeric())
                .any(|word| {
                    word.eq_ignore_ascii_case("SlimeNRF") || word.eq_ignore_ascii_case("SlimeVR")
                })
    })
}
fn unique_usb(endpoint: &SerialEndpoint, inventory: &Inventory) -> bool {
    if endpoint.vid.is_none()
        || endpoint.pid.is_none()
        || inventory
            .ports
            .iter()
            .filter(|other| same_port_name(&other.port_name, &endpoint.port_name))
            .count()
            != 1
    {
        return false;
    }
    endpoint
        .serial_number
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .is_none_or(|serial| {
            inventory
                .ports
                .iter()
                .filter(|other| {
                    other.vid == endpoint.vid
                        && other.pid == endpoint.pid
                        && other
                            .serial_number
                            .as_deref()
                            .is_some_and(|value| value.eq_ignore_ascii_case(serial))
                })
                .count()
                == 1
        })
}

pub(crate) fn selections(
    inventory: &Inventory,
    manager: &SessionManager,
    monitor: &ReceiverMonitor,
) -> Vec<GuideSelection> {
    let mut output = Vec::new();
    if !inventory.serial_enumeration_failed {
        for endpoint in &inventory.ports {
            if endpoint.kind != SerialPortKind::Usb
                || !console_port_allowed(endpoint, inventory)
                || !unique_usb(endpoint, inventory)
                || !known_tracker(endpoint, manager)
            {
                continue;
            }
            let session = manager.session(&endpoint.port_name).filter(|session| {
                session.state == ConnectionState::Connected
                    && session.identity.role == DeviceRole::Tracker
            });
            let address = session
                .and_then(|session| session.identity.address.as_deref())
                .and_then(normalize_address);
            let identity = address
                .as_deref()
                .or(endpoint.serial_number.as_deref())
                .unwrap_or("");
            output.push(GuideSelection {
                label: format!("{} · {}", endpoint.label, identity),
                target: Target::Direct {
                    endpoint: endpoint.clone(),
                    generation: session.map(|session| session.generation),
                    address,
                },
            });
        }
    }
    for receiver in &inventory.receivers {
        if let Some(observation) = monitor.observation(&receiver.key) {
            for tracker in &observation.trackers {
                if tracker.slot >= 16 || !tracker.online {
                    continue;
                }
                let Some(address) = tracker.address.as_deref().and_then(normalize_address) else {
                    continue;
                };
                output.push(GuideSelection {
                    label: format!("{} · #{} · {}", receiver.label, tracker.slot, address),
                    target: Target::Wireless {
                        receiver: receiver.clone(),
                        slot: tracker.slot,
                        address,
                    },
                });
            }
        }
    }
    output
}

struct Pending {
    request: RunRequest,
    started: Instant,
    stopped: bool,
    previous: Option<Snapshot>,
}
#[derive(Default)]
pub(crate) struct CalibrationSession {
    next_id: u64,
    pin: Option<Pin>,
    pending: Option<Pending>,
    worker: Option<CalibrationWorker>,
    serial: Option<SerialReservation>,
    receiver: Option<ReceiverReservation>,
    result: Option<Arc<Snapshot>>,
    #[cfg(test)]
    fixture: Option<super::fixture::Factory>,
}

impl CalibrationSession {
    #[cfg(test)]
    pub(crate) fn install_fixture(&mut self, factory: super::fixture::Factory) {
        assert!(!self.native_busy());
        self.fixture = Some(factory);
    }
    pub(crate) fn review(
        &self,
        selection: GuideSelection,
        task: CalibrationTask,
        inventory: &Inventory,
        manager: &SessionManager,
        monitor: &ReceiverMonitor,
    ) -> Result<ReviewTicket, String> {
        let inspected = self.result.as_ref().is_some_and(|snapshot| {
            snapshot.joined
                && match (&selection.target, &snapshot.target) {
                    (
                        Target::Direct {
                            endpoint, address, ..
                        },
                        Target::Direct {
                            endpoint: observed, ..
                        },
                    ) => {
                        same_endpoint(endpoint, observed)
                            && address.is_some()
                            && *address == snapshot.identity
                    }
                    _ => false,
                }
        });
        validate_target(&selection.target, inventory, manager, monitor, inspected)?;
        let mut pin = Pin {
            address: match &selection.target {
                Target::Direct { address, .. } => address.clone(),
                Target::Wireless { address, .. } => Some(address.clone()),
            },
            ports: BTreeSet::new(),
            aliases: RefCell::new(Aliases::default()),
        };
        match &selection.target {
            Target::Direct { endpoint, .. } => {
                pin.ports.insert(endpoint.port_name.clone());
                if let Some(usb) = usb_binding(endpoint) {
                    pin.aliases.get_mut().usb.insert(usb);
                }
            }
            Target::Wireless { receiver, .. } => {
                pin.aliases.get_mut().receivers.insert(receiver.key.clone());
            }
        }
        learn_aliases(&pin, inventory, manager, monitor);
        for endpoint in &inventory.ports {
            if pin.blocks_port(
                &endpoint.port_name,
                manager
                    .session(&endpoint.port_name)
                    .map(|session| &session.identity),
                Some(endpoint),
            ) {
                pin.ports.insert(endpoint.port_name.clone());
            }
        }
        Ok(ReviewTicket {
            target: selection.target,
            task,
            pin,
        })
    }
    pub(crate) fn busy(&self) -> bool {
        self.native_busy()
            || self
                .result
                .as_ref()
                .is_some_and(|result| result.needs_acknowledgement() && !result.acknowledged)
    }
    pub(crate) fn native_busy(&self) -> bool {
        self.pending.is_some() || self.worker.is_some()
    }
    pub(crate) fn stopping(&self) -> bool {
        self.pending.as_ref().is_some_and(|pending| pending.stopped)
            || self
                .worker
                .as_ref()
                .is_some_and(CalibrationWorker::stopping)
    }
    pub(crate) fn snapshot(&self) -> Option<Arc<Snapshot>> {
        self.worker
            .as_ref()
            .map(CalibrationWorker::snapshot)
            .or_else(|| self.result.clone())
    }
    pub(crate) fn observe_aliases(
        &self,
        inventory: &Inventory,
        manager: &SessionManager,
        monitor: &ReceiverMonitor,
    ) {
        if let Some(pin) = &self.pin {
            learn_aliases(pin, inventory, manager, monitor);
        }
    }
    pub(crate) fn blocks_port(
        &self,
        port: &str,
        identity: Option<&DeviceIdentity>,
        endpoint: Option<&SerialEndpoint>,
    ) -> bool {
        self.pin
            .as_ref()
            .is_some_and(|pin| pin.blocks_port(port, identity, endpoint))
    }
    pub(crate) fn blocks_receiver(&self, key: &str) -> bool {
        self.pin
            .as_ref()
            .is_some_and(|pin| pin.aliases.borrow().receivers.contains(key))
    }
    pub(crate) fn blocks_receiver_io(&self, key: &str) -> bool {
        self.native_busy() && self.blocks_receiver(key)
    }

    pub(crate) fn blocks_scoped_tracker_io(&self, receiver: &str, address: &str) -> bool {
        self.blocks_receiver_io(receiver)
            || self.pin.as_ref().is_some_and(|pin| {
                address_digits(address).map_or_else(
                    || self.blocks_receiver(receiver),
                    |address| {
                        pin.address
                            .as_ref()
                            .is_some_and(|known| known.as_bytes() == address.as_slice())
                    },
                )
            })
    }

    pub(crate) fn start(
        &mut self,
        ticket: ReviewTicket,
        mode: RunMode,
        manager: &mut SessionManager,
        monitor: &mut ReceiverMonitor,
    ) -> Result<(), String> {
        if self.busy() {
            return Err("Review or acknowledge the current calibration result first".into());
        }
        if mode == RunMode::Recover {
            return Err("Use observe-only recovery for the retained result".into());
        }
        if mode == RunMode::Execute
            && matches!(&ticket.target, Target::Direct { address: None, .. })
        {
            return Err("Inspect and identify the tracker before starting".into());
        }
        self.admit(ticket, mode, None, manager, monitor)
    }
    fn admit(
        &mut self,
        ticket: ReviewTicket,
        mode: RunMode,
        previous: Option<Snapshot>,
        manager: &mut SessionManager,
        monitor: &mut ReceiverMonitor,
    ) -> Result<(), String> {
        for session in manager.sessions() {
            if ticket
                .pin
                .blocks_port(&session.port, Some(&session.identity), None)
                && manager.thermal_dump_busy(&session.port)
            {
                return Err("A temperature dump owns this tracker console".into());
            }
        }
        let next_id = self
            .next_id
            .checked_add(1)
            .ok_or("Calibration run identifier exhausted")?;
        let receiver = if let Target::Wireless { receiver, .. } = &ticket.target {
            Some(monitor.reserve(receiver)?)
        } else {
            None
        };
        self.next_id = next_id;
        let request = RunRequest {
            id: previous
                .as_ref()
                .map_or(self.next_id, |snapshot| snapshot.id),
            target: ticket.target,
            task: ticket.task,
            mode,
            recovery: previous.as_ref().and_then(|snapshot| snapshot.association),
        };
        let initial = previous
            .clone()
            .map_or_else(
                || Engine::new(request.clone()),
                |snapshot| Engine::recover(request.clone(), snapshot),
            )
            .snapshot;
        self.result = Some(Arc::new(initial));
        self.receiver = receiver;
        self.pin = Some(ticket.pin);
        self.pending = Some(Pending {
            request,
            started: Instant::now(),
            stopped: false,
            previous,
        });
        Ok(())
    }
    pub(crate) fn recover(
        &mut self,
        manager: &mut SessionManager,
        monitor: &mut ReceiverMonitor,
    ) -> Result<(), String> {
        if self.native_busy() {
            return Err("Wait for calibration observation cleanup to join".into());
        }
        let previous = self
            .result
            .as_ref()
            .ok_or("No calibration result to observe")?
            .as_ref()
            .clone();
        if !previous.joined || previous.acknowledged {
            return Err("No pending calibration outcome to observe".into());
        }
        if matches!(previous.target, Target::Wireless { .. }) && previous.association.is_none() {
            return Err(
                "Receiver firmware cannot recover an operation that was never identified".into(),
            );
        }
        let pin = self
            .pin
            .as_ref()
            .cloned()
            .ok_or("No retained tracker identity")?;
        let target = match &previous.target {
            Target::Direct {
                endpoint, address, ..
            } => Target::Direct {
                endpoint: endpoint.clone(),
                generation: None,
                address: previous.identity.clone().or_else(|| address.clone()),
            },
            target => target.clone(),
        };
        let ticket = ReviewTicket {
            target,
            task: previous.task,
            pin,
        };
        self.admit(ticket, RunMode::Recover, Some(previous), manager, monitor)
    }
    pub(crate) fn stop_observing(&mut self) {
        if let Some(pending) = &mut self.pending {
            pending.stopped = true;
        }
        if let Some(worker) = &self.worker {
            worker.stop();
        }
    }
    pub(crate) fn acknowledge(&mut self) -> Result<(), String> {
        if self.native_busy() {
            return Err("Wait for native calibration cleanup to finish".into());
        }
        let result = self
            .result
            .as_ref()
            .ok_or("No calibration result to acknowledge")?;
        if !result.joined {
            return Err("Native calibration cleanup has not joined".into());
        }
        let mut result = result.as_ref().clone();
        result.acknowledged = true;
        self.result = Some(Arc::new(result));
        self.pin = None;
        Ok(())
    }
    pub(crate) fn poll(
        &mut self,
        inventory: &Inventory,
        manager: &mut SessionManager,
        monitor: &mut ReceiverMonitor,
    ) -> bool {
        self.observe_aliases(inventory, manager, monitor);
        let mut changed = false;
        if self.pending.is_some() {
            let advance = self.advance(inventory, manager, monitor);
            match advance {
                Ok(false) => {}
                Ok(true) => changed = true,
                Err(error) => {
                    self.pending = None;
                    if let Some(result) = &self.result {
                        let mut result = result.as_ref().clone();
                        result.uncertainty = Some(error);
                        result.end = Some(ObservationEnd::PreparationFailed);
                        result.joined = true;
                        self.result = Some(Arc::new(result));
                    }
                    self.release(manager, monitor);
                    changed = true;
                }
            }
        }
        if let Some(worker) = &mut self.worker {
            if worker.join() {
                let mut snapshot = worker.snapshot().as_ref().clone();
                snapshot.joined = true;
                self.result = Some(Arc::new(snapshot));
                self.worker = None;
                self.release(manager, monitor);
                changed = true;
            } else {
                let snapshot = worker.snapshot();
                if self
                    .result
                    .as_ref()
                    .is_none_or(|previous| !Arc::ptr_eq(previous, &snapshot))
                {
                    self.result = Some(snapshot);
                    changed = true;
                }
            }
        }
        changed
    }
    fn advance(
        &mut self,
        inventory: &Inventory,
        manager: &mut SessionManager,
        monitor: &mut ReceiverMonitor,
    ) -> Result<bool, String> {
        let pending = self.pending.as_ref().expect("pending calibration");
        if let Some(token) = &self.receiver
            && !monitor.reservation_ready(token)?
        {
            return Ok(false);
        }
        if let Some(token) = &self.serial
            && !manager.reservation_ready(token)?
        {
            return Ok(false);
        }
        if pending.stopped {
            self.pending = None;
            if let Some(result) = &self.result {
                let mut result = result.as_ref().clone();
                if result.end.is_none() {
                    result.end = Some(ObservationEnd::Stopped);
                }
                result.joined = true;
                self.result = Some(Arc::new(result));
            }
            self.release(manager, monitor);
            return Ok(true);
        }
        if manager.sessions().any(|session| {
            self.blocks_port(
                &session.port,
                Some(&session.identity),
                inventory
                    .ports
                    .iter()
                    .find(|endpoint| same_port_name(&endpoint.port_name, &session.port)),
            ) && !manager.writes_idle(&session.port)
        }) {
            if pending.started.elapsed() >= Duration::from_secs(5) {
                return Err("Accepted serial writes did not drain; no calibration was sent".into());
            }
            return Ok(false);
        }
        if let Target::Direct {
            endpoint,
            generation,
            ..
        } = &pending.request.target
        {
            if inventory.serial_enumeration_failed
                || !console_port_allowed(endpoint, inventory)
                || !inventory
                    .ports
                    .iter()
                    .any(|current| same_endpoint(endpoint, current))
            {
                return Err("Tracker USB endpoint changed before calibration handoff".into());
            }
            if self.serial.is_none() {
                self.serial = Some(manager.reserve(&endpoint.port_name, *generation)?);
                return Ok(false);
            }
        } else if let Target::Wireless { receiver, .. } = &pending.request.target
            && !inventory
                .receivers
                .iter()
                .any(|current| same_receiver(receiver, current))
        {
            return Err("Receiver endpoint changed before calibration handoff".into());
        }
        let access = match &pending.request.target {
            Target::Direct { .. } => {
                super::worker::Access::Direct(manager.reserved_console_source(
                    self.serial.as_ref().expect("reserved direct console"),
                )?)
            }
            Target::Wireless { receiver, .. } => super::worker::Access::Wireless(receiver.clone()),
        };
        let pending = self.pending.take().expect("pending calibration");
        #[cfg(test)]
        let worker = if let Some(factory) = &self.fixture {
            factory(&pending.request).and_then(|transport| {
                CalibrationWorker::fixture(pending.request, pending.previous, transport)
            })
        } else {
            CalibrationWorker::start(pending.request, access, pending.previous)
        };
        #[cfg(not(test))]
        let worker = CalibrationWorker::start(pending.request, access, pending.previous);
        match worker {
            Ok(worker) => self.worker = Some(worker),
            Err(error) => {
                if let Some(result) = &self.result {
                    let mut result = result.as_ref().clone();
                    result.end = Some(ObservationEnd::PreparationFailed);
                    result.uncertainty = Some(error);
                    result.joined = true;
                    self.result = Some(Arc::new(result));
                }
                self.release(manager, monitor);
            }
        }
        Ok(true)
    }
    fn release(&mut self, manager: &mut SessionManager, monitor: &mut ReceiverMonitor) {
        if self.worker.is_some() || self.pending.is_some() {
            return;
        }
        let mut error = None;
        if let Some(token) = self.serial.take()
            && let Err(message) = manager.release(&token)
        {
            error = Some(message);
        }
        if let Some(token) = self.receiver.take()
            && let Err(message) = monitor.release(&token)
        {
            error = Some(message);
        }
        if let Some(result) = &self.result {
            if let Some(error) = error {
                let mut result = result.as_ref().clone();
                result.cleanup_error = Some(error);
                self.result = Some(Arc::new(result));
            }
            if !self
                .result
                .as_ref()
                .is_some_and(|result| result.needs_acknowledgement() && !result.acknowledged)
            {
                self.pin = None;
            }
        } else {
            self.pin = None;
        }
    }
    pub(crate) fn take_shutdown_worker(&mut self) -> Option<CalibrationWorker> {
        self.pending = None;
        if let Some(worker) = &self.worker {
            worker.stop();
        }
        self.worker.take()
    }
}

fn same_receiver(a: &ReceiverEndpoint, b: &ReceiverEndpoint) -> bool {
    a.key == b.key && a.path == b.path && a.serial_number == b.serial_number
}
fn validate_target(
    target: &Target,
    inventory: &Inventory,
    manager: &SessionManager,
    monitor: &ReceiverMonitor,
    inspected: bool,
) -> Result<(), String> {
    match target {
        Target::Direct {
            endpoint,
            generation,
            address,
        } => {
            if inventory.serial_enumeration_failed
                || endpoint.kind != SerialPortKind::Usb
                || !console_port_allowed(endpoint, inventory)
                || !unique_usb(endpoint, inventory)
                || !inventory
                    .ports
                    .iter()
                    .any(|current| same_endpoint(endpoint, current))
                || (!known_tracker(endpoint, manager) && !inspected)
            {
                return Err("Select a positively identified tracker USB endpoint".into());
            }
            if generation.is_some_and(|generation| {
                manager.session(&endpoint.port_name).is_none_or(|session| {
                    session.generation != generation
                        || session.state != ConnectionState::Connected
                        || session.identity.role != DeviceRole::Tracker
                })
            }) {
                return Err("Tracker console connection changed".into());
            }
            if address
                .as_deref()
                .is_some_and(|address| normalize_address(address).as_deref() != Some(address))
            {
                return Err("Tracker identity is not canonical".into());
            }
        }
        Target::Wireless {
            receiver,
            slot,
            address,
        } => {
            if *slot >= 16
                || normalize_address(address).as_deref() != Some(address)
                || !inventory
                    .receivers
                    .iter()
                    .any(|current| same_receiver(receiver, current))
                || !monitor
                    .observation(&receiver.key)
                    .is_some_and(|observation| {
                        observation.trackers.iter().any(|tracker| {
                            tracker.slot == *slot
                                && tracker.online
                                && tracker
                                    .address
                                    .as_deref()
                                    .and_then(normalize_address)
                                    .as_deref()
                                    == Some(address)
                        })
                    })
            {
                return Err(
                    "Select a currently online tracker with a confirmed receiver identity".into(),
                );
            }
        }
    }
    Ok(())
}
fn learn_aliases(
    pin: &Pin,
    inventory: &Inventory,
    manager: &SessionManager,
    monitor: &ReceiverMonitor,
) {
    for receiver in &inventory.receivers {
        let matches = pin.address.as_ref().is_some_and(|address| {
            monitor
                .observation(&receiver.key)
                .is_some_and(|observation| {
                    observation.trackers.iter().any(|tracker| {
                        tracker
                            .address
                            .as_deref()
                            .and_then(normalize_address)
                            .as_ref()
                            == Some(address)
                    })
                })
        });
        let known = pin.aliases.borrow().receivers.contains(&receiver.key);
        if matches || known {
            if !known {
                pin.aliases
                    .borrow_mut()
                    .receivers
                    .insert(receiver.key.clone());
            }
            let serial_known = receiver.serial_number.as_deref().is_some_and(|serial| {
                pin.aliases
                    .borrow()
                    .receiver_serials
                    .iter()
                    .any(|known| known.eq_ignore_ascii_case(serial))
            });
            if let Some(serial) = receiver
                .serial_number
                .as_deref()
                .filter(|serial| !serial.trim().is_empty())
                && (matches || serial_known)
            {
                if matches && !serial_known {
                    pin.aliases
                        .borrow_mut()
                        .receiver_serials
                        .insert(serial.to_owned());
                }
                for endpoint in &inventory.ports {
                    if endpoint
                        .serial_number
                        .as_deref()
                        .is_some_and(|other| other.eq_ignore_ascii_case(serial))
                    {
                        let mut aliases = pin.aliases.borrow_mut();
                        if !aliases.ports.contains(&endpoint.port_name) {
                            aliases.ports.insert(endpoint.port_name.clone());
                        }
                        if !aliases.usb.iter().any(|(vid, pid, known)| {
                            *vid == endpoint.vid
                                && *pid == endpoint.pid
                                && known.eq_ignore_ascii_case(serial)
                        }) && let Some(usb) = usb_binding(endpoint)
                        {
                            aliases.usb.insert(usb);
                        }
                    }
                }
            }
        }
    }
    for session in manager.sessions() {
        pin.blocks_port(
            &session.port,
            Some(&session.identity),
            inventory
                .ports
                .iter()
                .find(|endpoint| same_port_name(&endpoint.port_name, &session.port)),
        );
    }
}

#[cfg(test)]
mod tests;
