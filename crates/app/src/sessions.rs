use crate::domain::{DeviceIdentity, SerialEndpoint, SerialRole};
use crate::i18n::tr;
use crate::rssi_scan::ScanSnapshot;
use parking_lot::Mutex;
use slimenrf_terminal_core::TerminalBuffer;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

mod connection;
mod lifecycle;
mod native;
#[cfg(test)]
pub(crate) mod test_support;
mod thermal_dump;
mod transcript;
mod worker;

pub(crate) use lifecycle::reap_workers;
use native::Factory;
use thermal_dump::{DumpCommand, DumpRequests};
pub use thermal_dump::{ThermalDumpOutcome, ThermalDumpRequest};
use transcript::DisplayQueue;

const IO_TIMEOUT: Duration = Duration::from_millis(30);
const WRITE_DEADLINE: Duration = Duration::from_millis(500);
// Inventory normally refreshes every three seconds. Stop trusting a stalled scan.
const INVENTORY_MAX_AGE: Duration = Duration::from_secs(10);
const QUEUE_CAPACITY: usize = 64;
const CHUNK_BYTES: usize = 2_048;
const MAX_COMMAND_BYTES: usize = 16_384;
const MAX_IDENTITY_LINE: usize = 16_384;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionState {
    Connecting,
    Connected,
    Closing,
    Disconnected,
    Faulted,
}

pub struct SessionView {
    pub port: String,
    pub generation: u64,
    pub state: ConnectionState,
    pub last_error: Option<String>,
    pub identity: DeviceIdentity,
    pub terminal: TerminalBuffer,
}

/// Latest received measurements, not a firmware completion acknowledgement.
pub struct RssiScanUpdate {
    pub port: String,
    pub generation: u64,
    pub snapshot: ScanSnapshot,
    /// First delivered observation of this scan in this worker generation.
    pub first_update: bool,
}

/// One terminal result for a transport write, not a firmware acknowledgement.
#[derive(Debug)]
pub struct WriteReceipt {
    completion: Option<mpsc::Receiver<Result<(), String>>>,
}

impl WriteReceipt {
    pub fn poll(&mut self) -> Option<Result<(), String>> {
        let completion = self.completion.as_ref()?;
        let result = match completion.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return None,
            Err(mpsc::TryRecvError::Disconnected) => {
                Err(tr("serial.session.error.stopped_before_send"))
            }
        };
        self.completion = None;
        Some(result)
    }
}

#[derive(Debug)]
struct Command {
    bytes: Vec<u8>,
    completion: mpsc::SyncSender<Result<(), String>>,
    thermal_dump: Option<DumpCommand>,
    _pending: PendingWrite,
}

/// Count the command, not its receipt: abandoning a receipt cannot make an
/// accepted transport write disappear from the firmware handoff barrier.
#[derive(Debug)]
struct PendingWrite(Arc<AtomicUsize>);

impl PendingWrite {
    fn new(count: &Arc<AtomicUsize>) -> Self {
        count.fetch_add(1, Ordering::Relaxed);
        Self(Arc::clone(count))
    }
}

impl Drop for PendingWrite {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Release);
    }
}

enum Lifecycle {
    Opened,
    Closed { error: Option<String>, dropped: u64 },
}

struct IdentitySnapshot {
    identity: DeviceIdentity,
    revision: u64,
    pairing_changed: bool,
}

struct Worker {
    generation: u64,
    cancel: Arc<AtomicBool>,
    commands: mpsc::SyncSender<Command>,
    display: Arc<Mutex<DisplayQueue>>,
    lifecycle: mpsc::Receiver<Lifecycle>,
    identity: Arc<Mutex<IdentitySnapshot>>,
    identity_revision: u64,
    rssi_scan: Arc<Mutex<Option<ScanSnapshot>>>,
    delivered_rssi_scan: Option<u64>,
    handle: Option<JoinHandle<()>>,
    opened: bool,
    closed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct UsbIdentity {
    serial: String,
    vid: u16,
    pid: u16,
    interface_number: Option<u8>,
    role: SerialRole,
}

impl UsbIdentity {
    fn from_endpoint(endpoint: &SerialEndpoint) -> Option<Self> {
        let serial = endpoint.serial_number.as_deref()?;
        if serial.trim().is_empty() {
            return None;
        }
        Some(Self {
            serial: serial.to_owned(),
            vid: endpoint.vid?,
            pid: endpoint.pid?,
            interface_number: endpoint
                .usb_interface
                .as_ref()
                .and_then(|interface| interface.number),
            role: endpoint.role(),
        })
    }

    fn matches_endpoint(&self, endpoint: &SerialEndpoint) -> bool {
        endpoint.serial_number.as_deref() == Some(self.serial.as_str())
            && endpoint.vid == Some(self.vid)
            && endpoint.pid == Some(self.pid)
            && endpoint
                .usb_interface
                .as_ref()
                .and_then(|interface| interface.number)
                == self.interface_number
            && endpoint.role() == self.role
    }
}

struct InventoryEntry {
    recovery_identity: Option<UsbIdentity>,
    console_allowed: bool,
    usb: bool,
}

enum LossProbe {
    Available,
    Waiting { lost_at: Instant },
    Spent,
}

struct Reconnect {
    identity: UsbIdentity,
    arrival_pending: bool,
    loss_probe: LossProbe,
    native_change_after: Option<Instant>,
}

impl Reconnect {
    fn note_loss(&mut self, lost_at: Instant) -> bool {
        if matches!(self.loss_probe, LossProbe::Available) {
            self.loss_probe = LossProbe::Waiting { lost_at };
            true
        } else {
            false
        }
    }

    fn eligible(&self, scan_started_at: Instant) -> bool {
        self.arrival_pending
            || matches!(
                self.loss_probe,
                LossProbe::Waiting { lost_at } if scan_started_at >= lost_at
            )
            || self
                .native_change_after
                .is_some_and(|after| scan_started_at >= after)
    }

    fn consume(&mut self) {
        self.arrival_pending = false;
        self.loss_probe = LossProbe::Spent;
        self.native_change_after = None;
    }
}

struct Entry {
    view: SessionView,
    worker: Option<Worker>,
    close_requested: bool,
    pending_writes: Arc<AtomicUsize>,
    opening_identity: Option<UsbIdentity>,
    // Retained only after this generation passes opening inventory continuity.
    captured_usb_identity: Option<UsbIdentity>,
    reconnect: Option<Reconnect>,
}

enum CloseRejection {
    Shutdown,
    Missing,
    Reserved,
    PendingWrites,
    ThermalDump,
}

/// A reservation is valid only in its issuing manager. Keep it until the
/// firmware worker has joined; releasing it does not reconnect the terminal.
#[derive(Clone, Debug)]
pub struct SerialReservation {
    identity: Arc<()>,
    port: String,
    generation: Option<u64>,
    console_claimed: bool,
}

pub(crate) struct ReservedConsoleSource {
    port: String,
    factory: Factory,
}

impl ReservedConsoleSource {
    pub(crate) fn open(self) -> Result<ReservedConsole, String> {
        (self.factory)(&self.port)
            .map(|transport| ReservedConsole { transport })
            .map_err(|error| error.to_string())
    }
}

pub(crate) struct ReservedConsole {
    transport: Box<dyn native::Transport>,
}

impl ReservedConsole {
    pub(crate) fn read(&mut self, bytes: &mut [u8]) -> Result<usize, String> {
        match self.transport.read(bytes) {
            Ok(0) => Err(tr("serial.transport.error.stream_closed")),
            Ok(length) => Ok(length),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::Interrupted
                ) =>
            {
                Ok(0)
            }
            Err(error) => Err(error.to_string()),
        }
    }

    pub(crate) fn write_command(&mut self, bytes: &[u8], stop: &AtomicBool) -> Result<(), String> {
        worker::write_command(&mut *self.transport, bytes, stop).map_err(|error| error.to_string())
    }
}

/// Positive RF evidence bound to one serial generation. Exclusion only, never
/// authorization for commands or reconnects.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SessionIdentityObservation {
    pub port: String,
    pub generation: u64,
    pub address: String,
    pub usb: Option<SessionUsbIdentity>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct SessionUsbIdentity {
    serial: String,
    vid: u16,
    pid: u16,
}

impl SessionUsbIdentity {
    pub(crate) fn new(serial: &str, vid: u16, pid: u16) -> Option<Self> {
        let serial = serial.trim();
        (!serial.is_empty()).then(|| Self {
            serial: serial.to_ascii_uppercase(),
            vid,
            pid,
        })
    }

    pub(crate) fn matches_endpoint(&self, endpoint: &SerialEndpoint) -> bool {
        endpoint.kind == crate::domain::SerialPortKind::Usb
            && endpoint.vid == Some(self.vid)
            && endpoint.pid == Some(self.pid)
            && endpoint
                .serial_number
                .as_deref()
                .is_some_and(|serial| self.serial.eq_ignore_ascii_case(serial.trim()))
    }
}

impl SessionIdentityObservation {
    fn from_generation(view: &SessionView, captured_usb: Option<&UsbIdentity>) -> Option<Self> {
        Some(Self {
            port: view.port.clone(),
            generation: view.generation,
            address: crate::domain::normalize_address(view.identity.address.as_deref()?)?,
            usb: captured_usb
                .and_then(|usb| SessionUsbIdentity::new(&usb.serial, usb.vid, usb.pid)),
        })
    }
}

/// Changes consumed by one poll. Positive identities contain at most one
/// publication per entry and are returned before automatic recovery resets it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[must_use = "Forward identity observations and schedule requested inventory refreshes"]
pub struct SessionChanges {
    pub state: bool,
    pub display: bool,
    /// Coalesced follow-up; a scan already running cannot satisfy a later loss.
    pub(crate) refresh_inventory: bool,
    pub(crate) identities: Vec<SessionIdentityObservation>,
}

pub struct SessionManager {
    entries: BTreeMap<String, Entry>,
    reservations: BTreeMap<String, SerialReservation>,
    // Only unique USB identities on unique canonical ports authorize recovery.
    inventory: BTreeMap<String, InventoryEntry>,
    inventory_updated_at: Option<Instant>,
    inventory_scan_started_at: Option<Instant>,
    factory: Factory,
    next_generation: u64,
    shutting_down: bool,
    thermal_dumps: Arc<Mutex<DumpRequests>>,
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionManager {
    pub fn new() -> Self {
        Self::with_factory(Arc::new(native::open))
    }

    fn with_factory(factory: Factory) -> Self {
        Self {
            entries: BTreeMap::new(),
            reservations: BTreeMap::new(),
            inventory: BTreeMap::new(),
            inventory_updated_at: None,
            inventory_scan_started_at: None,
            factory,
            next_generation: 0,
            shutting_down: false,
            thermal_dumps: Arc::new(Mutex::new(DumpRequests::default())),
        }
    }

    /// Stop the passive owner and lock the canonical port against reconnects.
    /// None permits an unconnected port; Some pins a previously selected session.
    pub fn reserve(
        &mut self,
        port: &str,
        expected_generation: Option<u64>,
    ) -> Result<SerialReservation, String> {
        if self.shutting_down {
            return Err(tr("serial.session.error.shutdown"));
        }
        if port.trim().is_empty() {
            return Err(tr("serial.session.error.empty_port"));
        }
        let key = port_key(port);
        if self
            .entries
            .get(&key)
            .is_some_and(|entry| entry.close_requested)
        {
            return Err(format!("{port} terminal is closing"));
        }
        if self.reservations.contains_key(&key) {
            return Err(format!("{port} is reserved for a firmware operation"));
        }
        let generation = self.entries.get(&key).map(|entry| entry.view.generation);
        if expected_generation.is_some() && generation != expected_generation {
            return Err(format!(
                "{port} connection changed; select the target again"
            ));
        }
        let token = SerialReservation {
            identity: Arc::new(()),
            port: key.clone(),
            generation,
            console_claimed: false,
        };
        self.reservations.insert(key.clone(), token.clone());
        self.thermal_dumps
            .lock()
            .finish_port(port, ThermalDumpOutcome::Handoff);
        if let Some(entry) = self.entries.get_mut(&key) {
            entry.reconnect = None;
            entry.opening_identity = None;
            if let Some(worker) = &entry.worker {
                worker.cancel.store(true, Ordering::Release);
                if !worker.closed {
                    entry.view.state = ConnectionState::Closing;
                }
            }
        }
        Ok(token)
    }

    /// Check handoff after `poll` has published and joined finished workers.
    /// Readiness must not consume another session's final identity publication.
    pub fn reservation_ready(&mut self, token: &SerialReservation) -> Result<bool, String> {
        self.validate_reservation(token)?;
        let entry = self.entries.get(&token.port);
        if entry.map(|entry| entry.view.generation) != token.generation {
            return Err(format!(
                "{} connection changed; select the target again",
                token.port
            ));
        }
        Ok(entry.is_none_or(|entry| entry.worker.is_none()))
    }

    pub(crate) fn reserved_console_source(
        &mut self,
        token: &SerialReservation,
    ) -> Result<ReservedConsoleSource, String> {
        if !self.reservation_ready(token)? {
            return Err("Serial predecessor has not joined".into());
        }
        let reservation = self
            .reservations
            .get_mut(&token.port)
            .expect("validated reservation");
        if std::mem::replace(&mut reservation.console_claimed, true) {
            return Err("Reserved console source was already consumed".into());
        }
        Ok(ReservedConsoleSource {
            port: self
                .entries
                .get(&token.port)
                .map_or_else(|| token.port.clone(), |entry| entry.view.port.clone()),
            factory: Arc::clone(&self.factory),
        })
    }

    pub(crate) fn thermal_dump_busy(&self, port: &str) -> bool {
        self.thermal_dumps.lock().busy(port)
    }

    /// Call only after the firmware owner has joined, or to cancel a pending
    /// handoff. A pending passive worker remains tracked until it really exits.
    pub fn release(&mut self, token: &SerialReservation) -> Result<(), String> {
        self.validate_reservation(token)?;
        self.reservations.remove(&token.port);
        Ok(())
    }

    fn validate_reservation(&self, token: &SerialReservation) -> Result<(), String> {
        if self.shutting_down
            || !self
                .reservations
                .get(&token.port)
                .is_some_and(|current| Arc::ptr_eq(&current.identity, &token.identity))
        {
            return Err("Serial reservation is no longer valid".into());
        }
        Ok(())
    }

    pub(crate) fn can_request_close(&self, port: &str) -> bool {
        self.check_close_request(port).is_ok()
    }

    fn check_close_request(&self, port: &str) -> Result<(), CloseRejection> {
        if self.shutting_down {
            return Err(CloseRejection::Shutdown);
        }
        let key = port_key(port);
        let entry = self.entries.get(&key).ok_or(CloseRejection::Missing)?;
        if entry.close_requested {
            return Ok(());
        }
        if self.reservations.contains_key(&key) {
            return Err(CloseRejection::Reserved);
        }
        if entry.pending_writes.load(Ordering::Acquire) != 0 {
            return Err(CloseRejection::PendingWrites);
        }
        let dumps = self.thermal_dumps.lock();
        if dumps.busy(port) && !dumps.needs_reconnect(port) {
            return Err(CloseRejection::ThermalDump);
        }
        Ok(())
    }

    /// Keep this generation closed until its settled history is forgotten.
    pub(crate) fn request_close(&mut self, port: &str) -> Result<(), String> {
        self.check_close_request(port)
            .map_err(|rejection| match rejection {
                CloseRejection::Shutdown => tr("serial.session.error.shutdown"),
                CloseRejection::Missing => {
                    rust_i18n::t!("serial.session.error.not_connected", port = port).to_string()
                }
                CloseRejection::Reserved => {
                    format!("{port} is reserved for a firmware operation")
                }
                CloseRejection::PendingWrites => format!("{port} has pending command writes"),
                CloseRejection::ThermalDump => {
                    format!("{port} has an active temperature dump; wait or cancel it first")
                }
            })?;
        let entry = self.entries.get_mut(&port_key(port)).unwrap();
        if entry.close_requested {
            return Ok(());
        }
        entry.close_requested = true;
        self.disconnect(port)
    }

    pub(crate) fn has_close_requests(&self) -> bool {
        self.entries.values().any(|entry| entry.close_requested)
    }

    pub(crate) fn is_close_requested(&self, port: &str) -> bool {
        self.entries
            .get(&port_key(port))
            .is_some_and(|entry| entry.close_requested)
    }

    pub fn disconnect(&mut self, port: &str) -> Result<(), String> {
        let entry = self.entries.get_mut(&port_key(port)).ok_or_else(|| {
            rust_i18n::t!("serial.session.error.not_connected", port = port).to_string()
        })?;
        self.thermal_dumps
            .lock()
            .finish_port(port, ThermalDumpOutcome::Disconnected);
        entry.reconnect = None;
        entry.opening_identity = None;
        if entry.worker.is_none() {
            entry.view.state = ConnectionState::Disconnected;
        }
        if let Some(worker) = &entry.worker {
            // Closed finalizes the view before the worker thread is reaped.
            // Do not replace that terminal state: no further Closed event follows.
            if !worker.closed {
                worker.cancel.store(true, Ordering::Release);
                entry.view.state = ConnectionState::Closing;
            }
        }
        Ok(())
    }

    /// Quiescence barrier for firmware operations that leave serial readers
    /// connected. Gate new submissions before querying all affected ports.
    /// Queued and in-flight writes count until completion/failure/destruction,
    /// independently of receipt polling and even during shutdown reaping.
    pub fn writes_idle(&self, port: &str) -> bool {
        self.entries
            .get(&port_key(port))
            .is_none_or(|entry| entry.pending_writes.load(Ordering::Acquire) == 0)
    }

    pub(crate) fn command_port_reserved(&self, port: &str) -> bool {
        self.reservations.contains_key(&port_key(port))
    }

    pub fn send_line(&mut self, port: &str, line: &str) -> Result<WriteReceipt, String> {
        let key = port_key(port);
        if self.reservations.contains_key(&key) {
            return Err(format!("{port} is reserved for a firmware operation"));
        }
        let entry = self.entries.get_mut(&key).ok_or_else(|| {
            rust_i18n::t!("serial.session.error.not_connected", port = port).to_string()
        })?;
        if entry.close_requested {
            return Err(format!("{port} terminal is closing"));
        }
        let result = (|| {
            if entry.view.state != ConnectionState::Connected {
                return Err(
                    rust_i18n::t!("serial.session.error.not_connected", port = port).to_string(),
                );
            }
            let line = line.trim_end_matches(['\r', '\n']);
            if line.len() >= MAX_COMMAND_BYTES {
                return Err(rust_i18n::t!(
                    "serial.session.error.command_too_long",
                    bytes = MAX_COMMAND_BYTES - 1
                )
                .to_string());
            }
            let worker = entry.worker.as_ref().ok_or_else(|| {
                rust_i18n::t!("serial.session.error.unavailable", port = port).to_string()
            })?;
            let mut dumps = self.thermal_dumps.lock();
            if dumps.needs_reconnect(port) {
                return Err(format!(
                    "{port} has an interrupted temperature dump response; disconnect and reconnect before sending another command"
                ));
            }
            if dumps.busy(port) {
                return Err(format!(
                    "{port} has an active temperature dump; wait or cancel it first"
                ));
            }
            let request = if line.trim() == "tcal dump" {
                Some(dumps.begin(port, entry.view.generation)?)
            } else {
                None
            };
            let mut bytes = Vec::with_capacity(line.len() + 1);
            bytes.extend_from_slice(line.as_bytes());
            bytes.push(b'\n');
            let (completion, receipt) = mpsc::sync_channel(1);
            let queued = worker.commands.try_send(Command {
                bytes,
                completion,
                thermal_dump: request.as_ref().map(|request| DumpCommand {
                    request: request.clone(),
                    requests: Arc::clone(&self.thermal_dumps),
                }),
                _pending: PendingWrite::new(&entry.pending_writes),
            });
            if queued.is_err()
                && let Some(request) = &request
            {
                dumps.reject(request);
            }
            drop(dumps);
            queued.map_err(|error| match error {
                mpsc::TrySendError::Full(_) => tr("serial.session.error.queue_full"),
                mpsc::TrySendError::Disconnected(_) => {
                    tr("serial.session.error.stopped_before_submit")
                }
            })?;
            Ok(WriteReceipt {
                completion: Some(receipt),
            })
        })();
        if let Err(error) = &result {
            entry.view.last_error = Some(error.clone());
        }
        result
    }

    pub fn sessions(&self) -> impl Iterator<Item = &SessionView> {
        self.entries.values().map(|entry| &entry.view)
    }

    pub fn session(&self, port: &str) -> Option<&SessionView> {
        self.entries.get(&port_key(port)).map(|entry| &entry.view)
    }

    /// Snapshot existing positive evidence once at operation admission. Captured
    /// USB proof stays tied to its generation even if inventory has changed.
    pub(crate) fn identity_observations(
        &self,
    ) -> impl Iterator<Item = SessionIdentityObservation> + '_ {
        self.entries.values().filter_map(|entry| {
            SessionIdentityObservation::from_generation(
                &entry.view,
                entry.captured_usb_identity.as_ref(),
            )
        })
    }

    /// Identity evidence for the live generation's captured USB console, never
    /// terminal history joined to a replacement device at the same port.
    pub(crate) fn connected_identity_for(
        &self,
        endpoint: &SerialEndpoint,
        generation: u64,
    ) -> Option<&DeviceIdentity> {
        if self.shutting_down
            || !self.inventory_is_fresh(Instant::now())
            || endpoint.kind != crate::domain::SerialPortKind::Usb
        {
            return None;
        }
        let key = port_key(&endpoint.port_name);
        if self.reservations.contains_key(&key) {
            return None;
        }
        let entry = self.entries.get(&key)?;
        if entry.view.state != ConnectionState::Connected || entry.view.generation != generation {
            return None;
        }
        let worker = entry.worker.as_ref()?;
        if worker.generation != generation
            || worker.closed
            || worker.cancel.load(Ordering::Acquire)
            || worker.handle.as_ref().is_none_or(JoinHandle::is_finished)
        {
            return None;
        }
        // Opened consumes opening_identity into this generation's recovery pin.
        let captured = &entry.reconnect.as_ref()?.identity;
        let current = self.inventory.get(&key)?;
        if !current.usb
            || !current.console_allowed
            || current.recovery_identity.as_ref() != Some(captured)
            || !captured.matches_endpoint(endpoint)
        {
            return None;
        }
        Some(&entry.view.identity)
    }

    /// Completed receiver success logs, never transport write receipts.
    pub(crate) fn take_receiver_pairing_changes(&mut self) -> Vec<(String, u64)> {
        let fresh = self.inventory_is_fresh(Instant::now());
        self.entries
            .iter()
            .filter_map(|(key, entry)| {
                let worker = entry.worker.as_ref()?;
                let mut snapshot = worker.identity.lock();
                if !snapshot.pairing_changed {
                    return None;
                }
                let current = self.inventory.get(key);
                let captured = entry.captured_usb_identity.as_ref();
                let eligible = !self.shutting_down
                    && !self.reservations.contains_key(key)
                    && entry.view.state == ConnectionState::Connected
                    && worker.generation == entry.view.generation
                    && !worker.closed
                    && !worker.cancel.load(Ordering::Acquire)
                    && worker
                        .handle
                        .as_ref()
                        .is_some_and(|handle| !handle.is_finished())
                    && captured.is_some()
                    && current.is_some_and(|current| {
                        current.console_allowed && current.recovery_identity.as_ref() == captured
                    })
                    && snapshot.identity.role == crate::domain::DeviceRole::Receiver;
                if !eligible {
                    snapshot.pairing_changed = false;
                    return None;
                }
                if !fresh || entry.view.identity.role != crate::domain::DeviceRole::Receiver {
                    // Keep one coalesced success on this live owner while a
                    // failed/aged enumeration or lifecycle publication catches up.
                    return None;
                }
                snapshot.pairing_changed = false;
                Some((entry.view.port.clone(), worker.generation))
            })
            .collect()
    }

    /// Drain one coalesced observation per current, live serial owner.
    pub fn take_rssi_scans(&mut self) -> Vec<RssiScanUpdate> {
        let mut updates = Vec::new();
        let inventory_fresh = self.inventory_is_fresh(Instant::now());
        for (key, entry) in &mut self.entries {
            let Some(worker) = &mut entry.worker else {
                continue;
            };
            // Before Opened is polled, automatic recovery must satisfy the
            // same fresh, exact inventory pin as lifecycle admission. Manual
            // startup and an already healthy Connected owner need no such pin.
            let current_identity = self
                .inventory
                .get(key)
                .filter(|_| inventory_fresh)
                .and_then(|record| record.recovery_identity.as_ref());
            let recovery_rejected = entry.view.state == ConnectionState::Connecting
                && entry
                    .reconnect
                    .as_ref()
                    .is_some_and(|retry| current_identity != Some(&retry.identity));
            let mut latest = worker.rssi_scan.lock();
            let eligible = !self.shutting_down
                && !self.reservations.contains_key(key)
                && worker.generation == entry.view.generation
                && matches!(
                    entry.view.state,
                    ConnectionState::Connecting | ConnectionState::Connected
                )
                && !recovery_rejected
                && !worker.closed
                && !worker.cancel.load(Ordering::Acquire)
                && !worker.handle.as_ref().is_some_and(JoinHandle::is_finished);
            let Some(snapshot) = latest.take().filter(|_| eligible) else {
                continue;
            };
            let first_update = worker.delivered_rssi_scan != Some(snapshot.scan_id);
            worker.delivered_rssi_scan = Some(snapshot.scan_id);
            updates.push(RssiScanUpdate {
                port: entry.view.port.clone(),
                generation: worker.generation,
                snapshot,
                first_update,
            });
        }
        updates
    }

    pub fn clear(&mut self, port: &str) {
        if let Some(entry) = self.entries.get_mut(&port_key(port)) {
            entry.view.terminal.clear();
        }
    }
}

fn port_key(port: &str) -> String {
    // Windows COM device names are case insensitive. Do not merge distinct
    // case-sensitive Unix device paths.
    if cfg!(windows) {
        let port = port
            .strip_prefix(r"\\.\")
            .or_else(|| port.strip_prefix(r"\\?\"))
            .unwrap_or(port);
        port.to_ascii_uppercase()
    } else {
        port.to_owned()
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod reserved_console_tests {
    use super::thermal_dump::{DumpCollector, DumpCommand};
    use super::{SessionManager, ThermalDumpOutcome};
    use parking_lot::Mutex;
    use std::io::{self, Cursor, Read, Write};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, mpsc};
    use std::time::Duration;

    struct FixtureConsole {
        input: Cursor<&'static [u8]>,
        output: Arc<Mutex<Vec<u8>>>,
        calls: Arc<AtomicUsize>,
        fail_second: bool,
    }

    impl Read for FixtureConsole {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            self.input.read(bytes)
        }
    }

    impl Write for FixtureConsole {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let call = self.calls.fetch_add(1, Ordering::Relaxed);
            if self.fail_second && call != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "possible partial write",
                ));
            }
            let length = bytes.len().min(3);
            self.output.lock().extend_from_slice(&bytes[..length]);
            Ok(length)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn reserved_console_source_is_one_use_across_token_clones_and_uses_owned_factory() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let calls = Arc::new(AtomicUsize::new(0));
        let opens = Arc::new(AtomicUsize::new(0));
        let mut manager = SessionManager::with_factory({
            let output = Arc::clone(&output);
            let calls = Arc::clone(&calls);
            let opens = Arc::clone(&opens);
            Arc::new(move |port| {
                assert_eq!(port, "FIXTURE");
                opens.fetch_add(1, Ordering::Relaxed);
                Ok(Box::new(FixtureConsole {
                    input: Cursor::new(b"fixture\n"),
                    output: Arc::clone(&output),
                    calls: Arc::clone(&calls),
                    fail_second: false,
                }))
            })
        });
        let port = if cfg!(windows) { "fixture" } else { "FIXTURE" };
        let token = manager.reserve(port, None).unwrap();
        let mut other = SessionManager::new();
        assert!(other.reserved_console_source(&token).is_err());
        let source = manager.reserved_console_source(&token).unwrap();
        assert!(manager.reserved_console_source(&token.clone()).is_err());
        let mut console = source.open().unwrap();
        console
            .write_command(b"calibrate\n", &AtomicBool::new(false))
            .unwrap();
        assert_eq!(&*output.lock(), b"calibrate\n");
        assert_eq!(calls.load(Ordering::Relaxed), 4);
        assert_eq!(opens.load(Ordering::Relaxed), 1);
        let mut bytes = [0; 32];
        assert_eq!(console.read(&mut bytes).unwrap(), 8);
        assert_eq!(&bytes[..8], b"fixture\n");
        assert!(console.read(&mut bytes).is_err());
        drop(console);
        manager.release(&token).unwrap();
        assert!(manager.reserved_console_source(&token).is_err());
        let next = manager.reserve(port, None).unwrap();
        drop(manager.reserved_console_source(&next).unwrap());
        assert!(manager.reserved_console_source(&next).is_err());
        manager.release(&next).unwrap();
    }

    #[test]
    fn reserved_console_whole_command_never_replays_after_uncertain_write() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let calls = Arc::new(AtomicUsize::new(0));
        let mut manager = SessionManager::with_factory({
            let output = Arc::clone(&output);
            let calls = Arc::clone(&calls);
            Arc::new(move |_| {
                Ok(Box::new(FixtureConsole {
                    input: Cursor::new(b""),
                    output: Arc::clone(&output),
                    calls: Arc::clone(&calls),
                    fail_second: true,
                }))
            })
        });
        let token = manager.reserve("fixture", None).unwrap();
        let mut console = manager
            .reserved_console_source(&token)
            .unwrap()
            .open()
            .unwrap();
        assert!(
            console
                .write_command(b"calibrate\n", &AtomicBool::new(false))
                .is_err()
        );
        assert_eq!(&*output.lock(), b"cal");
        assert_eq!(calls.load(Ordering::Relaxed), 2);
        drop(console);
        manager.release(&token).unwrap();
    }

    #[test]
    fn reserved_console_source_waits_for_predecessor_join() {
        let (opened_tx, opened_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        let mut manager = SessionManager::with_factory(Arc::new(move |_| {
            opened_tx.send(()).unwrap();
            release_rx
                .lock()
                .recv_timeout(Duration::from_secs(3))
                .unwrap();
            Ok(Box::new(Cursor::new(Vec::<u8>::new())))
        }));
        manager.connect("fixture").unwrap();
        opened_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        let token = manager.reserve("fixture", None).unwrap();
        assert!(manager.reserved_console_source(&token).is_err());
        release_tx.send(()).unwrap();
        super::test_support::until(|| {
            let _ = manager.poll();
            manager.reservation_ready(&token).unwrap()
        });
        drop(manager.reserved_console_source(&token).unwrap());
        assert!(manager.reserved_console_source(&token).is_err());
        manager.release(&token).unwrap();
    }

    #[test]
    fn thermal_dump_admission_includes_quarantined_incomplete_response() {
        let manager = SessionManager::new();
        let request = manager.thermal_dumps.lock().begin("fixture", 1).unwrap();
        assert!(manager.thermal_dump_busy("fixture"));
        let collector = DumpCollector::arm(DumpCommand {
            request: request.clone(),
            requests: Arc::clone(&manager.thermal_dumps),
        })
        .unwrap();
        collector.finish(ThermalDumpOutcome::TimedOut);
        assert!(manager.thermal_dump_busy("fixture"));
        manager
            .thermal_dumps
            .lock()
            .finish_port("fixture", ThermalDumpOutcome::Disconnected);
        assert!(!manager.thermal_dump_busy("fixture"));
    }
}
