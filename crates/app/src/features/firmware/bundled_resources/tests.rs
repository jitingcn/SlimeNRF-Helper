use super::reading::{read_bounded, read_member};
use super::{Member, Resource, canonical_member};
use sha2::{Digest, Sha256};
use std::{
    io::{Cursor, Read, Write},
    sync::atomic::{AtomicBool, Ordering},
};

fn archive(bytes: &[u8]) -> (Vec<u8>, Member) {
    let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
    archive
        .start_file(
            "firmwares/board.bin",
            zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored),
        )
        .unwrap();
    archive.write_all(bytes).unwrap();
    let member = Member {
        name: "firmwares/board.bin",
        len: bytes.len() as u64,
        sha256: Sha256::digest(bytes).into(),
    };
    (archive.finish().unwrap().into_inner(), member)
}

#[test]
fn generated_bundle_always_keeps_configuration_and_matches_firmware_feature() {
    let cancel = AtomicBool::new(false);
    let config = Resource::bundled("slimenrf-helper.yaml").unwrap();
    assert!(!config.is_intentionally_omitted());
    assert!(
        !config
            .read(4 * 1024 * 1024, &cancel)
            .unwrap()
            .bytes
            .is_empty()
    );
    let archive = zip::ZipArchive::new(Cursor::new(super::ARCHIVE)).unwrap();
    assert_eq!(archive.len(), super::MEMBERS.len());
    if cfg!(feature = "embed-firmware") {
        assert!(
            super::MEMBERS
                .iter()
                .any(|member| member.name.starts_with("firmwares/"))
        );
    } else {
        assert_eq!(super::MEMBERS.len(), 1);
        assert_eq!(super::MEMBERS[0].name, "slimenrf-helper.yaml");
        assert!(super::MISSING_CONFIGURED_MEMBERS.is_empty());
    }
}

#[test]
fn aliases_are_unambiguous_and_paths_cannot_escape_the_bundle() {
    let members = [Member {
        name: "firmwares/kawazu_tracker.uf2",
        len: 0,
        sha256: [0; 32],
    }];
    assert_eq!(
        canonical_member("./FiRmWaReS\\Kawazu_Tracker.uf2", &members).unwrap(),
        members[0].name
    );
    assert_eq!(
        canonical_member("SLIMENRF-HELPER.YAML", &members).unwrap(),
        "slimenrf-helper.yaml"
    );
    assert_eq!(
        canonical_member("Firmwares/missing.bin", &members).unwrap(),
        "firmwares/missing.bin"
    );
    for invalid in [
        "config.yaml",
        "../slimenrf-helper.yaml",
        "/slimenrf-helper.yaml",
        "C:/firmwares/a.bin",
        "firmwares/../a.bin",
        "firmwares//a.bin",
        "firmwares/./a.bin",
        "firmwares/a/b.bin",
        "firmwares/a\0.bin",
        "firmwares/a\n.bin",
    ] {
        assert!(canonical_member(invalid, &members).is_err(), "{invalid}");
    }
    let collision = [
        members.into_iter().next().unwrap(),
        Member {
            name: "firmwares/Kawazu_Tracker.uf2",
            len: 0,
            sha256: [0; 32],
        },
    ];
    assert!(canonical_member("firmwares/kawazu_tracker.uf2", &collision).is_err());
    assert!(
        Resource::File("firmwares/Kawazu_Tracker.uf2".into())
            != Resource::File("firmwares/kawazu_tracker.uf2".into())
    );
}

#[test]
fn member_reads_enforce_limits_cancel_integrity_and_availability() {
    let bytes = b"actual firmware bytes";
    let (archive, mut member) = archive(bytes);
    let cancel = AtomicBool::new(false);
    assert_eq!(
        read_member(
            &archive,
            std::slice::from_ref(&member),
            member.name,
            100,
            &cancel
        )
        .unwrap()
        .bytes,
        bytes
    );
    assert!(
        read_member(
            &archive,
            std::slice::from_ref(&member),
            member.name,
            2,
            &cancel
        )
        .is_err()
    );
    assert!(
        read_member(
            &archive,
            std::slice::from_ref(&member),
            "firmwares/missing.bin",
            100,
            &cancel
        )
        .is_err()
    );
    cancel.store(true, Ordering::Release);
    assert!(
        read_member(
            &archive,
            std::slice::from_ref(&member),
            member.name,
            100,
            &cancel
        )
        .is_err()
    );
    cancel.store(false, Ordering::Release);
    let mut corrupt = archive.clone();
    let payload_offset = corrupt
        .windows(bytes.len())
        .position(|chunk| chunk == bytes)
        .unwrap();
    corrupt[payload_offset] ^= 1;
    assert!(
        read_member(
            &corrupt,
            std::slice::from_ref(&member),
            member.name,
            100,
            &cancel
        )
        .is_err()
    );
    member.sha256[0] ^= 1;
    assert!(
        read_member(
            &archive,
            std::slice::from_ref(&member),
            member.name,
            100,
            &cancel
        )
        .is_err()
    );
    member.len += 1;
    assert!(
        read_member(
            &archive,
            std::slice::from_ref(&member),
            member.name,
            100,
            &cancel
        )
        .is_err()
    );
    assert!(
        read_member(
            b"corrupt zip",
            &[member],
            "firmwares/board.bin",
            100,
            &cancel
        )
        .is_err()
    );
    assert!(read_bounded(Cursor::new(bytes), 1, 2, &cancel).is_err());
}

#[test]
fn cancellation_during_read_does_not_publish_partial_bytes() {
    struct CancelOnRead<'a>(&'a AtomicBool);
    impl Read for CancelOnRead<'_> {
        fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
            output[0] = 42;
            self.0.store(true, Ordering::Release);
            Ok(1)
        }
    }
    let cancel = AtomicBool::new(false);
    assert!(read_bounded(CancelOnRead(&cancel), 1, 100, &cancel).is_err());
}

#[test]
fn firmware_inventory_contains_only_present_bundled_members() {
    let resources: Vec<_> = super::firmware_resources().collect();
    assert_eq!(
        resources.len(),
        super::MEMBERS
            .iter()
            .filter(|member| member.name.starts_with("firmwares/"))
            .count()
    );
    for resource in resources {
        let Resource::Bundled(name) = resource else {
            panic!("Bundled inventory must never contain disk resources");
        };
        assert!(super::MEMBERS.iter().any(|member| member.name == name));
        assert!(
            !super::MISSING_CONFIGURED_MEMBERS
                .iter()
                .any(|missing| *missing == name)
        );
    }
    if !cfg!(feature = "embed-firmware") {
        assert_eq!(super::firmware_resources().count(), 0);
    }
}
