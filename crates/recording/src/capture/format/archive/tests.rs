use super::{SizeChecked, export_csv_zip, safe_zip_name};
use crate::capture::Recording;
use crate::capture::format::load;
use crate::capture::format::test_support::{CSV, META, cancel, fixture};
use std::io::{Cursor, Write};
use std::sync::atomic::AtomicBool;
#[test]
fn csv_zip_pair_roundtrip_preserves_freshness_and_unknown_metadata() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("pairs.zip");
    let a = fixture();
    let mut b = fixture();
    b.identity.receiver = "second-receiver".into();
    b.samples[0].temperature_c = Some(0.);
    b.samples[1].temperature_c = Some(-8.);
    let recording = Recording {
        tracks: vec![a, b],
        warnings: vec!["Partial capture".into()],
        provenance: vec!["original receiver".into()],
    };
    export_csv_zip(&recording, &path, &cancel()).unwrap();
    let loaded = load(&path, &cancel()).unwrap();
    assert_eq!(loaded.tracks.len(), 2);
    assert_ne!(
        loaded.tracks[0].identity.receiver,
        loaded.tracks[1].identity.receiver
    );
    assert_eq!(loaded.tracks[0].samples[0].temperature_c, None);
    assert_eq!(loaded.tracks[1].samples[0].temperature_c, Some(0.));
    assert_eq!(loaded.tracks[1].samples[1].temperature_c, Some(-8.));
    assert!(!loaded.tracks[0].samples[2].acceleration_fresh);
    assert_eq!(loaded.tracks[0].samples[2].acceleration_g, [0.; 3]);
    assert!(loaded.warnings.iter().any(|w| w == "Partial capture"));
    assert!(loaded.provenance.iter().any(|p| p == "original receiver"));
    assert_eq!(
        loaded.tracks[0].metadata.extra["calibration_status"],
        "unmeasured_identity_no_external_correction"
    );
}
#[test]
fn unsafe_archive_names_and_orphan_metadata_are_rejected() {
    for name in [
        "../escape.csv",
        "/abs.csv",
        "C:drive.csv",
        "a\\b.csv",
        "a/../b.csv",
        "a//b.csv",
    ] {
        assert!(!safe_zip_name(name), "{name}");
    }
    assert!(safe_zip_name("safe/tracker.csv"));
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("unsafe.zip");
    let file = std::fs::File::create(&path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    zip.start_file("../escape.csv", zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(CSV.as_bytes()).unwrap();
    zip.finish().unwrap();
    assert!(load(&path, &cancel()).is_err());
    let path = directory.path().join("orphan.zip");
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
    zip.start_file("orphan.meta.txt", zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(META.as_bytes()).unwrap();
    zip.finish().unwrap();
    assert!(load(&path, &cancel()).is_err());
}
#[test]
fn cancelled_export_preserves_existing_destination() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("unchanged.zip");
    std::fs::write(&path, b"existing").unwrap();
    assert!(
        export_csv_zip(
            &Recording {
                tracks: vec![fixture()],
                ..Recording::default()
            },
            &path,
            &AtomicBool::new(true)
        )
        .is_err()
    );
    assert_eq!(std::fs::read(path).unwrap(), b"existing");
}
#[test]
fn actual_decompressed_size_must_match_archive_declaration() {
    use std::io::Read;
    let mut buffer = Vec::new();
    let mut oversized = SizeChecked {
        inner: Cursor::new(b"1234"),
        remaining: 3,
    };
    assert!(oversized.read_to_end(&mut buffer).is_err());
    let mut truncated = super::SizeChecked {
        inner: Cursor::new(b"12"),
        remaining: 3,
    };
    assert!(truncated.read_to_end(&mut buffer).is_err());
}

fn zip_bytes(members: &[(&str, &[u8])]) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, bytes) in members {
        zip.start_file(*name, zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap().into_inner()
}

fn central_offset(bytes: &[u8]) -> usize {
    bytes
        .windows(4)
        .position(|signature| signature == [0x50, 0x4b, 0x01, 0x02])
        .unwrap()
}

#[test]
fn orphan_metadata_is_rejected_before_attempting_inflate() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("orphan-corrupt.zip");
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    zip.start_file(
        "orphan.meta.txt",
        zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated),
    )
    .unwrap();
    zip.write_all(META.as_bytes()).unwrap();
    let mut bytes = zip.finish().unwrap().into_inner();
    let name = u16::from_le_bytes(bytes[26..28].try_into().unwrap()) as usize;
    let extra = u16::from_le_bytes(bytes[28..30].try_into().unwrap()) as usize;
    bytes[30 + name + extra] = 0xff;
    std::fs::write(&path, bytes).unwrap();
    assert_eq!(
        load(&path, &cancel()).unwrap_err(),
        "ZIP has no recordings or contains unpaired metadata"
    );
}

#[test]
fn archive_track_limit_accepts_512_csvs_and_pairs_but_rejects_513_csvs() {
    const TINY: &[u8] = b"seq,gx,gy,gz,ax,ay,az,mx,my,mz\n0,0,0,0,0,0,0,0,0,0\n";
    let directory = tempfile::tempdir().unwrap();
    for (count, paired) in [(512, false), (513, false), (512, true)] {
        let path = directory
            .path()
            .join(format!("tracks-{count}-{paired}.zip"));
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        for index in 0..count {
            zip.start_file(
                format!("track-{index:03}.csv"),
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
            zip.write_all(TINY).unwrap();
            if paired {
                zip.start_file(
                    format!("track-{index:03}.meta.txt"),
                    zip::write::SimpleFileOptions::default(),
                )
                .unwrap();
                zip.write_all(META.as_bytes()).unwrap();
            }
        }
        zip.finish().unwrap();
        if count == 513 {
            assert_eq!(
                load(&path, &cancel()).unwrap_err(),
                "ZIP has too many recording tracks"
            );
        } else {
            let recording = load(&path, &cancel()).unwrap();
            assert_eq!(recording.tracks.len(), 512);
            assert_eq!(recording.tracks[0].samples[0].sequence, 0);
            assert_eq!(recording.tracks[511].samples[0].sequence, 0);
            let exported = directory.path().join(format!("accepted-{paired}.zip"));
            export_csv_zip(&recording, &exported, &cancel()).unwrap();
            assert_eq!(load(&exported, &cancel()).unwrap().tracks.len(), 512);
        }
    }
}

#[test]
fn duplicate_member_names_are_rejected_before_loading_samples() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("duplicates.zip");
    let mut bytes = zip_bytes(&[("one.csv", CSV.as_bytes()), ("two.csv", CSV.as_bytes())]);
    let positions = bytes
        .windows(b"two.csv".len())
        .enumerate()
        .filter_map(|(index, bytes)| (bytes == b"two.csv").then_some(index))
        .collect::<Vec<_>>();
    assert_eq!(positions.len(), 2);
    for index in positions {
        bytes[index..index + 7].copy_from_slice(b"one.csv");
    }
    std::fs::write(&path, bytes).unwrap();
    assert!(
        load(&path, &cancel())
            .unwrap_err()
            .contains("duplicate names")
    );
}

#[test]
fn paired_archive_crc_and_zero_size_declaration_are_checked() {
    let directory = tempfile::tempdir().unwrap();
    let original = zip_bytes(&[
        ("track.csv", CSV.as_bytes()),
        ("track.meta.txt", META.as_bytes()),
    ]);
    let central = central_offset(&original);
    let mut checksum = original.clone();
    checksum[central + 16] ^= 1;
    let path = directory.path().join("crc.zip");
    std::fs::write(&path, checksum).unwrap();
    assert!(
        load(&path, &cancel()).is_err(),
        "a corrupted member checksum must reject the recording"
    );
    let mut size = original;
    size[22..26].copy_from_slice(&0u32.to_le_bytes());
    size[central + 24..central + 28].copy_from_slice(&0u32.to_le_bytes());
    let path = directory.path().join("false-zero.zip");
    std::fs::write(&path, size).unwrap();
    assert!(
        load(&path, &cancel()).is_err(),
        "a member with an incorrect zero-size declaration must reject the recording"
    );
}

#[test]
fn uncompressed_export_budget_spans_csv_and_metadata_members_transactionally() {
    let directory = tempfile::tempdir().unwrap();
    let source = Recording {
        tracks: vec![fixture(), fixture()],
        warnings: vec!["synthetic export context".into()],
        ..Recording::default()
    };
    let path = directory.path().join("reference.zip");
    export_csv_zip(&source, &path, &cancel()).unwrap();
    let mut archive = zip::ZipArchive::new(std::fs::File::open(&path).unwrap()).unwrap();
    assert_eq!(archive.len(), 4);
    let total = (0..4)
        .map(|index| archive.by_index(index).unwrap().size())
        .sum::<u64>();
    let destination = directory.path().join("bounded.zip");
    super::export_with_limit(&source, &destination, &cancel(), total).unwrap();
    assert_eq!(load(&destination, &cancel()).unwrap().tracks.len(), 2);
    std::fs::write(&destination, b"existing destination").unwrap();
    assert!(
        super::export_with_limit(&source, &destination, &cancel(), total - 1)
            .unwrap_err()
            .contains("total uncompressed limit")
    );
    assert_eq!(
        std::fs::read(&destination).unwrap(),
        b"existing destination"
    );
}

#[test]
fn uncompressed_export_counter_keeps_one_budget_across_member_writers() {
    let mut remaining = 6;
    let mut csv = Vec::new();
    let mut metadata = Vec::new();
    super::ExportBytes {
        writer: &mut csv,
        remaining: &mut remaining,
    }
    .write_all(b"abc")
    .unwrap();
    super::ExportBytes {
        writer: &mut metadata,
        remaining: &mut remaining,
    }
    .write_all(b"def")
    .unwrap();
    assert!(
        super::ExportBytes {
            writer: &mut metadata,
            remaining: &mut remaining,
        }
        .write_all(b"g")
        .is_err()
    );
    assert_eq!(csv, b"abc");
    assert_eq!(metadata, b"def");
}

#[test]
fn shared_context_entry_budget_preserves_duplicate_diagnostics_and_provenance() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("context-entries.zip");
    let mut recording = Recording {
        tracks: vec![fixture(), fixture()],
        warnings: vec!["repeated diagnostic".into(); 4098],
        provenance: vec!["repeated provenance".into(); 4096],
    };
    export_csv_zip(&recording, &path, &cancel()).unwrap();
    let restored = load(&path, &cancel()).unwrap();
    assert_eq!(restored.warnings, recording.warnings);
    assert_eq!(restored.provenance, recording.provenance);
    recording
        .provenance
        .push("one over the total budget".into());
    std::fs::write(&path, b"unchanged context destination").unwrap();
    assert!(
        export_csv_zip(&recording, &path, &cancel())
            .unwrap_err()
            .contains("context entry or byte limit")
    );
    assert_eq!(
        std::fs::read(path).unwrap(),
        b"unchanged context destination"
    );
}

#[test]
fn shared_context_byte_budget_roundtrips_at_boundary_without_import_growth() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("context-bytes.zip");
    let mut recording = Recording {
        tracks: vec![fixture()],
        warnings: vec!["x".repeat(512 * 1024 - 7)],
        ..Recording::default()
    };
    export_csv_zip(&recording, &path, &cancel()).unwrap();
    let restored = load(&path, &cancel()).unwrap();
    assert_eq!(restored.warnings, recording.warnings);
    assert_eq!(restored.provenance, recording.provenance);
    recording.warnings[0].push('x');
    std::fs::write(&path, b"unchanged byte destination").unwrap();
    assert!(
        export_csv_zip(&recording, &path, &cancel())
            .unwrap_err()
            .contains("context entry or byte limit")
    );
    assert_eq!(std::fs::read(path).unwrap(), b"unchanged byte destination");
}

#[test]
fn public_csv_import_rejects_context_lists_before_exceeding_total_budget() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("context.csv");
    let metadata = path.with_extension("meta.txt");
    std::fs::write(&path, CSV).unwrap();
    let entries = serde_json::to_string(&vec![""; 8195]).unwrap();
    std::fs::write(
        &metadata,
        format!("{META}capture_warnings_json={entries}\n"),
    )
    .unwrap();
    assert!(
        load(&path, &cancel())
            .unwrap_err()
            .contains("Capture context entry limit exceeded")
    );
    let oversized = serde_json::to_string(&vec!["x".repeat(512 * 1024)]).unwrap();
    std::fs::write(
        &metadata,
        format!("{META}capture_warnings_json={oversized}\n"),
    )
    .unwrap();
    assert_eq!(
        load(&path, &cancel()).unwrap_err(),
        "Capture context byte limit exceeded"
    );
}
