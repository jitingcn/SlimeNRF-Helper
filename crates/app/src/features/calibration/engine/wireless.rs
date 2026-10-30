use std::collections::BTreeMap;
use std::ffi::CString;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::domain::ReceiverEndpoint;
use crate::hid_log::decode::{CommandAck, EventSource, TrackerEvent};
use crate::hid_log::{Source, TracedHid};
use crate::receiver_events::{EventWatchStatus, Watch};

use super::{
    Application, CalibrationTask, Engine, Fact, ObservationEnd, OperationIdentity, Phase, RunMode,
    RunRequest, Snapshot, Target,
};

const OP_SEQUENCE: u8 = 128;
const PREPARE_MS: u64 = 8_000;
const DRAIN_MS: u64 = 250;
const MAX_DRAIN_REPORTS: usize = 128;
const MAX_OLD_OPERATIONS: usize = 128;
const MAX_EVENTS: usize = 512;

pub(crate) trait WirelessIo {
    fn now_ms(&self) -> u64;
    fn read(&mut self, bytes: &mut [u8; 65], timeout_ms: i32) -> Result<usize, String>;
    fn write(&mut self, bytes: &[u8; 65]) -> Result<(), String>;
}

pub(crate) struct NativeWirelessIo {
    device: TracedHid,
    epoch: Instant,
}
impl NativeWirelessIo {
    pub(crate) fn open(receiver: &ReceiverEndpoint) -> Result<Self, String> {
        let inventory = crate::discovery::scan()?;
        if !inventory.receivers.iter().any(|current| {
            current.key == receiver.key
                && current.path == receiver.path
                && current.serial_number == receiver.serial_number
        }) {
            return Err("Receiver control endpoint disappeared or changed".into());
        }
        let path =
            CString::new(receiver.path.as_slice()).map_err(|_| "Invalid receiver HID path")?;
        let api = hidapi::HidApi::new().map_err(|error| error.to_string())?;
        let device = api
            .open_path(&path)
            .map_err(|error| format!("Cannot open receiver HID: {error}"))?;
        Ok(Self {
            device: TracedHid::new(device, Source::main(receiver)),
            epoch: Instant::now(),
        })
    }
}
impl WirelessIo for NativeWirelessIo {
    fn now_ms(&self) -> u64 {
        self.epoch.elapsed().as_millis() as u64
    }
    fn read(&mut self, bytes: &mut [u8; 65], timeout_ms: i32) -> Result<usize, String> {
        self.device
            .read_timeout(bytes, timeout_ms)
            .map_err(|error| format!("Receiver HID read failed: {error}"))
    }
    fn write(&mut self, bytes: &[u8; 65]) -> Result<(), String> {
        match self.device.write(bytes) {
            Ok(65) => Ok(()),
            Ok(length) => Err(format!(
                "Partial receiver HID write ({length}/65); command not retried"
            )),
            Err(error) => Err(format!(
                "Receiver HID write failed; command not retried: {error}"
            )),
        }
    }
}

#[derive(PartialEq, Eq)]
enum OldOperation {
    Unknown,
    Active,
    Ended,
}

struct Association {
    slot: u8,
    address: [u8; 6],
    kind: u8,
    axis: Option<u8>,
    registered_at: Option<u64>,
    online_at: Option<u64>,
    invalid: bool,
    armed: bool,
    recovery: bool,
    session: Option<u32>,
    latest: Option<u16>,
    arm_sequence: Option<u16>,
    old: BTreeMap<OperationIdentity, OldOperation>,
    bound: Option<OperationIdentity>,
    refused: Option<OperationIdentity>,
    events: Vec<TrackerEvent>,
    armed_event_start: usize,
    opcode: u8,
    ack_deadline: Option<u64>,
    command_rejected: bool,
}
impl Association {
    fn new(request: &RunRequest, slot: u8, address: [u8; 6]) -> Result<Self, String> {
        let recovery = request.mode == RunMode::Recover;
        if recovery && request.recovery.is_none() {
            return Err("Wireless recovery requires the original operation identity".into());
        }
        let (kind, opcode) = match request.task {
            CalibrationTask::Stationary => (1, 0x02),
            CalibrationTask::Sensitivity { .. } => (5, 0x24),
        };
        Ok(Self {
            slot,
            address,
            kind,
            axis: match request.task {
                CalibrationTask::Stationary => None,
                CalibrationTask::Sensitivity { axis, .. } => Some(axis.index()),
            },
            registered_at: None,
            online_at: None,
            invalid: false,
            armed: recovery,
            recovery,
            session: request.recovery.map(|id| id.session),
            latest: None,
            arm_sequence: None,
            old: BTreeMap::new(),
            bound: request.recovery,
            refused: None,
            events: Vec::new(),
            armed_event_start: 0,
            opcode,
            ack_deadline: None,
            command_rejected: false,
        })
    }
    fn uncertain(&mut self, engine: &mut Engine, now: u64, message: &str) {
        self.invalid = true;
        engine.fact(Fact::Uncertain(message.into()), now);
    }
    fn conflicting(&mut self, engine: &mut Engine, now: u64, message: &str) {
        self.uncertain(engine, now, message);
        if self.bound.is_some() || self.refused.is_some() {
            engine.fact(Fact::Application(Application::Conflicting), now);
        }
    }
    fn fresh(&self, now: u64) -> bool {
        !self.invalid
            && self
                .registered_at
                .is_some_and(|seen| now.saturating_sub(seen) < 3_000)
            && self
                .online_at
                .is_some_and(|seen| now.saturating_sub(seen) < 3_000)
    }
    fn ingest(&mut self, bytes: &[u8], engine: &mut Engine, now: u64) -> Result<(), String> {
        if bytes.is_empty() {
            return Ok(());
        }
        if bytes.len() != 64 {
            return Err(format!(
                "Unexpected receiver main HID report size {}",
                bytes.len()
            ));
        }
        let records = bytes.as_chunks::<16>().0;
        for record in records {
            if record[1] == self.slot {
                if record[0] == 255 {
                    let address: [u8; 6] = record[2..8].try_into().expect("registration address");
                    if address != [0; 6] {
                        if address != self.address {
                            self.uncertain(engine, now, "RF registration changed; returning to the original address cannot restore association");
                        } else {
                            self.registered_at = Some(now);
                        }
                    }
                } else if record[0] <= 7 {
                    self.online_at = Some(now);
                }
            }
            if record[0] == 251
                && record[1] == OP_SEQUENCE
                && record[2] == self.opcode
                && self.armed
                && !self.recovery
            {
                let ack = CommandAck::parse(record);
                match ack.status {
                    0 => {
                        self.ack_deadline = None;
                        engine.fact(Fact::ReceiverConfirmed, now);
                    }
                    6 | 7 => {}
                    4 => {
                        self.ack_deadline = None;
                        engine.fact(Fact::Uncertain("Receiver confirmation timed out; the tracker may have received the request".into()), now);
                    }
                    1 | 2 | 3 | 5 => {
                        self.ack_deadline = None;
                        self.command_rejected = true;
                        engine.fact(Fact::DispatchRejected, now);
                        engine.fact(Fact::Application(Application::Rejected), now);
                        if self.bound.is_some() {
                            self.conflicting(
                                engine,
                                now,
                                "Receiver rejection contradicts the bound acceptance",
                            );
                        }
                    }
                    _ => engine.fact(
                        Fact::Uncertain("Unknown receiver command acknowledgement status".into()),
                        now,
                    ),
                }
            }
            if let Some(event) = TrackerEvent::parse(record) {
                self.event(event, engine, now);
            }
        }
        Ok(())
    }
    fn event(&mut self, event: TrackerEvent, engine: &mut Engine, now: u64) {
        if event.tracker_id != self.slot || !event.is_calibration() {
            return;
        }
        let identity = OperationIdentity {
            session: event.boot_id,
            operation: event.operation_id,
        };
        if !self.armed {
            let old = self.old.entry(identity).or_insert(OldOperation::Unknown);
            if event.source == EventSource::Tracker && event.kind == self.kind {
                if matches!(event.action, 4 | 5) {
                    *old = OldOperation::Ended;
                } else if matches!(event.action, 1..=3)
                    && event.phase != 18
                    && *old != OldOperation::Ended
                {
                    *old = OldOperation::Active;
                }
            }
            if self.old.len() > MAX_OLD_OPERATIONS {
                self.uncertain(
                    engine,
                    now,
                    "Too many preexisting operations to prepare unambiguously",
                );
            }
        }
        if self.session.is_some_and(|session| session != event.boot_id) {
            self.uncertain(
                engine,
                now,
                "Tracker event session changed; this is not proof of cancellation",
            );
            return;
        }
        self.session.get_or_insert(event.boot_id);
        if self.invalid {
            return;
        }
        if let Some(previous) = self
            .events
            .iter()
            .find(|previous| previous.source == event.source && previous.sequence == event.sequence)
        {
            if *previous != event {
                self.conflicting(engine, now, "Conflicting payloads share an event sequence");
            }
            return;
        }
        if self
            .latest
            .is_some_and(|latest| event.sequence.wrapping_sub(latest) == 0x8000)
        {
            self.uncertain(
                engine,
                now,
                "Event sequence moved by an ambiguous half range",
            );
            return;
        }
        if self.events.len() == MAX_EVENTS {
            self.uncertain(
                engine,
                now,
                "Calibration event history exceeded its observation bound",
            );
            return;
        }
        let newer = self
            .latest
            .is_none_or(|latest| sequence_after(event.sequence, latest));
        if newer {
            self.latest = Some(event.sequence);
        }
        self.events.push(event);
        if !self.armed
            || self.registered_at.is_none()
            || event.kind != self.kind
            || event.automatic_flag
        {
            return;
        }
        let fresh_request = !self.recovery
            && event.source == EventSource::Tracker
            && event.boot_id != 0
            && event.operation_id != 0
            && !self.old.contains_key(&identity)
            && self
                .arm_sequence
                .is_none_or(|before| sequence_after(event.sequence, before));
        if fresh_request && event.action == 5 && event.outcome == 2 {
            if self.bound.is_some() || self.refused.is_some() {
                self.conflicting(engine, now, "Competing matching user requests or request identity reuse prevent unique attribution");
            } else {
                self.refused = Some(identity);
                engine.fact(Fact::Application(Application::Rejected), now);
            }
            return;
        }
        if fresh_request && event.action == 1 && event.outcome == 0 {
            if self.command_rejected || self.refused.is_some() {
                self.conflicting(engine, now, "A user acceptance followed explicit request rejection and cannot bind this request");
                return;
            }
            if let Some(bound) = self.bound {
                let reused = self.events.iter().any(|previous| {
                    previous.source == EventSource::Tracker
                        && previous.action == 1
                        && previous.sequence != event.sequence
                        && previous.operation_id == identity.operation
                        && previous.boot_id == identity.session
                });
                if bound != identity || reused {
                    self.conflicting(engine, now, "Competing matching user acceptances or operation identity reuse prevent unique attribution");
                    return;
                }
            } else {
                self.bound = Some(identity);
                engine.fact(Fact::Association(identity), now);
                for index in self.armed_event_start..self.events.len() - 1 {
                    let buffered = self.events[index];
                    if buffered.boot_id == identity.session
                        && buffered.operation_id == identity.operation
                        && buffered.kind == self.kind
                        && !buffered.automatic_flag
                        && sequence_after(buffered.sequence, event.sequence)
                        && self
                            .arm_sequence
                            .is_none_or(|before| sequence_after(buffered.sequence, before))
                    {
                        self.observe_bound(
                            buffered,
                            engine,
                            now,
                            self.latest == Some(buffered.sequence),
                        );
                        if self.invalid {
                            return;
                        }
                    }
                }
            }
        }
        if event.action == 5 && !self.recovery {
            return;
        }
        if self.bound != Some(identity) {
            return;
        }
        self.observe_bound(event, engine, now, newer);
    }
    fn observe_bound(&mut self, event: TrackerEvent, engine: &mut Engine, now: u64, newer: bool) {
        if event.source == EventSource::Receiver {
            if event.action == 4 && event.outcome == 5 {
                engine.fact(
                    Fact::Uncertain(
                        "Receiver lost operation evidence; the tracker may still be running".into(),
                    ),
                    now,
                );
            }
            return;
        }
        if matches!(event.phase, 9 | 10) && self.axis.is_some_and(|axis| event.detail != axis) {
            self.uncertain(
                engine,
                now,
                "Tracker rotation cue conflicts with the requested sensor axis",
            );
            return;
        }
        if event.phase == 18 && event.detail == 18 {
            engine.fact(Fact::StorageError, now);
        }
        if event.action == 4 {
            match event.outcome {
                1 if event.phase == 16 => engine.fact(Fact::Application(Application::Applied), now),
                2 => engine.fact(Fact::Application(Application::Failed), now),
                3 => engine.fact(Fact::Application(Application::Cancelled), now),
                _ => engine.fact(
                    Fact::Uncertain("Tracker terminal did not prove application".into()),
                    now,
                ),
            }
        } else if event.action == 5 {
            engine.fact(Fact::Application(Application::Rejected), now);
        } else if newer && matches!(event.action, 1..=3) {
            let phase = match event.phase {
                2 => Some(Phase::WaitingStill),
                3 => Some(Phase::Retrimming),
                4 => Some(Phase::Collecting),
                9 => Some(Phase::WaitingRotation),
                10 => Some(Phase::Recording),
                13 => Some(Phase::Validating),
                15 => Some(Phase::CandidateQueued),
                _ => None,
            };
            if let Some(phase) = phase {
                engine.fact(Fact::Phase(phase), now);
            }
        }
    }
}
fn sequence_after(sequence: u16, previous: u16) -> bool {
    let distance = sequence.wrapping_sub(previous);
    distance != 0 && distance < 0x8000
}
fn address_bytes(address: &str) -> Result<[u8; 6], String> {
    if address.len() != 12 || !address.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("Invalid frozen RF address".into());
    }
    let mut bytes = [0; 6];
    for (index, byte) in bytes.iter_mut().rev().enumerate() {
        *byte = u8::from_str_radix(&address[index * 2..index * 2 + 2], 16)
            .map_err(|_| "Invalid frozen RF address")?;
    }
    if bytes == [0; 6] {
        return Err("Empty RF registration cannot identify a tracker".into());
    }
    Ok(bytes)
}
fn operation(request: &RunRequest, slot: u8) -> [u8; 65] {
    let mut bytes = [0; 65];
    bytes[1..6].copy_from_slice(&[254, OP_SEQUENCE, 2, 0, slot]);
    if let CalibrationTask::Sensitivity { axis, revolutions } = request.task {
        bytes[3] = 0x24;
        bytes[6] = axis.index();
        bytes[7..9].copy_from_slice(&revolutions.get().to_le_bytes());
    }
    bytes
}

struct Driver<'a> {
    request: &'a RunRequest,
    io: &'a mut dyn WirelessIo,
    engine: &'a mut Engine,
    stop: &'a AtomicBool,
    publish: &'a dyn Fn(&Snapshot),
    watch: Watch,
    association: Association,
    epoch: Instant,
}
impl Driver<'_> {
    fn now(&self) -> Instant {
        self.epoch + Duration::from_millis(self.io.now_ms())
    }
    fn read(&mut self, timeout: i32) -> Result<usize, String> {
        let mut input = [0; 65];
        let length = self.io.read(&mut input, timeout)?;
        if length > input.len() {
            return Err("Receiver returned an oversized report".into());
        }
        self.watch.ingest(&input[..length], self.now());
        self.association
            .ingest(&input[..length], self.engine, self.io.now_ms())?;
        (self.publish)(&self.engine.snapshot);
        Ok(length)
    }
    fn control(&mut self) -> Result<(), String> {
        if let Some(command) = self.watch.tick(self.now())
            && let Err(error) = self.io.write(&command.bytes)
        {
            self.watch.write_failed(error.clone());
            return Err(error);
        }
        match self.watch.status() {
            EventWatchStatus::Unsupported(error) | EventWatchStatus::Error(error) => {
                Err(error.clone())
            }
            _ => Ok(()),
        }
    }
    fn drain(&mut self) -> Result<(), String> {
        let deadline = self.io.now_ms().saturating_add(DRAIN_MS);
        for _ in 0..MAX_DRAIN_REPORTS {
            if self.stop.load(Ordering::Acquire) {
                return Ok(());
            }
            if self.read(0)? == 0 {
                return Ok(());
            }
            if self.io.now_ms() >= deadline {
                break;
            }
        }
        Err("Receiver input remained saturated; preexisting operations cannot be separated".into())
    }
    fn observe(&mut self) -> Result<ObservationEnd, String> {
        self.drain()?;
        let prepare_deadline = self.io.now_ms().saturating_add(PREPARE_MS);
        loop {
            if self.stop.load(Ordering::Acquire) {
                return Ok(ObservationEnd::Stopped);
            }
            if let Err(error) = self.control() {
                if matches!(self.watch.status(), EventWatchStatus::Unsupported(_)) {
                    self.engine.fact(Fact::Uncertain(error), self.io.now_ms());
                    return Ok(ObservationEnd::Unsupported);
                }
                return Err(error);
            }
            if self.association.invalid {
                return Ok(ObservationEnd::Ambiguous);
            }
            if self.watch.status() == &EventWatchStatus::Active
                && self.association.fresh(self.io.now_ms())
            {
                break;
            }
            if self.io.now_ms() >= prepare_deadline {
                return Err("Could not freshly confirm online tracker registration and receiver event support".into());
            }
            self.read(50)?;
        }
        self.drain()?;
        if self.stop.load(Ordering::Acquire) {
            return Ok(ObservationEnd::Stopped);
        }
        if self.association.invalid {
            return Ok(ObservationEnd::Ambiguous);
        }
        if !self.association.fresh(self.io.now_ms()) {
            return Err(
                "Frozen tracker identity or online evidence changed before dispatch".into(),
            );
        }
        self.engine.fact(
            Fact::Identity {
                address: match &self.request.target {
                    Target::Wireless { address, .. } => address.clone(),
                    _ => unreachable!(),
                },
                stationary: false,
                sensitivity: false,
            },
            self.io.now_ms(),
        );
        self.engine.fact(Fact::ReadbackIncomplete, self.io.now_ms());
        if self.request.mode == RunMode::Inspect {
            return Ok(ObservationEnd::Complete);
        }
        if self.request.mode == RunMode::Execute {
            if self
                .association
                .old
                .values()
                .any(|state| *state == OldOperation::Active)
            {
                return Err("An observed preexisting matching calibration is still active; no calibration command sent".into());
            }
            self.association.armed = true;
            self.association.arm_sequence = self.association.latest;
            self.association.armed_event_start = self.association.events.len();
            self.association.ack_deadline = Some(self.io.now_ms().saturating_add(6_000));
            self.engine.fact(Fact::DispatchAttempt, self.io.now_ms());
            (self.publish)(&self.engine.snapshot);
            self.io
                .write(&operation(self.request, self.association.slot))?;
            self.engine.fact(Fact::Written, self.io.now_ms());
            (self.publish)(&self.engine.snapshot);
        }
        loop {
            if self.stop.load(Ordering::Acquire) {
                return Ok(ObservationEnd::Stopped);
            }
            if self.association.invalid {
                return Ok(ObservationEnd::Ambiguous);
            }
            if self.engine.expired(self.io.now_ms()) {
                return Ok(self.engine.deadline_end(self.io.now_ms()));
            }
            self.control()?;
            if self
                .association
                .ack_deadline
                .is_some_and(|deadline| self.io.now_ms() >= deadline)
            {
                self.association.ack_deadline = None;
                self.engine.fact(
                    Fact::Uncertain(
                        "No receiver confirmation; request was not retried and may still run"
                            .into(),
                    ),
                    self.io.now_ms(),
                );
            }
            self.read(100)?;
        }
    }
    fn cleanup(&mut self) {
        self.watch.sync(false, 1, true);
        let deadline = self.io.now_ms().saturating_add(2_000);
        while self.watch.cleanup_pending() && self.io.now_ms() < deadline {
            if let Some(command) = self.watch.tick(self.now())
                && let Err(error) = self.io.write(&command.bytes)
            {
                self.watch.write_failed(error.clone());
                self.engine
                    .fact(Fact::CleanupError(error), self.io.now_ms());
            }
            if !self.watch.cleanup_pending() {
                break;
            }
            if let Err(error) = self.read(50) {
                self.engine
                    .fact(Fact::CleanupError(error), self.io.now_ms());
                break;
            }
        }
        if self.watch.cleanup_pending() && self.io.now_ms() >= deadline {
            self.engine.fact(Fact::CleanupError("Event subscription stop ACK did not arrive within the two-second cleanup window".into()), self.io.now_ms());
        }
        if let EventWatchStatus::Error(error) | EventWatchStatus::Unsupported(error) =
            self.watch.status()
        {
            self.engine
                .fact(Fact::CleanupError(error.clone()), self.io.now_ms());
        }
    }
}

pub(crate) fn run(
    request: &RunRequest,
    io: &mut dyn WirelessIo,
    engine: &mut Engine,
    stop: &AtomicBool,
    publish: &dyn Fn(&Snapshot),
) -> Result<(), String> {
    let Target::Wireless { slot, address, .. } = &request.target else {
        return Err("Wireless worker requires a receiver target".into());
    };
    if *slot >= 16 {
        return Err("Invalid receiver tracker slot".into());
    }
    let association = Association::new(request, *slot, address_bytes(address)?)?;
    let mut driver = Driver {
        request,
        io,
        engine,
        stop,
        publish,
        association,
        epoch: Instant::now(),
        watch: Watch::new(true, 0, EventWatchStatus::Checking, *slot, 1),
    };
    let result = driver.observe();
    driver.cleanup();
    match result {
        Ok(end) => {
            let end = if driver.association.invalid {
                ObservationEnd::Ambiguous
            } else {
                end
            };
            driver.engine.finish(end, driver.io.now_ms());
            (driver.publish)(&driver.engine.snapshot);
            Ok(())
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::{WirelessIo, run};
    use crate::domain::ReceiverEndpoint;
    use crate::features::calibration::engine::{
        Application, Axis, CalibrationTask, Dispatch, Engine, ObservationEnd, OperationIdentity,
        Persistence, Revolutions, RunMode, RunRequest, Snapshot, Target,
    };
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    const SESSION: u32 = 0x11223344;
    fn request(task: CalibrationTask) -> RunRequest {
        RunRequest {
            id: 7,
            target: Target::Wireless {
                receiver: ReceiverEndpoint {
                    key: "fixture".into(),
                    path: b"fixture-only".to_vec(),
                    label: "Fixture".into(),
                    serial_number: None,
                },
                slot: 3,
                address: "010203040506".into(),
            },
            task,
            mode: RunMode::Execute,
            recovery: None,
        }
    }
    fn event(
        sequence: u16,
        operation: u16,
        kind: u8,
        action: u8,
        outcome: u8,
        phase: u8,
        detail: u8,
    ) -> [u8; 16] {
        let mut record = [0; 16];
        record[..5].copy_from_slice(&[251, 0, 225, action | outcome << 4, 3]);
        record[5..9].copy_from_slice(&SESSION.to_le_bytes());
        record[9..11].copy_from_slice(&sequence.to_le_bytes());
        record[11..13].copy_from_slice(&operation.to_le_bytes());
        record[13..16].copy_from_slice(&[kind, phase, detail]);
        record
    }
    fn accepted(sequence: u16, operation: u16, kind: u8) -> [u8; 16] {
        event(sequence, operation, kind, 1, 0, 2, 0)
    }
    fn applied(sequence: u16, operation: u16, kind: u8) -> [u8; 16] {
        event(sequence, operation, kind, 4, 1, 16, 0)
    }
    fn storage(sequence: u16, operation: u16, kind: u8) -> [u8; 16] {
        event(sequence, operation, kind, 3, 0, 18, 18)
    }
    fn registration(last: u8) -> [u8; 16] {
        let mut record = [0; 16];
        record[..8].copy_from_slice(&[255, 3, last, 5, 4, 3, 2, 1]);
        record
    }
    fn report(records: &[[u8; 16]]) -> Vec<u8> {
        let mut bytes = vec![254; 64];
        for (index, record) in records.iter().enumerate() {
            bytes[index * 16..index * 16 + 16].copy_from_slice(record);
        }
        bytes
    }
    fn ack(sequence: u8, opcode: u8, status: u8) -> [u8; 16] {
        let mut bytes = [0; 16];
        bytes[..4].copy_from_slice(&[251, sequence, opcode, status]);
        bytes
    }
    struct ScriptedIo {
        now: u64,
        input: VecDeque<(u64, Result<Vec<u8>, String>)>,
        writes: Vec<[u8; 65]>,
        operation_input: Vec<(u64, Vec<u8>)>,
        stale: Vec<Vec<u8>>,
        cleanup: Vec<[u8; 16]>,
        stop: Option<(u64, Arc<AtomicBool>)>,
        fail_opcode: Option<u8>,
        fail_control_action: Option<u8>,
        subscription_version: u8,
        subscription_mask: Option<u8>,
        subscription_filter: Option<u8>,
        subscription_active: bool,
        receiver_capabilities: u8,
        online: bool,
        stop_ack: bool,
    }
    impl ScriptedIo {
        fn new(operation_input: Vec<(u64, Vec<u8>)>) -> Self {
            Self {
                now: 0,
                input: VecDeque::new(),
                writes: Vec::new(),
                operation_input,
                stale: Vec::new(),
                cleanup: Vec::new(),
                stop: None,
                fail_opcode: None,
                fail_control_action: None,
                subscription_version: 1,
                subscription_mask: None,
                subscription_filter: None,
                subscription_active: true,
                receiver_capabilities: 1,
                online: true,
                stop_ack: true,
            }
        }
        fn enqueue(&mut self, at: u64, bytes: Vec<u8>) {
            let index = self
                .input
                .iter()
                .position(|(time, _)| *time > at)
                .unwrap_or(self.input.len());
            self.input.insert(index, (at, Ok(bytes)));
        }
    }
    impl WirelessIo for ScriptedIo {
        fn now_ms(&self) -> u64 {
            self.now
        }
        fn read(&mut self, bytes: &mut [u8; 65], timeout: i32) -> Result<usize, String> {
            let until = self.now + timeout.max(0) as u64;
            let result = if self.input.front().is_some_and(|(at, _)| *at <= until) {
                let (at, result) = self.input.pop_front().unwrap();
                self.now = self.now.max(at);
                let input = result?;
                bytes[..input.len()].copy_from_slice(&input);
                input.len()
            } else {
                self.now = until.max(self.now + 1);
                0
            };
            if let Some((at, stop)) = &self.stop
                && self.now >= *at
            {
                stop.store(true, Ordering::Release);
            }
            Ok(result)
        }
        fn write(&mut self, bytes: &[u8; 65]) -> Result<(), String> {
            self.writes.push(*bytes);
            if bytes[3] == 224 {
                let action = bytes[6];
                let mut response = ack(bytes[2], 224, 0);
                response[4] = self.subscription_version;
                response[5] = self.receiver_capabilities;
                response[14] = self.subscription_filter.unwrap_or(bytes[7]);
                response[15] = if matches!(action, 1 | 2) {
                    u8::from(self.subscription_active)
                        | self.subscription_mask.unwrap_or(bytes[8]) << 1
                } else {
                    0
                };
                if action == 1 {
                    let mut sensor = [254; 16];
                    if self.online {
                        sensor[..2].copy_from_slice(&[1, 3]);
                    }
                    self.enqueue(self.now, report(&[registration(6), sensor, response]));
                    for stale in std::mem::take(&mut self.stale) {
                        self.enqueue(self.now, stale);
                    }
                } else if action != 3 || self.stop_ack {
                    let mut records = vec![response];
                    if action == 3 {
                        records.extend(std::mem::take(&mut self.cleanup));
                    }
                    self.enqueue(self.now, report(&records));
                }
                if self.fail_control_action == Some(action) {
                    return Err("scripted partial event-control write".into());
                }
            } else {
                for (delay, input) in std::mem::take(&mut self.operation_input) {
                    self.enqueue(self.now + delay, input);
                }
            }
            if self.fail_opcode == Some(bytes[3]) {
                return Err("scripted partial write".into());
            }
            Ok(())
        }
    }
    fn execute(
        request: &RunRequest,
        io: &mut ScriptedIo,
    ) -> (Result<(), String>, Snapshot, Vec<Snapshot>) {
        let mut engine = Engine::new(request.clone());
        let snapshots = RefCell::new(Vec::new());
        let stop = Arc::new(AtomicBool::new(false));
        if let Some((_, script_stop)) = &mut io.stop {
            *script_stop = Arc::clone(&stop);
        }
        let result = run(request, io, &mut engine, &stop, &|snapshot| {
            snapshots.borrow_mut().push(snapshot.clone())
        });
        (result, engine.snapshot, snapshots.into_inner())
    }
    fn only_safe_commands(io: &ScriptedIo, operation: Option<[u8; 65]>) {
        let operations: Vec<_> = io
            .writes
            .iter()
            .filter(|bytes| bytes[3] != 224)
            .copied()
            .collect();
        assert_eq!(operations, operation.into_iter().collect::<Vec<_>>());
        assert!(
            io.writes
                .iter()
                .all(|bytes| matches!(bytes[3], 2 | 0x24 | 224))
        );
    }
    fn expected_operation(opcode: u8, axis: u8, revolutions: u8) -> [u8; 65] {
        let mut bytes = [0; 65];
        bytes[..9].copy_from_slice(&[0, 254, 128, opcode, 0, 3, axis, revolutions, 0]);
        bytes
    }

    #[test]
    fn both_tasks_write_exactly_once_and_consume_ack_end_storage_in_one_report() {
        for (task, kind, expected) in [
            (CalibrationTask::Stationary, 1, expected_operation(2, 0, 0)),
            (
                CalibrationTask::Sensitivity {
                    axis: Axis::Y,
                    revolutions: Revolutions::new(37).unwrap(),
                },
                5,
                expected_operation(0x24, 1, 37),
            ),
        ] {
            let request = request(task);
            let mut io = ScriptedIo::new(vec![(
                0,
                report(&[
                    accepted(1, 8, kind),
                    ack(128, expected[3], 0),
                    applied(3, 8, kind),
                    storage(2, 8, kind),
                ]),
            )]);
            let (result, snapshot, published) = execute(&request, &mut io);
            assert_eq!(result, Ok(()));
            assert_eq!(
                snapshot.association,
                Some(OperationIdentity {
                    session: SESSION,
                    operation: 8
                })
            );
            assert_eq!(snapshot.dispatch, Dispatch::ReceiverConfirmed);
            assert_eq!(snapshot.application, Application::Applied);
            assert_eq!(snapshot.persistence, Persistence::StorageError);
            assert_eq!(snapshot.end, Some(ObservationEnd::Complete));
            assert!(snapshot.readback.is_none() && snapshot.readback_incomplete);
            assert!(
                published
                    .iter()
                    .any(|s| s.application == Application::Applied && s.end.is_none())
            );
            only_safe_commands(&io, Some(expected));
            assert_eq!(&io.writes[0][..9], &[0, 254, 1, 224, 0, 1, 0, 3, 0]);
            assert_eq!(&io.writes[1][..9], &[0, 254, 2, 224, 0, 1, 1, 3, 1]);
            assert_eq!(io.writes.last().unwrap()[6], 3);
        }
    }
    #[test]
    fn fresh_acceptance_replays_prior_storage_and_applied_before_duplicate_repeats() {
        for (task, kind, expected) in [
            (CalibrationTask::Stationary, 1, expected_operation(2, 0, 0)),
            (
                CalibrationTask::Sensitivity {
                    axis: Axis::Y,
                    revolutions: Revolutions::new(37).unwrap(),
                },
                5,
                expected_operation(0x24, 1, 37),
            ),
        ] {
            let mut io = ScriptedIo::new(vec![
                (
                    0,
                    report(&[
                        storage(3, 8, kind),
                        applied(2, 8, kind),
                        ack(128, expected[3], 0),
                    ]),
                ),
                (10, report(&[accepted(1, 8, kind)])),
                (
                    1_000,
                    report(&[
                        storage(3, 8, kind),
                        applied(2, 8, kind),
                        accepted(1, 8, kind),
                    ]),
                ),
            ]);
            let (result, snapshot, published) = execute(&request(task), &mut io);
            assert_eq!(result, Ok(()));
            only_safe_commands(&io, Some(expected));
            let dispatched_at = published
                .iter()
                .find(|snapshot| snapshot.dispatch == Dispatch::Attempted)
                .unwrap()
                .elapsed_ms;
            let pending = published
                .iter()
                .find(|snapshot| snapshot.dispatch == Dispatch::ReceiverConfirmed)
                .unwrap();
            assert_eq!(pending.elapsed_ms, dispatched_at);
            assert_eq!(pending.association, None);
            assert_eq!(pending.application, Application::Unconfirmed);
            assert_eq!(pending.persistence, Persistence::Unknown);
            let accepted = published
                .iter()
                .find(|snapshot| {
                    snapshot.elapsed_ms == dispatched_at + 10 && snapshot.association.is_some()
                })
                .unwrap();
            assert_eq!(
                accepted.association,
                Some(OperationIdentity {
                    session: SESSION,
                    operation: 8,
                })
            );
            assert_eq!(accepted.application, Application::Applied);
            assert_eq!(accepted.persistence, Persistence::StorageError);
            assert_eq!(accepted.end, None);
            assert_eq!(snapshot.association, accepted.association);
            assert_eq!(snapshot.application, Application::Applied);
            assert_eq!(snapshot.persistence, Persistence::StorageError);
            assert_eq!(snapshot.end, Some(ObservationEnd::Complete));
            assert!(
                (20_000..20_110).contains(&(snapshot.elapsed_ms - dispatched_at)),
                "duplicate evidence must not restart the terminal tail"
            );
        }
    }
    #[test]
    fn fresh_acceptance_replays_storage_that_precedes_acceptance_and_later_application() {
        let mut io = ScriptedIo::new(vec![
            (0, report(&[storage(3, 8, 1), ack(128, 2, 0)])),
            (10, report(&[accepted(1, 8, 1)])),
            (20, report(&[applied(2, 8, 1)])),
            (1_000, report(&[storage(3, 8, 1), applied(2, 8, 1)])),
        ]);
        let (result, snapshot, published) = execute(&request(CalibrationTask::Stationary), &mut io);
        assert_eq!(result, Ok(()));
        only_safe_commands(&io, Some(expected_operation(2, 0, 0)));
        let accepted = published
            .iter()
            .find(|snapshot| snapshot.association.is_some())
            .unwrap();
        assert_eq!(
            accepted.association,
            Some(OperationIdentity {
                session: SESSION,
                operation: 8,
            })
        );
        assert_eq!(accepted.application, Application::Unconfirmed);
        assert_eq!(accepted.persistence, Persistence::StorageError);
        assert_eq!(snapshot.application, Application::Applied);
        assert_eq!(snapshot.persistence, Persistence::StorageError);
        assert_eq!(snapshot.end, Some(ObservationEnd::Complete));
    }
    #[test]
    fn predispatched_and_orphan_evidence_cannot_bind_or_amend_a_fresh_acceptance() {
        for accept_fresh in [false, true] {
            let mut input = vec![(
                0,
                report(&[
                    accepted(4, 8, 1),
                    storage(6, 9, 1),
                    applied(5, 9, 1),
                    ack(128, 2, 0),
                ]),
            )];
            if accept_fresh {
                input.push((10, report(&[accepted(7, 10, 1)])));
            }
            let mut io = ScriptedIo::new(input);
            io.stale.push(report(&[storage(2, 8, 1), applied(3, 8, 1)]));
            io.stop = Some((1_000, Arc::new(AtomicBool::new(false))));
            let (result, snapshot, published) =
                execute(&request(CalibrationTask::Stationary), &mut io);
            assert_eq!(result, Ok(()));
            only_safe_commands(&io, Some(expected_operation(2, 0, 0)));
            assert_eq!(snapshot.dispatch, Dispatch::ReceiverConfirmed);
            assert_eq!(
                snapshot.association,
                accept_fresh.then_some(OperationIdentity {
                    session: SESSION,
                    operation: 10,
                })
            );
            assert_eq!(snapshot.application, Application::Unconfirmed);
            assert_eq!(snapshot.persistence, Persistence::Unknown);
            assert_eq!(snapshot.end, Some(ObservationEnd::Stopped));
            assert!(published.iter().all(|snapshot| {
                snapshot.application == Application::Unconfirmed
                    && snapshot.persistence == Persistence::Unknown
            }));
        }
    }
    #[test]
    fn fresh_standalone_tracker_rejection_refuses_request_without_binding_an_operation() {
        for (task, kind, expected) in [
            (CalibrationTask::Stationary, 1, expected_operation(2, 0, 0)),
            (
                CalibrationTask::Sensitivity {
                    axis: Axis::Z,
                    revolutions: Revolutions::new(5).unwrap(),
                },
                5,
                expected_operation(0x24, 2, 5),
            ),
        ] {
            let rejected = event(1, 8, kind, 5, 2, 0, 1);
            let mut io = ScriptedIo::new(vec![
                (0, report(&[ack(128, expected[3], 0), rejected])),
                (
                    1_000,
                    report(&[rejected, applied(2, 8, kind), storage(3, 8, kind)]),
                ),
                (19_000, report(&[rejected])),
            ]);
            let (result, snapshot, published) = execute(&request(task), &mut io);
            assert_eq!(result, Ok(()));
            only_safe_commands(&io, Some(expected));
            let dispatched_at = published
                .iter()
                .find(|snapshot| snapshot.dispatch == Dispatch::Attempted)
                .unwrap()
                .elapsed_ms;
            let refusal = published
                .iter()
                .find(|snapshot| snapshot.dispatch == Dispatch::ReceiverConfirmed)
                .unwrap();
            assert_eq!(refusal.elapsed_ms, dispatched_at);
            assert_eq!(refusal.application, Application::Rejected);
            assert_eq!(refusal.association, None);
            assert_eq!(refusal.persistence, Persistence::Unknown);
            assert_eq!(refusal.end, None);
            assert_eq!(snapshot.dispatch, Dispatch::ReceiverConfirmed);
            assert_eq!(snapshot.application, Application::Rejected);
            assert_eq!(snapshot.association, None);
            assert_eq!(snapshot.persistence, Persistence::Unknown);
            assert_eq!(snapshot.end, Some(ObservationEnd::Complete));
            assert!(
                (20_000..20_100).contains(&(snapshot.elapsed_ms - dispatched_at)),
                "duplicate refusal must not restart the terminal tail"
            );
        }
    }
    #[test]
    fn competing_fresh_tracker_refusals_and_acceptances_are_not_first_wins() {
        for (first, competing, initially_bound) in [
            (
                event(1, 8, 1, 5, 2, 0, 1),
                event(2, 9, 1, 5, 2, 0, 1),
                false,
            ),
            (
                event(2, 8, 1, 5, 2, 0, 1),
                event(1, 9, 1, 5, 2, 0, 1),
                false,
            ),
            (event(2, 8, 1, 5, 2, 0, 1), accepted(1, 9, 1), false),
            (accepted(2, 8, 1), event(1, 9, 1, 5, 2, 0, 1), true),
            (
                event(1, 8, 1, 5, 2, 0, 1),
                event(1, 9, 1, 5, 2, 0, 1),
                false,
            ),
        ] {
            let mut io = ScriptedIo::new(vec![
                (0, report(&[ack(128, 2, 0), first])),
                (10, report(&[competing, applied(3, 9, 1), storage(4, 9, 1)])),
            ]);
            let (result, snapshot, published) =
                execute(&request(CalibrationTask::Stationary), &mut io);
            assert_eq!(result, Ok(()));
            only_safe_commands(&io, Some(expected_operation(2, 0, 0)));
            let initial = published
                .iter()
                .find(|snapshot| snapshot.dispatch == Dispatch::ReceiverConfirmed)
                .unwrap();
            assert_eq!(
                initial.application,
                if initially_bound {
                    Application::Unconfirmed
                } else {
                    Application::Rejected
                }
            );
            assert_eq!(
                snapshot.association,
                initially_bound.then_some(OperationIdentity {
                    session: SESSION,
                    operation: 8,
                })
            );
            assert_eq!(snapshot.application, Application::Conflicting);
            assert_eq!(snapshot.persistence, Persistence::Unknown);
            assert!(snapshot.uncertainty.is_some());
            assert_eq!(snapshot.end, Some(ObservationEnd::Ambiguous));
        }
    }
    #[test]
    fn old_and_unrelated_tracker_rejections_cannot_refuse_a_new_request() {
        let old = event(10, 7, 1, 5, 2, 0, 1);
        let mut wrong_slot = event(12, 9, 1, 5, 2, 0, 1);
        wrong_slot[4] = 4;
        let mut receiver_origin = event(12, 9, 1, 5, 2, 0, 1);
        receiver_origin[2] = 226;
        for unrelated in [
            old,
            event(12, 7, 1, 5, 2, 0, 1),
            event(9, 9, 1, 5, 2, 0, 1),
            event(12, 9, 129, 5, 2, 0, 1),
            event(12, 9, 5, 5, 2, 0, 1),
            wrong_slot,
            receiver_origin,
            event(12, 0, 1, 5, 2, 0, 1),
        ] {
            let mut io = ScriptedIo::new(vec![
                (0, report(&[ack(128, 2, 0), unrelated])),
                (10, report(&[accepted(20, 8, 1), applied(21, 8, 1)])),
            ]);
            io.stale.push(report(&[old]));
            let (result, snapshot, published) =
                execute(&request(CalibrationTask::Stationary), &mut io);
            assert_eq!(result, Ok(()));
            only_safe_commands(&io, Some(expected_operation(2, 0, 0)));
            let before_acceptance = published
                .iter()
                .find(|snapshot| snapshot.dispatch == Dispatch::ReceiverConfirmed)
                .unwrap();
            assert_eq!(before_acceptance.application, Application::Unconfirmed);
            assert_eq!(before_acceptance.association, None);
            assert_eq!(
                snapshot.association,
                Some(OperationIdentity {
                    session: SESSION,
                    operation: 8,
                })
            );
            assert_eq!(snapshot.application, Application::Applied);
            assert_eq!(snapshot.persistence, Persistence::Unknown);
            assert_eq!(snapshot.end, Some(ObservationEnd::Complete));
        }
    }
    #[test]
    fn stale_automatic_wrong_kind_and_orphan_end_never_bind() {
        let request = request(CalibrationTask::Stationary);
        let mut io = ScriptedIo::new(vec![
            (
                0,
                report(&[
                    accepted(4, 1, 1),
                    accepted(5, 2, 129),
                    accepted(6, 3, 5),
                    applied(7, 4, 1),
                ]),
            ),
            (10, report(&[accepted(8, 5, 1), applied(9, 5, 1)])),
        ]);
        io.stale
            .push(report(&[accepted(1, 1, 1), applied(2, 1, 1)]));
        let (result, snapshot, _) = execute(&request, &mut io);
        assert_eq!(result, Ok(()));
        assert_eq!(snapshot.application, Application::Applied);
        assert_eq!(
            snapshot.association,
            Some(OperationIdentity {
                session: SESSION,
                operation: 5
            })
        );
        assert_eq!(snapshot.persistence, Persistence::Unknown);
        only_safe_commands(&io, Some(expected_operation(2, 0, 0)));
    }
    #[test]
    fn storage_is_sticky_before_and_after_applied_with_wrap_and_reordering() {
        for input in [
            vec![(
                0,
                report(&[
                    accepted(65534, 8, 1),
                    storage(0, 8, 1),
                    applied(65535, 8, 1),
                ]),
            )],
            vec![
                (0, report(&[accepted(65534, 8, 1), applied(0, 8, 1)])),
                (1000, report(&[storage(65535, 8, 1), storage(65535, 8, 1)])),
            ],
        ] {
            let mut io = ScriptedIo::new(input);
            let (result, snapshot, _) = execute(&request(CalibrationTask::Stationary), &mut io);
            assert_eq!(result, Ok(()));
            assert_eq!(snapshot.application, Application::Applied);
            assert_eq!(snapshot.persistence, Persistence::StorageError);
            assert!(snapshot.elapsed_ms < 21_000);
        }
    }
    #[test]
    fn acceptance_ambiguity_is_not_first_wins_even_when_reordered() {
        for records in [
            vec![accepted(2, 8, 1), applied(3, 8, 1), accepted(1, 9, 1)],
            vec![accepted(1, 8, 1), accepted(1, 9, 1)],
        ] {
            let mut io = ScriptedIo::new(vec![(0, report(&records))]);
            let (result, snapshot, _) = execute(&request(CalibrationTask::Stationary), &mut io);
            assert_eq!(result, Ok(()));
            assert_eq!(snapshot.application, Application::Conflicting);
            assert_eq!(snapshot.end, Some(ObservationEnd::Ambiguous));
        }
    }
    #[test]
    fn rf_a_b_a_and_changed_event_session_are_sticky_uncertainty() {
        let mut session_change = event(2, 8, 1, 3, 0, 4, 0);
        session_change[5] ^= 1;
        for records in [
            vec![registration(7), registration(6), applied(2, 8, 1)],
            vec![session_change, applied(3, 8, 1)],
        ] {
            let mut io = ScriptedIo::new(vec![
                (0, report(&[accepted(1, 8, 1)])),
                (1, report(&records)),
            ]);
            let (result, snapshot, _) = execute(&request(CalibrationTask::Stationary), &mut io);
            assert_eq!(result, Ok(()));
            assert_eq!(snapshot.application, Application::Unconfirmed);
            assert_eq!(snapshot.end, Some(ObservationEnd::Ambiguous));
        }
        let mut io = ScriptedIo::new(vec![
            (0, report(&[accepted(1, 8, 1), applied(2, 8, 1)])),
            (1000, report(&[registration(7), registration(6)])),
        ]);
        let (result, snapshot, _) = execute(&request(CalibrationTask::Stationary), &mut io);
        assert_eq!(result, Ok(()));
        assert_eq!(snapshot.application, Application::Applied);
        assert_eq!(snapshot.end, Some(ObservationEnd::Ambiguous));
        assert!(snapshot.uncertainty.is_some());
    }
    #[test]
    fn deduplicated_heartbeats_are_not_a_host_timeout_and_unknown_can_strengthen() {
        let mut unknown = event(2, 8, 5, 4, 5, 9, 30);
        unknown[2] = 226;
        let mut io = ScriptedIo::new(vec![
            (
                0,
                report(&[
                    accepted(1, 8, 5),
                    ack(128, 0x24, 0),
                    event(2, 8, 5, 3, 0, 9, 2),
                ]),
            ),
            (10_000, report(&[unknown])),
            (35_000, report(&[applied(3, 8, 5)])),
        ]);
        let request = request(CalibrationTask::Sensitivity {
            axis: Axis::Z,
            revolutions: Revolutions::new(5).unwrap(),
        });
        let (result, snapshot, published) = execute(&request, &mut io);
        assert_eq!(result, Ok(()));
        assert_eq!(snapshot.application, Application::Applied);
        assert_eq!(snapshot.end, Some(ObservationEnd::Complete));
        assert!(published.iter().any(|snapshot| snapshot.elapsed_ms < 35_000
            && snapshot.application == Application::Unconfirmed));
        assert!(snapshot.elapsed_ms >= 55_000 && snapshot.elapsed_ms < 55_200);
        assert!(
            io.writes
                .iter()
                .filter(|bytes| bytes[3] == 224 && bytes[6] == 2)
                .count()
                >= 10
        );
    }
    #[test]
    fn wrong_operation_storage_and_automatic_success_cannot_amend_result() {
        let mut io = ScriptedIo::new(vec![(
            0,
            report(&[
                accepted(1, 8, 1),
                applied(2, 9, 1),
                storage(3, 9, 1),
                applied(4, 8, 129),
            ]),
        )]);
        io.stop = Some((1000, Arc::new(AtomicBool::new(false))));
        let (result, snapshot, _) = execute(&request(CalibrationTask::Stationary), &mut io);
        assert_eq!(result, Ok(()));
        assert_eq!(snapshot.application, Application::Unconfirmed);
        assert_eq!(snapshot.persistence, Persistence::Unknown);
        assert_eq!(snapshot.end, Some(ObservationEnd::Stopped));
    }
    #[test]
    fn cleanup_consumes_remaining_records_and_timeout_is_bounded() {
        let mut io = ScriptedIo::new(vec![(0, report(&[accepted(1, 8, 1), applied(2, 8, 1)]))]);
        io.cleanup.push(storage(3, 8, 1));
        let (result, snapshot, _) = execute(&request(CalibrationTask::Stationary), &mut io);
        assert_eq!(result, Ok(()));
        assert_eq!(snapshot.persistence, Persistence::StorageError);
        let mut io = ScriptedIo::new(vec![(0, report(&[accepted(1, 8, 1), applied(2, 8, 1)]))]);
        io.stop_ack = false;
        let (result, snapshot, _) = execute(&request(CalibrationTask::Stationary), &mut io);
        assert_eq!(result, Ok(()));
        assert_eq!(snapshot.application, Application::Applied);
        assert!(snapshot.cleanup_error.is_some());
        assert!(snapshot.elapsed_ms >= 22_000 && snapshot.elapsed_ms < 22_200);
    }
    #[test]
    fn recovery_observes_only_original_association_without_calibration_write() {
        let mut request = request(CalibrationTask::Stationary);
        request.mode = RunMode::Recover;
        request.recovery = Some(OperationIdentity {
            session: SESSION,
            operation: 8,
        });
        let mut io = ScriptedIo::new(Vec::new());
        io.stale.push(report(&[
            applied(2, 9, 1),
            storage(3, 9, 1),
            applied(4, 8, 1),
            storage(5, 8, 1),
        ]));
        let (result, snapshot, _) = execute(&request, &mut io);
        assert_eq!(result, Ok(()));
        assert_eq!(snapshot.association, request.recovery);
        assert_eq!(snapshot.application, Application::Applied);
        assert_eq!(snapshot.persistence, Persistence::StorageError);
        only_safe_commands(&io, None);
        request.recovery = None;
        let mut io = ScriptedIo::new(Vec::new());
        let (result, snapshot, _) = execute(&request, &mut io);
        assert!(result.unwrap_err().contains("original operation identity"));
        assert_eq!(snapshot.dispatch, Dispatch::NotAttempted);
        only_safe_commands(&io, None);
    }
    #[test]
    fn subscription_identity_and_online_failures_prevent_operation() {
        for case in 0..5 {
            let mut io = ScriptedIo::new(Vec::new());
            match case {
                0 => io.subscription_version = 2,
                1 => io.receiver_capabilities = 0,
                2 => io.subscription_mask = Some(3),
                3 => io.subscription_filter = Some(4),
                _ => io.subscription_active = false,
            }
            let (result, snapshot, _) = execute(&request(CalibrationTask::Stationary), &mut io);
            assert!(result.is_err() || snapshot.end == Some(ObservationEnd::Unsupported));
            assert_eq!(snapshot.dispatch, Dispatch::NotAttempted);
            only_safe_commands(&io, None);
        }
        let mut io = ScriptedIo::new(Vec::new());
        io.online = false;
        let (result, snapshot, _) = execute(&request(CalibrationTask::Stationary), &mut io);
        assert!(result.unwrap_err().contains("freshly confirm"));
        assert_eq!(snapshot.identity, None);
        only_safe_commands(&io, None);
    }
    #[test]
    fn saturated_drain_and_short_reports_fail_before_dispatch() {
        for malformed in [false, true] {
            let mut io = ScriptedIo::new(Vec::new());
            if malformed {
                io.enqueue(0, vec![0; 63]);
            } else {
                for _ in 0..129 {
                    io.enqueue(0, report(&[registration(6)]));
                }
            }
            let (result, snapshot, _) = execute(&request(CalibrationTask::Stationary), &mut io);
            assert!(result.is_err());
            assert_eq!(snapshot.dispatch, Dispatch::NotAttempted);
            only_safe_commands(&io, None);
        }
    }
    #[test]
    fn stop_before_write_partial_write_disconnect_and_absolute_deadline_do_not_replay() {
        let request = request(CalibrationTask::Stationary);
        let mut io = ScriptedIo::new(Vec::new());
        let mut engine = Engine::new(request.clone());
        assert_eq!(
            run(
                &request,
                &mut io,
                &mut engine,
                &AtomicBool::new(true),
                &|_| {}
            ),
            Ok(())
        );
        assert_eq!(engine.snapshot.end, Some(ObservationEnd::Stopped));
        only_safe_commands(&io, None);
        let mut io = ScriptedIo::new(Vec::new());
        io.fail_opcode = Some(2);
        let (result, snapshot, _) = execute(&request, &mut io);
        assert_eq!(result, Err("scripted partial write".into()));
        assert_eq!(snapshot.dispatch, Dispatch::Attempted);
        only_safe_commands(&io, Some(expected_operation(2, 0, 0)));
        let mut io = ScriptedIo::new(Vec::new());
        io.input.push_back((100, Err("scripted disconnect".into())));
        let (result, snapshot, _) = execute(&request, &mut io);
        assert_eq!(result, Err("scripted disconnect".into()));
        assert_eq!(snapshot.dispatch, Dispatch::Written);
        only_safe_commands(&io, Some(expected_operation(2, 0, 0)));
        let mut io = ScriptedIo::new(Vec::new());
        let (result, snapshot, _) = execute(&request, &mut io);
        assert_eq!(result, Ok(()));
        assert_eq!(snapshot.end, Some(ObservationEnd::Deadline));
        assert_eq!(snapshot.application, Application::Unconfirmed);
        only_safe_commands(&io, Some(expected_operation(2, 0, 0)));
    }
    #[test]
    fn operation_ack_in_each_record_position_preserves_other_events() {
        for index in 0..4 {
            let mut records = vec![accepted(1, 8, 1), applied(3, 8, 1), storage(2, 8, 1)];
            records.insert(index, ack(128, 2, 0));
            let mut io = ScriptedIo::new(vec![(0, report(&records))]);
            let (result, snapshot, _) = execute(&request(CalibrationTask::Stationary), &mut io);
            assert_eq!(result, Ok(()));
            assert_eq!(snapshot.dispatch, Dispatch::ReceiverConfirmed);
            assert_eq!(snapshot.application, Application::Applied);
            assert_eq!(snapshot.persistence, Persistence::StorageError);
        }
    }
    #[test]
    fn inspect_and_preparation_identity_change_never_send_calibration() {
        let mut request = request(CalibrationTask::Stationary);
        request.mode = RunMode::Inspect;
        let mut io = ScriptedIo::new(Vec::new());
        let (result, snapshot, _) = execute(&request, &mut io);
        assert_eq!(result, Ok(()));
        assert_eq!(snapshot.identity.as_deref(), Some("010203040506"));
        assert!(!snapshot.stationary_supported && !snapshot.sensitivity_supported);
        assert_eq!(snapshot.end, Some(ObservationEnd::Complete));
        only_safe_commands(&io, None);
        request.mode = RunMode::Execute;
        let mut io = ScriptedIo::new(Vec::new());
        io.stale.push(report(&[registration(7), registration(6)]));
        let (result, snapshot, _) = execute(&request, &mut io);
        assert!(result.is_err() || snapshot.end == Some(ObservationEnd::Ambiguous));
        assert_eq!(snapshot.dispatch, Dispatch::NotAttempted);
        only_safe_commands(&io, None);
    }
    #[test]
    fn partial_subscribe_and_renew_failures_clean_up_without_operation_retry() {
        let mut io = ScriptedIo::new(Vec::new());
        io.fail_control_action = Some(1);
        let (result, snapshot, _) = execute(&request(CalibrationTask::Stationary), &mut io);
        assert_eq!(result, Err("scripted partial event-control write".into()));
        assert_eq!(snapshot.dispatch, Dispatch::NotAttempted);
        assert_eq!(io.writes.last().unwrap()[6], 3);
        only_safe_commands(&io, None);
        let mut io = ScriptedIo::new(vec![(0, report(&[accepted(1, 8, 1), applied(2, 8, 1)]))]);
        io.fail_control_action = Some(2);
        let (result, snapshot, _) = execute(&request(CalibrationTask::Stationary), &mut io);
        assert_eq!(result, Err("scripted partial event-control write".into()));
        assert_eq!(snapshot.application, Application::Applied);
        assert_eq!(io.writes.last().unwrap()[6], 3);
        only_safe_commands(&io, Some(expected_operation(2, 0, 0)));
    }
    #[test]
    fn negative_ack_and_confirmation_timeout_are_distinct_from_application() {
        let mut io = ScriptedIo::new(vec![(0, report(&[ack(128, 2, 3)]))]);
        let (result, snapshot, _) = execute(&request(CalibrationTask::Stationary), &mut io);
        assert_eq!(result, Ok(()));
        assert_eq!(snapshot.dispatch, Dispatch::Rejected);
        assert_eq!(snapshot.application, Application::Rejected);
        let mut io = ScriptedIo::new(vec![(0, report(&[ack(128, 2, 4)]))]);
        io.stop = Some((1000, Arc::new(AtomicBool::new(false))));
        let (result, snapshot, _) = execute(&request(CalibrationTask::Stationary), &mut io);
        assert_eq!(result, Ok(()));
        assert_eq!(snapshot.dispatch, Dispatch::Written);
        assert_eq!(snapshot.application, Application::Unconfirmed);
        assert!(snapshot.uncertainty.is_some());
        only_safe_commands(&io, Some(expected_operation(2, 0, 0)));
    }
    #[test]
    fn active_preexisting_operation_blocks_dispatch_but_reordered_terminal_closes_it() {
        let mut io = ScriptedIo::new(Vec::new());
        io.stale
            .push(report(&[accepted(1, 8, 1), event(2, 8, 1, 3, 0, 4, 0)]));
        let (result, snapshot, _) = execute(&request(CalibrationTask::Stationary), &mut io);
        assert!(
            result
                .unwrap_err()
                .contains("preexisting matching calibration")
        );
        assert_eq!(snapshot.dispatch, Dispatch::NotAttempted);
        only_safe_commands(&io, None);
        let mut io = ScriptedIo::new(vec![(0, report(&[accepted(4, 9, 1), applied(5, 9, 1)]))]);
        io.stale.push(report(&[
            accepted(1, 8, 1),
            applied(3, 8, 1),
            event(2, 8, 1, 3, 0, 4, 0),
        ]));
        let (result, snapshot, _) = execute(&request(CalibrationTask::Stationary), &mut io);
        assert_eq!(result, Ok(()));
        assert_eq!(
            snapshot.association,
            Some(OperationIdentity {
                session: SESSION,
                operation: 9
            })
        );
        assert_eq!(snapshot.application, Application::Applied);
        only_safe_commands(&io, Some(expected_operation(2, 0, 0)));
    }
    #[test]
    fn reused_operation_and_half_range_sequence_cannot_produce_false_application() {
        for records in [
            vec![accepted(1, 8, 1), accepted(2, 8, 1), applied(3, 8, 1)],
            vec![accepted(1, 8, 1), applied(32769, 8, 1)],
        ] {
            let mut io = ScriptedIo::new(vec![(0, report(&records))]);
            let (result, snapshot, _) = execute(&request(CalibrationTask::Stationary), &mut io);
            assert_eq!(result, Ok(()));
            assert_ne!(snapshot.application, Application::Applied);
            assert_eq!(snapshot.end, Some(ObservationEnd::Ambiguous));
        }
    }
    #[test]
    fn sensitivity_axes_and_explicit_boundary_counts_reach_real_transport() {
        for (axis, index) in [(Axis::X, 0), (Axis::Y, 1), (Axis::Z, 2)] {
            for revolutions in [1, 100] {
                let request = request(CalibrationTask::Sensitivity {
                    axis,
                    revolutions: Revolutions::new(revolutions).unwrap(),
                });
                let mut io = ScriptedIo::new(vec![(
                    0,
                    report(&[accepted(1, 8, 5), ack(128, 0x24, 0), applied(2, 8, 5)]),
                )]);
                let (result, snapshot, _) = execute(&request, &mut io);
                assert_eq!(result, Ok(()));
                assert_eq!(snapshot.application, Application::Applied);
                only_safe_commands(
                    &io,
                    Some(expected_operation(0x24, index, revolutions as u8)),
                );
            }
        }
    }
    #[test]
    fn applied_record_before_rf_replacement_in_same_report_is_preserved() {
        let mut io = ScriptedIo::new(vec![(
            0,
            report(&[
                accepted(1, 8, 1),
                applied(2, 8, 1),
                registration(7),
                registration(6),
            ]),
        )]);
        let (result, snapshot, _) = execute(&request(CalibrationTask::Stationary), &mut io);
        assert_eq!(result, Ok(()));
        assert_eq!(snapshot.application, Application::Applied);
        assert_eq!(snapshot.end, Some(ObservationEnd::Ambiguous));
        assert!(snapshot.uncertainty.is_some());
    }
}
