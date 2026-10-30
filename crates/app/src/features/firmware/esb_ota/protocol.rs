use super::FirmwareInfo;
use slimenrf_firmware_image::image::FirmwareImage;

pub(super) const QUERY: u8 = 0xf0;
pub(super) const INFO: u8 = 0xf1;
pub(super) const BEGIN: u8 = 0xf2;
pub(super) const DATA: u8 = 0xf3;
pub(super) const STATUS: u8 = 0xf4;
pub(super) const VERIFY: u8 = 0xf5;
pub(super) const ACTIVATE: u8 = 0xf6;
pub(super) const ABORT: u8 = 0xf7;
pub(super) const PAYLOAD: usize = 60;

pub(super) fn command(opcode: u8, id: u8) -> [u8; 64] {
    let mut packet = [0; 64];
    packet[0] = opcode;
    packet[1] = id;
    packet
}

pub(super) fn begin(id: u8, image: &FirmwareImage, board: &str) -> Result<[u8; 64], String> {
    let packets = image.data.len().div_ceil(PAYLOAD);
    if image.data.is_empty() || packets > u16::MAX as usize {
        return Err("ESB image exceeds packet sequence range or is empty".into());
    }
    if board.is_empty() || board.len() > 47 || board.contains('\0') {
        return Err("ESB board target must contain 1–47 UTF-8 bytes without NUL".into());
    }
    if image.base_address & 0xfff != 0 || image.base_address >> 12 > u16::MAX as u32 {
        return Err("ESB firmware base is not a representable flash page".into());
    }
    let mut packet = command(BEGIN, id);
    packet[2..6].copy_from_slice(&(image.data.len() as u32).to_le_bytes());
    packet[6..10].copy_from_slice(&image.crc32.to_le_bytes());
    packet[10..12].copy_from_slice(&(packets as u16).to_be_bytes());
    packet[12] = 1;
    packet[13..13 + board.len()].copy_from_slice(board.as_bytes());
    packet[61..63].copy_from_slice(&((image.base_address >> 12) as u16).to_be_bytes());
    Ok(packet)
}

pub(super) fn data(id: u8, sequence: usize, image: &[u8]) -> [u8; 64] {
    let mut packet = command(DATA, id);
    packet[2..4].copy_from_slice(&(sequence as u16).to_be_bytes());
    let start = sequence * PAYLOAD;
    let end = (start + PAYLOAD).min(image.len());
    packet[4..4 + end - start].copy_from_slice(&image[start..end]);
    packet
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Status {
    pub id: u8,
    pub code: u8,
    pub next: usize,
    pub bytes: u32,
    pub ring: u8,
}

impl Status {
    pub fn parse(record: &[u8; 16]) -> Option<Self> {
        (record[0] == STATUS).then(|| Self {
            id: record[1],
            code: record[2],
            next: u16::from_be_bytes([record[3], record[4]]) as usize,
            bytes: u32::from_le_bytes(record[5..9].try_into().unwrap()),
            ring: record[9],
        })
    }

    pub fn failure(self) -> Option<String> {
        let description = match self.code {
            4 => "firmware CRC verification failed",
            0x10 => "firmware rejected OTA operation",
            0x11 => "board target mismatch",
            0x12 => "flash write failed",
            0x13 => "firmware image exceeds device capacity",
            0x15 => "device OTA timeout",
            _ => return None,
        };
        Some(format!("Target {}: {description}", self.id))
    }
}

// FW_INFO is a 66-byte ESB packet, not a single 16-byte HID record.
// Five chunks carry bytes 2..66; the sixth emitted chunk is empty.
pub(super) struct InfoChunks {
    bytes: [u8; 66],
    seen: u8,
}

impl InfoChunks {
    pub fn new(id: u8) -> Self {
        let mut bytes = [0; 66];
        bytes[0] = 0x22;
        bytes[1] = id;
        Self { bytes, seen: 0 }
    }

    pub fn push(&mut self, record: &[u8; 16]) -> Result<Option<FirmwareInfo>, String> {
        if record[0] != INFO || record[1] != self.bytes[1] || record[2] >= 5 {
            return Ok(None);
        }
        let index = record[2] as usize;
        let start = 2 + 13 * index;
        let count = 13.min(66 - start);
        self.bytes[start..start + count].copy_from_slice(&record[3..3 + count]);
        self.seen |= 1 << index;
        if self.seen != 0x1f {
            return Ok(None);
        }
        let b = &self.bytes;
        if crc8(&b[..65]) != b[65] {
            return Err("FW_INFO CRC mismatch; query again".into());
        }
        let end = b[15..63].iter().position(|v| *v == 0).unwrap_or(48) + 15;
        let board = std::str::from_utf8(&b[15..end]).map_err(|_| "Invalid FW_INFO board UTF-8")?;
        if board.is_empty() || end == 63 {
            return Err("FW_INFO contains no valid terminated board target".into());
        }
        let dt = u32::from_be_bytes(b[5..9].try_into().unwrap());
        Ok(Some(FirmwareInfo {
            version: format!("{}.{}.{}", b[2], b[3], b[4]),
            build_date: format!(
                "{}-{:02}-{:02} {:02}:{:02}:{:02}",
                2020 + (dt >> 25),
                (dt >> 21) & 15,
                (dt >> 16) & 31,
                (dt >> 11) & 31,
                (dt >> 5) & 63,
                (dt & 31) * 2
            ),
            firmware_size: u32::from_le_bytes(b[9..13].try_into().unwrap()),
            bootloader: match b[13] {
                0 => "none".into(),
                1 => "adafruit_uf2".into(),
                2 => "nrf5_opendfu".into(),
                3 => "mcuboot".into(),
                value => format!("unknown({value})"),
            },
            protocol_version: b[14],
            board_target: board.into(),
            flash_base: u16::from_be_bytes([b[63], b[64]]) as u32 * 4096,
        }))
    }
}

pub(super) fn crc8(bytes: &[u8]) -> u8 {
    let mut crc = 0u8;
    for byte in bytes {
        crc ^= byte;
        for _ in 0..8 {
            crc = if crc & 0x80 != 0 {
                (crc << 1) ^ 7
            } else {
                crc << 1
            };
        }
    }
    crc
}
