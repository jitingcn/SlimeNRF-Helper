use super::support::target;
use crate::application::flash::images::{
    load_local_image, local_image_provenance, single_firmware_path,
};
use crate::application::flash::{
    Action, FlashState, Mode, load_image, make_image, ota_target_key, plan_ota_groups,
};
use crate::features::firmware::bundled_resources::Resource;
use std::sync::Arc;

#[cfg(feature = "embed-firmware")]
#[test]
fn bundled_image_identity_retains_configured_dfu_options_and_notes() {
    use crate::features::firmware::board_catalog::{BleOtaTarget, Catalog, OpenDfuTarget};
    let resource = Resource::bundled("firmwares/SlimeNRF_Holyiot_Dongle_Receiver.hex").unwrap();
    let catalog = Catalog {
        opendfu_targets: vec![OpenDfuTarget {
            name: "custom-board".into(),
            firmware: resource.clone(),
            hw_version: 81,
            app_version: 42,
            sd_req: "0x12,0x34".into(),
            success: Some("configured DFU note".into()),
        }],
        ..Catalog::default()
    };
    let image = load_image(resource.clone(), &std::sync::atomic::AtomicBool::new(false)).unwrap();
    assert_eq!(image.resource.as_deref(), Some(&resource));
    assert!(matches!(
        image.resource.as_deref(),
        Some(Resource::Bundled(_))
    ));
    assert!(image.parsed.is_some());
    assert!(image.local);
    let configured = image.configured_opendfu(&catalog).unwrap();
    assert_eq!(configured.name, "custom-board");
    assert_eq!(configured.success.as_deref(), Some("configured DFU note"));
    let options = crate::application::flash::configured_opendfu_options(Some(configured)).unwrap();
    assert_eq!(options.hw_version, 81);
    assert_eq!(options.app_version, 42);
    assert_eq!(options.sd_req, [0x12, 0x34]);
    let mut external = catalog.clone();
    external.opendfu_targets[0].firmware =
        Resource::File("firmwares/SlimeNRF_Holyiot_Dongle_Receiver.hex".into());
    assert!(image.configured_opendfu(&external).is_none());
    let package =
        Resource::bundled("firmwares/SlimeNRF_Tracker_SPI_SD_StackedSmol-mod_ota.zip").unwrap();
    let mut catalog = catalog;
    catalog.ble_ota_targets.push(BleOtaTarget {
        package: package.clone(),
        display_name: "configured BLE board".into(),
        success: Some("configured BLE note".into()),
    });
    let image = load_image(package, &std::sync::atomic::AtomicBool::new(false)).unwrap();
    assert!(image.local);
    assert_eq!(
        catalog
            .ble_ota_targets
            .iter()
            .find(|target| image.resource.as_deref() == Some(&target.package))
            .and_then(|target| target.success.as_deref()),
        Some("configured BLE note")
    );
}

#[cfg(not(feature = "embed-firmware"))]
#[test]
fn omitted_bundled_images_fail_without_affecting_explicit_local_images() {
    let directory = tempfile::tempdir().unwrap();
    let cancel = std::sync::atomic::AtomicBool::new(false);
    for (filename, bytes) in [
        (
            "SlimeNRF_Holyiot_Dongle_Receiver.hex",
            b":0410000001020304E2\n:00000001FF\n".as_slice(),
        ),
        (
            "SlimeNRF_Tracker_SPI_SD_StackedSmol-mod_ota.zip",
            b"local package".as_slice(),
        ),
    ] {
        let bundled = Resource::bundled(&format!("firmwares/{filename}")).unwrap();
        assert!(bundled.is_intentionally_omitted());
        assert!(load_image(bundled, &cancel).is_err());
        let path = directory.path().join(filename);
        std::fs::write(&path, bytes).unwrap();
        let external = Resource::File(path);
        assert!(!external.is_intentionally_omitted());
        let image = load_image(external.clone(), &cancel).unwrap();
        assert_eq!(image.bytes.as_ref(), bytes);
        assert_eq!(image.resource.as_deref(), Some(&external));
    }
}

#[test]
fn configured_relative_image_load_keeps_identity_and_cleans_provenance() {
    let directory = tempfile::tempdir().unwrap();
    let firmware_dir = directory.path().join("firmwares");
    std::fs::create_dir(&firmware_dir).unwrap();
    let bytes = b":0410000001020304E2\n:00000001FF\n";
    std::fs::write(firmware_dir.join("board.hex"), bytes).unwrap();
    std::fs::write(firmware_dir.join("board.zip"), b"local package").unwrap();
    let config = directory.path().join("slimenrf-helper.yaml");
    std::fs::write(
        &config,
        br#"{"schema_version":1,"firmware_root":".",
            "legacy_targets":{"board":{"firmware":"./firmwares/board.hex",
                "ota_firmware":"./firmwares/board.zip","success":"Board updated"}},
            "opendfu_targets":[{"name":"board","firmware":"./firmwares/board.hex",
                "success":"OpenDFU updated"}]
        }"#,
    )
    .unwrap();
    let catalog = crate::features::firmware::board_catalog::Catalog::load(&config).unwrap();
    let configured = &catalog.opendfu_targets[0].firmware;
    let image = load_image(
        configured.clone(),
        &std::sync::atomic::AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(image.bytes.as_ref(), bytes);
    assert_eq!(
        image.provenance,
        firmware_dir.join("board.hex").display().to_string()
    );
    assert_eq!(image.resource.as_deref(), Some(configured));
    assert!(matches!(image.resource.as_deref(), Some(Resource::File(_))));
    assert_eq!(
        catalog
            .opendfu_targets
            .iter()
            .find(|target| image.resource.as_deref() == Some(&target.firmware))
            .and_then(|target| target.success.as_deref()),
        Some("OpenDFU updated")
    );
    let package = &catalog.ble_ota_targets[0].package;
    let image = load_image(package.clone(), &std::sync::atomic::AtomicBool::new(false)).unwrap();
    assert_eq!(image.bytes.as_ref(), b"local package");
    assert_eq!(
        image.provenance,
        firmware_dir.join("board.zip").display().to_string()
    );
    assert_eq!(
        catalog
            .ble_ota_targets
            .iter()
            .find(|target| image.resource.as_deref() == Some(&target.package))
            .and_then(|target| target.success.as_deref()),
        Some("Board updated")
    );
}

#[test]
fn remote_image_provenance_is_not_treated_as_a_local_path() {
    let provenance =
        "https://example.com/releases/./build | member firmwares/board.hex | member SHA-256 abc";
    let image = make_image(
        "board.hex".into(),
        b":0410000001020304E2\n:00000001FF\n".to_vec(),
        provenance.into(),
        "abc".into(),
        false,
    );
    assert_eq!(image.provenance, provenance);
    assert!(image.resource.is_none());
}

#[cfg(windows)]
#[test]
fn local_image_provenance_preserves_windows_path_kinds() {
    for (input, expected) in [
        (
            r"C:\example workspace\./firmwares/board.uf2",
            r"C:\example workspace\firmwares\board.uf2",
        ),
        (r"C:/", r"C:\"),
        (r"C:./firmwares/board.hex", r"C:firmwares\board.hex"),
        (r"/firmwares/./board.hex", r"\firmwares\board.hex"),
        (
            r"//server/share/./firmwares/board.hex",
            r"\\server\share\firmwares\board.hex",
        ),
        (r"\\server\share\", r"\\server\share\"),
        (r"C:\link\..\board.hex", r"C:\link\..\board.hex"),
        (
            r"\\?\C:\firmwares\.\board.hex",
            r"\\?\C:\firmwares\.\board.hex",
        ),
        (
            r"\\?\UNC\server\share\firmwares\board.hex",
            r"\\?\UNC\server\share\firmwares\board.hex",
        ),
        (r"\\?\C:\firmwares\a/b.hex", r"\\?\C:\firmwares\a/b.hex"),
        (
            r"\\.\C:\firmwares\.\board.hex",
            r"\\.\C:\firmwares\.\board.hex",
        ),
    ] {
        assert_eq!(
            local_image_provenance(std::path::Path::new(input)),
            expected,
            "{input}"
        );
    }
}

#[cfg(unix)]
#[test]
fn local_image_provenance_preserves_unix_literal_backslashes_and_parents() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join(r"board\variant.hex");
    let bytes = b":0410000001020304E2\n:00000001FF\n";
    std::fs::write(&path, bytes).unwrap();
    let image = load_image(
        Resource::File(directory.path().join("./").join(r"board\variant.hex")),
        &std::sync::atomic::AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(image.bytes.as_ref(), bytes);
    assert_eq!(image.provenance, path.display().to_string());
    assert_eq!(
        local_image_provenance(std::path::Path::new("/link/.././board.hex")),
        "/link/../board.hex"
    );
    assert_eq!(local_image_provenance(std::path::Path::new("/")), "/");
}

#[test]
fn invalid_archive_member_does_not_discard_usable_firmware() {
    let images = [
        make_image(
            "raw.bin".into(),
            vec![0; 32],
            "CI member".into(),
            String::new(),
            false,
        ),
        make_image(
            "valid.hex".into(),
            b":0410000001020304E2\n:00000001FF\n".to_vec(),
            "CI member".into(),
            String::new(),
            false,
        ),
    ];
    assert!(images[0].validation_error.is_some());
    assert!(images[0].parsed.is_none());
    assert!(images[1].validation_error.is_none());
    assert_eq!(images[1].parsed.as_ref().unwrap().data, [1, 2, 3, 4]);
}

#[test]
fn external_firmware_selection_requires_exactly_one_path() {
    use std::path::PathBuf;

    assert!(single_firmware_path(Vec::new().into_iter()).is_err());
    assert!(
        single_firmware_path(
            vec![PathBuf::from("first.hex"), PathBuf::from("second.hex")].into_iter()
        )
        .is_err()
    );
    let path = PathBuf::from("firmware folder").join("board.hex");
    assert_eq!(
        single_firmware_path(std::iter::once(path.clone())).unwrap(),
        path
    );
}

#[test]
fn external_local_firmware_uses_existing_validation_and_resource_identity() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("local 固件.hex");
    let bytes = b":0410000001020304E2\n:00000001FF\n";
    std::fs::write(&path, bytes).unwrap();
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let image = load_local_image(path.clone(), &cancel).unwrap();
    assert_eq!(image.name, "local 固件.hex");
    assert_eq!(image.bytes.as_ref(), bytes);
    assert_eq!(
        image.resource.as_deref(),
        Some(&Resource::File(path.clone()))
    );
    assert_eq!(image.provenance, local_image_provenance(&path));
    assert_eq!(image.parsed.as_ref().unwrap().data, [1, 2, 3, 4]);
    assert!(image.local);
    assert!(!image.digest.is_empty());

    let unsupported = directory.path().join("board.txt");
    std::fs::write(&unsupported, bytes).unwrap();
    assert!(load_local_image(unsupported, &cancel).is_err());
    let malformed = directory.path().join("malformed.hex");
    std::fs::write(&malformed, b"not Intel HEX").unwrap();
    assert!(load_local_image(malformed, &cancel).is_err());
    assert!(load_local_image(path, &std::sync::atomic::AtomicBool::new(true)).is_err());
}

#[test]
fn external_local_firmware_rejects_directories_and_missing_files() {
    let directory = tempfile::tempdir().unwrap();
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let firmware_directory = directory.path().join("looks-like-firmware.hex");
    std::fs::create_dir(&firmware_directory).unwrap();
    assert!(load_local_image(firmware_directory, &cancel).is_err());
    assert!(load_local_image(directory.path().join("missing.hex"), &cancel).is_err());
}

#[test]
fn incompatible_local_replacement_preserves_validated_ota_assignment() {
    use sha2::{Digest, Sha256};

    let directory = tempfile::tempdir().unwrap();
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let load_mcuboot = |name: &str, payload: u8| {
        let mut bytes = vec![0; 76];
        bytes[..4].copy_from_slice(&0x96f3_b83du32.to_le_bytes());
        bytes[8..10].copy_from_slice(&32u16.to_le_bytes());
        bytes[12..16].copy_from_slice(&4u32.to_le_bytes());
        bytes[32..36].fill(payload);
        bytes[36..40].copy_from_slice(&[7, 0x69, 40, 0]);
        bytes[40..44].copy_from_slice(&[0x10, 0, 32, 0]);
        let digest = Sha256::digest(&bytes[..36]);
        bytes[44..].copy_from_slice(&digest);
        let path = directory.path().join(name);
        std::fs::write(&path, bytes).unwrap();
        load_local_image(path, &cancel).unwrap()
    };
    let mut tracker = target("a", 0, "board");
    tracker.info.bootloader = "mcuboot".into();
    let mut state = FlashState {
        mode: Mode::Esb,
        activity_origin: Mode::Esb,
        selected_targets: [ota_target_key(&tracker)].into(),
        targets: vec![tracker],
        ..FlashState::default()
    };
    state.reconcile_groups(None);
    let destination = state.destination(Mode::Esb).unwrap();
    state.apply_images(destination.clone(), vec![load_mcuboot("original.bin", 1)]);
    assert_eq!(
        state.displayed_image(Mode::Esb).unwrap().name,
        "original.bin"
    );
    assert!(state.error.is_none());

    let replacement = load_mcuboot("replacement.bin", 2);
    state.apply_images(destination.clone(), vec![replacement.clone()]);
    assert!(Arc::ptr_eq(
        &state.displayed_image(Mode::Esb).unwrap().bytes,
        &replacement.bytes
    ));
    assert_eq!(
        plan_ota_groups(&state).unwrap()[0].image.name,
        "replacement.bin"
    );
    state.review = Some(Action::EsbGroups(plan_ota_groups(&state).unwrap()));
    state.acknowledged = true;
    state.auto = true;
    state.auto_requested = true;

    let path = directory.path().join("incompatible.hex");
    std::fs::write(&path, b":0410000001020304E2\n:00000001FF\n").unwrap();
    let incompatible = load_local_image(path, &cancel).unwrap();
    assert!(incompatible.validation_error.is_none());
    assert!(!incompatible.parsed.as_ref().unwrap().mcuboot);
    state.apply_images(destination.clone(), vec![incompatible]);

    let retained = state.displayed_image(Mode::Esb).unwrap();
    assert_eq!(retained.name, replacement.name);
    assert_eq!(retained.resource, replacement.resource);
    assert_eq!(retained.digest, replacement.digest);
    assert!(Arc::ptr_eq(&retained.bytes, &replacement.bytes));
    assert_eq!(
        plan_ota_groups(&state).unwrap()[0].image.name,
        "replacement.bin"
    );
    assert!(state.error.is_some());
    assert!(state.error_origin == Mode::Esb);
    assert!(state.destination(Mode::Esb).as_ref() == Some(&destination));
    assert!(state.review.is_none());
    assert!(!state.acknowledged);
    assert!(!state.auto);
    assert!(!state.auto_requested);
    assert!(!state.busy());
}
