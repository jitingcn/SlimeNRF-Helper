//! Parsing application-only Adafruit/Nordic Legacy DFU packages.

use std::{
    collections::HashSet,
    fmt,
    io::{self, Cursor, Read},
};

use serde_json::Value;

const MAX_IMAGE: usize = 1024 * 1024;
const MAX_ZIP: usize = 4 * 1024 * 1024;

/// An owned Legacy DFU application and its unchanged init packet.
#[derive(Debug)]
pub struct Package {
    image: Vec<u8>,
    init: Vec<u8>,
}

impl Package {
    /// Borrows the application image and init packet without copying them.
    pub fn payloads(&self) -> (&[u8], &[u8]) {
        (&self.image, &self.init)
    }
}

/// The reason a Legacy DFU package could not be parsed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorKind {
    /// The package uses a format or feature this parser does not support.
    Unsupported,
    /// The package is structurally invalid or unsafe.
    Malformed,
    /// A configured package or member size limit was exceeded.
    Limit,
    /// Declared and observed package contents do not agree.
    Integrity,
    /// Reading a ZIP member failed, including decompression or checksum errors.
    Io,
}

/// A Legacy DFU package could not be parsed or is not supported.
#[derive(Debug)]
pub struct Error {
    kind: ErrorKind,
    detail: ErrorDetail,
}

#[derive(Debug)]
enum ErrorDetail {
    Message(&'static str),
    UnsafeMember(String),
    DuplicateMember(String),
    MemberSize {
        name: String,
        limit: usize,
    },
    InconsistentSize(String),
    Envelope(crate::zip_envelope::Error),
    ZipMember {
        name: String,
        source: zip::result::ZipError,
    },
    MemberRead {
        name: String,
        source: io::Error,
    },
    ZipIndex(zip::result::ZipError),
    ZipName(zip::result::ZipError),
    Manifest(serde_json::Error),
}

impl Error {
    /// Returns the category of the failure without inspecting its diagnostic text.
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    fn new(kind: ErrorKind, detail: ErrorDetail) -> Self {
        Self { kind, detail }
    }

    fn message(kind: ErrorKind, message: &'static str) -> Self {
        Self::new(kind, ErrorDetail::Message(message))
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.detail {
            ErrorDetail::Message(message) => f.write_str(message),
            ErrorDetail::UnsafeMember(name) => write!(f, "Unsafe DFU ZIP member: {name:?}"),
            ErrorDetail::DuplicateMember(name) => write!(f, "Duplicate DFU ZIP member: {name}"),
            ErrorDetail::MemberSize { name, limit } => {
                write!(f, "DFU ZIP {name} is empty or exceeds {limit} bytes")
            }
            ErrorDetail::InconsistentSize(name) => {
                write!(f, "DFU ZIP {name} has an inconsistent size")
            }
            ErrorDetail::Envelope(source) => fmt::Display::fmt(source, f),
            ErrorDetail::ZipMember { name, source } => write!(f, "DFU ZIP {name}: {source}"),
            ErrorDetail::MemberRead { name, source } => write!(f, "DFU ZIP {name}: {source}"),
            ErrorDetail::ZipIndex(source) => write!(f, "Invalid DFU ZIP: {source}"),
            ErrorDetail::ZipName(source) => {
                write!(f, "Cannot decode DFU ZIP member name: {source}")
            }
            ErrorDetail::Manifest(source) => write!(f, "Invalid DFU manifest: {source}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.detail {
            ErrorDetail::Envelope(source) => Some(source),
            ErrorDetail::ZipMember { source, .. }
            | ErrorDetail::ZipIndex(source)
            | ErrorDetail::ZipName(source) => Some(source),
            ErrorDetail::MemberRead { source, .. } => Some(source),
            ErrorDetail::Manifest(source) => Some(source),
            _ => None,
        }
    }
}

impl From<Error> for String {
    fn from(error: Error) -> Self {
        error.to_string()
    }
}

fn zip_error_kind(error: &zip::result::ZipError) -> ErrorKind {
    use zip::result::ZipError;

    match error {
        ZipError::Io(_) => ErrorKind::Io,
        ZipError::UnsupportedArchive(_)
        | ZipError::InvalidPassword
        | ZipError::CompressionMethodNotSupported(_)
        | ZipError::DecompressedSizeUnknown => ErrorKind::Unsupported,
        ZipError::DecompressedSizeLimitExceeded { .. } => ErrorKind::Limit,
        _ => ErrorKind::Malformed,
    }
}

fn safe_member(name: &str) -> Result<(), Error> {
    if name.is_empty()
        || name.contains(['\\', ':', '\0'])
        || name
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == "..")
    {
        return Err(Error::new(
            ErrorKind::Malformed,
            ErrorDetail::UnsafeMember(name.to_owned()),
        ));
    }
    Ok(())
}

fn member(
    archive: &mut zip::ZipArchive<Cursor<&[u8]>>,
    name: &str,
    limit: usize,
) -> Result<Vec<u8>, Error> {
    safe_member(name)?;
    let mut file = archive.by_name(name).map_err(|source| {
        Error::new(
            zip_error_kind(&source),
            ErrorDetail::ZipMember {
                name: name.to_owned(),
                source,
            },
        )
    })?;
    let size = file.size();
    if size == 0 || size > limit as u64 {
        return Err(Error::new(
            if size == 0 {
                ErrorKind::Malformed
            } else {
                ErrorKind::Limit
            },
            ErrorDetail::MemberSize {
                name: name.to_owned(),
                limit,
            },
        ));
    }
    let mut bytes = Vec::with_capacity(size as usize);
    (&mut file)
        .take(size + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| {
            Error::new(
                ErrorKind::Io,
                ErrorDetail::MemberRead {
                    name: name.to_owned(),
                    source,
                },
            )
        })?;
    if bytes.len() as u64 != size {
        return Err(Error::new(
            ErrorKind::Integrity,
            ErrorDetail::InconsistentSize(name.to_owned()),
        ));
    }
    Ok(bytes)
}

/// Parses an application-only Legacy DFU ZIP into owned, unchanged payloads.
///
/// This validates the supported init-packet layout but does not authenticate it or
/// compare its checksum or signature with the image. The caller retains `bytes`.
///
/// # Errors
///
/// Returns an error for unsupported, malformed, unsafe, or oversized packages.
pub fn parse_package(name: &str, bytes: &[u8]) -> Result<Package, Error> {
    let input_error = "BLE OTA requires a Legacy DFU application ZIP (maximum 4 MiB)";
    if !name.to_ascii_lowercase().ends_with(".zip") {
        return Err(Error::message(ErrorKind::Unsupported, input_error));
    }
    if bytes.len() > MAX_ZIP {
        return Err(Error::message(ErrorKind::Limit, input_error));
    }
    let mut archive = crate::zip_envelope::open(
        bytes,
        crate::zip_envelope::Limits {
            bytes: MAX_ZIP,
            members: 64,
            directory: MAX_ZIP,
        },
    )
    .map_err(|source| {
        let kind = match source.kind() {
            crate::zip_envelope::ErrorKind::Malformed => ErrorKind::Malformed,
            crate::zip_envelope::ErrorKind::Unsupported => ErrorKind::Unsupported,
            crate::zip_envelope::ErrorKind::Limit => ErrorKind::Limit,
        };
        Error::new(kind, ErrorDetail::Envelope(source))
    })?;
    let mut names = HashSet::new();
    for i in 0..archive.len() {
        let file = archive
            .by_index(i)
            .map_err(|source| Error::new(zip_error_kind(&source), ErrorDetail::ZipIndex(source)))?;
        let name = file
            .name()
            .map_err(|source| Error::new(zip_error_kind(&source), ErrorDetail::ZipName(source)))?;
        safe_member(name.trim_end_matches('/'))?;
        if !names.insert(name.as_ref().to_owned()) {
            return Err(Error::new(
                ErrorKind::Malformed,
                ErrorDetail::DuplicateMember(name.into_owned()),
            ));
        }
        if file
            .unix_mode()
            .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            return Err(Error::message(
                ErrorKind::Unsupported,
                "DFU ZIP symbolic links are not supported",
            ));
        }
    }
    let manifest: Value = serde_json::from_slice(&member(&mut archive, "manifest.json", 65536)?)
        .map_err(|source| Error::new(ErrorKind::Malformed, ErrorDetail::Manifest(source)))?;
    let root = manifest
        .get("manifest")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            Error::message(
                ErrorKind::Unsupported,
                "Not a Legacy DFU manifest; Secure DFU/MCUboot packages are not supported",
            )
        })?;
    if root
        .iter()
        .any(|(key, value)| key != "application" && !value.is_null())
    {
        return Err(Error::message(
            ErrorKind::Unsupported,
            "BLE OTA accepts application-only Legacy DFU packages, not bootloader/SoftDevice updates",
        ));
    }
    let entry = root
        .get("application")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            Error::message(
                ErrorKind::Malformed,
                "Legacy DFU ZIP has no application entry",
            )
        })?;
    let bin = entry
        .get("bin_file")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            Error::message(ErrorKind::Malformed, "DFU application is missing bin_file")
        })?;
    let dat = entry
        .get("dat_file")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            Error::message(ErrorKind::Malformed, "DFU application is missing dat_file")
        })?;
    let members_error = "Legacy DFU requires separate .bin and .dat members";
    if bin == dat {
        return Err(Error::message(ErrorKind::Malformed, members_error));
    }
    if !bin.to_ascii_lowercase().ends_with(".bin") || !dat.to_ascii_lowercase().ends_with(".dat") {
        return Err(Error::message(ErrorKind::Unsupported, members_error));
    }
    let image = member(&mut archive, bin, MAX_IMAGE)?;
    let init = member(&mut archive, dat, 1024)?;
    if image.len() % 4 != 0 {
        return Err(Error::message(
            ErrorKind::Malformed,
            "Legacy DFU firmware must be 4-byte aligned",
        ));
    }
    if image.starts_with(&[0x3d, 0xb8, 0xf3, 0x96]) {
        return Err(Error::message(
            ErrorKind::Unsupported,
            "MCUboot images cannot be sent over Legacy BLE DFU",
        ));
    }
    // Secure DFU .dat is protobuf, not this fixed little-endian SDK11 header.
    let init_error = "Not an Adafruit Legacy init packet (device_type must be 0x0052); Secure DFU is not supported";
    if init.len() < 12 {
        return Err(Error::message(ErrorKind::Malformed, init_error));
    }
    if init[..2] != [0x52, 0] {
        return Err(Error::message(ErrorKind::Unsupported, init_error));
    }
    let count = u16::from_le_bytes([init[8], init[9]]) as usize;
    let extension = init.get(10 + count * 2..).ok_or_else(|| {
        Error::message(
            ErrorKind::Malformed,
            "Truncated Legacy SoftDevice requirements",
        )
    })?;
    let extension_error = match extension.len() {
        2 => None,                                   // DFU 0.5 CRC16
        6 if extension[..4] == [0, 0, 0, 0] => None, // DFU 0.6 CRC16
        40 | 104 => {
            let kind = if extension.len() == 40 { 1 } else { 2 };
            if extension[..4] != [kind, 0, 0, 0] {
                Some(ErrorKind::Malformed)
            } else if u32::from_le_bytes(extension[4..8].try_into().unwrap()) as usize
                != image.len()
            {
                Some(ErrorKind::Integrity)
            } else {
                None
            }
        }
        6 => Some(ErrorKind::Malformed),
        _ => Some(ErrorKind::Unsupported),
    };
    if let Some(kind) = extension_error {
        return Err(Error::message(
            kind,
            "Unsupported or malformed Legacy init packet; Secure DFU is not supported",
        ));
    }
    Ok(Package { image, init })
}

#[cfg(test)]
mod tests {
    use super::{ErrorKind, MAX_IMAGE, MAX_ZIP, parse_package};
    use std::{
        error::Error as _,
        io::{self, Cursor, Write},
    };

    fn init() -> Vec<u8> {
        vec![0x52, 0, 0xff, 0xff, 1, 0, 0, 0, 1, 0, 0xfe, 0xff, 0, 0]
    }

    fn archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        for (name, bytes) in entries {
            zip.start_file(*name, options).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    fn package(bin_name: &str, image: &[u8], dat: &[u8], extra: bool) -> Vec<u8> {
        let mut manifest = serde_json::json!({"manifest":{"application":{"bin_file":bin_name,"dat_file":"application.dat"}}});
        if extra {
            manifest["manifest"]["bootloader"] =
                serde_json::json!({"bin_file":"boot.bin","dat_file":"boot.dat"});
        }
        let manifest = serde_json::to_vec(&manifest).unwrap();
        archive(&[
            ("manifest.json", &manifest),
            (bin_name, image),
            ("application.dat", dat),
        ])
    }

    #[test]
    fn parses_legacy_application_without_changing_init() {
        let bytes = package("nested/app.bin", &[1, 2, 3, 4], &init(), false);
        let parsed = parse_package("legacy.ZIP", &bytes).unwrap();
        let (image, packet) = parsed.payloads();
        assert_eq!(image, [1, 2, 3, 4]);
        assert_eq!(packet, init());
    }

    #[test]
    fn package_errors_expose_categories_and_native_sources() {
        let mut bytes = archive(&[("manifest.json", b"{}")]);
        let end = bytes.len() - 22;
        let central = u32::from_le_bytes(bytes[end + 16..end + 20].try_into().unwrap()) as usize;
        let directory_len =
            u32::from_le_bytes(bytes[end + 12..end + 16].try_into().unwrap()) as usize;
        let name_len =
            u16::from_le_bytes(bytes[central + 28..central + 30].try_into().unwrap()) as usize;
        let extra_len =
            u16::from_le_bytes(bytes[central + 30..central + 32].try_into().unwrap()) as usize;
        let extra_end = central + 46 + name_len + extra_len;
        bytes[central + 30..central + 32].copy_from_slice(&((extra_len + 4) as u16).to_le_bytes());
        bytes[end + 12..end + 16].copy_from_slice(&((directory_len + 4) as u32).to_le_bytes());
        // The ZIP parser encounters a framed Unicode path field with no payload.
        drop(bytes.splice(extra_end..extra_end, [0x75, 0x70, 0, 0]));
        let error = parse_package("legacy.zip", &bytes).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Malformed);
        let envelope = error
            .source()
            .unwrap()
            .downcast_ref::<crate::zip_envelope::Error>()
            .unwrap();
        assert_eq!(envelope.kind(), crate::zip_envelope::ErrorKind::Malformed);
        let zip = envelope
            .source()
            .unwrap()
            .downcast_ref::<zip::result::ZipError>()
            .unwrap();
        assert!(matches!(zip, zip::result::ZipError::Io(_)));
        assert_eq!(
            zip.source()
                .unwrap()
                .downcast_ref::<io::Error>()
                .unwrap()
                .kind(),
            io::ErrorKind::UnexpectedEof
        );

        let bytes = archive(&[("manifest.json", b"{")]);
        let error = parse_package("legacy.zip", &bytes).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Malformed);
        assert!(
            error
                .source()
                .unwrap()
                .downcast_ref::<serde_json::Error>()
                .unwrap()
                .is_eof()
        );

        let manifest =
            br#"{"manifest":{"application":{"bin_file":"app.bin","dat_file":"app.dat"}}}"#;
        let bytes = archive(&[("manifest.json", manifest)]);
        let error = parse_package("legacy.zip", &bytes).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Malformed);
        assert!(matches!(
            error
                .source()
                .unwrap()
                .downcast_ref::<zip::result::ZipError>(),
            Some(zip::result::ZipError::FileNotFound)
        ));

        let mut bytes = archive(&[("manifest.json", b"{}")]);
        let end = bytes.len() - 22;
        let central = u32::from_le_bytes(bytes[end + 16..end + 20].try_into().unwrap()) as usize;
        let local =
            u32::from_le_bytes(bytes[central + 42..central + 46].try_into().unwrap()) as usize;
        bytes[central + 16] ^= 1;
        bytes[local + 14] ^= 1;
        let error = parse_package("legacy.zip", &bytes).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Io);
        assert_eq!(
            error
                .source()
                .unwrap()
                .downcast_ref::<io::Error>()
                .unwrap()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn mismatched_init_image_length_is_an_integrity_failure() {
        let mut packet = init();
        packet.truncate(12);
        let mut extension = [0; 40];
        extension[0] = 1;
        extension[4] = 8;
        packet.extend_from_slice(&extension);
        let bytes = package("app.bin", &[1, 2, 3, 4], &packet, false);
        assert_eq!(
            parse_package("legacy.zip", &bytes).unwrap_err().kind(),
            ErrorKind::Integrity
        );
    }

    #[test]
    fn legacy_zip_rejects_unsafe_envelopes_and_false_member_sizes() {
        let bytes = package("app.bin", &[1, 2, 3, 4], &init(), false);
        let end = bytes.len() - 22;
        for (field, value) in [(8, 65u16), (10, 65), (4, 1), (6, 1), (10, u16::MAX)] {
            let mut malformed = bytes.clone();
            malformed[end + field..end + field + 2].copy_from_slice(&value.to_le_bytes());
            assert!(parse_package("legacy.zip", &malformed).is_err());
        }
        let central = u32::from_le_bytes(bytes[end + 16..end + 20].try_into().unwrap()) as usize;
        let local =
            u32::from_le_bytes(bytes[central + 42..central + 46].try_into().unwrap()) as usize;
        let size = u32::from_le_bytes(bytes[central + 24..central + 28].try_into().unwrap());
        for declared in [0, size - 1, size + 1] {
            let mut malformed = bytes.clone();
            malformed[local + 22..local + 26].copy_from_slice(&declared.to_le_bytes());
            malformed[central + 24..central + 28].copy_from_slice(&declared.to_le_bytes());
            assert!(parse_package("legacy.zip", &malformed).is_err());
        }
    }

    #[test]
    fn rejects_secure_mcuboot_traversal_unaligned_and_multi_image() {
        for (name, image, dat, extra) in [
            (
                "app.bin",
                vec![1, 2, 3, 4],
                vec![0x12, 0x0c, 0x0a, 0x0a, 0x08, 1],
                false,
            ),
            ("app.bin", vec![0x3d, 0xb8, 0xf3, 0x96], init(), false),
            ("../app.bin", vec![1, 2, 3, 4], init(), false),
            ("app.bin", vec![1, 2, 3], init(), false),
            ("app.bin", vec![1, 2, 3, 4], init(), true),
        ] {
            assert!(parse_package("bad.zip", &package(name, &image, &dat, extra)).is_err());
        }
    }

    #[test]
    fn enforces_package_and_member_limits() {
        let valid = package("app.bin", &[1, 2, 3, 4], &init(), false);
        assert_eq!(
            parse_package("legacy.bin", &valid).unwrap_err().kind(),
            ErrorKind::Unsupported
        );
        let oversized_archive = vec![0; MAX_ZIP + 1];
        assert_eq!(
            parse_package("legacy.zip", &oversized_archive)
                .unwrap_err()
                .kind(),
            ErrorKind::Limit
        );
        let large_image = vec![0; MAX_IMAGE + 4];
        let oversized = package("app.bin", &large_image, &init(), false);
        assert_eq!(
            parse_package("legacy.zip", &oversized).unwrap_err().kind(),
            ErrorKind::Limit
        );
        let large_init = vec![0; 1025];
        let oversized_init = package("app.bin", &[1, 2, 3, 4], &large_init, false);
        assert_eq!(
            parse_package("legacy.zip", &oversized_init)
                .unwrap_err()
                .kind(),
            ErrorKind::Limit
        );
        assert_eq!(
            parse_package("legacy.zip", &package("app.bin", &[], &init(), false))
                .unwrap_err()
                .kind(),
            ErrorKind::Malformed
        );
        assert!(
            parse_package("legacy.zip", &package("app.bin", &[1, 2, 3, 4], &[], false)).is_err(),
            "empty init packet"
        );

        let max_image = vec![0; MAX_IMAGE];
        let mut max_init = vec![0; 1024];
        max_init[0] = 0x52;
        max_init[8..10].copy_from_slice(&506u16.to_le_bytes());
        let at_limit = package("app.bin", &max_image, &max_init, false);
        let parsed = parse_package("legacy.zip", &at_limit).unwrap();
        assert_eq!(
            parsed.payloads(),
            (max_image.as_slice(), max_init.as_slice())
        );
    }

    #[test]
    fn accepts_supported_init_layouts_without_authenticating_content() {
        let image = [1, 2, 3, 4];
        let standard_init = init();
        let header = &standard_init[..12];
        for extension in [
            vec![0x12, 0x34],
            vec![0, 0, 0, 0, 0x12, 0x34],
            {
                let mut ext = vec![0xa5; 40];
                ext[..8].copy_from_slice(&[1, 0, 0, 0, 4, 0, 0, 0]);
                ext
            },
            {
                let mut ext = vec![0xa5; 104];
                ext[..8].copy_from_slice(&[2, 0, 0, 0, 4, 0, 0, 0]);
                ext
            },
        ] {
            let mut packet = header.to_vec();
            packet.extend_from_slice(&extension);
            let bytes = package("app.bin", &image, &packet, false);
            let parsed = parse_package("legacy.zip", &bytes).unwrap();
            assert_eq!(parsed.payloads(), (image.as_slice(), packet.as_slice()));
        }
    }

    #[test]
    fn manifest_allows_null_non_application_entries_but_rejects_live_ones() {
        let mut manifest = serde_json::json!({
            "manifest": {
                "application": {"bin_file": "app.bin", "dat_file": "application.dat"},
                "bootloader": null
            }
        });
        let image = [1, 2, 3, 4];
        let packet = init();
        let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
        let bytes = archive(&[
            ("manifest.json", &manifest_bytes),
            ("app.bin", &image),
            ("application.dat", &packet),
        ]);
        assert!(parse_package("legacy.zip", &bytes).is_ok());

        manifest["manifest"]["bootloader"] = serde_json::json!({});
        let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
        let bytes = archive(&[
            ("manifest.json", &manifest_bytes),
            ("app.bin", &image),
            ("application.dat", &packet),
        ]);
        assert!(
            parse_package("legacy.zip", &bytes).is_err(),
            "non-null bootloader entry"
        );
    }

    #[test]
    fn rejects_unsafe_unreferenced_members_and_truncated_requirements() {
        let manifest =
            br#"{"manifest":{"application":{"bin_file":"app.bin","dat_file":"application.dat"}}}"#;
        let image = [1, 2, 3, 4];
        let packet = init();
        for name in [
            "../unused.bin",
            "/unused.bin",
            "a//unused.bin",
            "a\\unused.bin",
            "a:unused.bin",
        ] {
            let bytes = archive(&[
                ("manifest.json", manifest),
                ("app.bin", &image),
                ("application.dat", &packet),
                (name, b"ignored"),
            ]);
            assert!(
                parse_package("legacy.zip", &bytes).is_err(),
                "member {name:?}"
            );
        }

        let mut truncated = packet;
        truncated[8..10].copy_from_slice(&3u16.to_le_bytes());
        let bytes = package("app.bin", &image, &truncated, false);
        assert!(
            parse_package("legacy.zip", &bytes).is_err(),
            "truncated SoftDevice requirements"
        );
    }

    #[test]
    fn rejects_duplicate_members_and_symbolic_links() {
        let manifest =
            br#"{"manifest":{"application":{"bin_file":"app.bin","dat_file":"application.dat"}}}"#;
        let image = [1, 2, 3, 4];
        let packet = init();
        let entries: [(&str, &[u8]); 3] = [
            ("manifest.json", manifest),
            ("app.bin", &image),
            ("application.dat", &packet),
        ];

        // ZipWriter refuses duplicate names; mutate equal-length names in a valid archive.
        let mut duplicate = archive(&[entries[0], entries[1], entries[2], ("alt.bin", b"ignored")]);
        let end = duplicate.len() - 22;
        let mut central =
            u32::from_le_bytes(duplicate[end + 16..end + 20].try_into().unwrap()) as usize;
        let mut renamed = false;
        while central < end {
            let name_len =
                u16::from_le_bytes(duplicate[central + 28..central + 30].try_into().unwrap())
                    as usize;
            let extra_len =
                u16::from_le_bytes(duplicate[central + 30..central + 32].try_into().unwrap())
                    as usize;
            let comment_len =
                u16::from_le_bytes(duplicate[central + 32..central + 34].try_into().unwrap())
                    as usize;
            if &duplicate[central + 46..central + 46 + name_len] == b"alt.bin" {
                let local =
                    u32::from_le_bytes(duplicate[central + 42..central + 46].try_into().unwrap())
                        as usize;
                duplicate[local + 30..local + 37].copy_from_slice(b"app.bin");
                duplicate[central + 46..central + 53].copy_from_slice(b"app.bin");
                renamed = true;
                break;
            }
            central += 46 + name_len + extra_len + comment_len;
        }
        assert!(renamed, "duplicate fixture member was not found");
        assert!(
            parse_package("legacy.zip", &duplicate).is_err(),
            "duplicate archive member"
        );

        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        for (name, bytes) in entries {
            zip.start_file(name, options).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.add_symlink("unused", "app.bin", options).unwrap();
        let symlink = zip.finish().unwrap().into_inner();
        assert!(
            parse_package("legacy.zip", &symlink).is_err(),
            "symbolic link archive member"
        );
    }
}
