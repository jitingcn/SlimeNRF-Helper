use super::support::{local_image, target};
use crate::esb_ota::RECEIVER_ID;
use crate::ui::flash::{
    FlashState, OtaQueries, ota_assignment_key, ota_target_key, plan_ota_groups, validate_ota_image,
};
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
    assert!(validate_ota_image(&image, &device).is_ok());
    device.info.bootloader = "mcuboot".into();
    assert!(validate_ota_image(&image, &device).is_err());
    device.info.bootloader = "none".into();
    device.info.protocol_version = 2;
    assert!(validate_ota_image(&image, &device).is_err());
    device.info.protocol_version = 1;
    image.local = false;
    assert!(validate_ota_image(&image, &device).is_err());
    image.local = true;
    image.hints = vec![crate::board_catalog::FirmwareHint {
        board_target: "another-board".into(),
        kind: Some(crate::board_catalog::EsbOtaKind::Tracker),
        variant: "Standard".into(),
        mcuboot: false,
        bootloader: false,
    }]
    .into();
    assert!(validate_ota_image(&image, &device).is_err());
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
        failures: vec![("lost".into(), ("lost".into(), Err("disconnected".into())))],
    });
    assert_eq!(state.selected_targets, [ota_target_key(&previous)].into());
    assert!(!state.selected_targets.contains(&ota_target_key(&receiver)));
    assert_eq!(state.query_failures.len(), 1);
}

fn uf2_volume(board: &str) -> (tempfile::TempDir, crate::uf2::Uf2Volume) {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("INFO_UF2.TXT"),
        format!("Board-ID: {board}\n"),
    )
    .unwrap();
    let volume = crate::uf2::inspect_volume(directory.path(), "INFO_UF2.TXT").unwrap();
    (directory, volume)
}

fn uf2_catalog() -> crate::board_catalog::Catalog {
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
    crate::board_catalog::Catalog::load(&config).unwrap()
}

fn uf2_test_image(bootloader: bool) -> crate::ui::flash::Image {
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
    crate::ui::flash::make_image(
        "maintenance.uf2".into(),
        bytes,
        "local".into(),
        "frozen-digest".into(),
        true,
    )
}

#[test]
fn bootloader_planning_requires_known_preset_mcu_and_matching_hints_even_for_local_images() {
    use crate::ui::flash::{ImageCategory, planning::plan_uf2_action};
    let catalog = uf2_catalog();
    let mut image = uf2_test_image(true);
    image.hints = catalog.match_filename(&image.name).into();
    for board in ["unknown", "missing-mcu"] {
        let (_directory, volume) = uf2_volume(board);
        assert!(
            plan_uf2_action(
                Some(&catalog),
                ImageCategory::Bootloader,
                vec![volume],
                image.clone(),
            )
            .is_err()
        );
    }
    let (_directory, volume) = uf2_volume("known");
    image.hints = Arc::from([]);
    assert!(
        plan_uf2_action(
            Some(&catalog),
            ImageCategory::Bootloader,
            vec![volume.clone()],
            image.clone(),
        )
        .is_err()
    );
    image.hints = catalog.match_filename(&image.name).into();
    let mut wrong = image.hints.to_vec();
    for hint in &mut wrong {
        hint.board_target = "unrelated".into();
    }
    image.hints = wrong.into();
    assert!(
        plan_uf2_action(
            Some(&catalog),
            ImageCategory::Bootloader,
            vec![volume],
            image,
        )
        .is_err()
    );
}

#[test]
fn shared_bootloader_image_cannot_mix_configured_chips() {
    use crate::ui::flash::{ImageCategory, planning::plan_uf2_action};
    let catalog = uf2_catalog();
    let mut image = uf2_test_image(true);
    image.hints = catalog.match_filename(&image.name).into();
    let (_known_directory, known) = uf2_volume("known");
    let (_other_directory, other) = uf2_volume("other");
    assert!(
        plan_uf2_action(
            Some(&catalog),
            ImageCategory::Bootloader,
            vec![known, other],
            image,
        )
        .is_err()
    );
}

#[test]
fn catalog_classified_nonstandard_bootloader_freezes_purpose_and_shared_bytes_into_request() {
    use crate::firmware_update::{ExecutionRequest, UpdateRequest};
    use crate::ui::flash::{Action, ImageCategory, planning::plan_uf2_action};
    let catalog = uf2_catalog();
    let mut image = uf2_test_image(true);
    image.hints = catalog.match_filename(&image.name).into();
    let bytes = image.bytes.clone();
    let (_directory, volume) = uf2_volume("known");
    let identity = volume.identity.clone();
    let expected = crate::uf2::WritePurpose::Bootloader(crate::firmware::Nrf52Chip::Nrf52840);
    let action = plan_uf2_action(
        Some(&catalog),
        ImageCategory::Bootloader,
        vec![volume],
        image,
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
fn bootloader_review_rejects_application_payload_despite_catalog_name_and_local_origin() {
    use crate::ui::flash::{ImageCategory, planning::plan_uf2_action};
    let catalog = uf2_catalog();
    let mut image = uf2_test_image(false);
    image.hints = catalog.match_filename(&image.name).into();
    let (_directory, volume) = uf2_volume("known");
    assert!(
        plan_uf2_action(
            Some(&catalog),
            ImageCategory::Bootloader,
            vec![volume],
            image,
        )
        .is_err()
    );
}

#[test]
fn reviewed_local_application_keeps_unknown_board_bypass_but_validates_every_volume() {
    use crate::ui::flash::{Action, ImageCategory, planning::plan_uf2_action};
    let catalog = uf2_catalog();
    let image = uf2_test_image(false);
    let (_directory, volume) = uf2_volume("unknown");
    let action = plan_uf2_action(
        Some(&catalog),
        ImageCategory::Application,
        vec![volume.clone()],
        image.clone(),
    )
    .unwrap();
    assert!(matches!(
        action,
        Action::Uf2(_, _, crate::uf2::WritePurpose::Application)
    ));
    let mut hinted = image.clone();
    hinted.hints = vec![crate::board_catalog::FirmwareHint {
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
        )
        .is_err()
    );
}
