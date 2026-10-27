#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EventSubscription {
    pub version: u8,
    pub capabilities: u8,
    pub max_repeats: u8,
    pub spacing_ms: u16,
    pub calibration_heartbeat_ms: u16,
    pub calibration_silence_ms: u16,
    pub lease_ms: u16,
    pub tracker_filter: u8,
    pub active: bool,
    pub kind_mask: u8,
    pub flags_raw: u8,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WomResult {
    Tracker {
        tracker_id: u8,
        minutes: u16,
        error: u16,
        token: u16,
    },
    Complete {
        token: u16,
        success_mask: u32,
        failure_mask: u32,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommandAck {
    pub sequence: u8,
    pub opcode: u8,
    pub status: u8,
    pub result: [u8; 12],
    pub event_subscription: Option<EventSubscription>,
    pub wom: Option<WomResult>,
}
impl CommandAck {
    pub fn parse(b: &[u8; 16]) -> Self {
        let result: [u8; 12] = b[4..16].try_into().unwrap();
        let event_subscription = (b[2] == 224 && result[0] == 1).then(|| EventSubscription {
            version: result[0],
            capabilities: result[1],
            max_repeats: result[2],
            spacing_ms: u16::from(result[3]) * 10,
            calibration_heartbeat_ms: le16(&result, 4),
            calibration_silence_ms: le16(&result, 6),
            lease_ms: le16(&result, 8),
            tracker_filter: result[10],
            active: result[11] & 1 != 0,
            kind_mask: (result[11] >> 1) & 31,
            flags_raw: result[11],
        });
        let wom = if b[2] == 0x28 {
            match result[0] {
                1 => Some(WomResult::Tracker {
                    tracker_id: result[1],
                    minutes: le16(&result, 2),
                    error: le16(&result, 4),
                    token: le16(&result, 6),
                }),
                2 => Some(WomResult::Complete {
                    token: le16(&result, 1),
                    success_mask: le32(&result, 3),
                    failure_mask: le32(&result, 7),
                }),
                _ => None,
            }
        } else {
            None
        };
        Self {
            sequence: b[1],
            opcode: b[2],
            status: b[3],
            result,
            event_subscription,
            wom,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Command {
    pub sequence: u8,
    pub opcode: u8,
    pub flags: u8,
    pub arguments: [u8; 12],
}
impl Command {
    pub fn parse(b: &[u8]) -> Self {
        Self {
            sequence: b[1],
            opcode: b[2],
            flags: b[3],
            arguments: b[4..16].try_into().unwrap(),
        }
    }
    pub fn tracker_id(&self) -> Option<u8> {
        // Only verified tracker-targeted opcodes use arguments[0] as a tracker ID.
        (matches!(self.opcode, 1..=9 | 0x0c..=0x28 | 0x30..=0x33 | 214 | 224)
            && self.arguments[if self.opcode == 224 { 2 } else { 0 }] != 255)
            .then_some(self.arguments[if self.opcode == 224 { 2 } else { 0 }])
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ota {
    Control {
        opcode: u8,
        target: u8,
    },
    InfoFragment {
        target: u8,
        index: u8,
        payload: [u8; 13],
    },
    Status {
        target: u8,
        code: u8,
        next_sequence: u16,
        bytes: u32,
        ring: u8,
    },
    Begin {
        target: u8,
        size: u32,
        crc32: u32,
        packet_count: u16,
        protocol: u8,
        board_wire: [u8; 48],
        flash_base: u32,
    },
    Data {
        target: u8,
        sequence: u16,
    },
}
impl Ota {
    pub fn inbound(b: &[u8; 16]) -> Self {
        match b[0] {
            0xf1 => Self::InfoFragment {
                target: b[1],
                index: b[2],
                payload: b[3..16].try_into().unwrap(),
            },
            0xf4 => Self::Status {
                target: b[1],
                code: b[2],
                next_sequence: u16::from_be_bytes([b[3], b[4]]),
                bytes: le32(b, 5),
                ring: b[9],
            },
            _ => Self::Control {
                opcode: b[0],
                target: b[1],
            },
        }
    }
    pub fn outbound(b: &[u8]) -> Self {
        match b[0] {
            0xf2 => Self::Begin {
                target: b[1],
                size: le32(b, 2),
                crc32: le32(b, 6),
                packet_count: u16::from_be_bytes([b[10], b[11]]),
                protocol: b[12],
                board_wire: b[13..61].try_into().unwrap(),
                flash_base: u32::from(u16::from_be_bytes([b[61], b[62]])) * 4096,
            },
            0xf3 => Self::Data {
                target: b[1],
                sequence: u16::from_be_bytes([b[2], b[3]]),
            },
            _ => Self::Control {
                opcode: b[0],
                target: b[1],
            },
        }
    }
    pub fn target(&self) -> u8 {
        match self {
            Self::Control { target, .. }
            | Self::InfoFragment { target, .. }
            | Self::Status { target, .. }
            | Self::Begin { target, .. }
            | Self::Data { target, .. } => *target,
        }
    }
    pub fn opcode(&self) -> u8 {
        match self {
            Self::Control { opcode, .. } => *opcode,
            Self::InfoFragment { .. } => 0xf1,
            Self::Status { .. } => 0xf4,
            Self::Begin { .. } => 0xf2,
            Self::Data { .. } => 0xf3,
        }
    }
}
fn le16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(b[at..at + 2].try_into().unwrap())
}
fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

pub fn opcode_name(op: u8) -> Option<&'static str> {
    Some(match op {
        1 => "shutdown",
        2 => "calibrate",
        3 => "calibrate-acc",
        4 => "meow",
        5 => "scan",
        6 => "mag-clear",
        7 => "reboot",
        8 => "clear",
        9 => "dfu",
        0x0c => "sens-set",
        0x0d => "sens-reset",
        0x0e => "reset-zro",
        0x0f => "reset-acc",
        0x10 => "reset-bat",
        0x11 => "ping",
        0x12 => "reset-tcal",
        0x13 => "tcal-auto-on",
        0x14 => "tcal-auto-off",
        0x15 => "fusion",
        0x16 => "tcal-boot-on",
        0x17 => "tcal-boot-off",
        0x18 => "mag-cal",
        0x19 => "mag-on",
        0x1a => "mag-off",
        0x1b => "tcal-on",
        0x1c => "tcal-off",
        0x1d => "tdma-on",
        0x1e => "tdma-off",
        0x1f => "test-on",
        0x20 => "test-off",
        0x21 => "dfu-ota",
        0x22 => "collect-on",
        0x23 => "collect-off",
        0x24 => "sens-auto",
        0x25 => "mag-auto-on",
        0x26 => "mag-auto-off",
        0x27 => "tcal-heat-start",
        0x28 => "wom",
        0x30 => "ota-query-info",
        0x31 => "ota-abort",
        0x32 => "ota-suppress",
        0x33 => "ota-unsuppress",
        201 => "pair",
        202 => "exit-pair",
        203 => "clear",
        204 => "add",
        205 => "remove",
        206 => "list",
        207 => "channel-set",
        208 => "channel-clear",
        209 => "rssi-scan",
        210 => "info",
        211 => "uptime",
        212 => "stats",
        213 => "resetstats",
        214 => "collect-start",
        215 => "collect-stop",
        216 => "reboot",
        217 => "dfu",
        218 => "tracker-channel-all",
        219 => "tracker-channel-clear",
        220 => "nop",
        224 => "tracker-events",
        225 => "tracker-event",
        226 => "tracker-observation",
        0xf0 => "OTA_QUERY",
        0xf1 => "FW_INFO",
        0xf2 => "OTA_BEGIN",
        0xf3 => "OTA_DATA",
        0xf4 => "OTA_STATUS",
        0xf5 => "OTA_VERIFY",
        0xf6 => "OTA_ACTIVATE",
        0xf7 => "OTA_ABORT",
        _ => return None,
    })
}
pub fn status_name(status: u8) -> Option<&'static str> {
    [
        "OK", "EINVAL", "ENOSPC", "EBUSY", "ENOENT", "ENOTSUP", "QUEUED", "STARTED",
    ]
    .get(usize::from(status))
    .copied()
}
