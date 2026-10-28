//! Nordic Secure DFU image preparation and package validation.
//!
//! These checks validate image content, not device identity or authenticity.

use crate::image::MAX_IMAGE_SIZE as MAX_IMAGE;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::io::{Cursor, Read};

const MAX_INIT: usize = 4096;

/// Parameters used to generate an application init packet for an Intel HEX image.
#[derive(Clone, Debug)]
pub struct Options {
    /// Hardware version advertised to the bootloader.
    pub hw_version: u32,
    /// Application version advertised to the bootloader.
    pub app_version: u32,
    /// Compatible SoftDevice identifiers encoded in the init packet.
    pub sd_req: Vec<u32>,
    /// Known bootloader application start. Secure DFU carries no HEX addresses.
    pub app_base: u32,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            hw_version: 52,
            app_version: 1,
            sd_req: vec![0],
            app_base: 0x1000,
        }
    }
}

/// The reason Secure DFU image or package preparation failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorKind {
    /// The input uses a format, feature, or target layout that is not supported.
    Unsupported,
    /// The input is structurally invalid or lacks required metadata.
    Malformed,
    /// The input exceeds a parser or image size limit.
    Limit,
    /// Content, sizes, or checksums disagree with the package metadata.
    Integrity,
    /// Reading or decompressing an archive member failed.
    Io,
}

/// An invalid or unsupported Secure DFU firmware image or package.
///
/// Nested parser and archive failures remain available through
/// [`std::error::Error::source`].
#[derive(Debug)]
pub struct Error {
    kind: ErrorKind,
    detail: ErrorDetail,
}

#[derive(Debug)]
enum ErrorDetail {
    Message(String),
    Source {
        context: Option<String>,
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

impl Error {
    /// Returns the category of this failure without interpreting its message.
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    fn message(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            detail: ErrorDetail::Message(message.into()),
        }
    }

    fn caused_by(
        kind: ErrorKind,
        context: Option<String>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            kind,
            detail: ErrorDetail::Source {
                context,
                source: Box::new(source),
            },
        }
    }

    fn zip(context: String, source: zip::result::ZipError) -> Self {
        use zip::result::ZipError;

        let kind = match &source {
            ZipError::Io(_) => ErrorKind::Io,
            ZipError::UnsupportedArchive(_)
            | ZipError::CompressionMethodNotSupported(_)
            | ZipError::InvalidPassword
            | ZipError::DecompressedSizeUnknown => ErrorKind::Unsupported,
            ZipError::DecompressedSizeLimitExceeded { .. } => ErrorKind::Limit,
            _ => ErrorKind::Malformed,
        };
        Self::caused_by(kind, Some(context), source)
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.detail {
            ErrorDetail::Message(message) => f.write_str(message),
            ErrorDetail::Source {
                context: Some(context),
                source,
            } => write!(f, "{context}: {source}"),
            ErrorDetail::Source {
                context: None,
                source,
            } => std::fmt::Display::fmt(source, f),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.detail {
            ErrorDetail::Message(_) => None,
            ErrorDetail::Source { source, .. } => Some(source.as_ref()),
        }
    }
}

impl From<crate::image::Error> for Error {
    fn from(source: crate::image::Error) -> Self {
        let kind = match source.kind() {
            crate::image::ErrorKind::Malformed => ErrorKind::Malformed,
            crate::image::ErrorKind::Unsupported => ErrorKind::Unsupported,
            crate::image::ErrorKind::Limit => ErrorKind::Limit,
            crate::image::ErrorKind::Integrity => ErrorKind::Integrity,
        };
        Self::caused_by(kind, None, source)
    }
}

impl From<crate::zip_envelope::Error> for Error {
    fn from(source: crate::zip_envelope::Error) -> Self {
        let kind = match source.kind() {
            crate::zip_envelope::ErrorKind::Malformed => ErrorKind::Malformed,
            crate::zip_envelope::ErrorKind::Unsupported => ErrorKind::Unsupported,
            crate::zip_envelope::ErrorKind::Limit => ErrorKind::Limit,
        };
        Self::caused_by(kind, None, source)
    }
}

impl From<Error> for String {
    fn from(error: Error) -> Self {
        match error {
            Error {
                detail: ErrorDetail::Message(message),
                ..
            } => message,
            error => error.to_string(),
        }
    }
}

/// A validated Secure DFU image and its accompanying init packet.
#[derive(Debug)]
pub struct Image {
    name: String,
    firmware: Vec<u8>,
    init: Vec<u8>,
}

impl Image {
    /// The source filename or ZIP member name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The bytes to write to the firmware data object.
    pub fn firmware(&self) -> &[u8] {
        &self.firmware
    }

    /// The exact command-object init packet.
    pub fn init(&self) -> &[u8] {
        &self.init
    }
}

fn varint(mut value: u32, out: &mut Vec<u8>) {
    while value >= 128 {
        out.push((value as u8 & 127) | 128);
        value >>= 7;
    }
    out.push(value as u8);
}
fn number(field: u32, value: u32, out: &mut Vec<u8>) {
    varint(field << 3, out);
    varint(value, out);
}
fn blob(field: u32, bytes: &[u8], out: &mut Vec<u8>) {
    varint((field << 3) | 2, out);
    varint(bytes.len() as u32, out);
    out.extend_from_slice(bytes);
}
fn generate_init(data: &[u8], options: &Options) -> Vec<u8> {
    let mut hash = Vec::with_capacity(36);
    number(1, 3, &mut hash);
    let mut digest = Sha256::digest(data);
    digest.reverse();
    blob(2, &digest, &mut hash);
    let mut init = Vec::with_capacity(80);
    number(1, options.app_version, &mut init);
    number(2, options.hw_version, &mut init);
    if !options.sd_req.is_empty() {
        let mut packed = Vec::with_capacity(options.sd_req.len() * 3);
        for &sd in &options.sd_req {
            varint(sd, &mut packed);
        }
        blob(3, &packed, &mut init);
    }
    number(4, 0, &mut init);
    number(7, data.len() as u32, &mut init);
    blob(8, &hash, &mut init);
    let mut command = Vec::with_capacity(init.len() + 4);
    number(1, 1, &mut command);
    blob(2, &init, &mut command);
    let mut packet = Vec::with_capacity(command.len() + 2);
    blob(1, &command, &mut packet);
    packet
}

// Read only the Secure DFU protobuf fields needed to reject Legacy packets,
// mismatched package members and malformed sizes before opening any serial port.
#[derive(Clone, Copy)]
enum Field<'a> {
    Number(u64),
    Bytes(&'a [u8]),
}
fn take_varint(bytes: &[u8], pos: &mut usize) -> Result<u64, Error> {
    let mut result = 0;
    for shift in (0..70).step_by(7) {
        let b = *bytes.get(*pos).ok_or_else(|| {
            Error::message(ErrorKind::Malformed, "Truncated Secure DFU init protobuf")
        })?;
        *pos += 1;
        if shift == 63 && b > 1 {
            return Err(Error::message(
                ErrorKind::Malformed,
                "Overflow in Secure DFU init protobuf",
            ));
        }
        result |= u64::from(b & 127) << shift;
        if b & 128 == 0 {
            return Ok(result);
        }
    }
    Err(Error::message(
        ErrorKind::Malformed,
        "Invalid Secure DFU init protobuf varint",
    ))
}
fn fields(bytes: &[u8]) -> Result<Vec<(u64, Field<'_>)>, Error> {
    let mut pos = 0;
    let mut result = Vec::new();
    while pos < bytes.len() {
        let tag = take_varint(bytes, &mut pos)?;
        if tag >> 3 == 0 {
            return Err(Error::message(
                ErrorKind::Malformed,
                "Not a Secure DFU init packet (Legacy BLE ZIP is not supported here)",
            ));
        }
        let value = match tag & 7 {
            0 => Field::Number(take_varint(bytes, &mut pos)?),
            2 => {
                let len = usize::try_from(take_varint(bytes, &mut pos)?)
                    .map_err(|_| Error::message(ErrorKind::Malformed, "Invalid protobuf length"))?;
                let end = pos
                    .checked_add(len)
                    .filter(|&e| e <= bytes.len())
                    .ok_or_else(|| {
                        Error::message(
                            ErrorKind::Malformed,
                            "Truncated Secure DFU init protobuf field",
                        )
                    })?;
                let data = &bytes[pos..end];
                pos = end;
                Field::Bytes(data)
            }
            _ => {
                return Err(Error::message(
                    ErrorKind::Unsupported,
                    "Unsupported Secure DFU protobuf wire type",
                ));
            }
        };
        result.push((tag >> 3, value));
    }
    Ok(result)
}
fn field<'a>(fields: &[(u64, Field<'a>)], key: u64) -> Result<Option<Field<'a>>, Error> {
    let mut matches = fields.iter().filter(|(k, _)| *k == key);
    let value = matches.next().map(|(_, v)| *v);
    if matches.next().is_some() {
        return Err(Error::message(
            ErrorKind::Malformed,
            format!("Duplicate Secure DFU init field {key}"),
        ));
    }
    Ok(value)
}
fn bytes_field<'a>(f: &[(u64, Field<'a>)], key: u64) -> Result<&'a [u8], Error> {
    match field(f, key)? {
        Some(Field::Bytes(v)) => Ok(v),
        _ => Err(Error::message(
            ErrorKind::Malformed,
            format!("Missing Secure DFU init message {key}"),
        )),
    }
}
fn num_field(f: &[(u64, Field<'_>)], key: u64, default: u64) -> Result<u64, Error> {
    match field(f, key)? {
        Some(Field::Number(v)) => Ok(v),
        None => Ok(default),
        _ => Err(Error::message(
            ErrorKind::Malformed,
            format!("Invalid Secure DFU init integer {key}"),
        )),
    }
}
fn validate_init(init: &[u8], firmware: &[u8], kind: &str) -> Result<(), Error> {
    let root = fields(init)?;
    let command = match (field(&root, 1)?, field(&root, 2)?) {
        (Some(Field::Bytes(c)), None) => c,
        (None, Some(Field::Bytes(s))) => {
            let signed = fields(s)?;
            bytes_field(&signed, 1)?
        }
        _ => {
            return Err(Error::message(
                ErrorKind::Malformed,
                "ZIP requires a Nordic Secure DFU .dat packet, not Legacy BLE DFU",
            ));
        }
    };
    let command = fields(command)?;
    if num_field(&command, 1, 0)? != 1 {
        return Err(Error::message(
            ErrorKind::Unsupported,
            "Secure DFU packet is not an INIT command",
        ));
    }
    let init = fields(bytes_field(&command, 2)?)?;
    let expected_type = match kind {
        "application" => 0,
        "softdevice" => 1,
        "bootloader" => 2,
        "softdevice_bootloader" => 3,
        _ => unreachable!(),
    };
    if num_field(&init, 4, 0)? != expected_type {
        return Err(Error::message(
            ErrorKind::Integrity,
            format!("Secure DFU init type does not match {kind}"),
        ));
    }
    let size = num_field(&init, 5, 0)?
        .checked_add(num_field(&init, 6, 0)?)
        .and_then(|v| v.checked_add(num_field(&init, 7, 0).ok()?))
        .ok_or_else(|| Error::message(ErrorKind::Malformed, "Secure DFU image size overflow"))?;
    if size != firmware.len() as u64 {
        return Err(Error::message(
            ErrorKind::Integrity,
            "Secure DFU .dat image size does not match .bin",
        ));
    }
    let hash = fields(bytes_field(&init, 8)?)?;
    let actual = bytes_field(&hash, 2)?;
    let valid = match num_field(&hash, 1, 0)? {
        1 => actual == crc32fast::hash(firmware).to_le_bytes(),
        3 => {
            let mut expected = Sha256::digest(firmware);
            expected.reverse();
            actual == expected.as_slice()
        }
        _ => {
            return Err(Error::message(
                ErrorKind::Unsupported,
                "Secure DFU package must contain a CRC32 or SHA256 image hash",
            ));
        }
    };
    if !valid {
        return Err(Error::message(
            ErrorKind::Integrity,
            "Secure DFU .dat hash does not match .bin",
        ));
    }
    Ok(())
}
fn safe_member(name: &str) -> bool {
    !name.is_empty()
        && !name.contains('\\')
        && !name.contains(':')
        && !name.starts_with('/')
        && !name.split('/').any(|p| p == ".." || p == ".")
}
fn zip_member(
    archive: &mut zip::ZipArchive<Cursor<&[u8]>>,
    name: &str,
    limit: usize,
) -> Result<Vec<u8>, Error> {
    if !safe_member(name) {
        return Err(Error::message(
            ErrorKind::Malformed,
            "Unsafe DFU ZIP member path",
        ));
    }
    let mut member = archive
        .by_name(name)
        .map_err(|e| Error::zip(format!("Missing ZIP member {name}"), e))?;
    if member.is_dir() || member.size() == 0 {
        return Err(Error::message(
            ErrorKind::Malformed,
            format!("DFU ZIP member {name} has invalid size"),
        ));
    }
    if member.size() > limit as u64 {
        return Err(Error::message(
            ErrorKind::Limit,
            format!("DFU ZIP member {name} has invalid size"),
        ));
    }
    let size = member.size();
    let mut bytes = Vec::with_capacity(size as usize);
    (&mut member)
        .take(size + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| {
            Error::caused_by(
                ErrorKind::Io,
                Some(format!("Cannot read DFU ZIP member {name}")),
                e,
            )
        })?;
    if bytes.len() as u64 != size {
        return Err(Error::message(
            ErrorKind::Integrity,
            format!("DFU ZIP member {name} has an inconsistent size"),
        ));
    }
    Ok(bytes)
}

/// Parses Intel HEX or a Nordic Secure DFU ZIP into images in transfer order.
///
/// HEX images must start at the configured application base. ZIP system images
/// precede the application. Parsing validates contents but does not authenticate
/// firmware or authorize writing it to a device.
///
/// # Errors
/// Returns an error if the input format, image, init packet, or ZIP is invalid.
/// [`Error::kind`] classifies the failure, and wrapped parser errors are available
/// through [`std::error::Error::source`].
pub fn parse_images(name: &str, bytes: &[u8], options: &Options) -> Result<Vec<Image>, Error> {
    let extension = std::path::Path::new(name)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    if extension.eq_ignore_ascii_case("hex") {
        let image = crate::image::parse_image(name, bytes)?;
        if image.data.is_empty() {
            return Err(Error::message(
                ErrorKind::Malformed,
                "Invalid OpenDFU HEX image size",
            ));
        }
        if image.data.len() > MAX_IMAGE {
            return Err(Error::message(
                ErrorKind::Limit,
                "Invalid OpenDFU HEX image size",
            ));
        }
        if image.base_address != options.app_base {
            return Err(Error::message(
                ErrorKind::Unsupported,
                format!(
                    "HEX application begins at {:#x}, but this bootloader is configured for {:#x}; refusing to relocate an addressless Secure DFU image",
                    image.base_address, options.app_base
                ),
            ));
        }
        return Ok(vec![Image {
            name: name.into(),
            init: generate_init(&image.data, options),
            firmware: image.data,
        }]);
    }
    if !extension.eq_ignore_ascii_case("zip") {
        return Err(Error::message(
            ErrorKind::Unsupported,
            "OpenDFU requires Intel HEX or a Nordic Secure DFU ZIP",
        ));
    }
    if bytes.len() > 16 * 1024 * 1024 {
        return Err(Error::message(
            ErrorKind::Limit,
            "DFU ZIP exceeds 16 MiB limit",
        ));
    }
    let mut archive = crate::zip_envelope::open(
        bytes,
        crate::zip_envelope::Limits {
            bytes: 16 * 1024 * 1024,
            members: 128,
            directory: 4 * 1024 * 1024,
        },
    )?;
    let mut seen = HashSet::new();
    for i in 0..archive.len() {
        let member = archive
            .by_index(i)
            .map_err(|e| Error::zip("Invalid DFU ZIP member".into(), e))?;
        let name = member
            .name()
            .map_err(|e| Error::zip("Cannot decode DFU ZIP member name".into(), e))?;
        if !safe_member(&name) || !seen.insert(name.into_owned()) {
            return Err(Error::message(
                ErrorKind::Malformed,
                "Unsafe or duplicate DFU ZIP member",
            ));
        }
    }
    let manifest = zip_member(&mut archive, "manifest.json", 64 * 1024)?;
    let manifest: serde_json::Value = serde_json::from_slice(&manifest).map_err(|e| {
        Error::caused_by(ErrorKind::Malformed, Some("Invalid DFU manifest".into()), e)
    })?;
    let entries = manifest
        .get("manifest")
        .unwrap_or(&manifest)
        .as_object()
        .ok_or_else(|| Error::message(ErrorKind::Malformed, "DFU manifest must be an object"))?;
    if entries.contains_key("softdevice_bootloader")
        && (entries.contains_key("softdevice") || entries.contains_key("bootloader"))
    {
        return Err(Error::message(
            ErrorKind::Malformed,
            "DFU ZIP contains conflicting combined and separate system images",
        ));
    }
    let mut images = Vec::new();
    // System updates must precede an application which depends on the new SD/BL.
    for kind in [
        "softdevice_bootloader",
        "softdevice",
        "bootloader",
        "application",
    ] {
        if let Some(entry) = entries.get(kind) {
            let bin = entry
                .get("bin_file")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    Error::message(ErrorKind::Malformed, "DFU manifest is missing bin_file")
                })?;
            let dat = entry
                .get("dat_file")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    Error::message(ErrorKind::Malformed, "DFU manifest is missing dat_file")
                })?;
            let firmware = zip_member(&mut archive, bin, MAX_IMAGE)?;
            let init = zip_member(&mut archive, dat, MAX_INIT)?;
            validate_init(&init, &firmware, kind)?;
            images.push(Image {
                name: bin.into(),
                firmware,
                init,
            });
        }
    }
    if images.is_empty() {
        return Err(Error::message(
            ErrorKind::Malformed,
            "No Secure DFU firmware in ZIP",
        ));
    }
    Ok(images)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error as _;
    use std::io::{Cursor, Write};

    #[test]
    fn secure_init_rejects_legacy_wrong_hash_and_wrong_manifest_kind() {
        let bytes = vec![4; 100];
        let init = generate_init(&bytes, &Options::default());
        assert!(validate_init(&init, &bytes, "application").is_ok());
        assert_eq!(
            validate_init(&[52, 0, 0, 0, 1, 0], &bytes, "application")
                .unwrap_err()
                .kind(),
            ErrorKind::Unsupported
        );
        for error in [
            validate_init(&init, &[5; 100], "application").unwrap_err(),
            validate_init(&init, &bytes, "bootloader").unwrap_err(),
            validate_init(&init, &[4; 99], "application").unwrap_err(),
        ] {
            assert_eq!(error.kind(), ErrorKind::Integrity);
        }
    }

    fn hex_record(address: u16, bytes: &[u8]) -> String {
        let mut record = vec![bytes.len() as u8, (address >> 8) as u8, address as u8, 0];
        record.extend_from_slice(bytes);
        let checksum = 0u8.wrapping_sub(record.iter().fold(0u8, |sum, &b| sum.wrapping_add(b)));
        record.push(checksum);
        let encoded: String = record.iter().map(|b| format!("{b:02X}")).collect();
        format!(":{encoded}\n")
    }

    #[test]
    fn hex_drops_mbr_and_never_relocates_an_unknown_application_base() {
        let app = [0x00, 0x10, 0x00, 0x20, 0x09, 0x10, 0x00, 0x00];
        let mixed = format!(
            "{}{}:00000001FF\n",
            hex_record(0, &[1; 16]),
            hex_record(0x1000, &app)
        );
        let images = parse_images("holyiot.hex", mixed.as_bytes(), &Options::default()).unwrap();
        assert_eq!(images[0].firmware, app);
        assert_eq!(
            images[0].init(),
            generate_init(&app, &Options::default()).as_slice()
        );
        let crossing = format!(
            "{}:00000001FF\n",
            hex_record(0x0ffc, &[1, 2, 3, 4, 5, 6, 7, 8])
        );
        assert_eq!(
            parse_images("crossing.hex", crossing.as_bytes(), &Options::default()).unwrap()[0]
                .firmware,
            [5, 6, 7, 8]
        );
        let higher = format!("{}:00000001FF\n", hex_record(0x2000, &app));
        assert!(parse_images("other-base.hex", higher.as_bytes(), &Options::default()).is_err());
        let known_other_bootloader = Options {
            app_base: 0x2000,
            ..Options::default()
        };
        assert_eq!(
            parse_images("known-base.hex", higher.as_bytes(), &known_other_bootloader).unwrap()[0]
                .firmware,
            app
        );
    }

    #[test]
    fn hex_init_preserves_configured_versions_and_softdevice_requirements() {
        let firmware = [0, 1, 2, 3, 4, 5, 6, 7];
        let hex = format!("{}:00000001FF\n", hex_record(0x1000, &firmware));
        let options = Options {
            hw_version: 123,
            app_version: 0x10203,
            sd_req: vec![0, 0x1001],
            ..Options::default()
        };
        let images = parse_images("application.hex", hex.as_bytes(), &options).unwrap();
        let image = &images[0];
        assert_eq!(image.name(), "application.hex");
        assert_eq!(image.firmware(), firmware.as_slice());
        let root = fields(image.init()).unwrap();
        let command = fields(bytes_field(&root, 1).unwrap()).unwrap();
        let init = fields(bytes_field(&command, 2).unwrap()).unwrap();
        assert_eq!(
            num_field(&init, 1, 0).unwrap(),
            u64::from(options.app_version)
        );
        assert_eq!(
            num_field(&init, 2, 0).unwrap(),
            u64::from(options.hw_version)
        );
        assert_eq!(bytes_field(&init, 3).unwrap(), &[0, 0x81, 0x20][..]);
        assert_eq!(num_field(&init, 7, 0).unwrap(), firmware.len() as u64);
        assert!(validate_init(image.init(), image.firmware(), "application").is_ok());
    }

    #[test]
    fn init_packet_matches_nordic_protobuf_sha256_vector() {
        // SHA256("abc"), reversed per Nordic dfu-cc.proto; hw52/fw1/sd0.
        let expected = [
            0x0a, 0x35, 0x08, 0x01, 0x12, 0x31, 0x08, 0x01, 0x10, 0x34, 0x1a, 0x01, 0x00, 0x20,
            0x00, 0x38, 0x03, 0x42, 0x24, 0x08, 0x03, 0x12, 0x20, 0xad, 0x15, 0x00, 0xf2, 0x61,
            0xff, 0x10, 0xb4, 0x9c, 0x7a, 0x17, 0x96, 0xa3, 0x61, 0x03, 0xb0, 0x23, 0x22, 0xae,
            0x5d, 0xde, 0x40, 0x41, 0x41, 0xea, 0xcf, 0x01, 0x8f, 0xbf, 0x16, 0x78, 0xba,
        ];
        assert_eq!(generate_init(b"abc", &Options::default()), expected);
    }

    fn package(bin: &[u8], dat: &[u8], bin_name: &str) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        writer.start_file("manifest.json", options).unwrap();
        let manifest = serde_json::json!({"manifest": {"application": {"bin_file": bin_name, "dat_file": "app.dat"}}});
        writer.write_all(manifest.to_string().as_bytes()).unwrap();
        writer.start_file(bin_name, options).unwrap();
        writer.write_all(bin).unwrap();
        writer.start_file("app.dat", options).unwrap();
        writer.write_all(dat).unwrap();
        writer.finish().unwrap().into_inner()
    }

    fn manifest_package(manifest: &[u8]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        writer.start_file("manifest.json", options).unwrap();
        writer.write_all(manifest).unwrap();
        writer.finish().unwrap().into_inner()
    }

    fn system_package(kinds: &[(&str, u8)]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        let mut manifest = serde_json::Map::new();
        for &(kind, image_type) in kinds {
            let bin_name = format!("{kind}.bin");
            let dat_name = format!("{kind}.dat");
            let firmware = kind.as_bytes();
            // The current validator sums all three size fields for every image
            // type, rather than requiring a type-specific size field.
            let mut init = generate_init(firmware, &Options::default());
            let type_field = init
                .windows(2)
                .position(|field| field == &[0x20, 0][..])
                .unwrap();
            init[type_field + 1] = image_type;
            writer.start_file(&bin_name, options).unwrap();
            writer.write_all(firmware).unwrap();
            writer.start_file(&dat_name, options).unwrap();
            writer.write_all(&init).unwrap();
            manifest.insert(
                kind.into(),
                serde_json::json!({"bin_file": bin_name, "dat_file": dat_name}),
            );
        }
        writer.start_file("manifest.json", options).unwrap();
        writer
            .write_all(
                serde_json::json!({"manifest": manifest})
                    .to_string()
                    .as_bytes(),
            )
            .unwrap();
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn system_images_precede_application_and_keep_their_exact_payloads() {
        let options = Options::default();
        let package = system_package(&[("application", 0), ("bootloader", 2), ("softdevice", 1)]);
        let images = parse_images("firmware.zip", &package, &options).unwrap();
        assert_eq!(
            images.iter().map(Image::name).collect::<Vec<_>>(),
            ["softdevice.bin", "bootloader.bin", "application.bin"]
        );
        for (image, kind) in images
            .iter()
            .zip(["softdevice", "bootloader", "application"])
        {
            assert_eq!(image.firmware(), kind.as_bytes());
            assert!(validate_init(image.init(), image.firmware(), kind).is_ok());
        }

        let package = system_package(&[("application", 0), ("softdevice_bootloader", 3)]);
        let images = parse_images("firmware.zip", &package, &options).unwrap();
        assert_eq!(
            images.iter().map(Image::name).collect::<Vec<_>>(),
            ["softdevice_bootloader.bin", "application.bin"]
        );
        let conflict = system_package(&[("softdevice_bootloader", 3), ("bootloader", 2)]);
        assert!(parse_images("firmware.zip", &conflict, &options).is_err());
    }

    #[test]
    fn unsupported_and_malformed_inputs_are_classified() {
        let options = Options::default();
        assert_eq!(
            parse_images("firmware.ihex", b":00000001FF\n", &options)
                .unwrap_err()
                .kind(),
            ErrorKind::Unsupported
        );
        let empty = parse_images("empty.hex", b"", &options).unwrap_err();
        assert_eq!(empty.kind(), ErrorKind::Malformed);
        assert!(empty.source().unwrap().is::<crate::image::Error>());

        let bad = parse_images("bad.zip", b"not-a-zip", &options).unwrap_err();
        assert_eq!(bad.kind(), ErrorKind::Malformed);
        assert!(bad.source().unwrap().is::<crate::zip_envelope::Error>());
    }

    #[test]
    fn nested_image_and_envelope_kinds_are_preserved() {
        let options = Options::default();
        let bad_hex = b":0110000000F0\n:00000001FF\n";
        let error = parse_images("bad.hex", bad_hex, &options).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Integrity);
        let image_error = error
            .source()
            .unwrap()
            .downcast_ref::<crate::image::Error>()
            .unwrap();
        assert_eq!(image_error.kind(), crate::image::ErrorKind::Integrity);

        let error = parse_images("bad-encoding.hex", &[0xff], &options).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Malformed);
        let image_error = error
            .source()
            .unwrap()
            .downcast_ref::<crate::image::Error>()
            .unwrap();
        assert!(image_error.source().unwrap().is::<std::str::Utf8Error>());

        let mut bytes = manifest_package(b"{}");
        let end = bytes.len() - 22;
        bytes[end + 8..end + 10].copy_from_slice(&129u16.to_le_bytes());
        bytes[end + 10..end + 12].copy_from_slice(&129u16.to_le_bytes());
        let error = parse_images("too-many.zip", &bytes, &options).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Limit);
        let envelope = error
            .source()
            .unwrap()
            .downcast_ref::<crate::zip_envelope::Error>()
            .unwrap();
        assert_eq!(envelope.kind(), crate::zip_envelope::ErrorKind::Limit);
    }

    #[test]
    fn manifest_and_member_failures_retain_native_sources() {
        let options = Options::default();
        let malformed_json = manifest_package(br#"{"manifest":"#);
        let error = parse_images("invalid-json.zip", &malformed_json, &options).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Malformed);
        assert!(error.source().unwrap().is::<serde_json::Error>());

        let missing = manifest_package(
            br#"{"manifest":{"application":{"bin_file":"missing.bin","dat_file":"app.dat"}}}"#,
        );
        let error = parse_images("missing.zip", &missing, &options).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Malformed);
        assert!(matches!(
            error
                .source()
                .unwrap()
                .downcast_ref::<zip::result::ZipError>(),
            Some(zip::result::ZipError::FileNotFound)
        ));

        let mut corrupt = manifest_package(b"{}");
        let name_len = u16::from_le_bytes(corrupt[26..28].try_into().unwrap()) as usize;
        let extra_len = u16::from_le_bytes(corrupt[28..30].try_into().unwrap()) as usize;
        corrupt[30 + name_len + extra_len] ^= 1;
        let error = parse_images("bad-crc.zip", &corrupt, &options).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Io);
        let io_error = error
            .source()
            .unwrap()
            .downcast_ref::<std::io::Error>()
            .unwrap();
        assert_eq!(io_error.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn zip_decoder_failures_are_structured_and_chain_through_the_envelope() {
        let options = Options::default();
        let mut unsupported = manifest_package(b"{}");
        let end = unsupported.len() - 22;
        let central =
            u32::from_le_bytes(unsupported[end + 16..end + 20].try_into().unwrap()) as usize;
        unsupported[8..10].copy_from_slice(&u16::MAX.to_le_bytes());
        unsupported[central + 10..central + 12].copy_from_slice(&u16::MAX.to_le_bytes());
        let error = parse_images("unsupported.zip", &unsupported, &options).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Unsupported);
        assert!(matches!(
            error
                .source()
                .unwrap()
                .downcast_ref::<zip::result::ZipError>(),
            Some(zip::result::ZipError::CompressionMethodNotSupported(
                u16::MAX
            ))
        ));

        // The envelope accepts the framed extra field; the ZIP reader must
        // reject the truncated Unicode path data and retain its I/O cause.
        let mut truncated = manifest_package(b"{}");
        let end = truncated.len() - 22;
        let central =
            u32::from_le_bytes(truncated[end + 16..end + 20].try_into().unwrap()) as usize;
        let directory_size = u32::from_le_bytes(truncated[end + 12..end + 16].try_into().unwrap());
        let name_len =
            u16::from_le_bytes(truncated[central + 28..central + 30].try_into().unwrap()) as usize;
        let extra_len =
            u16::from_le_bytes(truncated[central + 30..central + 32].try_into().unwrap());
        let extra_end = central + 46 + name_len + usize::from(extra_len);
        drop(truncated.splice(extra_end..extra_end, [0x75, 0x70, 0, 0]));
        truncated[central + 30..central + 32].copy_from_slice(&(extra_len + 4).to_le_bytes());
        let end = end + 4;
        truncated[end + 12..end + 16].copy_from_slice(&(directory_size + 4).to_le_bytes());

        let error = parse_images("truncated-extra.zip", &truncated, &options).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Malformed);
        let envelope = error
            .source()
            .unwrap()
            .downcast_ref::<crate::zip_envelope::Error>()
            .unwrap();
        let zip = envelope
            .source()
            .unwrap()
            .downcast_ref::<zip::result::ZipError>()
            .unwrap();
        let io = zip
            .source()
            .unwrap()
            .downcast_ref::<std::io::Error>()
            .unwrap();
        assert_eq!(io.kind(), std::io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn expanded_member_limit_and_init_integrity_are_distinct() {
        let options = Options::default();
        let bin = [1, 2, 3, 4];
        let oversized = package(&bin, &[0; MAX_INIT + 1], "app.bin");
        let error = parse_images("oversized.zip", &oversized, &options).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Limit);

        let init = generate_init(&[4, 3, 2, 1], &options);
        let error =
            parse_images("wrong-hash.zip", &package(&bin, &init, "app.bin"), &options).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Integrity);
    }

    #[test]
    fn secure_zip_extracts_real_members_and_rejects_traversal_or_legacy_init() {
        let bin = vec![0xdb; 128];
        let options = Options::default();
        let init = generate_init(&bin, &options);
        let bytes = package(&bin, &init, "app.bin");
        let parsed = parse_images("firmware.zip", &bytes, &options).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].firmware, bin);
        assert_eq!(parsed[0].init, init);
        assert!(
            parse_images(
                "legacy.zip",
                &package(&bin, &[52, 0, 0, 0, 1, 0], "app.bin"),
                &options
            )
            .is_err()
        );
        assert!(parse_images("unsafe.zip", &package(&bin, &init, "../app.bin"), &options).is_err());
    }

    #[test]
    fn secure_zip_rejects_unsafe_envelopes_and_false_member_sizes() {
        let options = Options::default();
        let bin = [1, 2, 3, 4];
        let bytes = package(&bin, &generate_init(&bin, &options), "app.bin");
        let end = bytes.len() - 22;
        for (field, value) in [(8, 129u16), (10, 129), (4, 1), (6, 1), (10, u16::MAX)] {
            let mut malformed = bytes.clone();
            malformed[end + field..end + field + 2].copy_from_slice(&value.to_le_bytes());
            assert!(parse_images("secure.zip", &malformed, &options).is_err());
        }
        let central = u32::from_le_bytes(bytes[end + 16..end + 20].try_into().unwrap()) as usize;
        let local =
            u32::from_le_bytes(bytes[central + 42..central + 46].try_into().unwrap()) as usize;
        let size = u32::from_le_bytes(bytes[central + 24..central + 28].try_into().unwrap());
        for declared in [0, size - 1, size + 1] {
            let mut malformed = bytes.clone();
            malformed[local + 22..local + 26].copy_from_slice(&declared.to_le_bytes());
            malformed[central + 24..central + 28].copy_from_slice(&declared.to_le_bytes());
            assert!(parse_images("secure.zip", &malformed, &options).is_err());
        }
    }
}
