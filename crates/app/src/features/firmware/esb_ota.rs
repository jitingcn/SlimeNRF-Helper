//! Exclusive HID OTA engine. Call only after ReceiverMonitor's reservation is ready;
//! retain that reservation until this blocking operation's worker has joined.
#[path = "esb_ota/progress.rs"]
mod progress;
#[path = "esb_ota/protocol.rs"]
mod protocol;
#[cfg(test)]
#[path = "esb_ota/tests.rs"]
mod tests;

use crate::{
    domain::{ReceiverEndpoint, TrackerLink, normalize_address},
    hid_log::{Source, TracedHid},
    update::{
        ActivationSubmission, OtaFailure, OtaFailureKind, OtaPhase, OtaPreparation, OtaStatusPhase,
        Progress, ReceiverActivationReport, ReceiverAvailability, UpdateOutcome,
    },
};
use parking_lot::Mutex;
use progress::Telemetry;
use protocol::*;
use slimenrf_firmware_image::image::{self as firmware, FirmwareImage};
use std::{
    collections::HashSet,
    ffi::CString,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

pub const RECEIVER_ID: u8 = 0xfe;
const SLOTS: usize = 16;
const WINDOW: usize = 111; // Leave headroom in the receiver's 128-entry shared ring.
const QUERY_GAP: Duration = Duration::from_millis(100);
const QUERY_TIMEOUT: Duration = Duration::from_secs(3);
// Some receivers reset without a COMPLETE status; observation must stay brief.
const RECEIVER_ACTIVATION_OBSERVATION: Duration = Duration::from_secs(3);
const PREFLIGHT_QUERY_RETRY_DRAIN: Duration = Duration::from_millis(500);
const TRACKER_BATCH_RECOVERY: Duration = Duration::from_secs(2);
// Receiver esb_ota.c retains scoped ABORT tombstones for ten seconds, and
// connection/esb.c blocks all new radio operations until they expire.
const ABORT_ADMISSION: Duration = Duration::from_secs(10);
static ABORT_DEADLINES: Mutex<Vec<(ReceiverEndpoint, Instant)>> = Mutex::new(Vec::new());

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FirmwareInfo {
    pub version: String,
    pub build_date: String,
    pub firmware_size: u32,
    pub bootloader: String,
    pub protocol_version: u8,
    pub board_target: String,
    pub flash_base: u32,
}

/// Frozen query result; flash revalidates the native interface and every field
/// affecting compatibility, as well as the RF address (a slot alone is not identity).
#[derive(Clone, Debug)]
pub struct Target {
    pub receiver: ReceiverEndpoint,
    pub id: u8,
    pub address: Option<String>,
    pub info: FirmwareInfo,
}

#[derive(Clone, Debug)]
pub struct QueryResult {
    pub receiver: Option<Target>,
    pub trackers: Vec<Target>,
    pub failures: Vec<(u8, String)>,
}

#[derive(Clone, Debug)]
pub struct TargetOutcome {
    pub target: Target,
    pub outcome: UpdateOutcome,
}

/// Images selected for one admitted operation. Receivers get independent,
/// exclusive sessions; all tracker metadata on each receiver is prepared before
/// its first BEGIN, including trackers using different images.
/// Receiver self-updates are deferred until every selected tracker plan finishes.
pub struct FlashGroup<'a> {
    pub targets: &'a [Target],
    pub image: &'a FirmwareImage,
    pub board: &'a str,
}

trait Transport {
    fn write(&mut self, report: &[u8; 64]) -> Result<(), String>;
    fn read(&mut self, frame: &mut [u8; 64], timeout_ms: i32) -> Result<usize, String>;
    fn now(&self) -> Duration;
    fn reconnect(
        &mut self,
        cancel: &AtomicBool,
        deadline: Duration,
    ) -> Result<(), ReceiverOpenError>;
    fn abort_pending_for(&self) -> Duration {
        Duration::ZERO
    }
}

#[derive(Debug)]
enum ReceiverOpenError {
    Unavailable(String),
    Identity(String),
}

impl ReceiverOpenError {
    fn message(self) -> String {
        match self {
            Self::Unavailable(message) | Self::Identity(message) => message,
        }
    }
}

fn validate_receiver_interface(
    endpoint: &ReceiverEndpoint,
    path: &[u8],
    vid: u16,
    pid: u16,
    usage_page: u16,
    serial: Option<&str>,
) -> Result<(), ReceiverOpenError> {
    if path != endpoint.path
        || vid != 0x1209
        || pid != 0x7690
        || usage_page != 1
        || serial.map(str::trim).filter(|s| !s.is_empty()) != endpoint.serial_number.as_deref()
    {
        return Err(ReceiverOpenError::Identity(
            "Receiver identity changed; query and select it again".into(),
        ));
    }
    Ok(())
}

fn open_receiver_handle<H>(
    endpoint: &ReceiverEndpoint,
    open: impl FnOnce() -> Result<H, ReceiverOpenError>,
    read_serial: impl FnOnce(&H) -> Result<Option<String>, String>,
) -> Result<H, ReceiverOpenError> {
    let handle = open()?;
    // A native path can be reused after enumeration; verify the handle itself
    // before exposing it to any operation that can issue HID commands.
    let serial = read_serial(&handle).map_err(|error| {
        ReceiverOpenError::Identity(format!("Cannot verify receiver HID identity: {error}"))
    })?;
    if serial.as_deref().map(str::trim).filter(|s| !s.is_empty())
        != endpoint.serial_number.as_deref()
    {
        return Err(ReceiverOpenError::Identity(
            "Receiver identity changed; query and select it again".into(),
        ));
    }
    Ok(handle)
}

fn reopen_receiver<H>(
    cancel: &AtomicBool,
    deadline: Duration,
    mut open: impl FnMut() -> Result<H, ReceiverOpenError>,
    now: impl Fn() -> Duration,
    mut wait: impl FnMut(Duration),
) -> Result<H, ReceiverOpenError> {
    let mut last_error =
        "Receiver did not return before the activation observation deadline".into();
    while now() < deadline {
        check_cancel(cancel).map_err(ReceiverOpenError::Unavailable)?;
        match open() {
            Ok(handle) => {
                check_cancel(cancel).map_err(ReceiverOpenError::Unavailable)?;
                if now() >= deadline {
                    break;
                }
                return Ok(handle);
            }
            Err(ReceiverOpenError::Identity(error)) => {
                return Err(ReceiverOpenError::Identity(error));
            }
            Err(ReceiverOpenError::Unavailable(error)) => last_error = error,
        }
        let remaining = deadline.saturating_sub(now());
        if !remaining.is_zero() {
            wait(remaining.min(Duration::from_millis(100)));
        }
    }
    Err(ReceiverOpenError::Unavailable(last_error))
}

struct NativeHid {
    device: Option<TracedHid>,
    endpoint: ReceiverEndpoint,
    started: Instant,
}

impl NativeHid {
    fn open(endpoint: &ReceiverEndpoint) -> Result<Self, String> {
        Self::open_checked(endpoint).map_err(ReceiverOpenError::message)
    }

    fn open_checked(endpoint: &ReceiverEndpoint) -> Result<Self, ReceiverOpenError> {
        let devices = crate::native_hid::enumerate()
            .map_err(|e| ReceiverOpenError::Unavailable(e.to_string()))?;
        let path = CString::new(endpoint.path.as_slice())
            .map_err(|_| ReceiverOpenError::Identity("Invalid HID native path".into()))?;
        let device = devices
            .device_list()
            .find(|d| d.path() == path.as_c_str())
            .ok_or_else(|| {
                ReceiverOpenError::Unavailable(
                    "Selected receiver control interface is no longer connected".into(),
                )
            })?;
        validate_receiver_interface(
            endpoint,
            device.path().to_bytes(),
            device.vendor_id(),
            device.product_id(),
            device.usage_page(),
            device.serial_number(),
        )?;
        let device = open_receiver_handle(
            endpoint,
            || {
                devices
                    .open_path(path)
                    .map_err(|e| ReceiverOpenError::Unavailable(format!("Open receiver HID: {e}")))
            },
            |device| device.get_serial_number_string().map_err(|e| e.to_string()),
        )?;
        Ok(Self {
            device: Some(TracedHid::new(device, Source::main(endpoint))),
            endpoint: endpoint.clone(),
            started: Instant::now(),
        })
    }
}

impl Transport for NativeHid {
    fn write(&mut self, report: &[u8; 64]) -> Result<(), String> {
        let mut framed = [0u8; 65];
        framed[1..].copy_from_slice(report);
        let result = self
            .device
            .as_ref()
            .ok_or("HID disconnected")?
            .write(&framed);
        if report[0] == ABORT && (report[1] as usize) < SLOTS {
            // An errored/short write cannot prove the receiver did not accept it.
            let now = Instant::now();
            let mut deadlines = ABORT_DEADLINES.lock();
            deadlines.retain(|(_, until)| *until > now);
            if let Some((_, until)) = deadlines.iter_mut().find(|(endpoint, _)| {
                endpoint.key == self.endpoint.key
                    && endpoint.path == self.endpoint.path
                    && endpoint.serial_number == self.endpoint.serial_number
            }) {
                *until = now + ABORT_ADMISSION;
            } else {
                deadlines.push((self.endpoint.clone(), now + ABORT_ADMISSION));
            }
        }
        let count = result.map_err(|e| format!("HID write: {e}"))?;
        if count != framed.len() {
            return Err(format!("Short HID write: {count}/65 bytes; not retried"));
        }
        Ok(())
    }
    fn read(&mut self, frame: &mut [u8; 64], timeout_ms: i32) -> Result<usize, String> {
        self.device
            .as_ref()
            .ok_or("HID disconnected")?
            .read_timeout(frame, timeout_ms)
            .map_err(|e| format!("HID read: {e}"))
    }
    fn now(&self) -> Duration {
        self.started.elapsed()
    }
    fn abort_pending_for(&self) -> Duration {
        let now = Instant::now();
        let mut deadlines = ABORT_DEADLINES.lock();
        deadlines.retain(|(_, until)| *until > now);
        deadlines
            .iter()
            .find(|(endpoint, _)| {
                endpoint.key == self.endpoint.key
                    && endpoint.path == self.endpoint.path
                    && endpoint.serial_number == self.endpoint.serial_number
            })
            .map_or(Duration::ZERO, |(_, until)| {
                until.saturating_duration_since(now)
            })
    }
    fn reconnect(
        &mut self,
        cancel: &AtomicBool,
        deadline: Duration,
    ) -> Result<(), ReceiverOpenError> {
        self.device.take();
        let mut opened = reopen_receiver(
            cancel,
            deadline,
            || Self::open_checked(&self.endpoint),
            || self.started.elapsed(),
            std::thread::sleep,
        )?;
        self.device = opened.device.take();
        Ok(())
    }
}

fn check_cancel(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) {
        Err("ESB OTA cancelled".into())
    } else {
        Ok(())
    }
}

struct Session<'a, T> {
    io: T,
    cancel: &'a AtomicBool,
    // Observations belong only to this exclusive native Session, never discovery
    // cache. HID registrations are opportunistic padding: full sensor/OTA frames
    // can starve duplicates even while fresh FW_INFO arrives.
    addresses: [Option<String>; SLOTS],
    // A->B->A is not stable identity proof even before this slot is pinned.
    // Keep ambiguity local to that slot until binding; retired slot changes must
    // not poison unrelated batches. A new Session establishes a new proof epoch.
    address_changed: [bool; SLOTS],
    pinned: [Option<String>; SLOTS],
    next_query_at: Duration,
    next_begin_at: Duration,
    unfinished: [bool; SLOTS + 1],
    // Pins and sticky contradictions cover the whole prepared receiver plan,
    // including later image groups, until each target is consumed or abandoned.
    reassigned: Option<usize>,
}

impl<'a, T: Transport> Session<'a, T> {
    fn new(io: T, cancel: &'a AtomicBool) -> Self {
        Self {
            next_query_at: io.now() + QUERY_GAP,
            next_begin_at: io.now() + io.abort_pending_for(),
            unfinished: [false; SLOTS + 1],
            reassigned: None,
            io,
            cancel,
            addresses: std::array::from_fn(|_| None),
            address_changed: [false; SLOTS],
            pinned: std::array::from_fn(|_| None),
        }
    }
    fn write(&mut self, report: &[u8; 64]) -> Result<(), String> {
        check_cancel(self.cancel)?;
        self.check_identity()?;
        self.io.write(report)
    }
    fn write_begin(&mut self, id: u8, image: &FirmwareImage, board: &str) -> Result<(), String> {
        let report = begin(id, image, board)?;
        check_cancel(self.cancel)?;
        self.check_identity()?;
        // Even a short or failed HID write may have delivered BEGIN.
        self.unfinished[if id == RECEIVER_ID {
            SLOTS
        } else {
            id as usize
        }] = true;
        self.io.write(&report)
    }
    fn records(&mut self, timeout_ms: i32) -> Result<([[u8; 16]; 4], usize), String> {
        check_cancel(self.cancel)?;
        self.check_identity()?;
        let mut frame = [0; 64];
        let length = self.io.read(&mut frame, timeout_ms)?;
        if length % 16 != 0 || length > 64 {
            return Err(format!("Malformed control HID report length {length}"));
        }
        let mut records = [[0; 16]; 4];
        for (index, record) in frame[..length].as_chunks::<16>().0.iter().enumerate() {
            records[index].copy_from_slice(record);
            let slot = record[1] as usize;
            if record[0] == 0xff && slot < SLOTS {
                let mut digits = [0u8; 12];
                const HEX: &[u8; 16] = b"0123456789ABCDEF";
                for (index, byte) in record[2..8].iter().rev().enumerate() {
                    digits[index * 2] = HEX[(byte >> 4) as usize];
                    digits[index * 2 + 1] = HEX[(byte & 15) as usize];
                }
                let address = std::str::from_utf8(&digits).unwrap();
                if self.pinned[slot]
                    .as_deref()
                    .is_some_and(|expected| expected != address)
                {
                    self.address_changed[slot] = true;
                    self.reassigned.get_or_insert(slot);
                }
                if self.addresses[slot].as_deref() != Some(address) {
                    self.address_changed[slot] |= self.addresses[slot].is_some();
                    self.addresses[slot] = Some(address.to_owned());
                }
            }
        }
        self.check_identity()?;
        Ok((records, length / 16))
    }
    fn pin(&mut self, id: u8, address: Option<&str>) -> Result<(), String> {
        if id == RECEIVER_ID {
            return self.check_identity();
        }
        let slot = id as usize;
        if slot >= SLOTS {
            return Err(format!("Invalid tracker slot {id}"));
        }
        if self.pinned[slot]
            .as_deref()
            .is_some_and(|pinned| Some(pinned) == address)
        {
            return self.check_identity();
        }
        let expected = address
            .and_then(normalize_address)
            .ok_or("Tracker has no valid immutable RF address")?;
        if self.address_changed[slot]
            || self.pinned[slot]
                .as_deref()
                .is_some_and(|pinned| pinned != expected)
            || self.addresses[slot]
                .as_deref()
                .is_some_and(|observed| observed != expected)
        {
            self.reassigned = Some(slot);
        }
        if self.pinned[slot].is_none() {
            self.pinned[slot] = Some(expected);
        }
        self.check_identity()
    }
    fn release_batch_identity(&mut self) {
        // Release only after abandoning/finishing the entire prepared plan.
        assert!(self.unfinished.iter().all(|unfinished| !unfinished));
        self.pinned.fill(None);
        self.reassigned = None;
    }
    fn query_info(
        &mut self,
        receiver: &ReceiverEndpoint,
        id: u8,
        address: Option<&str>,
    ) -> Result<FirmwareInfo, String> {
        self.pin(id, address)?;
        let links = if id == RECEIVER_ID {
            Vec::new()
        } else {
            // Retain identity observed during acquisition, admission, or a peer
            // query. Fresh full metadata still has its bounded three-second
            // deadline; every later contradictory registration remains fatal.
            // This is session-scoped proof, not a tokenized identity response:
            // an unreported reassignment cannot be detected by this protocol.
            let expected = self.pinned[id as usize].as_ref().unwrap().clone();
            vec![TrackerLink {
                receiver_key: receiver.key.clone(),
                slot: id,
                address: Some(expected),
                online: true,
            }]
        };
        let mut result = query_with_purpose(
            self,
            receiver,
            &links,
            id == RECEIVER_ID,
            QueryPurpose::ExecutionPreflight,
            &mut |_| {},
        )?;
        if let Some((_, error)) = result.failures.pop() {
            return Err(error);
        }
        result
            .receiver
            .or_else(|| result.trackers.pop())
            .map(|target| target.info)
            .ok_or_else(|| format!("Target {id} did not return complete firmware information"))
    }
    fn check_identity(&self) -> Result<(), String> {
        match self.reassigned {
            Some(slot) => Err(format!("Tracker slot {slot} was reassigned; OTA stopped")),
            None => Ok(()),
        }
    }
    fn wait_for_admission(&mut self) -> Result<(), String> {
        self.wait_until(self.next_begin_at)
    }
    fn wait_until(&mut self, deadline: Duration) -> Result<(), String> {
        loop {
            let remaining = deadline.saturating_sub(self.io.now());
            if remaining.is_zero() {
                break;
            }
            self.records(remaining.as_millis().clamp(1, 100) as i32)?;
        }
        check_cancel(self.cancel)?;
        self.check_identity()
    }
    fn retire(&mut self, id: u8) {
        self.unfinished[if id == RECEIVER_ID {
            SLOTS
        } else {
            id as usize
        }] = false;
        if id != RECEIVER_ID {
            self.pinned[id as usize] = None;
        }
    }
    fn fail_and_abort(&mut self, id: u8, error: String) -> String {
        match self.abort(id) {
            Ok(()) => error,
            Err(cleanup) => format!("{error}; OTA abort failed: {cleanup}"),
        }
    }
    fn abort(&mut self, id: u8) -> Result<(), String> {
        let slot = if id == RECEIVER_ID {
            SLOTS
        } else {
            id as usize
        };
        if !self.unfinished[slot] {
            return Ok(());
        }
        // Retire before the attempt: even a transport error must not cause a
        // second ABORT to refresh the receiver's tombstone.
        self.unfinished[slot] = false;
        if id != RECEIVER_ID && self.address_changed[slot] {
            return Err(format!(
                "ABORT skipped: tracker slot {slot} no longer identifies the selected tracker"
            ));
        }
        let result = self.io.write(&command(ABORT, id));
        if id != RECEIVER_ID {
            self.next_begin_at = self.io.now() + ABORT_ADMISSION;
        }
        result
    }
}

pub fn query(
    receiver: &ReceiverEndpoint,
    trackers: &[TrackerLink],
    include_receiver: bool,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(Progress),
) -> Result<QueryResult, String> {
    check_cancel(cancel)?;
    let mut session = Session::new(NativeHid::open(receiver)?, cancel);
    query_with(&mut session, receiver, trackers, include_receiver, progress)
}

#[derive(Clone, Copy)]
enum QueryPurpose {
    Discovery,
    ExecutionPreflight,
}

fn query_with<T: Transport>(
    session: &mut Session<'_, T>,
    receiver: &ReceiverEndpoint,
    trackers: &[TrackerLink],
    include_receiver: bool,
    progress: &mut dyn FnMut(Progress),
) -> Result<QueryResult, String> {
    query_with_purpose(
        session,
        receiver,
        trackers,
        include_receiver,
        QueryPurpose::Discovery,
        progress,
    )
}

fn query_with_purpose<T: Transport>(
    session: &mut Session<'_, T>,
    receiver: &ReceiverEndpoint,
    trackers: &[TrackerLink],
    include_receiver: bool,
    purpose: QueryPurpose,
    progress: &mut dyn FnMut(Progress),
) -> Result<QueryResult, String> {
    struct Pending {
        id: u8,
        address: Option<String>,
        address_bytes: Option<[u8; 6]>,
        validated: bool,
        chunks: InfoChunks,
        info: Option<Result<FirmwareInfo, String>>,
    }

    check_cancel(session.cancel)?;
    let mut pending = Vec::<Pending>::with_capacity(trackers.len().min(SLOTS) + 1);
    if include_receiver {
        pending.push(Pending {
            id: RECEIVER_ID,
            address: None,
            address_bytes: None,
            validated: true,
            chunks: InfoChunks::new(RECEIVER_ID),
            info: None,
        });
    }
    let mut failures = Vec::new();
    let mut invalid_ids = HashSet::new();
    for link in trackers {
        let address = link.address.as_deref().and_then(normalize_address);
        let error = if link.slot as usize >= SLOTS {
            Some(format!("Invalid tracker slot {}", link.slot))
        } else if link.receiver_key != receiver.key {
            Some("Tracker belongs to another receiver".into())
        } else if address.is_none() {
            Some("Tracker RF address is unknown or invalid".into())
        } else {
            None
        };
        if let Some(error) = error {
            if invalid_ids.insert(link.slot) {
                failures.push((link.slot, error));
            }
            continue;
        }
        if let Some(previous) = pending.iter().find(|entry| entry.id == link.slot) {
            if previous.address != address && invalid_ids.insert(link.slot) {
                failures.push((
                    link.slot,
                    "Conflicting RF addresses for tracker slot".into(),
                ));
            }
            continue;
        }
        let expected = address.as_deref().unwrap();
        let bytes = std::array::from_fn(|index| {
            let start = (5 - index) * 2;
            u8::from_str_radix(&expected[start..start + 2], 16).unwrap()
        });
        pending.push(Pending {
            id: link.slot,
            address,
            address_bytes: Some(bytes),
            validated: false,
            chunks: InfoChunks::new(link.slot),
            info: None,
        });
    }
    pending.retain(|entry| entry.id == RECEIVER_ID || !invalid_ids.contains(&entry.id));
    if pending.is_empty() {
        return Ok(QueryResult {
            receiver: None,
            trackers: Vec::new(),
            failures,
        });
    }

    while session.io.now() < session.next_query_at {
        let remaining = session.next_query_at.saturating_sub(session.io.now());
        session.records(remaining.as_millis().clamp(1, 100) as i32)?;
    }
    check_cancel(session.cancel)?;
    let total = pending.len() + failures.len();
    for current in 0..pending.len() {
        let entry = &mut pending[current];
        if entry.id != RECEIVER_ID {
            // QUERY_INFO is a radio operation too: a known ABORT can override
            // it. Receiver-local information needs no radio admission wait.
            session.wait_for_admission()?;
        }
        if let Some(expected) = &entry.address
            && let Some(actual) = &session.addresses[entry.id as usize]
        {
            if actual != expected {
                entry.info = Some(Err(format!("Tracker slot {} address changed", entry.id)));
            } else {
                entry.validated = true;
            }
        }
        if entry.info.is_none() {
            let attempts = match purpose {
                QueryPurpose::Discovery => 1,
                QueryPurpose::ExecutionPreflight => 2,
            };
            for attempt in 0..attempts {
                let id = pending[current].id;
                if let Err(error) = session.write(&command(QUERY, id)) {
                    check_cancel(session.cancel)?;
                    pending[current].info = Some(Err(error));
                    break;
                }
                // The receiver relays every reply through one shared buffer.
                // Five chunks contain the CRC, but only the sixth (empty) chunk
                // marks the end of the relay. Never advance on CRC alone.
                let deadline = session.io.now() + QUERY_TIMEOUT;
                let mut relay_complete = false;
                loop {
                    check_cancel(session.cancel)?;
                    let entry = &pending[current];
                    let resolved = match &entry.info {
                        Some(Err(_)) => true,
                        Some(Ok(_)) => entry.validated,
                        None => false,
                    };
                    if (relay_complete && resolved) || session.io.now() >= deadline {
                        break;
                    }
                    let remaining_ms = deadline.saturating_sub(session.io.now()).as_millis();
                    if remaining_ms == 0 {
                        break;
                    }
                    let (records, length) = session.records(remaining_ms.min(100) as i32)?;
                    check_cancel(session.cancel)?;
                    for record in &records[..length] {
                        if record[0] == 0xff {
                            if let Some(entry) =
                                pending.iter_mut().find(|entry| entry.id == record[1])
                                && let Some(expected) = entry.address_bytes
                            {
                                if record[2..8] != expected {
                                    // Sticky for queued and completed targets too,
                                    // even if the expected address is restored.
                                    entry.info = Some(Err(format!(
                                        "Tracker slot {} was reassigned; query discarded",
                                        entry.id
                                    )));
                                } else {
                                    entry.validated = true;
                                }
                            }
                        } else {
                            let entry = &mut pending[current];
                            if record[0] == INFO && record[1] == entry.id && record[2] == 5 {
                                relay_complete = true;
                            }
                            if entry.info.is_none() {
                                match entry.chunks.push(record) {
                                    Ok(Some(info)) => entry.info = Some(Ok(info)),
                                    Ok(None) => {}
                                    Err(error) => entry.info = Some(Err(error)),
                                }
                            }
                        }
                    }
                }
                let entry = &pending[current];
                if attempt + 1 < attempts && entry.validated && entry.info.is_none() {
                    // Discard late INFO without losing cancellation or sticky
                    // identity observations. The protocol has no transaction token.
                    let drain_until = session.io.now() + PREFLIGHT_QUERY_RETRY_DRAIN;
                    while session.io.now() < drain_until {
                        let remaining = drain_until.saturating_sub(session.io.now());
                        session.records(remaining.as_millis().clamp(1, 100) as i32)?;
                    }
                    check_cancel(session.cancel)?;
                    pending[current].chunks = InfoChunks::new(id);
                    continue;
                }
                // A missing trailer consumes only this attempt's response budget;
                // retain valid metadata. A radio reply arriving after timeout can
                // still overlap a later request without a transaction token.
                // Freeze unresolved results before advancing to another target.
                let entry = &mut pending[current];
                if !matches!(entry.info, Some(Err(_))) {
                    if !entry.validated {
                        entry.info = Some(Err(format!(
                            "Tracker {} RF address could not be revalidated; refresh receiver discovery",
                            entry.id
                        )));
                    } else if entry.info.is_none() {
                        entry.info = Some(Err(format!(
                            "Target {} did not return complete firmware information",
                            entry.id
                        )));
                    }
                }
                break;
            }
        }
        check_cancel(session.cancel)?;
        progress(Progress::bytes(
            "Querying firmware information",
            (current + 1 + failures.len()) as u64,
            total as u64,
        ));
    }
    let mut result = QueryResult {
        receiver: None,
        trackers: Vec::new(),
        failures,
    };
    for entry in pending {
        let queried = match entry.info {
            Some(Err(error)) => Err(error),
            _ if !entry.validated => Err(format!(
                "Tracker {} RF address could not be revalidated; refresh receiver discovery",
                entry.id
            )),
            Some(Ok(info)) => Ok(info),
            None => Err(format!(
                "Target {} did not return complete firmware information",
                entry.id
            )),
        };
        match queried {
            Ok(info) => {
                let target = Target {
                    receiver: receiver.clone(),
                    id: entry.id,
                    address: entry.address,
                    info,
                };
                if entry.id == RECEIVER_ID {
                    result.receiver = Some(target);
                } else {
                    result.trackers.push(target);
                }
            }
            Err(error) => result.failures.push((entry.id, error)),
        }
    }
    Ok(result)
}

fn validate_target(target: &Target, image: &FirmwareImage, board: &str) -> Result<(), String> {
    if target.id != RECEIVER_ID && target.id as usize >= SLOTS {
        return Err("Invalid ESB target slot".into());
    }
    if target.info.board_target != board {
        return Err(format!(
            "Firmware board {board} does not match queried target {}",
            target.info.board_target
        ));
    }
    if target.info.protocol_version != 1 {
        return Err(format!(
            "Unsupported ESB OTA protocol {}",
            target.info.protocol_version
        ));
    }
    match target.info.bootloader.as_str() {
        "nrf5_opendfu" => return Err("nRF5 OpenDFU protects application flash; use serial OpenDFU (HEX/Secure DFU), not ESB self-update".into()),
        "none" | "adafruit_uf2" | "mcuboot" => {}
        _ => return Err("Unknown bootloader; refusing OTA".into()),
    }
    if image.mcuboot != (target.info.bootloader == "mcuboot") {
        return Err(
            "MCUboot targets require an MCUboot update BIN; other targets require UF2/HEX".into(),
        );
    }
    if image.mcuboot && target.info.flash_base != 0 {
        return Err("MCUboot target reported an unexpected nonzero update base".into());
    }
    if !image.mcuboot && target.info.flash_base < 0x1000 {
        return Err("Device did not provide a safe application flash base".into());
    }
    if !image.mcuboot && image.base_address > target.info.flash_base {
        return Err("Firmware requires an application/SoftDevice base above the running device; use the matching image".into());
    }
    Ok(())
}

pub fn flash_groups(
    groups: &[FlashGroup<'_>],
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(Progress),
) -> Result<Vec<TargetOutcome>, String> {
    if groups.iter().any(|group| group.targets.is_empty()) || groups.is_empty() {
        return Err("No ESB targets selected".into());
    }
    let mut remaining: Vec<_> = groups.iter().collect();
    let mut outcomes = Vec::new();
    while !remaining.is_empty() {
        let receiver_groups = take_receiver_groups(&mut remaining);
        let receiver = &receiver_groups[0].targets[0].receiver;
        let opened = (|| {
            validate_groups(&receiver_groups)?;
            check_cancel(cancel)?;
            NativeHid::open(receiver)
        })();
        let result = opened.and_then(|io| {
            flash_validated(&mut Session::new(io, cancel), &receiver_groups, progress)
        });
        match result {
            Ok(results) => outcomes.extend(results),
            Err(error) => {
                let targets: Vec<_> = receiver_groups
                    .iter()
                    .flat_map(|group| group.targets.iter().cloned())
                    .collect();
                let mut telemetry = Telemetry::new(&targets);
                telemetry.fail_pending(&error, Duration::ZERO);
                telemetry.publish(Duration::ZERO, progress);
                outcomes.extend(targets.into_iter().map(|target| TargetOutcome {
                    target,
                    outcome: UpdateOutcome::Failed(error.clone()),
                }));
            }
        }
    }
    Ok(outcomes)
}

fn take_receiver_groups<'group, 'image>(
    remaining: &mut Vec<&'group FlashGroup<'image>>,
) -> Vec<&'group FlashGroup<'image>> {
    // Preserve the global tracker-before-receiver contract, not merely order
    // within a receiver. A self-update can close/reopen its session later without
    // reacquiring tracker metadata at any post-COMPLETE image-group boundary.
    let Some(first) = remaining
        .iter()
        .copied()
        .find(|group| group.targets[0].id != RECEIVER_ID)
        .or_else(|| remaining.first().copied())
    else {
        return Vec::new();
    };
    let receiver = &first.targets[0].receiver;
    let self_update = first.targets[0].id == RECEIVER_ID;
    let mut groups = Vec::new();
    remaining.retain(|group| {
        let endpoint = &group.targets[0].receiver;
        if endpoint.key == receiver.key
            && endpoint.path == receiver.path
            && endpoint.serial_number == receiver.serial_number
            && (group.targets[0].id == RECEIVER_ID) == self_update
        {
            groups.push(*group);
            false
        } else {
            true
        }
    });
    groups
}

fn validate_selection(
    targets: &[Target],
    image: &FirmwareImage,
    board: &str,
) -> Result<(), String> {
    let first = targets.first().ok_or("No ESB targets selected")?;
    let mut ids = HashSet::new();
    for target in targets {
        if target.receiver.key != first.receiver.key
            || target.receiver.path != first.receiver.path
            || target.receiver.serial_number != first.receiver.serial_number
        {
            return Err("An ESB batch must use one frozen receiver interface".into());
        }
        if !ids.insert(target.id) {
            return Err("Duplicate ESB target".into());
        }
        if target.id == RECEIVER_ID && targets.len() != 1 {
            return Err("Receiver self-update cannot run together with tracker updates".into());
        }
        validate_target(target, image, board)?;
    }
    let mcuboot_magic = image.data.get(..4) == Some(0x96f3b83du32.to_le_bytes().as_slice());
    if image.mcuboot != mcuboot_magic || (image.mcuboot && image.base_address != 0) {
        return Err("Firmware format flag does not match MCUboot image header/base".into());
    }
    if image.data.is_empty() || image.data.len().div_ceil(PAYLOAD) > u16::MAX as usize {
        return Err("ESB firmware is empty or exceeds the protocol packet limit".into());
    }
    if crc32fast::hash(&image.data) != image.crc32 {
        return Err("Firmware bytes do not match the frozen CRC32".into());
    }
    Ok(())
}

fn validate_groups(groups: &[&FlashGroup<'_>]) -> Result<(), String> {
    let first = groups
        .first()
        .and_then(|group| group.targets.first())
        .ok_or("No ESB targets selected")?;
    let mut ids = HashSet::new();
    for group in groups {
        validate_selection(group.targets, group.image, group.board)?;
        for target in group.targets {
            if target.receiver.key != first.receiver.key
                || target.receiver.path != first.receiver.path
                || target.receiver.serial_number != first.receiver.serial_number
            {
                return Err("An ESB plan must use one frozen receiver interface".into());
            }
            if !ids.insert(target.id) {
                return Err("Duplicate ESB target across image groups".into());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
fn flash_with<T: Transport>(
    session: &mut Session<'_, T>,
    targets: &[Target],
    image: &FirmwareImage,
    board: &str,
    progress: &mut dyn FnMut(Progress),
) -> Result<Vec<TargetOutcome>, String> {
    flash_groups_with(
        session,
        &[&FlashGroup {
            targets,
            image,
            board,
        }],
        progress,
    )
}

#[cfg(test)]
fn flash_groups_with<T: Transport>(
    session: &mut Session<'_, T>,
    groups: &[&FlashGroup<'_>],
    progress: &mut dyn FnMut(Progress),
) -> Result<Vec<TargetOutcome>, String> {
    validate_groups(groups)?;
    flash_validated(session, groups, progress)
}

fn flash_validated<T: Transport>(
    session: &mut Session<'_, T>,
    groups: &[&FlashGroup<'_>],
    progress: &mut dyn FnMut(Progress),
) -> Result<Vec<TargetOutcome>, String> {
    let targets: Vec<_> = groups.iter().flat_map(|group| group.targets).collect();
    let mut telemetry = Telemetry::new(targets.iter().copied());
    telemetry.publish(session.io.now(), progress);
    let mut results: Vec<_> = targets
        .iter()
        .map(|target| TargetOutcome {
            target: (*target).clone(),
            outcome: UpdateOutcome::Failed("Not started".into()),
        })
        .collect();
    let mut ready = vec![false; targets.len()];

    // At most sixteen tracker slots plus the receiver are admitted. Prepared
    // metadata and RF pins belong to this one exclusive session, never UI cache.
    // Clip each image/base once and keep its packet policy separate from peers.
    struct Prepared<'a> {
        indices: Vec<usize>,
        image: std::borrow::Cow<'a, FirmwareImage>,
        board: &'a str,
        parallel: usize,
    }
    let mut prepared = Vec::<Prepared<'_>>::new();
    let mut offset = 0;
    for group in groups {
        let mut remaining: Vec<_> = (offset..offset + group.targets.len()).collect();
        offset += group.targets.len();
        while let Some(&first) = remaining.first() {
            let base = targets[first].info.flash_base;
            let mut indices = Vec::new();
            remaining.retain(|&index| {
                if targets[index].info.flash_base == base {
                    indices.push(index);
                    false
                } else {
                    true
                }
            });
            match firmware::prepare_for_target(group.image, base) {
                Ok(image) => prepared.push(Prepared {
                    indices,
                    image,
                    board: group.board,
                    parallel: if targets[first].id != RECEIVER_ID
                        && group.board.contains("nrf52840")
                    {
                        2
                    } else {
                        1
                    },
                }),
                Err(error) => {
                    let error: String = error.into();
                    for index in indices {
                        results[index].outcome = UpdateOutcome::Failed(error.clone());
                        telemetry.fail(index, &error, session.io.now());
                    }
                }
            }
        }
    }
    // Receiver self-update must be last even if its image appeared first.
    prepared.sort_by_key(|plan| targets[plan.indices[0]].id == RECEIVER_ID);
    if targets.iter().any(|target| target.id != RECEIVER_ID)
        && let Err(error) = session.query_info(&targets[0].receiver, RECEIVER_ID, None)
    {
        telemetry.fail_pending(&error, session.io.now());
        telemetry.publish(session.io.now(), progress);
        return Err(error);
    }
    let preparation = (|| {
        for plan in &prepared {
            for &index in &plan.indices {
                let target = &targets[index];
                if target.id == RECEIVER_ID {
                    continue; // Fresh self-update authorization happens last.
                }
                telemetry.prepare(index, plan.image.data.len(), session.io.now());
                telemetry.publish(session.io.now(), progress);
                let validated = (|| {
                    session.pin(target.id, target.address.as_deref())?;
                    if session.io.now() < session.next_begin_at {
                        telemetry.preparation(
                            index,
                            OtaPreparation::ReceiverRecovery,
                            session.io.now(),
                        );
                        telemetry.publish(session.io.now(), progress);
                    }
                    session.wait_for_admission()?;
                    telemetry.preparation(index, OtaPreparation::QueryingDevice, session.io.now());
                    telemetry.publish(session.io.now(), progress);
                    let current = session.query_info(
                        &target.receiver,
                        target.id,
                        target.address.as_deref(),
                    )?;
                    if current != target.info {
                        return Err(format!(
                            "Target {} firmware/identity changed since confirmation; query again",
                            target.id
                        ));
                    }
                    Ok(())
                })();
                match validated {
                    Ok(()) => {
                        ready[index] = true;
                        telemetry.clear_preparation(index);
                        telemetry.phase(index, OtaPhase::Queued, session.io.now());
                    }
                    Err(error) => {
                        results[index].outcome = UpdateOutcome::Failed(error.clone());
                        telemetry.fail(index, &error, session.io.now());
                        session.retire(target.id);
                        check_cancel(session.cancel)?;
                        if let Err(identity_error) = session.check_identity() {
                            // Abandon every already-prepared identity together
                            // before releasing sticky evidence. Unqueried slots
                            // may still establish their own session/history proof.
                            for peer in 0..ready.len() {
                                if ready[peer] {
                                    ready[peer] = false;
                                    results[peer].outcome =
                                        UpdateOutcome::Failed(identity_error.clone());
                                    telemetry.fail(peer, &identity_error, session.io.now());
                                }
                            }
                            session.release_batch_identity();
                        }
                    }
                }
                telemetry.publish(session.io.now(), progress);
            }
        }
        Ok::<(), String>(())
    })();
    if let Err(error) = preparation {
        for (index, result) in results.iter_mut().enumerate() {
            if matches!(&result.outcome, UpdateOutcome::Failed(error) if error == "Not started") {
                result.outcome = UpdateOutcome::Failed(error.clone());
                telemetry.fail(index, &error, session.io.now());
            }
        }
        session.release_batch_identity();
        telemetry.outcomes(&results, session.io.now());
        telemetry.publish(session.io.now(), progress);
        return Ok(results);
    }

    let mut tracker_ready_at = Duration::ZERO;
    let mut uncertain_tracker_slots = None;
    for plan in prepared {
        let receiver_update = targets[plan.indices[0]].id == RECEIVER_ID;
        if receiver_update {
            let index = plan.indices[0];
            let target = &targets[index];
            telemetry.prepare(index, plan.image.data.len(), session.io.now());
            telemetry.preparation(index, OtaPreparation::QueryingDevice, session.io.now());
            telemetry.publish(session.io.now(), progress);
            let validated = session
                .query_info(&target.receiver, target.id, None)
                .and_then(|current| {
                    if current == target.info {
                        Ok(())
                    } else {
                        Err(
                            "Receiver firmware/identity changed since confirmation; query again"
                                .into(),
                        )
                    }
                });
            match validated {
                Ok(()) => ready[index] = true,
                Err(error) => {
                    results[index].outcome = UpdateOutcome::Failed(error.clone());
                    telemetry.fail(index, &error, session.io.now());
                }
            }
        }
        let active: Vec<_> = plan
            .indices
            .into_iter()
            .filter(|&index| ready[index])
            .collect();
        for batch in active.chunks(plan.parallel) {
            if !receiver_update && let Some(slots) = uncertain_tracker_slots {
                let cancelled = check_cancel(session.cancel).err();
                for &index in batch {
                    results[index].outcome = if let Some(error) = &cancelled {
                        UpdateOutcome::Failed(error.clone())
                    } else {
                        UpdateOutcome::OtaFailed(OtaFailureKind::BeginBatchSkipped { slots }.into())
                    };
                    session.retire(targets[index].id);
                }
                continue;
            }

            let deadline = if receiver_update {
                session.next_begin_at
            } else {
                session.next_begin_at.max(tracker_ready_at)
            };
            let now = session.io.now();
            if now < deadline {
                for &index in batch {
                    telemetry.preparation(index, OtaPreparation::ReceiverRecovery, now);
                }
                telemetry.publish(now, progress);
            }
            let result = session
                .wait_until(deadline)
                .map_err(BatchError::Failed)
                .and_then(|()| {
                    run_batch(
                        session,
                        batch,
                        &plan.image,
                        plan.board,
                        &mut results,
                        progress,
                        &mut telemetry,
                    )
                });
            let started_tracker_batch = !receiver_update
                && (result.is_ok()
                    || batch
                        .iter()
                        .any(|&index| session.unfinished[targets[index].id as usize]));
            match result {
                Ok(()) => {}
                Err(BatchError::Failed(error)) => {
                    for &index in batch {
                        if matches!(&results[index].outcome, UpdateOutcome::Failed(error) if error == "Not started")
                        {
                            results[index].outcome = UpdateOutcome::Failed(error.clone());
                        }
                    }
                }
                Err(BatchError::TrackerAdmissionUncertain { slots }) => {
                    uncertain_tracker_slots = Some(slots);
                }
            }
            // COMPLETE already retires the firmware participant. Successful
            // targets get no ABORT and lose only their own pin; future prepared
            // targets remain pinned through reads/admission and later groups.
            for &index in batch {
                let id = targets[index].id;
                match &mut results[index].outcome {
                    UpdateOutcome::Failed(error) => {
                        if let Err(cleanup) = session.abort(id) {
                            error.push_str("; OTA abort failed: ");
                            error.push_str(&cleanup);
                        }
                    }
                    UpdateOutcome::OtaFailed(failure) => {
                        if let Err(cleanup) = session.abort(id) {
                            failure.abort_error = Some(cleanup);
                        }
                    }
                    _ => {}
                }
                session.retire(id);
            }
            let now = session.io.now();
            if started_tracker_batch {
                tracker_ready_at = now + TRACKER_BATCH_RECOVERY;
            }
            telemetry.outcomes(&results, now);
            telemetry.publish(now, progress);
        }
    }
    session.release_batch_identity();
    telemetry.outcomes(&results, session.io.now());
    telemetry.publish(session.io.now(), progress);
    Ok(results)
}

#[derive(Clone)]
struct TransferState {
    index: usize,
    id: u8,
    next: usize,
    ring: u8,
    verified: bool,
}

#[derive(Debug)]
enum BatchError {
    Failed(String),
    TrackerAdmissionUncertain { slots: u16 },
}

impl From<String> for BatchError {
    fn from(error: String) -> Self {
        Self::Failed(error)
    }
}

fn fail_tracker_begin(
    results: &mut [TargetOutcome],
    indices: impl IntoIterator<Item = usize>,
    slots: u16,
    diagnostic: Option<&str>,
    cancel: &AtomicBool,
) -> BatchError {
    let cancelled = check_cancel(cancel).err();
    for index in indices {
        let result = &mut results[index];
        result.outcome = if let Some(error) = &cancelled {
            UpdateOutcome::Failed(error.clone())
        } else {
            let kind = if slots & (1 << result.target.id) != 0 {
                OtaFailureKind::BeginUnconfirmed
            } else {
                OtaFailureKind::BeginPeerUnconfirmed { slots }
            };
            UpdateOutcome::OtaFailed(OtaFailure {
                kind,
                diagnostic: diagnostic.map(str::to_owned),
                abort_error: None,
            })
        };
    }
    BatchError::TrackerAdmissionUncertain { slots }
}

fn run_batch<T: Transport>(
    session: &mut Session<'_, T>,
    indices: &[usize],
    image: &FirmwareImage,
    board: &str,
    results: &mut [TargetOutcome],
    progress: &mut dyn FnMut(Progress),
    telemetry: &mut Telemetry,
) -> Result<(), BatchError> {
    let mut states = Vec::with_capacity(indices.len());
    for &index in indices {
        telemetry.preparation(index, OtaPreparation::StartingUpdate, session.io.now());
    }
    telemetry.publish(session.io.now(), progress);
    for &index in indices {
        let id = results[index].target.id;
        if let Err(error) = session.write_begin(id, image, board) {
            let slots = indices
                .iter()
                .map(|&index| results[index].target.id)
                .filter(|&id| id != RECEIVER_ID && session.unfinished[usize::from(id)])
                .fold(0, |slots, id| slots | (1 << id));
            return if slots == 0 {
                Err(error.into())
            } else {
                Err(fail_tracker_begin(
                    results,
                    indices.iter().copied(),
                    slots,
                    Some(&error),
                    session.cancel,
                ))
            };
        }
        states.push(TransferState {
            index,
            id,
            next: 0,
            ring: 0,
            verified: false,
        });
    }
    phase(
        session,
        &mut states,
        results,
        BEGIN,
        image,
        board,
        Duration::from_secs(15),
        &[1, 2],
        progress,
        telemetry,
    )?;
    if states.is_empty() {
        return Ok(());
    }
    for state in &states {
        telemetry.phase(state.index, OtaPhase::Transferring, session.io.now());
    }
    telemetry.publish(session.io.now(), progress);
    if states[0].id == RECEIVER_ID {
        stream_receiver(session, &mut states, image, results, progress, telemetry)?;
    } else {
        stream_trackers(session, &mut states, image, results, progress, telemetry)?;
    }
    for state in &states {
        telemetry.phase(state.index, OtaPhase::Verifying, session.io.now());
    }
    telemetry.publish(session.io.now(), progress);
    for state in states.iter().filter(|state| !state.verified) {
        session.write(&command(VERIFY, state.id))?;
    }
    phase(
        session,
        &mut states,
        results,
        VERIFY,
        image,
        board,
        Duration::from_secs(30),
        &[3],
        progress,
        telemetry,
    )?;
    if states.is_empty() {
        return Ok(());
    }
    for state in &states {
        telemetry.phase(state.index, OtaPhase::Activating, session.io.now());
    }
    telemetry.publish(session.io.now(), progress);
    if states[0].id != RECEIVER_ID {
        for state in &states {
            session.write(&command(ACTIVATE, state.id))?;
        }
    }
    if states[0].id == RECEIVER_ID {
        let index = states[0].index;
        results[index].outcome = activate_receiver(session, &results[index].target, image);
    } else {
        phase(
            session,
            &mut states,
            results,
            ACTIVATE,
            image,
            board,
            Duration::from_secs(20),
            &[6],
            progress,
            telemetry,
        )?;
        for state in states {
            results[state.index].outcome = UpdateOutcome::Completed;
        }
    }
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "Explicit protocol parameters, transfer state, and progress sinks avoid artificial argument grouping"
)]
fn phase<T: Transport>(
    session: &mut Session<'_, T>,
    states: &mut Vec<TransferState>,
    results: &mut [TargetOutcome],
    opcode: u8,
    image: &FirmwareImage,
    board: &str,
    timeout: Duration,
    accepted: &[u8],
    progress: &mut dyn FnMut(Progress),
    telemetry: &mut Telemetry,
) -> Result<(), BatchError> {
    let deadline = session.io.now() + timeout;
    let mut retry = session.io.now() + Duration::from_secs(3);
    let status_phase = match opcode {
        BEGIN => OtaStatusPhase::Begin,
        VERIFY => OtaStatusPhase::Verify,
        ACTIVATE => OtaStatusPhase::Activate,
        _ => unreachable!("unsupported OTA phase opcode {opcode:#04x}"),
    };
    let slot = |id| {
        if id == RECEIVER_ID {
            SLOTS
        } else {
            usize::from(id)
        }
    };
    let mut done = [false; SLOTS + 1];
    if opcode == VERIFY {
        for state in states.iter() {
            done[slot(state.id)] = state.verified;
        }
    }
    let mut stale_slots = 0;
    let wait_result = 'listen: loop {
        if states.iter().all(|state| done[slot(state.id)]) || session.io.now() >= deadline {
            break Ok(());
        }
        let (records, count) = match session.records(100) {
            Ok(frame) => frame,
            Err(error) => break Err(error),
        };
        for status in records[..count].iter().filter_map(Status::parse) {
            let Some(position) = states.iter().position(|s| s.id == status.id) else {
                continue;
            };
            let acknowledged = done[slot(status.id)];
            // READY is not final while another tracker's BEGIN is pending. A
            // later terminal status can remove that member before any DATA.
            if acknowledged && opcode != BEGIN {
                continue;
            }
            if let Some(kind) = status.refusal(status_phase) {
                let state = states.remove(position);
                results[state.index].outcome = refusal_outcome(session, state.id, kind);
            } else if let Some(error) = status.failure().or_else(|| match status.code {
                0x14 => Some("Device rejected OTA sequence/byte count".into()),
                6 if opcode == BEGIN && status.id != RECEIVER_ID => {
                    Some("Device reported OTA completion before transfer".into())
                }
                _ => None,
            }) {
                let state = states.remove(position);
                results[state.index].outcome =
                    UpdateOutcome::Failed(session.fail_and_abort(state.id, error));
            } else if !acknowledged && accepted.contains(&status.code) {
                if (opcode == VERIFY || opcode == ACTIVATE)
                    && (status.next != image.data.len().div_ceil(PAYLOAD)
                        || status.bytes as usize != image.data.len())
                {
                    let state = states.remove(position);
                    results[state.index].outcome = UpdateOutcome::Failed(session.fail_and_abort(
                        state.id,
                        "Device claimed terminal OTA status without the complete image".into(),
                    ));
                    continue;
                }
                if opcode == BEGIN && (status.next != 0 || status.bytes != 0) {
                    let state = states.remove(position);
                    let error =
                        "Device retained an earlier OTA session; abort and query again".into();
                    results[state.index].outcome = if state.id == RECEIVER_ID {
                        UpdateOutcome::Failed(session.fail_and_abort(state.id, error))
                    } else {
                        stale_slots |= 1 << state.id;
                        UpdateOutcome::OtaFailed(OtaFailure {
                            diagnostic: Some(error),
                            ..OtaFailureKind::BeginUnconfirmed.into()
                        })
                    };
                } else {
                    done[slot(status.id)] = true;
                    // The receiver may omit its final DATA status. A matching
                    // VERIFY_OK proves those bytes without implying activation.
                    if opcode == VERIFY {
                        telemetry.verified_bytes(states[position].index);
                    }
                    if opcode == ACTIVATE {
                        results[states[position].index].outcome = UpdateOutcome::Completed;
                        session.retire(status.id);
                    }
                }
            }
        }
        telemetry.outcomes(results, session.io.now());
        telemetry.publish(session.io.now(), progress);
        if stale_slots != 0 {
            break Ok(());
        }
        if session.io.now() >= retry && session.io.now() < deadline {
            for state in states.iter().filter(|s| !done[slot(s.id)]) {
                let written = if opcode == BEGIN {
                    session.write_begin(state.id, image, board)
                } else {
                    session.write(&command(opcode, state.id))
                };
                if let Err(error) = written {
                    break 'listen Err(error);
                }
            }
            retry = session.io.now() + Duration::from_secs(3);
        }
    };
    if opcode == BEGIN {
        let slots = states
            .iter()
            .filter(|s| s.id != RECEIVER_ID && !done[slot(s.id)])
            .fold(stale_slots, |slots, state| slots | (1 << state.id));
        if slots != 0 {
            // DATA feeds one shared ring, irrespective of the slot in the HID
            // report. A targeted ABORT is cleanup, not proof of its delivery.
            return Err(fail_tracker_begin(
                results,
                states.iter().map(|state| state.index),
                slots,
                wait_result.as_ref().err().map(String::as_str),
                session.cancel,
            ));
        }
    }
    wait_result?;
    states.retain(|state| {
        if done[slot(state.id)] {
            true
        } else {
            results[state.index].outcome = UpdateOutcome::Failed(session.fail_and_abort(
                state.id,
                format!(
                    "Target {} did not acknowledge ESB command 0x{opcode:02X}",
                    state.id
                ),
            ));
            false
        }
    });
    telemetry.outcomes(results, session.io.now());
    telemetry.publish(session.io.now(), progress);
    Ok(())
}

fn refusal_outcome<T: Transport>(
    session: &mut Session<'_, T>,
    id: u8,
    kind: OtaFailureKind,
) -> UpdateOutcome {
    let mut failure = OtaFailure::from(kind);
    if kind == OtaFailureKind::TrackerBeginNeedsDirectLink {
        // This receiver refused before admitting the tracker.
        session.retire(id);
    } else {
        failure.abort_error = session.abort(id).err();
    }
    UpdateOutcome::OtaFailed(failure)
}

fn consume<T: Transport>(
    session: &mut Session<'_, T>,
    states: &mut Vec<TransferState>,
    image: &FirmwareImage,
    results: &mut [TargetOutcome],
    sent: usize,
    timeout_ms: i32,
) -> Result<bool, String> {
    let (records, count) = session.records(timeout_ms)?;
    let mut advanced = false;
    for status in records[..count].iter().filter_map(Status::parse) {
        if status.is_begin_busy() {
            continue; // A queued BEGIN retry response is not DATA progress.
        }
        let Some(position) = states.iter().position(|s| s.id == status.id) else {
            continue;
        };
        if let Some(kind) = status.refusal(OtaStatusPhase::Transfer) {
            let state = states.remove(position);
            results[state.index].outcome = refusal_outcome(session, state.id, kind);
            continue;
        }
        let failure = status.failure().or_else(|| {
            if status.next > sent || status.bytes as usize > image.data.len() || status.ring > 128 {
                Some("Device reported impossible OTA progress".into())
            } else if status.id == RECEIVER_ID && status.code == 0x14 {
                Some("Receiver rejected a DATA sequence gap".into())
            } else if status.code == 3
                && (status.next != image.data.len().div_ceil(PAYLOAD)
                    || status.bytes as usize != image.data.len())
            {
                Some("Device claimed CRC verification before receiving the complete image".into())
            } else if ![1, 2, 3, 0x14].contains(&status.code) {
                Some(format!(
                    "Unexpected OTA transfer status 0x{:02X}",
                    status.code
                ))
            } else {
                None
            }
        });
        if let Some(error) = failure {
            let state = states.remove(position);
            results[state.index].outcome =
                UpdateOutcome::Failed(session.fail_and_abort(state.id, error));
        } else {
            let state = &mut states[position];
            advanced |= status.next > state.next;
            state.next = state.next.max(status.next);
            state.ring = status.ring;
            // In-place RAM engines verify automatically after final DATA and
            // transition directly to VERIFY_OK, without a final RECEIVING.
            state.verified |= status.code == 3;
        }
    }
    Ok(advanced)
}

fn stream_trackers<T: Transport>(
    session: &mut Session<'_, T>,
    states: &mut Vec<TransferState>,
    image: &FirmwareImage,
    results: &mut [TargetOutcome],
    progress: &mut dyn FnMut(Progress),
    telemetry: &mut Telemetry,
) -> Result<(), String> {
    let total = image.data.len().div_ceil(PAYLOAD);
    let deadline = session.io.now() + Duration::from_secs(120);
    let mut last_advance = session.io.now();
    let mut sent = 0;
    let mut highest_sent = 0;
    let mut retries = 0;
    let mut warmup = true;
    while !states.is_empty() && session.io.now() < deadline {
        let consumed = states.iter().map(|s| s.next).min().unwrap();
        if consumed == total {
            return Ok(());
        }
        if session.io.now().saturating_sub(last_advance) >= Duration::from_secs(2) {
            if retries == 5 {
                return Err("ESB transfer stalled after five gap-fill attempts".into());
            }
            // Refill the missing window, not BEGIN (which resets staged flash).
            sent = consumed;
            retries += 1;
            last_advance = session.io.now();
            warmup = false;
        }
        let in_flight = sent.saturating_sub(consumed);
        let ring = states.iter().map(|s| s.ring as usize).max().unwrap_or(0);
        let burst = (if warmup { 8 } else { 48 })
            .min(WINDOW.saturating_sub(in_flight))
            .min(127usize.saturating_sub(ring))
            .min(total - sent);
        for _ in 0..burst {
            session.write(&data(states[0].id, sent, &image.data))?;
            sent += 1;
        }
        highest_sent = highest_sent.max(sent);
        if consume(
            session,
            states,
            image,
            results,
            highest_sent,
            if burst == 0 { 100 } else { 20 },
        )? {
            last_advance = session.io.now();
            warmup = false;
        }
        telemetry.transfer(states, session.io.now());
        telemetry.outcomes(results, session.io.now());
        telemetry.publish(session.io.now(), progress);
    }
    if states.is_empty() {
        Ok(())
    } else {
        Err("ESB transfer exceeded 120 seconds".into())
    }
}

fn stream_receiver<T: Transport>(
    session: &mut Session<'_, T>,
    states: &mut Vec<TransferState>,
    image: &FirmwareImage,
    results: &mut [TargetOutcome],
    progress: &mut dyn FnMut(Progress),
    telemetry: &mut Telemetry,
) -> Result<(), String> {
    let total = image.data.len().div_ceil(PAYLOAD);
    let deadline = session.io.now() + Duration::from_secs(120);
    let mut sent = 0;
    while sent < total && !states.is_empty() {
        if session.io.now() >= deadline {
            return Err("Receiver transfer exceeded 120 seconds".into());
        }
        // Web yields to input every five sequential receiver packets. Pump
        // statuses at the same boundary, without replaying blocked HID writes.
        for _ in 0..5.min(total - sent) {
            session.write(&data(RECEIVER_ID, sent, &image.data))?;
            sent += 1;
        }
        consume(session, states, image, results, sent, 1)?;
        telemetry.transfer(states, session.io.now());
        telemetry.outcomes(results, session.io.now());
        telemetry.publish(session.io.now(), progress);
    }
    // Receiver status is throttled to 20ms and need not acknowledge the final
    // DATA. VERIFY flushes the writer queue and checks byte count plus CRC32.
    // Unlike tracker relay DATA, a receiver sequence gap is terminal.
    Ok(())
}

fn receiver_activation_status(
    record: &[u8; 16],
    image: &FirmwareImage,
    submission: &mut ActivationSubmission,
) -> Result<bool, UpdateOutcome> {
    let Some(status) = Status::parse(record) else {
        return Ok(false);
    };
    if status.id != RECEIVER_ID {
        return if matches!(status.code, 5 | 6) {
            Err(UpdateOutcome::Failed(
                "Activation status returned an unexpected receiver identity".into(),
            ))
        } else {
            Ok(false)
        };
    }
    if let Some(kind) = status.refusal(crate::update::OtaStatusPhase::Activate) {
        return Err(UpdateOutcome::OtaFailed(kind.into()));
    }
    if let Some(error) = status.failure() {
        return Err(UpdateOutcome::Failed(error));
    }
    if !matches!(status.code, 1..=3 | 5 | 6) || status.ring > 128 {
        return Err(UpdateOutcome::Failed(format!(
            "Unexpected receiver activation status 0x{:02X}",
            status.code
        )));
    }
    if status.next > image.data.len().div_ceil(PAYLOAD)
        || status.bytes as usize > image.data.len()
        || (matches!(status.code, 5 | 6)
            && (status.next != image.data.len().div_ceil(PAYLOAD)
                || status.bytes as usize != image.data.len()))
    {
        return Err(UpdateOutcome::Failed(
            "Receiver claimed activation status without the complete image".into(),
        ));
    }
    if status.code == 5 {
        *submission = ActivationSubmission::Accepted;
    }
    Ok(status.code == 6)
}

fn activate_receiver<T: Transport>(
    session: &mut Session<'_, T>,
    target: &Target,
    image: &FirmwareImage,
) -> UpdateOutcome {
    if let Err(error) = check_cancel(session.cancel).and_then(|()| session.check_identity()) {
        return UpdateOutcome::Failed(error);
    }
    let deadline = session.io.now() + RECEIVER_ACTIVATION_OBSERVATION;
    session.retire(RECEIVER_ID);
    let mut submission = match session.io.write(&command(ACTIVATE, RECEIVER_ID)) {
        Ok(()) => ActivationSubmission::Sent,
        Err(error) => ActivationSubmission::DeliveryUncertain(error),
    };
    let mut availability = None;
    let mut pending_reason = "Receiver activation completion has not been confirmed";
    let mut reopen = false;
    'connection: while session.io.now() < deadline {
        if reopen {
            match session.io.reconnect(session.cancel, deadline) {
                Ok(()) => {}
                Err(ReceiverOpenError::Identity(error)) => return UpdateOutcome::Failed(error),
                Err(ReceiverOpenError::Unavailable(error)) => {
                    availability = Some(ReceiverAvailability::Unconfirmed(error));
                    break;
                }
            }
        }
        let mut queried = false;
        let mut chunks = None;
        while session.io.now() < deadline {
            // The original handle already passed identity checks. Query it once
            // after acceptance; reopen only if transport observation fails.
            if !queried && (reopen || submission == ActivationSubmission::Accepted) {
                queried = true;
                match session.write(&command(QUERY, RECEIVER_ID)) {
                    Ok(()) => {
                        chunks = Some(InfoChunks::new(RECEIVER_ID));
                        availability = None;
                        pending_reason = "Receiver did not return complete firmware information after activation";
                    }
                    Err(error) => {
                        availability = Some(ReceiverAvailability::Unconfirmed(error));
                        if session.cancel.load(Ordering::Relaxed) {
                            break 'connection;
                        }
                        if reopen {
                            continue 'connection;
                        }
                        // A failed query write need not prevent late status reads.
                    }
                }
            }
            let remaining = deadline.saturating_sub(session.io.now());
            if remaining.is_zero() {
                break 'connection;
            }
            let (records, count) = match session.records(remaining.as_millis().clamp(1, 100) as i32)
            {
                Ok(frame) => frame,
                Err(error) if session.cancel.load(Ordering::Relaxed) => {
                    availability = Some(ReceiverAvailability::Unconfirmed(error));
                    break 'connection;
                }
                Err(error) if error.starts_with("HID read:") => {
                    availability = Some(ReceiverAvailability::Unconfirmed(error));
                    reopen = true;
                    continue 'connection;
                }
                Err(error) => return UpdateOutcome::Failed(error),
            };
            if session.io.now() > deadline {
                break 'connection;
            }
            let records = &records[..count];
            let mut complete = false;
            let mut accepted_at = None;
            // Check the entire report for status failures before deciding success.
            for (index, record) in records.iter().enumerate() {
                let was_accepted = submission == ActivationSubmission::Accepted;
                match receiver_activation_status(record, image, &mut submission) {
                    Ok(done) => complete |= done,
                    Err(outcome) => return outcome,
                }
                if !was_accepted && submission == ActivationSubmission::Accepted {
                    accepted_at = Some(index);
                    if availability == Some(ReceiverAvailability::Responding) {
                        availability = None;
                    }
                }
            }
            if let Some(assembler) = &mut chunks {
                for (index, record) in records.iter().enumerate() {
                    if accepted_at == Some(index) {
                        // A reconnect probe may already be in flight. Do not
                        // use its pre-acceptance fragments to confirm activation.
                        *assembler = InfoChunks::new(RECEIVER_ID);
                        continue;
                    }
                    let info = match assembler.push(record) {
                        Ok(Some(info)) => info,
                        Ok(None) => continue,
                        Err(error) => return UpdateOutcome::Failed(error),
                    };
                    if info.board_target != target.info.board_target
                        || info.bootloader != target.info.bootloader
                        || info.protocol_version != target.info.protocol_version
                    {
                        return UpdateOutcome::Failed(
                            "Receiver returned unexpected firmware identity after activation"
                                .into(),
                        );
                    }
                    if submission == ActivationSubmission::Accepted {
                        complete |= accepted_at.is_none_or(|accepted| accepted < index);
                    } else {
                        availability = Some(ReceiverAvailability::Responding);
                    }
                    chunks = None;
                    break;
                }
            }
            if complete {
                return UpdateOutcome::Completed;
            }
        }
        break;
    }
    UpdateOutcome::ActivationUnconfirmed(ReceiverActivationReport {
        submission,
        availability: availability
            .unwrap_or_else(|| ReceiverAvailability::Unconfirmed(pending_reason.into())),
    })
}
