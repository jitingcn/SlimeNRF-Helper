use super::{WritePurpose, inspect_volume, validate_for_volume, write};
use crate::firmware::Nrf52Chip;
use std::{fs, sync::atomic::AtomicBool};

fn set_word(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

pub(super) fn updater(chip: Nrf52Chip) -> Vec<u8> {
    let (flash, ram, family) = match chip {
        Nrf52Chip::Nrf52833 => (0x80000, 0x20000, 0x621e937a),
        Nrf52Chip::Nrf52840 => (0x100000, 0x40000, 0xada52840),
    };
    let mut bytes = vec![0; 3 * 512];
    for (index, address) in [flash - 0xc000, flash - 0x2800, 0x10001000]
        .into_iter()
        .enumerate()
    {
        let block = &mut bytes[index * 512..(index + 1) * 512];
        for (offset, value) in [
            (0, 0x0a324655),
            (4, 0x9e5d5157),
            (8, 0x2000),
            (12, address),
            (16, 256),
            (20, index as u32),
            (24, 3),
            (28, 0xd663823c),
            (508, 0x0ab16f30),
        ] {
            set_word(block, offset, value);
        }
    }
    for (index, value) in [
        0x1e9e10f1, 0x20227a79, 5, 100, 204, flash, 205, ram, 208, 0x12097693, 209, family, 210,
        0x20,
    ]
    .into_iter()
    .enumerate()
    {
        set_word(&mut bytes, 512 + 32 + index * 4, value);
    }
    set_word(&mut bytes, 1024 + 32 + 0x14, flash - 0xc000);
    set_word(&mut bytes, 1024 + 32 + 0x18, flash - 0x2000);
    bytes
}

#[test]
fn bootloader_authorization_is_chip_specific_and_separate_from_application() {
    for chip in [Nrf52Chip::Nrf52833, Nrf52Chip::Nrf52840] {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join("INFO_UF2.TXT"),
            "Board-ID: known-board\n",
        )
        .unwrap();
        let volume = inspect_volume(directory.path(), "INFO_UF2.TXT").unwrap();
        let bytes = updater(chip);
        let purpose = WritePurpose::Bootloader(chip);
        validate_for_volume(&volume, &bytes, purpose).unwrap();
        assert!(validate_for_volume(&volume, &bytes, WritePurpose::Application).is_err());
        let other = match chip {
            Nrf52Chip::Nrf52833 => Nrf52Chip::Nrf52840,
            Nrf52Chip::Nrf52840 => Nrf52Chip::Nrf52833,
        };
        assert!(validate_for_volume(&volume, &bytes, WritePurpose::Bootloader(other)).is_err());
        let mut application = bytes.clone();
        for block in application.as_chunks_mut::<512>().0 {
            set_word(block, 28, chip.application_family());
        }
        assert!(validate_for_volume(&volume, &application, purpose).is_err());
        // A filename is not an execution-layer authorization source.
        write(
            &volume,
            "pinned-custom-name.uf2",
            &bytes,
            purpose,
            &AtomicBool::new(false),
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(
            fs::read(directory.path().join("pinned-custom-name.uf2")).unwrap(),
            bytes
        );
    }
}

#[test]
fn updater_rejects_conflicting_missing_or_unflashed_chip_metadata() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(
        directory.path().join("INFO_UF2.TXT"),
        "Board-ID: known-board\n",
    )
    .unwrap();
    let volume = inspect_volume(directory.path(), "INFO_UF2.TXT").unwrap();
    let purpose = WritePurpose::Bootloader(Nrf52Chip::Nrf52840);
    let baseline = updater(Nrf52Chip::Nrf52840);
    for (offset, value) in [
        (512 + 32, 0),                   // no CF2 magic
        (512 + 32 + 8, 0),               // no used entries
        (512 + 32 + 12, u32::MAX),       // unbounded capacity
        (512 + 32 + 8, 32),              // used entries absent from UF2
        (512 + 32 + 5 * 4, 0x80000),     // wrong flash capacity
        (512 + 32 + 7 * 4, 0x20000),     // wrong RAM capacity
        (512 + 32 + 11 * 4, 0x621e937a), // wrong application family
        (512 + 32 + 10 * 4, 204),        // duplicate flash, missing family
        (512 + 8, 0x2001),               // NOFLASH cannot attest CF2
        (1024 + 32 + 0x14, 0x74000),     // wrong UICR boot address
        (1024 + 32 + 0x18, 0x7e000),     // wrong MBR parameter page
        (1024 + 12, 0x10001100),         // unsupported UICR record
        (12, 0x100000),                  // payload exceeds chip capacity
        (12, 0x1000),                    // application/SoftDevice region
    ] {
        let mut malformed = baseline.clone();
        set_word(&mut malformed, offset, value);
        assert!(
            validate_for_volume(&volume, &malformed, purpose).is_err(),
            "offset {offset}, value {value:#x}"
        );
    }
    let mut comment_code = baseline.clone();
    set_word(&mut comment_code, 8, 0x2001);
    assert!(validate_for_volume(&volume, &comment_code, purpose).is_err());
}

#[test]
fn updater_cannot_override_explicit_board_chip_or_replaced_marker() {
    let directory = tempfile::tempdir().unwrap();
    let marker = directory.path().join("INFO_UF2.TXT");
    fs::write(&marker, "Board-ID: nRF52833-test\n").unwrap();
    let volume = inspect_volume(directory.path(), "INFO_UF2.TXT").unwrap();
    let bytes = updater(Nrf52Chip::Nrf52840);
    assert!(
        validate_for_volume(
            &volume,
            &bytes,
            WritePurpose::Bootloader(Nrf52Chip::Nrf52840)
        )
        .is_err()
    );
    let bytes = updater(Nrf52Chip::Nrf52833);
    let purpose = WritePurpose::Bootloader(Nrf52Chip::Nrf52833);
    validate_for_volume(&volume, &bytes, purpose).unwrap();
    fs::write(&marker, "Board-ID: replacement\n").unwrap();
    let error = write(
        &volume,
        "update.uf2",
        &bytes,
        purpose,
        &AtomicBool::new(false),
        &mut |_| {},
    )
    .unwrap_err();
    assert!(error.contains("identity changed"), "{error}");
    assert!(!directory.path().join("update.uf2").exists());
}

#[test]
#[ignore = "requires downloaded upstream assets in SLIMENRF_UPDATER_FIXTURES; never writes UF2"]
fn upstream_nosd_updater_payloads() {
    use sha2::{Digest, Sha256};
    let fixtures = std::path::PathBuf::from(
        std::env::var_os("SLIMENRF_UPDATER_FIXTURES")
            .expect("set SLIMENRF_UPDATER_FIXTURES to the downloaded release asset directory"),
    );
    // GitHub releases 0.11.0-slimenrf and 0.11.1-slimenrf, hashes published by their API.
    // Download separately; this test never accesses the network or a device.
    for (name, digest, chip, other) in [
        (
            "update-mochaelec_mochi_bootloader-0.11.1-slimenrf_nosd.uf2",
            "d6ad6cfac482c1dc2ab8e270f6bb4143c75e43eb6b75e857e17230f6b641f1e9",
            Nrf52Chip::Nrf52833,
            Nrf52Chip::Nrf52840,
        ),
        (
            "update-slimenrf_promicro_bootloader-0.11.1-slimenrf_nosd.uf2",
            "f25b3d0b9dfdc6a52c5e07c94be28e6571558f11a88fcdfe6bc094f1ab3f2d27",
            Nrf52Chip::Nrf52840,
            Nrf52Chip::Nrf52833,
        ),
        (
            "update-slimenrf_promicro_bootloader-0.11.0-slimenrf_nosd.uf2",
            "9f9db70f562b9c23314704cd79d1ccc9697fa94b86264273a1c8f5761115d652",
            Nrf52Chip::Nrf52840,
            Nrf52Chip::Nrf52833,
        ),
    ] {
        let bytes = fs::read(fixtures.join(name)).unwrap();
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let actual = Sha256::digest(&bytes);
        let mut text = String::with_capacity(actual.len() * 2);
        for byte in actual {
            text.push(HEX[(byte >> 4) as usize] as char);
            text.push(HEX[(byte & 0x0f) as usize] as char);
        }
        assert_eq!(text, digest, "{name}");
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join("INFO_UF2.TXT"),
            "Board-ID: offline-parser-fixture\n",
        )
        .unwrap();
        let volume = inspect_volume(directory.path(), "INFO_UF2.TXT").unwrap();
        validate_for_volume(&volume, &bytes, WritePurpose::Bootloader(chip)).unwrap();
        assert!(validate_for_volume(&volume, &bytes, WritePurpose::Bootloader(other)).is_err());
        assert!(validate_for_volume(&volume, &bytes, WritePurpose::Application).is_err());
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }
}
