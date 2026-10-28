use super::*;

#[test]
fn explicit_updater_board_works_without_any_local_resource() {
    let directory = tempfile::tempdir().unwrap();
    let raw = br#"{"schema_version":1,"firmware_root":"absent","board_targets":{
        "physical-board":{"bootloader_board":"slimenrf_board_nrf52833","bootloader_mcu":"nrf52833","targets":[]}
    }}"#;
    let catalog = Catalog::from_bytes(raw, directory.path()).unwrap();
    let preset = &catalog.board_targets["physical-board"];
    assert!(preset.targets.is_empty());
    assert_eq!(preset.firmware_candidates().count(), 0);
    for version in [
        "0.11.1",
        "0.12.0-slimenrf-2-gabc123-dirty",
        "abc123",
        "release-tag",
        "0.11",
    ] {
        let filename = format!("update-slimenrf_board_nrf52833_bootloader-{version}_nosd.uf2");
        let hints = catalog.match_filename(&filename);
        assert_eq!(hints.len(), 1, "{filename}");
        assert_eq!(hints[0].board_target, "physical-board");
        assert!(hints[0].bootloader && hints[0].kind.is_none());
    }
    for filename in [
        "update-slimenrf_board_nrf52840_bootloader-0.11.1_nosd.uf2",
        "update-slimenrf_board_nrf52833_sense_bootloader-0.11.1_nosd.uf2",
        "update-slimenrf_board_nrf52833plus_bootloader-0.11.1_nosd.uf2",
        "update-slimenrf_board_nrf52833_bootloader-0.11.1_s140_7.3.0.uf2",
        "update-slimenrf_board_nrf52833_bootloader-0.11.1_s140_7.3.0_nosd.uf2",
        "update-slimenrf_board_nrf52833_bootloader-0.11.1-s140_nosd.uf2",
        "update-slimenrf_board_nrf52833_bootloader-0.11.1_nosd.hex",
        "update-slimenrf_board_nrf52833_bootloader-0.11.1_nosd.zip",
        "update-slimenrf_board_nrf52833_bootloader-_nosd.uf2",
    ] {
        assert!(catalog.match_filename(filename).is_empty(), "{filename}");
    }
}

#[test]
fn updater_tokens_reject_paths_patterns_and_injection() {
    for token in [
        "",
        "../board",
        "board/name",
        "board\\name",
        "board:*",
        "board?",
        "board\n",
        " board",
        "board[12]",
    ] {
        let raw = format!(
            "schema_version: 1\nfirmware_root: .\nboard_targets:\n  board:\n    bootloader_mcu: nrf52840\n    bootloader_board: {}\n",
            serde_json::to_string(token).unwrap()
        );
        assert!(
            Catalog::from_bytes(raw.as_bytes(), Path::new(".")).is_err(),
            "{token:?}"
        );
    }
}

#[test]
fn raw_hex_and_nonstandard_pins_do_not_gain_updater_profile_compatibility() {
    let raw = br#"{"schema_version":1,"firmware_root":"missing","board_targets":{
        "board":{"bootloader":"update-board_bootloader-0.11.1_s140_7.3.0.hex","targets":[]},
        "custom":{"bootloader":"maintenance.uf2","targets":[]}
    }}"#;
    let catalog = Catalog::from_bytes(raw, Path::new(".")).unwrap();
    assert_eq!(catalog.match_filename("maintenance.uf2").len(), 1);
    assert_eq!(
        catalog
            .match_filename("update-board_bootloader-0.11.1_s140_7.3.0.hex")
            .len(),
        1
    );
    assert!(
        catalog
            .match_filename("update-board_bootloader-0.12.0_s140_7.3.0.hex")
            .is_empty()
    );
    assert!(
        catalog
            .match_filename("update-board_bootloader-0.11.1_nosd.uf2")
            .is_empty()
    );
    assert!(
        Catalog::default()
            .match_filename("update-SlimeNRF_Holyiot_Receiver_bootloader-main_nosd.uf2")
            .is_empty()
    );
}

#[test]
fn explicit_application_aliases_keep_profile_role_category_and_container() {
    let raw = br#"{"schema_version":1,"firmware_root":"missing","application_targets":{
        "application":{"targets":[
            {"name":"custom/nrf52833/spi","esb_ota_kind":"tracker","firmware_aliases":["Custom_nrf52833_SPI_Tracker.uf2"]},
            {"name":"custom/nrf52833/cdc","esb_ota_kind":"receiver","firmware_aliases":["Custom_nrf52833_SPI_Receiver_DataCollect_CDC-0.1.0.hex"]}
        ]}
    }}"#;
    let catalog = Catalog::from_bytes(raw, Path::new(".")).unwrap();
    for filename in [
        "Custom_nrf52833_SPI_Tracker-1.2.3.uf2",
        "Custom_nrf52833_SPI_Tracker_v2.0.1-rc.1.uf2",
    ] {
        let hints = catalog.match_filename(filename);
        assert_eq!(hints.len(), 1, "{filename}");
        assert_eq!(hints[0].board_target, "custom/nrf52833/spi");
        assert_eq!(hints[0].variant, "Standard");
    }
    let hints = catalog.match_filename("Custom_nrf52833_SPI_Receiver_DataCollect_CDC-1.2.3.hex");
    assert_eq!(hints.len(), 1);
    assert_eq!(hints[0].board_target, "custom/nrf52833/cdc");
    assert_eq!(hints[0].variant, "DataCollect CDC");
    for filename in [
        "Custom_nrf52840_SPI_Tracker-1.2.3.uf2",
        "Custom_nrf52833_I2C_Tracker-1.2.3.uf2",
        "Custom_nrf52833_SPI_Tracker_Sense-1.2.3.uf2",
        "Custom_nrf52833_SPI_Tracker_DataCollect_CDC-1.2.3.uf2",
        "Custom_nrf52833_SPI_Tracker-1.2.3.hex",
        "Custom_nrf52833_SPI_Tracker_update-1.2.3.bin",
        "Custom_nrf52833_SPI_Receiver_DataCollect_HID-1.2.3.hex",
        "Custom_nrf52833_SPI_Receiver-1.2.3.hex",
        "Custom_nrf52833_SPI_Tracker-release.uf2",
    ] {
        assert!(catalog.match_filename(filename).is_empty(), "{filename}");
    }
    assert!(
        catalog
            .preset_for_volume("SLIMENRF", Some("application"))
            .is_none()
    );
}

#[test]
fn inventory_lists_versions_without_replacing_defaults_and_is_frozen() {
    let directory = tempfile::tempdir().unwrap();
    for filename in [
        "Custom-1.0.0.uf2",
        "Custom-2.0.0.uf2",
        "Custom-2.0.0.hex",
        "Custom-2.0.0-Pro.uf2",
        "Custom-2.0.0-P00.uf2",
        "Custom-2.0.0-Lite.uf2",
        "update-board_bootloader-main_nosd.uf2",
        "update-board_bootloader-0.11.1_nosd.uf2",
        "other.uf2",
    ] {
        std::fs::write(directory.path().join(filename), b"fixture").unwrap();
    }
    std::fs::create_dir(directory.path().join("nested")).unwrap();
    std::fs::write(directory.path().join("nested/Custom-3.0.0.uf2"), b"fixture").unwrap();
    let raw = br#"{"schema_version":1,"firmware_root":".","legacy_targets":{
        "VOLUME":{"bootloader_board":"board","bootloader_mcu":"nrf52840","targets":[{"name":"custom/board","esb_ota_kind":"tracker","firmware":"Custom-1.0.0.uf2","firmware_aliases":["Custom.uf2"]}]}
    }}"#;
    let catalog = Catalog::from_bytes(raw, directory.path()).unwrap();
    let preset = &catalog.legacy_targets["VOLUME"];
    assert_eq!(
        preset.targets[0].firmware.as_ref().unwrap().filename(),
        Some("Custom-1.0.0.uf2")
    );
    assert_eq!(preset.targets[0].additional_firmware.len(), 1);
    assert_eq!(preset.additional_bootloaders.len(), 2);
    assert_eq!(
        preset.targets[0]
            .image_for_bootloader("adafruit_uf2")
            .unwrap(),
        preset.targets[0].firmware.as_ref().unwrap()
    );
    let before: Vec<_> = preset
        .firmware_candidates()
        .map(|(index, resource, label)| {
            (
                index,
                resource.filename().unwrap().to_owned(),
                label.to_owned(),
            )
        })
        .collect();
    assert_eq!(
        before
            .iter()
            .map(|(_, name, _)| name.as_str())
            .collect::<Vec<_>>(),
        [
            "Custom-1.0.0.uf2",
            "Custom-2.0.0.uf2",
            "update-board_bootloader-0.11.1_nosd.uf2",
            "update-board_bootloader-main_nosd.uf2"
        ]
    );
    assert_eq!(
        before
            .iter()
            .map(|(index, _, _)| *index)
            .collect::<Vec<_>>(),
        [0, 0, 1, 1]
    );
    std::fs::remove_file(directory.path().join("Custom-2.0.0.uf2")).unwrap();
    std::fs::write(directory.path().join("Custom-3.0.0.uf2"), b"fixture").unwrap();
    let after: Vec<_> = preset
        .firmware_candidates()
        .map(|(index, resource, label)| {
            (
                index,
                resource.filename().unwrap().to_owned(),
                label.to_owned(),
            )
        })
        .collect();
    assert_eq!(before, after);
}

#[test]
fn overlapping_aliases_remain_ambiguous_and_unknown_files_stay_unknown() {
    let raw = br#"{"schema_version":1,"firmware_root":"missing","board_targets":{
        "a":{"name":"a/nrf52840","firmware_aliases":["Custom.uf2"]},
        "b":{"name":"b/nrf52840","firmware_aliases":["Custom.uf2"]}
    }}"#;
    let catalog = Catalog::from_bytes(raw, Path::new(".")).unwrap();
    assert_eq!(catalog.match_filename("Custom-2.0.0.uf2").len(), 2);
    assert!(catalog.match_filename("Unknown-2.0.0.uf2").is_empty());
}

#[test]
fn positive_board_id_never_falls_back_to_a_conflicting_volume() {
    let raw = br#"{"schema_version":1,"firmware_root":"missing","legacy_targets":{
        "SHARED":{"bootloader_board":"legacy","bootloader_mcu":"nrf52840","targets":[]}
    },"board_targets":{"actual":{"bootloader_board":"actual","bootloader_mcu":"nrf52840","targets":[]}}}"#;
    let catalog = Catalog::from_bytes(raw, Path::new(".")).unwrap();
    assert_eq!(
        catalog
            .preset_for_volume("SHARED", Some("actual"))
            .unwrap()
            .key,
        "actual"
    );
    assert!(
        catalog
            .preset_for_volume("SHARED", Some("unknown"))
            .is_none()
    );
    assert_eq!(
        catalog.preset_for_volume("SHARED", None).unwrap().key,
        "SHARED"
    );
}

#[test]
fn alias_only_application_inventory_is_not_an_implicit_ota_default() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("Custom-2.0.0.hex"), b"fixture").unwrap();
    let raw = br#"{"schema_version":1,"firmware_root":".","application_targets":{
        "headless":{"name":"custom/nrf52840","esb_ota_kind":"receiver","firmware_aliases":["Custom.hex"]}
    }}"#;
    let catalog = Catalog::from_bytes(raw, directory.path()).unwrap();
    let target = catalog
        .ota_target(EsbOtaKind::Receiver, "custom/nrf52840")
        .unwrap();
    assert_eq!(target.additional_firmware.len(), 1);
    assert!(target.image_for_bootloader("adafruit_uf2").is_err());
    assert!(
        catalog
            .preset_for_volume("SLIMENRF", Some("headless"))
            .is_none()
    );
}

#[test]
fn updater_board_requires_unambiguous_explicit_mcu() {
    for raw in [
        br#"{"schema_version":1,"firmware_root":"missing","board_targets":{"a":{"bootloader_board":"board"}}}"#.as_slice(),
        br#"{"schema_version":1,"firmware_root":"missing","board_targets":{
            "a":{"bootloader_board":"board","bootloader_mcu":"nrf52833"},
            "b":{"bootloader_board":"BOARD","bootloader_mcu":"nrf52840"}
        }}"#.as_slice(),
    ] {
        assert!(Catalog::from_bytes(raw, Path::new(".")).is_err());
    }
}

#[test]
fn application_versions_never_swallow_chip_or_profile_markers() {
    let raw = br#"{"schema_version":1,"firmware_root":"missing","application_targets":{
        "custom":{"name":"custom","firmware_aliases":["Custom.uf2"]}
    }}"#;
    let catalog = Catalog::from_bytes(raw, Path::new(".")).unwrap();
    for filename in [
        "Custom-1.2.3-Sense.uf2",
        "Custom-1.2.3-nrf52833.uf2",
        "Custom-1.2.3-SPI.uf2",
        "Custom-1.2.3-CDC.uf2",
        "Custom-1.2.3-Pro.uf2",
        "Custom-1.2.3-Lite.uf2",
        "Custom-1.2.3-P00.uf2",
        "Custom-1.2.3-P10.uf2",
        "Custom-1.2.3-R33.uf2",
        "Custom-1.2.3+Pro.uf2",
    ] {
        assert!(catalog.match_filename(filename).is_empty(), "{filename}");
    }
}

#[test]
fn pinned_updater_stays_exact_and_metadata_does_not_alias_another_board() {
    let directory = tempfile::tempdir().unwrap();
    for filename in [
        "update-board_bootloader-0.11.1_nosd.uf2",
        "update-board_bootloader-0.12.0_nosd.uf2",
    ] {
        std::fs::write(directory.path().join(filename), b"fixture").unwrap();
    }
    let raw = br#"{"schema_version":1,"firmware_root":".","board_targets":{
        "board":{"bootloader_board":"board","bootloader_mcu":"nrf52840",
            "bootloader":"update-board_bootloader-0.11.1_nosd.uf2","targets":[]}
    }}"#;
    let catalog = Catalog::from_bytes(raw, directory.path()).unwrap();
    let preset = &catalog.board_targets["board"];
    assert_eq!(
        preset.bootloader.as_ref().unwrap().filename(),
        Some("update-board_bootloader-0.11.1_nosd.uf2")
    );
    assert_eq!(preset.additional_bootloaders.len(), 1);
    assert_eq!(
        preset.additional_bootloaders[0].filename(),
        Some("update-board_bootloader-0.12.0_nosd.uf2")
    );
    let raw = br#"{"schema_version":1,"firmware_root":"missing","board_targets":{
        "board":{"bootloader_board":"board","bootloader_mcu":"nrf52840",
            "bootloader":"update-other_bootloader-0.11.1_nosd.uf2","targets":[]}
    }}"#;
    let catalog = Catalog::from_bytes(raw, directory.path()).unwrap();
    assert_eq!(
        catalog
            .match_filename("update-other_bootloader-0.11.1_nosd.uf2")
            .len(),
        1
    );
    assert!(
        catalog
            .match_filename("update-other_bootloader-0.12.0_nosd.uf2")
            .is_empty()
    );
    assert_eq!(
        catalog
            .match_filename("update-board_bootloader-0.12.0_nosd.uf2")
            .len(),
        1
    );
}

#[test]
fn upstream_fallback_consumes_complete_identity_instead_of_substrings() {
    let raw = br#"{"schema_version":1,"firmware_root":"missing","application_targets":{
        "aero":{"name":"aero_tracker_uf2/nrf52840","firmware_aliases":["Aero_Tracker.uf2","SlimeNRF_Aero_Tracker.uf2"]},
        "pro":{"name":"aero_pro_uf2/nrf52840","firmware_aliases":["Aero_Tracker_Pro.uf2","SlimeNRF_Aero_Tracker_Pro.uf2"]}
    }}"#;
    let configured = Catalog::from_bytes(raw, Path::new(".")).unwrap();
    for filename in [
        "Aero_Tracker-1.2.3.hex",
        "Aero_Tracker_update-1.2.3.bin",
        "Aero_Tracker_DataCollect_CDC-1.2.3.uf2",
    ] {
        assert!(configured.match_filename(filename).is_empty(), "{filename}");
    }
    for catalog in [Catalog::default(), configured] {
        for filename in [
            "Aero_Tracker-1.2.3-Pro.uf2",
            "SlimeNRF_Aero_Tracker-1.2.3-Lite.uf2",
            "Aero_Tracker-1.2.3-P00.uf2",
            "Aero_Tracker-1.2.3-P10.uf2",
            "Aero_Tracker-1.2.3-R33.uf2",
            "Aero_Tracker_nrf52833-1.2.3.uf2",
            "Unknown_Aero_Tracker.uf2",
            "SlimeNRF_Aero_Tracker_Unknown.uf2",
            "Aero_Tracker.bin",
            "Aero_Tracker_update.uf2",
            "Aero_Tracker.zip",
            "SlimeNRF_XIAO_nRF54LM20A_Tracker.uf2",
            "SlimeNRF_XIAO_nRF54L15_Tracker.hex",
            "SlimeNRF_SlimeNRF_XIAO_nRF54LM20A_Tracker.hex",
        ] {
            assert!(catalog.match_filename(filename).is_empty(), "{filename}");
        }
        for (filename, board, category, mcuboot) in [
            (
                "Aero_Tracker-1.2.3.uf2",
                "aero_tracker_uf2/nrf52840",
                "Standard",
                false,
            ),
            (
                "SlimeNRF_Aero_Tracker_Pro-1.2.3.uf2",
                "aero_pro_uf2/nrf52840",
                "Standard",
                false,
            ),
            (
                "SlimeNRF_ProMicro_StackedSmol_Tracker_SPI-1.2.3.uf2",
                "promicro_uf2/nrf52840/spi",
                "Standard",
                false,
            ),
            (
                "SlimeNRF_Holyiot_Receiver-1.2.3_DataCollect_CDC.hex",
                "holyiot_21017/nrf52840",
                "DataCollect CDC",
                false,
            ),
            (
                "SlimeNRF_Holyiot_Receiver_update-1.2.3.bin",
                "holyiot_21017/nrf52840",
                "Standard",
                true,
            ),
            (
                "SlimeNRF_Tracker_Mag_SPI_ProMicro_JitingCat.uf2",
                "promicro_uf2/nrf52840/default_spi",
                "Standard",
                false,
            ),
            (
                "Tracker_DefaultI2C.uf2",
                "promicro_uf2/nrf52840/default_i2c",
                "Standard",
                false,
            ),
            (
                "FoxApplication_FoxSnackLite_Tracker.uf2",
                "foxsnacklite_uf2/nrf52840",
                "Standard",
                false,
            ),
            (
                "SlimeNRF_XIAO_nRF54LM20A_Sense_Tracker.hex",
                "xiao_nrf54lm20a/nrf54lm20a/cpuapp/sense",
                "Standard",
                false,
            ),
        ] {
            let hints = catalog.match_filename(filename);
            assert_eq!(hints.len(), 1, "{filename}: {hints:?}");
            assert_eq!(hints[0].board_target, board, "{filename}");
            assert_eq!(hints[0].variant, category, "{filename}");
            assert_eq!(hints[0].mcuboot, mcuboot, "{filename}");
        }
    }
}

#[test]
fn anchored_legacy_formats_preserve_mcuboot_and_mixed_category_separators() {
    let catalog = Catalog::default();
    for filename in [
        "SlimeNRF_Holyiot_Receiver_update.bin",
        "SlimeNRF_Holyiot_Receiver_mcuboot.bin",
        "SlimeNRF_Holyiot_Receiver-1.2.3_mcuboot.bin",
        "SlimeNRF_Holyiot_Receiver_mcuboot-1.2.3.bin",
    ] {
        let hints = catalog.match_filename(filename);
        assert_eq!(hints.len(), 1, "{filename}");
        assert_eq!(hints[0].board_target, "holyiot_21017/nrf52840");
        assert_eq!(hints[0].variant, "Standard");
        assert!(hints[0].mcuboot && !hints[0].bootloader);
    }
    for (filename, category) in [
        ("SlimeNRF_Holyiot_Receiver_DataCollect.hex", "DataCollect"),
        (
            "SlimeNRF_Holyiot_Receiver_DataCollect_CDC.hex",
            "DataCollect CDC",
        ),
        (
            "SlimeNRF_Holyiot_Receiver_DataCollect-HID.hex",
            "DataCollect HID",
        ),
        (
            "SlimeNRF_Holyiot_Receiver-DataCollect_CDC.hex",
            "DataCollect CDC",
        ),
        (
            "SlimeNRF_Holyiot_Receiver-1.2.3_DataCollect-HID.hex",
            "DataCollect HID",
        ),
    ] {
        let hints = catalog.match_filename(filename);
        assert_eq!(hints.len(), 1, "{filename}");
        assert_eq!(hints[0].variant, category, "{filename}");
        assert!(!hints[0].mcuboot && !hints[0].bootloader);
    }
    for filename in [
        "SlimeNRF_Holyiot_Receiver.bin",
        "SlimeNRF_Holyiot_Receiver_unknown.bin",
        "SlimeNRF_Holyiot_Receiver_mcuboot_unknown.bin",
        "SlimeNRF_Holyiot_Receiver_mcuboot.uf2",
        "SlimeNRF_Holyiot_Receiver_mcuboot-1.2.3-Pro.bin",
    ] {
        assert!(catalog.match_filename(filename).is_empty(), "{filename}");
    }
    let raw = br#"{"schema_version":1,"firmware_root":"missing","application_targets":{
        "receiver":{"name":"holyiot_21017/nrf52840","esb_ota_kind":"receiver",
            "firmware_aliases":["SlimeNRF_Holyiot_Receiver_update.bin"]}
    }}"#;
    let catalog = Catalog::from_bytes(raw, Path::new(".")).unwrap();
    assert_eq!(
        catalog
            .match_filename("SlimeNRF_Holyiot_Receiver_update-1.2.3.bin")
            .len(),
        1
    );
    assert!(
        catalog
            .match_filename("SlimeNRF_Holyiot_Receiver_mcuboot-1.2.3.bin")
            .is_empty()
    );
}

#[test]
fn current_fox_identities_resolve_exact_standard_defaults_and_roles() {
    let catalog = Catalog::load_embedded().unwrap();
    for (board_id, board, kind, bootloader, filename) in [
        (
            "FoxSnack-v3",
            "foxsnacklitev3_uf2/nrf52840",
            EsbOtaKind::Tracker,
            "foxsnackv3_nrf52840",
            "FoxApplication_FoxSnackLiteV3_Tracker.uf2",
        ),
        (
            "FoxDongleNEO-v1",
            "foxdongleneo_uf2/nrf52840",
            EsbOtaKind::Receiver,
            "foxdongleneo_nrf52840",
            "SlimeNRF_FoxDongleNEO_Receiver.uf2",
        ),
    ] {
        let preset = catalog
            .preset_for_volume("FOX-BOOT", Some(board_id))
            .unwrap();
        assert_eq!(preset.key, board_id);
        assert_eq!(preset.bootloader_board.as_deref(), Some(bootloader));
        assert_eq!(
            preset.bootloader_mcu,
            Some(slimenrf_firmware_image::image::Nrf52Chip::Nrf52840)
        );
        assert!(preset.bootloader.is_none(), "{board_id}");
        assert_eq!(preset.targets.len(), 1, "{board_id}");
        let target = catalog.ota_target(kind, board).unwrap();
        assert_eq!(target.name.as_deref(), Some(board));
        assert_eq!(target.esb_ota_kind, Some(kind));
        assert_eq!(
            target
                .image_for_bootloader("adafruit_uf2")
                .unwrap()
                .filename(),
            Some(filename)
        );
        let wrong_role = match kind {
            EsbOtaKind::Tracker => EsbOtaKind::Receiver,
            EsbOtaKind::Receiver => EsbOtaKind::Tracker,
        };
        assert!(catalog.ota_target(wrong_role, board).is_err(), "{board}");
        let hints = catalog.match_filename(filename);
        assert_eq!(hints.len(), 1, "{filename}");
        assert_eq!(hints[0].board_target, board);
        assert_eq!(hints[0].kind, Some(kind));
        assert_eq!(hints[0].variant, "Standard");
        assert!(!hints[0].bootloader && !hints[0].mcuboot);
    }
    let cdc = catalog.match_filename("SlimeNRF_FoxDongleNEO_Receiver_DataCollect_CDC.uf2");
    assert_eq!(cdc.len(), 1);
    assert_eq!(cdc[0].board_target, "foxdongleneo_uf2/nrf52840");
    assert_eq!(cdc[0].kind, Some(EsbOtaKind::Receiver));
    assert_eq!(cdc[0].variant, "DataCollect CDC");
    for board_id in [
        None,
        Some("unknown"),
        Some("FoxSnackLite-v3"),
        Some("FoxDongleNEO"),
    ] {
        assert!(catalog.preset_for_volume("FOX-BOOT", board_id).is_none());
    }
}

#[test]
fn current_fox_updaters_match_exact_boards_independently_of_version() {
    let catalog = Catalog::load_embedded().unwrap();
    for (board, token) in [
        ("foxsnacklitev3_uf2/nrf52840", "foxsnackv3_nrf52840"),
        ("foxdongleneo_uf2/nrf52840", "foxdongleneo_nrf52840"),
    ] {
        // Synthetic future names exercise matching, not released-updater availability.
        for version in ["0.12.0", "0.12.1-slimenrf-2-gabc123-dirty", "main"] {
            let filename = format!("update-{token}_bootloader-{version}_nosd.uf2");
            let hints = catalog.match_filename(&filename);
            assert_eq!(hints.len(), 1, "{filename}");
            assert_eq!(hints[0].board_target, board);
            assert!(hints[0].bootloader && !hints[0].mcuboot);
            assert!(hints[0].kind.is_none());
        }
        for filename in [
            format!("update-{token}_sense_bootloader-0.12.0_nosd.uf2"),
            format!("update-{token}plus_bootloader-0.12.0_nosd.uf2"),
            format!("update-{token}_bootloader-0.12.0_s140_7.3.0.uf2"),
            format!("update-{token}_bootloader-0.12.0_s140_7.3.0_nosd.uf2"),
            format!("update-{token}_bootloader-0.12.0_nosd.hex"),
            format!("update-{token}_bootloader-_nosd.uf2"),
        ] {
            assert!(catalog.match_filename(&filename).is_empty(), "{filename}");
        }
    }
    let older = catalog.match_filename("update-foxsnacklitev2_nrf52840_bootloader-0.12.0_nosd.uf2");
    assert_eq!(older.len(), 1);
    assert_eq!(older[0].board_target, "foxsnacklite_uf2/nrf52840");
    for filename in [
        "update-foxsnackv3_nrf52833_bootloader-0.12.0_nosd.uf2",
        "update-foxdongleneo_nrf52833_bootloader-0.12.0_nosd.uf2",
        "update-foxsnacklitev3_nrf52840_bootloader-0.12.0_nosd.uf2",
    ] {
        assert!(catalog.match_filename(filename).is_empty(), "{filename}");
    }
}

#[test]
fn current_fox_application_aliases_do_not_cross_old_hardware_or_roles() {
    let catalog = Catalog::load_embedded().unwrap();
    for (filename, expected_board, kind) in [
        (
            "FoxApplication_FoxSnackLite_Tracker.uf2",
            "foxsnacklite_uf2/nrf52840",
            EsbOtaKind::Tracker,
        ),
        (
            "SlimeNRF_FoxDongle33_Receiver.uf2",
            "foxdongle33_uf2/nrf52833",
            EsbOtaKind::Receiver,
        ),
        (
            "SlimeNRF_FoxDongle_Receiver.uf2",
            "foxdongle_uf2/nrf52840",
            EsbOtaKind::Receiver,
        ),
    ] {
        let hints = catalog.match_filename(filename);
        assert_eq!(hints.len(), 1, "{filename}");
        assert_eq!(hints[0].board_target, expected_board);
        assert_eq!(hints[0].kind, Some(kind));
        assert_ne!(hints[0].board_target, "foxsnacklitev3_uf2/nrf52840");
        assert_ne!(hints[0].board_target, "foxdongleneo_uf2/nrf52840");
    }
    for filename in [
        "FoxApplication_FoxSnackLiteV3_Receiver.uf2",
        "SlimeNRF_FoxDongleNEO_Tracker.uf2",
    ] {
        assert!(catalog.match_filename(filename).is_empty(), "{filename}");
    }
}
