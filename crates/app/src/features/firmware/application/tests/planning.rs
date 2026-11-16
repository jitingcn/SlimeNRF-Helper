use super::support::{local_image, target};
use crate::application::flash::{
    ConfirmationPolicy, FlashState, OtaQueries, ota_assignment_key, ota_target_key,
    plan_ota_groups, validate_ota_image,
};
use crate::features::firmware::esb_ota::RECEIVER_ID;
use slimenrf_firmware_image::image::{Nrf52Chip, Uf2ImagePurpose};
use std::sync::Arc;

#[test]
fn receiver_qualified_slots_do_not_collide() {
    let a = target("receiver:a", 0, "board");
    let b = target("receiver:b", 0, "board");
    assert_ne!(ota_target_key(&a), ota_target_key(&b));
    assert_eq!(ota_assignment_key(&a), ota_assignment_key(&b));
    let state = FlashState {
        targets: vec![a.clone(), b],
        selected_targets: [ota_target_key(&a)].into(),
        assignments: [(ota_assignment_key(&a), local_image("a.hex"))].into(),
        ..FlashState::default()
    };
    let groups = plan_ota_groups(&state).unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].targets.len(), 1);
    assert_eq!(groups[0].targets[0].receiver.key, a.receiver.key);
}

#[test]
fn grouped_plan_freezes_board_images_and_orders_relays_before_receivers() {
    let targets = vec![
        target("a", RECEIVER_ID, "receiver-board"),
        target("b", 0, "board-one"),
        target("a", 0, "board-one"),
        target("a", 1, "board-one"),
        target("a", 2, "board-two"),
    ];
    let mut state = FlashState {
        selected_targets: targets.iter().map(ota_target_key).collect(),
        assignments: targets
            .iter()
            .map(|target| {
                (
                    ota_assignment_key(target),
                    local_image(&format!("{}.hex", target.info.board_target)),
                )
            })
            .collect(),
        targets,
        ..FlashState::default()
    };
    assert!(state.image.is_none());
    let groups = plan_ota_groups(&state).unwrap();
    assert_eq!(groups.len(), 4);
    assert_eq!(groups[0].targets.len(), 2);
    assert_eq!(groups.last().unwrap().targets[0].id, RECEIVER_ID);
    for group in &groups {
        let first = &group.targets[0];
        assert_eq!(group.image.name, format!("{}.hex", first.info.board_target));
        assert!(
            group
                .targets
                .iter()
                .all(|target| target.receiver.key == first.receiver.key
                    && ota_assignment_key(target) == ota_assignment_key(first))
        );
        assert!(Arc::ptr_eq(
            &group.image.bytes,
            &state.assignments[&ota_assignment_key(first)].bytes
        ));
    }
    state.assignments.clear();
    assert_eq!(groups[0].image.name, "board-one.hex");
}

#[test]
fn every_selected_board_and_role_requires_explicit_assignment() {
    let tracker = target("a", 0, "board");
    let receiver = target("a", RECEIVER_ID, "board");
    let other = target("b", 0, "other-board");
    let mut state = FlashState {
        selected_targets: [&tracker, &receiver, &other]
            .into_iter()
            .map(ota_target_key)
            .collect(),
        targets: vec![tracker.clone(), receiver.clone(), other.clone()],
        image: Some(local_image("current.hex")),
        assignments: [(ota_assignment_key(&tracker), local_image("tracker.hex"))].into(),
        ..FlashState::default()
    };
    assert!(plan_ota_groups(&state).is_err());
    state
        .assignments
        .insert(ota_assignment_key(&receiver), local_image("receiver.hex"));
    assert!(plan_ota_groups(&state).is_err());
    state
        .assignments
        .insert(ota_assignment_key(&other), local_image("other.hex"));
    assert_eq!(plan_ota_groups(&state).unwrap().len(), 3);
    state.selected_targets.insert("vanished:7".into());
    assert!(plan_ota_groups(&state).is_err());
}

#[test]
fn assigned_images_still_enforce_mcuboot_protocol_and_known_board() {
    let mut device = target("a", 0, "board");
    let mut image = local_image("image.hex");
    assert!(validate_ota_image(&image, &device, ConfirmationPolicy::Standard).is_ok());
    device.info.bootloader = "mcuboot".into();
    assert!(validate_ota_image(&image, &device, ConfirmationPolicy::Standard).is_err());
    device.info.bootloader = "none".into();
    device.info.protocol_version = 2;
    assert!(validate_ota_image(&image, &device, ConfirmationPolicy::Standard).is_err());
    device.info.protocol_version = 1;
    image.local = false;
    assert!(validate_ota_image(&image, &device, ConfirmationPolicy::Standard).is_err());
    image.local = true;
    image.hints = vec![crate::features::firmware::board_catalog::FirmwareHint {
        board_target: "another-board".into(),
        kind: Some(crate::features::firmware::board_catalog::EsbOtaKind::Tracker),
        variant: "Standard".into(),
        mcuboot: false,
        bootloader: false,
    }]
    .into();
    assert!(validate_ota_image(&image, &device, ConfirmationPolicy::Standard).is_err());
}

#[test]
fn refresh_preserves_explicit_selection_without_selecting_new_targets() {
    let previous = target("a", 0, "board");
    let changed = target("b", 0, "board");
    let requested = target("c", 0, "board");
    let receiver = target("c", RECEIVER_ID, "board");
    let mut state = FlashState {
        selected_targets: [ota_target_key(&previous), ota_target_key(&changed)].into(),
        tracker_selection: ["tracker:a:0".into(), "tracker:b:0".into()].into(),
        targets: vec![previous.clone(), changed.clone()],
        ..FlashState::default()
    };
    let mut replacement = changed;
    replacement.address = Some("AABBCCDDEEFF".into());
    state.apply_queries(OtaQueries {
        targets: vec![
            previous.clone(),
            replacement,
            requested.clone(),
            receiver.clone(),
        ],
        failures: vec![("lost".into(), "disconnected".into())],
    });
    assert_eq!(state.selected_targets, [ota_target_key(&previous)].into());
    assert!(!state.selected_targets.contains(&ota_target_key(&receiver)));
    assert_eq!(state.query_failures.len(), 1);
}

pub(super) fn uf2_volume(
    board: &str,
) -> (tempfile::TempDir, crate::features::firmware::uf2::Uf2Volume) {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("INFO_UF2.TXT"),
        format!("Board-ID: {board}\n"),
    )
    .unwrap();
    let volume =
        crate::features::firmware::uf2::inspect_volume(directory.path(), "INFO_UF2.TXT").unwrap();
    (directory, volume)
}

pub(super) fn uf2_catalog() -> crate::features::firmware::board_catalog::Catalog {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("slimenrf-helper.yaml");
    std::fs::write(
        &config,
        br#"{"schema_version":1,"firmware_root":"missing","board_targets":{
            "known":{"bootloader":"maintenance.uf2","bootloader_mcu":"nrf52840","targets":[]},
            "other":{"bootloader":"maintenance.uf2","bootloader_mcu":"nrf52833","targets":[]},
            "missing-mcu":{"bootloader":"maintenance.uf2","targets":[]}
        }}"#,
    )
    .unwrap();
    crate::features::firmware::board_catalog::Catalog::load(&config).unwrap()
}

pub(super) fn uf2_test_image(bootloader: bool) -> crate::application::flash::Image {
    let flash = 0x10_0000u32;
    let addresses = if bootloader {
        vec![flash - 0xc000, flash - 0x2800, 0x1000_1000]
    } else {
        vec![0x1000]
    };
    let mut bytes = vec![0; addresses.len() * 512];
    for (number, address) in addresses.iter().enumerate() {
        let block = &mut bytes[number * 512..(number + 1) * 512];
        for (offset, value) in [
            (0, 0x0a32_4655u32),
            (4, 0x9e5d_5157),
            (8, 0x2000),
            (12, *address),
            (16, 256),
            (20, number as u32),
            (24, addresses.len() as u32),
            (28, if bootloader { 0xd663_823c } else { 0xada5_2840 }),
            (508, 0x0ab1_6f30),
        ] {
            block[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
    }
    if bootloader {
        let cf2 = [
            0x1e9e_10f1u32,
            0x2022_7a79,
            3,
            100,
            204,
            flash,
            205,
            0x4_0000,
            209,
            0xada5_2840,
        ];
        for (index, value) in cf2.iter().enumerate() {
            let offset = 512 + 32 + index * 4;
            bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        for (offset, value) in [(0x14, flash - 0xc000), (0x18, flash - 0x2000)] {
            let offset = 1024 + 32 + offset;
            bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
    }
    crate::application::flash::make_image(
        "maintenance.uf2".into(),
        bytes,
        "local".into(),
        "frozen-digest".into(),
        true,
    )
}

#[cfg(not(target_os = "linux"))]
fn copied_uf2_catalog() -> crate::features::firmware::board_catalog::Catalog {
    use crate::features::firmware::{
        board_catalog::{EsbOtaKind, FlashTarget},
        bundled_resources::Resource,
    };

    let mut catalog = uf2_catalog();
    for (board, bootloader_board, firmware) in [
        ("known", "known_board", "known-app.uf2"),
        ("other", "other_board", "other-app.uf2"),
    ] {
        let preset = catalog.board_targets.get_mut(board).unwrap();
        preset.bootloader_board = Some(bootloader_board.into());
        preset.targets.push(FlashTarget {
            name: Some(board.into()),
            firmware: Some(Resource::File(firmware.into())),
            esb_ota_kind: Some(EsbOtaKind::Tracker),
            ..FlashTarget::default()
        });
    }
    let mut peer = catalog.board_targets["known"].clone();
    peer.key = "peer".into();
    peer.display_name = "peer".into();
    peer.bootloader_board = Some("peer_board".into());
    peer.targets[0].name = Some("peer".into());
    peer.targets[0].firmware = Some(Resource::File("peer-app.uf2".into()));
    catalog.board_targets.insert("peer".into(), peer);
    catalog
}

#[cfg(not(target_os = "linux"))]
fn copied_uf2_image(
    catalog: &crate::features::firmware::board_catalog::Catalog,
    name: &str,
    bootloader: bool,
    local: bool,
) -> crate::application::flash::Image {
    let mut image = uf2_test_image(bootloader);
    image.name = name.into();
    image.hints = catalog.match_filename(name).into();
    image.local = local;
    image
}

#[cfg(not(target_os = "linux"))]
#[test]
fn copied_uf2_names_freeze_purpose_volume_path_and_shared_payload() {
    use crate::application::flash::{Action, ImageCategory, planning::plan_uf2_action};
    use crate::features::firmware::executor::{
        CompatibilityApproval, ExecutionRequest, UpdateRequest,
    };

    let catalog = copied_uf2_catalog();
    for (name, bootloader, expected) in [
        ("out/known-app (2).uf2", false, Uf2ImagePurpose::Application),
        ("known-app - Copy.uf2", false, Uf2ImagePurpose::Application),
        ("known-app 副本.uf2", false, Uf2ImagePurpose::Application),
        (
            "known-app（２） [3] copy 2.uf2",
            false,
            Uf2ImagePurpose::Application,
        ),
        (
            "out/update-known_board_bootloader-0.11.1_nosd (2).uf2",
            true,
            Uf2ImagePurpose::Bootloader(Nrf52Chip::Nrf52840),
        ),
        (
            "maintenance - Copy.uf2",
            true,
            Uf2ImagePurpose::Bootloader(Nrf52Chip::Nrf52840),
        ),
    ] {
        let mut image = copied_uf2_image(&catalog, name, bootloader, false);
        assert!(
            image
                .hints
                .iter()
                .any(|hint| hint.board_target == "known" && hint.bootloader == bootloader),
            "catalog must identify {name}"
        );
        let provenance = format!("CI provenance | member {name}");
        image.provenance = provenance.clone();
        let bytes = Arc::clone(&image.bytes);
        let (_directory, volume) = uf2_volume("known");
        let identity = volume.identity.clone();
        let mount = volume.mount.clone();
        let action = plan_uf2_action(
            Some(&catalog),
            if bootloader {
                ImageCategory::Bootloader
            } else {
                ImageCategory::Application
            },
            vec![volume],
            image,
            crate::application::flash::ConfirmationPolicy::Standard,
        )
        .unwrap_or_else(|error| panic!("{name}: {error}"));
        let Action::Uf2(_, reviewed_image, purpose) = &action else {
            panic!("expected UF2 action for {name}");
        };
        assert_eq!(*purpose, expected, "{name}");
        assert_eq!(reviewed_image.name, name);
        assert_eq!(reviewed_image.provenance, provenance);
        assert!(Arc::ptr_eq(&reviewed_image.bytes, &bytes));

        let mut review = FlashState {
            review: Some(action),
            ..FlashState::default()
        };
        assert!(
            review.take_confirmed_action().is_none(),
            "standard review must be acknowledged before executing {name}"
        );
        review.acknowledged = true;
        let action = review
            .take_confirmed_action()
            .expect("acknowledged review must admit the frozen UF2 action");

        let ExecutionRequest::Blocking(UpdateRequest::Uf2(volumes, image, purpose)) =
            action.into_request()
        else {
            panic!("expected frozen UF2 request for {name}");
        };
        assert_eq!(purpose, expected, "{name}");
        assert_eq!(volumes.len(), 1);
        assert_eq!(volumes[0].identity, identity);
        assert_eq!(volumes[0].mount, mount);
        assert_eq!(image.name, name);
        assert!(Arc::ptr_eq(&image.bytes, &bytes));
        assert_eq!(image.approval, Some(CompatibilityApproval::Standard));

        if bootloader && name.starts_with("out/") {
            use crate::features::firmware::executor::{ResultIdentity, UpdateCompletion, execute};
            use crate::update::UpdateOutcome;
            use std::sync::atomic::AtomicBool;

            let completion = execute(
                UpdateRequest::Uf2(volumes, image, purpose),
                &AtomicBool::new(false),
                &mut |_| {},
            )
            .unwrap();
            let UpdateCompletion::Results(mut results) = completion else {
                panic!("expected UF2 write results for {name}");
            };
            assert_eq!(results.len(), 1);
            let (ResultIdentity::Uf2(written_volume), outcome) = results.pop().unwrap() else {
                panic!("expected the frozen UF2 volume for {name}");
            };
            assert_eq!(written_volume.identity, identity);
            assert!(matches!(outcome, UpdateOutcome::Completed));
            assert_eq!(
                std::fs::read(mount.join("update-known_board_bootloader-0.11.1_nosd (2).uf2"))
                    .unwrap()
                    .as_slice(),
                bytes.as_ref()
            );
            assert!(!mount.join("out").exists());
        }
    }
}

#[cfg(not(target_os = "linux"))]
#[test]
fn copied_known_uf2_names_still_reject_same_mcu_wrong_board() {
    use crate::application::flash::{ImageCategory, planning::plan_uf2_action};

    let catalog = copied_uf2_catalog();
    for (name, bootloader, error_key) in [
        (
            "peer-app - Copy.uf2",
            false,
            "firmware.error.board_role_mismatch",
        ),
        (
            "update-peer_board_bootloader-0.11.1_nosd [2].uf2",
            true,
            "firmware.error.bootloader_board_mismatch",
        ),
    ] {
        for local in [true, false] {
            let image = copied_uf2_image(&catalog, name, bootloader, local);
            assert!(
                image.hints.iter().any(|hint| hint.board_target == "peer"),
                "catalog must identify the conflicting board for {name}"
            );
            let (_directory, volume) = uf2_volume("known");
            let error = plan_uf2_action(
                Some(&catalog),
                if bootloader {
                    ImageCategory::Bootloader
                } else {
                    ImageCategory::Application
                },
                vec![volume],
                image,
                crate::application::flash::ConfirmationPolicy::Standard,
            )
            .err()
            .expect("a copied filename must not bypass a known board conflict");
            assert_eq!(error, crate::i18n::tr(error_key), "{name}, local={local}");
        }
    }
}

#[cfg(not(target_os = "linux"))]
#[test]
fn copied_uf2_names_do_not_bypass_content_or_mcu_validation() {
    use crate::application::flash::{ImageCategory, planning::plan_uf2_action};

    let catalog = copied_uf2_catalog();
    for (name, category, payload_is_bootloader, board, diagnostic) in [
        (
            "known-app - Copy.uf2",
            ImageCategory::Application,
            true,
            "known",
            "not a supported nRF52 application family",
        ),
        (
            "update-known_board_bootloader-0.11.1_nosd (2).uf2",
            ImageCategory::Bootloader,
            false,
            "known",
            "not an application image",
        ),
        (
            "update-other_board_bootloader-0.11.1_nosd (2).uf2",
            ImageCategory::Bootloader,
            true,
            "other",
            "outside the Nrf52833 bootloader/MBR layout",
        ),
    ] {
        let image = copied_uf2_image(&catalog, name, payload_is_bootloader, false);
        assert!(!image.hints.is_empty(), "catalog must identify {name}");
        let (_directory, volume) = uf2_volume(board);
        let error = plan_uf2_action(
            Some(&catalog),
            category,
            vec![volume],
            image,
            crate::application::flash::ConfirmationPolicy::Standard,
        )
        .err()
        .expect("filename recognition must not authorize incompatible content");
        assert!(error.contains(diagnostic), "{name}: {error}");
    }
}

#[cfg(not(target_os = "linux"))]
#[test]
fn copied_unknown_remote_profiles_and_bootloaders_remain_blocked() {
    use crate::application::flash::{ImageCategory, planning::plan_uf2_action};

    let catalog = copied_uf2_catalog();
    for (name, category, bootloader, error_key) in [
        (
            "known-app-Pro (2).uf2",
            ImageCategory::Application,
            false,
            "firmware.error.unknown_remote_target",
        ),
        (
            "update-known_board_bootloader-0.11.1_s140 - Copy.uf2",
            ImageCategory::Bootloader,
            true,
            "firmware.error.bootloader_board_mismatch",
        ),
        (
            "custom_bootloader (2).uf2",
            ImageCategory::Bootloader,
            true,
            "firmware.error.bootloader_board_mismatch",
        ),
    ] {
        let image = copied_uf2_image(&catalog, name, bootloader, false);
        assert!(image.hints.is_empty(), "{name} must stay unidentified");
        let (_directory, volume) = uf2_volume("known");
        let error = plan_uf2_action(
            Some(&catalog),
            category,
            vec![volume],
            image,
            crate::application::flash::ConfirmationPolicy::Standard,
        )
        .err()
        .expect("unknown remote firmware must not be admitted");
        assert_eq!(error, crate::i18n::tr(error_key), "{name}");
    }

    let name = "known-app_DataCollect_HID (2).uf2";
    let image = copied_uf2_image(&catalog, name, false, false);
    assert!(!image.hints.is_empty(), "catalog must identify {name}");
    let (_directory, volume) = uf2_volume("known");
    let error = plan_uf2_action(
        Some(&catalog),
        ImageCategory::Application,
        vec![volume],
        image,
        crate::application::flash::ConfirmationPolicy::Standard,
    )
    .err()
    .expect("UF2 must reject the unsupported DataCollect HID profile");
    assert_eq!(
        error,
        crate::i18n::tr("firmware.error.image_method_mismatch")
    );
}

#[test]
fn unhinted_local_bootloader_still_requires_known_volume_preset_and_mcu() {
    use crate::application::flash::{ImageCategory, planning::plan_uf2_action};
    let catalog = uf2_catalog();
    let mut image = uf2_test_image(true);
    image.name = "custom_bootloader.uf2".into();
    image.hints = catalog.match_filename(&image.name).into();
    assert!(image.hints.is_empty());
    for board in ["unknown", "missing-mcu"] {
        let (_directory, volume) = uf2_volume(board);
        let error = plan_uf2_action(
            Some(&catalog),
            ImageCategory::Bootloader,
            vec![volume],
            image.clone(),
            crate::application::flash::ConfirmationPolicy::Standard,
        )
        .err()
        .expect("unidentified bootloader MCU must not be admitted");
        assert_eq!(
            error,
            crate::i18n::tr("firmware.error.bootloader_identity_required"),
            "volume board {board}"
        );
    }
    let (_directory, volume) = uf2_volume("known");
    let error = plan_uf2_action(
        None,
        ImageCategory::Bootloader,
        vec![volume],
        image,
        crate::application::flash::ConfirmationPolicy::Standard,
    )
    .err()
    .expect("bootloader authorization requires a catalog preset");
    assert_eq!(
        error,
        crate::i18n::tr("firmware.error.bootloader_identity_required")
    );
}

#[test]
fn local_bootloader_with_known_hints_still_rejects_board_mismatch() {
    use crate::application::flash::{ImageCategory, planning::plan_uf2_action};
    let catalog = uf2_catalog();
    let mut image = uf2_test_image(true);
    let mut hints = catalog.match_filename(&image.name);
    assert!(!hints.is_empty());
    for hint in &mut hints {
        hint.board_target = "unrelated".into();
    }
    image.hints = hints.into();
    let (_directory, volume) = uf2_volume("known");
    let error = plan_uf2_action(
        Some(&catalog),
        ImageCategory::Bootloader,
        vec![volume],
        image,
        crate::application::flash::ConfirmationPolicy::Standard,
    )
    .err()
    .expect("known conflicting bootloader hints must not be bypassed");
    assert_eq!(
        error,
        crate::i18n::tr("firmware.error.bootloader_board_mismatch")
    );
}

#[test]
fn unhinted_remote_bootloader_is_not_admitted_for_known_volume() {
    use crate::application::flash::{ImageCategory, planning::plan_uf2_action};
    let catalog = uf2_catalog();
    let mut image = uf2_test_image(true);
    image.name = "custom_bootloader.uf2".into();
    image.hints = catalog.match_filename(&image.name).into();
    image.local = false;
    assert!(image.hints.is_empty());
    let (_directory, volume) = uf2_volume("known");
    let error = plan_uf2_action(
        Some(&catalog),
        ImageCategory::Bootloader,
        vec![volume],
        image,
        crate::application::flash::ConfirmationPolicy::Standard,
    )
    .err()
    .expect("remote bootloaders require a known board hint");
    assert_eq!(
        error,
        crate::i18n::tr("firmware.error.bootloader_board_mismatch")
    );
}

#[test]
fn shared_bootloader_image_cannot_mix_configured_chips() {
    use crate::application::flash::{ImageCategory, planning::plan_uf2_action};
    let catalog = uf2_catalog();
    let mut image = uf2_test_image(true);
    image.name = "custom_bootloader.uf2".into();
    image.hints = catalog.match_filename(&image.name).into();
    assert!(image.hints.is_empty());
    let (_known_directory, known) = uf2_volume("known");
    let (_other_directory, other) = uf2_volume("other");
    let error = plan_uf2_action(
        Some(&catalog),
        ImageCategory::Bootloader,
        vec![known, other],
        image,
        crate::application::flash::ConfirmationPolicy::Standard,
    )
    .err()
    .expect("one frozen bootloader purpose cannot cover different MCUs");
    assert_eq!(
        error,
        crate::i18n::tr("firmware.error.bootloader_mcu_mismatch")
    );
}

#[test]
fn catalog_classified_nonstandard_bootloader_freezes_purpose_and_shared_bytes_into_request() {
    use crate::application::flash::{Action, ImageCategory, planning::plan_uf2_action};
    use crate::features::firmware::executor::{ExecutionRequest, UpdateRequest};
    let catalog = uf2_catalog();
    let mut image = uf2_test_image(true);
    image.hints = catalog.match_filename(&image.name).into();
    let bytes = image.bytes.clone();
    let (_directory, volume) = uf2_volume("known");
    let identity = volume.identity.clone();
    let expected = Uf2ImagePurpose::Bootloader(Nrf52Chip::Nrf52840);
    let action = plan_uf2_action(
        Some(&catalog),
        ImageCategory::Bootloader,
        vec![volume],
        image,
        crate::application::flash::ConfirmationPolicy::Standard,
    )
    .unwrap();
    assert!(matches!(&action, Action::Uf2(_, _, purpose) if *purpose == expected));
    let ExecutionRequest::Blocking(UpdateRequest::Uf2(volumes, image, purpose)) =
        action.into_request()
    else {
        panic!("expected a frozen UF2 request");
    };
    assert!(purpose == expected);
    assert_eq!(volumes[0].identity, identity);
    assert!(Arc::ptr_eq(&bytes, &image.bytes));
}

#[test]
fn unhinted_local_bootloader_freezes_purpose_shared_bytes_and_volume_identity_into_request() {
    use crate::application::flash::{Action, ImageCategory, planning::plan_uf2_action};
    use crate::features::firmware::executor::{ExecutionRequest, UpdateRequest};
    let catalog = uf2_catalog();
    let mut image = uf2_test_image(true);
    image.name = "custom_bootloader.uf2".into();
    image.hints = catalog.match_filename(&image.name).into();
    assert!(image.local);
    assert!(image.hints.is_empty());
    let bytes = image.bytes.clone();
    let (_directory, volume) = uf2_volume("known");
    let identity = volume.identity.clone();
    let mount = volume.mount.clone();
    let expected = Uf2ImagePurpose::Bootloader(Nrf52Chip::Nrf52840);
    let action = plan_uf2_action(
        Some(&catalog),
        ImageCategory::Bootloader,
        vec![volume],
        image,
        crate::application::flash::ConfirmationPolicy::Standard,
    )
    .unwrap();
    assert!(matches!(&action, Action::Uf2(_, _, purpose) if *purpose == expected));
    let ExecutionRequest::Blocking(UpdateRequest::Uf2(volumes, image, purpose)) =
        action.into_request()
    else {
        panic!("expected the reviewed local bootloader's frozen UF2 request");
    };
    assert!(purpose == expected);
    assert_eq!(volumes.len(), 1);
    assert_eq!(volumes[0].identity, identity);
    assert_eq!(volumes[0].mount, mount);
    assert_eq!(image.name, "custom_bootloader.uf2");
    assert!(Arc::ptr_eq(&bytes, &image.bytes));
}

#[test]
fn unhinted_local_bootloader_still_validates_payload_against_configured_mcu() {
    use crate::application::flash::{ImageCategory, planning::plan_uf2_action};
    let catalog = uf2_catalog();
    let mut image = uf2_test_image(true);
    image.name = "custom_bootloader.uf2".into();
    image.hints = catalog.match_filename(&image.name).into();
    assert!(image.hints.is_empty());
    let (_directory, volume) = uf2_volume("other");
    let error = plan_uf2_action(
        Some(&catalog),
        ImageCategory::Bootloader,
        vec![volume],
        image,
        crate::application::flash::ConfirmationPolicy::Standard,
    )
    .err()
    .expect("nRF52840 content must not be authorized by an nRF52833 preset");
    assert!(
        error.contains("outside the Nrf52833 bootloader/MBR layout"),
        "{error}"
    );
}

#[test]
fn unhinted_local_bootloader_review_still_rejects_application_payload() {
    use crate::application::flash::{ImageCategory, planning::plan_uf2_action};
    let catalog = uf2_catalog();
    let mut image = uf2_test_image(false);
    image.name = "custom_bootloader.uf2".into();
    image.hints = catalog.match_filename(&image.name).into();
    assert!(image.hints.is_empty());
    let (_directory, volume) = uf2_volume("known");
    let error = plan_uf2_action(
        Some(&catalog),
        ImageCategory::Bootloader,
        vec![volume],
        image,
        crate::application::flash::ConfirmationPolicy::Standard,
    )
    .err()
    .expect("an unknown local filename cannot turn application bytes into a bootloader");
    assert!(error.contains("not an application image"), "{error}");
}

#[test]
fn reviewed_local_application_keeps_unknown_board_bypass_but_validates_every_volume() {
    use crate::application::flash::{Action, ImageCategory, planning::plan_uf2_action};
    let catalog = uf2_catalog();
    let image = uf2_test_image(false);
    let (_directory, volume) = uf2_volume("unknown");
    let action = plan_uf2_action(
        Some(&catalog),
        ImageCategory::Application,
        vec![volume.clone()],
        image.clone(),
        crate::application::flash::ConfirmationPolicy::Standard,
    )
    .unwrap();
    assert!(matches!(
        action,
        Action::Uf2(_, _, Uf2ImagePurpose::Application)
    ));
    let mut hinted = image.clone();
    hinted.hints = vec![crate::features::firmware::board_catalog::FirmwareHint {
        board_target: "another-board".into(),
        kind: None,
        variant: "Standard".into(),
        mcuboot: false,
        bootloader: false,
    }]
    .into();
    assert!(
        plan_uf2_action(
            Some(&catalog),
            ImageCategory::Application,
            vec![volume.clone()],
            hinted,
            crate::application::flash::ConfirmationPolicy::Standard,
        )
        .is_ok()
    );
    let mut remote = image.clone();
    remote.local = false;
    assert!(
        plan_uf2_action(
            Some(&catalog),
            ImageCategory::Application,
            vec![volume.clone()],
            remote,
            crate::application::flash::ConfirmationPolicy::Standard,
        )
        .is_err()
    );
    let (_other_directory, other) = uf2_volume("nrf52833-test");
    assert!(
        plan_uf2_action(
            Some(&catalog),
            ImageCategory::Application,
            vec![volume, other],
            image,
            crate::application::flash::ConfirmationPolicy::Standard,
        )
        .is_err()
    );
}

#[test]
fn optional_policy_distinguishes_matched_unknown_and_conflicting_uf2_without_authorizing_io() {
    use crate::application::flash::{
        Compatibility, ConfirmationPolicy, ImageCategory,
        planning::{action_compatibility, plan_uf2_action},
    };
    use crate::features::firmware::{
        board_catalog::{EsbOtaKind, FirmwareHint, FlashTarget},
        executor::{self, ExecutionRequest},
    };
    use std::sync::atomic::AtomicBool;

    let mut catalog = uf2_catalog();
    catalog
        .board_targets
        .get_mut("known")
        .unwrap()
        .targets
        .push(FlashTarget {
            name: Some("known".into()),
            esb_ota_kind: Some(EsbOtaKind::Tracker),
            ..FlashTarget::default()
        });
    let (_directory, volume) = uf2_volume("known");
    let mut image = uf2_test_image(false);
    image.name = "known.uf2".into();
    image.hints = vec![FirmwareHint {
        board_target: "known".into(),
        kind: Some(EsbOtaKind::Tracker),
        variant: "Standard".into(),
        mcuboot: false,
        bootloader: false,
    }]
    .into();

    let matched = plan_uf2_action(
        Some(&catalog),
        ImageCategory::Application,
        vec![volume.clone()],
        image.clone(),
        ConfirmationPolicy::Minimal,
    )
    .unwrap();
    assert_eq!(
        action_compatibility(&matched, Some(&catalog)),
        Compatibility::Matched
    );

    let mut wrong = image.clone();
    Arc::make_mut(&mut wrong.hints)[0].board_target = "other".into();
    assert!(
        plan_uf2_action(
            Some(&catalog),
            ImageCategory::Application,
            vec![volume.clone()],
            wrong.clone(),
            ConfirmationPolicy::Standard,
        )
        .is_err()
    );
    let conflicting = plan_uf2_action(
        Some(&catalog),
        ImageCategory::Application,
        vec![volume.clone()],
        wrong,
        ConfirmationPolicy::Minimal,
    )
    .unwrap();
    assert_eq!(
        action_compatibility(&conflicting, Some(&catalog)),
        Compatibility::Mismatch
    );
    let ExecutionRequest::Blocking(request) = conflicting.into_request() else {
        panic!("expected a UF2 request");
    };
    let error = executor::execute(request, &AtomicBool::new(false), &mut |_| {})
        .err()
        .expect("an unacknowledged request must stop before device IO");
    assert!(error.contains("has not been confirmed"));

    image.local = false;
    image.hints = Arc::from([]);
    assert!(
        plan_uf2_action(
            Some(&catalog),
            ImageCategory::Application,
            vec![volume.clone()],
            image.clone(),
            ConfirmationPolicy::Standard,
        )
        .is_err()
    );
    let unknown = plan_uf2_action(
        Some(&catalog),
        ImageCategory::Application,
        vec![volume],
        image.clone(),
        ConfirmationPolicy::Minimal,
    )
    .unwrap();
    assert_eq!(
        action_compatibility(&unknown, Some(&catalog)),
        Compatibility::Unknown
    );

    let (_other_directory, other) = uf2_volume("other");
    assert!(
        plan_uf2_action(
            Some(&catalog),
            ImageCategory::Application,
            vec![other],
            image,
            ConfirmationPolicy::Minimal,
        )
        .is_err(),
        "metadata consent cannot waive the configured target MCU"
    );
}

#[test]
fn optional_ota_policy_keeps_content_and_physical_guards() {
    use crate::application::flash::{Compatibility, ConfirmationPolicy, planning};
    use crate::features::firmware::board_catalog::{EsbOtaKind, FirmwareHint};

    let mut device = target("a", 0, "board/nrf52840");
    let mut image = local_image("other.hex");
    image.hints = vec![FirmwareHint {
        board_target: "other/nrf52840".into(),
        kind: Some(EsbOtaKind::Receiver),
        variant: "Standard".into(),
        mcuboot: false,
        bootloader: false,
    }]
    .into();
    assert!(planning::validate_ota_image(&image, &device, ConfirmationPolicy::Standard).is_err());
    assert!(planning::validate_ota_image(&image, &device, ConfirmationPolicy::Minimal).is_ok());
    assert_eq!(
        planning::ota_compatibility(&image, &device),
        Compatibility::Mismatch
    );
    device.info.protocol_version = 2;
    assert!(planning::validate_ota_image(&image, &device, ConfirmationPolicy::Minimal).is_err());
    device.info.protocol_version = 1;
    device.info.bootloader = "mcuboot".into();
    assert!(planning::validate_ota_image(&image, &device, ConfirmationPolicy::Minimal).is_err());
    device.info.bootloader = "none".into();
    let mut wrong_family = uf2_test_image(false);
    device.info.board_target = "board/nrf52833".into();
    wrong_family.hints = image.hints.clone();
    assert!(
        planning::validate_ota_image(&wrong_family, &device, ConfirmationPolicy::Minimal).is_err()
    );
    image.local = false;
    image.hints = Arc::from([]);
    assert!(planning::validate_ota_image(&image, &device, ConfirmationPolicy::Standard).is_err());
    assert!(planning::validate_ota_image(&image, &device, ConfirmationPolicy::Minimal).is_ok());
    assert_eq!(
        planning::ota_compatibility(&image, &device),
        Compatibility::Unknown
    );
}

#[test]
fn bootloader_candidates_require_explicit_selection_and_valid_chip_layout() {
    use crate::application::flash::{
        Compatibility, ImageCategory, ImageDestination,
        planning::{action_compatibility, plan_uf2_action},
    };
    use crate::features::firmware::{board_catalog::FirmwareHint, source::BootloaderNamePolicy};

    let catalog = uf2_catalog();
    let (_directory, volume) = uf2_volume("known");
    let mut image = uf2_test_image(true);
    image.name = "unrecognized-application.uf2".into();
    image.local = false;
    image.acquisition_policy = BootloaderNamePolicy::AllowUnclassified;
    let mut state = FlashState {
        category: ImageCategory::Bootloader,
        catalog: Some(catalog.clone()),
        ..FlashState::default()
    };
    state.set_confirmation_policy(true);
    state.apply_images(ImageDestination::Global, vec![image.clone()]);
    assert!(
        state.image.is_none(),
        "an unclassified remote member needs explicit selection"
    );
    assert_eq!(state.images.len(), 1);
    state
        .choose_image(&ImageDestination::Global, image.clone())
        .unwrap();
    assert!(
        plan_uf2_action(
            Some(&catalog),
            ImageCategory::Bootloader,
            vec![volume.clone()],
            image.clone(),
            ConfirmationPolicy::Standard,
        )
        .is_err()
    );
    let action = plan_uf2_action(
        Some(&catalog),
        ImageCategory::Bootloader,
        vec![volume.clone()],
        image,
        ConfirmationPolicy::Minimal,
    )
    .unwrap();
    assert_eq!(
        action_compatibility(&action, Some(&catalog)),
        Compatibility::Unknown
    );
    let mut application = uf2_test_image(false);
    application.name = "unrecognized-application.uf2".into();
    application.local = false;
    application.acquisition_policy = BootloaderNamePolicy::AllowUnclassified;
    assert!(
        plan_uf2_action(
            Some(&catalog),
            ImageCategory::Bootloader,
            vec![volume.clone()],
            application.clone(),
            ConfirmationPolicy::Minimal,
        )
        .is_err(),
        "an application UF2 cannot become a bootloader updater by naming or consent"
    );
    application.name = "looks-like-bootloader.uf2".into();
    application.hints = vec![FirmwareHint {
        board_target: "known".into(),
        kind: None,
        variant: "Bootloader".into(),
        mcuboot: false,
        bootloader: true,
    }]
    .into();
    let reviewed = plan_uf2_action(
        Some(&catalog),
        ImageCategory::Application,
        vec![volume],
        application,
        ConfirmationPolicy::Minimal,
    )
    .expect("valid application content can enter risk review despite a conflicting name");
    assert_eq!(
        action_compatibility(&reviewed, Some(&catalog)),
        Compatibility::Mismatch
    );
}

#[test]
fn multi_hint_bootloader_is_matched_only_when_every_hint_belongs_to_the_selected_preset() {
    use crate::application::flash::{
        Compatibility, ImageCategory,
        planning::{action_compatibility, plan_uf2_action},
    };
    use crate::features::firmware::board_catalog::{FirmwareHint, FlashTarget};
    let mut catalog = uf2_catalog();
    let preset = catalog.board_targets.get_mut("known").unwrap();
    preset.targets = ["known-a", "known-b"]
        .into_iter()
        .map(|name| FlashTarget {
            name: Some(name.into()),
            ..FlashTarget::default()
        })
        .collect();
    let mut image = uf2_test_image(true);
    image.hints = ["known-a", "known-b"]
        .into_iter()
        .map(|board| FirmwareHint {
            board_target: board.into(),
            kind: None,
            variant: "Bootloader".into(),
            mcuboot: false,
            bootloader: true,
        })
        .collect::<Vec<_>>()
        .into();
    let (_directory, volume) = uf2_volume("known");
    let action = plan_uf2_action(
        Some(&catalog),
        ImageCategory::Bootloader,
        vec![volume],
        image,
        ConfirmationPolicy::Minimal,
    )
    .unwrap();
    assert_eq!(
        action_compatibility(&action, Some(&catalog)),
        Compatibility::Matched
    );
}

#[test]
fn configured_opendfu_matching_uses_the_configured_board_and_receiver_hint() {
    use crate::application::flash::{Action, Compatibility, planning::action_compatibility};
    use crate::domain::{SerialEndpoint, SerialPortKind};
    use crate::features::firmware::{
        board_catalog::{EsbOtaKind, FirmwareHint, OpenDfuTarget},
        bundled_resources::Resource,
    };
    use slimenrf_firmware_image::secure_dfu::Options;

    let resource = Resource::File("custom.hex".into());
    let mut catalog = uf2_catalog();
    catalog.opendfu_targets.push(OpenDfuTarget {
        name: "custom-board".into(),
        firmware: resource.clone(),
        hw_version: 52,
        app_version: 1,
        sd_req: "0x00".into(),
        success: None,
    });
    let port = SerialEndpoint {
        port_name: "synthetic".into(),
        label: "Synthetic OpenDFU".into(),
        manufacturer: None,
        product: Some("Open DFU Bootloader".into()),
        kind: SerialPortKind::Usb,
        serial_number: Some("fixture".into()),
        vid: None,
        pid: None,
        usb_interface: None,
    };
    let mut image = local_image("custom.hex");
    image.resource = Some(Arc::new(resource));
    image.hints = vec![FirmwareHint {
        board_target: "custom-board".into(),
        kind: Some(EsbOtaKind::Receiver),
        variant: "Standard".into(),
        mcuboot: false,
        bootloader: false,
    }]
    .into();
    let action = Action::OpenDfu(port.clone(), image.clone(), Options::default());
    assert_eq!(
        action_compatibility(&action, Some(&catalog)),
        Compatibility::Matched
    );
    Arc::make_mut(&mut image.hints)[0].board_target = "different".into();
    let action = Action::OpenDfu(port, image, Options::default());
    assert_eq!(
        action_compatibility(&action, Some(&catalog)),
        Compatibility::Mismatch
    );
}

#[test]
fn restored_minimal_preference_is_used_by_first_assignment_and_plan() {
    use crate::application::flash::{Action, Compatibility, Mode, planning::action_compatibility};
    use crate::features::firmware::board_catalog::{EsbOtaKind, FirmwareHint};
    let device = target("receiver", 0, "board/a");
    let mut image = local_image("other.hex");
    image.hints = vec![FirmwareHint {
        board_target: "board/b".into(),
        kind: Some(EsbOtaKind::Tracker),
        variant: "Standard".into(),
        mcuboot: false,
        bootloader: false,
    }]
    .into();
    for minimal in [false, true] {
        let mut state = FlashState::from_confirmation_preference(minimal);
        state.mode = Mode::Esb;
        state.targets = vec![device.clone()];
        state.selected_targets.insert(ota_target_key(&device));
        state.reconcile_groups(None);
        let destination = state.destination(Mode::Esb).unwrap();
        let result = state.choose_image(&destination, image.clone());
        assert_eq!(result.is_ok(), minimal);
        if minimal {
            let groups = plan_ota_groups(&state).unwrap();
            assert_eq!(
                action_compatibility(&Action::EsbGroups(groups), None),
                Compatibility::Mismatch,
            );
        }
    }
}
