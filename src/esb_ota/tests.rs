use super::protocol::{self, ABORT, ACTIVATE, BEGIN, DATA, INFO, QUERY, STATUS, VERIFY};
use super::{
    FirmwareInfo, FlashGroup, QueryResult, RECEIVER_ID, Session, Target, Transport,
    flash_groups_with, flash_with, query_with, take_receiver_groups, validate_selection,
};
use crate::{
    domain::{ReceiverEndpoint, TrackerLink},
    firmware::FirmwareImage,
    update::{ActivationSubmission, ReceiverActivationReport, ReceiverAvailability, UpdateOutcome},
};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

fn failure(outcome: &UpdateOutcome) -> &str {
    match outcome {
        UpdateOutcome::Failed(error) => error,
        other => panic!("Expected failed update, got {other:?}"),
    }
}

fn endpoint() -> ReceiverEndpoint {
    ReceiverEndpoint {
        key: "receiver-path".into(),
        path: b"receiver-path".to_vec(),
        label: "Receiver".into(),
        serial_number: Some("serial-1".into()),
    }
}
fn info() -> FirmwareInfo {
    FirmwareInfo {
        version: "1.2.3".into(),
        build_date: "2026-10-10 01:02:04".into(),
        firmware_size: 12000,
        bootloader: "adafruit_uf2".into(),
        protocol_version: 1,
        board_target: "test/nrf52840".into(),
        flash_base: 0x1000,
    }
}
pub(super) fn target(id: u8) -> Target {
    Target {
        receiver: endpoint(),
        id,
        address: (id != RECEIVER_ID).then(|| format!("AABBCCDDEE{id:02X}")),
        info: info(),
    }
}
fn image(packets: usize) -> FirmwareImage {
    let mut data: Vec<u8> = (0..packets * 60 - 7).map(|i| (i % 251) as u8).collect();
    data[..4].copy_from_slice(&0x20010000u32.to_le_bytes());
    data[4..8].copy_from_slice(&0x1009u32.to_le_bytes());
    FirmwareImage {
        crc32: crc32fast::hash(&data),
        data,
        base_address: 0x1000,
        mcuboot: false,
    }
}
fn info_records(id: u8, firmware: &FirmwareInfo) -> Vec<[u8; 16]> {
    let mut raw = [0u8; 66];
    raw[0] = 0x22;
    raw[1] = id;
    raw[2..5].copy_from_slice(&[1, 2, 3]);
    let dt: u32 = (6 << 25) | (10 << 21) | (10 << 16) | (1 << 11) | (2 << 5) | 2;
    raw[5..9].copy_from_slice(&dt.to_be_bytes());
    raw[9..13].copy_from_slice(&firmware.firmware_size.to_le_bytes());
    raw[13] = match firmware.bootloader.as_str() {
        "none" => 0,
        "adafruit_uf2" => 1,
        "nrf5_opendfu" => 2,
        "mcuboot" => 3,
        _ => 9,
    };
    raw[14] = firmware.protocol_version;
    raw[15..15 + firmware.board_target.len()].copy_from_slice(firmware.board_target.as_bytes());
    raw[63..65].copy_from_slice(&((firmware.flash_base >> 12) as u16).to_be_bytes());
    raw[65] = protocol::crc8(&raw[..65]);
    (0..6)
        .map(|index| {
            let mut r = [0; 16];
            r[0] = INFO;
            r[1] = id;
            r[2] = index;
            let start = 2 + 13 * index as usize;
            if start < 66 {
                let n = 13.min(66 - start);
                r[3..3 + n].copy_from_slice(&raw[start..start + n]);
            }
            r
        })
        .collect()
}
fn status(id: u8, code: u8, next: usize, bytes: usize) -> [u8; 16] {
    let mut r = [0; 16];
    r[0] = STATUS;
    r[1] = id;
    r[2] = code;
    r[3..5].copy_from_slice(&(next as u16).to_be_bytes());
    r[5..9].copy_from_slice(&(bytes as u32).to_le_bytes());
    r
}
struct Staging {
    data: Vec<u8>,
    size: usize,
    crc: u32,
    next: usize,
    verified: bool,
}
struct Emulator {
    time: Duration,
    incoming: VecDeque<[u8; 16]>,
    firmware: BTreeMap<u8, FirmwareInfo>,
    staging: BTreeMap<u8, Staging>,
    writes: Vec<[u8; 64]>,
    query_writes: Vec<(u8, Duration)>,
    silent_query: Option<u8>,
    corrupt_query: Option<u8>,
    query_write_errors: BTreeMap<(u8, usize), String>,
    query_read_error_at: Option<Duration>,
    query_reads: Vec<(Duration, i32, usize)>,
    query_write_ms: u64,
    query_read_ms: Option<u64>,
    missing_addresses: Vec<u8>,
    delay_addresses: bool,
    address_after: Option<(u8, Duration)>,
    query_records_per_read: usize,
    shared_query_mailbox: bool,
    query_relay_id: Option<u8>,
    query_collisions: usize,
    omit_query_trailer: Option<u8>,
    reassign_on_query: Option<u8>,
    reassign_after_queries: usize,
    reassign_not_before: Duration,
    full_fifo_after_queries: Option<usize>,
    cancel_on_read_at: Option<(Duration, Arc<AtomicBool>)>,
    dropped: Option<usize>,
    dropped_for: Option<(u8, usize)>,
    transfer_read_ms: Option<u64>,
    corrupt: Option<u8>,
    ready_failure: Option<u8>,
    changed_address: Option<u8>,
    silent_activate: bool,
    receiver_reboot: bool,
    reconnects: usize,
    max_window: usize,
    max_parallel: usize,
    cancel_on_data: Option<Arc<AtomicBool>>,
    automatic_verify: bool,
    omit_final_receiver_data_status: bool,
    tombstones: BTreeMap<u8, Duration>,
    blocked_begins: usize,
    begin_writes: Vec<(u8, Duration)>,
    abort_writes: Vec<(u8, Duration)>,
    partial_verify: bool,
    partial_complete: bool,
    activating_only: bool,
    activation_write_error: Option<String>,
    activation_status: Option<[u8; 16]>,
    activation_query_status: Option<[u8; 16]>,
    cancel_on_activate: Option<Arc<AtomicBool>>,
    disconnect_after_activate: bool,
    silent_activation_query: bool,
    wrong_activation_info: bool,
    activation_reconnect_error: Option<super::ReceiverOpenError>,
    activation_attempt_at: Option<Duration>,
    activation_query_write_error: bool,
    activation_reconnect_delay: Duration,
    abort_write_error: Option<u8>,
    begin_write_error: Option<u8>,
    data_since_read: usize,
    max_receiver_burst: usize,
    // Source-derived relay model: six records are enqueued from one sleeping
    // system-workqueue handler while HID drain work cannot run. Hot ordinary
    // traffic reserves only two priority slots after terminal unsuppression.
    hot_after_complete: bool,
    completed: usize,
    dropped_info_chunks: usize,
    reassign_after_complete: Option<(u8, usize)>,
    delayed_info: Option<(u8, Duration)>,
    delayed_records: Option<(Duration, Vec<[u8; 16]>)>,
    incomplete_info: Option<(u8, usize)>,
    dropped_query_fragments: BTreeMap<(u8, usize), Vec<u8>>,
    query_replies: BTreeMap<(u8, usize), Vec<[u8; 16]>>,
    delayed_query_replies: BTreeMap<(u8, usize), Duration>,
    blocked_queries: usize,
}
impl Emulator {
    fn new(ids: &[u8]) -> Self {
        let firmware = ids
            .iter()
            .copied()
            .chain([RECEIVER_ID])
            .map(|id| (id, info()))
            .collect();
        Self {
            time: Duration::ZERO,
            incoming: VecDeque::new(),
            firmware,
            staging: BTreeMap::new(),
            writes: Vec::new(),
            query_writes: Vec::new(),
            silent_query: None,
            corrupt_query: None,
            query_write_errors: BTreeMap::new(),
            query_read_error_at: None,
            query_reads: Vec::new(),
            query_write_ms: 0,
            query_read_ms: None,
            missing_addresses: Vec::new(),
            delay_addresses: false,
            address_after: None,
            query_records_per_read: 4,
            shared_query_mailbox: false,
            query_relay_id: None,
            query_collisions: 0,
            omit_query_trailer: None,
            reassign_on_query: None,
            reassign_after_queries: 1,
            reassign_not_before: Duration::ZERO,
            full_fifo_after_queries: None,
            cancel_on_read_at: None,
            dropped: None,
            dropped_for: None,
            transfer_read_ms: None,
            corrupt: None,
            ready_failure: None,
            changed_address: None,
            silent_activate: false,
            receiver_reboot: false,
            reconnects: 0,
            max_window: 0,
            max_parallel: 0,
            cancel_on_data: None,
            automatic_verify: false,
            omit_final_receiver_data_status: false,
            tombstones: BTreeMap::new(),
            blocked_begins: 0,
            begin_writes: Vec::new(),
            abort_writes: Vec::new(),
            partial_verify: false,
            partial_complete: false,
            activating_only: false,
            activation_write_error: None,
            activation_status: None,
            activation_query_status: None,
            cancel_on_activate: None,
            disconnect_after_activate: false,
            silent_activation_query: false,
            wrong_activation_info: false,
            activation_reconnect_error: None,
            activation_attempt_at: None,
            activation_query_write_error: false,
            activation_reconnect_delay: Duration::ZERO,
            abort_write_error: None,
            begin_write_error: None,
            data_since_read: 0,
            max_receiver_burst: 0,
            hot_after_complete: false,
            completed: 0,
            dropped_info_chunks: 0,
            reassign_after_complete: None,
            delayed_info: None,
            delayed_records: None,
            incomplete_info: None,
            dropped_query_fragments: BTreeMap::new(),
            query_replies: BTreeMap::new(),
            delayed_query_replies: BTreeMap::new(),
            blocked_queries: 0,
        }
    }
    fn queue_address(&mut self, id: u8) {
        if self.missing_addresses.contains(&id)
            || (self.delay_addresses && self.query_writes.is_empty())
            || self
                .address_after
                .is_some_and(|(slot, at)| slot == id && self.time < at)
        {
            return;
        }
        let mut r = [0; 16];
        r[0] = 0xff;
        r[1] = id;
        r[2..8].copy_from_slice(&[
            if self.changed_address == Some(id) {
                99
            } else {
                id
            },
            0xee,
            0xdd,
            0xcc,
            0xbb,
            0xaa,
        ]);
        self.incoming.push_back(r);
    }
}
impl Transport for Emulator {
    fn write(&mut self, r: &[u8; 64]) -> Result<(), String> {
        self.writes.push(*r);
        let id = r[1];
        match r[0] {
            QUERY => {
                self.query_writes.push((id, self.time));
                self.time += Duration::from_millis(self.query_write_ms);
                let attempt = self
                    .query_writes
                    .iter()
                    .filter(|(target, _)| *target == id)
                    .count();
                if let Some(error) = self.query_write_errors.remove(&(id, attempt)) {
                    return Err(error);
                }
                let after_activation = self.writes.iter().any(|r| r[0] == ACTIVATE);
                if after_activation && self.activation_query_write_error {
                    self.time += Duration::from_millis(100);
                    return Err("HID write: activation query temporarily unavailable".into());
                }
                if after_activation && self.silent_activation_query {
                    return Ok(());
                }
                if id != RECEIVER_ID && self.tombstones.values().any(|until| *until > self.time) {
                    self.blocked_queries += 1;
                    return Ok(());
                }
                if self.silent_query == Some(id) {
                    return Ok(());
                }
                if let Some(firmware) = self.firmware.get(&id) {
                    if self.shared_query_mailbox {
                        if self.query_relay_id.is_some() {
                            self.query_collisions += 1;
                            self.incoming.retain(|record| record[0] != INFO);
                        }
                        self.query_relay_id = Some(id);
                    }
                    let mut records = info_records(id, firmware);
                    if after_activation && self.wrong_activation_info {
                        let mut firmware = firmware.clone();
                        firmware.board_target = "other/nrf52840".into();
                        records = info_records(id, &firmware);
                    }
                    if self.corrupt_query == Some(id) {
                        records[4][8] ^= 1;
                    }
                    if self.omit_query_trailer == Some(id) {
                        records.pop();
                    }
                    if after_activation && let Some(record) = self.activation_query_status.take() {
                        records.push(record);
                    }
                    if self.hot_after_complete && self.completed != 0 && id != RECEIVER_ID {
                        // Capacity 32, with ordinary traffic filling 30 slots.
                        // No sender drain runs during this six-chunk handler.
                        let mut fifo = VecDeque::from(vec![[0x01; 16]; 30]);
                        for record in records {
                            if fifo.len() < 32 {
                                fifo.push_back(record);
                            } else {
                                self.dropped_info_chunks += 1;
                            }
                        }
                        self.incoming.extend(fifo);
                        return Ok(());
                    }
                    if let Some((slot, count)) = self.incomplete_info
                        && id == slot
                    {
                        records.truncate(count);
                    }
                    if let Some(reply) = self.query_replies.remove(&(id, attempt)) {
                        records = reply;
                    }
                    if let Some(dropped) = self.dropped_query_fragments.remove(&(id, attempt)) {
                        records.retain(|record| !dropped.contains(&record[2]));
                    }
                    if let Some(delay) =
                        self.delayed_query_replies
                            .remove(&(id, attempt))
                            .or_else(|| {
                                self.delayed_info
                                    .and_then(|(slot, delay)| (id == slot).then_some(delay))
                            })
                    {
                        self.delayed_records = Some((self.time + delay, records));
                        return Ok(());
                    }
                    if let Some(first) = records.first() {
                        self.incoming.push_back(*first); // Duplicate must not stand in for a missing chunk.
                    }
                    self.incoming.extend(records);
                }
            }
            BEGIN => {
                self.begin_writes.push((id, self.time));
                // Receiver esb_ota.c checks radio admission for a new session
                // before clearing the selected slot's abort tombstone.
                if self.staging.is_empty()
                    && self.tombstones.values().any(|until| *until > self.time)
                {
                    self.blocked_begins += 1;
                    return Ok(());
                }
                if self.ready_failure == Some(id) {
                    self.incoming.push_back(status(id, 0x13, 0, 0));
                    return Ok(());
                }
                self.staging.insert(
                    id,
                    Staging {
                        data: Vec::new(),
                        size: u32::from_le_bytes(r[2..6].try_into().unwrap()) as usize,
                        crc: u32::from_le_bytes(r[6..10].try_into().unwrap()),
                        next: 0,
                        verified: false,
                    },
                );
                self.max_parallel = self.max_parallel.max(self.staging.len());
                self.incoming.push_back(status(id, 1, 0, 0));
                if self.begin_write_error == Some(id) {
                    return Err("HID write: injected ambiguous BEGIN failure".into());
                }
            }
            DATA => {
                if id == RECEIVER_ID {
                    self.data_since_read += 1;
                    self.max_receiver_burst = self.max_receiver_burst.max(self.data_since_read);
                }
                let sequence = u16::from_be_bytes([r[2], r[3]]) as usize;
                let min = self.staging.values().map(|s| s.next).min().unwrap_or(0);
                self.max_window = self.max_window.max(sequence.saturating_sub(min) + 1);
                if self.dropped == Some(sequence) {
                    self.dropped = None;
                    return Ok(());
                }
                for (&target, stage) in &mut self.staging {
                    if id == RECEIVER_ID && target != id {
                        continue;
                    }
                    if self.dropped_for == Some((target, sequence)) {
                        self.dropped_for = None;
                        continue;
                    }
                    if stage.next == sequence {
                        let n = 60.min(stage.size - stage.data.len());
                        stage.data.extend_from_slice(&r[4..4 + n]);
                        stage.next += 1;
                        if self.automatic_verify && stage.data.len() == stage.size {
                            stage.verified = crc32fast::hash(&stage.data) == stage.crc
                                && self.corrupt != Some(target);
                            self.incoming.push_back(status(
                                target,
                                if stage.verified { 3 } else { 4 },
                                stage.next,
                                stage.data.len(),
                            ));
                        }
                    }
                }
                if let Some(cancel) = &self.cancel_on_data {
                    cancel.store(true, Ordering::Relaxed);
                }
            }
            VERIFY => {
                let stage = self.staging.get_mut(&id).unwrap();
                stage.verified = stage.data.len() == stage.size
                    && crc32fast::hash(&stage.data) == stage.crc
                    && self.corrupt != Some(id);
                self.incoming.push_back(status(
                    id,
                    if stage.verified { 3 } else { 4 },
                    stage.next - usize::from(self.partial_verify),
                    stage.data.len() - usize::from(self.partial_verify),
                ));
            }
            ACTIVATE => {
                self.activation_attempt_at = Some(self.time);
                assert!(
                    self.staging[&id].verified,
                    "activation before CRC verification"
                );
                if let Some(cancel) = &self.cancel_on_activate {
                    cancel.store(true, Ordering::Relaxed);
                }
                if let Some(record) = self.activation_status.take() {
                    self.incoming.push_back(record);
                } else if !(self.silent_activate || id == RECEIVER_ID && self.receiver_reboot) {
                    self.incoming.push_back(status(
                        id,
                        if self.activating_only { 5 } else { 6 },
                        self.staging[&id].next - usize::from(self.partial_complete),
                        self.staging[&id].size - usize::from(self.partial_complete),
                    ));
                }
                if let Some(error) = self.activation_write_error.take() {
                    return Err(error);
                }
            }
            ABORT => {
                self.abort_writes.push((id, self.time));
                if id != RECEIVER_ID {
                    // queue_abort runs even when COMPLETE already removed id.
                    self.tombstones
                        .insert(id, self.time + Duration::from_secs(10));
                }
                self.staging.remove(&id);
                if self.abort_write_error == Some(id) {
                    return Err("HID write: injected ABORT failure".into());
                }
            }
            _ => panic!("unexpected opcode"),
        }
        Ok(())
    }
    fn read(&mut self, frame: &mut [u8; 64], timeout_ms: i32) -> Result<usize, String> {
        if self.disconnect_after_activate && self.writes.iter().any(|r| r[0] == ACTIVATE) {
            self.disconnect_after_activate = false;
            return Err("HID read: injected stale-handle disconnect".into());
        }
        self.data_since_read = 0;
        if self.staging.is_empty() {
            self.query_reads
                .push((self.time, timeout_ms, self.query_writes.len()));
            if self.query_writes.len() >= self.reassign_after_queries
                && self.time >= self.reassign_not_before
                && let Some(id) = self.reassign_on_query.take()
            {
                self.changed_address = Some(id);
                self.queue_address(id);
                let changed = self.incoming.pop_back().unwrap();
                // A transient reassignment must remain invalid even when
                // the expected address is restored in the very same frame.
                self.changed_address = None;
                self.queue_address(id);
                let restored = self.incoming.pop_back().unwrap();
                self.incoming.push_front(restored);
                self.incoming.push_front(changed);
            }
        }
        let elapsed = if self.staging.is_empty() {
            self.query_read_ms
                .unwrap_or(timeout_ms.max(1) as u64)
                .min(timeout_ms.max(1) as u64)
        } else {
            self.transfer_read_ms.unwrap_or(timeout_ms.max(1) as u64)
        };
        self.time += Duration::from_millis(elapsed);
        if self.query_read_error_at.is_some_and(|at| self.time >= at) {
            self.query_read_error_at = None;
            return Err("HID read: injected query failure".into());
        }
        if let Some((at, cancel)) = &self.cancel_on_read_at
            && self.time >= *at
        {
            cancel.store(true, Ordering::Relaxed);
        }
        if let Some((id, at)) = self.address_after
            && self.time >= at
        {
            self.address_after = None;
            self.queue_address(id);
        }
        if self
            .delayed_records
            .as_ref()
            .is_some_and(|(at, _)| self.time >= *at)
        {
            let (_, records) = self.delayed_records.take().unwrap();
            self.incoming.extend(records);
        }
        if let Some((slot, count)) = self.reassign_after_complete
            && self.completed >= count
        {
            self.reassign_after_complete = None;
            self.changed_address = Some(slot);
            self.queue_address(slot);
            let changed = self.incoming.pop_back().unwrap();
            self.changed_address = None;
            self.queue_address(slot);
            let restored = self.incoming.pop_back().unwrap();
            self.incoming.push_front(restored);
            self.incoming.push_front(changed);
        }
        let full_fifo = self
            .full_fifo_after_queries
            .is_some_and(|count| self.query_writes.len() >= count);
        if self.incoming.is_empty() {
            for (&id, stage) in &self.staging {
                if self.omit_final_receiver_data_status
                    && id == RECEIVER_ID
                    && stage.data.len() == stage.size
                    && !self.automatic_verify
                {
                    continue;
                }
                self.incoming.push_back(status(
                    id,
                    if self.automatic_verify && stage.verified {
                        3
                    } else {
                        2
                    },
                    stage.next,
                    stage.data.len(),
                ));
            }
            if !full_fifo {
                let ids: Vec<_> = self
                    .firmware
                    .keys()
                    .copied()
                    .filter(|id| *id != RECEIVER_ID)
                    .collect();
                for id in ids {
                    self.queue_address(id);
                }
            }
        }
        if full_fifo {
            // Receiver hid_fill_padding only supplies registrations when fewer
            // than four FIFO records are available. Model continuous sensor
            // traffic behind queued FW_INFO/status, leaving no padding slots.
            while self.incoming.len() < 4 {
                let mut sensor = [0; 16];
                sensor[0] = 0x01;
                self.incoming.push_back(sensor);
            }
        }
        let mut length = 0;
        while length < 64 {
            if self.staging.is_empty() && length / 16 >= self.query_records_per_read {
                break;
            }
            let Some(record) = self.incoming.pop_front() else {
                break;
            };
            if record[0] == INFO && record[2] == 5 {
                self.query_relay_id = None;
            }
            // Receiver auto-removes COMPLETE/errors before accepting another
            // session; host success cleanup must not create a new tombstone.
            if record[0] == STATUS && matches!(record[2], 4 | 6 | 0x10..=0x14) {
                if record[2] == 6 {
                    self.completed += 1;
                }
                self.staging.remove(&record[1]);
            }
            frame[length..length + 16].copy_from_slice(&record);
            length += 16;
        }
        Ok(length)
    }
    fn now(&self) -> Duration {
        self.time
    }
    fn abort_pending_for(&self) -> Duration {
        self.tombstones
            .values()
            .map(|until| until.saturating_sub(self.time))
            .max()
            .unwrap_or(Duration::ZERO)
    }
    fn reconnect(
        &mut self,
        cancel: &AtomicBool,
        deadline: Duration,
    ) -> Result<(), super::ReceiverOpenError> {
        self.reconnects += 1;
        super::check_cancel(cancel).map_err(super::ReceiverOpenError::Unavailable)?;
        if let Some(error) = self.activation_reconnect_error.take() {
            return Err(error);
        }
        assert!(self.time < deadline);
        self.time = deadline.min(self.time + self.activation_reconnect_delay);
        self.incoming.clear();
        self.staging.clear();
        Ok(())
    }
}

#[test]
fn query_reassembles_crc_checked_info_and_only_writes_read_command() {
    let cancel = AtomicBool::new(false);
    let mut session = Session::new(Emulator::new(&[0]), &cancel);
    let links = [TrackerLink {
        receiver_key: endpoint().key,
        slot: 0,
        address: target(0).address,
        online: true,
    }];
    let QueryResult {
        receiver,
        trackers,
        failures,
    } = query_with(&mut session, &endpoint(), &links, true, &mut |_| {}).unwrap();
    assert_eq!(receiver.unwrap().info, info());
    assert_eq!(trackers[0].info, info());
    assert!(failures.is_empty());
    assert!(session.io.writes.iter().all(|r| r[0] == QUERY));
}

fn links(ids: &[u8]) -> Vec<TrackerLink> {
    ids.iter()
        .map(|&slot| TrackerLink {
            receiver_key: endpoint().key,
            slot,
            address: target(slot).address,
            online: true,
        })
        .collect()
}

#[test]
fn queries_wait_for_each_relay_and_finish_early_without_an_extra_gap() {
    let cancel = AtomicBool::new(false);
    let io = Emulator::new(&[0, 1]);
    let mut session = Session::new(io, &cancel);
    let result = query_with(
        &mut session,
        &endpoint(),
        &links(&[0, 1]),
        true,
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(result.trackers.len(), 2);
    assert!(result.failures.is_empty());
    assert_eq!(
        session.io.query_writes,
        [
            (RECEIVER_ID, Duration::from_millis(100)),
            (0, Duration::from_millis(300)),
            (1, Duration::from_millis(500)),
        ]
    );
    assert!(
        session
            .io
            .query_reads
            .iter()
            .skip(1)
            .map(|(_, _, writes)| *writes)
            .eq([1, 1, 2, 2, 3, 3])
    );
    assert_eq!(session.io.time, Duration::from_millis(700));
    // Registrations from the one initial drain validate the expected identities.
    assert_eq!(session.addresses[0], target(0).address);
    assert_eq!(session.addresses[1], target(1).address);
}

#[test]
fn shared_mailbox_relay_never_collides_even_when_crc_finishes_before_the_trailer() {
    for corrupt in [false, true] {
        let cancel = AtomicBool::new(false);
        let mut io = Emulator::new(&[0, 1]);
        io.shared_query_mailbox = true;
        io.query_records_per_read = 1;
        io.query_read_ms = Some(2);
        io.firmware.get_mut(&0).unwrap().firmware_size = 24000;
        io.firmware.get_mut(&1).unwrap().firmware_size = 36000;
        if corrupt {
            io.corrupt_query = Some(0);
        }
        let mut session = Session::new(io, &cancel);
        let result = query_with(
            &mut session,
            &endpoint(),
            &links(&[0, 1]),
            true,
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(session.io.query_collisions, 0);
        assert_eq!(
            session.io.query_writes,
            [
                (RECEIVER_ID, Duration::from_millis(100)),
                (0, Duration::from_millis(114)),
                (1, Duration::from_millis(128)),
            ]
        );
        assert_eq!(session.io.time, Duration::from_millis(142));
        assert_eq!(result.receiver.unwrap().info, info());
        assert_eq!(result.trackers.last().unwrap().info.firmware_size, 36000);
        if corrupt {
            assert_eq!(result.trackers.len(), 1);
            assert_eq!(result.failures.len(), 1);
            assert!(result.failures[0].1.contains("CRC"));
        } else {
            assert_eq!(result.trackers[0].info.firmware_size, 24000);
            assert!(result.failures.is_empty());
        }
        assert_eq!(session.io.writes.len(), 3); // No resends or other commands.
    }
}

#[test]
fn missing_trailer_uses_only_its_target_deadline_then_continues_without_resending() {
    for corrupt in [false, true] {
        let cancel = AtomicBool::new(false);
        let mut io = Emulator::new(&[0, 1]);
        io.omit_query_trailer = Some(0);
        if corrupt {
            io.corrupt_query = Some(0);
        }
        let mut session = Session::new(io, &cancel);
        let result = query_with(
            &mut session,
            &endpoint(),
            &links(&[0, 1]),
            false,
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(
            session.io.query_writes,
            [
                (0, Duration::from_millis(100)),
                (1, Duration::from_millis(3100))
            ]
        );
        assert_eq!(session.io.time, Duration::from_millis(3300));
        assert_eq!(result.trackers.last().unwrap().id, 1);
        if corrupt {
            assert_eq!(result.trackers.len(), 1);
            assert!(result.failures[0].1.contains("CRC"));
        } else {
            assert_eq!(result.trackers.len(), 2);
            assert!(result.failures.is_empty());
        }
        assert_eq!(session.io.writes.len(), 2);
    }
}

#[test]
fn late_identity_must_arrive_within_its_own_target_deadline() {
    for at in [3000, 3200] {
        let cancel = AtomicBool::new(false);
        let mut io = Emulator::new(&[0, 1]);
        io.delay_addresses = true;
        io.address_after = Some((0, Duration::from_millis(at)));
        let mut session = Session::new(io, &cancel);
        let result = query_with(
            &mut session,
            &endpoint(),
            &links(&[0, 1]),
            false,
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(session.io.query_writes.len(), 2);
        assert_eq!(
            session.io.query_writes[1].1,
            Duration::from_millis(at.min(3100))
        );
        assert_eq!(result.trackers.last().unwrap().id, 1);
        assert_eq!(session.addresses[0], target(0).address);
        if at == 3000 {
            assert_eq!(result.trackers.len(), 2);
            assert!(result.failures.is_empty());
        } else {
            assert_eq!(result.trackers.len(), 1);
            assert_eq!(result.failures.len(), 1);
            assert!(
                result.failures[0]
                    .1
                    .contains("RF address could not be revalidated")
            );
        }
    }
}

#[test]
fn incremental_query_only_requests_specified_tracker_without_receiver() {
    let cancel = AtomicBool::new(false);
    let mut io = Emulator::new(&[0, 1, 2]);
    io.silent_query = Some(RECEIVER_ID);
    let mut session = Session::new(io, &cancel);
    let result = query_with(
        &mut session,
        &endpoint(),
        &links(&[1, 1]),
        false,
        &mut |_| {},
    )
    .unwrap();
    assert!(result.receiver.is_none());
    assert_eq!(result.trackers.len(), 1);
    assert_eq!(result.trackers[0].id, 1);
    assert_eq!(result.trackers[0].address, target(1).address);
    assert!(result.failures.is_empty());
    assert_eq!(session.io.query_writes, [(1, Duration::from_millis(100))]);
    assert_eq!(session.io.writes, [protocol::command(QUERY, 1)]);
}

#[test]
fn cancellation_during_initial_drain_or_current_query_stops_querying() {
    for (at, expected_requests) in [(50, 0), (200, 1)] {
        let cancel = Arc::new(AtomicBool::new(false));
        let mut io = Emulator::new(&[0]);
        io.cancel_on_read_at = Some((Duration::from_millis(at), cancel.clone()));
        let mut session = Session::new(io, &cancel);
        let error =
            query_with(&mut session, &endpoint(), &links(&[0]), true, &mut |_| {}).unwrap_err();
        assert!(error.contains("cancelled"));
        assert_eq!(session.io.writes.len(), expected_requests);
        if expected_requests != 0 {
            assert_eq!(
                session.io.query_writes,
                [(RECEIVER_ID, Duration::from_millis(100))]
            );
        }
    }
}

#[test]
fn silent_or_corrupt_query_preserves_other_trackers_with_individual_deadlines() {
    for corrupt in [false, true] {
        let cancel = AtomicBool::new(false);
        let mut io = Emulator::new(&[0, 1]);
        if corrupt {
            io.corrupt_query = Some(0);
        } else {
            io.silent_query = Some(0);
        }
        let mut session = Session::new(io, &cancel);
        let result = query_with(
            &mut session,
            &endpoint(),
            &links(&[0, 1]),
            false,
            &mut |_| {},
        )
        .unwrap();
        assert!(result.receiver.is_none());
        assert_eq!(result.failures.len(), 1);
        assert_eq!(result.failures[0].0, 0);
        assert!(result.failures[0].1.contains(if corrupt {
            "CRC"
        } else {
            "did not return complete firmware information"
        }));
        assert_eq!(result.trackers.len(), 1);
        assert_eq!(result.trackers[0].id, 1);
        assert_eq!(
            session.io.query_writes,
            [
                (0, Duration::from_millis(100)),
                (1, Duration::from_millis(if corrupt { 300 } else { 3100 })),
            ]
        );
        if corrupt {
            assert!(session.io.time < Duration::from_millis(3100));
        } else {
            assert_eq!(session.io.time, Duration::from_millis(3300));
        }
    }
}

#[test]
fn receiver_query_failure_does_not_discard_valid_trackers() {
    for corrupt in [false, true] {
        let cancel = AtomicBool::new(false);
        let mut io = Emulator::new(&[0, 1]);
        if corrupt {
            io.corrupt_query = Some(RECEIVER_ID);
        } else {
            io.silent_query = Some(RECEIVER_ID);
        }
        let mut session = Session::new(io, &cancel);
        let result = query_with(
            &mut session,
            &endpoint(),
            &links(&[0, 1]),
            true,
            &mut |_| {},
        )
        .unwrap();
        assert!(result.receiver.is_none());
        assert_eq!(result.trackers.len(), 2);
        assert_eq!(result.failures.len(), 1);
        assert_eq!(result.failures[0].0, RECEIVER_ID);
        assert!(result.failures[0].1.contains(if corrupt {
            "CRC"
        } else {
            "did not return complete"
        }));
        assert_eq!(session.io.query_writes.len(), 3);
    }
}

#[test]
fn silent_and_corrupt_targets_keep_individual_errors_and_partial_success() {
    let cancel = AtomicBool::new(false);
    let mut io = Emulator::new(&[0, 1, 2]);
    io.silent_query = Some(0);
    io.corrupt_query = Some(1);
    let mut session = Session::new(io, &cancel);
    let result = query_with(
        &mut session,
        &endpoint(),
        &links(&[0, 1, 2]),
        true,
        &mut |_| {},
    )
    .unwrap();
    assert!(result.receiver.is_some());
    assert_eq!(result.trackers.len(), 1);
    assert_eq!(result.trackers[0].id, 2);
    assert_eq!(result.failures.len(), 2);
    assert!(
        result
            .failures
            .iter()
            .any(|(id, e)| *id == 0 && e.contains("did not return complete"))
    );
    assert!(
        result
            .failures
            .iter()
            .any(|(id, e)| *id == 1 && e.contains("CRC"))
    );
    assert_eq!(session.io.time, Duration::from_millis(3700));
    assert_eq!(session.io.query_writes.len(), 4);
}

#[test]
fn each_no_response_gets_three_seconds_after_its_write_and_caps_reads() {
    let cancel = AtomicBool::new(false);
    let mut io = Emulator::new(&[0, 1]);
    io.queue_address(0);
    io.queue_address(1);
    io.firmware.clear();
    io.query_write_ms = 7;
    io.query_read_ms = Some(73);
    let mut session = Session::new(io, &cancel);
    let result = query_with(
        &mut session,
        &endpoint(),
        &links(&[0, 1]),
        true,
        &mut |_| {},
    )
    .unwrap();
    assert!(result.receiver.is_none());
    assert!(result.trackers.is_empty());
    assert_eq!(result.failures.len(), 3);
    assert!(
        result
            .failures
            .iter()
            .all(|(_, e)| e.contains("did not return complete"))
    );
    let last_write_end = Duration::from_millis(6121);
    let deadline = last_write_end + Duration::from_secs(3);
    assert_eq!(session.io.time, deadline);
    assert_eq!(
        session.io.query_writes,
        [
            (RECEIVER_ID, Duration::from_millis(100)),
            (0, Duration::from_millis(3107)),
            (1, Duration::from_millis(6114)),
        ]
    );
    let reads: Vec<_> = session
        .io
        .query_reads
        .iter()
        .filter(|(_, _, writes)| *writes > 0)
        .collect();
    assert_eq!(reads[0].0, Duration::from_millis(107));
    assert!(reads.iter().all(|(at, timeout, writes)| {
        let deadline = session.io.query_writes[*writes - 1].1
            + Duration::from_millis(7)
            + Duration::from_secs(3);
        *at + Duration::from_millis(*timeout as u64) <= deadline
    }));
    assert!(reads.last().unwrap().1 < 100);
}

#[test]
fn query_waits_for_identity_within_its_deadline_not_the_flash_pin_timeout() {
    for missing in [false, true] {
        let cancel = AtomicBool::new(false);
        let mut io = Emulator::new(&[0, 1]);
        io.delay_addresses = true;
        if missing {
            io.missing_addresses.push(0);
        }
        let mut session = Session::new(io, &cancel);
        let result = query_with(
            &mut session,
            &endpoint(),
            &links(&[0, 1]),
            false,
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(
            session.io.query_writes,
            [
                (0, Duration::from_millis(100)),
                (1, Duration::from_millis(if missing { 3100 } else { 400 })),
            ]
        );
        if missing {
            assert_eq!(result.trackers.len(), 1);
            assert_eq!(result.trackers[0].id, 1);
            assert_eq!(result.failures.len(), 1);
            assert!(
                result.failures[0]
                    .1
                    .contains("RF address could not be revalidated")
            );
            assert_eq!(session.io.time, Duration::from_millis(3300));
        } else {
            assert_eq!(result.trackers.len(), 2);
            assert!(result.failures.is_empty());
            assert!(session.io.time < Duration::from_millis(3100));
        }
    }
}

#[test]
fn reassignment_while_another_target_is_queried_is_sticky() {
    let cancel = AtomicBool::new(false);
    let mut io = Emulator::new(&[0, 1]);
    io.reassign_on_query = Some(0);
    let mut session = Session::new(io, &cancel);
    let result = query_with(
        &mut session,
        &endpoint(),
        &links(&[0, 1]),
        true,
        &mut |_| {},
    )
    .unwrap();
    assert!(result.receiver.is_some());
    assert_eq!(result.trackers.len(), 1);
    assert_eq!(result.trackers[0].id, 1);
    assert_eq!(result.failures.len(), 1);
    assert!(result.failures[0].1.contains("reassigned"));
    assert_eq!(session.addresses[0], target(0).address);
}

#[test]
fn unknown_identity_cannot_be_rescued_after_a_transient_reassignment() {
    let cancel = AtomicBool::new(false);
    let mut io = Emulator::new(&[0, 1]);
    io.delay_addresses = true;
    io.reassign_on_query = Some(0);
    let mut session = Session::new(io, &cancel);
    let result = query_with(
        &mut session,
        &endpoint(),
        &links(&[0, 1]),
        false,
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(result.trackers.len(), 1);
    assert_eq!(result.trackers[0].id, 1);
    assert_eq!(result.failures.len(), 1);
    assert!(result.failures[0].1.contains("reassigned"));
    assert_eq!(session.addresses[0], target(0).address);
    assert_eq!(session.io.query_writes.len(), 2);
}

#[test]
fn completed_target_is_discarded_if_its_slot_changes_during_the_next_query() {
    let cancel = AtomicBool::new(false);
    let mut io = Emulator::new(&[0, 1]);
    io.reassign_on_query = Some(0);
    io.reassign_after_queries = 2;
    let mut session = Session::new(io, &cancel);
    let result = query_with(
        &mut session,
        &endpoint(),
        &links(&[0, 1]),
        false,
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(result.trackers.len(), 1);
    assert_eq!(result.trackers[0].id, 1);
    assert_eq!(result.failures.len(), 1);
    assert!(result.failures[0].1.contains("reassigned"));
    assert_eq!(session.io.query_writes.len(), 2);
}

#[test]
fn invalid_query_inputs_are_rejected_before_any_slot_can_be_queried() {
    let cancel = AtomicBool::new(false);
    let mut io = Emulator::new(&[0, 1, 2, 3, 4]);
    io.changed_address = Some(4);
    io.queue_address(4);
    let mut requested = links(&[0, 0, 1, 2, 3, 4, 16, 255]);
    requested[1].address = target(1).address;
    requested[2].address = None;
    requested[3].address = Some("not-an-address".into());
    requested[4].receiver_key = "other-receiver".into();
    // A valid duplicate may not rescue a slot with a conflicting bad entry.
    requested.extend(links(&[1, 2, 3]));
    let mut session = Session::new(io, &cancel);
    let result = query_with(&mut session, &endpoint(), &requested, false, &mut |_| {}).unwrap();
    assert!(result.trackers.is_empty());
    assert_eq!(result.failures.len(), 7);
    assert!(session.io.query_writes.is_empty());
    assert_eq!(session.io.time, Duration::from_millis(100));
}

#[test]
fn flash_identity_pin_remains_active_during_subsequent_queries() {
    let cancel = AtomicBool::new(false);
    let mut session = Session::new(Emulator::new(&[0, 1]), &cancel);
    session
        .query_info(&endpoint(), 0, target(0).address.as_deref())
        .unwrap();
    session.io.changed_address = Some(0);
    session.io.queue_address(0);
    assert!(
        session
            .query_info(&endpoint(), 1, target(1).address.as_deref())
            .unwrap_err()
            .contains("reassigned")
    );
    assert_eq!(session.io.query_writes.len(), 2);
}

#[test]
fn info_rejects_crc_and_does_not_accept_duplicate_chunks_as_complete() {
    let mut chunks = protocol::InfoChunks::new(3);
    let mut records = info_records(3, &info());
    for _ in 0..8 {
        assert!(chunks.push(&records[0]).unwrap().is_none());
    }
    records[4][8] ^= 1;
    for record in &records[1..4] {
        assert!(chunks.push(record).unwrap().is_none());
    }
    assert!(chunks.push(&records[4]).unwrap_err().contains("CRC"));
}

#[test]
fn parallel_transfer_gap_fill_verifies_exact_bytes_and_preserves_target_failure() {
    let cancel = AtomicBool::new(false);
    let mut io = Emulator::new(&[0, 1, 2]);
    io.dropped = Some(10);
    io.corrupt = Some(1);
    let mut session = Session::new(io, &cancel);
    let image = image(300);
    let outcomes = flash_with(
        &mut session,
        &[target(0), target(1), target(2)],
        &image,
        &info().board_target,
        &mut |_| {},
    )
    .unwrap();
    assert!(matches!(outcomes[0].outcome, UpdateOutcome::Completed));
    assert!(matches!(outcomes[1].outcome, UpdateOutcome::Failed(_)));
    assert!(matches!(outcomes[2].outcome, UpdateOutcome::Completed));
    assert!(session.io.max_window <= 111);
    assert_eq!(session.io.max_parallel, 2);
    assert!(
        session
            .io
            .writes
            .iter()
            .filter(|r| r[0] == DATA && r[2..4] == 10u16.to_be_bytes())
            .count()
            > 1
    );
    assert!(
        !session
            .io
            .writes
            .iter()
            .any(|r| r[0] == ACTIVATE && r[1] == 1)
    );
    assert!(session.io.staging.is_empty());
}

#[test]
fn nrf52833_uses_single_target_batches() {
    let cancel = AtomicBool::new(false);
    let mut io = Emulator::new(&[0, 1]);
    let mut targets = [target(0), target(1)];
    for target in &mut targets {
        target.info.board_target = "test/nrf52833".into();
        io.firmware.insert(target.id, target.info.clone());
    }
    let mut session = Session::new(io, &cancel);
    let results = flash_with(
        &mut session,
        &targets,
        &image(2),
        &targets[0].info.board_target,
        &mut |_| {},
    )
    .unwrap();
    assert!(
        results
            .iter()
            .all(|r| matches!(r.outcome, UpdateOutcome::Completed))
    );
    assert_eq!(session.io.max_parallel, 1);
}

#[test]
fn opendfu_receiver_still_relays_trackers_but_cannot_self_flash() {
    let cancel = AtomicBool::new(false);
    let mut io = Emulator::new(&[0]);
    io.firmware.get_mut(&RECEIVER_ID).unwrap().bootloader = "nrf5_opendfu".into();
    let mut session = Session::new(io, &cancel);
    assert!(matches!(
        flash_with(
            &mut session,
            &[target(0)],
            &image(2),
            &info().board_target,
            &mut |_| {}
        )
        .unwrap()[0]
            .outcome,
        UpdateOutcome::Completed
    ));
    let mut receiver = target(RECEIVER_ID);
    receiver.info.bootloader = "nrf5_opendfu".into();
    assert!(
        validate_selection(&[receiver], &image(2), &info().board_target)
            .unwrap_err()
            .contains("OpenDFU")
    );
}

#[test]
fn reassigned_tracker_and_changed_firmware_never_begin() {
    let cancel = AtomicBool::new(false);
    let mut io = Emulator::new(&[0, 1]);
    io.changed_address = Some(0);
    io.firmware.get_mut(&1).unwrap().firmware_size += 1;
    let mut session = Session::new(io, &cancel);
    let results = flash_with(
        &mut session,
        &[target(0), target(1)],
        &image(2),
        &info().board_target,
        &mut |_| {},
    )
    .unwrap();
    assert!(
        results
            .iter()
            .all(|r| matches!(r.outcome, UpdateOutcome::Failed(_)))
    );
    assert!(!session.io.writes.iter().any(|r| r[0] == BEGIN));
}

#[test]
fn cancellation_aborts_every_started_target_without_activation() {
    let cancel = Arc::new(AtomicBool::new(false));
    let mut io = Emulator::new(&[0, 1]);
    io.cancel_on_data = Some(cancel.clone());
    let mut session = Session::new(io, &cancel);
    let results = flash_with(
        &mut session,
        &[target(0), target(1)],
        &image(20),
        &info().board_target,
        &mut |_| {},
    )
    .unwrap();
    assert!(
        results
            .iter()
            .all(|r| matches!(r.outcome, UpdateOutcome::Failed(_)))
    );
    for id in [0, 1] {
        assert!(
            session
                .io
                .writes
                .iter()
                .any(|r| r[0] == ABORT && r[1] == id)
        );
    }
    assert!(!session.io.writes.iter().any(|r| r[0] == ACTIVATE));
}

#[test]
fn silent_activation_is_not_success() {
    let cancel = AtomicBool::new(false);
    let mut io = Emulator::new(&[0]);
    io.silent_activate = true;
    let mut session = Session::new(io, &cancel);
    let results = flash_with(
        &mut session,
        &[target(0)],
        &image(2),
        &info().board_target,
        &mut |_| {},
    )
    .unwrap();
    assert!(matches!(results[0].outcome, UpdateOutcome::Failed(_)));
}

#[test]
fn receiver_fresh_info_is_unconfirmed_and_crc_failure_never_activates() {
    let cancel = AtomicBool::new(false);
    let mut io = Emulator::new(&[]);
    io.receiver_reboot = true;
    let mut session = Session::new(io, &cancel);
    let results = flash_with(
        &mut session,
        &[target(RECEIVER_ID)],
        &image(3),
        &info().board_target,
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(
        results[0].outcome,
        UpdateOutcome::ActivationUnconfirmed(ReceiverActivationReport {
            submission: ActivationSubmission::Sent,
            availability: ReceiverAvailability::Responding,
        })
    );
    assert_eq!(session.io.reconnects, 1);
    let mut io = Emulator::new(&[]);
    io.corrupt = Some(RECEIVER_ID);
    let mut session = Session::new(io, &cancel);
    let results = flash_with(
        &mut session,
        &[target(RECEIVER_ID)],
        &image(3),
        &info().board_target,
        &mut |_| {},
    )
    .unwrap();
    assert!(matches!(results[0].outcome, UpdateOutcome::Failed(_)));
    assert!(!session.io.writes.iter().any(|r| r[0] == ACTIVATE));
}

#[test]
fn incompatible_mcuboot_board_and_receiver_mix_rejected_before_io() {
    let image = image(2);
    let mut mcuboot = target(0);
    mcuboot.info.bootloader = "mcuboot".into();
    mcuboot.info.flash_base = 0;
    assert!(validate_selection(&[mcuboot], &image, &info().board_target).is_err());
    assert!(validate_selection(&[target(0)], &image, "other/nrf52840").is_err());
    assert!(
        validate_selection(
            &[target(0), target(RECEIVER_ID)],
            &image,
            &info().board_target
        )
        .is_err()
    );
}

#[test]
fn begin_rejection_does_not_prevent_other_target_completing() {
    let cancel = AtomicBool::new(false);
    let mut io = Emulator::new(&[0, 1]);
    io.ready_failure = Some(0);
    let mut session = Session::new(io, &cancel);
    let results = flash_with(
        &mut session,
        &[target(0), target(1)],
        &image(2),
        &info().board_target,
        &mut |_| {},
    )
    .unwrap();
    assert!(matches!(results[0].outcome, UpdateOutcome::Failed(_)));
    assert!(matches!(results[1].outcome, UpdateOutcome::Completed));
}

#[test]
fn mcuboot_image_uses_zero_base_and_full_image_crc() {
    let cancel = AtomicBool::new(false);
    let mut selected = target(0);
    selected.info.bootloader = "mcuboot".into();
    selected.info.flash_base = 0;
    let mut image = image(4);
    image.data[..4].copy_from_slice(&0x96f3b83du32.to_le_bytes());
    image.data[8..10].copy_from_slice(&32u16.to_le_bytes());
    let body_size = (image.data.len() - 32) as u32;
    image.data[12..16].copy_from_slice(&body_size.to_le_bytes());
    image.mcuboot = true;
    image.base_address = 0;
    image.crc32 = crc32fast::hash(&image.data);
    let mut io = Emulator::new(&[0]);
    io.firmware.insert(0, selected.info.clone());
    let mut session = Session::new(io, &cancel);
    let results = flash_with(
        &mut session,
        &[selected],
        &image,
        &info().board_target,
        &mut |_| {},
    )
    .unwrap();
    assert!(matches!(results[0].outcome, UpdateOutcome::Completed));
    let begin = session.io.writes.iter().find(|r| r[0] == BEGIN).unwrap();
    assert_eq!(&begin[61..63], &[0, 0]);
    assert_eq!(&begin[6..10], &image.crc32.to_le_bytes());
}

#[test]
fn missing_target_stays_failed_and_never_starts_flash() {
    let cancel = AtomicBool::new(false);
    let mut session = Session::new(Emulator::new(&[]), &cancel);
    let results = flash_with(
        &mut session,
        &[target(0)],
        &image(2),
        &info().board_target,
        &mut |_| {},
    )
    .unwrap();
    assert!(matches!(results[0].outcome, UpdateOutcome::Failed(_)));
    assert!(!session.io.writes.iter().any(|r| r[0] == BEGIN));
}

#[test]
fn image_requiring_uninstalled_softdevice_is_rejected_before_io() {
    let mut image = image(2);
    image.base_address = 0x27000;
    let error = validate_selection(&[target(0)], &image, &info().board_target).unwrap_err();
    assert!(error.contains("SoftDevice"));
}

#[test]
fn ram_engine_auto_verification_is_preserved_until_activation() {
    let cancel = AtomicBool::new(false);
    let mut io = Emulator::new(&[0, 1]);
    io.automatic_verify = true;
    io.corrupt = Some(1);
    let mut session = Session::new(io, &cancel);
    let results = flash_with(
        &mut session,
        &[target(0), target(1)],
        &image(30),
        &info().board_target,
        &mut |_| {},
    )
    .unwrap();
    assert!(matches!(results[0].outcome, UpdateOutcome::Completed));
    assert!(matches!(results[1].outcome, UpdateOutcome::Failed(_)));
    assert!(!session.io.writes.iter().any(|r| r[0] == VERIFY));
    assert!(
        session
            .io
            .writes
            .iter()
            .any(|r| r[0] == ACTIVATE && r[1] == 0)
    );
    assert!(
        !session
            .io
            .writes
            .iter()
            .any(|r| r[0] == ACTIVATE && r[1] == 1)
    );
}

#[test]
fn early_verify_ok_is_rejected_without_activation() {
    let cancel = AtomicBool::new(false);
    let mut io = Emulator::new(&[0]);
    io.incoming.push_back(status(0, 3, 1, 60));
    let mut session = Session::new(io, &cancel);
    let mut states = vec![super::TransferState {
        index: 0,
        id: 0,
        next: 0,
        ring: 0,
        verified: false,
    }];
    let mut results = vec![super::TargetOutcome {
        target: target(0),
        outcome: UpdateOutcome::Failed("Not started".into()),
    }];
    super::consume(&mut session, &mut states, &image(2), &mut results, 1, 20).unwrap();
    assert!(states.is_empty());
    assert!(failure(&results[0].outcome).contains("complete image"));
    assert!(!session.io.writes.iter().any(|r| r[0] == ACTIVATE));
}

#[test]
fn parallel_progress_uses_individual_acknowledgements_and_survives_gap_fill() {
    use crate::update::OtaPhase;
    let cancel = AtomicBool::new(false);
    let mut io = Emulator::new(&[0, 1, 2]);
    io.dropped_for = Some((1, 152));
    io.transfer_read_ms = Some(100);
    let firmware = image(400);
    let mut session = Session::new(io, &cancel);
    let mut events = Vec::new();
    let results = flash_with(
        &mut session,
        &[target(0), target(1), target(2)],
        &firmware,
        "test/nrf52840",
        &mut |event| events.push(event),
    )
    .unwrap();
    assert!(
        results
            .iter()
            .all(|result| matches!(result.outcome, UpdateOutcome::Completed))
    );
    assert_eq!(session.io.max_parallel, 2);
    assert!(
        session.io.writes.iter().filter(|r| r[0] == DATA).count() > 800,
        "a missing target-specific packet must really exercise replay"
    );
    assert!(events.iter().all(|event| event.ota.len() == 3));
    assert!(events.iter().all(|event| {
        event
            .ota
            .iter()
            .enumerate()
            .all(|(id, row)| row.receiver_key == "receiver-path" && row.id == id as u8)
    }));
    assert!(
        events
            .iter()
            .any(|event| event.ota[0].phase == OtaPhase::Transferring
                && event.ota[1].phase == OtaPhase::Transferring
                && event.ota[0].completed > event.ota[1].completed
                && event.ota[2].phase == OtaPhase::Queued
                && event.ota[2].total == firmware.data.len() as u64)
    );
    for id in 0..3 {
        let first_ack = events
            .iter()
            .map(|event| &event.ota[id])
            .find(|row| row.phase == OtaPhase::Transferring && row.completed > 0)
            .unwrap();
        assert_eq!(
            first_ack.completed,
            8 * 60,
            "must use the consumed ACK, not the preceding floor"
        );
        assert_eq!(
            first_ack.bytes_per_second, None,
            "rate needs a warm-up window"
        );
        let rates: Vec<_> = events
            .iter()
            .filter_map(|event| event.ota[id].bytes_per_second)
            .collect();
        assert!(
            !rates.is_empty(),
            "real production stream must expose measured rates"
        );
        assert!(
            rates.iter().all(|&rate| rate > 0 && rate <= 28_800),
            "only acknowledged advances count, never broadcast/replayed bytes"
        );
        assert!(
            events
                .windows(2)
                .all(|pair| pair[0].ota[id].completed <= pair[1].ota[id].completed)
        );
        assert!(events.iter().any(|event| {
            let row = &event.ota[id];
            row.phase == OtaPhase::Verifying && row.completed == row.total
        }));
        assert!(
            events
                .iter()
                .filter(|event| !matches!(event.ota[id].phase, OtaPhase::Transferring))
                .all(|event| event.ota[id].bytes_per_second.is_none()
                    && event.ota[id].eta_seconds.is_none())
        );
    }
    assert!(
        events.iter().any(|event| {
            let row = &event.ota[1];
            row.phase == OtaPhase::Transferring
                && row.completed == 152 * 60
                && row.bytes_per_second.is_none()
                && row.eta_seconds.is_none()
        }),
        "stalled target must lose stale estimates"
    );
    assert!(
        events
            .iter()
            .any(|event| event.ota[0].phase == OtaPhase::Transferring
                && event.ota[0].completed == event.ota[0].total
                && event.completed == 0),
        "all DATA acknowledged is not operation success"
    );
    assert!(
        events
            .iter()
            .any(|event| event.ota[0].phase == OtaPhase::Completed
                && event.ota[1].phase == OtaPhase::Completed
                && event.ota[2].phase == OtaPhase::Transferring)
    );
    assert_eq!(events.last().unwrap().completed, 3);
    assert!(
        events
            .last()
            .unwrap()
            .ota
            .iter()
            .all(|row| row.phase == OtaPhase::Completed)
    );
}

#[test]
fn progress_reports_rejection_crc_failure_and_unconfirmed_activation_as_failed() {
    use crate::update::OtaPhase;
    for failure in 0..3 {
        let cancel = AtomicBool::new(false);
        let mut io = Emulator::new(&[0, 1]);
        match failure {
            0 => io.ready_failure = Some(0),
            1 => io.corrupt = Some(0),
            _ => io.silent_activate = true,
        }
        let mut session = Session::new(io, &cancel);
        let mut events = Vec::new();
        let results = flash_with(
            &mut session,
            &[target(0), target(1)],
            &image(20),
            "test/nrf52840",
            &mut |event| events.push(event),
        )
        .unwrap();
        assert!(matches!(results[0].outcome, UpdateOutcome::Failed(_)));
        let final_event = events.last().unwrap();
        assert_eq!(final_event.ota[0].phase, OtaPhase::Failed);
        assert!(
            events
                .iter()
                .all(|event| event.ota[0].phase != OtaPhase::Completed)
        );
        if failure != 2 {
            assert!(matches!(results[1].outcome, UpdateOutcome::Completed));
            assert_eq!(final_event.ota[1].phase, OtaPhase::Completed);
            assert_eq!(final_event.completed, 1);
        } else {
            assert_eq!(final_event.completed, 0);
        }
        assert!(
            final_event
                .ota
                .iter()
                .all(|row| row.bytes_per_second.is_none() && row.eta_seconds.is_none())
        );
    }
}

#[test]
fn cancellation_progress_covers_started_and_queued_targets_without_activation() {
    use crate::update::OtaPhase;
    let cancel = Arc::new(AtomicBool::new(false));
    let mut io = Emulator::new(&[0, 1, 2]);
    io.cancel_on_data = Some(cancel.clone());
    let mut session = Session::new(io, &cancel);
    let mut events = Vec::new();
    let results = flash_with(
        &mut session,
        &[target(0), target(1), target(2)],
        &image(30),
        "test/nrf52840",
        &mut |event| events.push(event),
    )
    .unwrap();
    assert!(
        results
            .iter()
            .all(|outcome| failure(&outcome.outcome).contains("cancelled"))
    );
    let last = events.last().unwrap();
    assert_eq!(last.completed, 0);
    assert!(last.ota.iter().all(|row| row.phase == OtaPhase::Cancelled
        && row.bytes_per_second.is_none()
        && row.eta_seconds.is_none()));
    assert!(!session.io.writes.iter().any(|report| report[0] == ACTIVATE));
    for id in [0, 1] {
        assert!(
            session
                .io
                .writes
                .iter()
                .any(|report| report[0] == ABORT && report[1] == id)
        );
    }
}

#[test]
fn receiver_progress_only_completes_after_confirmed_activation() {
    use crate::update::OtaPhase;
    for (reboot, silent_activate) in [(false, false), (true, false), (false, true)] {
        let cancel = AtomicBool::new(false);
        let mut io = Emulator::new(&[]);
        io.receiver_reboot = reboot;
        io.silent_activate = silent_activate;
        io.omit_final_receiver_data_status = true;
        io.transfer_read_ms = Some(100);
        let mut session = Session::new(io, &cancel);
        let mut events = Vec::new();
        let outcomes = flash_with(
            &mut session,
            &[target(RECEIVER_ID)],
            &image(100),
            "test/nrf52840",
            &mut |event| events.push(event),
        )
        .unwrap();
        assert_eq!(
            matches!(outcomes[0].outcome, UpdateOutcome::Completed),
            !reboot && !silent_activate
        );
        assert!(events.iter().all(|event| event.ota.len() == 1
            && event.ota[0].receiver_key == "receiver-path"
            && event.ota[0].id == RECEIVER_ID));
        let verifying = events
            .iter()
            .find(|event| event.ota[0].phase == OtaPhase::Verifying)
            .unwrap();
        assert_eq!(verifying.completed, 0);
        assert!(
            verifying.ota[0].completed < verifying.ota[0].total,
            "omitted final DATA ACK must not invent byte progress before VERIFY_OK"
        );
        let activating = events
            .iter()
            .find(|event| event.ota[0].phase == OtaPhase::Activating)
            .unwrap();
        assert_eq!(activating.completed, 0);
        assert_eq!(
            activating.ota[0].completed, activating.ota[0].total,
            "full VERIFY_OK acknowledges the bytes before activation is confirmed"
        );
        assert_eq!(activating.ota[0].bytes_per_second, None);
        assert_eq!(activating.ota[0].eta_seconds, None);
        assert_eq!(
            events.last().unwrap().ota[0].phase,
            if reboot || silent_activate {
                OtaPhase::ActivationUnconfirmed
            } else {
                OtaPhase::Completed
            }
        );
        if reboot || silent_activate {
            assert!(
                events
                    .iter()
                    .all(|event| event.completed == 0 && event.ota[0].phase != OtaPhase::Completed)
            );
        }
        assert_eq!(
            session.io.reconnects,
            usize::from(reboot || silent_activate)
        );
    }
}

#[test]
fn preflight_error_finishes_every_snapshot_target_and_receiver_keys_are_not_labels() {
    use crate::update::OtaPhase;
    let cancel = AtomicBool::new(false);
    for key in ["receiver-a", "receiver-b"] {
        let mut frozen = vec![target(0), target(1)];
        for target in &mut frozen {
            target.receiver.key = key.into();
            target.receiver.label = "Identical friendly name".into();
        }
        let mut io = Emulator::new(&[0, 1]);
        io.silent_query = Some(RECEIVER_ID);
        let mut session = Session::new(io, &cancel);
        let mut events = Vec::new();
        let error = flash_with(
            &mut session,
            &frozen,
            &image(20),
            "test/nrf52840",
            &mut |event| events.push(event),
        )
        .unwrap_err();
        assert!(error.contains("firmware information"));
        let final_event = events.last().unwrap();
        assert_eq!(final_event.ota.len(), 2);
        assert!(
            final_event.ota.iter().all(|row| row.receiver_key == key
                && row.phase == OtaPhase::Failed
                && row.total == 0)
        );
        assert_eq!(final_event.completed, 0);
        assert!(!session.io.writes.iter().any(|report| report[0] == BEGIN));
    }
}

#[test]
fn completed_trackers_do_not_tombstone_later_batches_or_receiver_self_update() {
    let cancel = AtomicBool::new(false);
    let mut io = Emulator::new(&[0, 1, 2, 3]);
    io.firmware.get_mut(&RECEIVER_ID).unwrap().board_target = "relay/nrf52833".into();
    let mut session = Session::new(io, &cancel);
    let outcomes = flash_with(
        &mut session,
        &[target(0), target(1), target(2)],
        &image(3),
        &info().board_target,
        &mut |_| {},
    )
    .unwrap();
    assert!(
        outcomes
            .iter()
            .all(|outcome| matches!(outcome.outcome, UpdateOutcome::Completed))
    );
    assert_eq!(session.io.max_parallel, 2, "tracker SoC, not receiver SoC");
    assert_eq!(session.io.begin_writes.len(), 3);
    assert!(session.io.abort_writes.is_empty());
    // Separate calls model the UI's firmware-group lifecycle, including opening
    // a new Session for the same receiver.
    let mut session = Session::new(session.io, &cancel);
    assert!(matches!(
        flash_with(
            &mut session,
            &[target(3)],
            &image(4),
            &info().board_target,
            &mut |_| {},
        )
        .unwrap()[0]
            .outcome,
        UpdateOutcome::Completed
    ));
    let mut receiver = target(RECEIVER_ID);
    receiver.info = session.io.firmware[&RECEIVER_ID].clone();
    let board = receiver.info.board_target.clone();
    assert!(matches!(
        flash_with(&mut session, &[receiver], &image(12), &board, &mut |_| {},).unwrap()[0].outcome,
        UpdateOutcome::Completed
    ));
    assert_eq!(session.io.blocked_begins, 0);
    assert!(session.io.abort_writes.is_empty());
    assert!(session.io.max_receiver_burst <= 5);
}

#[test]
fn failed_target_is_aborted_once_and_next_group_waits_for_admission() {
    let cancel = AtomicBool::new(false);
    let mut io = Emulator::new(&[0, 1, 2]);
    io.corrupt = Some(0);
    let mut session = Session::new(io, &cancel);
    let outcomes = flash_with(
        &mut session,
        &[target(0), target(1), target(2)],
        &image(3),
        &info().board_target,
        &mut |_| {},
    )
    .unwrap();
    assert!(matches!(outcomes[0].outcome, UpdateOutcome::Failed(_)));
    assert!(
        outcomes[1..]
            .iter()
            .all(|outcome| matches!(outcome.outcome, UpdateOutcome::Completed))
    );
    assert_eq!(session.io.abort_writes.len(), 1);
    let (id, aborted_at) = session.io.abort_writes[0];
    assert_eq!(id, 0);
    let next_begin = session
        .io
        .begin_writes
        .iter()
        .find(|(id, _)| *id == 2)
        .unwrap()
        .1;
    assert!(next_begin >= aborted_at + Duration::from_secs(10));
    let next_query = session
        .io
        .query_writes
        .iter()
        .find(|(id, _)| *id == 2)
        .unwrap()
        .1;
    assert!(
        next_query < session.io.begin_writes[0].1,
        "all metadata is prepared before any BEGIN"
    );
    assert_eq!(session.io.blocked_begins, 0);
    assert_eq!(session.io.begin_writes.len(), 3);
}

#[test]
fn cancelled_cleanup_returns_promptly_and_new_session_waits_before_retry_query() {
    let cancel = Arc::new(AtomicBool::new(false));
    let mut io = Emulator::new(&[0, 1, 2]);
    io.cancel_on_data = Some(cancel.clone());
    let mut session = Session::new(io, &cancel);
    let outcomes = flash_with(
        &mut session,
        &[target(0), target(1), target(2)],
        &image(3),
        &info().board_target,
        &mut |_| {},
    )
    .unwrap();
    assert!(
        outcomes
            .iter()
            .all(|outcome| matches!(outcome.outcome, UpdateOutcome::Failed(_)))
    );
    assert_eq!(
        session
            .io
            .abort_writes
            .iter()
            .map(|(id, _)| *id)
            .collect::<Vec<_>>(),
        [0, 1]
    );
    let aborted_at = session.io.abort_writes.last().unwrap().1;
    assert_eq!(
        session.io.time, aborted_at,
        "cancel must not wait out admission"
    );
    cancel.store(false, Ordering::Relaxed);
    session.io.cancel_on_data = None;
    let query_count = session.io.query_writes.len();
    let mut session = Session::new(session.io, &cancel);
    let mut events = Vec::new();
    assert!(matches!(
        flash_with(
            &mut session,
            &[target(0)],
            &image(3),
            &info().board_target,
            &mut |event| events.push(event),
        )
        .unwrap()[0]
            .outcome,
        UpdateOutcome::Completed
    ));
    let retry_query = session.io.query_writes[query_count..]
        .iter()
        .find(|(id, _)| *id == 0)
        .unwrap()
        .1;
    assert!(retry_query >= aborted_at + Duration::from_secs(10));
    assert!(events.iter().any(|event| {
        event
            .ota
            .iter()
            .any(|row| row.phase == crate::update::OtaPhase::Preparing)
    }));
    assert_eq!(session.io.blocked_begins, 0);
    assert_eq!(session.io.abort_writes.len(), 2);
}

#[test]
fn ambiguous_begin_and_abort_write_errors_are_preserved_without_duplicate_cleanup() {
    let cancel = AtomicBool::new(false);
    let mut io = Emulator::new(&[0, 1]);
    io.begin_write_error = Some(0);
    io.abort_write_error = Some(0);
    let mut session = Session::new(io, &cancel);
    let outcomes = flash_with(
        &mut session,
        &[target(0), target(1)],
        &image(3),
        &info().board_target,
        &mut |_| {},
    )
    .unwrap();
    let error = failure(&outcomes[0].outcome);
    assert!(error.contains("BEGIN failure"));
    assert!(error.contains("ABORT failure"));
    assert_eq!(
        session
            .io
            .abort_writes
            .iter()
            .map(|(id, _)| *id)
            .collect::<Vec<_>>(),
        [0]
    );
    assert!(!session.io.begin_writes.iter().any(|(id, _)| *id == 1));
}

#[test]
fn phase_verification_and_complete_require_both_final_counters() {
    for (opcode, code) in [(VERIFY, 3), (ACTIVATE, 6)] {
        for (next, bytes) in [(1, 113), (2, 112)] {
            let cancel = AtomicBool::new(false);
            let mut io = Emulator::new(&[0]);
            io.incoming.push_back(status(0, code, next, bytes));
            let mut session = Session::new(io, &cancel);
            session.unfinished[0] = true;
            let targets = [target(0)];
            let mut telemetry = super::Telemetry::new(&targets);
            let mut states = vec![super::TransferState {
                index: 0,
                id: 0,
                next: 2,
                ring: 0,
                verified: false,
            }];
            let mut outcomes = vec![super::TargetOutcome {
                target: target(0),
                outcome: UpdateOutcome::Failed("Not started".into()),
            }];
            super::phase(
                &mut session,
                &mut states,
                &mut outcomes,
                opcode,
                &image(2),
                &info().board_target,
                Duration::from_secs(1),
                &[code],
                &mut |_| {},
                &mut telemetry,
            )
            .unwrap();
            assert!(states.is_empty());
            assert!(failure(&outcomes[0].outcome).contains("complete image"));
            assert_eq!(session.io.abort_writes.len(), 1);
        }
    }
}

#[test]
fn partial_verify_in_real_phase_never_activates_and_cleanup_errors_remain_visible() {
    let cancel = AtomicBool::new(false);
    let mut io = Emulator::new(&[0]);
    io.partial_verify = true;
    io.abort_write_error = Some(0);
    let mut session = Session::new(io, &cancel);
    let outcomes = flash_with(
        &mut session,
        &[target(0)],
        &image(3),
        &info().board_target,
        &mut |_| {},
    )
    .unwrap();
    let error = failure(&outcomes[0].outcome);
    assert!(error.contains("complete image"));
    assert!(error.contains("ABORT failure"));
    assert_eq!(session.io.abort_writes.len(), 1);
    assert!(!session.io.writes.iter().any(|r| r[0] == ACTIVATE));
}

#[test]
fn receiver_activation_acceptance_does_not_send_abort() {
    let cancel = AtomicBool::new(false);
    let mut io = Emulator::new(&[]);
    io.activating_only = true;
    let mut session = Session::new(io, &cancel);
    let firmware = image(3);
    let _outcomes = flash_with(
        &mut session,
        &[target(RECEIVER_ID)],
        &firmware,
        &info().board_target,
        &mut |_| {},
    )
    .unwrap();

    let verify = session
        .io
        .writes
        .iter()
        .position(|r| r[0] == VERIFY && r[1] == RECEIVER_ID)
        .expect("the receiver must receive VERIFY for the transferred image");
    let activate = session
        .io
        .writes
        .iter()
        .position(|r| r[0] == ACTIVATE && r[1] == RECEIVER_ID)
        .expect("the CRC-verified receiver image must reach ACTIVATE");
    assert!(verify < activate);
    let transferred: Vec<_> = session.io.writes[..verify]
        .iter()
        .filter(|r| r[0] == DATA && r[1] == RECEIVER_ID)
        .flat_map(|r| r[4..].iter().copied())
        .take(firmware.data.len())
        .collect();
    assert_eq!(transferred, firmware.data);
    assert_eq!(crc32fast::hash(&transferred), firmware.crc32);
    assert_eq!(
        session.io.writes[activate + 1..]
            .iter()
            .filter(|r| r[0] == ABORT && r[1] == RECEIVER_ID)
            .count(),
        0,
        "accepted receiver activation must not be followed by ABORT"
    );
}

#[test]
fn activation_progress_or_incomplete_complete_is_never_success() {
    for id in [0, RECEIVER_ID] {
        for incomplete_complete in [false, true] {
            let cancel = AtomicBool::new(false);
            let mut io = Emulator::new(&[0]);
            io.partial_complete = incomplete_complete;
            io.activating_only = !incomplete_complete;
            let mut session = Session::new(io, &cancel);
            let outcomes = flash_with(
                &mut session,
                &[target(id)],
                &image(3),
                &info().board_target,
                &mut |_| {},
            )
            .unwrap();
            if id == RECEIVER_ID && !incomplete_complete {
                assert!(matches!(
                    outcomes[0].outcome,
                    UpdateOutcome::ActivationUnconfirmed(_)
                ));
            } else {
                assert!(matches!(outcomes[0].outcome, UpdateOutcome::Failed(_)));
            }
            assert_eq!(
                session.io.abort_writes.len(),
                usize::from(id != RECEIVER_ID)
            );
        }
    }
}

#[test]
fn flash_preflight_uses_shared_query_trailer_and_one_address_response_deadline() {
    let cancel = AtomicBool::new(false);
    let mut io = Emulator::new(&[0, 1]);
    io.shared_query_mailbox = true;
    io.query_records_per_read = 1;
    io.query_read_ms = Some(10);
    io.delay_addresses = true;
    let mut session = Session::new(io, &cancel);
    assert!(
        flash_with(
            &mut session,
            &[target(0), target(1)],
            &image(3),
            &info().board_target,
            &mut |_| {},
        )
        .unwrap()
        .iter()
        .all(|outcome| matches!(outcome.outcome, UpdateOutcome::Completed))
    );
    assert_eq!(session.io.query_collisions, 0);
    assert!(session.io.query_relay_id.is_none());

    for (missing_address, incomplete_info) in [(true, false), (true, true), (false, true)] {
        let mut io = Emulator::new(&[0]);
        if missing_address {
            io.missing_addresses.push(0);
        }
        if incomplete_info {
            io.silent_query = Some(0);
        }
        let mut session = Session::new(io, &cancel);
        let outcomes = flash_with(
            &mut session,
            &[target(0)],
            &image(3),
            &info().board_target,
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(
            failure(&outcomes[0].outcome),
            if missing_address {
                "Tracker 0 RF address could not be revalidated; refresh receiver discovery"
            } else {
                "Target 0 did not return complete firmware information"
            }
        );
        let requested = session
            .io
            .query_writes
            .iter()
            .find(|(id, _)| *id == 0)
            .unwrap()
            .1;
        assert_eq!(
            session.io.time - requested,
            if missing_address {
                Duration::from_secs(3)
            } else {
                Duration::from_millis(6500)
            }
        );
        assert_eq!(
            session
                .io
                .query_writes
                .iter()
                .filter(|(id, _)| *id == 0)
                .count(),
            if missing_address { 1 } else { 2 }
        );
        assert!(session.io.begin_writes.is_empty());
        assert!(session.io.abort_writes.is_empty());
    }
}

#[test]
fn flash_preflight_missing_trailer_waits_only_its_deadline_and_keeps_valid_info() {
    let cancel = AtomicBool::new(false);
    let mut io = Emulator::new(&[0, 1]);
    io.omit_query_trailer = Some(0);
    let mut session = Session::new(io, &cancel);
    let outcomes = flash_with(
        &mut session,
        &[target(0), target(1)],
        &image(3),
        &info().board_target,
        &mut |_| {},
    )
    .unwrap();
    assert!(
        outcomes
            .iter()
            .all(|outcome| matches!(outcome.outcome, UpdateOutcome::Completed))
    );
    let zero = session
        .io
        .query_writes
        .iter()
        .find(|(id, _)| *id == 0)
        .unwrap()
        .1;
    let one = session
        .io
        .query_writes
        .iter()
        .find(|(id, _)| *id == 1)
        .unwrap()
        .1;
    assert_eq!(one - zero, Duration::from_secs(3));
    assert_eq!(
        session
            .io
            .query_writes
            .iter()
            .filter(|(id, _)| *id == 0)
            .count(),
        1
    );
}

#[test]
fn flash_preflight_retries_one_lost_payload_fragment_before_begin() {
    let cancel = AtomicBool::new(false);
    let mut io = Emulator::new(&[0]);
    io.dropped_query_fragments.insert((0, 1), vec![4]);
    io.shared_query_mailbox = true;
    io.query_records_per_read = 1;
    let mut session = Session::new(io, &cancel);
    let outcomes = flash_with(
        &mut session,
        &[target(0)],
        &image(3),
        &info().board_target,
        &mut |_| {},
    )
    .unwrap();

    assert_eq!(outcomes.len(), 1);
    assert!(
        matches!(outcomes[0].outcome, UpdateOutcome::Completed),
        "{:?}",
        outcomes[0].outcome
    );
    assert_eq!(
        session
            .io
            .query_writes
            .iter()
            .map(|(id, _)| *id)
            .collect::<Vec<_>>(),
        [RECEIVER_ID, 0, 0]
    );
    assert_eq!(
        session.io.query_writes[2].1 - session.io.query_writes[1].1,
        Duration::from_millis(3500)
    );
    let first_begin = session
        .io
        .writes
        .iter()
        .position(|r| r[0] == BEGIN)
        .unwrap();
    assert_eq!(
        &session.io.writes[..first_begin],
        &[
            protocol::command(QUERY, RECEIVER_ID),
            protocol::command(QUERY, 0),
            protocol::command(QUERY, 0),
        ],
        "receiver authorization and both tracker queries must precede BEGIN"
    );
    assert_eq!(
        session
            .io
            .begin_writes
            .iter()
            .map(|(id, _)| *id)
            .collect::<Vec<_>>(),
        [0]
    );
    assert!(session.io.abort_writes.is_empty());
    assert_eq!(session.io.query_collisions, 0);
}

#[test]
fn flash_preflight_exhausts_two_fresh_attempts_and_still_updates_successful_peers() {
    for dropped in [vec![4], vec![0, 1, 2, 3, 4, 5]] {
        let cancel = AtomicBool::new(false);
        let mut io = Emulator::new(&[0, 1, 2]);
        io.query_write_ms = 7;
        io.query_read_ms = Some(73);
        io.dropped_query_fragments.insert((0, 1), dropped.clone());
        io.dropped_query_fragments.insert((0, 2), dropped);
        let mut session = Session::new(io, &cancel);
        let outcomes = flash_with(
            &mut session,
            &[target(0), target(1), target(2)],
            &image(3),
            &info().board_target,
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(
            failure(&outcomes[0].outcome),
            "Target 0 did not return complete firmware information"
        );
        assert_eq!(outcomes[1].outcome, UpdateOutcome::Completed);
        assert_eq!(outcomes[2].outcome, UpdateOutcome::Completed);
        assert_eq!(
            session
                .io
                .query_writes
                .iter()
                .map(|(id, _)| *id)
                .collect::<Vec<_>>(),
            [RECEIVER_ID, 0, 0, 1, 2]
        );
        assert_eq!(
            session.io.query_writes[2].1 - session.io.query_writes[1].1,
            Duration::from_millis(3507)
        );
        assert_eq!(
            session.io.query_writes[3].1 - session.io.query_writes[2].1,
            Duration::from_millis(3007)
        );
        let first_begin = session
            .io
            .writes
            .iter()
            .position(|r| r[0] == BEGIN)
            .unwrap();
        assert_eq!(
            session.io.writes[..first_begin]
                .iter()
                .map(|r| (r[0], r[1]))
                .collect::<Vec<_>>(),
            [
                (QUERY, RECEIVER_ID),
                (QUERY, 0),
                (QUERY, 0),
                (QUERY, 1),
                (QUERY, 2),
            ]
        );
        assert_eq!(
            session
                .io
                .begin_writes
                .iter()
                .map(|(id, _)| *id)
                .collect::<Vec<_>>(),
            [1, 2]
        );
        assert!(session.io.abort_writes.is_empty());
        assert_eq!(session.io.reconnects, 0);
    }
}

#[test]
fn flash_preflight_never_combines_complementary_partial_attempts() {
    let cancel = AtomicBool::new(false);
    let mut io = Emulator::new(&[0]);
    io.dropped_query_fragments.insert((0, 1), vec![4]);
    io.dropped_query_fragments.insert((0, 2), vec![0, 1, 2, 3]);
    let mut session = Session::new(io, &cancel);
    let outcomes = flash_with(
        &mut session,
        &[target(0)],
        &image(3),
        &info().board_target,
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(
        failure(&outcomes[0].outcome),
        "Target 0 did not return complete firmware information"
    );
    assert_eq!(
        session.io.query_writes,
        [
            (RECEIVER_ID, Duration::from_millis(100)),
            (0, Duration::from_millis(300)),
            (0, Duration::from_millis(3800)),
        ]
    );
    assert_eq!(session.io.time, Duration::from_millis(6800));
    assert!(session.io.begin_writes.is_empty());
    assert!(session.io.abort_writes.is_empty());
}

#[test]
fn flash_preflight_discards_late_complete_reply_during_retry_drain() {
    let cancel = AtomicBool::new(false);
    let mut io = Emulator::new(&[0]);
    io.delayed_query_replies
        .insert((0, 1), Duration::from_millis(3200));
    io.dropped_query_fragments.insert((0, 2), vec![4]);
    let mut session = Session::new(io, &cancel);
    let outcomes = flash_with(
        &mut session,
        &[target(0)],
        &image(3),
        &info().board_target,
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(
        failure(&outcomes[0].outcome),
        "Target 0 did not return complete firmware information"
    );
    assert_eq!(
        session.io.query_writes,
        [
            (RECEIVER_ID, Duration::from_millis(100)),
            (0, Duration::from_millis(300)),
            (0, Duration::from_millis(3800)),
        ]
    );
    assert_eq!(session.io.time, Duration::from_millis(6800));
    assert!(session.io.begin_writes.is_empty());
    assert!(session.io.abort_writes.is_empty());
}

#[test]
fn flash_preflight_cancellation_during_retry_drain_prevents_second_query() {
    let cancel = Arc::new(AtomicBool::new(false));
    let mut io = Emulator::new(&[0]);
    io.dropped_query_fragments.insert((0, 1), vec![4]);
    io.cancel_on_read_at = Some((Duration::from_millis(3400), cancel.clone()));
    let mut session = Session::new(io, &cancel);
    let outcomes = flash_with(
        &mut session,
        &[target(0)],
        &image(3),
        &info().board_target,
        &mut |_| {},
    )
    .unwrap();
    assert!(failure(&outcomes[0].outcome).contains("cancelled"));
    assert_eq!(session.io.time, Duration::from_millis(3400));
    assert_eq!(
        session.io.query_writes,
        [
            (RECEIVER_ID, Duration::from_millis(100)),
            (0, Duration::from_millis(300)),
        ]
    );
    assert!(session.io.begin_writes.is_empty());
    assert!(session.io.abort_writes.is_empty());
    assert_eq!(session.io.reconnects, 0);
}

#[test]
fn flash_preflight_reassignment_during_retry_drain_invalidates_prepared_peers() {
    for changed_slot in [0, 1] {
        let cancel = AtomicBool::new(false);
        let mut io = Emulator::new(&[0, 1]);
        io.dropped_query_fragments.insert((1, 1), vec![4]);
        io.reassign_on_query = Some(changed_slot);
        io.reassign_after_queries = 3;
        io.reassign_not_before = Duration::from_millis(3600);
        let mut session = Session::new(io, &cancel);
        let outcomes = flash_with(
            &mut session,
            &[target(0), target(1)],
            &image(3),
            &info().board_target,
            &mut |_| {},
        )
        .unwrap();
        for outcome in &outcomes {
            assert!(
                failure(&outcome.outcome).contains("reassigned"),
                "{changed_slot}: {:?}",
                outcome.outcome
            );
        }
        assert_eq!(
            session.io.query_writes,
            [
                (RECEIVER_ID, Duration::from_millis(100)),
                (0, Duration::from_millis(300)),
                (1, Duration::from_millis(500)),
            ]
        );
        assert_eq!(session.io.time, Duration::from_millis(3700));
        assert!(session.io.begin_writes.is_empty());
        assert!(session.io.abort_writes.is_empty());
        assert_eq!(session.io.reconnects, 0);
    }
}

#[test]
fn flash_preflight_keeps_hard_query_errors_terminal_on_either_attempt() {
    #[derive(Clone, Copy, Debug)]
    enum Fault {
        Crc,
        EmptyBoard,
        UnterminatedBoard,
        ChangedMetadata,
        ShortWrite,
        WriteFailure,
        ReadFailure,
    }
    for attempt in [1, 2] {
        for fault in [
            Fault::Crc,
            Fault::EmptyBoard,
            Fault::UnterminatedBoard,
            Fault::ChangedMetadata,
            Fault::ShortWrite,
            Fault::WriteFailure,
            Fault::ReadFailure,
        ] {
            let cancel = AtomicBool::new(false);
            let mut io = Emulator::new(&[0]);
            if attempt == 2 {
                io.dropped_query_fragments.insert((0, 1), vec![4]);
            }
            let expected = match fault {
                Fault::Crc => {
                    let mut records = info_records(0, &info());
                    records[4][8] ^= 1;
                    io.query_replies.insert((0, attempt), records);
                    "FW_INFO CRC mismatch; query again"
                }
                Fault::EmptyBoard | Fault::UnterminatedBoard => {
                    let mut malformed = info();
                    malformed.board_target = if matches!(fault, Fault::EmptyBoard) {
                        String::new()
                    } else {
                        "x".repeat(48)
                    };
                    io.query_replies
                        .insert((0, attempt), info_records(0, &malformed));
                    "FW_INFO contains no valid terminated board target"
                }
                Fault::ChangedMetadata => {
                    let mut changed = info();
                    changed.firmware_size += 1;
                    io.query_replies
                        .insert((0, attempt), info_records(0, &changed));
                    "Target 0 firmware/identity changed since confirmation; query again"
                }
                Fault::ShortWrite => {
                    let error = "Short HID write: 12/65 bytes; not retried";
                    io.query_write_errors.insert((0, attempt), error.into());
                    error
                }
                Fault::WriteFailure => {
                    let error = "HID write: injected query failure";
                    io.query_write_errors.insert((0, attempt), error.into());
                    error
                }
                Fault::ReadFailure => {
                    io.query_read_error_at =
                        Some(Duration::from_millis(if attempt == 1 { 400 } else { 3900 }));
                    "HID read: injected query failure"
                }
            };
            let mut session = Session::new(io, &cancel);
            let outcomes = flash_with(
                &mut session,
                &[target(0)],
                &image(3),
                &info().board_target,
                &mut |_| {},
            )
            .unwrap();
            assert_eq!(
                failure(&outcomes[0].outcome),
                expected,
                "{fault:?}, attempt {attempt}"
            );
            assert_eq!(
                session
                    .io
                    .query_writes
                    .iter()
                    .filter(|(id, _)| *id == 0)
                    .count(),
                attempt,
                "{fault:?}"
            );
            assert!(session.io.begin_writes.is_empty());
            assert!(session.io.abort_writes.is_empty());
            assert_eq!(session.io.reconnects, 0);
        }
    }
}

#[test]
fn page_query_does_not_retry_a_recoverable_partial_response() {
    let cancel = AtomicBool::new(false);
    let mut io = Emulator::new(&[0, 1]);
    io.dropped_query_fragments.insert((0, 1), vec![4]);
    let mut session = Session::new(io, &cancel);
    let result = query_with(
        &mut session,
        &endpoint(),
        &links(&[0, 1]),
        false,
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(
        result.failures,
        [(
            0,
            "Target 0 did not return complete firmware information".into()
        )]
    );
    assert_eq!(result.trackers.len(), 1);
    assert_eq!(result.trackers[0].id, 1);
    assert_eq!(result.trackers[0].info, info());
    assert_eq!(
        session.io.query_writes,
        [
            (0, Duration::from_millis(100)),
            (1, Duration::from_millis(3100)),
        ]
    );
}

#[test]
fn flash_pin_reassignment_during_peer_preflight_invalidates_already_prepared_target() {
    let cancel = AtomicBool::new(false);
    let mut io = Emulator::new(&[0, 1]);
    io.reassign_on_query = Some(0);
    io.reassign_after_queries = 3; // receiver, tracker 0, then tracker 1
    let mut session = Session::new(io, &cancel);
    let outcomes = flash_with(
        &mut session,
        &[target(0), target(1)],
        &image(3),
        &info().board_target,
        &mut |_| {},
    )
    .unwrap();
    assert!(
        outcomes
            .iter()
            .all(|outcome| matches!(outcome.outcome, UpdateOutcome::Failed(_)))
    );
    assert!(session.io.begin_writes.is_empty());
    assert!(session.io.abort_writes.is_empty());
}

#[test]
fn late_fresh_address_or_changed_full_metadata_never_admits_begin() {
    let cancel = AtomicBool::new(false);
    for late_address in [true, false] {
        let mut io = Emulator::new(&[0]);
        if late_address {
            io.address_after = Some((0, Duration::from_millis(3400)));
        } else {
            io.firmware.get_mut(&0).unwrap().firmware_size += 1;
        }
        let mut session = Session::new(io, &cancel);
        let outcomes = flash_with(
            &mut session,
            &[target(0)],
            &image(3),
            &info().board_target,
            &mut |_| {},
        )
        .unwrap();
        assert!(matches!(outcomes[0].outcome, UpdateOutcome::Failed(_)));
        assert!(session.io.begin_writes.is_empty());
    }
}

#[test]
fn flash_session_identity_survives_full_fifo_registration_starvation() {
    for observed_in_session in [true, false] {
        let cancel = AtomicBool::new(false);
        let mut io = Emulator::new(&[0, 1]);
        // With 1, the initial acquisition drain supplies identities before any
        // QUERY. Thereafter every report is full FIFO data, never registration
        // padding, even though fresh CRC-valid FW_INFO arrives for each target.
        io.full_fifo_after_queries = Some(usize::from(observed_in_session));
        let mut session = Session::new(io, &cancel);
        let outcomes = flash_with(
            &mut session,
            &[target(0), target(1)],
            &image(3),
            &info().board_target,
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(outcome.outcome, UpdateOutcome::Completed))
                .count(),
            if observed_in_session { 2 } else { 0 }
        );
        assert_eq!(
            session.io.begin_writes.len(),
            if observed_in_session { 2 } else { 0 }
        );
        assert!(session.io.abort_writes.is_empty());
        if !observed_in_session {
            assert!(outcomes.iter().all(|outcome| {
                failure(&outcome.outcome).contains("RF address could not be revalidated")
            }));
            let queries: Vec<_> = session
                .io
                .query_writes
                .iter()
                .filter(|(id, _)| *id != RECEIVER_ID)
                .collect();
            assert_eq!(queries[1].1 - queries[0].1, Duration::from_secs(3));
        }
    }
}

#[test]
fn flash_retired_slot_replacement_does_not_poison_next_batch() {
    for failed_before_begin in [None, Some(false), Some(true)] {
        let cancel = AtomicBool::new(false);
        let mut io = Emulator::new(&[0, 1, 2]);
        match failed_before_begin {
            None => {}
            Some(false) => io.ready_failure = Some(0),
            Some(true) => io.silent_query = Some(0),
        }
        // Replacement is observed only after the first pair has retired.
        io.reassign_after_complete = Some((0, if failed_before_begin.is_none() { 2 } else { 1 }));
        let mut session = Session::new(io, &cancel);
        let outcomes = flash_with(
            &mut session,
            &[target(0), target(1), target(2)],
            &image(3),
            &info().board_target,
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(
            matches!(outcomes[0].outcome, UpdateOutcome::Completed),
            failed_before_begin.is_none()
        );
        assert!(matches!(outcomes[1].outcome, UpdateOutcome::Completed));
        assert!(matches!(outcomes[2].outcome, UpdateOutcome::Completed));
        assert_eq!(
            session
                .io
                .abort_writes
                .iter()
                .filter(|(id, _)| *id == 0)
                .count(),
            usize::from(failed_before_begin == Some(false))
        );
        assert_eq!(session.io.blocked_begins, 0);
        assert!(session.io.begin_writes.iter().any(|(id, _)| *id == 2));
    }
}

#[test]
fn flash_abandoned_batch_releases_sticky_reassignment_before_next_batch() {
    let cancel = AtomicBool::new(false);
    let mut io = Emulator::new(&[0, 1, 2]);
    io.reassign_on_query = Some(0);
    io.reassign_after_queries = 3;
    let mut session = Session::new(io, &cancel);
    let outcomes = flash_with(
        &mut session,
        &[target(0), target(1), target(2)],
        &image(3),
        &info().board_target,
        &mut |_| {},
    )
    .unwrap();
    assert!(
        outcomes[..2]
            .iter()
            .all(|outcome| failure(&outcome.outcome).contains("reassigned"))
    );
    assert!(matches!(outcomes[2].outcome, UpdateOutcome::Completed));
    assert_eq!(
        session
            .io
            .begin_writes
            .iter()
            .map(|(id, _)| *id)
            .collect::<Vec<_>>(),
        [2]
    );
    assert!(session.io.abort_writes.is_empty());
}

#[test]
fn flash_never_started_batch_releases_pins_before_next_batch() {
    let cancel = AtomicBool::new(false);
    let mut io = Emulator::new(&[0, 1, 2]);
    io.silent_query = Some(0);
    io.corrupt_query = Some(1);
    io.reassign_on_query = Some(0);
    io.reassign_after_queries = 4;
    let mut session = Session::new(io, &cancel);
    let outcomes = flash_with(
        &mut session,
        &[target(0), target(1), target(2)],
        &image(3),
        &info().board_target,
        &mut |_| {},
    )
    .unwrap();
    assert!(
        outcomes[..2]
            .iter()
            .all(|outcome| matches!(outcome.outcome, UpdateOutcome::Failed(_)))
    );
    assert!(matches!(outcomes[2].outcome, UpdateOutcome::Completed));
    assert_eq!(
        session
            .io
            .begin_writes
            .iter()
            .map(|(id, _)| *id)
            .collect::<Vec<_>>(),
        [2]
    );
    assert!(session.io.abort_writes.is_empty());
}

#[test]
fn tracker_batches_never_mix_differently_clipped_flash_bases() {
    let cancel = AtomicBool::new(false);
    let mut selected = [target(0), target(1)];
    selected[1].info.flash_base = 0x2000;
    let mut io = Emulator::new(&[0, 1]);
    io.firmware.insert(1, selected[1].info.clone());
    let mut firmware = image(100);
    firmware.data[4096..4100].copy_from_slice(&0x20010000u32.to_le_bytes());
    firmware.data[4100..4104].copy_from_slice(&0x2009u32.to_le_bytes());
    firmware.crc32 = crc32fast::hash(&firmware.data);
    let mut session = Session::new(io, &cancel);
    let outcomes = flash_with(
        &mut session,
        &selected,
        &firmware,
        &info().board_target,
        &mut |_| {},
    )
    .unwrap();
    assert!(
        outcomes
            .iter()
            .all(|outcome| matches!(outcome.outcome, UpdateOutcome::Completed))
    );
    assert_eq!(session.io.max_parallel, 1);
    let sizes: Vec<_> = session
        .io
        .writes
        .iter()
        .filter(|r| r[0] == BEGIN)
        .map(|r| u32::from_le_bytes(r[2..6].try_into().unwrap()) as usize)
        .collect();
    assert_eq!(sizes, [firmware.data.len(), firmware.data.len() - 4096]);
    assert!(session.io.abort_writes.is_empty());
}

#[test]
fn native_abort_deadline_is_scoped_to_exact_interface_identity_and_expires() {
    let mut receiver = endpoint();
    receiver.key = "test-abort-deadline".into();
    receiver.path = b"test-abort-deadline".to_vec();
    let now = std::time::Instant::now();
    super::ABORT_DEADLINES
        .lock()
        .push((receiver.clone(), now + Duration::from_secs(10)));
    let mut io = super::NativeHid {
        device: None,
        endpoint: receiver.clone(),
        started: now,
    };
    assert!(io.abort_pending_for() > Duration::from_secs(9));
    io.endpoint.serial_number = Some("different-serial".into());
    assert_eq!(io.abort_pending_for(), Duration::ZERO);
    io.endpoint = receiver.clone();
    io.endpoint.path.push(0);
    assert_eq!(io.abort_pending_for(), Duration::ZERO);
    {
        let mut deadlines = super::ABORT_DEADLINES.lock();
        let (_, until) = deadlines
            .iter_mut()
            .find(|(entry, _)| entry.key == receiver.key)
            .unwrap();
        *until = now;
    }
    io.endpoint = receiver.clone();
    assert_eq!(io.abort_pending_for(), Duration::ZERO);
    assert!(
        !super::ABORT_DEADLINES
            .lock()
            .iter()
            .any(|(entry, _)| entry.key == receiver.key)
    );
}

#[test]
fn flash_unbound_identity_transition_cannot_be_hidden_by_restored_address() {
    for (changed_slot, during_query) in [(0, 1), (1, 2)] {
        let cancel = AtomicBool::new(false);
        let mut io = Emulator::new(&[0, 1, 2]);
        // The initial drain observes A. During receiver QUERY, or slot 0's
        // preflight for its still-unbound peer, the FIFO supplies B then A.
        io.reassign_on_query = Some(changed_slot);
        io.reassign_after_queries = during_query;
        io.full_fifo_after_queries = Some(1);
        let mut session = Session::new(io, &cancel);
        let outcomes = flash_with(
            &mut session,
            &[target(0), target(1), target(2)],
            &image(3),
            &info().board_target,
            &mut |_| {},
        )
        .unwrap();
        let failed = if changed_slot == 0 { 1 } else { 2 };
        // With no prepared peer yet, abandoning stale slot 0 cannot poison
        // unqueried slot 1. If slot 0 is already prepared when slot 1's history
        // fails binding, abandon both pending proofs before proceeding.
        assert!(
            outcomes[..failed]
                .iter()
                .all(|outcome| failure(&outcome.outcome).contains("reassigned"))
        );
        assert!(
            outcomes[failed..]
                .iter()
                .all(|outcome| matches!(outcome.outcome, UpdateOutcome::Completed))
        );
        assert_eq!(
            session
                .io
                .begin_writes
                .iter()
                .map(|(id, _)| *id)
                .collect::<Vec<_>>(),
            if changed_slot == 0 {
                vec![1, 2]
            } else {
                vec![2]
            }
        );
        assert!(session.io.abort_writes.is_empty());
    }
}

#[test]
fn flash_admission_identity_proof_and_preparation_substages_are_preserved() {
    use crate::update::OtaPreparation;

    let cancel = AtomicBool::new(false);
    let mut io = Emulator::new(&[0]);
    io.delay_addresses = true;
    io.full_fifo_after_queries = Some(2);
    io.tombstones.insert(7, Duration::from_secs(10));
    let mut session = Session::new(io, &cancel);
    let mut events = Vec::new();
    let outcomes = flash_with(
        &mut session,
        &[target(0)],
        &image(3),
        &info().board_target,
        &mut |event| events.push(event),
    )
    .unwrap();
    assert!(matches!(outcomes[0].outcome, UpdateOutcome::Completed));
    let stages: Vec<_> = events
        .iter()
        .filter_map(|event| event.ota[0].preparation)
        .collect();
    let recovery = stages
        .iter()
        .position(|stage| *stage == OtaPreparation::ReceiverRecovery)
        .unwrap();
    let query = stages
        .iter()
        .position(|stage| *stage == OtaPreparation::QueryingDevice)
        .unwrap();
    let starting = stages
        .iter()
        .position(|stage| *stage == OtaPreparation::StartingUpdate)
        .unwrap();
    assert!(recovery < query && query < starting);
    assert!(events.last().unwrap().ota[0].preparation.is_none());
    assert!(
        session
            .io
            .query_writes
            .iter()
            .filter(|(id, _)| *id == 0)
            .all(|(_, at)| *at >= Duration::from_secs(10))
    );
    assert!(session.io.abort_writes.is_empty());
}

#[test]
fn all_image_groups_prefetch_before_first_begin_and_receiver_updates_last() {
    let cancel = AtomicBool::new(false);
    let mut session = Session::new(Emulator::new(&[0, 1, 2, 3, 4]), &cancel);
    let first = [target(0), target(1), target(2)];
    let second = [target(3), target(4)];
    let receiver = [target(RECEIVER_ID)];
    let first_image = image(3);
    let second_image = image(5);
    let receiver_image = image(7);
    let board = info().board_target;
    let groups = [
        FlashGroup {
            targets: &receiver,
            image: &receiver_image,
            board: &board,
        },
        FlashGroup {
            targets: &first,
            image: &first_image,
            board: &board,
        },
        FlashGroup {
            targets: &second,
            image: &second_image,
            board: &board,
        },
    ];
    let outcomes = flash_groups_with(
        &mut session,
        &groups.iter().collect::<Vec<_>>(),
        &mut |_| {},
    )
    .unwrap();
    assert!(
        outcomes
            .iter()
            .all(|outcome| matches!(outcome.outcome, UpdateOutcome::Completed))
    );
    let first_begin = session
        .io
        .writes
        .iter()
        .position(|r| r[0] == BEGIN)
        .unwrap();
    let tracker_queries: Vec<_> = session
        .io
        .writes
        .iter()
        .enumerate()
        .filter(|(_, r)| r[0] == QUERY && r[1] != RECEIVER_ID)
        .collect();
    assert_eq!(tracker_queries.len(), 5);
    assert!(
        tracker_queries
            .iter()
            .all(|(position, _)| *position < first_begin)
    );
    assert_eq!(
        session
            .io
            .begin_writes
            .iter()
            .map(|(id, _)| *id)
            .collect::<Vec<_>>(),
        [0, 1, 2, 3, 4, RECEIVER_ID]
    );
    assert!(session.io.abort_writes.is_empty());
    let sizes: Vec<_> = session
        .io
        .writes
        .iter()
        .filter(|r| r[0] == BEGIN)
        .map(|r| u32::from_le_bytes(r[2..6].try_into().unwrap()) as usize)
        .collect();
    assert_eq!(
        sizes,
        [
            first_image.data.len(),
            first_image.data.len(),
            first_image.data.len(),
            second_image.data.len(),
            second_image.data.len(),
            receiver_image.data.len()
        ]
    );
}

#[test]
fn later_image_preflight_cannot_hide_change_to_pending_prepared_identity() {
    let cancel = AtomicBool::new(false);
    let mut io = Emulator::new(&[0, 1, 2]);
    io.reassign_on_query = Some(0);
    io.reassign_after_queries = 4;
    let mut session = Session::new(io, &cancel);
    let first = [target(0), target(1)];
    let second = [target(2)];
    let firmware = image(3);
    let other = image(4);
    let board = info().board_target;
    let groups = [
        FlashGroup {
            targets: &first,
            image: &firmware,
            board: &board,
        },
        FlashGroup {
            targets: &second,
            image: &other,
            board: &board,
        },
    ];
    let outcomes = flash_groups_with(
        &mut session,
        &groups.iter().collect::<Vec<_>>(),
        &mut |_| {},
    )
    .unwrap();
    assert!(
        outcomes
            .iter()
            .all(|outcome| failure(&outcome.outcome).contains("reassigned"))
    );
    assert!(session.io.begin_writes.is_empty());
    assert!(session.io.abort_writes.is_empty());
}

#[test]
fn prepared_future_slot_change_during_transfer_never_receives_image_data() {
    let cancel = AtomicBool::new(false);
    let mut io = Emulator::new(&[0, 1, 2]);
    io.reassign_after_complete = Some((2, 2));
    let mut session = Session::new(io, &cancel);
    let outcomes = flash_with(
        &mut session,
        &[target(0), target(1), target(2)],
        &image(3),
        &info().board_target,
        &mut |_| {},
    )
    .unwrap();
    assert!(
        outcomes[..2]
            .iter()
            .all(|outcome| matches!(outcome.outcome, UpdateOutcome::Completed))
    );
    assert!(failure(&outcomes[2].outcome).contains("reassigned"));
    // A queued registration can first be observed in the BEGIN reply frame;
    // no data/activation may follow that contradiction.
    assert!(
        !session
            .io
            .writes
            .iter()
            .any(|r| r[0] == ACTIVATE && r[1] == 2)
    );
    let begin = session
        .io
        .writes
        .iter()
        .position(|r| r[0] == BEGIN && r[1] == 2)
        .unwrap();
    assert!(!session.io.writes[begin + 1..].iter().any(|r| r[0] == DATA));
    assert_eq!(
        session
            .io
            .abort_writes
            .iter()
            .map(|(id, _)| *id)
            .collect::<Vec<_>>(),
        [2]
    );
}

#[test]
fn modeled_hot_fifo_old_boundaries_vs_prefetched_operation() {
    let cancel = AtomicBool::new(false);
    let firmware = image(3);
    let board = info().board_target;
    let mut old_io = Emulator::new(&[0, 1, 2, 3, 4, 5]);
    old_io.hot_after_complete = true;
    let mut old = Session::new(old_io, &cancel);
    // Exercise the actual host preflight/transfer at the old per-pair boundary,
    // not a replacement query implementation or a physical firmware claim.
    let mut old_outcomes = Vec::new();
    for pair in [
        [target(0), target(1)],
        [target(2), target(3)],
        [target(4), target(5)],
    ] {
        old_outcomes.extend(flash_with(&mut old, &pair, &firmware, &board, &mut |_| {}).unwrap());
    }
    assert_eq!(
        old_outcomes
            .iter()
            .filter(|outcome| matches!(outcome.outcome, UpdateOutcome::Completed))
            .count(),
        2
    );
    assert!(old_outcomes[2..].iter().all(|outcome| {
        failure(&outcome.outcome).contains("did not return complete firmware information")
    }));
    assert_eq!(old.io.dropped_info_chunks, 32);
    assert!(old.io.abort_writes.is_empty());

    let mut new_io = Emulator::new(&[0, 1, 2, 3, 4, 5]);
    new_io.hot_after_complete = true;
    let mut new = Session::new(new_io, &cancel);
    let selected: Vec<_> = (0..6).map(target).collect();
    let outcomes = flash_with(&mut new, &selected, &firmware, &board, &mut |_| {}).unwrap();
    assert!(
        outcomes
            .iter()
            .all(|outcome| matches!(outcome.outcome, UpdateOutcome::Completed))
    );
    assert_eq!(new.io.dropped_info_chunks, 0);
    assert!(new.io.abort_writes.is_empty());
    println!(
        "modeled actual-host FIFO smoke: old success=2 preparation_failure=4 dropped_chunks=32; prefetched success=6 preparation_failure=0 dropped_chunks=0; successful-target ABORT=0"
    );
}

#[test]
fn discovery_tracker_queries_obey_known_abort_but_receiver_query_does_not_wait() {
    let cancel = AtomicBool::new(false);
    let mut io = Emulator::new(&[0, 1]);
    io.tombstones.insert(7, Duration::from_secs(10));
    let mut session = Session::new(io, &cancel);
    let result = query_with(
        &mut session,
        &endpoint(),
        &links(&[0, 1]),
        true,
        &mut |_| {},
    )
    .unwrap();
    assert!(result.failures.is_empty());
    assert_eq!(session.io.blocked_queries, 0);
    assert_eq!(
        session.io.query_writes[0],
        (RECEIVER_ID, Duration::from_millis(100))
    );
    assert!(
        session.io.query_writes[1..]
            .iter()
            .all(|(_, at)| *at >= Duration::from_secs(10))
    );

    let mut io = Emulator::new(&[]);
    io.tombstones.insert(7, Duration::from_secs(10));
    let mut session = Session::new(io, &cancel);
    assert!(
        query_with(&mut session, &endpoint(), &[], true, &mut |_| {})
            .unwrap()
            .receiver
            .is_some()
    );
    assert!(session.io.time < Duration::from_secs(1));
}

#[test]
fn cancellation_while_discovery_waits_for_abort_never_sends_tracker_query() {
    let cancel = Arc::new(AtomicBool::new(false));
    let mut io = Emulator::new(&[0]);
    io.tombstones.insert(7, Duration::from_secs(10));
    io.cancel_on_read_at = Some((Duration::from_millis(500), cancel.clone()));
    let mut session = Session::new(io, &cancel);
    assert!(
        query_with(&mut session, &endpoint(), &links(&[0]), false, &mut |_| {})
            .unwrap_err()
            .contains("cancelled")
    );
    assert!(session.io.query_writes.is_empty());
    assert_eq!(session.io.time, Duration::from_millis(500));
}

#[test]
fn full_info_and_trailer_received_in_third_second_complete_without_fixed_wait() {
    let cancel = AtomicBool::new(false);
    let mut io = Emulator::new(&[0]);
    io.delayed_info = Some((0, Duration::from_millis(2400)));
    let mut session = Session::new(io, &cancel);
    let result = query_with(&mut session, &endpoint(), &links(&[0]), false, &mut |_| {}).unwrap();
    assert!(result.failures.is_empty());
    assert_eq!(result.trackers[0].info, info());
    let elapsed = session.io.time - session.io.query_writes[0].1;
    assert!(elapsed >= Duration::from_millis(2400));
    assert!(elapsed < Duration::from_secs(3));
    assert_eq!(session.io.query_writes.len(), 1);
}

#[test]
fn timed_out_incomplete_info_is_not_rescued_by_later_target_or_reused_for_flash() {
    let cancel = AtomicBool::new(false);
    let mut io = Emulator::new(&[0, 1]);
    io.delayed_info = Some((0, Duration::from_millis(3200)));
    let mut session = Session::new(io, &cancel);
    let result = query_with(
        &mut session,
        &endpoint(),
        &links(&[0, 1]),
        false,
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(result.trackers.len(), 1);
    assert_eq!(result.trackers[0].id, 1);
    assert_eq!(result.failures.len(), 1);
    assert_eq!(result.failures[0].0, 0);
    assert!(result.failures[0].1.contains("did not return complete"));

    let mut io = Emulator::new(&[0, 1, 2]);
    io.incomplete_info = Some((0, 4));
    let mut session = Session::new(io, &cancel);
    let outcomes = flash_with(
        &mut session,
        &[target(0), target(1), target(2)],
        &image(3),
        &info().board_target,
        &mut |_| {},
    )
    .unwrap();
    assert!(failure(&outcomes[0].outcome).contains("did not return complete"));
    assert!(
        outcomes[1..]
            .iter()
            .all(|outcome| matches!(outcome.outcome, UpdateOutcome::Completed))
    );
    assert_eq!(
        session
            .io
            .query_writes
            .iter()
            .filter(|(id, _)| *id == 0)
            .count(),
        2
    );
    assert!(!session.io.begin_writes.iter().any(|(id, _)| *id == 0));
    assert!(session.io.abort_writes.is_empty());
}

#[test]
fn duplicate_target_across_images_is_rejected_before_any_query_or_begin() {
    let cancel = AtomicBool::new(false);
    let mut session = Session::new(Emulator::new(&[0]), &cancel);
    let selected = [target(0)];
    let firmware = image(3);
    let board = info().board_target;
    let groups = [
        FlashGroup {
            targets: &selected,
            image: &firmware,
            board: &board,
        },
        FlashGroup {
            targets: &selected,
            image: &firmware,
            board: &board,
        },
    ];
    assert!(
        flash_groups_with(
            &mut session,
            &groups.iter().collect::<Vec<_>>(),
            &mut |_| {}
        )
        .unwrap_err()
        .contains("Duplicate")
    );
    assert!(session.io.writes.is_empty());
}

#[test]
fn every_receiver_tracker_plan_finishes_before_any_receiver_self_update() {
    let cancel = AtomicBool::new(false);
    let first = [target(0), target(1)];
    let other_image_targets = [target(2)];
    let receiver_a = [target(RECEIVER_ID)];
    let mut tracker_b = target(3);
    tracker_b.receiver.key = "receiver-b".into();
    tracker_b.receiver.path = b"receiver-b".to_vec();
    tracker_b.receiver.serial_number = Some("serial-b".into());
    let mut self_b = target(RECEIVER_ID);
    self_b.receiver = tracker_b.receiver.clone();
    let second_receiver = [tracker_b];
    let receiver_b = [self_b];
    let firmware = image(3);
    let other_image = image(4);
    let board = info().board_target;
    // Deliberately interleave self-update plans with tracker image groups.
    let groups = [
        FlashGroup {
            targets: &receiver_a,
            image: &firmware,
            board: &board,
        },
        FlashGroup {
            targets: &first,
            image: &firmware,
            board: &board,
        },
        FlashGroup {
            targets: &second_receiver,
            image: &firmware,
            board: &board,
        },
        FlashGroup {
            targets: &other_image_targets,
            image: &other_image,
            board: &board,
        },
        FlashGroup {
            targets: &receiver_b,
            image: &firmware,
            board: &board,
        },
    ];
    let mut remaining: Vec<_> = groups.iter().collect();
    let mut commands = Vec::new();
    let mut session_count = 0;
    while !remaining.is_empty() {
        let plan = take_receiver_groups(&mut remaining);
        let receiver = &plan[0].targets[0].receiver;
        let mut session = Session::new(Emulator::new(&[0, 1, 2, 3]), &cancel);
        let outcomes = flash_groups_with(&mut session, &plan, &mut |_| {}).unwrap();
        assert!(
            outcomes
                .iter()
                .all(|outcome| matches!(outcome.outcome, UpdateOutcome::Completed))
        );
        commands.extend(
            session
                .io
                .writes
                .iter()
                .map(|report| (receiver.key.clone(), report[0], report[1])),
        );
        session_count += 1;
    }
    assert_eq!(
        session_count, 4,
        "all tracker images per receiver share a session; self-updates reopen last"
    );
    let begins: Vec<_> = commands
        .iter()
        .filter(|(_, opcode, _)| *opcode == BEGIN)
        .map(|(receiver, _, id)| (receiver.as_str(), *id))
        .collect();
    assert_eq!(
        begins,
        [
            ("receiver-path", 0),
            ("receiver-path", 1),
            ("receiver-path", 2),
            ("receiver-b", 3),
            ("receiver-path", RECEIVER_ID),
            ("receiver-b", RECEIVER_ID),
        ]
    );
    let first_receiver_begin = commands
        .iter()
        .position(|(_, opcode, id)| *opcode == BEGIN && *id == RECEIVER_ID)
        .unwrap();
    assert!(
        commands
            .iter()
            .enumerate()
            .filter(|(_, (_, opcode, id))| *opcode == ACTIVATE && *id != RECEIVER_ID)
            .all(|(position, _)| position < first_receiver_begin)
    );
    let first_tracker_begin = commands
        .iter()
        .position(|(_, opcode, id)| *opcode == BEGIN && *id != RECEIVER_ID)
        .unwrap();
    assert!(
        commands
            .iter()
            .enumerate()
            .filter(|(_, (receiver, opcode, id))| receiver == "receiver-path"
                && *opcode == QUERY
                && *id != RECEIVER_ID)
            .all(|(position, _)| position < first_tracker_begin)
    );
}

#[test]
fn receiver_activation_outcomes_preserve_evidence_without_post_attempt_abort() {
    #[derive(Clone, Copy, Debug)]
    enum Case {
        Complete,
        Accepted,
        Silent,
        ShortWrite,
        ErroredWrite,
        ShortWriteAccepted,
        ErroredWriteComplete,
        LateComplete,
        LateFailure,
        NoQueryReply,
        Cancelled,
        WrongInfo,
        WrongNativeIdentity,
        MalformedAccepted,
        MalformedComplete,
        WrongStatusIdentity,
    }
    for case in [
        Case::Complete,
        Case::Accepted,
        Case::Silent,
        Case::ShortWrite,
        Case::ErroredWrite,
        Case::ShortWriteAccepted,
        Case::ErroredWriteComplete,
        Case::LateComplete,
        Case::LateFailure,
        Case::NoQueryReply,
        Case::Cancelled,
        Case::WrongInfo,
        Case::WrongNativeIdentity,
        Case::MalformedAccepted,
        Case::MalformedComplete,
        Case::WrongStatusIdentity,
    ] {
        let cancel = Arc::new(AtomicBool::new(false));
        let firmware = image(3);
        let mut io = Emulator::new(&[]);
        io.silent_activate = true;
        match case {
            Case::Complete => io.silent_activate = false,
            Case::Accepted => {
                io.activation_status = Some(status(RECEIVER_ID, 5, 3, firmware.data.len()))
            }
            Case::Silent => {}
            Case::ShortWrite | Case::ShortWriteAccepted => {
                io.activation_write_error =
                    Some("Short HID write: 12/65 bytes; not retried".into());
                if matches!(case, Case::ShortWriteAccepted) {
                    io.activation_status = Some(status(RECEIVER_ID, 5, 3, firmware.data.len()));
                }
            }
            Case::ErroredWrite | Case::ErroredWriteComplete => {
                io.activation_write_error = Some("HID write: device disconnected".into());
                if matches!(case, Case::ErroredWriteComplete) {
                    io.activation_status = Some(status(RECEIVER_ID, 6, 3, firmware.data.len()));
                }
            }
            Case::LateComplete | Case::LateFailure => {
                io.disconnect_after_activate = true;
                io.activation_query_status = Some(status(
                    RECEIVER_ID,
                    if matches!(case, Case::LateComplete) {
                        6
                    } else {
                        0x12
                    },
                    3,
                    firmware.data.len(),
                ));
            }
            Case::NoQueryReply => io.silent_activation_query = true,
            Case::Cancelled => io.cancel_on_activate = Some(cancel.clone()),
            Case::WrongInfo => io.wrong_activation_info = true,
            Case::WrongNativeIdentity => {
                io.activation_reconnect_error = Some(super::ReceiverOpenError::Identity(
                    "Receiver identity changed".into(),
                ))
            }
            Case::MalformedAccepted | Case::MalformedComplete => {
                io.activation_status = Some(status(
                    RECEIVER_ID,
                    if matches!(case, Case::MalformedAccepted) {
                        5
                    } else {
                        6
                    },
                    2,
                    firmware.data.len() - 1,
                ));
            }
            Case::WrongStatusIdentity => {
                io.activation_status = Some(status(0, 6, 3, firmware.data.len()))
            }
        }
        let mut session = Session::new(io, &cancel);
        let mut progress = Vec::new();
        let outcomes = flash_with(
            &mut session,
            &[target(RECEIVER_ID)],
            &firmware,
            &info().board_target,
            &mut |event| progress.push(event),
        )
        .unwrap();
        let outcome = &outcomes[0].outcome;
        match case {
            Case::Complete | Case::LateComplete | Case::ErroredWriteComplete => {
                assert_eq!(outcome, &UpdateOutcome::Completed, "{case:?}");
            }
            Case::LateFailure
            | Case::WrongInfo
            | Case::WrongNativeIdentity
            | Case::MalformedAccepted
            | Case::MalformedComplete
            | Case::WrongStatusIdentity => {
                let error = failure(outcome);
                assert!(!error.is_empty(), "{case:?}");
                if matches!(case, Case::LateFailure) {
                    assert!(error.contains("flash write failed"));
                }
            }
            _ => {
                let UpdateOutcome::ActivationUnconfirmed(report) = outcome else {
                    panic!("{case:?}: expected uncertainty, got {outcome:?}");
                };
                match case {
                    Case::Accepted | Case::ShortWriteAccepted => {
                        assert_eq!(report.submission, ActivationSubmission::Accepted);
                    }
                    Case::ShortWrite => assert_eq!(
                        report.submission,
                        ActivationSubmission::DeliveryUncertain(
                            "Short HID write: 12/65 bytes; not retried".into()
                        ),
                    ),
                    Case::ErroredWrite => assert_eq!(
                        report.submission,
                        ActivationSubmission::DeliveryUncertain(
                            "HID write: device disconnected".into()
                        ),
                    ),
                    _ => assert_eq!(report.submission, ActivationSubmission::Sent),
                }
                if matches!(case, Case::NoQueryReply | Case::Cancelled) {
                    let ReceiverAvailability::Unconfirmed(reason) = &report.availability else {
                        panic!("{case:?}: missing availability evidence");
                    };
                    assert!(reason.contains(if matches!(case, Case::Cancelled) {
                        "cancelled"
                    } else {
                        "did not return complete firmware information"
                    }));
                } else {
                    assert_eq!(report.availability, ReceiverAvailability::Responding);
                }
                assert_eq!(
                    progress.last().unwrap().ota[0].phase,
                    crate::update::OtaPhase::ActivationUnconfirmed
                );
                assert_eq!(progress.last().unwrap().completed, 0);
            }
        }
        assert!(session.io.abort_writes.is_empty(), "{case:?}");
        assert_eq!(
            session
                .io
                .writes
                .iter()
                .filter(|r| r[0] == ACTIVATE)
                .count(),
            1,
            "{case:?}"
        );
        assert!(session.unfinished.iter().all(|unfinished| !unfinished));
        assert!(
            session.io.time - session.io.activation_attempt_at.unwrap() <= Duration::from_secs(30),
            "{case:?}"
        );
        let verify = session
            .io
            .writes
            .iter()
            .position(|r| r[0] == VERIFY)
            .unwrap();
        let transferred: Vec<_> = session.io.writes[..verify]
            .iter()
            .filter(|r| r[0] == DATA)
            .flat_map(|r| r[4..].iter().copied())
            .take(firmware.data.len())
            .collect();
        assert_eq!(transferred, firmware.data, "{case:?}");
        assert_eq!(crc32fast::hash(&transferred), firmware.crc32, "{case:?}");
    }
}

#[derive(Debug)]
struct OpenedReceiverHandle {
    drops: std::rc::Rc<std::cell::Cell<usize>>,
    writes: std::rc::Rc<std::cell::Cell<usize>>,
}

impl OpenedReceiverHandle {
    fn write(&self) {
        self.writes.set(self.writes.get() + 1);
    }
}

impl Drop for OpenedReceiverHandle {
    fn drop(&mut self) {
        self.drops.set(self.drops.get() + 1);
    }
}

#[test]
fn native_open_rejects_unverified_handle_identity_and_drops_without_writes() {
    use std::{cell::Cell, rc::Rc};

    for (expected_serial, opened_serial) in [
        (Some("serial-1"), Ok(Some("replacement"))),
        (Some("serial-1"), Ok(Some("SERIAL-1"))),
        (Some("serial-1"), Ok(None)),
        (Some("serial-1"), Ok(Some(" \t"))),
        (Some("serial-1"), Err("serial query failed")),
        (None, Ok(Some("unexpected-serial"))),
        (None, Err("serial query failed")),
    ] {
        let receiver = ReceiverEndpoint {
            serial_number: expected_serial.map(str::to_owned),
            ..endpoint()
        };
        let opens = Cell::new(0);
        let queries = Cell::new(0);
        let drops = Rc::new(Cell::new(0));
        let writes = Rc::new(Cell::new(0));
        let result = super::open_receiver_handle(
            &receiver,
            || {
                opens.set(opens.get() + 1);
                Ok(OpenedReceiverHandle {
                    drops: Rc::clone(&drops),
                    writes: Rc::clone(&writes),
                })
            },
            |_| {
                queries.set(queries.get() + 1);
                opened_serial
                    .map(|serial| serial.map(str::to_owned))
                    .map_err(str::to_owned)
            },
        );
        assert!(matches!(result, Err(super::ReceiverOpenError::Identity(_))));
        assert_eq!(opens.get(), 1);
        assert_eq!(queries.get(), 1);
        assert_eq!(drops.get(), 1, "unverified handles must close immediately");
        assert_eq!(
            writes.get(),
            0,
            "identity checks must precede all HID writes"
        );
    }
}

#[test]
fn native_open_accepts_matching_normalized_and_serialless_handle_identity() {
    use std::{cell::Cell, rc::Rc};

    for (expected_serial, opened_serial) in [
        (Some("serial-1"), Some("serial-1")),
        (Some("serial-1"), Some(" \tserial-1\r\n")),
        (None, None),
        (None, Some(" \t\r\n")),
    ] {
        let receiver = ReceiverEndpoint {
            serial_number: expected_serial.map(str::to_owned),
            ..endpoint()
        };
        let drops = Rc::new(Cell::new(0));
        let writes = Rc::new(Cell::new(0));
        let handle = super::open_receiver_handle(
            &receiver,
            || {
                Ok(OpenedReceiverHandle {
                    drops: Rc::clone(&drops),
                    writes: Rc::clone(&writes),
                })
            },
            |_| Ok(opened_serial.map(str::to_owned)),
        )
        .unwrap();
        assert_eq!(
            drops.get(),
            0,
            "verified handle remains owned by its caller"
        );
        assert_eq!(writes.get(), 0, "opening must not issue HID commands");
        handle.write();
        assert_eq!(writes.get(), 1);
        drop(handle);
        assert_eq!(drops.get(), 1);
    }
}

#[test]
fn native_reopen_accepts_first_fresh_exact_identity_without_a_missed_gap() {
    use std::cell::Cell;
    let cancel = AtomicBool::new(false);
    let now = Cell::new(Duration::from_secs(3));
    let opens = Cell::new(0);
    let waits = Cell::new(0);
    let receiver = endpoint();
    let opened = super::reopen_receiver(
        &cancel,
        Duration::from_secs(30),
        || {
            opens.set(opens.get() + 1);
            super::validate_receiver_interface(
                &receiver,
                &receiver.path,
                0x1209,
                0x7690,
                1,
                Some("serial-1"),
            )?;
            Ok(17)
        },
        || now.get(),
        |_| waits.set(waits.get() + 1),
    )
    .unwrap();
    assert_eq!(opened, 17);
    assert_eq!(opens.get(), 1);
    assert_eq!(waits.get(), 0);
    assert_eq!(now.get(), Duration::from_secs(3));
}

#[test]
fn native_reopen_rejects_every_identity_contradiction_without_retry() {
    use std::cell::Cell;
    let receiver = endpoint();
    for (path, vid, pid, usage, serial) in [
        (b"different".as_slice(), 0x1209, 0x7690, 1, Some("serial-1")),
        (
            receiver.path.as_slice(),
            0x9999,
            0x7690,
            1,
            Some("serial-1"),
        ),
        (
            receiver.path.as_slice(),
            0x1209,
            0x9999,
            1,
            Some("serial-1"),
        ),
        (
            receiver.path.as_slice(),
            0x1209,
            0x7690,
            0xff00,
            Some("serial-1"),
        ),
        (receiver.path.as_slice(), 0x1209, 0x7690, 1, Some("other")),
        (receiver.path.as_slice(), 0x1209, 0x7690, 1, None),
    ] {
        let opens = Cell::new(0);
        let result = super::reopen_receiver(
            &AtomicBool::new(false),
            Duration::from_secs(30),
            || {
                opens.set(opens.get() + 1);
                super::validate_receiver_interface(&receiver, path, vid, pid, usage, serial)
            },
            || Duration::ZERO,
            |_| panic!("Identity contradictions must not be retried"),
        );
        assert!(matches!(result, Err(super::ReceiverOpenError::Identity(_))));
        assert_eq!(opens.get(), 1);
    }
}

#[test]
fn native_reopen_retries_only_availability_with_one_absolute_budget_and_cancellation() {
    use std::cell::Cell;
    for succeeds in [false, true] {
        let now = Cell::new(Duration::from_secs(29));
        let opens = Cell::new(0);
        let result = super::reopen_receiver(
            &AtomicBool::new(false),
            Duration::from_secs(30),
            || {
                opens.set(opens.get() + 1);
                if succeeds && opens.get() == 3 {
                    Ok(23)
                } else {
                    Err(super::ReceiverOpenError::Unavailable("not present".into()))
                }
            },
            || now.get(),
            |delay| now.set(now.get() + delay),
        );
        if succeeds {
            assert_eq!(result.unwrap(), 23);
            assert_eq!(now.get(), Duration::from_millis(29_200));
        } else {
            assert!(matches!(
                result,
                Err(super::ReceiverOpenError::Unavailable(_))
            ));
            assert_eq!(now.get(), Duration::from_secs(30));
            assert_eq!(opens.get(), 10);
        }
    }
    let cancel = AtomicBool::new(false);
    let now = Cell::new(Duration::ZERO);
    let opens = Cell::new(0);
    let result = super::reopen_receiver(
        &cancel,
        Duration::from_secs(30),
        || {
            opens.set(opens.get() + 1);
            Err::<(), _>(super::ReceiverOpenError::Unavailable("not present".into()))
        },
        || now.get(),
        |delay| {
            now.set(now.get() + delay);
            cancel.store(true, Ordering::Relaxed);
        },
    );
    assert!(
        matches!(result, Err(super::ReceiverOpenError::Unavailable(error)) if error.contains("cancelled"))
    );
    assert_eq!(opens.get(), 1);
    assert_eq!(now.get(), Duration::from_millis(100));
}

#[test]
fn receiver_activation_query_retries_share_the_original_availability_budget() {
    let cancel = AtomicBool::new(false);
    let mut io = Emulator::new(&[]);
    io.silent_activate = true;
    io.activation_query_write_error = true;
    io.activation_reconnect_delay = Duration::from_millis(200);
    let mut session = Session::new(io, &cancel);
    let outcomes = flash_with(
        &mut session,
        &[target(RECEIVER_ID)],
        &image(3),
        &info().board_target,
        &mut |_| {},
    )
    .unwrap();
    let UpdateOutcome::ActivationUnconfirmed(report) = &outcomes[0].outcome else {
        panic!("Transient availability errors must retain uncertainty");
    };
    assert_eq!(report.submission, ActivationSubmission::Sent);
    assert!(
        matches!(&report.availability, ReceiverAvailability::Unconfirmed(error)
        if error.contains("temporarily unavailable"))
    );
    assert_eq!(
        session.io.time - session.io.activation_attempt_at.unwrap(),
        Duration::from_secs(30)
    );
    assert!(session.io.reconnects > 1);
    assert!(session.io.abort_writes.is_empty());
    assert_eq!(
        session
            .io
            .writes
            .iter()
            .filter(|r| r[0] == ACTIVATE)
            .count(),
        1
    );
}
