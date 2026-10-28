use super::{Catalog, EsbOtaKind, filename_variant};
use crate::bundled_resources::Resource;
use std::path::Path;
use std::sync::atomic::AtomicBool;

fn embedded_config() -> Vec<u8> {
    Resource::bundled("slimenrf-helper.yaml")
        .unwrap()
        .read(1024 * 1024, &AtomicBool::new(false))
        .unwrap()
        .bytes
}
#[test]
fn matches_cdc_without_inventing_targets_or_roles() {
    let catalog = Catalog::from_bytes(&embedded_config(), Path::new("base")).unwrap();
    let hints = catalog.match_filename("SlimeNRF_Holyiot_Dongle_Receiver_DataCollect_CDC.hex");
    assert_eq!(hints.len(), 1);
    assert_eq!(hints[0].board_target, "holyiot_21017/nrf52840");
    assert_eq!(hints[0].variant, "DataCollect CDC");
    assert!(catalog.match_filename("unknown_receiver.uf2").is_empty());
    assert!(
        catalog
            .ota_target(EsbOtaKind::Tracker, "PROMICRO_UF2\\NRF52840\\SPI")
            .is_ok()
    );
    assert!(
        catalog
            .ota_target(EsbOtaKind::Tracker, "SLIMENRFTRK")
            .is_err()
    );
    assert!(catalog.ota_target(EsbOtaKind::Receiver, "MOCHI").is_err());
    assert!(
        catalog
            .ota_target(EsbOtaKind::Receiver, "xiao_ble/nrf52840")
            .is_ok()
    );
    let fallback = Catalog::default();
    assert_eq!(
        fallback.match_filename("SlimeNRF_Aero_Tracker_Pro.uf2")[0].board_target,
        "aero_pro_uf2/nrf52840"
    );
    assert_eq!(
        fallback.match_filename("SlimeNRF_Holyiot_Dongle_Receiver_DataCollect_CDC.hex")[0]
            .board_target,
        "holyiot_21017/nrf52840"
    );
    let promicro = catalog.match_filename("SlimeNRF_ProMicro_Receiver.uf2");
    assert_eq!(promicro[0].kind, Some(EsbOtaKind::Receiver));
}
#[test]
fn bootloader_only_presets_match_without_application_targets() {
    let raw =
            br#"{"schema_version":1,"firmware_root":".","legacy_targets":{"NICENANO":{"bootloader":"update-nicenano.uf2","targets":[]}}}"#;
    let catalog = Catalog::from_bytes(raw, Path::new("base")).unwrap();
    let preset = catalog.preset_for_volume("NICENANO", None).unwrap();
    assert!(preset.targets.is_empty());
    assert_eq!(
        preset.bootloader.as_ref(),
        Some(&Resource::File("base/update-nicenano.uf2".into()))
    );
    let hints = catalog.match_filename("downloads/UPDATE-NICENANO.UF2");
    assert_eq!(hints.len(), 1);
    assert_eq!(hints[0].board_target, "nicenano");
    assert_eq!(hints[0].kind, None);
    assert_eq!(hints[0].variant, "Bootloader");
    assert!(hints[0].bootloader);
    assert!(!hints[0].mcuboot);
    assert!(
        Catalog::from_bytes(
            br#"{"schema_version":1,"firmware_root":".","board_targets":{"empty":{"targets":[]}}}"#,
            Path::new("."),
        )
        .is_err()
    );
}

#[test]
fn preset_bootloader_keeps_each_board_but_never_application_roles() {
    let raw = br#"{"schema_version":1,"firmware_root":".","board_targets":{
            "shared":{"bootloader":"maintenance.uf2","targets":[
                {"name":"board/spi","firmware":"tracker.uf2","esb_ota_kind":"tracker"},
                {"name":"board/i2c","firmware":"receiver.uf2","esb_ota_kind":"receiver"}
            ]},
            "second":{"bootloader":"maintenance.uf2","targets":[
                {"name":"Holyiot-21017","firmware":"other.uf2","esb_ota_kind":"receiver"}
            ]}
        }}"#;
    let catalog = Catalog::from_bytes(raw, Path::new("base")).unwrap();
    let hints = catalog.match_filename("maintenance.uf2");
    let mut boards: Vec<_> = hints
        .iter()
        .map(|hint| hint.board_target.as_str())
        .collect();
    boards.sort_unstable();
    assert_eq!(boards, ["board/i2c", "board/spi", "holyiot_21017/nrf52840"]);
    assert!(hints.iter().all(|hint| {
        hint.kind.is_none() && hint.bootloader && !hint.mcuboot && hint.variant == "Bootloader"
    }));
    let application = catalog.match_filename("tracker.uf2");
    assert_eq!(application.len(), 1);
    assert_eq!(application[0].board_target, "board/spi");
    assert_eq!(application[0].kind, Some(EsbOtaKind::Tracker));
    assert!(!application[0].bootloader);
}

#[test]
fn local_categories_preserve_mcuboot_as_an_application_format() {
    let raw = br#"{"schema_version":1,"firmware_root":".","board_targets":{"board":{"targets":[{
            "name":"holyiot_21017/nrf52840","esb_ota_kind":"receiver",
            "firmware":"SlimeNRF_Holyiot_Receiver.uf2",
            "esb_ota_mcuboot_firmware":"SlimeNRF_Holyiot_Receiver_update.bin"
        }]}}}"#;
    let catalog = Catalog::from_bytes(raw, Path::new("base")).unwrap();
    for catalog in [catalog, Catalog::default()] {
        for (name, category, mcuboot, bootloader) in [
            ("SlimeNRF_Holyiot_Receiver.uf2", "Standard", false, false),
            (
                "SlimeNRF_Holyiot_Receiver_update.bin",
                "Standard",
                true,
                false,
            ),
            (
                "SlimeNRF_Holyiot_Receiver_DataCollect.hex",
                "DataCollect",
                false,
                false,
            ),
            (
                "SlimeNRF_Holyiot_Receiver_DataCollect_CDC.hex",
                "DataCollect CDC",
                false,
                false,
            ),
            (
                "SlimeNRF_Holyiot_Receiver_DataCollect_HID.hex",
                "DataCollect HID",
                false,
                false,
            ),
        ] {
            assert_eq!(filename_variant(name), category);
            let hints = catalog.match_filename(name);
            assert_eq!(hints.len(), 1, "{name}");
            let hint = &hints[0];
            assert_eq!(hint.board_target, "holyiot_21017/nrf52840", "{name}");
            assert_eq!(hint.variant, category, "{name}");
            assert_eq!(hint.mcuboot, mcuboot, "{name}");
            assert_eq!(hint.bootloader, bootloader, "{name}");
            assert_eq!(
                hint.kind,
                if bootloader {
                    None
                } else {
                    Some(EsbOtaKind::Receiver)
                },
                "{name}"
            );
        }
    }
    for (name, category) in [
        ("unknown_DataCollect.bin", "DataCollect"),
        ("unknown_DataCollect-CDC.bin", "DataCollect CDC"),
        ("unknown_DataCollect-HID.bin", "DataCollect HID"),
        ("unknown_mcuboot_update.bin", "Standard"),
        ("update-unknown.uf2", "Bootloader"),
        ("update-board.hex", "Bootloader"),
        ("update-board.uf2.zip", "Bootloader"),
        ("board_update.bin", "Standard"),
        ("unknown_bootloader_DataCollect_CDC.uf2", "Bootloader"),
        ("update-SlimeNRF_Holyiot_Receiver.uf2", "Bootloader"),
        ("SlimeNRF_Holyiot_Receiver_bootloader.bin", "Bootloader"),
    ] {
        assert_eq!(filename_variant(name), category);
        assert!(Catalog::default().match_filename(name).is_empty());
    }
}

#[test]
fn derives_ble_and_preserves_json_and_relative_paths() {
    let raw = br#"{"schema_version":1,"firmware_root":".","board_targets":{"b":{"name":"board","ota_firmware":"ota.zip","firmware":"a.uf2"}},"opendfu_targets":[{"name":"x","firmware":"x.hex"}]}"#;
    let catalog = Catalog::from_bytes(raw, Path::new("configdir")).unwrap();
    assert_eq!(
        catalog.ble_ota_targets[0].package,
        Resource::File("configdir/ota.zip".into())
    );
    assert_eq!(catalog.opendfu_targets[0].app_version, 1);
    assert!(
        Catalog::from_bytes(
            b"schema_version: 1\nfirmware_root: .\ninfo_filename: ../info.txt",
            Path::new(".")
        )
        .is_err()
    );
}
#[test]
fn ble_options_keep_distinct_variant_names_without_legacy_metadata() {
    let raw = br#"{"schema_version":1,"firmware_root":".","board_targets":{"b":{"name":"Shared board","targets":[{"name":"SPI","ota_firmware":"spi.zip"},{"name":"I2C","ota_firmware":"i2c.zip"}]}}}"#;
    let catalog = Catalog::from_bytes(raw, Path::new("configdir")).unwrap();
    assert_eq!(catalog.ble_ota_targets.len(), 2);
    assert_eq!(catalog.ble_ota_targets[0].display_name, "SPI");
    assert_eq!(catalog.ble_ota_targets[1].display_name, "I2C");
}

#[test]
fn no_external_config_loads_real_embedded_catalog_and_case_aliases() {
    let directory = tempfile::tempdir().unwrap();
    let exe = directory.path().join("exe/helper.exe");
    let cwd = directory.path().join("unrelated/work");
    std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
    std::fs::create_dir_all(&cwd).unwrap();
    let catalog = Catalog::load_default_in(Some(&exe), Some(&cwd)).unwrap();
    assert_eq!(catalog.poll_interval_seconds, 3.);
    let resources: Vec<_> = catalog
        .presets()
        .flat_map(|preset| {
            preset
                .bootloader
                .iter()
                .chain(preset.targets.iter().flat_map(|target| {
                    target
                        .firmware
                        .iter()
                        .chain(target.ota_firmware.iter())
                        .chain(target.esb_ota_firmware.iter())
                        .chain(target.esb_ota_mcuboot_firmware.iter())
                }))
        })
        .chain(
            catalog
                .opendfu_targets
                .iter()
                .map(|target| &target.firmware),
        )
        .collect();
    assert!(!resources.is_empty());
    assert!(
        resources
            .iter()
            .all(|resource| matches!(resource, Resource::Bundled(_)))
    );
    for filename in ["kawazu_tracker.uf2", "dawn_tracker_mini.uf2"] {
        let resource = resources
            .iter()
            .find(|resource| resource.filename() == Some(filename))
            .unwrap();
        let result = resource.read(64 * 1024 * 1024, &AtomicBool::new(false));
        assert_eq!(
            resource.is_intentionally_omitted(),
            !cfg!(feature = "embed-firmware")
        );
        if cfg!(feature = "embed-firmware") {
            assert!(
                slimenrf_firmware_image::image::parse_image(filename, &result.unwrap().bytes)
                    .is_ok()
            );
        } else {
            assert!(result.is_err());
        }
    }
}

#[test]
fn external_precedence_and_relative_absolute_resources_are_authoritative() {
    let directory = tempfile::tempdir().unwrap();
    let exe = directory.path().join("exe/helper.exe");
    let cwd = directory.path().join("parent/work");
    std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
    std::fs::create_dir_all(&cwd).unwrap();
    let absolute = directory.path().join("absolute.hex");
    std::fs::write(&absolute, b"absolute external").unwrap();
    let candidates = [
        exe.parent().unwrap().join("slimenrf-helper.yaml"),
        cwd.join("slimenrf-helper.yaml"),
    ];
    for (index, config) in candidates.iter().enumerate() {
        let raw = serde_json::json!({
            "schema_version": 1, "firmware_root": "payloads",
            "poll_interval_seconds": index + 10,
            "legacy_targets": {"external": {
                "firmware": "Kawazu_Tracker.uf2",
                "bootloader": "boot.uf2", "ota_firmware": "ota.zip",
                "esb_ota_firmware": "esb.hex", "esb_ota_mcuboot_firmware": "update.bin"
            }},
            "opendfu_targets": [{"name": "absolute", "firmware": absolute}]
        });
        std::fs::write(config, serde_json::to_vec(&raw).unwrap()).unwrap();
    }
    for (index, config) in candidates.iter().enumerate() {
        let catalog = Catalog::load_default_in(Some(&exe), Some(&cwd)).unwrap();
        assert_eq!(catalog.poll_interval_seconds, (index + 10) as f64);
        let preset = &catalog.legacy_targets["external"];
        let target = &preset.targets[0];
        for (resource, relative) in [
            (target.firmware.as_ref().unwrap(), "Kawazu_Tracker.uf2"),
            (preset.bootloader.as_ref().unwrap(), "boot.uf2"),
            (target.ota_firmware.as_ref().unwrap(), "ota.zip"),
            (target.esb_ota_firmware.as_ref().unwrap(), "esb.hex"),
            (
                target.esb_ota_mcuboot_firmware.as_ref().unwrap(),
                "update.bin",
            ),
        ] {
            assert_eq!(
                resource,
                &Resource::File(config.parent().unwrap().join("payloads").join(relative))
            );
        }
        assert!(
            target
                .firmware
                .as_ref()
                .unwrap()
                .read(64 * 1024 * 1024, &AtomicBool::new(false))
                .is_err()
        );
        assert_eq!(
            catalog.ble_ota_targets[0].package,
            *target.ota_firmware.as_ref().unwrap()
        );
        assert_eq!(
            catalog.opendfu_targets[0].firmware,
            Resource::File(absolute.clone())
        );
        assert_eq!(
            catalog.opendfu_targets[0]
                .firmware
                .read(100, &AtomicBool::new(false))
                .unwrap()
                .bytes,
            b"absolute external"
        );
        std::fs::remove_file(config).unwrap();
    }
}

#[test]
fn malformed_selected_external_config_never_uses_another_config_or_bundle() {
    let directory = tempfile::tempdir().unwrap();
    let exe = directory.path().join("exe/helper.exe");
    let cwd = directory.path().join("work");
    std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::write(
        cwd.join("slimenrf-helper.yaml"),
        b"schema_version: 1\nfirmware_root: .\npoll_interval_seconds: 9",
    )
    .unwrap();
    std::fs::write(
        exe.parent().unwrap().join("slimenrf-helper.yaml"),
        b"legacy_targets: [unterminated",
    )
    .unwrap();
    assert!(Catalog::load_default_in(Some(&exe), Some(&cwd)).is_err());
}

#[test]
fn embedded_conversion_covers_all_fields_without_disk_paths() {
    let raw = br#"{"schema_version":1,"firmware_root":".","legacy_targets":{"board":{
            "firmware":"./FiRmWaReS/Kawazu_Tracker.uf2",
            "bootloader":"Firmwares/boot.uf2", "ota_firmware":"firmwares/ota.zip",
            "esb_ota_firmware":"firmwares/esb.hex", "esb_ota_mcuboot_firmware":"firmwares/update.bin"
        }},"opendfu_targets":[{"name":"dfu","firmware":"firmwares/dfu.hex","hw_version":81,"app_version":42,"sd_req":"0x12"}]}"#;
    let catalog = Catalog::from_resource_bytes(raw, None).unwrap();
    let preset = &catalog.legacy_targets["board"];
    let target = &preset.targets[0];
    for (resource, member) in [
        (
            target.firmware.as_ref().unwrap(),
            if cfg!(feature = "embed-firmware") {
                "firmwares/kawazu_tracker.uf2"
            } else {
                "firmwares/Kawazu_Tracker.uf2"
            },
        ),
        (preset.bootloader.as_ref().unwrap(), "firmwares/boot.uf2"),
        (target.ota_firmware.as_ref().unwrap(), "firmwares/ota.zip"),
        (
            target.esb_ota_firmware.as_ref().unwrap(),
            "firmwares/esb.hex",
        ),
        (
            target.esb_ota_mcuboot_firmware.as_ref().unwrap(),
            "firmwares/update.bin",
        ),
        (&catalog.opendfu_targets[0].firmware, "firmwares/dfu.hex"),
    ] {
        assert_eq!(resource, &Resource::Bundled(member.into()));
    }
    assert_eq!(
        catalog.ble_ota_targets[0].package,
        Resource::Bundled("firmwares/ota.zip".into())
    );
    assert_eq!(catalog.opendfu_targets[0].hw_version, 81);
    assert_eq!(catalog.opendfu_targets[0].app_version, 42);
    assert_eq!(catalog.opendfu_targets[0].sd_req, "0x12");
    assert!(
            Catalog::from_resource_bytes(
                br#"{"schema_version":1,"firmware_root":".","legacy_targets":{"board":{"firmware":"../escape.uf2"}}}"#,
                None
            )
            .is_err()
        );
}

#[test]
fn schema_and_root_are_required_and_legacy_parent_config_is_ignored() {
    for raw in [
        b"firmware_root: .".as_slice(),
        b"schema_version: 2\nfirmware_root: .",
        b"schema_version: 1\nfirmware_root: ''",
        b"schema_version: 1",
    ] {
        assert!(Catalog::from_bytes(raw, Path::new(".")).is_err());
    }
    let directory = tempfile::tempdir().unwrap();
    let cwd = directory.path().join("work");
    std::fs::create_dir(&cwd).unwrap();
    std::fs::write(directory.path().join("slimenrf-helper.yaml"), b"invalid: [").unwrap();
    std::fs::write(cwd.join("config.yaml"), b"invalid: [").unwrap();
    assert!(Catalog::load_default_in(None, Some(&cwd)).is_ok());
}

#[test]
fn bootloader_version_hints_preserve_board_stem_and_application_matching() {
    let raw = br#"{"schema_version":1,"firmware_root":".","board_targets":{
            "board":{"name":"custom_board","bootloader_board":"slimenrf_promicro","bootloader_mcu":"nrf52840","firmware":"app-0.11.0.uf2"}
        }}"#;
    let catalog = Catalog::from_bytes(raw, Path::new(".")).unwrap();
    let hints =
        catalog.match_filename("update-slimenrf_promicro_bootloader-0.11.1-1-gabcdef0_nosd.uf2");
    assert_eq!(hints.len(), 1);
    assert!(hints[0].bootloader && hints[0].kind.is_none());
    assert_eq!(
        catalog.match_filename("update-slimenrf_promicro_bootloader-0.12.0-slimenrf_nosd.uf2"),
        hints
    );
    for name in [
        "update-slimenrf_promicro_bootloader-0.11.1.uf2",
        "update-other_promicro_bootloader-0.11.1-1-gabcdef0_nosd.uf2",
        "update-slimenrf_promicro_bootloader-0.11.1_s140_7.3.0.uf2",
    ] {
        assert!(catalog.match_filename(name).is_empty(), "{name}");
    }
    assert_eq!(catalog.match_filename("app-0.11.0.uf2").len(), 1);
    assert!(catalog.match_filename("app-0.11.1.uf2").is_empty());
}

#[test]
fn bootloader_ci_board_hints_share_filename_semantics_without_global_bare_name_matching() {
    let raw = br#"{"schema_version":1,"firmware_root":".","board_targets":{
            "shared":{"bootloader_board":"board","bootloader_mcu":"nrf52840","targets":[
                {"name":"board/spi","firmware":"tracker.uf2","esb_ota_kind":"tracker"},
                {"name":"board/i2c","firmware":"receiver.uf2","esb_ota_kind":"receiver"}
            ]},
            "duplicate":{"bootloader_board":"board","bootloader_mcu":"nrf52840",
                "name":"board/spi","firmware":"duplicate.uf2"}
        },"legacy_targets":{
            "BOOTONLY":{"bootloader_board":"bootonly","bootloader_mcu":"nrf52833",
                "bootloader":"update-bootonly_bootloader-0.11.1_nosd.uf2","targets":[]},
            "IMPLICIT":{"bootloader":"update-implicit_bootloader-0.11.1_nosd.uf2","targets":[]}
        }}"#;
    let catalog = Catalog::from_bytes(raw, Path::new(".")).unwrap();
    for (board, count) in [("board", 2), ("bootonly", 1)] {
        let hints = catalog.match_bootloader_board(board);
        assert_eq!(hints.len(), count, "{board}");
        assert_eq!(
            hints,
            catalog.match_filename(&format!("update-{board}_bootloader-main_nosd.uf2")),
            "{board}"
        );
        assert!(hints.iter().all(|hint| {
            hint.kind.is_none() && hint.bootloader && !hint.mcuboot && hint.variant == "Bootloader"
        }));
        assert!(catalog.match_filename(board).is_empty(), "{board}");
    }
    for board in ["unknown", "BOARD", "board.zip", "path/board", "implicit"] {
        assert!(catalog.match_bootloader_board(board).is_empty(), "{board}");
    }
    assert_eq!(
        catalog
            .match_filename("update-implicit_bootloader-main_nosd.uf2")
            .len(),
        1
    );
}

#[test]
fn external_remote_sources_are_owned_and_validated() {
    let raw = br#"{"schema_version":1,"firmware_root":"payloads","remote_firmware":{
            "application":{"releases":{"owner":"custom-owner","name":"custom-app"}},
            "bootloader":{
                "releases":{"owner":"custom-owner","name":"custom-bootloader"},
                "ci":{"owner":"build-owner","name":"bootloader-builds"}
            }
        }}"#;
    let catalog = Catalog::from_bytes(raw, Path::new("configdir")).unwrap();
    assert_eq!(
        catalog.remote_firmware.application.releases.name,
        "custom-app"
    );
    assert_eq!(
        catalog.remote_firmware.bootloader.releases.name,
        "custom-bootloader"
    );
    assert_eq!(catalog.remote_firmware.bootloader.ci.owner, "build-owner");
    assert_eq!(
        catalog.remote_firmware.bootloader.ci.name,
        "bootloader-builds"
    );
    let release_only = Catalog::from_bytes(
        br#"{"schema_version":1,"firmware_root":".","remote_firmware":{
            "bootloader":{"releases":{"owner":"custom-owner","name":"custom-bootloader"}}
        }}"#,
        Path::new("."),
    )
    .unwrap();
    assert_eq!(release_only.remote_firmware.bootloader.ci.owner, "jitingcn");
    assert_eq!(
        release_only.remote_firmware.bootloader.ci.name,
        "Adafruit_nRF52_Bootloader"
    );
    assert_eq!(
        catalog.remote_firmware.application.tracker_ci.name,
        "SlimeVR-Tracker-nRF"
    );
    assert!(
        Catalog::from_bytes(
            br#"{"schema_version":1,"firmware_root":".","remote_firmware":{
                "bootloader":{"releases":{"owner":"../escape","name":"bootloader"}}
            }}"#,
            Path::new("."),
        )
        .is_err()
    );
    assert!(
        Catalog::from_bytes(
            br#"{"schema_version":1,"firmware_root":".","remote_firmware":{
                "bootloader":{"ci":{"owner":"custom-owner","name":"../bootloader"}}
            }}"#,
            Path::new("."),
        )
        .is_err()
    );
}

#[test]
fn scalar_alias_expansion_is_bounded_before_labels_are_owned() {
    let label = "x".repeat(32 * 1024);
    let raw = format!(
        "schema_version: 1\nfirmware_root: .\nboard_volume_labels: [&large '{label}', {}]",
        ["*large"; 1023].join(", ")
    );
    assert!(
        raw.len() < 1024 * 1024,
        "the input-file bound alone cannot reject this"
    );
    let error = Catalog::from_bytes(raw.as_bytes(), Path::new(".")).unwrap_err();
    assert!(error.contains("expanded text"), "{error}");
}

#[test]
fn collection_alias_expansion_is_bounded_before_target_validation() {
    let raw = format!(
        "schema_version: 1\nfirmware_root: .\nboard_targets:\n  many:\n    targets: [&target {{name: shared, firmware: app.uf2, esb_ota_kind: tracker}}, {}]",
        ["*target"; 3000].join(", ")
    );
    let error = Catalog::from_bytes(raw.as_bytes(), Path::new(".")).unwrap_err();
    assert!(error.contains("node count"), "{error}");
}

#[test]
fn nested_alias_growth_and_ignored_fields_share_the_catalog_budget() {
    let mut raw = "schema_version: 1\nfirmware_root: .\na: &a [null, null, null, null, null, null, null, null, null, null]\n".to_owned();
    for (previous, current) in [('a', 'b'), ('b', 'c'), ('c', 'd'), ('d', 'e')] {
        raw.push_str(&format!(
            "{current}: &{current} [{}]\n",
            vec![format!("*{previous}"); 10].join(", ")
        ));
    }
    let error = Catalog::from_bytes(raw.as_bytes(), Path::new(".")).unwrap_err();
    assert!(error.contains("node count"), "{error}");
}

#[test]
fn catalog_nesting_is_bounded_before_buffering_untagged_labels() {
    let raw = format!(
        "schema_version: 1\nfirmware_root: .\nboard_volume_labels: {}label{}",
        "[".repeat(64),
        "]".repeat(64)
    );
    let error = Catalog::from_bytes(raw.as_bytes(), Path::new(".")).unwrap_err();
    assert!(error.contains("nesting depth"), "{error}");
}

#[test]
fn duplicate_catalog_mapping_keys_are_rejected_at_every_schema_level() {
    for raw in [
        "schema_version: 1\nschema_version: 1\nfirmware_root: .",
        "schema_version: 1\nfirmware_root: .\nboard_targets:\n  board: {firmware: first.uf2}\n  board: {firmware: second.uf2}",
        "schema_version: 1\nfirmware_root: .\nboard_targets:\n  board: {name: first, name: second, firmware: app.uf2}",
        "schema_version: 1\nfirmware_root: .\nremote_firmware:\n  application:\n    releases: {owner: example, name: first, name: second}",
    ] {
        let error = Catalog::from_bytes(raw.as_bytes(), Path::new(".")).unwrap_err();
        assert!(error.contains("duplicate mapping key"), "{raw}: {error}");
    }
}

#[test]
fn malformed_catalog_schema_and_unknown_remote_fields_stay_rejected() {
    for raw in [
        "schema_version: wrong\nfirmware_root: .",
        "schema_version: 1\nfirmware_root: [not, a, path]",
        "schema_version: 1\nfirmware_root: .\nboard_volume_labels: [[nested]]",
        "schema_version: 1\nfirmware_root: .\nboard_targets: [not_a_mapping]",
        "schema_version: 1\nfirmware_root: .\nremote_firmware:\n  application:\n    releases: {owner: example, name: repo, unexpected: true}",
        "schema_version: 1\nfirmware_root: .\n? [complex, key]\n: ignored",
    ] {
        assert!(
            Catalog::from_bytes(raw.as_bytes(), Path::new(".")).is_err(),
            "{raw}"
        );
    }
}

#[test]
fn ordinary_scalar_and_collection_aliases_preserve_resource_resolution() {
    let raw = b"schema_version: 1
firmware_root: payloads
board_volume_labels: [&label CUSTOM, *label]
legacy_targets:
  original: &preset
    name: custom_board
    firmware: &image app.uf2
    success: &success SUCCESS
board_targets:
  copy: *preset
  second:
    name: other_board
    firmware: *image
    success: *success
";
    let catalog = Catalog::from_bytes(raw, Path::new("configdir")).unwrap();
    assert_eq!(catalog.board_volume_labels, ["CUSTOM", "CUSTOM"]);
    for preset in catalog.presets() {
        assert_eq!(preset.success.as_deref(), Some("SUCCESS"));
        assert_eq!(
            preset.targets[0].firmware,
            Some(Resource::File("configdir/payloads/app.uf2".into()))
        );
    }
    assert_eq!(catalog.board_targets["copy"].display_name, "custom_board");
}

#[test]
fn reasonable_external_catalog_loads_without_changing_default_precedence() {
    let directory = tempfile::tempdir().unwrap();
    let exe = directory.path().join("helper.exe");
    let config = directory.path().join("slimenrf-helper.yaml");
    let mut raw =
        "schema_version: 1\nfirmware_root: payloads\npoll_interval_seconds: 7\nboard_targets:\n"
            .to_owned();
    for index in 0..256 {
        raw.push_str(&format!(
            "  board{index}: {{name: board{index}, firmware: app{index}.uf2, esb_ota_kind: tracker}}\n"
        ));
    }
    std::fs::write(&config, raw).unwrap();
    let catalog = Catalog::load_default_in(Some(&exe), None).unwrap();
    assert_eq!(catalog.board_targets.len(), 256);
    assert_eq!(catalog.poll_interval_seconds, 7.);
    assert_eq!(
        catalog.board_targets["board255"].targets[0].firmware,
        Some(Resource::File(directory.path().join("payloads/app255.uf2")))
    );
}

#[test]
fn numeric_looking_aliases_cannot_bypass_owned_catalog_text_limits() {
    let scalar = format!("0x{}1", "0".repeat(32 * 1024));
    let mut raw = format!(
        "schema_version: 1\nfirmware_root: .\nboard_targets:\n  first: {{firmware: app.uf2, success: &large {scalar}}}\n"
    );
    for index in 0..1024 {
        raw.push_str(&format!(
            "  board{index}: {{firmware: app.uf2, success: *large}}\n"
        ));
    }
    assert!(raw.len() < 1024 * 1024);
    let error = Catalog::from_bytes(raw.as_bytes(), Path::new(".")).unwrap_err();
    assert!(error.contains("expanded text"), "{error}");
}

mod version_matching;
