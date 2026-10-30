use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

// Sizes and offsets below are from the Microsoft PE/COFF image format.
const DOS_HEADER_LEN: usize = 64;
const PE_HEADER_LEN: usize = 24;
const PE32_PLUS_PREFIX_LEN: usize = 112;
const DATA_DIRECTORY_LEN: u64 = 8;
const SECTION_HEADER_LEN: u64 = 40;
const MAX_SECTIONS: u16 = 96;
const IMAGE_FILE_EXECUTABLE_IMAGE: u16 = 0x0002;
const IMAGE_FILE_DLL: u16 = 0x2000;
const PE32_PLUS_MAGIC: u16 = 0x020b;

/// Checks the executable's header and target architecture, not its authenticity.
///
/// # Errors
/// Returns an error if the file cannot be read, its headers are incomplete or
/// incompatible, or the compiled target architecture is not supported.
pub(super) fn validate_payload(path: &Path) -> Result<(), String> {
    let machine = machine_for_architecture(std::env::consts::ARCH)?;
    validate_payload_for_machine(path, machine)
}

fn machine_for_architecture(architecture: &str) -> Result<u16, String> {
    match architecture {
        "x86_64" => Ok(0x8664),
        "aarch64" => Ok(0xaa64),
        _ => Err(format!("unsupported update architecture: {architecture}")),
    }
}

fn validate_payload_for_machine(path: &Path, expected_machine: u16) -> Result<(), String> {
    let mut file =
        File::open(path).map_err(|error| format!("cannot open update image: {error}"))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("cannot inspect update image: {error}"))?;
    if !metadata.is_file() {
        return Err("update image is not a regular file".to_owned());
    }

    let mut dos = [0_u8; DOS_HEADER_LEN];
    file.read_exact(&mut dos)
        .map_err(|error| format!("cannot read DOS header: {error}"))?;
    if &dos[..2] != b"MZ" {
        return Err("invalid DOS signature in update image".to_owned());
    }

    // e_lfanew is a signed 32-bit file offset, not a length or an RVA.
    let pe_offset = i32::from_le_bytes([dos[0x3c], dos[0x3d], dos[0x3e], dos[0x3f]]);
    let pe_offset = u64::try_from(pe_offset)
        .map_err(|_| "negative PE header offset in update image".to_owned())?;
    if pe_offset < DOS_HEADER_LEN as u64 {
        return Err("PE header overlaps the DOS header".to_owned());
    }
    // The signed DOS offset and the 16-bit optional-header length ensure these
    // additions fit in u64; metadata bounds every seek into the image.
    let pe_end = pe_offset + PE_HEADER_LEN as u64;
    if pe_end > metadata.len() {
        return Err("truncated PE header in update image".to_owned());
    }
    file.seek(SeekFrom::Start(pe_offset))
        .map_err(|error| format!("cannot seek to PE header: {error}"))?;
    let mut pe = [0_u8; PE_HEADER_LEN];
    file.read_exact(&mut pe)
        .map_err(|error| format!("cannot read PE header: {error}"))?;
    if &pe[..4] != b"PE\0\0" {
        return Err("invalid PE signature in update image".to_owned());
    }
    let machine = u16::from_le_bytes([pe[4], pe[5]]);
    if machine != expected_machine {
        return Err(format!(
            "update image machine 0x{machine:04x} does not match expected machine 0x{expected_machine:04x}"
        ));
    }
    let characteristics = u16::from_le_bytes([pe[22], pe[23]]);
    if characteristics & IMAGE_FILE_EXECUTABLE_IMAGE == 0 || characteristics & IMAGE_FILE_DLL != 0 {
        return Err("update image is not an executable EXE".to_owned());
    }
    let section_count = u16::from_le_bytes([pe[6], pe[7]]);
    if section_count > MAX_SECTIONS {
        return Err("PE section count exceeds the Windows loader limit".to_owned());
    }

    let optional_len = u16::from_le_bytes([pe[20], pe[21]]);
    if usize::from(optional_len) < PE32_PLUS_PREFIX_LEN {
        return Err("PE optional header is too short for PE32+".to_owned());
    }
    let optional_end = pe_end + u64::from(optional_len);
    if optional_end > metadata.len() {
        return Err("truncated PE optional header in update image".to_owned());
    }
    let section_end = optional_end + u64::from(section_count) * SECTION_HEADER_LEN;
    if section_end > metadata.len() {
        return Err("truncated PE section table in update image".to_owned());
    }
    let mut optional = [0_u8; PE32_PLUS_PREFIX_LEN];
    file.read_exact(&mut optional)
        .map_err(|error| format!("cannot read PE optional header: {error}"))?;
    if u16::from_le_bytes([optional[0], optional[1]]) != PE32_PLUS_MAGIC {
        return Err("incompatible PE optional-header magic".to_owned());
    }
    let directory_count =
        u32::from_le_bytes([optional[108], optional[109], optional[110], optional[111]]);
    let directory_capacity =
        (u64::from(optional_len) - PE32_PLUS_PREFIX_LEN as u64) / DATA_DIRECTORY_LEN;
    if u64::from(directory_count) > directory_capacity {
        return Err("PE data directories exceed the optional header".to_owned());
    }

    // Probe the declared end as well as metadata, without reading section
    // entries or allocating according to untrusted header values.
    file.seek(SeekFrom::Start(section_end - 1))
        .map_err(|error| format!("cannot seek to end of PE headers: {error}"))?;
    file.read_exact(&mut [0_u8; 1])
        .map_err(|error| format!("cannot read end of PE headers: {error}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{machine_for_architecture, validate_payload, validate_payload_for_machine};
    use std::fs;

    const AMD64: u16 = 0x8664;
    const ARM64: u16 = 0xaa64;
    const PE_OFFSET: usize = 0x80;
    const OPTIONAL_OFFSET: usize = PE_OFFSET + 24;
    const OPTIONAL_LEN: usize = 240;
    const SECTION_OFFSET: usize = OPTIONAL_OFFSET + OPTIONAL_LEN;
    const SECTION_LEN: usize = 40;

    fn put_u16(bytes: &mut [u8], offset: usize, value: u16) {
        bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
        bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }

    // A conventional unsigned, single-section PE32+ image; no external file or
    // executable is needed to exercise the parser.
    fn image(machine: u16) -> Vec<u8> {
        let mut bytes = vec![0; 0x400];
        bytes[..2].copy_from_slice(b"MZ");
        put_u32(&mut bytes, 0x3c, PE_OFFSET as u32);
        bytes[PE_OFFSET..PE_OFFSET + 4].copy_from_slice(b"PE\0\0");
        put_u16(&mut bytes, PE_OFFSET + 4, machine);
        put_u16(&mut bytes, PE_OFFSET + 6, 1);
        put_u16(&mut bytes, PE_OFFSET + 20, OPTIONAL_LEN as u16);
        put_u16(&mut bytes, PE_OFFSET + 22, 0x0022);

        put_u16(&mut bytes, OPTIONAL_OFFSET, 0x020b);
        put_u32(&mut bytes, OPTIONAL_OFFSET + 4, 0x200);
        put_u32(&mut bytes, OPTIONAL_OFFSET + 16, 0x1000);
        put_u32(&mut bytes, OPTIONAL_OFFSET + 20, 0x1000);
        put_u64(&mut bytes, OPTIONAL_OFFSET + 24, 0x1_4000_0000);
        put_u32(&mut bytes, OPTIONAL_OFFSET + 32, 0x1000);
        put_u32(&mut bytes, OPTIONAL_OFFSET + 36, 0x200);
        put_u16(&mut bytes, OPTIONAL_OFFSET + 40, 6);
        put_u16(&mut bytes, OPTIONAL_OFFSET + 48, 6);
        put_u32(&mut bytes, OPTIONAL_OFFSET + 56, 0x2000);
        put_u32(&mut bytes, OPTIONAL_OFFSET + 60, 0x200);
        put_u16(&mut bytes, OPTIONAL_OFFSET + 68, 3);
        put_u64(&mut bytes, OPTIONAL_OFFSET + 72, 0x10_0000);
        put_u64(&mut bytes, OPTIONAL_OFFSET + 80, 0x1000);
        put_u64(&mut bytes, OPTIONAL_OFFSET + 88, 0x10_0000);
        put_u64(&mut bytes, OPTIONAL_OFFSET + 96, 0x1000);
        put_u32(&mut bytes, OPTIONAL_OFFSET + 108, 16);

        bytes[SECTION_OFFSET..SECTION_OFFSET + 8].copy_from_slice(b".text\0\0\0");
        put_u32(&mut bytes, SECTION_OFFSET + 8, 8);
        put_u32(&mut bytes, SECTION_OFFSET + 12, 0x1000);
        put_u32(&mut bytes, SECTION_OFFSET + 16, 0x200);
        put_u32(&mut bytes, SECTION_OFFSET + 20, 0x200);
        put_u32(&mut bytes, SECTION_OFFSET + 36, 0x6000_0020);
        if machine == ARM64 {
            bytes[0x200..0x208].copy_from_slice(&[0x00, 0x00, 0x80, 0x52, 0xc0, 0x03, 0x5f, 0xd6]);
        } else {
            bytes[0x200..0x203].copy_from_slice(&[0x31, 0xc0, 0xc3]);
        }
        bytes
    }

    fn validate_image(bytes: &[u8], machine: u16) -> Result<(), String> {
        let directory = tempfile::tempdir().expect("create PE test directory");
        let path = directory.path().join("payload.exe");
        fs::write(&path, bytes).expect("write PE test image");
        validate_payload_for_machine(&path, machine)
    }

    fn assert_rejected(bytes: &[u8], machine: u16, reason: &str) {
        let error = validate_image(bytes, machine).expect_err("image should be rejected");
        assert!(error.contains(reason), "expected {reason:?}, got {error:?}");
    }

    #[test]
    fn validates_both_supported_machines_and_rejects_mismatches() {
        for (machine, wrong_machine) in [(AMD64, ARM64), (ARM64, AMD64)] {
            let bytes = image(machine);
            assert_eq!(validate_image(&bytes, machine), Ok(()));
            assert_rejected(&bytes, wrong_machine, "machine");
        }
        for machine in [0, 0x014c, 0xa641, 0xa64e] {
            assert_rejected(&image(machine), AMD64, "machine");
        }
    }

    #[test]
    fn public_entry_uses_the_compiled_architecture() {
        let directory = tempfile::tempdir().expect("create PE test directory");
        let path = directory.path().join("payload.exe");
        let machine = if cfg!(target_arch = "aarch64") {
            ARM64
        } else {
            AMD64
        };
        fs::write(&path, image(machine)).expect("write native image");
        if cfg!(any(target_arch = "x86_64", target_arch = "aarch64")) {
            assert_eq!(validate_payload(&path), Ok(()));
            let wrong_machine = if machine == AMD64 { ARM64 } else { AMD64 };
            fs::write(&path, image(wrong_machine)).expect("write foreign image");
            let error = validate_payload(&path).expect_err("foreign image should be rejected");
            assert!(error.contains("machine"), "unexpected error: {error}");
        } else {
            let error = validate_payload(&path).expect_err("unsupported target should be rejected");
            assert!(error.contains("unsupported update architecture"));
        }
        assert!(machine_for_architecture("x86").is_err());
        assert!(machine_for_architecture("arm64ec").is_err());
    }

    #[test]
    fn accepts_packed_style_sections_and_unsigned_images() {
        let mut bytes = image(AMD64);
        put_u16(&mut bytes, PE_OFFSET + 6, 2);
        put_u32(&mut bytes, OPTIONAL_OFFSET + 16, 0x3000);
        put_u32(&mut bytes, OPTIONAL_OFFSET + 20, 0x3000);
        put_u32(&mut bytes, OPTIONAL_OFFSET + 56, 0x4000);
        bytes[SECTION_OFFSET..SECTION_OFFSET + 8].copy_from_slice(b"UPX0\0\0\0\0");
        put_u32(&mut bytes, SECTION_OFFSET + 8, 0x2000);
        put_u32(&mut bytes, SECTION_OFFSET + 16, 0);
        put_u32(&mut bytes, SECTION_OFFSET + 20, 0);
        put_u32(&mut bytes, SECTION_OFFSET + 36, 0xe000_0080);
        let second = SECTION_OFFSET + SECTION_LEN;
        bytes[second..second + 8].copy_from_slice(b"UPX1\0\0\0\0");
        put_u32(&mut bytes, second + 8, 0x200);
        put_u32(&mut bytes, second + 12, 0x3000);
        put_u32(&mut bytes, second + 16, 0x200);
        put_u32(&mut bytes, second + 20, 0x200);
        put_u32(&mut bytes, second + 36, 0xe000_0020);
        bytes.extend_from_slice(b"arbitrary overlay");
        assert_eq!(validate_image(&bytes, AMD64), Ok(()));
    }

    #[test]
    fn rejects_missing_or_damaged_dos_and_pe_headers() {
        let bytes = image(AMD64);
        for len in [
            0,
            1,
            2,
            0x3b,
            0x3c,
            0x3f,
            0x40,
            PE_OFFSET,
            PE_OFFSET + 3,
            PE_OFFSET + 4,
            PE_OFFSET + 23,
            OPTIONAL_OFFSET,
            OPTIONAL_OFFSET + 1,
            OPTIONAL_OFFSET + 111,
            OPTIONAL_OFFSET + OPTIONAL_LEN - 1,
        ] {
            assert!(
                validate_image(&bytes[..len], AMD64).is_err(),
                "truncated image with {len} bytes was accepted"
            );
        }
        let mut wrong_dos = bytes.clone();
        wrong_dos[..2].copy_from_slice(b"NZ");
        assert_rejected(&wrong_dos, AMD64, "DOS signature");
        let mut wrong_pe = bytes;
        wrong_pe[PE_OFFSET + 2] = 1;
        assert_rejected(&wrong_pe, AMD64, "PE signature");
    }

    #[test]
    fn rejects_invalid_pe_offsets() {
        for offset in [0, 4, 0x3b, 0x3c, 0x3f, 0x7fff_ffff, 0xffff_ffff] {
            let mut bytes = image(AMD64);
            put_u32(&mut bytes, 0x3c, offset);
            assert!(
                validate_image(&bytes, AMD64).is_err(),
                "invalid PE offset {offset:#x} was accepted"
            );
        }
    }

    #[test]
    fn rejects_dlls_and_non_executable_images() {
        for characteristics in [0, 0x0020, 0x2000, 0x2002, 0x2022] {
            let mut bytes = image(AMD64);
            put_u16(&mut bytes, PE_OFFSET + 22, characteristics);
            assert_rejected(&bytes, AMD64, "executable EXE");
        }
    }

    #[test]
    fn validates_optional_header_magic_and_extent() {
        for magic in [0, 0x0107, 0x010b, 0x020c, 0xffff] {
            let mut bytes = image(AMD64);
            put_u16(&mut bytes, OPTIONAL_OFFSET, magic);
            assert_rejected(&bytes, AMD64, "optional-header magic");
        }
        for optional_len in [0, 1, 2, 111] {
            let mut bytes = image(AMD64);
            put_u16(&mut bytes, PE_OFFSET + 20, optional_len);
            assert_rejected(&bytes, AMD64, "optional header is too short");
        }
        let mut oversized = image(AMD64);
        put_u16(&mut oversized, PE_OFFSET + 20, u16::MAX);
        assert_rejected(&oversized, AMD64, "truncated PE optional header");
    }

    #[test]
    fn checks_section_table_bounds_without_assuming_layout() {
        for count in [1, 2] {
            let mut bytes = image(AMD64);
            put_u16(&mut bytes, PE_OFFSET + 6, count);
            bytes.truncate(SECTION_OFFSET + usize::from(count) * SECTION_LEN - 1);
            assert_rejected(&bytes, AMD64, "truncated PE section table");
        }

        let mut at_limit = image(AMD64);
        put_u16(&mut at_limit, PE_OFFSET + 6, 96);
        at_limit.resize(SECTION_OFFSET + 96 * SECTION_LEN, 0);
        assert_eq!(validate_image(&at_limit, AMD64), Ok(()));

        let mut too_many = image(AMD64);
        put_u16(&mut too_many, PE_OFFSET + 6, 97);
        too_many.resize(SECTION_OFFSET + 97 * SECTION_LEN, 0);
        assert_rejected(&too_many, AMD64, "section count exceeds");

        // Low-alignment PE images may legally have no section headers.
        let mut no_sections = image(AMD64);
        put_u16(&mut no_sections, PE_OFFSET + 6, 0);
        put_u32(&mut no_sections, OPTIONAL_OFFSET + 16, 0x200);
        put_u32(&mut no_sections, OPTIONAL_OFFSET + 20, 0x200);
        put_u32(&mut no_sections, OPTIONAL_OFFSET + 32, 4);
        put_u32(&mut no_sections, OPTIONAL_OFFSET + 36, 4);
        put_u32(&mut no_sections, OPTIONAL_OFFSET + 56, 0x400);
        put_u32(&mut no_sections, OPTIONAL_OFFSET + 60, 0x400);
        assert_eq!(validate_image(&no_sections, AMD64), Ok(()));
    }

    #[test]
    fn validates_variable_directory_count_within_optional_header() {
        let mut minimal = image(AMD64);
        minimal.copy_within(
            SECTION_OFFSET..SECTION_OFFSET + SECTION_LEN,
            OPTIONAL_OFFSET + 112,
        );
        put_u16(&mut minimal, PE_OFFSET + 20, 112);
        put_u32(&mut minimal, OPTIONAL_OFFSET + 108, 0);
        assert_eq!(validate_image(&minimal, AMD64), Ok(()));

        let mut extended = image(AMD64);
        extended.copy_within(
            SECTION_OFFSET..SECTION_OFFSET + SECTION_LEN,
            SECTION_OFFSET + 8,
        );
        extended[SECTION_OFFSET..SECTION_OFFSET + 8].fill(0);
        put_u16(&mut extended, PE_OFFSET + 20, 248);
        put_u32(&mut extended, OPTIONAL_OFFSET + 108, 17);
        assert_eq!(validate_image(&extended, AMD64), Ok(()));

        for (size, count) in [(112, 1), (240, 17), (240, u32::MAX)] {
            let mut bytes = image(AMD64);
            put_u16(&mut bytes, PE_OFFSET + 20, size);
            put_u32(&mut bytes, OPTIONAL_OFFSET + 108, count);
            assert_rejected(&bytes, AMD64, "data directories exceed");
        }
    }

    #[test]
    fn inaccessible_file_reports_an_error() {
        let directory = tempfile::tempdir().expect("create PE test directory");
        let missing = directory.path().join("missing.exe");
        let error = validate_payload_for_machine(&missing, AMD64).expect_err("missing file");
        assert!(error.contains("cannot open update image"));
    }
}
