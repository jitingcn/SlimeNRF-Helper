use super::{
    CHUNK_BYTES, IdentitySnapshot, MAX_IDENTITY_LINE, QUEUE_CAPACITY, SessionView, Worker,
};
use crate::domain::{DeviceIdentity, DeviceRole};
use crate::rssi_scan::{ScanDecoder, ScanSnapshot};
use parking_lot::Mutex;
use slimenrf_terminal_core::TerminalBuffer;
use std::sync::Arc;

#[derive(Clone)]
struct DisplayChunk {
    bytes: [u8; CHUNK_BYTES],
    len: usize,
    dropped_before: u64,
}

/// Reused byte blocks, not read-call slots: even one-byte reads can use the full
/// bounded backlog. The serial owner only holds this lock while copying bytes;
/// the consumer removes one block before doing any terminal parsing.
pub(super) struct DisplayQueue {
    chunks: Vec<DisplayChunk>,
    head: usize,
    len: usize,
    dropped: u64,
}

impl DisplayQueue {
    pub(super) fn new() -> Self {
        Self {
            chunks: vec![
                DisplayChunk {
                    bytes: [0; CHUNK_BYTES],
                    len: 0,
                    dropped_before: 0,
                };
                QUEUE_CAPACITY
            ],
            head: 0,
            len: 0,
            dropped: 0,
        }
    }

    pub(super) fn push(&mut self, mut bytes: &[u8]) {
        while !bytes.is_empty() {
            if self.len != 0 && self.dropped == 0 {
                let tail = (self.head + self.len - 1) % QUEUE_CAPACITY;
                let chunk = &mut self.chunks[tail];
                let count = bytes.len().min(CHUNK_BYTES - chunk.len);
                chunk.bytes[chunk.len..chunk.len + count].copy_from_slice(&bytes[..count]);
                chunk.len += count;
                bytes = &bytes[count..];
                if bytes.is_empty() {
                    break;
                }
            }
            if self.len == QUEUE_CAPACITY {
                self.dropped = self.dropped.saturating_add(bytes.len() as u64);
                break;
            }
            // Never append across a loss: its notice belongs immediately before
            // the next retained region, even if the old tail has spare room.
            let tail = (self.head + self.len) % QUEUE_CAPACITY;
            let dropped_before = self.take_dropped();
            let chunk = &mut self.chunks[tail];
            let count = bytes.len().min(CHUNK_BYTES);
            chunk.bytes[..count].copy_from_slice(&bytes[..count]);
            chunk.len = count;
            chunk.dropped_before = dropped_before;
            self.len += 1;
            bytes = &bytes[count..];
        }
    }

    pub(super) fn pop_into(&mut self, bytes: &mut [u8; CHUNK_BYTES]) -> Option<(usize, u64)> {
        if self.len == 0 {
            return None;
        }
        let chunk = &self.chunks[self.head];
        bytes[..chunk.len].copy_from_slice(&chunk.bytes[..chunk.len]);
        let result = (chunk.len, chunk.dropped_before);
        self.head = (self.head + 1) % QUEUE_CAPACITY;
        self.len -= 1;
        Some(result)
    }

    pub(super) fn take_dropped(&mut self) -> u64 {
        std::mem::take(&mut self.dropped)
    }
}

pub(super) fn drain_display(worker: &Worker, view: &mut SessionView) -> bool {
    let revision = view.terminal.revision();
    let mut bytes = [0; CHUNK_BYTES];
    for _ in 0..QUEUE_CAPACITY {
        // Keep the guard out of the parser, which may allocate and trim history.
        let chunk = worker.display.lock().pop_into(&mut bytes);
        let Some((len, dropped_before)) = chunk else {
            break;
        };
        if dropped_before != 0 {
            dropped_notice(&mut view.terminal, dropped_before);
        }
        view.terminal.push(&bytes[..len]);
    }
    revision != view.terminal.revision()
}

pub(super) fn dropped_notice(terminal: &mut TerminalBuffer, bytes: u64) {
    terminal.finish();
    terminal.push(rust_i18n::t!("serial.transcript.dropped_notice", bytes = bytes).as_bytes());
}

pub(super) struct ConsoleLines {
    line: Vec<u8>,
    oversized: bool,
    identity: DeviceIdentity,
    shared: Arc<Mutex<IdentitySnapshot>>,
    rssi: ScanDecoder,
    rssi_shared: Arc<Mutex<Option<ScanSnapshot>>>,
    pairing_ansi: crate::rssi_scan::ansi::AnsiFilter,
}

impl ConsoleLines {
    pub(super) fn new(
        shared: Arc<Mutex<IdentitySnapshot>>,
        rssi_shared: Arc<Mutex<Option<ScanSnapshot>>>,
    ) -> Self {
        Self {
            line: Vec::new(),
            oversized: false,
            identity: DeviceIdentity::default(),
            shared,
            rssi: ScanDecoder::new(),
            rssi_shared,
            pairing_ansi: crate::rssi_scan::ansi::AnsiFilter::default(),
        }
    }

    pub(super) fn push(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            if byte == b'\r' || byte == b'\n' {
                self.finish_line(true);
            } else if !self.oversized {
                if self.line.len() < MAX_IDENTITY_LINE {
                    self.line.push(byte);
                } else {
                    self.oversized = true;
                    // Only discarded lines need streaming control-state updates:
                    // no second byte framer runs for normal console traffic.
                    self.rssi.discard_line(&self.line);
                    self.rssi.discard_line(&[byte]);
                    self.pairing_ansi.discard(&self.line);
                    self.pairing_ansi.discard(&[byte]);
                    self.line.clear();
                }
            } else {
                self.rssi.discard_line(&[byte]);
                self.pairing_ansi.discard(&[byte]);
            }
        }
    }

    pub(super) fn finish(&mut self) {
        // Preserve identity's historical EOF flush without treating a partial
        // RSSI cell as a complete console line. Closed owners publish nothing.
        self.finish_line(false);
        self.rssi_shared.lock().take();
    }

    fn finish_line(&mut self, terminated: bool) {
        if !self.oversized {
            let line = String::from_utf8_lossy(&self.line);
            if terminated {
                match &line {
                    std::borrow::Cow::Borrowed(line) => {
                        if let Some(update) = self.rssi.feed_line(line) {
                            *self.rssi_shared.lock() = Some(update);
                        }
                        let line = self.pairing_ansi.filter(line);
                        if self.identity.role == DeviceRole::Receiver
                            && receiver_pairing_success(line)
                        {
                            self.shared.lock().pairing_changed = true;
                        }
                    }
                    std::borrow::Cow::Owned(_) => {
                        self.rssi.discard_line(&self.line);
                        self.rssi.discard_line(b"\n");
                        self.pairing_ansi.discard(&self.line);
                        self.pairing_ansi.discard(b"\n");
                    }
                }
            }
            if !line.is_empty() {
                self.identity.ingest_line(&line);
                let mut snapshot = self.shared.lock();
                let old = &snapshot.identity;
                let new = &self.identity;
                if old.name != new.name
                    || old.role != new.role
                    || old.address != new.address
                    || old.tracker_id != new.tracker_id
                    || old.target != new.target
                    || old.board != new.board
                {
                    snapshot.identity = self.identity.clone();
                    snapshot.revision = snapshot.revision.wrapping_add(1);
                }
            }
        } else if terminated {
            // Delimiters end any incomplete UTF-8 codepoint in discarded input,
            // but deliberately do not terminate an open OSC/control string.
            self.rssi.discard_line(b"\n");
            self.pairing_ansi.discard(b"\n");
        }
        self.line.clear();
        self.oversized = false;
    }
}

fn receiver_pairing_success(line: &str) -> bool {
    let line = line.trim();
    let line = if let Some(timestamped) = line.strip_prefix('[') {
        let Some((timestamp, payload)) = timestamped.split_once("] ") else {
            return false;
        };
        if timestamp.is_empty()
            || !timestamp
                .bytes()
                .all(|byte| byte.is_ascii_digit() || b":.,".contains(&byte))
        {
            return false;
        }
        payload
    } else {
        line
    };
    let Some(message) = line.strip_prefix("<inf> esb_event: ") else {
        return false;
    };
    if message == "Pairings cleared and saved" {
        return true;
    }
    let Some((slot, address)) = message
        .strip_prefix("Removed device on id ")
        .and_then(|message| message.split_once(" with address "))
    else {
        return false;
    };
    slot.parse::<u8>().is_ok_and(|slot| slot < 16)
        && address.len() == 12
        && address.bytes().all(|byte| byte.is_ascii_hexdigit())
        && address != "000000000000"
        && !address.eq_ignore_ascii_case("FFFFFFFFFFFF")
}

#[cfg(test)]
mod tests {
    use super::{ConsoleLines, IdentitySnapshot};
    use crate::domain::DeviceIdentity;
    use parking_lot::Mutex;
    use std::sync::Arc;

    fn lines() -> (ConsoleLines, Arc<Mutex<IdentitySnapshot>>) {
        let shared = Arc::new(Mutex::new(IdentitySnapshot {
            identity: DeviceIdentity::default(),
            revision: 0,
            pairing_changed: false,
        }));
        (
            ConsoleLines::new(Arc::clone(&shared), Arc::new(Mutex::new(None))),
            shared,
        )
    }

    #[test]
    fn receiver_success_logs_are_observed_only_after_complete_fragmented_lines() {
        for success in [
            "<inf> esb_event: Pairings cleared and saved",
            "[00:00:20.018,035] <inf> esb_event: Removed device on id 9 with address 010203040506",
        ] {
            for fragment in [1, 7, 1024] {
                let (mut lines, shared) = lines();
                lines.push(b"Device name: SlimeNRF Receiver\r\n");
                let raw = format!("\x1b[0m\x1b[32m{success}\x1b[0m");
                for bytes in raw.as_bytes().chunks(fragment) {
                    lines.push(bytes);
                    assert!(!shared.lock().pairing_changed);
                }
                lines.push(b"\r");
                assert!(shared.lock().pairing_changed);
                shared.lock().pairing_changed = false;
                lines.push(b"\n");
                assert!(!shared.lock().pairing_changed);
            }
        }
    }

    #[test]
    fn echoes_failures_tracker_clear_and_unterminated_or_hidden_logs_do_not_confirm_mutation() {
        for line in [
            "clear",
            "remove",
            "Pairings cleared and saved",
            "<err> esb_event: Pairings not cleared: -5",
            "<err> esb_event: Pairing removal not saved: -5",
            "<inf> esb_event: Pairings cleared",
            "<inf> esb_event: Pairings cleared and saved extra",
            "<inf> tracker: Pairings cleared and saved",
            "<inf> esb_event: Removed device on id 16 with address 010203040506",
            "<inf> esb_event: Removed device on id 9 with address 000000000000",
            "<inf> esb_event: Removed device on id 9 with address FFFFFFFFFFFF",
            "<inf> esb_event: Removed device on id 9 with address 01020304050Z",
            "<inf> esb_event: Removed device on id 9 with address 010203040506 extra",
            "\x1b]0;<inf> esb_event: Pairings cleared and saved\x07",
        ] {
            let (mut lines, shared) = lines();
            lines.push(b"Device name: SlimeNRF Receiver\n");
            lines.push(line.as_bytes());
            lines.push(b"\n");
            assert!(!shared.lock().pairing_changed, "{line:?}");
        }
        for identity in ["Device name: SlimeNRF Tracker\n", "Device name: Unknown\n"] {
            let (mut lines, shared) = lines();
            lines.push(identity.as_bytes());
            lines.push(b"<inf> esb_event: Pairings cleared and saved\n");
            assert!(!shared.lock().pairing_changed);
        }
        let (mut lines, shared) = lines();
        lines.push(b"Device name: SlimeNRF Receiver\n");
        lines.push(b"<inf> esb_event: Pairings cleared and saved");
        lines.finish();
        assert!(!shared.lock().pairing_changed);
    }

    #[test]
    fn discarded_console_lines_do_not_turn_embedded_log_text_into_success() {
        let (mut lines, shared) = lines();
        lines.push(b"Device name: SlimeNRF Receiver\n");
        lines.push(&vec![b'x'; super::MAX_IDENTITY_LINE + 1]);
        lines.push(b"<inf> esb_event: Pairings cleared and saved\n");
        lines.push(b"\xff<inf> esb_event: Pairings cleared and saved\n");
        assert!(!shared.lock().pairing_changed);
        lines.push(b"<inf> esb_event: Pairings cleared and saved\n");
        assert!(shared.lock().pairing_changed);
    }
}
