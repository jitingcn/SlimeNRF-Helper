use super::{Directory, DirectoryReader};
use crate::capture::format::load;
use crate::capture::{Gyro, Recording, Sample};
use std::cell::Cell;
use std::io::{self, Cursor, Read, Seek, SeekFrom, Write};
use std::sync::atomic::AtomicBool;

const CSV: &str = "seq,gx,gy,gz,ax,ay,az,mx,my,mz,note\n7,1.25,-2.5,3.75,0.5,0,1,0,0,0,ordinary\n";

struct ZipFixture {
    bytes: Vec<u8>,
    directory: usize,
    end: usize,
    payload: usize,
}

impl ZipFixture {
    fn stored(name: &str, payload: &[u8]) -> Self {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        writer
            .start_file(
                name,
                zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Stored),
            )
            .unwrap();
        writer.write_all(payload).unwrap();
        let bytes = writer.finish().unwrap().into_inner();
        let end = bytes.len() - 22;
        let directory = u32_at(&bytes, end + 16) as usize;
        let payload = 30 + usize::from(u16_at(&bytes, 26)) + usize::from(u16_at(&bytes, 28));
        Self {
            bytes,
            directory,
            end,
            payload,
        }
    }

    fn insert_directory_bytes(&mut self, offset: usize, extra: &[u8]) {
        self.bytes.splice(offset..offset, extra.iter().copied());
        self.end += extra.len();
        put_u32(
            &mut self.bytes,
            self.end + 12,
            (self.end - self.directory) as u32,
        );
    }

    fn add_central_extra(&mut self, extra: &[u8]) {
        let name = usize::from(u16_at(&self.bytes, self.directory + 28));
        let previous = usize::from(u16_at(&self.bytes, self.directory + 30));
        self.insert_directory_bytes(self.directory + 46 + name + previous, extra);
        put_u16(
            &mut self.bytes,
            self.directory + 30,
            (previous + extra.len()) as u16,
        );
    }

    fn add_comment(&mut self, comment: &[u8]) {
        put_u16(&mut self.bytes, self.end + 20, comment.len() as u16);
        self.bytes.extend_from_slice(comment);
    }
}

fn u16_at(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn put_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn end_record(count: u16, size: u32, start: u32) -> Vec<u8> {
    let mut bytes = vec![0; 22];
    bytes[..4].copy_from_slice(b"PK\x05\x06");
    put_u16(&mut bytes, 8, count);
    put_u16(&mut bytes, 10, count);
    put_u32(&mut bytes, 12, size);
    put_u32(&mut bytes, 16, start);
    bytes
}

fn directory_count(bytes: &[u8]) -> Result<usize, String> {
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(bytes).unwrap();
    Directory::read(&mut file, &AtomicBool::new(false)).map(|directory| directory.count)
}

fn load_bytes(bytes: &[u8]) -> Result<Recording, String> {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("recording.zip");
    std::fs::write(&path, bytes).unwrap();
    load(&path, &AtomicBool::new(false))
}

fn assert_preflight_rejection(bytes: &[u8], expected: &str) {
    assert_eq!(directory_count(bytes), Err(expected.to_owned()));
    assert_eq!(load_bytes(bytes).unwrap_err(), expected);
}

fn assert_loaded_sample(bytes: &[u8]) {
    let recording = load_bytes(bytes).unwrap();
    assert_eq!(recording.tracks.len(), 1);
    assert_eq!(
        recording.tracks[0].samples,
        [Sample {
            sequence: 7,
            gyro: Gyro::RatesDps([1.25, -2.5, 3.75]),
            acceleration_g: [0.5, 0., 1.],
            acceleration_fresh: true,
            ..Sample::default()
        }]
    );
}

#[test]
fn declared_entry_limit_precedes_directory_parsing_and_size_allocation() {
    assert_preflight_rejection(
        &end_record(1025, u32::MAX, u32::MAX),
        "Recording ZIP has too many entries or uses split/ZIP64 storage",
    );
    assert_eq!(
        directory_count(&end_record(1024, u32::MAX, u32::MAX)),
        Err("Recording ZIP central directory is invalid or excessive".into())
    );
}

#[test]
fn split_disks_and_disagreeing_eocd_counts_are_rejected_before_members() {
    for (field, value) in [(4, 1), (6, 1), (8, 0), (8, 2)] {
        let mut fixture = ZipFixture::stored("tracker.csv", CSV.as_bytes());
        put_u16(&mut fixture.bytes, fixture.end + field, value);
        assert_preflight_rejection(
            &fixture.bytes,
            "Recording ZIP has too many entries or uses split/ZIP64 storage",
        );
    }
    let mut fixture = ZipFixture::stored("tracker.csv", CSV.as_bytes());
    put_u16(&mut fixture.bytes, fixture.directory + 34, 1);
    assert_preflight_rejection(
        &fixture.bytes,
        "Recording ZIP member name or disk is invalid",
    );
}

#[test]
fn declared_directory_byte_limit_precedes_central_entry_parsing() {
    for (size, expected) in [
        (
            4 * 1024 * 1024 + 1,
            "Recording ZIP central directory is invalid or excessive",
        ),
        (
            4 * 1024 * 1024,
            "Recording ZIP has an inconsistent central directory",
        ),
    ] {
        let mut file = tempfile::tempfile().unwrap();
        file.set_len(u64::from(size) + 22).unwrap();
        file.seek(SeekFrom::Start(u64::from(size))).unwrap();
        file.write_all(&end_record(1, size, 0)).unwrap();
        assert_eq!(
            Directory::read(&mut file, &AtomicBool::new(false)).map(|directory| directory.count),
            Err(expected.to_owned())
        );
    }
}

#[test]
fn central_directory_count_and_envelope_must_match() {
    for (count, expected) in [
        (0, "Recording ZIP directory member count is inconsistent"),
        (2, "Recording ZIP has an inconsistent central directory"),
    ] {
        let mut fixture = ZipFixture::stored("tracker.csv", CSV.as_bytes());
        put_u16(&mut fixture.bytes, fixture.end + 8, count);
        put_u16(&mut fixture.bytes, fixture.end + 10, count);
        assert_preflight_rejection(&fixture.bytes, expected);
    }
    for field in [12, 16] {
        let mut fixture = ZipFixture::stored("tracker.csv", CSV.as_bytes());
        let declared = u32_at(&fixture.bytes, fixture.end + field);
        put_u32(&mut fixture.bytes, fixture.end + field, declared + 1);
        assert_preflight_rejection(
            &fixture.bytes,
            "Recording ZIP central directory is invalid or excessive",
        );
    }
    let mut fixture = ZipFixture::stored("tracker.csv", CSV.as_bytes());
    fixture.insert_directory_bytes(fixture.end, &[0]);
    assert_preflight_rejection(
        &fixture.bytes,
        "Recording ZIP directory member count is inconsistent",
    );
}

#[test]
fn zip64_sentinel_fields_are_rejected_without_relocation() {
    assert_preflight_rejection(
        &end_record(u16::MAX, 0, 0),
        "Recording ZIP has too many entries or uses split/ZIP64 storage",
    );
    for field in [12, 16] {
        let mut fixture = ZipFixture::stored("tracker.csv", CSV.as_bytes());
        put_u32(&mut fixture.bytes, fixture.end + field, u32::MAX);
        assert_preflight_rejection(
            &fixture.bytes,
            "Recording ZIP central directory is invalid or excessive",
        );
    }
    for field in [20, 24, 42] {
        let mut fixture = ZipFixture::stored("tracker.csv", CSV.as_bytes());
        put_u32(&mut fixture.bytes, fixture.directory + field, u32::MAX);
        assert_preflight_rejection(
            &fixture.bytes,
            "Recording ZIP uses invalid offsets or unsupported ZIP64 storage",
        );
    }
}

#[test]
fn forged_zip64_locator_and_central_zip64_extra_are_rejected() {
    let mut fixture = ZipFixture::stored("tracker.csv", CSV.as_bytes());
    let mut locator = [0; 20];
    locator[..4].copy_from_slice(b"PK\x06\x07");
    locator[8..16].copy_from_slice(&u64::MAX.to_le_bytes());
    put_u32(&mut locator, 16, 1);
    fixture.insert_directory_bytes(fixture.end, &locator);
    assert_preflight_rejection(
        &fixture.bytes,
        "Recording ZIP uses unsupported ZIP64 storage",
    );

    let mut fixture = ZipFixture::stored("tracker.csv", CSV.as_bytes());
    let mut extra = [0; 28];
    put_u16(&mut extra, 0, 1);
    put_u16(&mut extra, 2, 24);
    fixture.add_central_extra(&extra);
    assert_preflight_rejection(
        &fixture.bytes,
        "Recording ZIP uses unsupported ZIP64 storage",
    );
}

#[test]
fn central_extra_fields_must_fit_their_declared_envelope() {
    for extra in [&[0x34, 0x12, 0][..], &[0x34, 0x12, 1, 0][..]] {
        let mut fixture = ZipFixture::stored("tracker.csv", CSV.as_bytes());
        fixture.add_central_extra(extra);
        assert_preflight_rejection(&fixture.bytes, "Recording ZIP has malformed extra fields");
    }
    let mut fixture = ZipFixture::stored("tracker.csv", CSV.as_bytes());
    fixture.add_central_extra(&[0x34, 0x12, 1, 0, 0xff]);
    assert_loaded_sample(&fixture.bytes);
}

#[test]
fn eocd_must_end_at_its_declared_comment_boundary() {
    let mut fixture = ZipFixture::stored("tracker.csv", CSV.as_bytes());
    fixture.bytes.extend_from_slice(b"trailing bytes");
    assert_preflight_rejection(
        &fixture.bytes,
        "Recording ZIP has no valid end-of-directory record",
    );

    let mut fixture = ZipFixture::stored("tracker.csv", CSV.as_bytes());
    put_u16(&mut fixture.bytes, fixture.end + 20, 1);
    assert_preflight_rejection(
        &fixture.bytes,
        "Recording ZIP has no valid end-of-directory record",
    );
}

#[test]
fn competing_eocd_in_comment_is_rejected_but_normal_comment_loads() {
    let mut fixture = ZipFixture::stored("safe/tracker.csv", CSV.as_bytes());
    fixture.add_comment(b"A normal ZIP comment.\n");
    assert_loaded_sample(&fixture.bytes);

    let mut fixture = ZipFixture::stored("tracker.csv", CSV.as_bytes());
    let mut comment = b"An alternate record follows. ".to_vec();
    comment.extend_from_slice(&end_record(0, 0, 0));
    comment.push(b'!');
    fixture.add_comment(&comment);
    assert_preflight_rejection(
        &fixture.bytes,
        "Recording ZIP has competing end-of-directory records",
    );
}

#[test]
fn eocd_signature_in_stored_csv_note_does_not_hide_samples() {
    let fixture = ZipFixture::stored(
        "safe/tracker.csv",
        b"seq,gx,gy,gz,ax,ay,az,mx,my,mz,note\n7,1.25,-2.5,3.75,0.5,0,1,0,0,0,PK\x05\x06\n",
    );
    assert_loaded_sample(&fixture.bytes);
}

#[test]
fn invalid_outer_directory_cannot_load_a_nested_recording() {
    let nested = ZipFixture::stored("earlier.csv", CSV.as_bytes());
    assert_loaded_sample(&nested.bytes);
    let mut fixture = ZipFixture::stored("actual.csv", &nested.bytes);
    let nested_start = fixture.payload;
    // Keep the embedded recording addressable using absolute file offsets.
    put_u32(
        &mut fixture.bytes,
        nested_start + nested.directory + 42,
        nested_start as u32,
    );
    put_u32(
        &mut fixture.bytes,
        nested_start + nested.end + 16,
        (nested_start + nested.directory) as u32,
    );
    fixture.add_central_extra(&[0x01, 0x99, 0, 0]);
    assert_eq!(directory_count(&fixture.bytes), Ok(1));

    assert!(
        load_bytes(&fixture.bytes).is_err(),
        "malformed outer ZIP must not load the embedded recording"
    );
}

#[test]
fn constructor_reader_blocks_payload_and_keeps_an_immutable_directory_snapshot() {
    let fixture = ZipFixture::stored("tracker.csv", CSV.as_bytes());
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(&fixture.bytes).unwrap();
    let directory = Directory::read(&mut file, &AtomicBool::new(false)).unwrap();
    file.seek(SeekFrom::Start(fixture.directory as u64))
        .unwrap();
    file.write_all(b"gone").unwrap();
    let constructing = Cell::new(true);
    let mut reader = DirectoryReader::new(&mut file, &directory, &constructing);
    reader
        .seek(SeekFrom::Start(fixture.payload as u64))
        .unwrap();
    let mut bytes = [0; 4];
    let error = reader.read_exact(&mut bytes).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Other);
    assert_eq!(
        error.to_string(),
        "ZIP constructor left its validated directory"
    );
    reader
        .seek(SeekFrom::Start(fixture.directory as u64))
        .unwrap();
    reader.read_exact(&mut bytes).unwrap();
    assert_eq!(&bytes, b"PK\x01\x02");

    constructing.set(false);
    reader
        .seek(SeekFrom::Start(fixture.directory as u64))
        .unwrap();
    reader.read_exact(&mut bytes).unwrap();
    assert_eq!(&bytes, b"gone");
    reader
        .seek(SeekFrom::Start(fixture.payload as u64))
        .unwrap();
    reader.read_exact(&mut bytes).unwrap();
    assert_eq!(&bytes, b"seq,");
}

#[test]
fn original_member_crc_and_safe_path_rules_still_apply_after_construction() {
    let fixture = ZipFixture::stored("safe/tracker.csv", CSV.as_bytes());
    assert_loaded_sample(&fixture.bytes);

    let mut damaged = fixture;
    damaged.bytes[damaged.payload + CSV.find("1.25").unwrap()] = b'9';
    assert_eq!(load_bytes(&damaged.bytes).unwrap_err(), "Invalid checksum");

    let unsafe_path = ZipFixture::stored("../tracker.csv", CSV.as_bytes());
    assert_eq!(
        load_bytes(&unsafe_path.bytes).unwrap_err(),
        "Unsafe ZIP member: ../tracker.csv"
    );
}
