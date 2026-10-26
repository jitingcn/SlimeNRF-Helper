use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use hidapi::{DeviceInfo, HidApi};
use parking_lot::Mutex;
use serialport::SerialPortType;

#[cfg(not(any(windows, target_os = "linux")))]
use crate::domain::InterfaceDescription;
#[cfg(not(target_os = "linux"))]
use crate::domain::UsbInterface;
use crate::domain::{Inventory, ReceiverEndpoint, SerialEndpoint, SerialPortKind, TrackerLink};
use crate::hid_log::decode::BatteryTemperature;
use crate::i18n::tr;

mod events;
use crate::receiver_events::{EventWatchStatus, Watch};

#[cfg(windows)]
mod windows_device_notifications;

#[cfg(windows)]
mod windows_serial_names;

#[cfg(target_os = "linux")]
mod linux_serial_metadata;

const RECEIVER_VID: u16 = 0x1209;
const RECEIVER_PID: u16 = 0x7690;
const CONTROL_USAGE_PAGE: u16 = 0x01;
const REPORT_SIZE: usize = 64;
const RECORD_SIZE: usize = 16;
const SLOT_COUNT: usize = 16;
const TRAFFIC_TIMEOUT: Duration = Duration::from_secs(3);
const TPS_INTERVAL: Duration = Duration::from_secs(1);
const RETRY_INTERVAL: Duration = Duration::from_secs(3);
const READ_TIMEOUT_MS: i32 = 100;
const HEX: &[u8; 16] = b"0123456789ABCDEF";

/// Native notifications only accelerate enumeration; snapshots decide recovery.
/// Registration failure and other platforms retain the UI's periodic scan.
pub struct SerialNotifications {
    #[cfg(windows)]
    watcher: Option<windows_device_notifications::SerialNotifications>,
}

impl SerialNotifications {
    pub fn new() -> Self {
        Self {
            #[cfg(windows)]
            watcher: windows_device_notifications::SerialNotifications::register().ok(),
        }
    }

    /// Consume only when a scan can start, leaving one pending signal while busy.
    pub fn take_dirty(&self) -> bool {
        #[cfg(windows)]
        {
            self.watcher
                .as_ref()
                .is_some_and(|watcher| watcher.take_dirty())
        }
        #[cfg(not(windows))]
        {
            false
        }
    }
}

/// Enumerate enriched serial identities without opening serial or HID handles.
pub(crate) fn serial_endpoints() -> Result<Vec<SerialEndpoint>, String> {
    serialport::available_ports()
        .map(|ports| {
            #[cfg(windows)]
            let windows_names = windows_serial_names::PortNames::scan();
            let mut endpoints: Vec<_> = ports
                .into_iter()
                .map(|port| {
                    let (kind, manufacturer, product, serial_number, vid, pid, usb_interface) =
                        match port.port_type {
                            SerialPortType::UsbPort(usb) => {
                                #[cfg(windows)]
                                let interface = UsbInterface {
                                    number: usb.interface,
                                    description: windows_names
                                        .interface_description(&port.port_name),
                                };
                                #[cfg(target_os = "linux")]
                                let interface =
                                    linux_serial_metadata::usb_interface(&port.port_name, &usb);
                                #[cfg(not(any(windows, target_os = "linux")))]
                                let interface = UsbInterface {
                                    number: usb.interface,
                                    description: InterfaceDescription::Unavailable,
                                };
                                (
                                    SerialPortKind::Usb,
                                    usb.manufacturer
                                        .filter(|value| !value.trim().is_empty())
                                        .map(String::into_boxed_str),
                                    usb.product
                                        .filter(|value| !value.trim().is_empty())
                                        .map(String::into_boxed_str),
                                    clean_serial(usb.serial_number.as_deref()),
                                    Some(usb.vid),
                                    Some(usb.pid),
                                    Some(interface),
                                )
                            }
                            SerialPortType::BluetoothPort => (
                                SerialPortKind::Bluetooth,
                                None,
                                None,
                                None,
                                None,
                                None,
                                None,
                            ),
                            SerialPortType::PciPort => {
                                (SerialPortKind::Pci, None, None, None, None, None, None)
                            }
                            SerialPortType::Unknown => {
                                (SerialPortKind::Unknown, None, None, None, None, None, None)
                            }
                        };
                    let kind_description = match kind {
                        SerialPortKind::Bluetooth => Some(tr("devices.serial_kind.bluetooth")),
                        SerialPortKind::Pci => Some(tr("devices.serial_kind.pci")),
                        SerialPortKind::Usb | SerialPortKind::Unknown => None,
                    };
                    let description = product
                        .as_deref()
                        .or(manufacturer.as_deref())
                        .or(kind_description.as_deref());
                    #[cfg(windows)]
                    let description = windows_names.description(&port.port_name).or(description);
                    let label = description
                        .filter(|value| !value.trim().is_empty())
                        .map(|value| format!("{} — {}", port.port_name, value.trim()))
                        .unwrap_or_else(|| port.port_name.clone());
                    SerialEndpoint {
                        port_name: port.port_name,
                        label,
                        manufacturer,
                        product,
                        kind,
                        serial_number,
                        vid,
                        pid,
                        usb_interface,
                    }
                })
                .collect();
            endpoints.sort_by(|left, right| left.port_name.cmp(&right.port_name));
            endpoints
        })
        .map_err(|error| {
            rust_i18n::t!("devices.discovery.error.serial_enumeration", error = error).to_string()
        })
}

/// Enumerate endpoints only. No serial or HID handle is opened, and no command
/// is sent. A failed backend remains visible without hiding the other backend.
pub fn scan() -> Result<Inventory, String> {
    let ports = serial_endpoints();

    let receivers = HidApi::new()
        .map(|api| {
            let mut endpoints: Vec<_> = api
                .device_list()
                .filter(|device| is_control_receiver(device))
                .map(|device| {
                    let path = device.path().to_bytes().to_vec();
                    let serial_number = clean_serial(device.serial_number());
                    let product = device
                        .product_string()
                        .unwrap_or("SlimeNRF Receiver")
                        .trim();
                    let label = match serial_number.as_deref() {
                        Some(serial) => format!("{product} · {serial}"),
                        None => product.to_owned(),
                    };
                    ReceiverEndpoint {
                        key: receiver_key(&path),
                        path,
                        label,
                        serial_number,
                    }
                })
                .collect();
            endpoints.sort_by(|left, right| left.key.cmp(&right.key));
            // HID backends can repeat the same path. Never merge by serial:
            // missing or duplicate USB serials do not establish physical identity.
            endpoints.dedup_by(|left, right| left.path == right.path);
            endpoints
        })
        .map_err(|error| {
            rust_i18n::t!("devices.discovery.error.hid_enumeration", error = error).to_string()
        });

    combine_inventory(ports, receivers)
}

fn combine_inventory(
    ports: Result<Vec<SerialEndpoint>, String>,
    receivers: Result<Vec<ReceiverEndpoint>, String>,
) -> Result<Inventory, String> {
    match (ports, receivers) {
        (Ok(ports), Ok(receivers)) => Ok(Inventory {
            ports,
            receivers,
            warnings: Vec::new(),
            serial_enumeration_failed: false,
        }),
        (Ok(ports), Err(error)) => Ok(Inventory {
            ports,
            warnings: vec![error],
            ..Inventory::default()
        }),
        (Err(error), Ok(receivers)) => Ok(Inventory {
            receivers,
            warnings: vec![error],
            serial_enumeration_failed: true,
            ..Inventory::default()
        }),
        (Err(serial_error), Err(hid_error)) => Err(format!("{serial_error}\n{hid_error}")),
    }
}

fn clean_serial(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn is_control_receiver(device: &DeviceInfo) -> bool {
    control_interface(device.vendor_id(), device.product_id(), device.usage_page())
}

fn control_interface(vid: u16, pid: u16, usage_page: u16) -> bool {
    vid == RECEIVER_VID && pid == RECEIVER_PID && usage_page == CONTROL_USAGE_PAGE
}

fn receiver_key(path: &[u8]) -> String {
    let mut key = String::with_capacity(4 + path.len() * 2);
    key.push_str("hid:");
    for byte in path {
        key.push(HEX[(byte >> 4) as usize] as char);
        key.push(HEX[(byte & 0x0f) as usize] as char);
    }
    key
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct TrackerBattery {
    pub(crate) percent: u8,
    pub(crate) voltage_v: f32,
}

#[derive(Default)]
pub struct ReceiverObservation {
    pub trackers: Vec<TrackerLink>,
    pub error: Option<String>,
    rates: [u32; SLOT_COUNT],
    batteries: [Option<TrackerBattery>; SLOT_COUNT],
}

impl ReceiverObservation {
    /// Sensor records per second in the last completed sampling window.
    /// Missing slots and slots without recent sensor data report zero.
    pub fn tps(&self, slot: u8) -> u32 {
        self.rates.get(usize::from(slot)).copied().unwrap_or(0)
    }

    pub(crate) fn battery(&self, slot: u8) -> Option<TrackerBattery> {
        if self.error.is_some() {
            return None;
        }
        self.batteries.get(usize::from(slot)).copied().flatten()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BindingResetCause {
    ConfirmedMutation,
    AddressReplacement,
    EndpointReplacement,
    ManualRediscovery,
}

/// Frozen native identity, valid only in the monitor that issued it.
/// Keep this token until the exclusive firmware worker has joined.
#[derive(Clone, Debug)]
pub struct ReceiverReservation {
    identity: Arc<()>,
    endpoint: ReceiverEndpoint,
}

impl ReceiverReservation {
    pub fn endpoint(&self) -> &ReceiverEndpoint {
        &self.endpoint
    }
}

/// Owns one passive reader per native main-interface path. A cancelled reader
/// remains tracked until it exits, including across a rapid unplug/replug.
pub struct ReceiverMonitor {
    entries: BTreeMap<Vec<u8>, MonitorEntry>,
    changed: bool,
    shutting_down: bool,
    #[cfg(test)]
    fixture: Option<TrackerLink>,
}

struct MonitorEntry {
    receiver: ReceiverEndpoint,
    wanted: bool,
    worker: Option<ReaderWorker>,
    reservation: Option<ReceiverReservation>,
    reservation_invalidated: bool,
    state: Arc<Mutex<WorkerState>>,
    published_revision: u64,
    published_events: EventWatchStatus,
    delivered_binding_revision: u64,
    observation: ReceiverObservation,
    retry_at: Instant,
}

struct ReaderWorker {
    cancel: Arc<AtomicBool>,
    handle: JoinHandle<()>,
}

struct WorkerState {
    roster: Roster,
    error: Option<String>,
    revision: u64,
    events: events::Settings,
    binding_revision: u64,
    binding_reset: Option<BindingResetCause>,
    // Retained contradictions can revoke a plan even after a temporary handoff.
    known_bindings: [Option<[u8; 6]>; SLOT_COUNT],
}

type SpawnReader = fn(&ReceiverEndpoint, Arc<Mutex<WorkerState>>) -> std::io::Result<ReaderWorker>;

impl ReceiverMonitor {
    pub fn new() -> Self {
        Self {
            entries: BTreeMap::new(),
            changed: false,
            shutting_down: false,
            #[cfg(test)]
            fixture: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn fixture(
        receiver: ReceiverEndpoint,
        slot: u8,
        address: &str,
        online: bool,
    ) -> Self {
        assert!(slot < 16);
        let mut monitor = Self::new();
        let mut entry = MonitorEntry::new(receiver.clone(), Instant::now());
        let link = TrackerLink {
            receiver_key: receiver.key.clone(),
            slot,
            address: Some(address.to_owned()),
            online,
        };
        monitor.fixture = Some(link.clone());
        entry.observation.trackers.push(link);
        monitor.entries.insert(receiver.path, entry);
        monitor
    }

    #[cfg(test)]
    pub(crate) fn fixture_tps(&mut self, tps: u32) {
        let link = self
            .fixture
            .as_ref()
            .expect("TPS mutation requires a fixture monitor");
        let entry = self
            .entries
            .values_mut()
            .find(|entry| entry.receiver.key == link.receiver_key)
            .expect("fixture receiver is retained");
        entry.observation.rates[usize::from(link.slot)] = tps;
    }

    #[cfg(test)]
    pub(crate) fn fixture_extend(
        &mut self,
        receiver: ReceiverEndpoint,
        slot: u8,
        address: &str,
        online: bool,
    ) {
        let mut other = Self::fixture(receiver, slot, address, online);
        self.entries.append(&mut other.entries);
    }

    #[cfg(test)]
    pub(crate) fn fixture_observation_mut(
        &mut self,
        receiver: &ReceiverEndpoint,
    ) -> &mut ReceiverObservation {
        assert!(self.fixture.is_some());
        &mut self
            .entries
            .get_mut(&receiver.path)
            .expect("fixture receiver is retained")
            .observation
    }

    /// Retire observations, not firmware pairings or persisted device identities.
    pub(crate) fn rediscover_bindings(&mut self) {
        if self.shutting_down {
            return;
        }
        for entry in self.entries.values_mut() {
            if entry.wanted && entry.reservation.is_none() {
                entry.cancel();
                entry.retry_at = Instant::now();
                let mut state = entry.state.lock();
                state.reset_bindings(BindingResetCause::ManualRediscovery);
                state.publish(&mut entry.observation);
                self.changed = true;
            }
        }
    }

    pub(crate) fn observe_pairing_mutation(&mut self, receiver: &ReceiverEndpoint) -> bool {
        if self.shutting_down {
            return false;
        }
        let Some(entry) = self.entries.get_mut(&receiver.path).filter(|entry| {
            entry.wanted
                && entry.receiver.key == receiver.key
                && entry.receiver.serial_number == receiver.serial_number
        }) else {
            return false;
        };
        entry.cancel();
        entry.retry_at = Instant::now();
        let mut state = entry.state.lock();
        state.reset_bindings(BindingResetCause::ConfirmedMutation);
        state.publish(&mut entry.observation);
        self.changed = true;
        true
    }

    /// Coalesce changes per endpoint while retaining them across reader handoffs.
    pub(crate) fn take_binding_invalidations(
        &mut self,
    ) -> Vec<(ReceiverEndpoint, BindingResetCause)> {
        let mut changes = Vec::new();
        for entry in self.entries.values_mut() {
            let state = entry.state.lock();
            if state.binding_revision != entry.delivered_binding_revision {
                state.publish(&mut entry.observation);
                entry.delivered_binding_revision = state.binding_revision;
                if entry.wanted
                    && let Some(cause) = state.binding_reset
                {
                    changes.push((entry.receiver.clone(), cause));
                }
            }
        }
        changes
    }

    /// Explicit enable also retries a previous unsupported/protocol failure.
    pub fn set_events_enabled(
        &mut self,
        receiver: &ReceiverEndpoint,
        enabled: bool,
    ) -> Result<(), String> {
        if self.shutting_down {
            return Err("Receiver monitor is shut down".into());
        }
        let entry = self.entries.get_mut(&receiver.path).ok_or_else(|| {
            "Receiver is no longer available; refresh and select it again".to_string()
        })?;
        if !entry.wanted
            || entry.receiver.key != receiver.key
            || entry.receiver.serial_number != receiver.serial_number
        {
            return Err("Receiver identity changed; refresh and select it again".into());
        }
        let mut state = entry.state.lock();
        state.events.set_enabled(enabled);
        state.revision = state.revision.wrapping_add(1);
        self.changed = true;
        Ok(())
    }
    /// Revoke every retained intent, including disconnected and reserved receivers.
    /// Connected readers send their existing unsubscribe; replacement readers
    /// inherit the disabled intent rather than resubscribing on reconnect/release.
    pub fn stop_all_events(&mut self) -> usize {
        let mut stopped = 0;
        for entry in self.entries.values_mut() {
            let mut state = entry.state.lock();
            if state.events.enabled {
                state.events.set_enabled(false);
                state.revision = state.revision.wrapping_add(1);
                stopped += 1;
            }
        }
        self.changed |= stopped > 0;
        stopped
    }

    pub fn requested_events_count(&self) -> usize {
        self.entries
            .values()
            .filter(|entry| entry.state.lock().events.enabled)
            .count()
    }

    pub fn events_enabled(&self, key: &str) -> bool {
        self.entries
            .values()
            .find(|entry| entry.receiver.key == key)
            .is_some_and(|entry| entry.state.lock().events.enabled)
    }

    pub fn events_status(&self, key: &str) -> EventWatchStatus {
        let Some(entry) = self
            .entries
            .values()
            .find(|entry| entry.receiver.key == key)
        else {
            return EventWatchStatus::Disabled;
        };
        let state = entry.state.lock();
        if !state.events.enabled {
            state.events.status.clone()
        } else if entry.reservation.is_some() {
            EventWatchStatus::Paused
        } else if !entry.wanted {
            EventWatchStatus::Error(
                "Receiver is no longer available; reconnect it to resume events".into(),
            )
        } else if let Some(error) = &state.error {
            EventWatchStatus::Error(error.clone())
        } else {
            state.events.status.clone()
        }
    }

    /// Whether the current endpoint is held for an exclusive operation.
    /// Invalidated handoffs still block the reader until their owner releases.
    pub(crate) fn is_reserved(&self, receiver: &ReceiverEndpoint) -> bool {
        !self.shutting_down
            && self.entries.get(&receiver.path).is_some_and(|entry| {
                entry.wanted
                    && entry.receiver.key == receiver.key
                    && entry.receiver.serial_number == receiver.serial_number
                    && entry.reservation.is_some()
            })
    }

    pub fn reserve(&mut self, receiver: &ReceiverEndpoint) -> Result<ReceiverReservation, String> {
        if self.shutting_down {
            return Err("Receiver monitor is shut down".into());
        }
        let entry = self.entries.get_mut(&receiver.path).ok_or_else(|| {
            "Receiver is no longer available; refresh and select it again".to_string()
        })?;
        if !entry.wanted
            || entry.receiver.key != receiver.key
            || entry.receiver.serial_number != receiver.serial_number
        {
            return Err("Receiver identity changed; refresh and select it again".into());
        }
        if entry.reservation.is_some() {
            return Err("Receiver is reserved for a firmware operation".into());
        }
        let token = ReceiverReservation {
            identity: Arc::new(()),
            endpoint: receiver.clone(),
        };
        entry.reservation = Some(token.clone());
        entry.reservation_invalidated = false;
        entry.cancel();
        self.changed = true;
        Ok(token)
    }

    pub fn reservation_ready(&mut self, token: &ReceiverReservation) -> Result<bool, String> {
        self.reserved_entry(token)?;
        self.poll();
        let entry = self.reserved_entry(token)?;
        if entry.reservation_invalidated || !entry.wanted {
            return Err("Receiver disappeared or changed; refresh and select it again".into());
        }
        Ok(entry.worker.is_none())
    }

    /// Release after the firmware worker joins, or cancel a pending handoff.
    /// Reconciliation/polling may resume a reader only after its predecessor exits.
    pub fn release(&mut self, token: &ReceiverReservation) -> Result<(), String> {
        self.reserved_entry(token)?;
        let entry = self
            .entries
            .get_mut(&token.endpoint.path)
            .expect("validated reservation");
        entry.reservation = None;
        entry.reservation_invalidated = false;
        entry.retry_at = Instant::now();
        self.changed = true;
        Ok(())
    }

    fn reserved_entry(&self, token: &ReceiverReservation) -> Result<&MonitorEntry, String> {
        if !self.shutting_down
            && let Some(entry) = self.entries.get(&token.endpoint.path)
            && entry
                .reservation
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(&current.identity, &token.identity))
        {
            return Ok(entry);
        }
        Err("Receiver reservation is no longer valid".into())
    }

    pub fn reconcile(&mut self, receivers: &[ReceiverEndpoint]) {
        #[cfg(test)]
        if self.fixture.is_some() {
            return;
        }
        self.reconcile_at(receivers, Instant::now(), ReaderWorker::spawn);
    }

    fn reconcile_at(&mut self, receivers: &[ReceiverEndpoint], now: Instant, spawn: SpawnReader) {
        if self.shutting_down {
            return;
        }
        for (path, entry) in &mut self.entries {
            if entry.wanted && !receivers.iter().any(|receiver| receiver.path == *path) {
                entry.wanted = false;
                entry.reservation_invalidated |= entry.reservation.is_some();
                entry.cancel();
                self.changed = true;
            }
        }
        for receiver in receivers {
            let entry = self
                .entries
                .entry(receiver.path.clone())
                .or_insert_with(|| {
                    self.changed = true;
                    MonitorEntry::new(receiver.clone(), now)
                });
            // Compare the retained identity before restoring an absent entry.
            if entry.receiver.key != receiver.key
                || entry.receiver.serial_number != receiver.serial_number
            {
                entry.reservation_invalidated |= entry.reservation.is_some();
                entry.receiver = receiver.clone();
                entry.cancel_with_events(events::Settings::default());
                entry
                    .state
                    .lock()
                    .reset_bindings(BindingResetCause::EndpointReplacement);
                entry.retry_at = now;
                self.changed = true;
            }
            if !entry.wanted {
                entry.reservation_invalidated |= entry.reservation.is_some();
                entry.wanted = true;
                entry.retry_at = now;
                self.changed = true;
            }
            entry.start_if_due(now, spawn);
        }
    }

    /// Returns true for changed topology, recent-traffic status, sampled TPS, or errors.
    /// Sensor ingress only refreshes timestamps/counters; rate changes publish at
    /// most once per second, except when reassignment or expiry clears the rate.
    pub fn poll(&mut self) -> bool {
        #[cfg(test)]
        if let Some(link) = &self.fixture {
            for entry in self.entries.values_mut() {
                if entry.reservation.is_none() && entry.observation.trackers.is_empty() {
                    entry.observation.trackers.push(link.clone());
                    self.changed = true;
                }
            }
            return std::mem::take(&mut self.changed);
        }
        self.poll_at(Instant::now(), ReaderWorker::spawn)
    }

    fn poll_at(&mut self, now: Instant, spawn: SpawnReader) -> bool {
        for entry in self.entries.values_mut() {
            if entry
                .worker
                .as_ref()
                .is_some_and(|worker| worker.handle.is_finished())
            {
                let worker = entry.worker.take().expect("finished reader");
                let cancelled = worker.cancel.load(Ordering::Acquire);
                if worker.handle.join().is_err() && !cancelled {
                    entry
                        .state
                        .lock()
                        .set_error(Some(tr("devices.receiver.error.reader_stopped")));
                }
                if !cancelled {
                    entry.retry_at = now + RETRY_INTERVAL;
                }
            }
            if !entry.wanted {
                continue;
            }
            entry.start_if_due(now, spawn);
            let mut state = entry.state.lock();
            if state.roster.expire(now) {
                state.revision = state.revision.wrapping_add(1);
            }
            if entry.published_revision != state.revision {
                entry.published_revision = state.revision;
                self.changed |= state.publish(&mut entry.observation);
                if entry.published_events != state.events.status {
                    entry.published_events = state.events.status.clone();
                    self.changed = true;
                }
            }
        }
        self.entries.retain(|_, entry| {
            entry.wanted
                || entry.worker.is_some()
                || entry.reservation.is_some()
                || entry.state.lock().events.enabled
        });
        std::mem::take(&mut self.changed)
    }

    pub fn observation(&self, key: &str) -> Option<&ReceiverObservation> {
        self.entries
            .values()
            .find(|entry| entry.wanted && entry.receiver.key == key)
            .map(|entry| &entry.observation)
    }

    pub(crate) fn observation_for(
        &self,
        receiver: &ReceiverEndpoint,
    ) -> Option<&ReceiverObservation> {
        self.entries
            .get(&receiver.path)
            .filter(|entry| {
                entry.wanted
                    && entry.receiver.key == receiver.key
                    && entry.receiver.serial_number == receiver.serial_number
            })
            .map(|entry| &entry.observation)
    }

    /// Cancel immediately; joining readers happens off the caller/UI thread.
    /// HID reads time out every 100ms. OS enumeration/open latency is outside
    /// our control, so a pending open is never joined on the UI thread.
    pub fn shutdown(&mut self) -> Option<JoinHandle<()>> {
        self.shutting_down = true;
        let entries = std::mem::take(&mut self.entries);
        let mut handles = Vec::new();
        for entry in entries.into_values() {
            if let Some(worker) = entry.worker {
                worker.cancel.store(true, Ordering::Release);
                handles.push(worker.handle);
            }
        }
        self.changed = true;
        if handles.is_empty() {
            None
        } else {
            crate::sessions::reap_workers("receiver-reaper", handles)
        }
    }
}

impl Drop for ReceiverMonitor {
    fn drop(&mut self) {
        if let Some(reaper) = self.shutdown() {
            let _ = reaper.join();
        }
    }
}

impl MonitorEntry {
    fn new(receiver: ReceiverEndpoint, now: Instant) -> Self {
        Self {
            state: Arc::new(Mutex::new(WorkerState::new(&receiver.key))),
            receiver,
            wanted: true,
            worker: None,
            reservation: None,
            reservation_invalidated: false,
            published_revision: u64::MAX,
            observation: ReceiverObservation::default(),
            published_events: EventWatchStatus::Disabled,
            delivered_binding_revision: 0,
            retry_at: now,
        }
    }

    fn cancel(&mut self) {
        let settings = self.state.lock().events.for_reader_restart();
        self.cancel_with_events(settings);
    }

    fn cancel_with_events(&mut self, settings: events::Settings) {
        if let Some(worker) = &self.worker {
            worker.cancel.store(true, Ordering::Release);
        }
        // An old worker can finish/open late, but only writes its detached state.
        let mut state = WorkerState::new(&self.receiver.key);
        state.events = settings;
        {
            let previous = self.state.lock();
            state.binding_revision = previous.binding_revision;
            state.binding_reset = previous.binding_reset;
            state.known_bindings = previous.known_bindings;
        }
        self.state = Arc::new(Mutex::new(state));
        self.published_revision = u64::MAX;
        self.observation = ReceiverObservation::default();
    }

    fn start_if_due(&mut self, now: Instant, spawn: SpawnReader) {
        if !self.wanted
            || self.reservation.is_some()
            || self.worker.is_some()
            || now < self.retry_at
        {
            return;
        }
        let mut state = WorkerState::new(&self.receiver.key);
        {
            let previous = self.state.lock();
            state.error.clone_from(&previous.error);
            state.events = previous.events.for_reader_restart();
            state.binding_revision = previous.binding_revision;
            state.binding_reset = previous.binding_reset;
            state.known_bindings = previous.known_bindings;
        }
        self.state = Arc::new(Mutex::new(state));
        self.published_revision = u64::MAX;
        match spawn(&self.receiver, Arc::clone(&self.state)) {
            Ok(worker) => self.worker = Some(worker),
            Err(error) => {
                self.state.lock().set_error(Some(
                    rust_i18n::t!("devices.receiver.error.reader_start", error = error).to_string(),
                ));
                self.retry_at = now + RETRY_INTERVAL;
            }
        }
    }
}

impl ReaderWorker {
    fn spawn(receiver: &ReceiverEndpoint, state: Arc<Mutex<WorkerState>>) -> std::io::Result<Self> {
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let receiver = receiver.clone();
        let handle = thread::Builder::new()
            .name("receiver-hid".into())
            .spawn(move || {
                if let Err(error) = read_receiver(&receiver, &worker_cancel, &state)
                    && !worker_cancel.load(Ordering::Acquire)
                {
                    state.lock().set_error(Some(error));
                }
            })?;
        Ok(Self { cancel, handle })
    }
}

struct ReceiverInterface<'a> {
    path: &'a [u8],
    vid: u16,
    pid: u16,
    usage_page: u16,
    serial: Option<&'a str>,
}

enum ReceiverDevice {
    Native(hidapi::HidDevice),
    #[cfg(test)]
    Test(tests::TestHandle),
}

impl ReceiverDevice {
    fn trace(self, receiver: &ReceiverEndpoint) -> ReceiverReader {
        match self {
            Self::Native(device) => ReceiverReader::Native(crate::hid_log::TracedHid::new(
                device,
                crate::hid_log::Source::main(receiver),
            )),
            #[cfg(test)]
            Self::Test(device) => ReceiverReader::Test(device.trace(receiver)),
        }
    }
}

enum ReceiverReader {
    Native(crate::hid_log::TracedHid),
    #[cfg(test)]
    Test(tests::TracedTestHandle),
}

impl ReceiverReader {
    fn read_timeout(&self, bytes: &mut [u8], timeout_ms: i32) -> Result<usize, String> {
        match self {
            Self::Native(device) => device
                .read_timeout(bytes, timeout_ms)
                .map_err(|error| error.to_string()),
            #[cfg(test)]
            Self::Test(device) => device.read_timeout(bytes),
        }
    }

    fn write(&self, bytes: &[u8]) -> Result<usize, String> {
        match self {
            Self::Native(device) => device.write(bytes).map_err(|error| error.to_string()),
            #[cfg(test)]
            Self::Test(device) => device.write(bytes),
        }
    }
}

fn with_receiver_handle<'a, D, H>(
    receiver: &ReceiverEndpoint,
    cancel: &AtomicBool,
    interfaces: impl IntoIterator<Item = (ReceiverInterface<'a>, D)>,
    open: impl FnOnce(D) -> Result<H, String>,
    read_serial: impl FnOnce(&H) -> Result<Option<String>, String>,
    read: impl FnOnce(H) -> Result<(), String>,
) -> Result<(), String> {
    if cancel.load(Ordering::Acquire) {
        return Ok(());
    }
    let (interface, selected) = interfaces
        .into_iter()
        .find(|(interface, _)| {
            interface.path == receiver.path
                && control_interface(interface.vid, interface.pid, interface.usage_page)
        })
        .ok_or_else(|| {
            rust_i18n::t!(
                "devices.receiver.error.interface_unavailable",
                receiver = receiver.label
            )
            .to_string()
        })?;
    if interface.serial.map(str::trim).filter(|s| !s.is_empty())
        != receiver.serial_number.as_deref()
    {
        return Err(tr("devices.receiver.error.identity_changed"));
    }
    let device = open(selected)?;
    if cancel.load(Ordering::Acquire) {
        return Ok(());
    }
    match read_serial(&device) {
        Ok(serial)
            if serial.as_deref().map(str::trim).filter(|s| !s.is_empty())
                != receiver.serial_number.as_deref() =>
        {
            return Err(tr("devices.receiver.error.identity_changed"));
        }
        Err(error) => {
            return Err(
                rust_i18n::t!("devices.receiver.error.identity_unverified", error = error)
                    .to_string(),
            );
        }
        Ok(_) => {}
    }
    if cancel.load(Ordering::Acquire) {
        return Ok(());
    }
    read(device)
}

fn read_receiver(
    receiver: &ReceiverEndpoint,
    cancel: &AtomicBool,
    state: &Mutex<WorkerState>,
) -> Result<(), String> {
    if cancel.load(Ordering::Acquire) {
        return Ok(());
    }
    if receiver.path.contains(&0) {
        return Err(tr("devices.receiver.error.invalid_path"));
    }
    let api = HidApi::new().map_err(|error| {
        rust_i18n::t!("devices.discovery.error.hid_enumeration", error = error).to_string()
    })?;
    with_receiver_handle(
        receiver,
        cancel,
        api.device_list().map(|device| {
            (
                ReceiverInterface {
                    path: device.path().to_bytes(),
                    vid: device.vendor_id(),
                    pid: device.product_id(),
                    usage_page: device.usage_page(),
                    serial: device.serial_number(),
                },
                device,
            )
        }),
        |device| {
            device.open_device(&api).map_err(|error| {
                rust_i18n::t!(
                    "devices.receiver.error.open",
                    receiver = receiver.label,
                    error = error
                )
                .to_string()
            })
        },
        |device| {
            device
                .get_serial_number_string()
                .map_err(|error| error.to_string())
        },
        |device| read_receiver_reports(receiver, cancel, state, ReceiverDevice::Native(device)),
    )
}

fn read_receiver_reports(
    receiver: &ReceiverEndpoint,
    cancel: &AtomicBool,
    state: &Mutex<WorkerState>,
    device: ReceiverDevice,
) -> Result<(), String> {
    if cancel.load(Ordering::Acquire) {
        return Ok(());
    }
    let device = device.trace(receiver);
    state.lock().set_error(None);
    // One extra byte rejects report-ID/oversized frames instead of truncating.
    let mut report = [0_u8; REPORT_SIZE + 1];
    let mut watch = state.lock().events.watch();
    loop {
        let cancelled = cancel.load(Ordering::Acquire);
        {
            let mut current = state.lock();
            watch.sync(current.events.enabled, current.events.generation, cancelled);
            current.publish_events(&watch);
        }
        if let Some(command) = watch.tick(Instant::now()) {
            match device.write(&command.bytes) {
                Ok(length) if length == command.bytes.len() => {}
                Ok(length) => watch.write_failed(format!(
                    "Incomplete event subscription HID write ({length}/{} bytes); enable events to retry",
                    command.bytes.len()
                )),
                Err(error) => watch.write_failed(format!(
                    "Cannot write event subscription: {error}; enable events to retry"
                )),
            }
        }
        state.lock().publish_events(&watch);
        if cancelled && !watch.cleanup_pending() {
            break;
        }
        let length = match device.read_timeout(&mut report, READ_TIMEOUT_MS) {
            Ok(length) => length,
            Err(error) => {
                // A failed reader cannot await cleanup ACKs, but still attempts stop
                // on its own handle before returning ownership to the monitor.
                let settings = &state.lock().events;
                watch.sync(settings.enabled, settings.generation, true);
                if let Some(command) = watch.tick(Instant::now()) {
                    let _ = device.write(&command.bytes);
                }
                return Err(rust_i18n::t!(
                    "devices.receiver.error.read",
                    receiver = receiver.label,
                    error = error
                )
                .to_string());
            }
        };
        if length != 0 {
            let now = Instant::now();
            watch.ingest(&report[..length], now);
            let mut current = state.lock();
            if !cancel.load(Ordering::Acquire) {
                current.ingest(&report[..length], now);
            }
            current.publish_events(&watch);
        }
    }
    Ok(())
}

impl WorkerState {
    fn new(receiver_key: &str) -> Self {
        Self {
            roster: Roster::new(receiver_key),
            error: None,
            revision: 0,
            events: events::Settings::default(),
            binding_revision: 0,
            binding_reset: None,
            known_bindings: [None; SLOT_COUNT],
        }
    }

    fn publish_events(&mut self, watch: &Watch) {
        if self.events.generation == watch.generation() && self.events.status != *watch.status() {
            self.events.status = watch.status().clone();
            self.revision = self.revision.wrapping_add(1);
        }
    }

    fn reset_bindings(&mut self, cause: BindingResetCause) {
        self.roster.slots.fill_with(|| None);
        self.known_bindings.fill(None);
        self.binding_revision = self.binding_revision.wrapping_add(1);
        self.binding_reset = Some(cause);
        self.revision = self.revision.wrapping_add(1);
    }

    fn ingest(&mut self, report: &[u8], now: Instant) {
        if report.len() != REPORT_SIZE {
            return;
        }
        let records = report.as_chunks::<RECORD_SIZE>().0;
        if records
            .iter()
            .any(|record| record[0] == 251 && matches!(record[2], 203 | 205) && record[3] == 0)
        {
            self.reset_bindings(BindingResetCause::ConfirmedMutation);
            // Other queued records in an ACK frame may predate the mutation.
            return;
        }
        if records.iter().any(|record| {
            let slot = usize::from(record[1]);
            record[0] == 0xff
                && slot < SLOT_COUNT
                && valid_registration_address(&record[2..8])
                && self.known_bindings[slot].is_some_and(|address| address != record[2..8])
        }) {
            self.reset_bindings(BindingResetCause::AddressReplacement);
        }
        if self.roster.ingest_at(report, now) {
            self.revision = self.revision.wrapping_add(1);
        }
        for record in records {
            let slot = usize::from(record[1]);
            if record[0] == 0xff && slot < SLOT_COUNT && valid_registration_address(&record[2..8]) {
                self.known_bindings[slot] =
                    Some(record[2..8].try_into().expect("six-byte address"));
            }
        }
    }

    fn set_error(&mut self, error: Option<String>) {
        if self.error != error {
            self.error = error;
            self.revision = self.revision.wrapping_add(1);
        }
    }

    fn publish(&self, observation: &mut ReceiverObservation) -> bool {
        let mut links = observation.trackers.iter();
        let links_match = self.roster.slots.iter().flatten().all(|slot| {
            links.next().is_some_and(|link| {
                slot.link.receiver_key == link.receiver_key
                    && slot.link.slot == link.slot
                    && slot.link.address == link.address
                    && slot.link.online == link.online
            })
        }) && links.next().is_none();
        let rates = self.roster.rates();
        let batteries = self.roster.batteries();
        let changed = !links_match
            || self.error != observation.error
            || rates != observation.rates
            || batteries != observation.batteries;
        // Telemetry-only updates must not reallocate/clone tracker identities.
        if !links_match {
            observation.trackers = self.roster.links();
        }
        if self.error != observation.error {
            observation.error.clone_from(&self.error);
        }
        observation.rates = rates;
        observation.batteries = batteries;
        changed
    }
}

fn valid_registration_address(address: &[u8]) -> bool {
    address != [0; 6] && address != [0xff; 6]
}

struct Roster {
    receiver_key: String,
    slots: [Option<ObservedSlot>; SLOT_COUNT],
}

struct ObservedSlot {
    link: TrackerLink,
    address: Option<[u8; 6]>,
    last_seen: Option<Instant>,
    packet_count: u32,
    window_start: Option<Instant>,
    tps: u32,
    battery: Option<TrackerBattery>,
}

impl Roster {
    fn new(receiver_key: &str) -> Self {
        Self {
            receiver_key: receiver_key.to_owned(),
            slots: std::array::from_fn(|_| None),
        }
    }

    #[cfg(test)]
    fn ingest(&mut self, report: &[u8]) {
        self.ingest_at(report, Instant::now());
    }

    fn ingest_at(&mut self, report: &[u8], now: Instant) -> bool {
        // Do not zero-pad short reads: zeros are valid sensor records.
        // Reject raw-interface/report-ID payloads rather than guessing offsets.
        if report.len() != REPORT_SIZE {
            return false;
        }
        let mut changed = false;
        for record in report.as_chunks::<RECORD_SIZE>().0 {
            let kind = record[0];
            let slot = record[1] as usize;
            if slot >= SLOT_COUNT || !(kind == 0xff || kind <= 0x07) {
                continue;
            }
            if kind == 0xff && !valid_registration_address(&record[2..8]) {
                continue;
            }
            let observed = self.slots[slot].get_or_insert_with(|| {
                changed = true;
                ObservedSlot {
                    link: TrackerLink {
                        receiver_key: self.receiver_key.clone(),
                        slot: slot as u8,
                        address: None,
                        online: false,
                    },
                    address: None,
                    last_seen: None,
                    packet_count: 0,
                    window_start: None,
                    tps: 0,
                    battery: None,
                }
            });
            if kind == 0xff {
                let address: [u8; 6] = record[2..8].try_into().expect("six-byte address");
                if observed.address == Some(address) {
                    continue;
                }
                // Reassignment cannot inherit traffic from a previous address.
                // First registration may follow sensor data in the same report.
                if observed.address.is_some() {
                    observed.last_seen = None;
                    observed.link.online = false;
                    observed.packet_count = 0;
                    observed.window_start = None;
                    observed.tps = 0;
                    observed.battery = None;
                }
                observed.address = Some(address);
                let text = observed
                    .link
                    .address
                    .get_or_insert_with(|| String::with_capacity(12));
                text.clear();
                for byte in address.iter().rev() {
                    text.push(HEX[(byte >> 4) as usize] as char);
                    text.push(HEX[(byte & 0x0f) as usize] as char);
                }
                changed = true;
            } else {
                observed.window_start.get_or_insert(now);
                observed.packet_count = observed.packet_count.saturating_add(1);
                observed.last_seen = Some(now);
                changed |= !observed.link.online;
                observed.link.online = true;
                if let Some(telemetry) = BatteryTemperature::parse(record) {
                    let battery = telemetry
                        .percent()
                        .zip(telemetry.voltage_v())
                        .map(|(percent, voltage_v)| TrackerBattery { percent, voltage_v });
                    changed |= observed.battery != battery;
                    observed.battery = battery;
                }
            }
        }
        changed
    }

    fn expire(&mut self, now: Instant) -> bool {
        let mut changed = false;
        for slot in self.slots.iter_mut().flatten() {
            if slot.link.online
                && slot
                    .last_seen
                    .is_some_and(|seen| now.saturating_duration_since(seen) >= TRAFFIC_TIMEOUT)
            {
                slot.link.online = false;
                // Do not retain a sampled rate across the offline boundary,
                // even if the next one-second window has not completed yet.
                slot.packet_count = 0;
                slot.window_start = None;
                slot.tps = 0;
                slot.battery = None;
                changed = true;
            }
            if let Some(start) = slot.window_start {
                let elapsed = now.saturating_duration_since(start);
                if elapsed >= TPS_INTERVAL {
                    // Normalize by the actual poll interval, not an assumed 1s.
                    let rate =
                        (u128::from(slot.packet_count) * 1_000_000_000 / elapsed.as_nanos()) as u32;
                    changed |= slot.tps != rate;
                    slot.tps = rate;
                    slot.packet_count = 0;
                    slot.window_start = Some(now);
                }
            }
        }
        changed
    }

    fn rates(&self) -> [u32; SLOT_COUNT] {
        std::array::from_fn(|index| self.slots[index].as_ref().map_or(0, |slot| slot.tps))
    }

    fn batteries(&self) -> [Option<TrackerBattery>; SLOT_COUNT] {
        std::array::from_fn(|index| self.slots[index].as_ref().and_then(|slot| slot.battery))
    }

    fn links(&self) -> Vec<TrackerLink> {
        self.slots
            .iter()
            .flatten()
            .map(|slot| slot.link.clone())
            .collect()
    }

    #[cfg(test)]
    fn into_links(self) -> Vec<TrackerLink> {
        self.slots
            .into_iter()
            .flatten()
            .map(|slot| slot.link)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_monitor_reserves_and_restores_without_spawning_native_readers() {
        let receiver = receiver(b"fixture-no-os");
        let mut monitor = ReceiverMonitor::fixture(receiver.clone(), 3, "010203040506", true);
        assert_eq!(
            monitor.observation(&receiver.key).unwrap().trackers[0]
                .address
                .as_deref(),
            Some("010203040506")
        );
        let reservation = monitor.reserve(&receiver).unwrap();
        assert!(monitor.reservation_ready(&reservation).unwrap());
        assert!(
            monitor
                .observation(&receiver.key)
                .unwrap()
                .trackers
                .is_empty()
        );
        monitor.release(&reservation).unwrap();
        assert!(monitor.poll());
        monitor.reconcile(std::slice::from_ref(&receiver));
        let link = &monitor.observation(&receiver.key).unwrap().trackers[0];
        assert_eq!(link.slot, 3);
        assert!(link.online);
        assert!(monitor.entries.values().all(|entry| entry.worker.is_none()));
    }

    fn frame(kind: u8, slot: u8) -> [u8; REPORT_SIZE] {
        let mut report = [0xf8; REPORT_SIZE];
        report[0] = kind;
        report[1] = slot;
        report
    }

    fn battery_frame(kind: u8, slot: u8, telemetry: [u8; 3]) -> [u8; REPORT_SIZE] {
        let mut report = frame(kind, slot);
        report[2..5].copy_from_slice(&telemetry);
        report
    }

    fn receiver(path: &[u8]) -> ReceiverEndpoint {
        ReceiverEndpoint {
            key: receiver_key(path),
            path: path.to_vec(),
            label: "Test receiver".into(),
            serial_number: None,
        }
    }

    struct OpenProbe {
        result: Result<(), String>,
        opened: usize,
        serial_queries: usize,
        reader_entries: usize,
        dropped: usize,
        writes: Vec<[u8; 65]>,
        slots: Vec<u8>,
        opened_usage_pages: Vec<u16>,
        observation: ReceiverObservation,
        trace: Vec<&'static str>,
    }

    enum TestRead {
        Report([u8; REPORT_SIZE]),
        Cancel,
    }

    #[derive(Default)]
    struct TestIo {
        reads: std::collections::VecDeque<TestRead>,
        writes: Vec<[u8; 65]>,
        trace: Vec<&'static str>,
        opened: usize,
        opened_usage_pages: Vec<u16>,
        serial_queries: usize,
        reader_entries: usize,
        dropped: usize,
    }

    pub(super) struct TestHandle {
        io: Arc<Mutex<TestIo>>,
        cancel: Arc<AtomicBool>,
    }

    impl TestHandle {
        pub(super) fn trace(self, receiver: &ReceiverEndpoint) -> TracedTestHandle {
            self.io.lock().reader_entries += 1;
            let traced = TracedTestHandle {
                handle: self,
                source: Arc::new(crate::hid_log::Source::main(receiver)),
            };
            traced.publish(crate::hid_log::RecordData::Open);
            traced
        }
    }

    impl Drop for TestHandle {
        fn drop(&mut self) {
            self.io.lock().dropped += 1;
        }
    }

    pub(super) struct TracedTestHandle {
        handle: TestHandle,
        source: Arc<crate::hid_log::Source>,
    }

    impl TracedTestHandle {
        fn publish(&self, record: crate::hid_log::RecordData) {
            use crate::hid_log::{Direction, RecordData};
            self.handle.io.lock().trace.push(match &record {
                RecordData::Open => "open",
                RecordData::Close => "close",
                RecordData::Error { .. } => "error",
                RecordData::Report {
                    direction: Direction::In,
                    ..
                } => "in",
                RecordData::Report {
                    direction: Direction::Out,
                    ..
                } => "out",
            });
            crate::hid_log::publish(&self.source, record);
        }

        pub(super) fn read_timeout(&self, bytes: &mut [u8]) -> Result<usize, String> {
            let next = self.handle.io.lock().reads.pop_front();
            match next {
                Some(TestRead::Report(report)) => {
                    bytes[..REPORT_SIZE].copy_from_slice(&report);
                    self.publish(crate::hid_log::RecordData::Report {
                        direction: crate::hid_log::Direction::In,
                        bytes: crate::hid_log::ReportBytes::new(&report).unwrap(),
                        transferred: Some(REPORT_SIZE),
                        error: None,
                    });
                    Ok(REPORT_SIZE)
                }
                Some(TestRead::Cancel) => {
                    self.handle.cancel.store(true, Ordering::Release);
                    Ok(0)
                }
                None => Err("test receiver input exhausted".into()),
            }
        }

        pub(super) fn write(&self, bytes: &[u8]) -> Result<usize, String> {
            self.handle.io.lock().writes.push(bytes.try_into().unwrap());
            self.publish(crate::hid_log::RecordData::Report {
                direction: crate::hid_log::Direction::Out,
                bytes: crate::hid_log::ReportBytes::new(bytes).unwrap(),
                transferred: Some(bytes.len()),
                error: None,
            });
            Ok(bytes.len())
        }
    }

    impl Drop for TracedTestHandle {
        fn drop(&mut self) {
            self.publish(crate::hid_log::RecordData::Close);
        }
    }

    fn probe_receiver_open<'a>(
        receiver: &ReceiverEndpoint,
        interfaces: impl IntoIterator<Item = ReceiverInterface<'a>>,
        opened_serial: Result<Option<String>, String>,
        cancel_during_open: bool,
        events_enabled: bool,
    ) -> OpenProbe {
        let io = Arc::new(Mutex::new(TestIo::default()));
        {
            let mut script = io.lock();
            if events_enabled {
                for sequence in [1, 2] {
                    let mut ack = [0xf8; REPORT_SIZE];
                    ack[..6].copy_from_slice(&[251, sequence, 224, 0, 1, 1]);
                    ack[14] = 255;
                    ack[15] = 63;
                    script.reads.push_back(TestRead::Report(ack));
                }
            }
            let mut roster = battery_frame(2, 6, [200, 155, 144]);
            roster[16..24].copy_from_slice(&[255, 6, 1, 2, 3, 4, 5, 6]);
            script.reads.push_back(TestRead::Report(roster));
            script.reads.push_back(TestRead::Cancel);
            if events_enabled {
                let mut ack = [0xf8; REPORT_SIZE];
                ack[..6].copy_from_slice(&[251, 3, 224, 0, 1, 1]);
                ack[14] = 255;
                ack[15] = 0;
                script.reads.push_back(TestRead::Report(ack));
            }
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let state = Mutex::new(WorkerState::new(&receiver.key));
        state.lock().events.set_enabled(events_enabled);
        let result = with_receiver_handle(
            receiver,
            &cancel,
            interfaces.into_iter().map(|interface| {
                let usage_page = interface.usage_page;
                (interface, usage_page)
            }),
            |usage_page| {
                {
                    let mut io = io.lock();
                    io.opened += 1;
                    io.opened_usage_pages.push(usage_page);
                }
                cancel.store(cancel_during_open, Ordering::Release);
                Ok(TestHandle {
                    io: Arc::clone(&io),
                    cancel: Arc::clone(&cancel),
                })
            },
            |_| {
                io.lock().serial_queries += 1;
                opened_serial
            },
            |handle| read_receiver_reports(receiver, &cancel, &state, ReceiverDevice::Test(handle)),
        );
        let mut observation = ReceiverObservation::default();
        state.lock().publish(&mut observation);
        let slots = observation.trackers.iter().map(|link| link.slot).collect();
        let mut io = io.lock();
        OpenProbe {
            result,
            opened: io.opened,
            serial_queries: io.serial_queries,
            reader_entries: io.reader_entries,
            dropped: io.dropped,
            writes: std::mem::take(&mut io.writes),
            slots,
            trace: std::mem::take(&mut io.trace),
            opened_usage_pages: std::mem::take(&mut io.opened_usage_pages),
            observation,
        }
    }

    fn control_receiver_interface<'a>(
        receiver: &'a ReceiverEndpoint,
        serial: Option<&'a str>,
    ) -> ReceiverInterface<'a> {
        ReceiverInterface {
            path: &receiver.path,
            vid: 0x1209,
            pid: 0x7690,
            usage_page: 1,
            serial,
        }
    }

    fn no_reader(
        _: &ReceiverEndpoint,
        _: Arc<Mutex<WorkerState>>,
    ) -> std::io::Result<ReaderWorker> {
        panic!("no reader should start");
    }

    fn failing_reader(
        _: &ReceiverEndpoint,
        _: Arc<Mutex<WorkerState>>,
    ) -> std::io::Result<ReaderWorker> {
        Err(std::io::Error::other("test open failure"))
    }

    fn held_reader(
        _: &ReceiverEndpoint,
        state: Arc<Mutex<WorkerState>>,
    ) -> std::io::Result<ReaderWorker> {
        let cancel = Arc::new(AtomicBool::new(false));
        let handle = thread::spawn(move || {
            // Tests hold this lock until cancellation/reconciliation completes.
            state
                .lock()
                .set_error(Some("late result from removed reader".into()));
        });
        Ok(ReaderWorker { cancel, handle })
    }

    fn wait_until_finished(worker: &ReaderWorker) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !worker.handle.is_finished() {
            assert!(Instant::now() < deadline, "test reader failed to finish");
            thread::yield_now();
        }
    }

    fn gated_reader() -> (ReaderWorker, std::sync::mpsc::Sender<()>) {
        let (release, wait) = std::sync::mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let handle = thread::spawn(move || {
            wait.recv_timeout(Duration::from_secs(5)).unwrap();
        });
        (ReaderWorker { cancel, handle }, release)
    }

    #[test]
    fn reservation_visibility_is_read_only_and_matches_current_endpoint_identity() {
        let now = Instant::now();
        let endpoint = receiver(b"reservation-visibility");
        let mut monitor = ReceiverMonitor::new();
        monitor.entries.insert(
            endpoint.path.clone(),
            MonitorEntry::new(endpoint.clone(), now),
        );
        assert!(!monitor.is_reserved(&endpoint));
        let token = monitor.reserve(&endpoint).unwrap();
        assert!(monitor.is_reserved(&endpoint));
        for mismatched in [
            ReceiverEndpoint {
                path: b"different-path".to_vec(),
                ..endpoint.clone()
            },
            ReceiverEndpoint {
                key: "different-key".into(),
                ..endpoint.clone()
            },
            ReceiverEndpoint {
                serial_number: Some("different-serial".into()),
                ..endpoint.clone()
            },
        ] {
            assert!(!monitor.is_reserved(&mismatched));
        }
        // Reading the state neither consumes the lease nor admits another owner.
        assert!(monitor.reserve(&endpoint).is_err());
        monitor.release(&token).unwrap();
        assert!(!monitor.is_reserved(&endpoint));
    }

    #[test]
    fn reservation_visibility_tracks_unplug_and_replacement_until_release() {
        let now = Instant::now();
        let endpoint = receiver(b"replacement-reservation");
        let mut monitor = ReceiverMonitor::new();
        monitor.entries.insert(
            endpoint.path.clone(),
            MonitorEntry::new(endpoint.clone(), now),
        );
        let token = monitor.reserve(&endpoint).unwrap();
        monitor.reconcile_at(&[], now, no_reader);
        assert!(!monitor.is_reserved(&endpoint));
        let replacement = ReceiverEndpoint {
            serial_number: Some("replacement-serial".into()),
            ..endpoint.clone()
        };
        monitor.reconcile_at(std::slice::from_ref(&replacement), now, no_reader);
        assert!(!monitor.is_reserved(&endpoint));
        assert!(monitor.is_reserved(&replacement));
        assert!(monitor.reservation_ready(&token).is_err());
        monitor.release(&token).unwrap();
        assert!(!monitor.is_reserved(&replacement));
        let replacement_token = monitor.reserve(&replacement).unwrap();
        assert!(monitor.is_reserved(&replacement));
        assert!(monitor.shutdown().is_none());
        assert!(!monitor.is_reserved(&replacement));
        assert!(monitor.release(&replacement_token).is_err());
    }

    #[test]
    fn stop_all_events_revokes_disconnected_and_reserved_intents_before_reconnect_and_release() {
        let now = Instant::now();
        let receiver_a = receiver(b"stop-all-disconnected-a");
        let receiver_b = receiver(b"stop-all-reserved-b");
        let mut monitor = ReceiverMonitor::new();
        for endpoint in [&receiver_a, &receiver_b] {
            monitor.entries.insert(
                endpoint.path.clone(),
                MonitorEntry::new(endpoint.clone(), now),
            );
            monitor.set_events_enabled(endpoint, true).unwrap();
        }
        let reservation = monitor.reserve(&receiver_b).unwrap();
        monitor.reconcile_at(std::slice::from_ref(&receiver_b), now, no_reader);
        assert!(monitor.poll_at(now, no_reader));
        assert!(!monitor.entries[&receiver_a.path].wanted);
        assert_eq!(
            monitor.events_status(&receiver_b.key),
            EventWatchStatus::Paused
        );
        assert_eq!(monitor.requested_events_count(), 2);
        assert!(monitor.set_events_enabled(&receiver_a, true).is_err());
        let revisions: Vec<_> = [&receiver_a, &receiver_b]
            .iter()
            .map(|endpoint| monitor.entries[&endpoint.path].state.lock().revision)
            .collect();
        assert_eq!(monitor.stop_all_events(), 2);
        assert!(monitor.changed);
        assert_eq!(monitor.requested_events_count(), 0);
        for (endpoint, revision) in [&receiver_a, &receiver_b].into_iter().zip(revisions) {
            let state = monitor.entries[&endpoint.path].state.lock();
            assert!(!state.events.enabled);
            assert_eq!(state.events.status, EventWatchStatus::Disabled);
            assert_eq!(state.revision, revision.wrapping_add(1));
        }
        // The absent entry can now be forgotten without losing a pending intent.
        monitor.poll_at(now, no_reader);
        assert!(!monitor.entries.contains_key(&receiver_a.path));
        monitor.release(&reservation).unwrap();
        let resumed_at = monitor.entries[&receiver_b.path].retry_at;
        monitor.reconcile_at(
            &[receiver_a.clone(), receiver_b.clone()],
            resumed_at,
            failing_reader,
        );
        for endpoint in [&receiver_a, &receiver_b] {
            let state = monitor.entries[&endpoint.path].state.lock();
            assert!(!state.events.enabled);
            let mut watch = state.events.watch();
            assert!(
                watch.tick(resumed_at).is_none(),
                "no automatic query/subscribe after stop"
            );
        }
        assert_eq!(monitor.requested_events_count(), 0);
        // Only a new explicit, identity-checked start may resume one receiver.
        let mut changed_identity = receiver_a.clone();
        changed_identity.serial_number = Some("replacement".into());
        assert!(monitor.set_events_enabled(&changed_identity, true).is_err());
        monitor.set_events_enabled(&receiver_a, true).unwrap();
        assert!(monitor.events_enabled(&receiver_a.key));
        assert!(!monitor.events_enabled(&receiver_b.key));
    }

    #[test]
    fn stop_all_events_uses_existing_watch_unsubscribe_and_preserves_trace_recording_and_rosters() {
        use crate::hid_log::{self as trace, Direction, Recorder, Source, TraceItem};

        let now = Instant::now();
        let receiver_a = receiver(b"stop-all-connected-a");
        let receiver_b = receiver(b"stop-all-connected-b");
        let mut monitor = ReceiverMonitor::new();
        let mut watches = Vec::new();
        for endpoint in [&receiver_a, &receiver_b] {
            monitor.entries.insert(
                endpoint.path.clone(),
                MonitorEntry::new(endpoint.clone(), now),
            );
            monitor.set_events_enabled(endpoint, true).unwrap();
            let mut state = monitor.entries[&endpoint.path].state.lock();
            state.ingest(&frame(3, 4), now);
            let mut watch = state.events.watch();
            for action in [0, 1] {
                let command = watch.tick(now).unwrap();
                assert_eq!(command.bytes[6], action);
                let mut ack = [0; REPORT_SIZE];
                ack[..6].copy_from_slice(&[251, command.bytes[2], 224, 0, 1, 1]);
                ack[14] = 255;
                ack[15] = 63;
                watch.ingest(&ack, now);
            }
            state.publish_events(&watch);
            assert_eq!(state.events.status, EventWatchStatus::Active);
            watches.push(watch);
        }
        let directory = tempfile::tempdir().unwrap();
        let path = directory
            .path()
            .join("events-stop-recording-continues.hidlog");
        let recorder = Recorder::start(path.clone()).unwrap();
        let source = Arc::new(Source::main(&receiver_a));
        let mut log = trace::subscribe();
        assert_eq!(monitor.stop_all_events(), 2);
        assert_eq!(monitor.requested_events_count(), 0);
        for (endpoint, watch) in [&receiver_a, &receiver_b].into_iter().zip(&mut watches) {
            let state = monitor.entries[&endpoint.path].state.lock();
            assert_eq!(state.roster.links().len(), 1);
            assert_eq!(state.roster.links()[0].slot, 4);
            assert!(state.roster.links()[0].online);
            watch.sync(state.events.enabled, state.events.generation, false);
            let unsubscribe = watch.tick(now).unwrap();
            assert_eq!(&unsubscribe.bytes[3..9], &[224, 0, 1, 3, 255, 0]);
            let mut ack = [0; REPORT_SIZE];
            ack[..6].copy_from_slice(&[251, unsubscribe.bytes[2], 224, 0, 1, 1]);
            ack[14] = 255;
            watch.ingest(&ack, now);
            assert!(!watch.cleanup_pending());
            assert!(watch.tick(now + Duration::from_secs(60)).is_none());
        }
        assert!(
            !recorder.is_finished(),
            "subscription stop must not stop the file recorder"
        );
        // Observe publication on the independent trace consumer before stopping
        // the file consumer, even if another test briefly detaches a subscriber.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            trace::publish_report(&source, Direction::In, &frame(3, 9), Some(64), None).unwrap();
            if matches!(log.try_recv(), Some(TraceItem::Record(record))
                if record.source.connection_id == source.connection_id)
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "subscription stop detached trace logging"
            );
            thread::yield_now();
        }
        assert!(!recorder.is_finished());
        recorder.stop();
        let summary = recorder.join().unwrap();
        assert!(summary.reports > 0);
        let rows: Vec<serde_json::Value> = std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(rows.last().unwrap()["type"], "end");
        assert!(
            rows.iter()
                .any(|row| row["connection_id"] == source.connection_id
                    && row["data"]["bytes"] == serde_json::json!(frame(3, 9).to_vec()))
        );
        let revisions: Vec<_> = monitor
            .entries
            .values()
            .map(|entry| entry.state.lock().revision)
            .collect();
        monitor.changed = false;
        assert_eq!(monitor.stop_all_events(), 0);
        assert!(!monitor.changed);
        assert_eq!(
            monitor
                .entries
                .values()
                .map(|entry| entry.state.lock().revision)
                .collect::<Vec<_>>(),
            revisions
        );
    }

    #[test]
    fn event_intent_survives_reserved_reader_replacement_and_stale_state_is_detached() {
        let now = Instant::now();
        let endpoint = receiver(b"event-receiver");
        let mut entry = MonitorEntry::new(endpoint.clone(), now);
        let (worker, release) = gated_reader();
        entry.worker = Some(worker);
        let mut monitor = ReceiverMonitor::new();
        monitor.entries.insert(endpoint.path.clone(), entry);
        monitor.set_events_enabled(&endpoint, true).unwrap();
        let old = Arc::clone(&monitor.entries[&endpoint.path].state);
        old.lock().events.status = EventWatchStatus::Active;
        let token = monitor.reserve(&endpoint).unwrap();
        assert_eq!(
            monitor.events_status(&endpoint.key),
            EventWatchStatus::Paused
        );
        assert!(monitor.events_enabled(&endpoint.key));
        old.lock().events.set_enabled(false);
        old.lock().events.status = EventWatchStatus::Unsupported("stale failure".into());
        assert!(monitor.events_enabled(&endpoint.key));
        assert!(!monitor.reservation_ready(&token).unwrap());
        release.send(()).unwrap();
        wait_until_finished(monitor.entries[&endpoint.path].worker.as_ref().unwrap());
        assert!(monitor.reservation_ready(&token).unwrap());
        monitor.release(&token).unwrap();
        assert_eq!(
            monitor.events_status(&endpoint.key),
            EventWatchStatus::Checking
        );
        // Release schedules resumption at its own current instant, after `now`.
        // Poll at that actual deadline so the failing replacement is exercised.
        let resumed_at = monitor.entries[&endpoint.path].retry_at;
        monitor.poll_at(resumed_at, failing_reader);
        assert!(monitor.events_enabled(&endpoint.key));
        assert!(matches!(
            monitor.events_status(&endpoint.key),
            EventWatchStatus::Error(_)
        ));
        assert!(
            monitor.entries[&endpoint.path]
                .state
                .lock()
                .error
                .as_deref()
                .unwrap()
                .contains("test open failure")
        );
    }

    #[test]
    fn event_capability_failure_survives_restart_but_explicit_enable_retries() {
        let now = Instant::now();
        let endpoint = receiver(b"unsupported-events");
        let mut entry = MonitorEntry::new(endpoint.clone(), now);
        entry.state.lock().events.set_enabled(true);
        entry.state.lock().events.status = EventWatchStatus::Unsupported("older firmware".into());
        entry.cancel();
        entry.start_if_due(now, failing_reader);
        assert_eq!(
            entry.state.lock().events.status,
            EventWatchStatus::Unsupported("older firmware".into())
        );
        entry.state.lock().set_error(None);
        let mut monitor = ReceiverMonitor::new();
        monitor.entries.insert(endpoint.path.clone(), entry);
        assert!(matches!(
            monitor.events_status(&endpoint.key),
            EventWatchStatus::Unsupported(_)
        ));
        monitor.set_events_enabled(&endpoint, true).unwrap();
        assert_eq!(
            monitor.events_status(&endpoint.key),
            EventWatchStatus::Checking
        );
        let token = monitor.reserve(&endpoint).unwrap();
        monitor.set_events_enabled(&endpoint, false).unwrap();
        assert_eq!(
            monitor.events_status(&endpoint.key),
            EventWatchStatus::Disabled
        );
        monitor.release(&token).unwrap();
        assert!(!monitor.events_enabled(&endpoint.key));
    }

    #[test]
    fn event_enabled_intent_survives_unplug_and_reconnect_without_native_reader() {
        let now = Instant::now();
        let endpoint = receiver(b"reconnected-events");
        let mut monitor = ReceiverMonitor::new();
        monitor.entries.insert(
            endpoint.path.clone(),
            MonitorEntry::new(endpoint.clone(), now),
        );
        monitor.set_events_enabled(&endpoint, true).unwrap();
        monitor.reconcile_at(&[], now, no_reader);
        monitor.poll_at(now, no_reader);
        assert!(monitor.events_enabled(&endpoint.key));
        assert!(matches!(
            monitor.events_status(&endpoint.key),
            EventWatchStatus::Error(_)
        ));
        monitor.reconcile_at(std::slice::from_ref(&endpoint), now, failing_reader);
        assert!(monitor.events_enabled(&endpoint.key));
        assert_eq!(
            monitor.entries[&endpoint.path].state.lock().events.status,
            EventWatchStatus::Checking
        );
        assert_eq!(
            &monitor.entries[&endpoint.path]
                .state
                .lock()
                .events
                .watch()
                .tick(now)
                .unwrap()
                .bytes[..9],
            &[0, 254, 1, 224, 0, 1, 0, 255, 0]
        );
        let mut wrong_identity = endpoint.clone();
        wrong_identity.serial_number = Some("other".into());
        assert!(monitor.set_events_enabled(&wrong_identity, false).is_err());
    }

    fn assert_replacement_revokes_event_intent(absent_first: bool) {
        let now = Instant::now();
        let endpoint = receiver(b"reused-event-path");
        for replacement in [
            ReceiverEndpoint {
                serial_number: Some("replacement-serial".into()),
                ..endpoint.clone()
            },
            ReceiverEndpoint {
                key: "replacement-logical-key".into(),
                ..endpoint.clone()
            },
        ] {
            let mut entry = MonitorEntry::new(endpoint.clone(), now);
            let (worker, release) = gated_reader();
            let old_thread = worker.handle.thread().id();
            entry.worker = Some(worker);
            let mut monitor = ReceiverMonitor::new();
            monitor.entries.insert(endpoint.path.clone(), entry);
            monitor.set_events_enabled(&endpoint, true).unwrap();
            let old_state = Arc::clone(&monitor.entries[&endpoint.path].state);
            {
                let mut state = old_state.lock();
                state.ingest(&frame(1, 3), now);
                assert_eq!(
                    &state.events.watch().tick(now).unwrap().bytes[..9],
                    &[0, 254, 1, 224, 0, 1, 0, 255, 0]
                );
            }
            monitor.poll_at(now, no_reader);
            assert_eq!(
                monitor.observation(&endpoint.key).unwrap().trackers[0].slot,
                3
            );
            if absent_first {
                monitor.reconcile_at(&[], now, no_reader);
                monitor.poll_at(now, no_reader);
                assert_eq!(monitor.requested_events_count(), 1);
            }
            monitor.reconcile_at(std::slice::from_ref(&replacement), now, no_reader);
            assert_eq!(monitor.requested_events_count(), 0);
            assert_eq!(
                monitor.events_status(&replacement.key),
                EventWatchStatus::Disabled
            );
            assert!(
                monitor.entries[&endpoint.path]
                    .state
                    .lock()
                    .events
                    .watch()
                    .tick(now)
                    .is_none()
            );
            let current_worker = monitor.entries[&endpoint.path].worker.as_ref().unwrap();
            assert_eq!(current_worker.handle.thread().id(), old_thread);
            assert!(current_worker.cancel.load(Ordering::Acquire));
            {
                let mut stale = old_state.lock();
                stale.ingest(&frame(1, 9), now);
                stale.events.status = EventWatchStatus::Active;
                stale.set_error(Some("late old receiver publication".into()));
            }
            monitor.entries[&endpoint.path]
                .state
                .lock()
                .ingest(&frame(1, 4), now);
            monitor.poll_at(now, no_reader);
            let observation = monitor.observation_for(&replacement).unwrap();
            assert_eq!(
                observation
                    .trackers
                    .iter()
                    .map(|link| (link.receiver_key.as_str(), link.slot))
                    .collect::<Vec<_>>(),
                [(replacement.key.as_str(), 4)]
            );
            assert_eq!(observation.error, None);
            assert_eq!(
                monitor.events_status(&replacement.key),
                EventWatchStatus::Disabled
            );
            assert!(monitor.set_events_enabled(&endpoint, true).is_err());
            monitor.set_events_enabled(&replacement, true).unwrap();
            assert_eq!(
                &monitor.entries[&endpoint.path]
                    .state
                    .lock()
                    .events
                    .watch()
                    .tick(now)
                    .unwrap()
                    .bytes[..9],
                &[0, 254, 1, 224, 0, 1, 0, 255, 0]
            );
            release.send(()).unwrap();
            monitor.shutdown().unwrap().join().unwrap();
        }
    }

    #[test]
    fn event_opt_in_does_not_follow_direct_receiver_identity_replacement() {
        assert_replacement_revokes_event_intent(false);
    }

    #[test]
    fn event_opt_in_does_not_follow_absent_receiver_identity_replacement() {
        assert_replacement_revokes_event_intent(true);
    }

    #[test]
    fn event_status_changes_trigger_monitor_publication_without_roster_changes() {
        let now = Instant::now();
        let endpoint = receiver(b"status-events");
        let mut monitor = ReceiverMonitor::new();
        let mut entry = MonitorEntry::new(endpoint.clone(), now);
        let (worker, release) = gated_reader();
        entry.worker = Some(worker);
        monitor.entries.insert(endpoint.path.clone(), entry);
        monitor.set_events_enabled(&endpoint, true).unwrap();
        assert!(monitor.poll_at(now, no_reader));
        assert!(!monitor.poll_at(now, no_reader));
        {
            let mut state = monitor.entries[&endpoint.path].state.lock();
            let mut watch = state.events.watch();
            let query = watch.tick(now).unwrap();
            let mut report = [0; 64];
            report[..6].copy_from_slice(&[251, query.bytes[2], 224, 5, 1, 1]);
            watch.ingest(&report, now);
            state.publish_events(&watch);
        }
        assert!(monitor.poll_at(now, no_reader));
        assert!(matches!(
            monitor.events_status(&endpoint.key),
            EventWatchStatus::Unsupported(_)
        ));
        release.send(()).unwrap();
        if let Some(reaper) = monitor.shutdown() {
            reaper.join().unwrap();
        }
    }

    #[test]
    fn receiver_handoff_waits_for_join_blocks_reconcile_and_keeps_other_reader() {
        let now = Instant::now();
        let endpoint = receiver(b"receiver");
        let other = receiver(b"other");
        let mut entry = MonitorEntry::new(endpoint.clone(), now);
        let old_state = Arc::clone(&entry.state);
        {
            let mut state = old_state.lock();
            state.ingest(&battery_frame(2, 9, [200, 155, 144]), now);
            assert!(state.publish(&mut entry.observation));
            assert_eq!(
                entry.observation.battery(9),
                Some(TrackerBattery {
                    percent: 72,
                    voltage_v: 4.0,
                })
            );
        }
        let (worker, release) = gated_reader();
        entry.worker = Some(worker);
        let mut other_entry = MonitorEntry::new(other.clone(), now);
        let (other_worker, other_release) = gated_reader();
        let other_thread = other_worker.handle.thread().id();
        other_entry.worker = Some(other_worker);
        let mut monitor = ReceiverMonitor::new();
        monitor.entries.insert(endpoint.path.clone(), entry);
        monitor.entries.insert(other.path.clone(), other_entry);
        let token = monitor.reserve(&endpoint).unwrap();
        assert_eq!(monitor.observation(&endpoint.key).unwrap().battery(9), None);
        assert_eq!(token.endpoint().path, endpoint.path);
        assert!(monitor.reserve(&endpoint).is_err());
        let mut renamed_key = endpoint.clone();
        renamed_key.key = "different-logical-key".into();
        assert!(monitor.reserve(&renamed_key).is_err());
        assert!(!monitor.reservation_ready(&token).unwrap());
        monitor.reconcile_at(
            &[endpoint.clone(), endpoint.clone(), other.clone()],
            now,
            no_reader,
        );
        old_state
            .lock()
            .ingest(&battery_frame(0, 9, [255, 180, 144]), now);
        monitor.entries[&other.path]
            .state
            .lock()
            .ingest(&battery_frame(2, 3, [200, 155, 144]), now);
        monitor.poll_at(now, no_reader);
        assert!(
            monitor
                .observation(&endpoint.key)
                .unwrap()
                .trackers
                .is_empty()
        );
        assert_eq!(monitor.observation(&endpoint.key).unwrap().battery(9), None);
        assert_eq!(monitor.observation(&other.key).unwrap().trackers[0].slot, 3);
        assert_eq!(
            monitor.observation(&other.key).unwrap().battery(3),
            Some(TrackerBattery {
                percent: 72,
                voltage_v: 4.0,
            })
        );
        assert_eq!(
            monitor.entries[&other.path]
                .worker
                .as_ref()
                .unwrap()
                .handle
                .thread()
                .id(),
            other_thread
        );
        assert!(
            !monitor.entries[&other.path]
                .worker
                .as_ref()
                .unwrap()
                .cancel
                .load(Ordering::Acquire)
        );
        release.send(()).unwrap();
        wait_until_finished(monitor.entries[&endpoint.path].worker.as_ref().unwrap());
        assert!(monitor.reservation_ready(&token).unwrap());
        assert!(monitor.entries[&endpoint.path].worker.is_none());
        monitor.reconcile_at(&[endpoint.clone(), other.clone()], now, no_reader);
        monitor.release(&token).unwrap();
        let replacement = monitor.reserve(&endpoint).unwrap();
        assert!(monitor.release(&token).is_err());
        assert!(monitor.reservation_ready(&replacement).unwrap());
        monitor.release(&replacement).unwrap();
        // Releasing resumes the normal reader open path, not an OTA-owned handle.
        monitor.poll_at(Instant::now(), failing_reader);
        assert!(
            monitor
                .observation(&endpoint.key)
                .unwrap()
                .error
                .as_ref()
                .unwrap()
                .contains("test open failure")
        );
        other_release.send(()).unwrap();
        if let Some(reaper) = monitor.shutdown() {
            reaper.join().unwrap();
        }
        monitor.reconcile_at(&[endpoint], now, no_reader);
        assert!(monitor.entries.is_empty());
        assert!(monitor.reservation_ready(&replacement).is_err());
    }

    #[test]
    fn receiver_cancelled_handoff_keeps_late_reader_and_invalidates_reappeared_identity() {
        let now = Instant::now();
        let endpoint = receiver(b"native-path");
        let mut entry = MonitorEntry::new(endpoint.clone(), now);
        let (worker, release) = gated_reader();
        entry.worker = Some(worker);
        let mut monitor = ReceiverMonitor::new();
        monitor.entries.insert(endpoint.path.clone(), entry);
        let cancelled = monitor.reserve(&endpoint).unwrap();
        monitor.release(&cancelled).unwrap();
        monitor.reconcile_at(std::slice::from_ref(&endpoint), now, no_reader);
        monitor.poll_at(now, no_reader);
        let token = monitor.reserve(&endpoint).unwrap();
        monitor.reconcile_at(&[], now, no_reader);
        assert!(monitor.reservation_ready(&token).is_err());
        monitor.reconcile_at(std::slice::from_ref(&endpoint), now, no_reader);
        // Even identical USB metadata after a disconnect cannot revive a token.
        assert!(monitor.reservation_ready(&token).is_err());
        release.send(()).unwrap();
        wait_until_finished(monitor.entries[&endpoint.path].worker.as_ref().unwrap());
        monitor.poll_at(now, no_reader);
        assert!(monitor.entries[&endpoint.path].worker.is_none());
        monitor.release(&token).unwrap();
        let token = monitor.reserve(&endpoint).unwrap();
        let mut replacement = endpoint.clone();
        replacement.serial_number = Some("different-device".into());
        monitor.reconcile_at(&[replacement], now, no_reader);
        assert!(monitor.reservation_ready(&token).is_err());
        monitor.release(&token).unwrap();
        assert!(monitor.reserve(&endpoint).is_err());
        assert!(monitor.shutdown().is_none());
        assert!(monitor.reserve(&endpoint).is_err());
    }

    #[test]
    fn receiver_reservations_are_scoped_to_monitor_not_logical_key() {
        let now = Instant::now();
        let endpoint = receiver(b"native-path");
        let mut first = ReceiverMonitor::new();
        let mut second = ReceiverMonitor::new();
        first.entries.insert(
            endpoint.path.clone(),
            MonitorEntry::new(endpoint.clone(), now),
        );
        second.entries.insert(
            endpoint.path.clone(),
            MonitorEntry::new(endpoint.clone(), now),
        );
        let first_token = first.reserve(&endpoint).unwrap();
        let second_token = second.reserve(&endpoint).unwrap();
        assert!(second.release(&first_token).is_err());
        assert!(first.reservation_ready(&second_token).is_err());
        assert!(first.reservation_ready(&first_token).unwrap());
        assert!(second.reservation_ready(&second_token).unwrap());
        first.release(&first_token).unwrap();
        second.release(&second_token).unwrap();
    }

    #[test]
    fn tps_counts_each_packed_sensor_record_but_not_registration_or_invalid_data() {
        let now = Instant::now();
        let mut roster = Roster::new("receiver");
        for first_kind in [0, 4] {
            let mut report = [0xf8; REPORT_SIZE];
            for (index, record) in report
                .as_chunks_mut::<RECORD_SIZE>()
                .0
                .iter_mut()
                .enumerate()
            {
                record[0] = first_kind + index as u8;
                record[1] = 3;
            }
            roster.ingest_at(&report, now);
        }
        roster.ingest_at(&frame(0xff, 3), now);
        roster.ingest_at(&frame(0xff, 4), now);
        for kind in 8..=254 {
            roster.ingest_at(&frame(kind, 3), now);
        }
        for slot in 16..=255 {
            roster.ingest_at(&frame(0, slot), now);
        }
        let sensor = frame(0, 3);
        for length in 0..REPORT_SIZE {
            roster.ingest_at(&sensor[..length], now);
        }
        roster.ingest_at(&[0; REPORT_SIZE + 1], now);
        assert!(!roster.expire(now + Duration::from_millis(999)));
        assert_eq!(roster.rates(), [0; SLOT_COUNT]);
        assert!(roster.expire(now + TPS_INTERVAL));
        let observation = ReceiverObservation {
            trackers: roster.links(),
            rates: roster.rates(),
            batteries: roster.batteries(),
            error: None,
        };
        assert_eq!(observation.tps(3), 8);
        assert_eq!(observation.tps(4), 0);
        assert_eq!(observation.tps(0), 0);
        assert_eq!(observation.tps(16), 0);
        assert_eq!(observation.tps(255), 0);
        assert_eq!(observation.trackers.len(), 2);
        assert!(observation.trackers[0].online);
        assert!(!observation.trackers[1].online);
    }

    #[test]
    fn tps_uses_actual_elapsed_windows_and_does_not_reset_on_early_polls() {
        let now = Instant::now();
        let mut roster = Roster::new("receiver");
        for _ in 0..3 {
            roster.ingest_at(&frame(1, 2), now);
        }
        assert!(!roster.expire(now + Duration::from_millis(900)));
        assert!(roster.expire(now + Duration::from_millis(1500)));
        assert_eq!(roster.rates()[2], 2);
        for _ in 0..5 {
            roster.ingest_at(&frame(7, 2), now + Duration::from_millis(1600));
        }
        assert!(!roster.expire(now + Duration::from_millis(2400)));
        assert_eq!(roster.rates()[2], 2);
        assert!(roster.expire(now + Duration::from_millis(2750)));
        assert_eq!(roster.rates()[2], 4);
        assert!(!roster.expire(now + Duration::from_millis(2750)));
    }

    #[test]
    fn tps_silence_zeroes_rate_before_liveness_expires() {
        let now = Instant::now();
        let mut roster = Roster::new("receiver");
        roster.ingest_at(&frame(1, 2), now);
        assert!(roster.expire(now + TPS_INTERVAL));
        assert_eq!(roster.rates()[2], 1);
        assert!(roster.expire(now + 2 * TPS_INTERVAL));
        assert_eq!(roster.rates()[2], 0);
        assert!(roster.links()[0].online);
        assert!(roster.expire(now + TRAFFIC_TIMEOUT));
        assert!(!roster.links()[0].online);
        assert!(!roster.expire(now + 4 * TPS_INTERVAL));
        roster.ingest_at(&frame(2, 2), now + 10 * TPS_INTERVAL);
        assert!(roster.expire(now + 11 * TPS_INTERVAL));
        assert_eq!(roster.rates()[2], 1);
    }

    #[test]
    fn offline_boundary_clears_tps_even_before_next_sample_is_due() {
        let now = Instant::now();
        let mut roster = Roster::new("receiver");
        for _ in 0..20 {
            roster.ingest_at(&frame(1, 2), now);
        }
        assert!(roster.expire(now + Duration::from_millis(2900)));
        assert_eq!(roster.rates()[2], 6);
        assert!(roster.expire(now + TRAFFIC_TIMEOUT));
        assert_eq!(roster.rates()[2], 0);
        assert!(!roster.links()[0].online);
        assert!(!roster.expire(now + Duration::from_secs(4)));
    }

    #[test]
    fn reassignment_resets_sampled_telemetry_and_pending_sensor_counts() {
        let now = Instant::now();
        let mut roster = Roster::new("receiver");
        for _ in 0..4 {
            roster.ingest_at(&battery_frame(2, 4, [200, 155, 144]), now);
        }
        assert!(roster.expire(now + TPS_INTERVAL));
        let mut registration = frame(0xff, 4);
        assert!(roster.ingest_at(&registration, now + TPS_INTERVAL));
        assert_eq!(roster.rates()[4], 4, "first registration preserves traffic");
        let battery = Some(TrackerBattery {
            percent: 72,
            voltage_v: 4.0,
        });
        assert_eq!(roster.batteries()[4], battery);
        assert!(!roster.ingest_at(&frame(1, 4), now + Duration::from_millis(1100)));
        assert!(!roster.ingest_at(&registration, now + Duration::from_millis(1100)));
        assert_eq!(roster.batteries()[4], battery);
        registration[2] = 1;
        assert!(roster.ingest_at(&registration, now + Duration::from_millis(1200)));
        assert_eq!(roster.rates()[4], 0);
        assert!(!roster.links()[0].online);
        assert_eq!(roster.batteries()[4], None);
        assert!(!roster.expire(now + Duration::from_millis(2200)));
        assert!(roster.ingest_at(&frame(7, 4), now + Duration::from_millis(2300)));
        assert_eq!(roster.batteries()[4], None);
        assert!(roster.expire(now + Duration::from_millis(3300)));
        assert_eq!(roster.rates()[4], 1);
    }

    #[test]
    fn battery_changes_publish_without_cloning_identities_or_redrawing_other_telemetry() {
        let now = Instant::now();
        let mut state = WorkerState::new("receiver");
        let mut report = battery_frame(0, 3, [128, 0, 144]);
        report[16..24].copy_from_slice(&[255, 3, 1, 2, 3, 4, 5, 6]);
        state.ingest(&report, now);
        let mut observation = ReceiverObservation::default();
        assert!(state.publish(&mut observation));
        let empty_battery = Some(TrackerBattery {
            percent: 0,
            voltage_v: 2.45,
        });
        assert_eq!(observation.battery(3), empty_battery);
        assert_eq!(observation.battery(0), None);
        assert_eq!(observation.battery(16), None);
        assert_eq!(observation.battery(255), None);
        assert_eq!(
            observation.trackers[0].address.as_deref(),
            Some("060504030201")
        );
        let identities = (
            observation.trackers.as_ptr(),
            observation.trackers[0].receiver_key.as_ptr(),
            observation.trackers[0].address.as_ref().unwrap().as_ptr(),
        );
        let revision = state.revision;
        for report in [
            battery_frame(0, 3, [128, 0, 144]),
            battery_frame(0, 3, [128, 0, 145]),
            battery_frame(2, 3, [128, 0, 0]),
            battery_frame(6, 3, [0, 0, 0]),
            battery_frame(7, 3, [255, 255, 255]),
        ] {
            state.ingest(&report, now);
            assert_eq!(state.revision, revision);
            assert!(!state.publish(&mut observation));
            assert_eq!(observation.battery(3), empty_battery);
        }
        for (report, expected) in [
            (
                battery_frame(2, 3, [200, 155, 144]),
                Some(TrackerBattery {
                    percent: 72,
                    voltage_v: 4.0,
                }),
            ),
            (
                battery_frame(0, 3, [255, 180, 144]),
                Some(TrackerBattery {
                    percent: 100,
                    voltage_v: 4.25,
                }),
            ),
            (battery_frame(2, 3, [0, 180, 144]), None),
        ] {
            let revision = state.revision;
            state.ingest(&report, now);
            assert_eq!(state.revision, revision.wrapping_add(1));
            assert!(state.publish(&mut observation));
            assert_eq!(observation.battery(3), expected);
            assert_eq!(
                (
                    observation.trackers.as_ptr(),
                    observation.trackers[0].receiver_key.as_ptr(),
                    observation.trackers[0].address.as_ref().unwrap().as_ptr(),
                ),
                identities
            );
        }
        let revision = state.revision;
        state.ingest(&battery_frame(0, 3, [127, 0, 0]), now);
        assert_eq!(state.revision, revision);
        assert!(!state.publish(&mut observation));
        assert_eq!(observation.battery(3), None);
    }

    #[test]
    fn battery_observations_isolate_receiver_slots_and_suppress_reader_errors() {
        let now = Instant::now();
        let endpoints = [receiver(b"first"), receiver(b"second")];
        let mut monitor = ReceiverMonitor::new();
        for (endpoint, telemetry) in endpoints.iter().zip([[128, 155, 144], [255, 180, 144]]) {
            let mut entry = MonitorEntry::new(endpoint.clone(), now);
            entry.retry_at = now + Duration::from_secs(3600);
            entry
                .state
                .lock()
                .ingest(&battery_frame(2, 3, telemetry), now);
            monitor.entries.insert(endpoint.path.clone(), entry);
        }
        assert!(monitor.poll_at(now, no_reader));
        assert_eq!(
            monitor.observation_for(&endpoints[0]).unwrap().battery(3),
            Some(TrackerBattery {
                percent: 0,
                voltage_v: 4.0,
            })
        );
        let full_battery = Some(TrackerBattery {
            percent: 100,
            voltage_v: 4.25,
        });
        assert_eq!(
            monitor.observation_for(&endpoints[1]).unwrap().battery(3),
            full_battery
        );
        monitor.entries[&endpoints[0].path]
            .state
            .lock()
            .set_error(Some("reader failed".into()));
        assert!(monitor.poll_at(now, no_reader));
        let failed = monitor.observation_for(&endpoints[0]).unwrap();
        assert!(failed.trackers[0].online);
        assert_eq!(failed.error.as_deref(), Some("reader failed"));
        assert_eq!(failed.battery(3), None);
        assert_eq!(
            monitor.observation_for(&endpoints[1]).unwrap().battery(3),
            full_battery
        );
        let mut replaced = endpoints[1].clone();
        replaced.serial_number = Some("replacement".into());
        assert!(monitor.observation_for(&replaced).is_none());
    }

    #[test]
    fn only_changed_sampled_rates_publish_not_each_packet_or_unchanged_rate() {
        let now = Instant::now();
        let endpoint = receiver(b"receiver");
        let mut entry = MonitorEntry::new(endpoint.clone(), now);
        entry.retry_at = now + Duration::from_secs(3600);
        let state = Arc::clone(&entry.state);
        state.lock().ingest(&frame(1, 3), now);
        let mut monitor = ReceiverMonitor::new();
        monitor.entries.insert(endpoint.path.clone(), entry);
        assert!(monitor.poll_at(now, no_reader));
        let revision = state.lock().revision;
        state
            .lock()
            .ingest(&frame(7, 3), now + Duration::from_millis(500));
        assert_eq!(state.lock().revision, revision);
        assert!(!monitor.poll_at(now + Duration::from_millis(999), no_reader));
        assert!(monitor.poll_at(now + TPS_INTERVAL, no_reader));
        assert_eq!(monitor.observation(&endpoint.key).unwrap().tps(3), 2);
        assert_eq!(state.lock().revision, revision.wrapping_add(1));
        let revision = state.lock().revision;
        for millis in [1200, 1800] {
            state
                .lock()
                .ingest(&frame(7, 3), now + Duration::from_millis(millis));
            assert_eq!(state.lock().revision, revision);
            assert!(!monitor.poll_at(now + Duration::from_millis(millis), no_reader));
        }
        assert!(!monitor.poll_at(now + 2 * TPS_INTERVAL, no_reader));
        assert_eq!(state.lock().revision, revision);
        assert_eq!(monitor.observation(&endpoint.key).unwrap().tps(3), 2);
        assert!(monitor.poll_at(now + 3 * TPS_INTERVAL, no_reader));
        assert_eq!(monitor.observation(&endpoint.key).unwrap().tps(3), 0);
        assert!(monitor.observation(&endpoint.key).unwrap().trackers[0].online);
        assert!(!monitor.poll_at(now + 4 * TPS_INTERVAL, no_reader));
        assert!(monitor.poll_at(now + Duration::from_millis(4800), no_reader));
        assert!(!monitor.observation(&endpoint.key).unwrap().trackers[0].online);
    }

    #[test]
    fn reader_restart_drops_sampled_telemetry_and_unpublished_counts() {
        let now = Instant::now();
        let endpoint = receiver(b"receiver");
        let mut entry = MonitorEntry::new(endpoint.clone(), now);
        let old_state = Arc::clone(&entry.state);
        {
            let mut state = old_state.lock();
            state.ingest(&battery_frame(2, 3, [200, 155, 144]), now - TPS_INTERVAL);
            assert!(state.roster.expire(now));
            assert_eq!(state.roster.rates()[3], 1);
            state.ingest(&battery_frame(2, 3, [255, 180, 144]), now);
            assert!(state.publish(&mut entry.observation));
            assert_eq!(
                entry.observation.battery(3),
                Some(TrackerBattery {
                    percent: 100,
                    voltage_v: 4.25,
                })
            );
        }
        let mut monitor = ReceiverMonitor::new();
        monitor.entries.insert(endpoint.path.clone(), entry);
        assert!(monitor.poll_at(now, failing_reader));
        assert_eq!(monitor.observation(&endpoint.key).unwrap().tps(3), 0);
        assert_eq!(monitor.observation(&endpoint.key).unwrap().battery(3), None);
        assert!(
            monitor
                .observation(&endpoint.key)
                .unwrap()
                .trackers
                .is_empty()
        );
        assert_eq!(
            monitor.entries[&endpoint.path].state.lock().roster.rates(),
            [0; SLOT_COUNT]
        );
        assert_eq!(
            monitor.entries[&endpoint.path]
                .state
                .lock()
                .roster
                .batteries(),
            [None; SLOT_COUNT]
        );
        old_state
            .lock()
            .ingest(&battery_frame(2, 3, [255, 180, 144]), now);
        assert!(!monitor.poll_at(now + TPS_INTERVAL, no_reader));
        assert_eq!(monitor.observation(&endpoint.key).unwrap().battery(3), None);
    }

    #[test]
    fn traffic_refreshes_expiry_without_triggering_topology_redraws() {
        let now = Instant::now();
        let endpoint = receiver(b"receiver");
        let mut entry = MonitorEntry::new(endpoint.clone(), now);
        entry.retry_at = now + Duration::from_secs(3600);
        entry
            .state
            .lock()
            .ingest(&battery_frame(0, 3, [200, 155, 144]), now);
        let mut monitor = ReceiverMonitor::new();
        monitor.entries.insert(endpoint.path.clone(), entry);
        assert!(monitor.poll_at(now, no_reader));
        assert_eq!(
            monitor.observation(&endpoint.key).unwrap().battery(3),
            Some(TrackerBattery {
                percent: 72,
                voltage_v: 4.0,
            })
        );
        let state = Arc::clone(&monitor.entries[&endpoint.path].state);
        let revision = state.lock().revision;
        state.lock().ingest(
            &battery_frame(2, 3, [200, 155, 144]),
            now + Duration::from_secs(2),
        );
        assert_eq!(state.lock().revision, revision);
        assert!(!monitor.poll_at(now + TRAFFIC_TIMEOUT, no_reader));
        assert!(monitor.observation(&endpoint.key).unwrap().trackers[0].online);
        assert!(monitor.poll_at(now + Duration::from_secs(5), no_reader));
        assert!(!monitor.observation(&endpoint.key).unwrap().trackers[0].online);
        assert_eq!(monitor.observation(&endpoint.key).unwrap().battery(3), None);
        assert!(!monitor.poll_at(now + Duration::from_secs(6), no_reader));
        state
            .lock()
            .ingest(&frame(7, 3), now + Duration::from_secs(7));
        assert!(monitor.poll_at(now + Duration::from_secs(7), no_reader));
        assert!(monitor.observation(&endpoint.key).unwrap().trackers[0].online);
        assert_eq!(monitor.observation(&endpoint.key).unwrap().battery(3), None);
    }

    #[test]
    fn address_reassignment_cannot_inherit_old_tracker_liveness() {
        let now = Instant::now();
        let mut roster = Roster::new("receiver");
        let mut registration = frame(0xff, 4);
        registration[2..8].copy_from_slice(&[6, 5, 4, 3, 2, 1]);
        assert!(roster.ingest_at(&registration, now));
        assert!(!roster.links()[0].online);
        assert!(roster.ingest_at(&frame(0, 4), now));
        assert!(!roster.ingest_at(&registration, now + Duration::from_secs(1)));
        assert!(roster.links()[0].online);
        registration[2] = 7;
        assert!(roster.ingest_at(&registration, now + Duration::from_secs(2)));
        let links = roster.links();
        assert_eq!(links[0].address.as_deref(), Some("010203040507"));
        assert!(!links[0].online);
        assert!(!roster.expire(now + Duration::from_secs(3)));
        assert!(roster.ingest_at(&frame(1, 4), now + Duration::from_secs(4)));
        assert!(roster.links()[0].online);
    }

    #[test]
    fn registration_never_refreshes_traffic_expiration() {
        let now = Instant::now();
        let mut roster = Roster::new("receiver");
        let registration = frame(0xff, 2);
        roster.ingest_at(&registration, now);
        roster.ingest_at(&frame(1, 2), now);
        assert!(!roster.ingest_at(&registration, now + Duration::from_secs(2)));
        assert!(roster.expire(now + TRAFFIC_TIMEOUT));
        assert!(!roster.links()[0].online);
        assert!(!roster.ingest_at(&registration, now + Duration::from_secs(4)));
    }

    #[test]
    fn retries_are_bounded_and_identical_errors_do_not_trigger_redraws() {
        let now = Instant::now();
        let endpoint = receiver(b"receiver");
        let mut monitor = ReceiverMonitor::new();
        monitor.reconcile_at(std::slice::from_ref(&endpoint), now, failing_reader);
        assert!(monitor.poll_at(now, no_reader));
        assert!(
            monitor
                .observation(&endpoint.key)
                .unwrap()
                .error
                .as_ref()
                .unwrap()
                .contains("test open failure")
        );
        let original = Arc::clone(&monitor.entries[&endpoint.path].state);
        monitor.reconcile_at(
            std::slice::from_ref(&endpoint),
            now + Duration::from_secs(2),
            no_reader,
        );
        assert!(!monitor.poll_at(now + Duration::from_secs(2), no_reader));
        assert!(Arc::ptr_eq(
            &original,
            &monitor.entries[&endpoint.path].state
        ));
        assert!(!monitor.poll_at(now + RETRY_INTERVAL, failing_reader));
        assert!(!Arc::ptr_eq(
            &original,
            &monitor.entries[&endpoint.path].state
        ));
        monitor.reconcile_at(&[], now + RETRY_INTERVAL, no_reader);
        assert!(monitor.observation(&endpoint.key).is_none());
        assert!(monitor.poll_at(now + RETRY_INTERVAL, no_reader));
        assert!(monitor.entries.is_empty());
        assert!(!monitor.poll_at(now + RETRY_INTERVAL, no_reader));
    }

    #[test]
    fn removal_and_reappearance_never_overlap_readers_or_publish_stale_updates() {
        let now = Instant::now();
        let endpoint = receiver(b"receiver");
        let mut entry = MonitorEntry::new(endpoint.clone(), now);
        let old_state = Arc::clone(&entry.state);
        {
            let mut guard = old_state.lock();
            guard.ingest(&battery_frame(2, 1, [200, 155, 144]), now - TPS_INTERVAL);
            assert!(guard.roster.expire(now));
            assert_eq!(guard.roster.rates()[1], 1);
            guard.ingest(&frame(1, 1), now);
            assert!(guard.publish(&mut entry.observation));
            assert_eq!(
                entry.observation.battery(1),
                Some(TrackerBattery {
                    percent: 72,
                    voltage_v: 4.0,
                })
            );
        }
        let (worker, release) = gated_reader();
        entry.worker = Some(worker);
        let old_thread = entry.worker.as_ref().unwrap().handle.thread().id();
        let mut monitor = ReceiverMonitor::new();
        monitor.entries.insert(endpoint.path.clone(), entry);
        monitor.reconcile_at(&[], now, no_reader);
        assert!(monitor.observation(&endpoint.key).is_none());
        assert!(
            monitor.entries[&endpoint.path]
                .worker
                .as_ref()
                .unwrap()
                .cancel
                .load(Ordering::Acquire)
        );
        // A duplicate inventory record still cannot start a second reader.
        monitor.reconcile_at(&[endpoint.clone(), endpoint.clone()], now, no_reader);
        assert!(monitor.poll_at(now, no_reader));
        assert_eq!(
            monitor.entries[&endpoint.path]
                .worker
                .as_ref()
                .unwrap()
                .handle
                .thread()
                .id(),
            old_thread
        );
        {
            let mut guard = old_state.lock();
            guard.ingest(&battery_frame(0, 9, [255, 180, 144]), now);
            guard.set_error(Some("stale worker error".into()));
        }
        assert!(!monitor.poll_at(now, no_reader));
        let observation = monitor.observation(&endpoint.key).unwrap();
        assert!(observation.trackers.is_empty());
        assert!(observation.error.is_none());
        assert_eq!(observation.rates, [0; SLOT_COUNT]);
        assert_eq!(observation.batteries, [None; SLOT_COUNT]);
        assert_eq!(
            monitor.entries[&endpoint.path].state.lock().roster.rates(),
            [0; SLOT_COUNT]
        );
        assert_eq!(
            monitor.entries[&endpoint.path]
                .state
                .lock()
                .roster
                .batteries(),
            [None; SLOT_COUNT]
        );
        release.send(()).unwrap();
        wait_until_finished(monitor.entries[&endpoint.path].worker.as_ref().unwrap());
        assert!(monitor.poll_at(now, failing_reader));
        assert!(monitor.entries[&endpoint.path].worker.is_none());
        assert!(!Arc::ptr_eq(
            &old_state,
            &monitor.entries[&endpoint.path].state
        ));
        assert!(
            !monitor
                .observation(&endpoint.key)
                .unwrap()
                .error
                .as_ref()
                .unwrap()
                .contains("stale")
        );
        assert!(!monitor.poll_at(now + Duration::from_secs(1), no_reader));
    }

    #[test]
    fn completed_reader_errors_wait_before_retrying_and_shutdown_reaps_cancelled_reader() {
        let now = Instant::now();
        let endpoint = receiver(b"receiver");
        let mut entry = MonitorEntry::new(endpoint.clone(), now);
        let state = Arc::clone(&entry.state);
        let guard = state.lock();
        entry.worker = Some(held_reader(&endpoint, Arc::clone(&state)).unwrap());
        let mut monitor = ReceiverMonitor::new();
        monitor.entries.insert(endpoint.path.clone(), entry);
        drop(guard);
        wait_until_finished(monitor.entries[&endpoint.path].worker.as_ref().unwrap());
        assert!(monitor.poll_at(now, no_reader));
        assert!(monitor.observation(&endpoint.key).unwrap().error.is_some());
        assert!(!monitor.poll_at(now + Duration::from_secs(2), no_reader));
        assert!(monitor.poll_at(now + RETRY_INTERVAL, failing_reader));
        let state = Arc::clone(&monitor.entries[&endpoint.path].state);
        let guard = state.lock();
        let worker = held_reader(&endpoint, Arc::clone(&state)).unwrap();
        let cancellation = Arc::clone(&worker.cancel);
        monitor.entries.get_mut(&endpoint.path).unwrap().worker = Some(worker);
        let reaper = monitor.shutdown().expect("reaper thread");
        assert!(cancellation.load(Ordering::Acquire));
        assert!(monitor.observation(&endpoint.key).is_none());
        assert!(monitor.entries.is_empty());
        assert!(monitor.shutdown().is_none());
        drop(guard);
        reaper.join().unwrap();
    }

    #[test]
    fn fresh_receiver_identity_mismatch_rejects_before_open_and_event_write() {
        let mut endpoint = receiver(b"frozen-native-interface");
        endpoint.serial_number = Some("Frozen-SERIAL".into());
        let matching = || control_receiver_interface(&endpoint, Some("Frozen-SERIAL"));
        for interface in [
            ReceiverInterface {
                path: b"different-native-interface",
                ..matching()
            },
            ReceiverInterface {
                vid: 0x1208,
                ..matching()
            },
            ReceiverInterface {
                pid: 0x7691,
                ..matching()
            },
            ReceiverInterface {
                usage_page: 0xff00,
                ..matching()
            },
            ReceiverInterface {
                serial: Some("Replacement-SERIAL"),
                ..matching()
            },
            ReceiverInterface {
                serial: Some("frozen-serial"),
                ..matching()
            },
            ReceiverInterface {
                serial: None,
                ..matching()
            },
        ] {
            let probe = probe_receiver_open(
                &endpoint,
                [interface],
                Ok(Some("Frozen-SERIAL".into())),
                false,
                true,
            );
            assert!(probe.result.is_err());
            assert_eq!(
                (
                    probe.opened,
                    probe.serial_queries,
                    probe.reader_entries,
                    probe.dropped
                ),
                (0, 0, 0, 0)
            );
            assert!(probe.writes.is_empty());
            assert!(probe.slots.is_empty());
            assert!(probe.trace.is_empty());
        }
    }

    #[test]
    fn opened_receiver_identity_mismatch_drops_handle_before_event_write() {
        for (frozen, opened) in [
            (Some("Frozen-SERIAL"), Ok(Some("Replacement-SERIAL".into()))),
            (Some("Frozen-SERIAL"), Ok(Some("frozen-serial".into()))),
            (Some("Frozen-SERIAL"), Ok(None)),
            (Some("Frozen-SERIAL"), Ok(Some(" \t".into()))),
            (None, Ok(Some("Replacement-SERIAL".into()))),
            (Some("Frozen-SERIAL"), Err("serial query failed".into())),
            (None, Err("serial query failed".into())),
        ] {
            let mut endpoint = receiver(b"replaced-during-native-open");
            endpoint.serial_number = frozen.map(str::to_owned);
            let query_failed = opened.is_err();
            let probe = probe_receiver_open(
                &endpoint,
                [control_receiver_interface(&endpoint, frozen)],
                opened,
                false,
                true,
            );
            assert!(probe.result.is_err());
            if query_failed {
                assert!(
                    probe
                        .result
                        .as_ref()
                        .unwrap_err()
                        .contains("serial query failed")
                );
            }
            assert_eq!(
                (
                    probe.opened,
                    probe.serial_queries,
                    probe.reader_entries,
                    probe.dropped
                ),
                (1, 1, 0, 1)
            );
            assert!(probe.writes.is_empty());
            assert!(probe.slots.is_empty());
            assert!(probe.trace.is_empty());
        }
    }

    #[test]
    fn exact_normalized_receiver_and_serialless_handles_reach_reader() {
        for (frozen, enumerated, opened) in [
            (
                Some("Frozen-SERIAL"),
                Some(" \tFrozen-SERIAL\n"),
                Ok(Some("\nFrozen-SERIAL ".into())),
            ),
            (None, None, Ok(None)),
            (None, Some(" \t"), Ok(Some("\n ".into()))),
        ] {
            let mut endpoint = receiver(b"matching-native-interface");
            endpoint.serial_number = frozen.map(str::to_owned);
            let probe = probe_receiver_open(
                &endpoint,
                [control_receiver_interface(&endpoint, enumerated)],
                opened,
                false,
                false,
            );
            probe.result.unwrap();
            assert_eq!(
                (
                    probe.opened,
                    probe.serial_queries,
                    probe.reader_entries,
                    probe.dropped
                ),
                (1, 1, 1, 1)
            );
            assert!(probe.writes.is_empty());
            assert_eq!(probe.trace, ["open", "in", "close"]);
            assert_eq!(probe.slots, [6]);
            assert_eq!(
                probe.observation.battery(6),
                Some(TrackerBattery {
                    percent: 72,
                    voltage_v: 4.0,
                })
            );
        }
    }

    #[test]
    fn validated_receiver_runs_event_subscription_and_cleanup_on_shared_reader() {
        let endpoint = receiver(b"event-reader-positive-control");
        let probe = probe_receiver_open(
            &endpoint,
            [control_receiver_interface(&endpoint, None)],
            Ok(None),
            false,
            true,
        );
        probe.result.unwrap();
        let mut query = [0; 65];
        query[..9].copy_from_slice(&[0, 254, 1, 224, 0, 1, 0, 255, 0]);
        let mut subscribe = [0; 65];
        subscribe[..9].copy_from_slice(&[0, 254, 2, 224, 0, 1, 1, 255, 31]);
        let mut stop = [0; 65];
        stop[..9].copy_from_slice(&[0, 254, 3, 224, 0, 1, 3, 255, 0]);
        assert_eq!(probe.writes, [query, subscribe, stop]);
        assert_eq!(probe.slots, [6]);
        assert_eq!(
            probe.trace,
            ["open", "out", "in", "out", "in", "in", "out", "in", "close"]
        );
        assert_eq!((probe.reader_entries, probe.dropped), (1, 1));
    }

    #[test]
    fn shared_native_path_selects_control_usage_before_opening_and_publishing() {
        let mut endpoint = receiver(b"shared-native-path");
        endpoint.serial_number = Some("Frozen-SERIAL".into());
        for non_control_first in [true, false] {
            let control = control_receiver_interface(&endpoint, Some("Frozen-SERIAL"));
            let non_control = ReceiverInterface {
                usage_page: 0xff00,
                ..control_receiver_interface(&endpoint, Some("Frozen-SERIAL"))
            };
            let interfaces = if non_control_first {
                [non_control, control]
            } else {
                [control, non_control]
            };
            let probe = probe_receiver_open(
                &endpoint,
                interfaces,
                Ok(Some("Frozen-SERIAL".into())),
                false,
                true,
            );
            probe.result.unwrap();
            assert_eq!(probe.opened_usage_pages, [1]);
            assert_eq!(
                (
                    probe.opened,
                    probe.serial_queries,
                    probe.reader_entries,
                    probe.dropped
                ),
                (1, 1, 1, 1)
            );
            assert_eq!(
                probe
                    .observation
                    .trackers
                    .iter()
                    .map(|link| (
                        link.receiver_key.as_str(),
                        link.slot,
                        link.address.as_deref(),
                        link.online,
                    ))
                    .collect::<Vec<_>>(),
                [(
                    "hid:7368617265642D6E61746976652D70617468",
                    6,
                    Some("060504030201"),
                    true,
                )]
            );
            assert_eq!(probe.observation.error, None);
            let mut query = [0; 65];
            query[..9].copy_from_slice(&[0, 254, 1, 224, 0, 1, 0, 255, 0]);
            let mut subscribe = [0; 65];
            subscribe[..9].copy_from_slice(&[0, 254, 2, 224, 0, 1, 1, 255, 31]);
            let mut stop = [0; 65];
            stop[..9].copy_from_slice(&[0, 254, 3, 224, 0, 1, 3, 255, 0]);
            assert_eq!(probe.writes, [query, subscribe, stop]);
            assert_eq!(
                probe.trace,
                ["open", "out", "in", "out", "in", "in", "out", "in", "close"]
            );
        }
    }

    #[test]
    fn cancellation_during_native_open_drops_handle_before_serial_query_or_event_write() {
        let endpoint = receiver(b"cancelled-delayed-native-open");
        let probe = probe_receiver_open(
            &endpoint,
            [control_receiver_interface(&endpoint, None)],
            Ok(None),
            true,
            true,
        );
        probe.result.unwrap();
        assert_eq!(
            (
                probe.opened,
                probe.serial_queries,
                probe.reader_entries,
                probe.dropped
            ),
            (1, 0, 0, 1)
        );
        assert!(probe.writes.is_empty());
        assert!(probe.slots.is_empty());
        assert!(probe.trace.is_empty());
    }

    #[test]
    fn only_main_control_usage_page_is_enumerated() {
        assert!(control_interface(0x1209, 0x7690, 0x01));
        assert!(!control_interface(0x1209, 0x7690, 0xff00));
        assert!(!control_interface(0x1209, 0x7690, 0));
        assert!(!control_interface(0x1209, 0x7691, 0x01));
        assert!(!control_interface(0xffff, 0x7690, 0x01));
    }

    #[test]
    fn native_path_identity_is_lossless() {
        assert_ne!(receiver_key(&[0xfe]), receiver_key(&[0xff]));
        assert_ne!(receiver_key(b"receiver-a"), receiver_key(b"receiver-b"));
    }

    #[test]
    fn registration_reverses_address_without_claiming_sensor_activity() {
        let mut roster = Roster::new("receiver-a");
        let mut report = frame(0xff, 7);
        report[2..8].copy_from_slice(&[0x66, 0x55, 0x44, 0x33, 0x22, 0x11]);
        roster.ingest(&report);
        let links = roster.into_links();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].slot, 7);
        assert_eq!(links[0].receiver_key, "receiver-a");
        assert_eq!(links[0].address.as_deref(), Some("112233445566"));
        assert!(!links[0].online);
    }

    #[test]
    fn sensor_only_observations_preserve_unknown_address_for_all_sensor_types() {
        for kind in 0..=7 {
            let mut roster = Roster::new("receiver-a");
            roster.ingest(&frame(kind, 3));
            let links = roster.into_links();
            assert_eq!(links.len(), 1);
            assert!(links[0].online);
            assert!(links[0].address.is_none());
        }
    }

    #[test]
    fn four_records_merge_activity_and_registration_in_either_order() {
        let mut report = frame(0xff, 1);
        report[2..8].copy_from_slice(&[6, 5, 4, 3, 2, 1]);
        report[16] = 0;
        report[17] = 1;
        report[32] = 7;
        report[33] = 2;
        report[48] = 0xff;
        report[49] = 2;
        report[50..56].copy_from_slice(&[16, 15, 14, 13, 12, 11]);
        let mut roster = Roster::new("receiver-a");
        roster.ingest(&report);
        let links = roster.into_links();
        assert_eq!(links.len(), 2);
        assert!(links.iter().all(|link| link.online));
        assert_eq!(links[0].address.as_deref(), Some("010203040506"));
        assert_eq!(links[1].address.as_deref(), Some("0B0C0D0E0F10"));
    }

    #[test]
    fn malformed_payloads_invalid_slots_padding_and_control_packets_are_ignored() {
        let mut roster = Roster::new("receiver-a");
        let valid = frame(0xff, 0);
        for length in 0..REPORT_SIZE {
            roster.ingest(&valid[..length]);
        }
        roster.ingest(&[0; REPORT_SIZE + 1]);
        for slot in 16..=255 {
            roster.ingest(&frame(1, slot));
            roster.ingest(&frame(0xff, slot));
        }
        for kind in 8..=254 {
            roster.ingest(&frame(kind, 0));
        }
        assert!(roster.into_links().is_empty());
    }

    #[test]
    fn identical_slots_on_two_receivers_remain_separate() {
        let mut first = Roster::new("receiver-a");
        let mut second = Roster::new("receiver-b");
        first.ingest(&frame(1, 4));
        second.ingest(&frame(1, 4));
        let combined: Vec<_> = first
            .into_links()
            .into_iter()
            .chain(second.into_links())
            .collect();
        assert_eq!(combined.len(), 2);
        assert_eq!(combined[0].slot, combined[1].slot);
        assert_ne!(combined[0].receiver_key, combined[1].receiver_key);
        let fresh = Roster::new("receiver-a");
        assert!(fresh.into_links().is_empty());
    }

    #[test]
    fn partial_inventory_exposes_errors_and_total_failure_is_an_error() {
        let serial_only = combine_inventory(Ok(Vec::new()), Err("hid failure".into())).unwrap();
        assert_eq!(serial_only.warnings, ["hid failure"]);
        assert!(!serial_only.serial_enumeration_failed);
        let hid_only = combine_inventory(Err("serial failure".into()), Ok(Vec::new())).unwrap();
        assert_eq!(hid_only.warnings, ["serial failure"]);
        assert!(hid_only.serial_enumeration_failed);
        let empty = combine_inventory(Ok(Vec::new()), Ok(Vec::new())).unwrap();
        assert!(empty.ports.is_empty());
        assert!(!empty.serial_enumeration_failed);
        let error =
            combine_inventory(Err("serial failure".into()), Err("hid failure".into())).unwrap_err();
        assert!(error.contains("serial failure"));
        assert!(error.contains("hid failure"));
    }

    fn registered(slot: u8, identity: u8) -> [u8; REPORT_SIZE] {
        let mut report = frame(0xff, slot);
        report[2..8].copy_from_slice(&[identity, 2, 3, 4, 5, 6]);
        report
    }

    fn old_pairings(state: &mut WorkerState, now: Instant) {
        for slot in 0..10 {
            state.ingest(&registered(slot, slot + 1), now);
            state.ingest(&battery_frame(0, slot, [128, 155, 144]), now);
        }
    }

    #[test]
    fn positive_mutation_ack_retires_ten_bindings_and_reacquires_only_seven() {
        let now = Instant::now();
        for opcode in [203, 205] {
            let mut state = WorkerState::new("changed");
            let mut other = WorkerState::new("untouched");
            old_pairings(&mut state, now);
            old_pairings(&mut other, now);
            state.roster.expire(now + TRAFFIC_TIMEOUT);
            assert_eq!(state.roster.links().len(), 10);
            assert!(state.roster.links().iter().all(|link| !link.online));
            for status in [1, 3, 6, 7] {
                let mut ack = frame(251, 42);
                ack[2] = opcode;
                ack[3] = status;
                state.ingest(&ack, now);
                assert_eq!(state.roster.links().len(), 10);
                assert_eq!(state.binding_revision, 0);
            }
            let mut ack = frame(251, 42);
            ack[2] = opcode;
            ack[3] = 0;
            ack[16..32].copy_from_slice(&registered(9, 10)[..16]);
            state.ingest(&ack, now);
            assert!(state.roster.links().is_empty());
            assert_eq!(
                state.binding_reset,
                Some(BindingResetCause::ConfirmedMutation)
            );
            assert_eq!(state.roster.batteries(), [None; SLOT_COUNT]);
            for slot in 0..7 {
                state.ingest(&registered(slot, slot + 50), now);
                state.ingest(&frame(1, slot), now);
            }
            assert_eq!(
                state
                    .roster
                    .links()
                    .iter()
                    .map(|link| link.slot)
                    .collect::<Vec<_>>(),
                [0, 1, 2, 3, 4, 5, 6]
            );
            assert!(state.roster.links().iter().all(|link| link.online));
            assert_eq!(state.roster.batteries(), [None; SLOT_COUNT]);
            assert_eq!(other.roster.links().len(), 10);
            assert!(
                other
                    .roster
                    .batteries()
                    .iter()
                    .take(10)
                    .all(Option::is_some)
            );
            assert_eq!(other.binding_revision, 0);
        }
    }

    #[test]
    fn known_address_replacement_retires_old_tail_but_empty_addresses_are_not_tombstones() {
        let now = Instant::now();
        let mut state = WorkerState::new("receiver");
        old_pairings(&mut state, now);
        for address in [[0; 6], [0xff; 6]] {
            let mut invalid = registered(0, 1);
            invalid[2..8].copy_from_slice(&address);
            state.ingest(&invalid, now);
            assert_eq!(state.roster.links().len(), 10);
            assert_eq!(state.binding_revision, 0);
        }
        state.ingest(&registered(0, 1), now);
        state.ingest(&frame(254, 0), now);
        assert_eq!(state.binding_revision, 0);
        assert_eq!(state.roster.links().len(), 10);
        for slot in 0..7 {
            state.ingest(&registered(slot, slot + 50), now);
        }
        assert_eq!(state.binding_revision, 1);
        assert_eq!(
            state.binding_reset,
            Some(BindingResetCause::AddressReplacement)
        );
        assert_eq!(state.roster.links().len(), 7);
        assert!(
            state
                .roster
                .links()
                .iter()
                .all(|link| link.slot < 7 && !link.online)
        );
        assert_eq!(state.roster.batteries(), [None; SLOT_COUNT]);
    }

    #[test]
    fn cdc_mutation_detaches_old_ingress_and_waits_for_its_reader_to_join() {
        let now = Instant::now();
        let endpoint = receiver(b"changed");
        let other_endpoint = receiver(b"untouched");
        let mut monitor = ReceiverMonitor::new();
        for endpoint in [&endpoint, &other_endpoint] {
            let mut entry = MonitorEntry::new(endpoint.clone(), now);
            entry.retry_at = now + Duration::from_secs(60);
            old_pairings(&mut entry.state.lock(), now);
            entry.state.lock().publish(&mut entry.observation);
            monitor.entries.insert(endpoint.path.clone(), entry);
        }
        let (reader, release) = gated_reader();
        let entry = monitor.entries.get_mut(&endpoint.path).unwrap();
        entry.worker = Some(reader);
        let old_state = Arc::clone(&entry.state);
        let mut wrong = endpoint.clone();
        wrong.serial_number = Some("replacement".into());
        assert!(!monitor.observe_pairing_mutation(&wrong));
        assert!(monitor.observe_pairing_mutation(&endpoint));
        old_state.lock().ingest(&registered(9, 10), now);
        assert!(monitor.poll_at(now, no_reader));
        assert!(
            monitor
                .observation_for(&endpoint)
                .unwrap()
                .trackers
                .is_empty()
        );
        assert_eq!(
            monitor
                .observation_for(&other_endpoint)
                .unwrap()
                .trackers
                .len(),
            10
        );
        assert_eq!(
            monitor
                .take_binding_invalidations()
                .into_iter()
                .map(|(receiver, cause)| (receiver.key, cause))
                .collect::<Vec<_>>(),
            [(endpoint.key.clone(), BindingResetCause::ConfirmedMutation)]
        );
        assert!(monitor.take_binding_invalidations().is_empty());
        let entry = &monitor.entries[&endpoint.path];
        assert!(
            entry
                .worker
                .as_ref()
                .unwrap()
                .cancel
                .load(Ordering::Acquire)
        );
        release.send(()).unwrap();
        wait_until_finished(entry.worker.as_ref().unwrap());
        monitor.entries.get_mut(&endpoint.path).unwrap().retry_at = now + Duration::from_secs(60);
        monitor.poll_at(now, no_reader);
        let entry = monitor.entries.get_mut(&endpoint.path).unwrap();
        for slot in 0..7 {
            entry.state.lock().ingest(&registered(slot, slot + 50), now);
        }
        monitor.poll_at(now, no_reader);
        assert_eq!(
            monitor.observation_for(&endpoint).unwrap().trackers.len(),
            7
        );
        assert_eq!(
            monitor
                .observation_for(&other_endpoint)
                .unwrap()
                .trackers
                .len(),
            10
        );
    }

    #[test]
    fn handoff_and_periodic_enumeration_are_not_pairing_changes_but_rediscovery_is_explicit() {
        let now = Instant::now();
        let endpoint = receiver(b"retained");
        let mut monitor = ReceiverMonitor::new();
        let mut entry = MonitorEntry::new(endpoint.clone(), now);
        entry.retry_at = now + Duration::from_secs(60);
        old_pairings(&mut entry.state.lock(), now);
        entry.state.lock().publish(&mut entry.observation);
        monitor.entries.insert(endpoint.path.clone(), entry);
        monitor.reconcile_at(std::slice::from_ref(&endpoint), now, no_reader);
        assert!(monitor.take_binding_invalidations().is_empty());
        assert_eq!(
            monitor.observation_for(&endpoint).unwrap().trackers.len(),
            10
        );
        let token = monitor.reserve(&endpoint).unwrap();
        monitor.rediscover_bindings();
        assert!(monitor.take_binding_invalidations().is_empty());
        assert!(monitor.reservation_ready(&token).unwrap());
        monitor.release(&token).unwrap();
        let entry = monitor.entries.get_mut(&endpoint.path).unwrap();
        entry.retry_at = now + Duration::from_secs(60);
        entry.state.lock().ingest(&registered(0, 1), now);
        assert!(monitor.take_binding_invalidations().is_empty());
        monitor.entries[&endpoint.path]
            .state
            .lock()
            .ingest(&registered(0, 50), now);
        assert_eq!(
            monitor
                .take_binding_invalidations()
                .into_iter()
                .map(|(receiver, cause)| (receiver.key, cause))
                .collect::<Vec<_>>(),
            [(endpoint.key.clone(), BindingResetCause::AddressReplacement)]
        );
        monitor.rediscover_bindings();
        assert_eq!(
            monitor
                .take_binding_invalidations()
                .into_iter()
                .map(|(receiver, cause)| (receiver.key, cause))
                .collect::<Vec<_>>(),
            [(endpoint.key.clone(), BindingResetCause::ManualRediscovery)]
        );
        assert!(
            monitor
                .observation_for(&endpoint)
                .unwrap()
                .trackers
                .is_empty()
        );
        assert!(monitor.take_binding_invalidations().is_empty());
    }
}
