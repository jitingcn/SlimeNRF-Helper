use super::{Nrf52Chip, Uf2ContentError, Uf2ImagePurpose, inspect_uf2, validate_uf2_content};

fn set_word(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn block(address: u32, number: u32, count: u32, family: u32) -> Vec<u8> {
    let mut bytes = vec![0; 512];
    for (offset, value) in [
        (0, 0x0a324655),
        (4, 0x9e5d5157),
        (8, 0x2000),
        (12, address),
        (16, 256),
        (20, number),
        (24, count),
        (28, family),
        (508, 0x0ab16f30),
    ] {
        set_word(&mut bytes, offset, value);
    }
    bytes
}

fn updater(chip: Nrf52Chip) -> Vec<u8> {
    let (flash, ram, family) = match chip {
        Nrf52Chip::Nrf52833 => (0x80000, 0x20000, 0x621e937a),
        Nrf52Chip::Nrf52840 => (0x100000, 0x40000, 0xada52840),
    };
    let mut bytes = block(flash - 0xc000, 0, 3, 0xd663823c);
    bytes.extend(block(flash - 0x2800, 1, 3, 0xd663823c));
    bytes.extend(block(0x10001000, 2, 3, 0xd663823c));
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
fn bootloader_content_is_chip_specific_and_reports_both_families() {
    for (chip, other, family) in [
        (Nrf52Chip::Nrf52833, Nrf52Chip::Nrf52840, 0x621e937a),
        (Nrf52Chip::Nrf52840, Nrf52Chip::Nrf52833, 0xada52840),
    ] {
        let bytes = updater(chip);
        let purpose = Uf2ImagePurpose::Bootloader(chip);
        let facts = validate_uf2_content(&bytes, purpose).unwrap();
        assert_eq!(facts.actual_family_id, 0xd663823c);
        assert_eq!(facts.expected_application_family_id, family);
        assert!(validate_uf2_content(&bytes, Uf2ImagePurpose::Application).is_err());
        assert!(validate_uf2_content(&bytes, Uf2ImagePurpose::Bootloader(other)).is_err());
        let mut application = bytes.clone();
        for block in application.as_chunks_mut::<512>().0 {
            set_word(block, 28, family);
        }
        assert!(validate_uf2_content(&application, purpose).is_err());
    }
}

#[test]
fn updater_rejects_conflicting_missing_or_unflashed_chip_metadata() {
    let purpose = Uf2ImagePurpose::Bootloader(Nrf52Chip::Nrf52840);
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
            validate_uf2_content(&malformed, purpose).is_err(),
            "offset {offset}, value {value:#x}"
        );
    }
    let mut comment_code = baseline;
    set_word(&mut comment_code, 8, 0x2001);
    assert!(validate_uf2_content(&comment_code, purpose).is_err());
}

#[test]
fn application_families_capacity_and_structural_only_inspection_are_distinct() {
    for (family, limit) in [
        (0xada52840, 1024 * 1024),
        (0x621e937a, 512 * 1024),
        (0x1b57745f, 512 * 1024),
    ] {
        let valid = block(limit - 256, 0, 1, family);
        let facts = validate_uf2_content(&valid, Uf2ImagePurpose::Application).unwrap();
        assert_eq!(facts.actual_family_id, family);
        assert_eq!(facts.expected_application_family_id, family);
        let excessive = block(limit, 0, 1, family);
        assert!(matches!(
            validate_uf2_content(&excessive, Uf2ImagePurpose::Application),
            Err(Uf2ContentError::FlashCapacityExceeded { address, family_id })
                if address == limit && family_id == family
        ));
        let uicr = block(0x10001000, 0, 1, family);
        assert!(validate_uf2_content(&uicr, Uf2ImagePurpose::Application).is_ok());
    }
    let current = block(0x1000, 0, 1, 0x12345678);
    assert_eq!(inspect_uf2(&current).unwrap().family_id, Some(0x12345678));
    assert!(validate_uf2_content(&current, Uf2ImagePurpose::Application).is_err());
    let mut untagged = block(0x1000, 0, 1, 0xada52840);
    set_word(&mut untagged, 8, 0);
    assert_eq!(inspect_uf2(&untagged).unwrap().family_id, None);
    assert!(validate_uf2_content(&untagged, Uf2ImagePurpose::Application).is_err());
}
