use std::io::Cursor;

use zip::read::{ArchiveOffset, Config};

pub(crate) struct Limits {
    pub bytes: usize,
    pub members: usize,
    pub directory: usize,
}

fn u16_at(bytes: &[u8], offset: usize) -> usize {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap()) as usize
}

fn u32_at(bytes: &[u8], offset: usize) -> usize {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize
}

fn extra_fields(bytes: &[u8], mut field: usize, end: usize) -> Result<(), String> {
    while field < end {
        if field + 4 > end {
            return Err("Firmware ZIP has malformed extra data".into());
        }
        if u16_at(bytes, field) == 1 {
            return Err("Firmware ZIP uses unsupported ZIP64 extra data".into());
        }
        field = field
            .checked_add(4 + u16_at(bytes, field + 2))
            .filter(|next| *next <= end)
            .ok_or("Firmware ZIP has malformed extra data")?;
    }
    Ok(())
}

fn directory(bytes: &[u8], end: usize, limits: &Limits) -> Result<usize, String> {
    let count = u16_at(bytes, end + 10);
    if u16_at(bytes, end + 4) != 0
        || u16_at(bytes, end + 6) != 0
        || u16_at(bytes, end + 8) != count
        || count > limits.members
        || count == u16::MAX as usize
    {
        return Err("Firmware ZIP has too many members or uses split/ZIP64 storage".into());
    }
    let size = u32_at(bytes, end + 12);
    let start = u32_at(bytes, end + 16);
    if size > limits.directory || start.checked_add(size) != Some(end) {
        return Err("Firmware ZIP central directory is invalid or excessive".into());
    }
    let mut offset = start;
    for _ in 0..count {
        if offset.checked_add(46).is_none_or(|next| next > end)
            || bytes[offset..offset + 4] != *b"PK\x01\x02"
        {
            return Err("Firmware ZIP has an inconsistent central directory".into());
        }
        let name = u16_at(bytes, offset + 28);
        let extra = u16_at(bytes, offset + 30);
        let comment = u16_at(bytes, offset + 32);
        let next = offset
            .checked_add(46 + name + extra + comment)
            .filter(|next| *next <= end)
            .ok_or("Firmware ZIP directory entry exceeds its envelope")?;
        if u16_at(bytes, offset + 34) != 0
            || [20, 24, 42]
                .into_iter()
                .any(|field| u32_at(bytes, offset + field) == u32::MAX as usize)
        {
            return Err("Firmware ZIP uses unsupported split/ZIP64 members".into());
        }
        let extra_start = offset + 46 + name;
        extra_fields(bytes, extra_start, extra_start + extra)?;
        let local = u32_at(bytes, offset + 42);
        if local.checked_add(30).is_none_or(|next| next > start)
            || bytes[local..local + 4] != *b"PK\x03\x04"
        {
            return Err("Firmware ZIP has an invalid local member offset".into());
        }
        if [18, 22]
            .into_iter()
            .any(|field| u32_at(bytes, local + field) == u32::MAX as usize)
        {
            return Err("Firmware ZIP uses unsupported ZIP64 local members".into());
        }
        let data = local
            .checked_add(30 + u16_at(bytes, local + 26) + u16_at(bytes, local + 28))
            .and_then(|data| data.checked_add(u32_at(bytes, offset + 20)));
        if data.is_none_or(|end| end > start) {
            return Err("Firmware ZIP member exceeds its data envelope".into());
        }
        let extra_start = local + 30 + u16_at(bytes, local + 26);
        extra_fields(bytes, extra_start, extra_start + u16_at(bytes, local + 28))?;
        offset = next;
    }
    if offset != end {
        return Err("Firmware ZIP directory member count is inconsistent".into());
    }
    Ok(count)
}

pub(crate) fn open(bytes: &[u8], limits: Limits) -> Result<zip::ZipArchive<Cursor<&[u8]>>, String> {
    if bytes.len() < 22 || bytes.len() > limits.bytes {
        return Err("Firmware ZIP has an invalid or excessive length".into());
    }
    let end = (bytes.len().saturating_sub(22 + u16::MAX as usize)..=bytes.len() - 22)
        .rev()
        .find(|&offset| {
            bytes[offset..offset + 4] == *b"PK\x05\x06"
                && offset + 22 + u16_at(bytes, offset + 20) == bytes.len()
        })
        .ok_or("Firmware ZIP has no valid end-of-directory record")?;
    let count = directory(bytes, end, &limits)?;

    // zip 2.x retries earlier EOCDs after directory errors, across the entire file.
    // A fixed archive offset makes the allocation-reachable candidates explicit.
    for offset in 0..=bytes.len() - 22 {
        if bytes[offset..offset + 4] != *b"PK\x05\x06"
            || offset + 22 + u16_at(bytes, offset + 20) > bytes.len()
        {
            continue;
        }
        let members = u16_at(bytes, offset + 10);
        let central = u32_at(bytes, offset + 16);
        if (members == u16::MAX as usize || central == u32::MAX as usize)
            && offset >= 20
            && bytes[offset - 20..offset - 16] == *b"PK\x06\x07"
        {
            return Err("Firmware ZIP uses unsupported ZIP64 storage".into());
        }
        let reachable = if members == 0 {
            // Zero totals bypass the fixed offset; zip still allocates from the on-disk count.
            u16_at(bytes, offset + 8) != 0
        } else {
            central.checked_add(4).is_some_and(|next| next <= offset)
                && bytes[central..central + 4] == *b"PK\x01\x02"
        };
        if reachable && offset != end {
            return Err("Firmware ZIP has competing end-of-directory records".into());
        }
    }
    let archive = zip::ZipArchive::with_config(
        Config {
            archive_offset: ArchiveOffset::Known(0),
        },
        Cursor::new(bytes),
    )
    .map_err(|error| format!("Invalid firmware ZIP: {error}"))?;
    if archive.len() != count {
        return Err("Firmware ZIP member count is inconsistent".into());
    }
    Ok(archive)
}

#[cfg(test)]
mod tests {
    use super::{Limits, open};
    use std::io::{Cursor, Read, Write};

    fn limits() -> Limits {
        Limits {
            bytes: 4096,
            members: 3,
            directory: 1024,
        }
    }

    fn archive(data: &[u8]) -> Vec<u8> {
        archive_member("image.bin", data, zip::CompressionMethod::Stored)
    }

    fn archive_member(name: &str, data: &[u8], method: zip::CompressionMethod) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        writer
            .start_file(
                name,
                zip::write::SimpleFileOptions::default().compression_method(method),
            )
            .unwrap();
        writer.write_all(data).unwrap();
        writer.finish().unwrap().into_inner()
    }

    fn repeated_local_extra(pairs: usize) -> Vec<u8> {
        let original = archive_member("a.bin", b"Z", zip::CompressionMethod::Stored);
        let end = original.len() - 22;
        let central = u32::from_le_bytes(original[end + 16..end + 20].try_into().unwrap()) as usize;
        let mut footer = original[end..].to_vec();
        let mut bytes = original[..35].to_vec();
        bytes[28..30].copy_from_slice(&65_532u16.to_le_bytes());
        bytes.extend_from_slice(&[0xfe, 0xca, 0, 0].repeat(16_383));
        bytes.extend_from_slice(b"Z");
        for _ in 0..pairs {
            let central_offset = bytes.len() as u32;
            bytes.extend_from_slice(&original[central..end]);
            footer[16..20].copy_from_slice(&central_offset.to_le_bytes());
            bytes.extend_from_slice(&footer);
        }
        bytes
    }

    #[test]
    fn single_directory_with_large_local_extra_preserves_payload() {
        let bytes = repeated_local_extra(1);
        let mut parsed = open(
            &bytes,
            Limits {
                bytes: 1024 * 1024,
                ..limits()
            },
        )
        .unwrap();
        let mut output = Vec::new();
        parsed
            .by_name("a.bin")
            .unwrap()
            .read_to_end(&mut output)
            .unwrap();
        assert_eq!(output, b"Z");
    }

    #[test]
    fn rejects_4096_competing_directories_with_shared_local_extra() {
        let bytes = repeated_local_extra(4096);
        assert_eq!(
            open(
                &bytes,
                Limits {
                    bytes: 1024 * 1024,
                    ..limits()
                },
            )
            .err()
            .unwrap(),
            "Firmware ZIP has competing end-of-directory records"
        );
    }

    #[test]
    fn rejects_zero_total_nonzero_disk_count_in_comment_before_library_construction() {
        let mut bytes = archive(&[0; 65_536]);
        let end = bytes.len() - 22;
        let mut comment = [0; 64];
        comment[..4].copy_from_slice(b"PK\x05\x06");
        comment[8..10].copy_from_slice(&32_767u16.to_le_bytes());
        bytes[end + 20..end + 22].copy_from_slice(&64u16.to_le_bytes());
        bytes.extend_from_slice(&comment);
        assert_eq!(
            open(
                &bytes,
                Limits {
                    bytes: 128 * 1024,
                    ..limits()
                },
            )
            .err()
            .unwrap(),
            "Firmware ZIP has competing end-of-directory records"
        );
    }

    #[test]
    fn rejects_competing_directory_with_unsupported_compression() {
        let mut bytes = archive(b"firmware");
        let end = bytes.len() - 22;
        let central = u32::from_le_bytes(bytes[end + 16..end + 20].try_into().unwrap()) as usize;
        let mut malformed = bytes.clone();
        malformed[central + 10..central + 12].copy_from_slice(&99u16.to_le_bytes());

        let final_central = bytes.len() as u32;
        bytes.extend_from_slice(&malformed[central..]);
        let final_end = bytes.len() - 22;
        bytes[final_end + 16..final_end + 20].copy_from_slice(&final_central.to_le_bytes());
        assert!(
            open(&bytes, limits()).is_err(),
            "a second directory must not bypass firmware archive validation"
        );
    }

    #[test]
    fn stored_and_deflated_outer_zips_preserve_nested_zip_bytes() {
        let nested = archive(b"nested firmware\n");
        for method in [
            zip::CompressionMethod::Stored,
            zip::CompressionMethod::Deflated,
        ] {
            let bytes = archive_member("actual.zip", &nested, method);
            let mut parsed = open(&bytes, limits()).unwrap();
            let mut output = Vec::new();
            parsed
                .by_name("actual.zip")
                .unwrap()
                .read_to_end(&mut output)
                .unwrap();
            assert_eq!(output, nested, "{method:?}");
        }
    }

    #[test]
    fn rejects_oversized_inconsistent_split_and_zip64_envelopes() {
        let bytes = archive(b"firmware");
        let end = bytes.len() - 22;
        for (field, value) in [(8, 4u16), (10, 4), (4, 1), (6, 1), (10, u16::MAX)] {
            let mut malformed = bytes.clone();
            malformed[end + field..end + field + 2].copy_from_slice(&value.to_le_bytes());
            assert!(open(&malformed, limits()).is_err());
        }
        let mut malformed = bytes.clone();
        malformed[end + 12..end + 16].copy_from_slice(&1u32.to_le_bytes());
        assert!(open(&malformed, limits()).is_err());
        let mut malformed = bytes;
        malformed[end + 16..end + 20].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(open(&malformed, limits()).is_err());
    }

    #[test]
    fn rejects_split_members_zip64_fields_and_invalid_local_offsets() {
        let bytes = archive(b"firmware");
        let end = bytes.len() - 22;
        let central = u32::from_le_bytes(bytes[end + 16..end + 20].try_into().unwrap()) as usize;
        for field in [20, 24, 42] {
            let mut malformed = bytes.clone();
            malformed[central + field..central + field + 4]
                .copy_from_slice(&u32::MAX.to_le_bytes());
            assert!(open(&malformed, limits()).is_err());
        }
        for field in [18, 22] {
            let mut malformed = bytes.clone();
            malformed[field..field + 4].copy_from_slice(&u32::MAX.to_le_bytes());
            assert!(open(&malformed, limits()).is_err());
        }
        let mut malformed = bytes.clone();
        malformed[central + 34..central + 36].copy_from_slice(&1u16.to_le_bytes());
        assert!(open(&malformed, limits()).is_err());
        let mut malformed = bytes.clone();
        malformed[central + 42..central + 46].copy_from_slice(&(central as u32).to_le_bytes());
        assert!(open(&malformed, limits()).is_err());
        for (header, name_field, extra_field, fixed) in [(central, 28, 30, 46), (0, 26, 28, 30)] {
            let mut malformed = bytes.clone();
            malformed[header + name_field..header + name_field + 2].copy_from_slice(&[0, 0]);
            malformed[header + extra_field..header + extra_field + 2]
                .copy_from_slice(&9u16.to_le_bytes());
            malformed[header + fixed..header + fixed + 4].copy_from_slice(&[1, 0, 5, 0]);
            assert!(open(&malformed, limits()).is_err());
        }
    }

    #[test]
    fn rejects_competing_earlier_eocd_before_library_retry_allocation() {
        let mut bytes = archive(&vec![0; 70 * 1024]);
        let data = 30 + "image.bin".len();
        bytes[data..data + 4].copy_from_slice(b"PK\x01\x02");
        let forged = data + 46;
        bytes[forged..forged + 4].copy_from_slice(b"PK\x05\x06");
        bytes[forged + 8..forged + 12].copy_from_slice(&[0xff, 0x7f, 0xff, 0x7f]);
        bytes[forged + 12..forged + 16].copy_from_slice(&46u32.to_le_bytes());
        bytes[forged + 16..forged + 20].copy_from_slice(&(data as u32).to_le_bytes());
        assert!(
            open(
                &bytes,
                Limits {
                    bytes: 128 * 1024,
                    ..limits()
                },
            )
            .is_err()
        );

        let mut bytes = archive(&[0; 256]);
        let forged = data + 100;
        bytes[forged - 20..forged - 16].copy_from_slice(b"PK\x06\x07");
        bytes[forged..forged + 4].copy_from_slice(b"PK\x05\x06");
        bytes[forged + 8..forged + 12].copy_from_slice(&[0xff, 0xff, 0xff, 0xff]);
        assert!(open(&bytes, limits()).is_err());
    }

    #[test]
    fn harmless_eocd_magic_in_member_is_preserved() {
        let payload = b"image PK\x05\x06\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0 payload";
        let bytes = archive(payload);
        let mut parsed = open(&bytes, limits()).unwrap();
        let mut output = Vec::new();
        parsed
            .by_index(0)
            .unwrap()
            .read_to_end(&mut output)
            .unwrap();
        assert_eq!(output, payload);
    }
}
