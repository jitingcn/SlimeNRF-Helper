//! Identity registration and correlated control acknowledgements without retries.
use super::super::io::CaptureIo;
use super::super::{CaptureMode, CaptureSnapshot};
use super::{ACK_TIMEOUT, Session};
use parking_lot::Mutex;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

impl Session<'_> {
    pub(super) fn control(
        &mut self,
        bytes: &[u8],
        pending: Option<(u8, u8)>,
    ) -> Result<Option<u8>, String> {
        if bytes.is_empty() {
            return Ok(None);
        }
        if bytes.len() != 64 {
            return Err(format!("Unexpected main HID report size {}", bytes.len()));
        }
        let mut status = None;
        for record in bytes.as_chunks::<16>().0 {
            if record[0] == 251 && pending == Some((record[1], record[2])) {
                status = Some(record[3]);
            }
            if record[0] == 255 && record[1] < 16 {
                let slot = record[1];
                if self.registration_first == Some(slot) {
                    self.registration_cycle = true;
                }
                self.registration_first.get_or_insert(slot);
                const HEX: &[u8; 16] = b"0123456789ABCDEF";
                let mut address = [0; 12];
                for (index, byte) in record[2..8].iter().rev().enumerate() {
                    address[index * 2] = HEX[(byte >> 4) as usize];
                    address[index * 2 + 1] = HEX[(byte & 15) as usize];
                }
                // Unknown/empty registration is not positive evidence of reassignment.
                if address == *b"000000000000" {
                    continue;
                }
                let selected =
                    matches!(&self.request.mode, CaptureMode::Single { slot: s, .. } if *s == slot);
                if let Some(expected) = self.expected.get(&slot) {
                    if expected.as_bytes() != address {
                        self.identity_failed = true;
                        return Err(format!(
                            "RF identity changed for slot {slot}; capture stopped"
                        ));
                    }
                    self.seen.insert(slot);
                } else if selected || matches!(self.request.mode, CaptureMode::AllPaired { .. }) {
                    self.identity_failed = true;
                    return Err(format!(
                        "Paired roster changed at slot {slot}; choose the source again"
                    ));
                }
            }
        }
        Ok(status)
    }
    pub(super) fn command(
        &mut self,
        io: &mut impl CaptureIo,
        opcode: u8,
        args: &[u8],
        cancel: Option<&AtomicBool>,
        shared: &Mutex<Arc<CaptureSnapshot>>,
    ) -> Result<u8, String> {
        if cancel.is_some_and(|c| c.load(Ordering::Acquire)) {
            return Err("Capture cancelled before control write".into());
        }
        self.seq = self.seq.wrapping_add(1).max(1);
        let seq = self.seq;
        let mut report = [0; 65]; // hidapi requires the zero report-ID prefix.
        report[1] = 254;
        report[2] = seq;
        report[3] = opcode;
        report[5..5 + args.len()].copy_from_slice(args);
        // Mark before write: failed/partial HID writes can still have reached the receiver.
        if matches!(opcode, 214 | 221) {
            self.start_attempted = true;
        }
        io.write_control(&report)?;
        let deadline = io.now() + ACK_TIMEOUT;
        while io.now() < deadline {
            if cancel.is_some_and(|c| c.load(Ordering::Acquire)) {
                return Err("Capture start cancelled".into());
            }
            let mut input = [0; 65];
            let n = io.read_control(&mut input, 0)?;
            let status = self.control(&input[..n], Some((seq, opcode)))?;
            if let Some(status) = status {
                self.accept_status(opcode, status)?;
            }
            // Keep the actual data reader/writer running while any control ACK is pending.
            let data_result = self.pump_data(io);
            if cancel.is_some() {
                data_result?;
            } else if let Err(e) = data_result {
                self.cleanup_error.get_or_insert_with(|| e.clone());
                self.warn(e);
            }
            self.publish(io, shared);
            if let Some(status) = status {
                return Ok(status);
            }
            if self.data_failed {
                let n = io.read_control(&mut input, 10)?;
                if let Some(status) = self.control(&input[..n], Some((seq, opcode)))? {
                    return self.accept_status(opcode, status);
                }
            }
        }
        if opcode == 223 {
            self.metadata_disabled = true;
        }
        Err(format!(
            "ACK timeout for opcode {opcode}, sequence {seq}; command not retried"
        ))
    }
    fn accept_status(&mut self, opcode: u8, status: u8) -> Result<u8, String> {
        if matches!(status, 0 | 6 | 7) {
            return Ok(status);
        }
        if matches!(opcode, 214 | 221) {
            self.start_rejected = true;
        }
        if opcode == 223 {
            self.metadata_disabled = true;
        }
        Err(format!(
            "Receiver rejected opcode {opcode}: status {status}"
        ))
    }
}
