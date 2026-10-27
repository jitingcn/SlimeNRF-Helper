use super::support::{local_image, target};
use crate::esb_ota::RECEIVER_ID;
use crate::ui::flash::{
    ImageCategory, Mode, firmware_filter_matches, firmware_page_matches, ota_assignment_key,
    uf2_preset_matches, validate_ota_image,
};

#[test]
fn firmware_filters_combine_board_role_variant_and_unknown_fallback() {
    use crate::board_catalog::{EsbOtaKind, FirmwareHint};
    let hints = [FirmwareHint {
        board_target: "board-one".into(),
        kind: Some(EsbOtaKind::Tracker),
        variant: "Standard".into(),
        mcuboot: true,
        bootloader: false,
    }];
    let targets = [("board-one".into(), Some(EsbOtaKind::Tracker))];
    assert!(firmware_filter_matches(
        &hints,
        "Standard",
        &targets,
        true,
        Some("Standard")
    ));
    assert!(!firmware_filter_matches(
        &hints,
        "Standard",
        &targets,
        true,
        Some("DataCollect CDC")
    ));
    let other = [("board-two".into(), Some(EsbOtaKind::Tracker))];
    assert!(!firmware_filter_matches(
        &hints, "Standard", &other, true, None
    ));
    let receiver = [(targets[0].0.clone(), Some(EsbOtaKind::Receiver))];
    assert!(!firmware_filter_matches(
        &hints, "Standard", &receiver, true, None
    ));
    assert!(firmware_filter_matches(&hints, "Standard", &[], true, None));
    assert!(!firmware_filter_matches(
        &[],
        "Standard",
        &targets,
        true,
        None
    ));
    assert!(firmware_filter_matches(
        &hints, "Standard", &other, false, None
    ));
    assert!(firmware_filter_matches(&[], "Standard", &[], true, None));
    let mut unknown_role = hints[0].clone();
    unknown_role.kind = None;
    assert!(!firmware_filter_matches(
        &[unknown_role],
        "Standard",
        &targets,
        true,
        None
    ));
    assert!(firmware_filter_matches(
        &[],
        "Standard",
        &targets,
        false,
        None
    ));
}

#[test]
fn legacy_firmware_field_exposes_bootloader_upgrade_only_in_bootloader_category() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("slimenrf-helper.yaml");
    std::fs::write(
        &config,
        br#"{"schema_version":1,"firmware_root":".","legacy_targets":{
            "SLIMENRFTRK":{
                "bootloader":"./firmwares/update-slimenrf_promicro_bootloader-0.11.0-35-g5ba26d0_nosd.uf2",
                "targets":[{"name":"promicro_uf2/nrf52840/spi","esb_ota_kind":"tracker","firmware":"tracker.uf2"}]
            },
            "NICENANO":{
                "firmware":"./firmwares/update-slimenrf_promicro_bootloader-0.11.0-35-g5ba26d0_nosd.uf2",
                "success":"PROMICRO BOOTLOADER SUCCESS",
                "color":"magenta"
            }
        }}"#,
    )
    .unwrap();
    let catalog = crate::board_catalog::Catalog::load(&config).unwrap();
    let preset = catalog.preset_for_volume("NICENANO", None).unwrap();
    assert!(preset.bootloader.is_none());
    assert_eq!(preset.targets.len(), 1);
    let candidates = |category| {
        preset
            .firmware_candidates()
            .filter(|(_, path, _)| {
                let name = path.filename().unwrap();
                let hints = catalog.match_filename(name);
                firmware_page_matches(Mode::Uf2, category, name, &hints, std::iter::empty(), false)
                    && uf2_preset_matches(preset, &hints)
            })
            .count()
    };
    assert_eq!(candidates(ImageCategory::Bootloader), 1);
    assert_eq!(candidates(ImageCategory::Application), 0);
    let (_, path, _) = preset.firmware_candidates().next().unwrap();
    let name = path.filename().unwrap();
    let hints = catalog.match_filename(name);
    assert!(
        hints
            .iter()
            .all(|hint| hint.bootloader && hint.kind.is_none())
    );
    assert!(!firmware_page_matches(
        Mode::Esb,
        ImageCategory::Application,
        name,
        &hints,
        std::iter::empty(),
        false,
    ));
}

#[test]
fn uf2_bootloader_category_keeps_board_scope_and_is_forbidden_in_ota() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("slimenrf-helper.yaml");
    std::fs::write(
        &config,
        br#"{"schema_version":1,"firmware_root":".","legacy_targets":{"NICENANO":{"bootloader":"maintenance.uf2","targets":[]}}}"#,
    )
    .unwrap();
    let catalog = crate::board_catalog::Catalog::load(&config).unwrap();
    let preset = catalog.preset_for_volume("NICENANO", None).unwrap();
    let hints = catalog.match_filename("maintenance.uf2");
    assert!(uf2_preset_matches(preset, &hints));
    let mut wrong = hints.clone();
    wrong[0].board_target = "other-board".into();
    assert!(!uf2_preset_matches(preset, &wrong));
    assert!(firmware_page_matches(
        Mode::Uf2,
        ImageCategory::Bootloader,
        "maintenance.uf2",
        &hints,
        std::iter::empty(),
        false
    ));
    assert!(!firmware_page_matches(
        Mode::Uf2,
        ImageCategory::Application,
        "maintenance.uf2",
        &hints,
        std::iter::empty(),
        false
    ));
    assert!(!firmware_page_matches(
        Mode::Uf2,
        ImageCategory::Bootloader,
        "application.uf2",
        &[],
        std::iter::empty(),
        false
    ));
    for mode in [Mode::Esb, Mode::Ble, Mode::OpenDfu] {
        assert!(!firmware_page_matches(
            mode,
            ImageCategory::Bootloader,
            "maintenance.uf2",
            &hints,
            std::iter::empty(),
            false
        ));
    }
    let device = target("a", 0, "board");
    assert!(validate_ota_image(&local_image("board.hex"), &device).is_ok());
    assert!(validate_ota_image(&local_image("update-board.hex"), &device).is_err());
}

#[test]
fn bootloader_versions_keep_exact_board_and_nosd_profile_filters() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("slimenrf-helper.yaml");
    std::fs::write(
        &config,
        br#"{"schema_version":1,"firmware_root":".","legacy_targets":{"BOARD":{
            "bootloader_board":"exact_board","bootloader_mcu":"nrf52840","targets":[
                {"name":"exact_board/nrf52840/spi","esb_ota_kind":"tracker"}
            ]
        }}}"#,
    )
    .unwrap();
    let catalog = crate::board_catalog::Catalog::load(&config).unwrap();
    let preset = catalog.preset_for_volume("BOARD", None).unwrap();
    let target_filter = [("exact_board/nrf52840/spi".into(), None)];
    for version in ["0.11.0-35-g5ba26d0", "0.12.0-slimenrf", "g5ba26d0-dirty"] {
        let name = format!("update-exact_board_bootloader-{version}_nosd.uf2");
        let hints = catalog.match_filename(&name);
        assert!(uf2_preset_matches(preset, &hints), "{name}");
        assert!(firmware_filter_matches(
            &hints,
            "Bootloader",
            &target_filter,
            true,
            None
        ));
        assert!(firmware_page_matches(
            Mode::Uf2,
            ImageCategory::Bootloader,
            &name,
            &hints,
            std::iter::empty(),
            false
        ));
        assert!(!firmware_page_matches(
            Mode::Esb,
            ImageCategory::Application,
            &name,
            &hints,
            std::iter::empty(),
            false
        ));
    }
    for name in [
        "update-exact_board_pro_bootloader-0.12.0_nosd.uf2",
        "update-exact_board_bootloader-0.12.0_s140_7.3.0.uf2",
        "update-exact_board_bootloader-0.12.0-nrf52833_nosd.uf2",
        "update-exact_board_bootloader-0.12.0_nosd.hex",
        "update-exact_board_bootloader-0.12.0_nosd.zip",
    ] {
        let hints = catalog.match_filename(name);
        assert!(!uf2_preset_matches(preset, &hints), "{name}");
        assert!(!firmware_filter_matches(
            &hints,
            "Bootloader",
            &target_filter,
            true,
            None
        ));
    }
}

#[test]
fn application_format_filter_uses_matching_ota_group_not_global_bootloader() {
    use crate::board_catalog::{EsbOtaKind, FirmwareHint};
    let ordinary = target("a", 0, "plain");
    let mut mcuboot = target("a", 1, "signed");
    mcuboot.info.bootloader = "mcuboot".into();
    let hints = [FirmwareHint {
        board_target: "signed".into(),
        kind: Some(EsbOtaKind::Tracker),
        variant: "Standard".into(),
        mcuboot: true,
        bootloader: false,
    }];
    for mode in [Mode::Uf2, Mode::OpenDfu, Mode::Ble, Mode::Esb] {
        assert!(!firmware_page_matches(
            mode,
            ImageCategory::Application,
            "signed_update.bin",
            &hints,
            std::iter::empty(),
            false
        ));
    }
    assert!(!firmware_page_matches(
        Mode::Esb,
        ImageCategory::Application,
        "signed_update.bin",
        &hints,
        std::iter::once(&ordinary),
        false
    ));
    assert!(firmware_page_matches(
        Mode::Esb,
        ImageCategory::Application,
        "signed_update.bin",
        &hints,
        [&ordinary, &mcuboot].into_iter(),
        false
    ));
    let mut wrong = hints.clone();
    wrong[0].board_target = "plain".into();
    assert!(!firmware_page_matches(
        Mode::Esb,
        ImageCategory::Application,
        "plain_update.bin",
        &wrong,
        [&ordinary, &mcuboot].into_iter(),
        false
    ));
    assert!(!firmware_page_matches(
        Mode::Uf2,
        ImageCategory::Application,
        "signed_update.bin.zip",
        &hints,
        std::iter::once(&mcuboot),
        true
    ));
    assert!(firmware_page_matches(
        Mode::Esb,
        ImageCategory::Application,
        "signed_update.bin.zip",
        &hints,
        std::iter::once(&mcuboot),
        true
    ));
    let mut same_board = ordinary.clone();
    same_board.info.bootloader = "mcuboot".into();
    assert_ne!(
        ota_assignment_key(&ordinary),
        ota_assignment_key(&same_board)
    );
    assert!(!firmware_page_matches(
        Mode::Uf2,
        ImageCategory::Application,
        "board.hex",
        &[],
        std::iter::empty(),
        false
    ));
    assert!(firmware_page_matches(
        Mode::Uf2,
        ImageCategory::Application,
        "board.uf2",
        &[],
        std::iter::empty(),
        false
    ));
}

#[test]
fn only_receiver_cdc_is_an_available_datacollect_application() {
    use crate::board_catalog::{EsbOtaKind, FirmwareHint};
    let receiver = target("a", RECEIVER_ID, "board");
    let tracker = target("a", 0, "board");
    let hints = [FirmwareHint {
        board_target: "board".into(),
        kind: Some(EsbOtaKind::Receiver),
        variant: "DataCollect CDC".into(),
        mcuboot: false,
        bootloader: false,
    }];
    assert!(firmware_page_matches(
        Mode::Esb,
        ImageCategory::Application,
        "board_datacollect_cdc.uf2",
        &hints,
        std::iter::once(&receiver),
        false
    ));
    assert!(!firmware_page_matches(
        Mode::Esb,
        ImageCategory::Application,
        "board_datacollect_cdc.uf2",
        &hints,
        std::iter::once(&tracker),
        false
    ));
    for name in ["board_datacollect_hid.uf2", "board_datacollect.uf2"] {
        assert!(!firmware_page_matches(
            Mode::Uf2,
            ImageCategory::Application,
            name,
            &[],
            std::iter::empty(),
            false
        ));
        assert!(!firmware_page_matches(
            Mode::Esb,
            ImageCategory::Application,
            name,
            &[],
            std::iter::once(&receiver),
            false
        ));
    }
    assert!(!firmware_page_matches(
        Mode::Uf2,
        ImageCategory::Bootloader,
        "update-board_datacollect_cdc.uf2",
        &[],
        std::iter::empty(),
        false
    ));
}

#[test]
fn bootloader_name_does_not_confuse_application_image_format() {
    assert!(firmware_page_matches(
        Mode::Uf2,
        ImageCategory::Bootloader,
        "update-mcuboot.uf2",
        &[],
        std::iter::empty(),
        false,
    ));
    let mut device = target("a", 0, "board");
    device.info.bootloader = "mcuboot".into();
    assert!(!firmware_page_matches(
        Mode::Esb,
        ImageCategory::Application,
        "board_mcuboot.uf2",
        &[],
        std::iter::once(&device),
        false,
    ));
}
