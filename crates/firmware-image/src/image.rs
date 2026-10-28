//! Bounded firmware image parsing and nRF52 UF2 content validation.

use sha2::{Digest, Sha256};
use std::{borrow::Cow, fmt, path::Path};

#[path = "chip.rs"]
mod chip;
pub use chip::Nrf52Chip;

/// Broad, actionable categories of firmware image failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorKind {
    /// The image is structurally invalid or incomplete.
    Malformed,
    /// The format or feature is not supported by this parser.
    Unsupported,
    /// A configured image or address bound was exceeded.
    Limit,
    /// A checksum or image digest does not match.
    Integrity,
}

#[derive(Debug)]
enum ErrorCause {
    Utf8(std::str::Utf8Error),
    Integer(std::num::TryFromIntError),
}

/// A firmware parsing or content-validation failure.
#[derive(Debug)]
pub struct Error {
    kind: ErrorKind,
    message: String,
    cause: Option<ErrorCause>,
}

impl Error {
    /// Identifies the reason for rejection without interpreting diagnostics.
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            cause: None,
        }
    }

    fn malformed(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Malformed, message)
    }

    fn with_cause(kind: ErrorKind, message: impl Into<String>, cause: ErrorCause) -> Self {
        Self {
            kind,
            message: message.into(),
            cause: Some(cause),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.cause {
            Some(ErrorCause::Utf8(error)) => Some(error),
            Some(ErrorCause::Integer(error)) => Some(error),
            None => None,
        }
    }
}

impl From<Error> for String {
    fn from(error: Error) -> Self {
        error.message
    }
}

/// Maximum assembled image size, in bytes.
pub const MAX_IMAGE_SIZE: usize = 2 * 1024 * 1024;
/// Maximum UF2/HEX/MCUboot container size, in bytes.
pub const MAX_CONTAINER_SIZE: usize = 16 * 1024 * 1024;
const UF2_MAGIC: u32 = 0x0a32_4655;
const MCU_MAGIC: u32 = 0x96f3_b83d;
const APP_FLOOR: u32 = 0x1000;

/// The assembled firmware bytes and their transfer metadata.
#[derive(Clone, Debug)]
pub struct FirmwareImage {
    /// Assembled image bytes, including any flash gaps.
    pub data: Vec<u8>,
    /// Starting address represented by the assembled bytes.
    pub base_address: u32,
    /// CRC-32 of the assembled image bytes.
    pub crc32: u32,
    /// Whether this is a complete MCUboot update container.
    pub mcuboot: bool,
}

/// Metadata from structural UF2 inspection.
#[derive(Clone, Debug)]
pub struct Uf2Metadata {
    /// The recorded UF2 family, if blocks carry a family tag.
    pub family_id: Option<u32>,
}

fn u32le(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}
fn u16le(bytes: &[u8], offset: usize) -> usize {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap()) as usize
}
fn image(data: Vec<u8>, base_address: u32, mcuboot: bool) -> FirmwareImage {
    let crc32 = crc32fast::hash(&data);
    FirmwareImage {
        data,
        base_address,
        crc32,
        mcuboot,
    }
}

/// Parses a bounded UF2, Intel HEX, or MCUboot image.
///
/// # Errors
/// Returns an error if the image format or its contents are invalid.
pub fn parse_image(name: &str, bytes: &[u8]) -> Result<FirmwareImage, Error> {
    parse_image_inner(name, bytes)
}

fn parse_image_inner(name: &str, bytes: &[u8]) -> Result<FirmwareImage, Error> {
    if bytes.is_empty() || bytes.len() > MAX_CONTAINER_SIZE {
        let kind = if bytes.is_empty() {
            ErrorKind::Malformed
        } else {
            ErrorKind::Limit
        };
        return Err(Error::new(kind, "Firmware file is empty or exceeds 16 MiB"));
    }
    let extension = Path::new(name)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match extension.as_str() {
        "uf2" => parse_uf2(bytes),
        "hex" | "ihex" => parse_hex(bytes),
        "bin" => parse_mcuboot(bytes),
        _ if bytes.len() >= 4 && u32le(bytes, 0) == UF2_MAGIC => parse_uf2(bytes),
        _ if bytes.len() >= 4 && u32le(bytes, 0) == MCU_MAGIC => parse_mcuboot(bytes),
        _ if bytes.iter().copied().find(|b| !b.is_ascii_whitespace()) == Some(b':') => {
            parse_hex(bytes)
        }
        _ => Err(Error::new(
            ErrorKind::Unsupported,
            "Unsupported firmware: select UF2, Intel HEX, or an MCUboot update BIN",
        )),
    }
}

// The nRF52 UICR is a separate address space, never a gap to allocate in an app image.
fn address_range(address: u32, len: usize) -> Result<u32, Error> {
    let end = address
        .checked_add(u32::try_from(len).map_err(|error| {
            Error::with_cause(
                ErrorKind::Limit,
                "Firmware record too large",
                ErrorCause::Integer(error),
            )
        })?)
        .ok_or_else(|| Error::malformed("Firmware address overflow"))?;
    if (address < MAX_IMAGE_SIZE as u32 && end <= MAX_IMAGE_SIZE as u32)
        || (address >= 0x1000_1000 && end <= 0x1000_2000)
    {
        Ok(end)
    } else {
        Err(Error::new(
            ErrorKind::Unsupported,
            format!(
                "Firmware address range {address:#010x}..{end:#010x} is not supported nRF flash/UICR"
            ),
        ))
    }
}

type Uf2Record<'a> = (u32, &'a [u8]);

fn uf2_records(bytes: &[u8]) -> Result<(Vec<Uf2Record<'_>>, Uf2Metadata), Error> {
    if bytes.is_empty() || bytes.len() > MAX_CONTAINER_SIZE || !bytes.len().is_multiple_of(512) {
        let kind = if bytes.len() > MAX_CONTAINER_SIZE {
            ErrorKind::Limit
        } else {
            ErrorKind::Malformed
        };
        return Err(Error::new(
            kind,
            "UF2 length must be a nonzero multiple of 512 bytes (maximum 16 MiB)",
        ));
    }
    let count = bytes.len() / 512;
    let mut seen = vec![false; count];
    let mut family = None;
    let mut family_present = None;
    let mut records = Vec::with_capacity(count);
    for (index, block) in bytes.as_chunks::<512>().0.iter().enumerate() {
        if u32le(block, 0) != UF2_MAGIC
            || u32le(block, 4) != 0x9e5d_5157
            || u32le(block, 508) != 0x0ab1_6f30
        {
            return Err(Error::malformed(format!(
                "UF2 block {index}: invalid magic"
            )));
        }
        let flags = u32le(block, 8);
        // File containers, extension tags and checksum blocks need semantics this writer cannot safely ignore.
        if flags & !0x2000 != 0 {
            return Err(Error::new(
                ErrorKind::Unsupported,
                format!("UF2 block {index}: unsupported flags {flags:#x}"),
            ));
        }
        let payload = u32le(block, 16) as usize;
        let number = u32le(block, 20) as usize;
        if payload == 0
            || payload > 476
            || !payload.is_multiple_of(4)
            || !u32le(block, 12).is_multiple_of(4)
        {
            return Err(Error::malformed(format!(
                "UF2 block {index}: invalid payload size or alignment"
            )));
        }
        if u32le(block, 24) as usize != count || number >= count || seen[number] {
            return Err(Error::malformed(format!(
                "UF2 block {index}: inconsistent/duplicate block numbering"
            )));
        }
        seen[number] = true;
        let has_family = flags & 0x2000 != 0;
        if family_present.is_some_and(|present| present != has_family) {
            return Err(Error::malformed(
                "UF2 mixes family-tagged and untagged blocks",
            ));
        }
        family_present = Some(has_family);
        if has_family {
            let value = u32le(block, 28);
            if value == 0 || family.is_some_and(|id| id != value) {
                return Err(Error::malformed(
                    "UF2 has missing or inconsistent family IDs",
                ));
            }
            family = Some(value);
        }
        let address = u32le(block, 12);
        address_range(address, payload)?;
        records.push((address, &block[32..32 + payload]));
    }
    records.sort_unstable_by_key(|(address, _)| *address);
    for pair in records.windows(2) {
        if pair[0].0 + pair[0].1.len() as u32 > pair[1].0 {
            return Err(Error::malformed("UF2 contains overlapping flash records"));
        }
    }
    Ok((records, Uf2Metadata { family_id: family }))
}

/// Inspects UF2 structure without applying the application-family whitelist.
///
/// This is suitable for inspecting device-provided `CURRENT.UF2`; it does not
/// authorize copying an image to any device.
///
/// # Errors
/// Returns an error for malformed, unsupported, or oversized UF2 contents.
pub fn inspect_uf2(bytes: &[u8]) -> Result<Uf2Metadata, Error> {
    uf2_records(bytes).map(|(_, metadata)| metadata)
}

/// The content rules to apply to a UF2 image, not a physical write authorization.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Uf2ImagePurpose {
    /// An application image subject to the supported nRF52 family whitelist.
    Application,
    /// An updater image constrained to the specified nRF52 chip layout.
    Bootloader(Nrf52Chip),
}

/// UF2 family facts obtained from content validation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ValidatedUf2Content {
    /// The family recorded in the UF2 itself.
    pub actual_family_id: u32,
    /// The application family to compare with known device chip information.
    pub expected_application_family_id: u32,
}

/// A UF2 image failed purpose-specific content validation.
#[derive(Debug)]
pub enum Uf2ContentError {
    /// Structural, family, or bootloader-layout validation failed.
    InvalidContent(Error),
    /// An application record exceeds the capacity of its recognized family.
    FlashCapacityExceeded {
        /// Start address of the offending record.
        address: u32,
        /// Recognized application family from the UF2; not a device identity.
        family_id: u32,
    },
}

impl fmt::Display for Uf2ContentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidContent(error) => fmt::Display::fmt(error, formatter),
            Self::FlashCapacityExceeded { address, .. } => write!(
                formatter,
                "UF2 address {address:#x} exceeds the selected nRF family flash capacity"
            ),
        }
    }
}

impl std::error::Error for Uf2ContentError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidContent(error) => Some(error),
            Self::FlashCapacityExceeded { .. } => None,
        }
    }
}

impl From<Uf2ContentError> for String {
    fn from(error: Uf2ContentError) -> Self {
        match error {
            Uf2ContentError::InvalidContent(error) => error.into(),
            error => error.to_string(),
        }
    }
}

impl From<Error> for Uf2ContentError {
    fn from(error: Error) -> Self {
        Self::InvalidContent(error)
    }
}

/// Validates UF2 content for the intended image purpose.
///
/// This checks format and nRF52 image layout only; it does not establish exact
/// board identity, firmware authenticity, or authorization to write a device.
///
/// # Errors
/// Returns an error if the image violates the rules for its purpose.
pub fn validate_uf2_content(
    bytes: &[u8],
    purpose: Uf2ImagePurpose,
) -> Result<ValidatedUf2Content, Uf2ContentError> {
    validate_uf2_content_inner(bytes, purpose)
}

fn validate_uf2_content_inner(
    bytes: &[u8],
    purpose: Uf2ImagePurpose,
) -> Result<ValidatedUf2Content, Uf2ContentError> {
    match purpose {
        Uf2ImagePurpose::Application => {
            let (_, metadata) = uf2_records(bytes)?;
            let family = metadata.family_id.ok_or_else(|| {
                Error::malformed("UF2 has no family ID; select a family-tagged nRF image")
            })?;
            let limit = match family {
                0xada5_2840 => 1024 * 1024,
                0x621e_937a | 0x1b57_745f => 512 * 1024,
                _ => {
                    return Err(Error::new(
                        ErrorKind::Unsupported,
                        format!(
                            "UF2 family {family:#010x} is not a supported nRF52 application family"
                        ),
                    )
                    .into());
                }
            };
            for block in bytes.as_chunks::<512>().0 {
                let address = u32le(block, 12);
                let size = u32le(block, 16);
                if address < 0x1000_1000 && address + size > limit {
                    return Err(Uf2ContentError::FlashCapacityExceeded {
                        address,
                        family_id: family,
                    });
                }
            }
            Ok(ValidatedUf2Content {
                actual_family_id: family,
                expected_application_family_id: family,
            })
        }
        Uf2ImagePurpose::Bootloader(chip) => {
            chip.validate_bootloader(bytes)?;
            Ok(ValidatedUf2Content {
                actual_family_id: 0xd663_823c,
                expected_application_family_id: chip.application_family(),
            })
        }
    }
}
fn parse_uf2(bytes: &[u8]) -> Result<FirmwareImage, Error> {
    let (records, _) = uf2_records(bytes)?;
    assemble(records)
}

fn parse_hex(bytes: &[u8]) -> Result<FirmwareImage, Error> {
    let text = std::str::from_utf8(bytes).map_err(|error| {
        Error::with_cause(
            ErrorKind::Malformed,
            "HEX is not ASCII text",
            ErrorCause::Utf8(error),
        )
    })?;
    let mut base = 0u32;
    let mut eof = false;
    let mut records = Vec::new();
    let mut decoded = [0u8; 260];
    for (line_index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let error =
            |reason: &str| Error::malformed(format!("HEX line {}: {reason}", line_index + 1));
        if eof {
            return Err(error("data after EOF"));
        }
        if !line.starts_with(':') || !line.is_ascii() || line.len() % 2 != 1 || line.len() > 521 {
            return Err(error("invalid record syntax"));
        }
        let raw = &mut decoded[..(line.len() - 1) / 2];
        for (pair, byte) in line.as_bytes()[1..]
            .as_chunks::<2>()
            .0
            .iter()
            .zip(raw.iter_mut())
        {
            let digit = |b: u8| (b as char).to_digit(16).map(|v| v as u8);
            *byte = digit(pair[0]).ok_or_else(|| error("invalid hex digit"))? * 16
                + digit(pair[1]).ok_or_else(|| error("invalid hex digit"))?;
        }
        if raw.len() < 5 || raw.len() != raw[0] as usize + 5 {
            return Err(error("record length mismatch"));
        }
        if raw.iter().fold(0u8, |sum, b| sum.wrapping_add(*b)) != 0 {
            return Err(Error::new(
                ErrorKind::Integrity,
                format!("HEX line {}: checksum mismatch", line_index + 1),
            ));
        }
        let offset = u16::from_be_bytes([raw[1], raw[2]]) as u32;
        let data = &raw[4..raw.len() - 1];
        match raw[3] {
            0 => {
                if offset + data.len() as u32 > 0x10000 {
                    return Err(error("record crosses address page"));
                }
                let address = base
                    .checked_add(offset)
                    .ok_or_else(|| error("address overflow"))?;
                address_range(address, data.len())?;
                if !data.is_empty() {
                    records.push((address, data.to_vec()));
                }
            }
            1 if offset == 0 && data.is_empty() => eof = true,
            2 | 4 if offset == 0 && data.len() == 2 => {
                base = (u16::from_be_bytes([data[0], data[1]]) as u32)
                    << if raw[3] == 2 { 4 } else { 16 };
            }
            3 | 5 if offset == 0 && data.len() == 4 => {}
            _ => {
                let kind = if raw[3] <= 5 {
                    ErrorKind::Malformed
                } else {
                    ErrorKind::Unsupported
                };
                return Err(Error::new(
                    kind,
                    format!(
                        "HEX line {}: unsupported record type or invalid control record",
                        line_index + 1
                    ),
                ));
            }
        }
    }
    if !eof {
        return Err(Error::malformed("HEX is missing its EOF record"));
    }
    assemble(
        records
            .iter()
            .map(|(address, data)| (*address, data.as_slice()))
            .collect(),
    )
}

fn assemble(mut records: Vec<(u32, &[u8])>) -> Result<FirmwareImage, Error> {
    records.sort_unstable_by_key(|(address, _)| *address);
    let mut base = None;
    let mut end = 0;
    for (address, data) in &mut records {
        let record_end = address_range(*address, data.len())?;
        if *address >= MAX_IMAGE_SIZE as u32 || record_end <= APP_FLOOR {
            *data = &[];
            continue;
        }
        let start = (*address).max(APP_FLOOR);
        if start < end {
            return Err(Error::malformed(
                "Firmware contains overlapping flash records",
            ));
        }
        *data = &data[(start - *address) as usize..];
        *address = start;
        base.get_or_insert(start);
        end = record_end;
    }
    let base = base.ok_or_else(|| Error::malformed("Firmware contains no application data"))?;
    let mut result = vec![0xff; (end - base) as usize];
    for (address, data) in records {
        if !data.is_empty() {
            result[(address - base) as usize..(address - base) as usize + data.len()]
                .copy_from_slice(data);
        }
    }
    Ok(image(result, base, false))
}

fn parse_mcuboot(bytes: &[u8]) -> Result<FirmwareImage, Error> {
    if bytes.len() < 32 || bytes.len() > MAX_IMAGE_SIZE || u32le(bytes, 0) != MCU_MAGIC {
        let kind = if bytes.len() > MAX_IMAGE_SIZE {
            ErrorKind::Limit
        } else {
            ErrorKind::Malformed
        };
        return Err(Error::new(
            kind,
            "BIN is not a bounded MCUboot update image",
        ));
    }
    let header = u16le(bytes, 8);
    let protected = u16le(bytes, 10);
    let payload = u32le(bytes, 12) as usize;
    let flags = u32le(bytes, 16);
    if header < 32 || !header.is_multiple_of(4) || payload == 0 || flags != 0 {
        let kind = if header < 32 || !header.is_multiple_of(4) || payload == 0 {
            ErrorKind::Malformed
        } else {
            ErrorKind::Unsupported
        };
        return Err(Error::new(
            kind,
            "MCUboot header has invalid size or unsupported encrypted/compressed/non-bootable flags",
        ));
    }
    let mut cursor = header
        .checked_add(payload)
        .filter(|end| *end <= bytes.len())
        .ok_or_else(|| Error::malformed("Truncated MCUboot payload"))?;
    if protected != 0 {
        let end = parse_tlv(bytes, cursor, 0x6908)?;
        if end - cursor != protected {
            return Err(Error::malformed("MCUboot protected TLV size mismatch"));
        }
        cursor = end;
    }
    // Signed and hash-only imgtool images both carry the ordinary TLV area.
    let hash_end = cursor;
    let tlv_end = parse_tlv(bytes, cursor, 0x6907)?;
    let mut entry = cursor + 4;
    let mut digest_found = false;
    while entry < tlv_end {
        let len = u16le(bytes, entry + 2);
        if u16le(bytes, entry) == 0x10 {
            if digest_found
                || len != 32
                || Sha256::digest(&bytes[..hash_end]).as_slice()
                    != &bytes[entry + 4..entry + 4 + len]
            {
                let kind = if digest_found || len != 32 {
                    ErrorKind::Malformed
                } else {
                    ErrorKind::Integrity
                };
                return Err(Error::new(
                    kind,
                    "MCUboot SHA-256 integrity TLV is duplicated, malformed, or does not match the image",
                ));
            }
            digest_found = true;
        }
        entry += 4 + len;
    }
    if !digest_found {
        return Err(Error::malformed(
            "MCUboot update image has no SHA-256 integrity TLV",
        ));
    }
    cursor = tlv_end;
    if bytes[cursor..].iter().any(|b| *b != 0xff) {
        return Err(Error::malformed(
            "MCUboot has unexpected data after its TLVs (select update BIN, not a full slot image)",
        ));
    }
    Ok(image(bytes.to_vec(), 0, true))
}
fn parse_tlv(bytes: &[u8], start: usize, magic: usize) -> Result<usize, Error> {
    if start + 4 > bytes.len() || u16le(bytes, start) != magic {
        return Err(Error::malformed("MCUboot TLV header missing or invalid"));
    }
    let size = u16le(bytes, start + 2);
    let end = start
        .checked_add(size)
        .filter(|end| size >= 4 && *end <= bytes.len())
        .ok_or_else(|| Error::malformed("Truncated MCUboot TLV area"))?;
    let mut cursor = start + 4;
    while cursor < end {
        if cursor + 4 > end {
            return Err(Error::malformed("Truncated MCUboot TLV entry"));
        }
        let len = u16le(bytes, cursor + 2);
        cursor = cursor
            .checked_add(4 + len)
            .filter(|next| *next <= end)
            .ok_or_else(|| Error::malformed("MCUboot TLV entry exceeds area"))?;
    }
    Ok(end)
}

/// Prepares an image for a device application base, borrowing unchanged data.
///
/// # Errors
/// Returns an error for an unsupported base or an unsafe image/vector layout.
pub fn prepare_for_target(
    source: &FirmwareImage,
    flash_base: u32,
) -> Result<Cow<'_, FirmwareImage>, Error> {
    prepare_for_target_inner(source, flash_base)
}

fn prepare_for_target_inner(
    source: &FirmwareImage,
    flash_base: u32,
) -> Result<Cow<'_, FirmwareImage>, Error> {
    if source.data.is_empty() || source.data.len() > MAX_IMAGE_SIZE {
        let kind = if source.data.is_empty() {
            ErrorKind::Malformed
        } else {
            ErrorKind::Limit
        };
        return Err(Error::new(kind, "Invalid firmware image size"));
    }
    if source.mcuboot {
        return Ok(Cow::Borrowed(source));
    }
    if flash_base >= MAX_IMAGE_SIZE as u32 {
        return Err(Error::new(
            ErrorKind::Unsupported,
            "Device reports an unsupported flash base",
        ));
    }
    if source.base_address >= flash_base {
        return Ok(Cow::Borrowed(source));
    }
    let offset = (flash_base - source.base_address) as usize;
    let vectors = source
        .data
        .get(offset..offset.saturating_add(8))
        .ok_or_else(|| Error::malformed("Firmware does not reach the device application base"))?;
    let sp = u32le(vectors, 0);
    let reset = u32le(vectors, 4);
    let image_end = source
        .base_address
        .checked_add(source.data.len() as u32)
        .ok_or_else(|| Error::malformed("Firmware address overflow"))?;
    if !(0x2000_0000..=0x2008_0000).contains(&sp)
        || !sp.is_multiple_of(4)
        || reset & 1 == 0
        || reset < flash_base
        || reset >= image_end
    {
        return Err(Error::malformed(format!(
            "No valid application vector table at device flash base {flash_base:#x}; refusing to guess or clip firmware"
        )));
    }
    Ok(Cow::Owned(image(
        source.data[offset..].to_vec(),
        flash_base,
        false,
    )))
}

#[cfg(test)]
mod tests {
    use super::{
        ErrorKind, MAX_CONTAINER_SIZE, image, inspect_uf2, parse_image, prepare_for_target,
    };
    use std::{borrow::Cow, error::Error as _, fmt::Write as _};
    pub(super) fn block(address: u32, number: u32, count: u32) -> Vec<u8> {
        let mut b = vec![0; 512];
        for (offset, value) in [
            (0, 0x0a324655u32),
            (4, 0x9e5d5157),
            (8, 0x2000),
            (12, address),
            (16, 256),
            (20, number),
            (24, count),
            (28, 0xada52840),
            (508, 0x0ab16f30),
        ] {
            b[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        b
    }
    fn hex_record(offset: u16, kind: u8, data: &[u8]) -> String {
        let count = u8::try_from(data.len()).unwrap();
        let mut record = format!(":{count:02X}{offset:04X}{kind:02X}");
        let [high, low] = offset.to_be_bytes();
        let mut sum = count
            .wrapping_add(high)
            .wrapping_add(low)
            .wrapping_add(kind);
        for byte in data {
            write!(record, "{byte:02X}").unwrap();
            sum = sum.wrapping_add(*byte);
        }
        writeln!(record, "{:02X}", sum.wrapping_neg()).unwrap();
        record
    }
    #[test]
    fn uf2_rejects_number_family_overlap_and_truncation() {
        let mut b = block(0x1000, 0, 2);
        b.extend(block(0x1100, 1, 2));
        assert_eq!(parse_image("a.uf2", &b).unwrap().data.len(), 512);
        b[512 + 28] ^= 1;
        assert!(inspect_uf2(&b).is_err());
        b[512 + 28] ^= 1;
        b[512 + 20..512 + 24].copy_from_slice(&0u32.to_le_bytes());
        assert!(inspect_uf2(&b).is_err());
        assert!(parse_image("a.uf2", &b[..1023]).is_err());
        let mut overlap = block(0x1000, 0, 2);
        overlap.extend(block(0x1000, 1, 2));
        assert!(inspect_uf2(&overlap).is_err());
    }
    #[test]
    fn hex_checks_checksum_eof_and_address_bounds() {
        assert_eq!(
            parse_image("a.hex", b":0410000001020304E2\n:00000001FF\n")
                .unwrap()
                .data,
            [1, 2, 3, 4]
        );
        for text in [
            ":0410000001020304E3\n:00000001FF",
            ":0410000001020304E2",
            ":00000001FF\n:00000001FF",
            ":02000004FFFFFC\n:0410000001020304E2\n:00000001FF",
        ] {
            assert!(parse_image("a.hex", text.as_bytes()).is_err());
        }
    }
    #[test]
    fn hex_accepts_maximum_data_record_and_rejects_checksum_and_oversize() {
        let payload: Vec<_> = (0..=254u8).collect();
        let record = hex_record(0x1000, 0, &payload);
        let text = format!("{record}:00000001FF\n");
        let parsed = parse_image("maximum.hex", text.as_bytes()).unwrap();
        assert_eq!(parsed.base_address, 0x1000);
        assert_eq!(parsed.data, payload);
        assert_eq!(parsed.crc32, crc32fast::hash(&payload));
        assert!(!parsed.mcuboot);

        let mut bad_checksum = record.clone().into_bytes();
        let checksum_digit = bad_checksum.len() - 2;
        bad_checksum[checksum_digit] = if bad_checksum[checksum_digit] == b'0' {
            b'1'
        } else {
            b'0'
        };
        bad_checksum.extend_from_slice(b":00000001FF\n");
        assert_eq!(
            parse_image("checksum.hex", &bad_checksum)
                .unwrap_err()
                .kind(),
            ErrorKind::Integrity
        );

        let mut oversized = record;
        oversized.insert_str(oversized.len() - 1, "00");
        oversized.push_str(":00000001FF\n");
        assert!(parse_image("oversized.hex", oversized.as_bytes()).is_err());
    }
    #[test]
    fn image_errors_expose_limits_and_the_real_text_decoding_source() {
        let oversized = vec![0; MAX_CONTAINER_SIZE + 1];
        assert_eq!(
            parse_image("image.hex", &oversized).unwrap_err().kind(),
            ErrorKind::Limit
        );
        let invalid_text = parse_image("image.hex", &[0xff]).unwrap_err();
        assert_eq!(invalid_text.kind(), ErrorKind::Malformed);
        assert!(
            invalid_text
                .source()
                .is_some_and(|cause| cause.is::<std::str::Utf8Error>())
        );
    }
    #[test]
    fn hex_rejects_malformed_syntax_digits_and_lengths() {
        for record in [
            ":",
            "00000001FF",
            ":00000001F",
            ":00000001FG",
            ":01000001FF",
        ] {
            let text = format!("{record}\n:00000001FF\n");
            assert!(
                parse_image("syntax.hex", text.as_bytes()).is_err(),
                "{record}"
            );
        }
    }
    #[test]
    fn hex_control_records_preserve_flash_gaps_and_ignore_uicr() {
        let text = [
            hex_record(0, 2, &[0x01, 0x00]),
            hex_record(0, 0, &[1, 2, 3, 4]),
            hex_record(0, 3, &[0, 0, 0x10, 0]),
            hex_record(0, 5, &[0, 0, 0x10, 1]),
            hex_record(0, 4, &[0x10, 0x00]),
            hex_record(0x1000, 0, &[9, 10, 11, 12]),
            hex_record(0, 4, &[0, 0]),
            hex_record(0x1008, 0, &[5, 6, 7, 8]),
            hex_record(0, 1, &[]),
        ]
        .concat();
        let parsed = parse_image("controls.hex", text.as_bytes()).unwrap();
        let expected = [1, 2, 3, 4, 0xff, 0xff, 0xff, 0xff, 5, 6, 7, 8];
        assert_eq!(parsed.base_address, 0x1000);
        assert_eq!(parsed.data, expected);
        assert_eq!(parsed.crc32, crc32fast::hash(&expected));
        assert!(!parsed.mcuboot);
    }
    #[test]
    fn hex_rejects_invalid_control_records() {
        let malformed: &[(u16, u8, &[u8])] = &[
            (1, 1, &[]),
            (0, 1, &[0]),
            (1, 2, &[0, 0]),
            (0, 2, &[0]),
            (1, 4, &[0, 0]),
            (0, 4, &[0, 0, 0]),
            (1, 3, &[0, 0, 0, 0]),
            (0, 3, &[0, 0, 0]),
            (1, 5, &[0, 0, 0, 0]),
            (0, 5, &[0, 0, 0]),
            (0, 6, &[]),
        ];
        for &(offset, kind, data) in malformed {
            let text = format!(
                ":0410000001020304E2\n{}:00000001FF\n",
                hex_record(offset, kind, data)
            );
            assert!(parse_image("invalid-control.hex", text.as_bytes()).is_err());
        }
    }
    #[test]
    fn hex_rejects_overlapping_and_page_crossing_records() {
        for offset in [0x1000, 0x1002] {
            let text = format!(
                ":0410000001020304E2\n{}:00000001FF\n",
                hex_record(offset, 0, &[5, 6])
            );
            assert!(parse_image("overlap.hex", text.as_bytes()).is_err());
        }
        let text = format!("{}:00000001FF\n", hex_record(0xfffe, 0, &[1, 2, 3]));
        assert!(parse_image("page.hex", text.as_bytes()).is_err());
    }
    #[test]
    fn ignores_uicr_without_sparse_allocation_and_fills_real_flash_gaps() {
        let mut bytes = block(0x1000, 0, 3);
        bytes.extend(block(0x1200, 1, 3));
        bytes.extend(block(0x10001000, 2, 3));
        let parsed = parse_image("image.uf2", &bytes).unwrap();
        assert_eq!(parsed.base_address, 0x1000);
        assert_eq!(parsed.data.len(), 768);
        assert!(parsed.data[256..512].iter().all(|byte| *byte == 0xff));
        let huge = block(0x0020_0000, 0, 1);
        assert!(parse_image("huge.uf2", &huge).is_err());
    }
    #[test]
    fn target_preparation_borrows_unchanged_application_images() {
        let source = parse_image("app.hex", b":0410000001020304E2\n:00000001FF\n").unwrap();
        for flash_base in [0x0800, 0x1000] {
            let prepared = prepare_for_target(&source, flash_base).unwrap();
            assert!(matches!(&prepared, Cow::Borrowed(_)));
            assert!(std::ptr::eq(prepared.as_ref(), &source));
            assert_eq!(prepared.data, [1, 2, 3, 4]);
            assert_eq!(prepared.base_address, 0x1000);
            assert_eq!(prepared.crc32, source.crc32);
            assert!(!prepared.mcuboot);
        }
    }
    #[test]
    fn target_clipping_owns_validated_bytes_and_recomputes_crc() {
        let mut data = vec![0xff; 512];
        data[256..260].copy_from_slice(&0x20010000u32.to_le_bytes());
        data[260..264].copy_from_slice(&0x1181u32.to_le_bytes());
        let source = image(data, 0x1000, false);
        let prepared = prepare_for_target(&source, 0x1100).unwrap();
        assert!(matches!(&prepared, Cow::Owned(_)));
        assert_eq!(prepared.base_address, 0x1100);
        assert_eq!(prepared.data, source.data[256..]);
        assert_eq!(prepared.data.len(), 256);
        assert_eq!(prepared.crc32, crc32fast::hash(&source.data[256..]));
        assert!(!prepared.mcuboot);
        assert!(prepare_for_target(&source, 0x1080).is_err());
    }
    #[test]
    fn target_clipping_rejects_invalid_vectors_and_missing_vector_bytes() {
        for (sp, reset) in [
            (0x1fff_fffcu32, 0x1181u32),
            (0x2008_0004, 0x1181),
            (0x2001_0001, 0x1181),
            (0x2001_0000, 0x1180),
            (0x2001_0000, 0x10ff),
            (0x2001_0000, 0x1201),
        ] {
            let mut data = vec![0xff; 512];
            data[256..260].copy_from_slice(&sp.to_le_bytes());
            data[260..264].copy_from_slice(&reset.to_le_bytes());
            assert!(prepare_for_target(&image(data, 0x1000, false), 0x1100).is_err());
        }
        let source = image(vec![0xff; 260], 0x1000, false);
        assert!(prepare_for_target(&source, 0x1100).is_err());
    }
    #[test]
    fn mcuboot_validates_header_and_tlv_bounds() {
        use sha2::{Digest, Sha256};
        let mut b = vec![0; 76];
        b[..4].copy_from_slice(&0x96f3b83du32.to_le_bytes());
        b[8..10].copy_from_slice(&32u16.to_le_bytes());
        b[12..16].copy_from_slice(&4u32.to_le_bytes());
        b[36..40].copy_from_slice(&[7, 0x69, 40, 0]);
        b[40..44].copy_from_slice(&[0x10, 0, 32, 0]);
        let digest = Sha256::digest(&b[..36]);
        b[44..].copy_from_slice(&digest);
        let source = parse_image("update.bin", &b).unwrap();
        assert!(source.mcuboot);
        let prepared = prepare_for_target(&source, 0x1100).unwrap();
        assert!(matches!(&prepared, Cow::Borrowed(_)));
        assert!(std::ptr::eq(prepared.as_ref(), &source));
        assert_eq!(prepared.data, b);
        assert_eq!(prepared.base_address, 0);
        assert_eq!(prepared.crc32, crc32fast::hash(&b));
        assert!(prepared.mcuboot);
        b[32] ^= 1;
        assert!(parse_image("update.bin", &b).is_err());
        b[32] ^= 1;
        b[42] = 33;
        assert!(parse_image("update.bin", &b).is_err());
        assert!(parse_image("update.bin", &[0; 32]).is_err());
    }
}

#[cfg(test)]
#[path = "uf2_content_tests.rs"]
mod uf2_content_tests;
