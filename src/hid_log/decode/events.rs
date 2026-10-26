//! Legacy tracker notifications, matching receiver scripts/hid_cmd.py.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventSource {
    Tracker,
    Receiver,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrackerEvent {
    pub source: EventSource,
    pub snapshot: bool,
    pub tracker_id: u8,
    pub boot_id: u32,
    pub sequence: u16,
    pub operation_id: u16,
    pub kind: u8,
    /// Preserve the wire flag for every kind; only calibration assigns origin semantics.
    pub automatic_flag: bool,
    pub action: u8,
    pub outcome: u8,
    pub phase: u8,
    pub detail: u8,
}

impl TrackerEvent {
    pub fn parse(bytes: &[u8; 16]) -> Option<Self> {
        if bytes[0] != 251 || bytes[1] != 0 || !matches!(bytes[2], 225 | 226) {
            return None;
        }
        let action = bytes[3] & 15;
        let outcome = bytes[3] >> 4;
        Some(Self {
            source: if bytes[2] == 225 {
                EventSource::Tracker
            } else {
                EventSource::Receiver
            },
            snapshot: bytes[2] == 226 && action == 6 && outcome == 0,
            tracker_id: bytes[4],
            boot_id: u32::from_le_bytes(bytes[5..9].try_into().unwrap()),
            sequence: u16::from_le_bytes([bytes[9], bytes[10]]),
            operation_id: u16::from_le_bytes([bytes[11], bytes[12]]),
            kind: bytes[13] & 127,
            automatic_flag: bytes[13] & 128 != 0,
            action,
            outcome,
            phase: bytes[14],
            detail: bytes[15],
        })
    }

    pub fn is_calibration(&self) -> bool {
        (1..=7).contains(&self.kind)
    }
    pub fn button_count(&self) -> Option<(u8, bool)> {
        (self.kind == 0x31).then_some((self.detail, self.detail < 255))
    }
    pub fn action_name(&self) -> Option<&'static str> {
        indexed(
            &[
                "ACCEPTED", "BEGIN", "STEP", "END", "REJECTED", "STATE", "NOTICE",
            ],
            self.action.checked_sub(1)?,
        )
    }
    pub fn outcome_name(&self) -> Option<&'static str> {
        indexed(
            &[
                "NONE",
                "SUCCESS",
                "FAILED",
                "CANCELLED",
                "SKIPPED",
                "UNKNOWN",
            ],
            self.outcome,
        )
    }
    pub fn phase_name(&self) -> Option<&'static str> {
        if self.is_calibration() {
            return indexed(
                &[
                    "NONE",
                    "IDENTIFY",
                    "WAIT_STILL",
                    "SENSOR_RETRIM",
                    "COLLECT",
                    "WAIT_POSE",
                    "CAPTURE_POSE",
                    "POSE_DONE",
                    "RETRY",
                    "WAIT_ROTATION",
                    "RECORD_ROTATION",
                    "FIT",
                    "FREEZE",
                    "VALIDATE",
                    "PROBATION",
                    "APPLY_PENDING",
                    "APPLIED",
                    "CONFIRM",
                    "STORAGE",
                    "COVERAGE",
                ],
                self.phase,
            );
        }
        match (self.kind, self.phase) {
            (0x20, 0) => Some("NOT_REST"),
            (0x20, 1) => Some("REST"),
            (0x20, 2) => Some("UNKNOWN"),
            (0x21, 0) => Some("NOT_REST_DETECTED"),
            (0x21, 1) => Some("REST_DETECTED"),
            (0x21, 2) => Some("UNKNOWN"),
            (0x21, 3) => Some("UNAVAILABLE"),
            (0x30, 1) => Some("WILL_WOM"),
            (0x30, 2) => Some("WILL_SHUTDOWN"),
            (0x30, 3) => Some("BOOT"),
            (0x30, 4) => Some("WAKE"),
            (0x30, 5) => Some("WILL_REBOOT"),
            (0x30, 6) => Some("WOM_CANCELLED"),
            (0x30, 7) => Some("WATCHDOG_RESET"),
            (0x31, 1) => Some("CLICK_GROUP"),
            _ => None,
        }
    }
    pub fn detail_name(&self) -> Option<&'static str> {
        if self.is_calibration() && (matches!(self.action, 4 | 5) || matches!(self.phase, 8 | 18)) {
            return indexed(
                &[
                    "NONE",
                    "BUSY",
                    "UNSUPPORTED",
                    "INVALID_ARGUMENT",
                    "SENSOR_UNAVAILABLE",
                    "MOTION",
                    "SAMPLE_TIMEOUT",
                    "INSUFFICIENT_SAMPLES",
                    "POSE_TIMEOUT",
                    "FIT_ERROR",
                    "INVALID_MODEL",
                    "QUALITY",
                    "NO_BENEFIT",
                    "ENVIRONMENT_ONLY",
                    "DISABLED",
                    "REPLACED",
                    "POWER_DOWN",
                    "RESET",
                    "STORAGE_ERROR",
                    "CANDIDATE_REJECTED",
                    "NO_TCAL_COVERAGE",
                    "START_TIMEOUT",
                    "RECORD_TIMEOUT",
                    "TEMPERATURE",
                    "RADIAL",
                    "DIP",
                    "COVERAGE",
                    "EXPIRED",
                    "INVALID_SAMPLE",
                    "OVERFLOW",
                    "TRANSPORT_SILENCE",
                    "SESSION_CHANGED",
                    "PARTIAL",
                ],
                self.detail,
            );
        }
        match self.kind {
            0x20 => indexed(
                &[
                    "OBSERVED",
                    "RESET",
                    "SUSPENDED",
                    "NO_FRESH_FRAME",
                    "INITIALIZING",
                ],
                self.detail,
            ),
            0x21 => indexed(&["UNKNOWN", "VQF", "EQF"], self.detail),
            0x30 if matches!(self.phase, 1 | 6) => {
                indexed(&["UNKNOWN", "WOM_NORMAL", "WOM_FORCED"], self.detail)
            }
            _ => None,
        }
    }
}
fn indexed(names: &[&'static str], value: u8) -> Option<&'static str> {
    names.get(usize::from(value)).copied()
}
