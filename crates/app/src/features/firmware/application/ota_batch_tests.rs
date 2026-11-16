use super::super::{
    ConfirmationPolicy, FlashState, Image, ImageDestination, Mode, OtaStep, Payload, images,
    make_image, ota_assignment_key, ota_target_key, validate_ota_image,
};
use super::{
    BatchOutcome, MAX_BATCH_FILE_BYTES, MAX_BATCH_FILES, OtaBatchPrepared, distinct_batch_paths,
    prepare_ota_batch,
};
use crate::domain::ReceiverEndpoint;
use crate::features::firmware::{
    board_catalog,
    bundled_resources::Resource,
    esb_ota::{FirmwareInfo, RECEIVER_ID, Target},
};
use crate::i18n::tr;
use std::{
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
};

const HEX: &[u8] = b":0410000001020304E2\n:00000001FF\n";
const CONFIG: &[u8] = br#"{
  "schema_version": 1, "firmware_root": ".", "board_targets": {
    "a": {"bootloader_mcu":"nrf52833", "targets":[{"name":"board-a", "firmware":"a.hex", "esb_ota_firmware":"a.uf2", "esb_ota_mcuboot_firmware":"a.bin", "esb_ota_kind":"tracker"}]},
    "b": {"targets":[{"name":"board-b", "firmware":"b.hex", "esb_ota_mcuboot_firmware":"b.bin", "esb_ota_kind":"tracker"}]},
    "c": {"targets":[{"name":"board-c", "firmware":"c.hex", "esb_ota_kind":"receiver"}]},
    "d": {"targets":[{"name":"board-d", "firmware":"d.hex", "esb_ota_kind":"tracker"}]},
    "e": {"targets":[{"name":"board-e", "firmware":"e.hex", "esb_ota_kind":"tracker"}]}
  }
}"#;

fn target(receiver: &str, id: u8, board: &str) -> Target {
    Target {
        receiver: ReceiverEndpoint {
            key: receiver.into(),
            path: receiver.as_bytes().to_vec(),
            label: receiver.into(),
            serial_number: None,
        },
        id,
        address: (id != RECEIVER_ID).then(|| format!("1122334455{id:02X}")),
        info: FirmwareInfo {
            version: "1".into(),
            build_date: "today".into(),
            firmware_size: 4,
            bootloader: "none".into(),
            protocol_version: 1,
            board_target: board.into(),
            flash_base: 0x1000,
        },
    }
}

fn setup() -> (tempfile::TempDir, FlashState) {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.yaml");
    std::fs::write(&config, CONFIG).unwrap();
    let catalog = board_catalog::Catalog::load(&config).unwrap();
    let targets = vec![
        target("r", 0, "board-a"),
        target("r", 1, "board-b"),
        target("r", RECEIVER_ID, "board-c"),
        target("r", 2, "board-e"),
    ];
    let mut state = FlashState {
        mode: Mode::Esb,
        catalog: Some(catalog),
        selected_targets: targets.iter().map(ota_target_key).collect(),
        targets,
        ..FlashState::default()
    };
    state.reconcile_groups(None);
    (dir, state)
}

fn file(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

fn old(name: &str) -> Image {
    make_image(
        name.into(),
        HEX.to_vec(),
        "previous".into(),
        name.into(),
        true,
    )
}

fn prepare(state: &mut FlashState, paths: Vec<PathBuf>) -> OtaBatchPrepared {
    let mut groups = state.ota_batch_scope();
    state.begin_ota_batch(&mut groups);
    let paths = distinct_batch_paths(paths).unwrap();
    prepare_ota_batch(
        paths,
        groups,
        state.catalog.clone(),
        &AtomicBool::new(false),
    )
    .unwrap()
}

fn outcomes<'a>(state: &'a FlashState, path: &Path) -> &'a [BatchOutcome] {
    &state
        .ota_batch_result
        .as_ref()
        .unwrap()
        .entries
        .iter()
        .find(|entry| entry.path == path)
        .unwrap()
        .outcomes
}

#[test]
fn mixed_boards_and_receiver_replace_existing_sources_without_touching_unmatched() {
    let (dir, mut state) = setup();
    let paths: Vec<_> = ["a.hex", "b.hex", "c.hex", "d.hex", "unknown.hex"]
        .into_iter()
        .map(|name| file(dir.path(), name, HEX))
        .collect();
    let a = ota_assignment_key(&state.targets[0]);
    let b = ota_assignment_key(&state.targets[1]);
    let c = ota_assignment_key(&state.targets[2]);
    let e = ota_assignment_key(&state.targets[3]);
    state.assignments.insert(a.clone(), old("manual.hex"));
    state.assignments.insert(b.clone(), old("bundled.hex"));
    let unchanged = old("unchanged.hex");
    state.assignments.insert(e.clone(), unchanged.clone());
    let before_step = state.ota_step;
    let prepared = prepare(&mut state, paths.clone());
    state.apply_ota_batch(prepared);
    for (key, path) in [(&a, &paths[0]), (&b, &paths[1]), (&c, &paths[2])] {
        let image = &state.assignments[key];
        assert_eq!(
            image.resource.as_deref(),
            Some(&Resource::File(path.clone()))
        );
        assert_eq!(image.provenance, images::local_image_provenance(path));
        assert!(!image.digest.is_empty());
    }
    assert!(Arc::ptr_eq(&state.assignments[&e].bytes, &unchanged.bytes));
    assert!(matches!(
        outcomes(&state, &paths[0]),
        [BatchOutcome::Replaced(_)]
    ));
    assert!(matches!(
        outcomes(&state, &paths[2]),
        [BatchOutcome::Assigned(_)]
    ));
    assert!(matches!(
        outcomes(&state, &paths[3]),
        [BatchOutcome::NoMatch]
    ));
    assert!(matches!(
        outcomes(&state, &paths[4]),
        [BatchOutcome::Unknown]
    ));
    assert_eq!(state.ota_step, before_step);
    assert!(state.review.is_none() && state.pending.is_none() && state.leases.is_empty());
}

#[test]
fn duplicate_paths_collapse_but_distinct_matching_paths_conflict_in_either_order() {
    let (dir, mut state) = setup();
    let a = file(dir.path(), "a.hex", HEX);
    let copy = file(dir.path(), "a (2).hex", HEX);
    let b = file(dir.path(), "b.hex", HEX);
    let key = ota_assignment_key(&state.targets[0]);
    let original = old("original.hex");
    state.assignments.insert(key.clone(), original.clone());
    let dedup = distinct_batch_paths(vec![a.clone(), a.clone(), b.clone()]).unwrap();
    assert_eq!(dedup, vec![a.clone(), b.clone()]);
    let first = prepare(&mut state, vec![a.clone(), a.clone(), b.clone()]);
    state.apply_ota_batch(first);
    assert_eq!(
        state.assignments[&key].resource.as_deref(),
        Some(&Resource::File(a.clone()))
    );
    for paths in [
        vec![a.clone(), copy.clone(), b.clone()],
        vec![copy.clone(), a.clone(), b.clone()],
    ] {
        state.assignments.insert(key.clone(), original.clone());
        let prepared = prepare(&mut state, paths);
        state.apply_ota_batch(prepared);
        assert!(Arc::ptr_eq(&state.assignments[&key].bytes, &original.bytes));
        assert!(matches!(outcomes(&state, &a), [BatchOutcome::Conflict(_)]));
        assert!(matches!(
            outcomes(&state, &copy),
            [BatchOutcome::Conflict(_)]
        ));
        assert!(matches!(outcomes(&state, &b), [BatchOutcome::Replaced(_)]));
    }
}

#[test]
fn case_aliases_make_a_file_ambiguous_in_either_input_order() {
    for reversed in [false, true] {
        let (dir, mut state) = setup();
        let alias = target("other", 3, "BOARD-A");
        state.selected_targets.insert(ota_target_key(&alias));
        state.targets.push(alias.clone());
        state.reconcile_groups(None);
        let original_key = ota_assignment_key(&state.targets[0]);
        let alias_key = ota_assignment_key(&alias);
        let original = old("original.hex");
        let prior_alias = old("alias.hex");
        state
            .assignments
            .insert(original_key.clone(), original.clone());
        state
            .assignments
            .insert(alias_key.clone(), prior_alias.clone());
        let a = file(dir.path(), "a.hex", HEX);
        let copy = file(dir.path(), "a (2).hex", HEX);
        let b = file(dir.path(), "b.hex", HEX);
        let paths = if reversed {
            vec![b.clone(), copy.clone(), a.clone()]
        } else {
            vec![a.clone(), copy.clone(), b.clone()]
        };
        let prepared = prepare(&mut state, paths);
        state.apply_ota_batch(prepared);
        assert!(Arc::ptr_eq(
            &state.assignments[&original_key].bytes,
            &original.bytes
        ));
        assert!(Arc::ptr_eq(
            &state.assignments[&alias_key].bytes,
            &prior_alias.bytes
        ));
        for path in [&a, &copy] {
            let [BatchOutcome::Ambiguous(groups)] = outcomes(&state, path) else {
                panic!("a file matching two distinct groups must remain ambiguous");
            };
            assert_eq!(groups.len(), 2);
            assert!(groups.iter().any(|group| group.board == "board-a"));
            assert!(groups.iter().any(|group| group.board == "BOARD-A"));
        }
        let b_key = ota_assignment_key(&state.targets[1]);
        assert_eq!(
            state.assignments[&b_key].resource.as_deref(),
            Some(&Resource::File(b.clone()))
        );
        assert!(matches!(outcomes(&state, &b), [BatchOutcome::Assigned(_)]));
    }
}

#[test]
fn ambiguous_files_do_not_create_conflicts_for_unique_candidates() {
    for reversed in [false, true] {
        let (dir, mut state) = setup();
        let alias = target("other", 3, "BOARD-A");
        let alias_key = ota_assignment_key(&alias);
        state.selected_targets.insert(ota_target_key(&alias));
        state.targets.push(alias);
        state.reconcile_groups(None);
        let original_key = ota_assignment_key(&state.targets[0]);
        let previous_alias = old("alias.hex");
        state
            .assignments
            .insert(alias_key.clone(), previous_alias.clone());
        let ambiguous = file(dir.path(), "a.hex", HEX);
        let unique = file(dir.path(), "a (2).hex", HEX);
        let paths = if reversed {
            vec![unique.clone(), ambiguous.clone()]
        } else {
            vec![ambiguous.clone(), unique.clone()]
        };
        let mut prepared = prepare(&mut state, paths);
        let alias_index = prepared
            .groups
            .iter()
            .position(|group| group.key == alias_key)
            .unwrap();
        // Model a target-specific rejection from the same Standard validator.
        let mut unsupported = prepared.groups[alias_index].targets[0].clone();
        unsupported.info.protocol_version = 2;
        let file = prepared
            .files
            .iter_mut()
            .find(|file| file.path == unique)
            .unwrap();
        let error = validate_ota_image(
            file.image.as_ref().unwrap(),
            &unsupported,
            ConfirmationPolicy::Standard,
        )
        .unwrap_err();
        file.candidates
            .iter_mut()
            .find(|(index, _)| *index == alias_index)
            .unwrap()
            .1 = Err(error);
        state.apply_ota_batch(prepared);
        assert!(matches!(
            outcomes(&state, &ambiguous),
            [BatchOutcome::Ambiguous(_)]
        ));
        assert!(
            outcomes(&state, &unique)
                .iter()
                .any(|outcome| matches!(outcome, BatchOutcome::Assigned(_)))
        );
        assert!(
            outcomes(&state, &unique)
                .iter()
                .any(|outcome| matches!(outcome, BatchOutcome::Invalid(_, _)))
        );
        assert_eq!(
            state.assignments[&original_key].resource.as_deref(),
            Some(&Resource::File(unique))
        );
        assert!(Arc::ptr_eq(
            &state.assignments[&alias_key].bytes,
            &previous_alias.bytes
        ));
    }
}

#[gpui_kit::test]
fn retained_batch_group_role_uses_the_current_locale(cx: &mut gpui_kit::TestAppContext) {
    let _session = crate::test_support::UiTestSession::new(cx);
    let (dir, mut state) = setup();
    let receiver_file = file(dir.path(), "c.hex", HEX);
    let prepared = prepare(&mut state, vec![receiver_file.clone()]);
    state.apply_ota_batch(prepared);
    let [BatchOutcome::Assigned(group)] = outcomes(&state, &receiver_file) else {
        panic!("the receiver image must be assigned");
    };
    assert!(group.receiver);
    let before = group.label();
    rust_i18n::set_locale("zh-CN");
    let after = group.label();
    assert_ne!(before, after);
    assert!(before.contains(&group.board) && after.contains(&group.board));
    assert!(before.contains(&group.bootloader) && after.contains(&group.bootloader));
}

#[test]
fn defaults_can_arrive_during_picker_but_membership_recreation_and_manual_edits_win() {
    let (dir, mut state) = setup();
    let a = file(dir.path(), "a.hex", HEX);
    let b = file(dir.path(), "b.hex", HEX);
    let c = file(dir.path(), "c.hex", HEX);
    let keys: Vec<_> = state.targets.iter().map(ota_assignment_key).collect();
    let mut picker_scope = state.ota_batch_scope();
    let default = old("default.hex");
    let destination = ImageDestination::Ota {
        key: keys[0].clone(),
        generation: state.group_generations[&keys[0]],
    };
    state.assign_image(&destination, default).unwrap();
    state.begin_ota_batch(&mut picker_scope);
    let prepared = prepare_ota_batch(
        vec![a.clone(), b.clone(), c.clone()],
        picker_scope,
        state.catalog.clone(),
        &AtomicBool::new(false),
    )
    .unwrap();
    let manual_destination = ImageDestination::Ota {
        key: keys[1].clone(),
        generation: state.group_generations[&keys[1]],
    };
    let manual = old("manual-later.hex");
    state
        .choose_image(&manual_destination, manual.clone())
        .unwrap();
    let receiver_key = ota_target_key(&state.targets[2]);
    state.selected_targets.remove(&receiver_key);
    state.reconcile_groups(None);
    state.selected_targets.insert(receiver_key);
    state.reconcile_groups(None);
    state.apply_ota_batch(prepared);
    assert_eq!(
        state.assignments[&keys[0]].resource.as_deref(),
        Some(&Resource::File(a.clone()))
    );
    assert!(Arc::ptr_eq(
        &state.assignments[&keys[1]].bytes,
        &manual.bytes
    ));
    assert!(!state.assignments.contains_key(&keys[2]));
    assert!(matches!(outcomes(&state, &b), [BatchOutcome::Stale(_)]));
    assert!(matches!(outcomes(&state, &c), [BatchOutcome::Stale(_)]));

    let mut groups = state.ota_batch_scope();
    state.begin_ota_batch(&mut groups);
    let prepared = prepare_ota_batch(
        vec![a.clone()],
        groups,
        state.catalog.clone(),
        &AtomicBool::new(false),
    )
    .unwrap();
    let added = target("other", 3, "board-a");
    state.selected_targets.insert(ota_target_key(&added));
    state.targets.push(added);
    state.reconcile_groups(None);
    state
        .selected_targets
        .remove(&ota_target_key(state.targets.last().unwrap()));
    assert!(state.targets.pop().is_some());
    state.reconcile_groups(None);
    state.apply_ota_batch(prepared);
    assert!(matches!(outcomes(&state, &a), [BatchOutcome::Stale(_)]));

    let mut groups = state.ota_batch_scope();
    state.begin_ota_batch(&mut groups);
    let prepared = prepare_ota_batch(
        vec![a.clone()],
        groups,
        state.catalog.clone(),
        &AtomicBool::new(false),
    )
    .unwrap();
    state.targets[0].info.flash_base ^= 0x1000;
    state.reconcile_groups(None);
    state.targets[0].info.flash_base ^= 0x1000;
    state.reconcile_groups(None);
    state.apply_ota_batch(prepared);
    assert!(matches!(outcomes(&state, &a), [BatchOutcome::Stale(_)]));

    let mut picker_scope = state.ota_batch_scope();
    let later = old("during-picker.hex");
    state.choose_image(&destination, later.clone()).unwrap();
    state.begin_ota_batch(&mut picker_scope);
    let prepared = prepare_ota_batch(
        vec![a.clone()],
        picker_scope,
        state.catalog.clone(),
        &AtomicBool::new(false),
    )
    .unwrap();
    state.apply_ota_batch(prepared);
    assert!(matches!(outcomes(&state, &a), [BatchOutcome::Stale(_)]));
    assert!(Arc::ptr_eq(
        &state.assignments[&keys[0]].bytes,
        &later.bytes
    ));
}

#[test]
fn accepted_unmatched_batch_suppresses_a_late_internal_default() {
    let (dir, mut state) = setup();
    let key = ota_assignment_key(&state.targets[0]);
    let destination = ImageDestination::Ota {
        key: key.clone(),
        generation: state.group_generations[&key],
    };
    let mut groups = state.ota_batch_scope();
    state.begin_ota_batch(&mut groups);
    state.apply_internal_default(destination, Ok(old("a.hex")));
    assert!(!state.assignments.contains_key(&key));
    let unknown = file(dir.path(), "unknown.hex", HEX);
    let prepared = prepare_ota_batch(
        vec![unknown.clone()],
        groups,
        state.catalog.clone(),
        &AtomicBool::new(false),
    )
    .unwrap();
    state.apply_ota_batch(prepared);
    assert!(matches!(
        outcomes(&state, &unknown),
        [BatchOutcome::Unknown]
    ));
    assert!(!state.assignments.contains_key(&key));
}

fn uf2_52840() -> Vec<u8> {
    let mut bytes = vec![0u8; 512];
    for (offset, value) in [
        (0, 0x0a32_4655u32),
        (4, 0x9e5d_5157),
        (8, 0x2000),
        (12, 0x1000),
        (16, 256),
        (20, 0),
        (24, 1),
        (28, 0xada5_2840),
        (508, 0x0ab1_6f30),
    ] {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    bytes
}

fn mcuboot() -> Vec<u8> {
    use sha2::{Digest, Sha256};
    let mut bytes = vec![0; 76];
    bytes[..4].copy_from_slice(&0x96f3_b83du32.to_le_bytes());
    bytes[8..10].copy_from_slice(&32u16.to_le_bytes());
    bytes[12..16].copy_from_slice(&4u32.to_le_bytes());
    bytes[32..36].fill(1);
    bytes[36..40].copy_from_slice(&[7, 0x69, 40, 0]);
    bytes[40..44].copy_from_slice(&[0x10, 0, 32, 0]);
    let digest = Sha256::digest(&bytes[..36]);
    bytes[44..].copy_from_slice(&digest);
    bytes
}

#[test]
fn one_batch_loads_uf2_hex_and_mcuboot_for_their_exact_groups() {
    let (dir, mut state) = setup();
    state.targets[1].info.bootloader = "mcuboot".into();
    state.reconcile_groups(None);
    let mut uf2 = uf2_52840();
    uf2[28..32].copy_from_slice(&0x621e_937au32.to_le_bytes());
    let a = file(dir.path(), "a.uf2", &uf2);
    let b = file(dir.path(), "b.bin", &mcuboot());
    let c = file(dir.path(), "c.hex", HEX);
    let prepared = prepare(&mut state, vec![a.clone(), b.clone(), c.clone()]);
    state.apply_ota_batch(prepared);
    for (index, path) in [a, b, c].into_iter().enumerate() {
        let key = ota_assignment_key(&state.targets[index]);
        assert_eq!(
            state.assignments[&key].resource.as_deref(),
            Some(&Resource::File(path.clone()))
        );
        assert!(matches!(
            outcomes(&state, &path),
            [BatchOutcome::Assigned(_)]
        ));
    }
}

#[test]
fn incompatible_method_does_not_hide_another_exact_method_group() {
    let (dir, mut state) = setup();
    let plain = target("r", 1, "board-b");
    let mut mcuboot_target = target("other", 4, "board-b");
    mcuboot_target.info.bootloader = "mcuboot".into();
    state.targets = vec![plain.clone(), mcuboot_target.clone()];
    state.selected_targets = state.targets.iter().map(ota_target_key).collect();
    state.reconcile_groups(None);
    let hex = file(dir.path(), "b.hex", HEX);
    let bin = file(dir.path(), "b.bin", &mcuboot());
    let prepared = prepare(&mut state, vec![hex.clone(), bin.clone()]);
    state.apply_ota_batch(prepared);
    for (target, path) in [(&plain, &hex), (&mcuboot_target, &bin)] {
        assert_eq!(
            state.assignments[&ota_assignment_key(target)]
                .resource
                .as_deref(),
            Some(&Resource::File(path.clone()))
        );
        assert!(
            outcomes(&state, path)
                .iter()
                .any(|outcome| matches!(outcome, BatchOutcome::Assigned(_)))
        );
        assert!(
            outcomes(&state, path)
                .iter()
                .any(|outcome| matches!(outcome, BatchOutcome::Invalid(_, _)))
        );
    }
}

#[test]
fn every_group_member_must_pass_standard_validation_under_both_policies() {
    for minimal in [false, true] {
        let (dir, mut state) = setup();
        state.set_confirmation_policy(minimal);
        let mut unsupported = target("other", 3, "board-a");
        unsupported.info.protocol_version = 2;
        state.selected_targets.insert(ota_target_key(&unsupported));
        state.targets.push(unsupported);
        state.reconcile_groups(None);
        let key = ota_assignment_key(&state.targets[0]);
        let previous = old("previous.hex");
        state.assignments.insert(key.clone(), previous.clone());
        let path = file(dir.path(), "a.hex", HEX);
        let prepared = prepare(&mut state, vec![path.clone()]);
        state.apply_ota_batch(prepared);
        assert!(Arc::ptr_eq(&state.assignments[&key].bytes, &previous.bytes));
        assert!(matches!(
            outcomes(&state, &path),
            [BatchOutcome::Invalid(_, _)]
        ));
    }
}

#[test]
fn strict_profile_role_format_and_mcu_checks_do_not_follow_minimal_policy() {
    for minimal in [false, true] {
        let (dir, mut state) = setup();
        state.set_confirmation_policy(minimal);
        let a = ota_assignment_key(&state.targets[0]);
        let initial = old("old.hex");
        state.assignments.insert(a.clone(), initial.clone());
        let wrong_family = file(dir.path(), "a.uf2", &uf2_52840());
        let wrong_format = file(dir.path(), "a.bin", &mcuboot());
        let profile = file(dir.path(), "a_DataCollect_HID.hex", HEX);
        let malformed = file(dir.path(), "b.hex", b"invalid hex");
        let prepared = prepare(
            &mut state,
            vec![
                wrong_family.clone(),
                wrong_format.clone(),
                profile.clone(),
                malformed.clone(),
            ],
        );
        state.apply_ota_batch(prepared);
        assert!(Arc::ptr_eq(&state.assignments[&a].bytes, &initial.bytes));
        assert!(matches!(
            outcomes(&state, &wrong_family),
            [BatchOutcome::Invalid(_, _)]
        ));
        assert!(matches!(
            outcomes(&state, &wrong_format),
            [BatchOutcome::Invalid(_, _)]
        ));
        assert!(matches!(
            outcomes(&state, &profile),
            [BatchOutcome::Invalid(_, _)]
        ));
        assert!(matches!(
            outcomes(&state, &malformed),
            [BatchOutcome::Invalid(_, _)]
        ));
        let receiver = target("x", RECEIVER_ID, "board-a");
        state.selected_targets.clear();
        state.targets = vec![receiver.clone()];
        state.selected_targets.insert(ota_target_key(&receiver));
        state.reconcile_groups(None);
        let tracker_file = file(dir.path(), "a.hex", HEX);
        let prepared = prepare(&mut state, vec![tracker_file.clone()]);
        state.apply_ota_batch(prepared);
        assert!(state.assignments.is_empty());
        assert!(matches!(
            outcomes(&state, &tracker_file),
            [BatchOutcome::NoMatch]
        ));
    }
}

#[test]
fn bounds_cancellation_and_mixed_file_errors_preserve_other_choices() {
    let (dir, mut state) = setup();
    let a = file(dir.path(), "a.hex", HEX);
    let malformed = file(dir.path(), "b.hex", b"invalid");
    let missing = dir.path().join("missing.hex");
    let prepared = prepare(
        &mut state,
        vec![a.clone(), malformed.clone(), missing.clone()],
    );
    state.apply_ota_batch(prepared);
    assert!(matches!(outcomes(&state, &a), [BatchOutcome::Assigned(_)]));
    assert!(matches!(
        outcomes(&state, &malformed),
        [BatchOutcome::Invalid(_, _)]
    ));
    assert!(matches!(
        outcomes(&state, &missing),
        [BatchOutcome::Invalid(_, _)]
    ));
    let oversized = dir.path().join("c.hex");
    std::fs::File::create(&oversized)
        .unwrap()
        .set_len(MAX_BATCH_FILE_BYTES + 1)
        .unwrap();
    let valid_b = file(dir.path(), "b.hex", HEX);
    let prepared = prepare(&mut state, vec![oversized.clone(), valid_b.clone()]);
    state.apply_ota_batch(prepared);
    assert!(matches!(
        outcomes(&state, &oversized),
        [BatchOutcome::Invalid(_, _)]
    ));
    assert!(matches!(
        outcomes(&state, &valid_b),
        [BatchOutcome::Assigned(_)]
    ));
    let key = ota_assignment_key(&state.targets[0]);
    let previous = state.assignments[&key].bytes.clone();
    let mut groups = state.ota_batch_scope();
    state.begin_ota_batch(&mut groups);
    assert!(
        prepare_ota_batch(
            vec![a.clone()],
            groups,
            state.catalog.clone(),
            &AtomicBool::new(true)
        )
        .is_err()
    );
    assert!(Arc::ptr_eq(&state.assignments[&key].bytes, &previous));
    assert!(
        distinct_batch_paths((0..=MAX_BATCH_FILES).map(|n| dir.path().join(format!("{n}.hex"))))
            .is_err()
    );
    let mut large = Vec::new();
    for n in 0..5 {
        let path = dir.path().join(format!("large-{n}.hex"));
        let handle = std::fs::File::create(&path).unwrap();
        handle.set_len(MAX_BATCH_FILE_BYTES).unwrap();
        large.push(path);
    }
    let mut groups = state.ota_batch_scope();
    state.begin_ota_batch(&mut groups);
    assert!(
        prepare_ota_batch(
            large,
            groups,
            state.catalog.clone(),
            &AtomicBool::new(false)
        )
        .is_err()
    );
    assert!(Arc::ptr_eq(&state.assignments[&key].bytes, &previous));
}

struct BatchStartHarness {
    workbench: gpui_kit::Entity<crate::application::Workbench>,
    workflow: bool,
}

impl gpui_kit::Render for BatchStartHarness {
    fn render(
        &mut self,
        _: &mut gpui_kit::Window,
        cx: &mut gpui_kit::Context<Self>,
    ) -> impl gpui_kit::IntoElement {
        self.workbench.update(cx, |this, cx| {
            if self.workflow {
                this.flash_esb_workflow(cx)
            } else {
                this.flash_review_controls(cx)
            }
        })
    }
}

#[gpui_kit::test]
fn failed_drop_revokes_consent_and_old_rendered_start_for_both_controls(
    cx: &mut gpui_kit::TestAppContext,
) {
    use crate::application::{FeaturePage, test_support::offline_workbench};
    use gpui_kit::{
        AppContext as _, Bounds, ExternalPaths, InputEvent as _, MouseButton, MouseDownEvent,
        MouseMoveEvent, MouseUpEvent, WindowBounds, WindowOptions, px, size,
        test::TestWindowExt as _,
    };

    let _session = crate::test_support::UiTestSession::new(cx);
    let runtime = crate::app_runtime::AppRuntime::new().unwrap();
    for workflow in [false, true] {
        let (dir, mut state) = setup();
        let receiver = state.targets[2].clone();
        state.targets = vec![receiver.clone()];
        state.selected_targets = [ota_target_key(&receiver)].into();
        state.reconcile_groups(None);
        let mut image = old("c.hex");
        image.hints = state
            .catalog
            .as_ref()
            .unwrap()
            .match_filename(&image.name)
            .into();
        let destination = state.destination(Mode::Esb).unwrap();
        state.assign_image(&destination, image).unwrap();
        state.set_confirmation_policy(true);
        state.ota_step = OtaStep::Mapping;
        let executor = runtime.executor();
        let (window, harness) = cx.update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        Default::default(),
                        size(px(1000.), px(900.)),
                    ))),
                    ..Default::default()
                },
                cx,
                move |window, cx| {
                    let workbench = cx.new(|cx| {
                        let mut this = offline_workbench(
                            window,
                            cx,
                            crate::sessions::SessionManager::new(),
                            executor,
                        );
                        this.active_page = FeaturePage::Ota;
                        this.preferences.minimal_firmware_confirmation = true;
                        this.inventory.receivers.push(receiver.receiver.clone());
                        this.flash = state;
                        this
                    });
                    cx.new(|_| BatchStartHarness {
                        workbench,
                        workflow,
                    })
                },
            )
            .unwrap()
        });
        let workbench = harness.read_with(cx, |harness, _| harness.workbench.clone());
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            let id = if workflow {
                "ota-step-next"
            } else {
                "flash-review"
            };
            assert_eq!(
                window.find(id).label(),
                Some(tr("firmware.action.start_update").as_str())
            );
            let position = window.find(id).bounds().center();
            let modifiers = window.modifiers();
            window.dispatch_event(
                MouseMoveEvent {
                    position,
                    pressed_button: None,
                    modifiers,
                }
                .to_platform_input(),
                cx,
            );
            window.render_frame(cx);
            workbench.update(cx, |this, cx| {
                let action = this.selected_flash_action().unwrap();
                this.flash.review = Some(action);
                this.flash.acknowledged = true;
                this.flash.risk_acknowledged = true;
                this.flash.auto = true;
                this.flash.auto_requested = true;
                let paths: Vec<_> = (0..=MAX_BATCH_FILES)
                    .map(|n| dir.path().join(format!("{n}.hex")))
                    .collect();
                this.drop_firmware(&ExternalPaths(paths.into()), cx);
                assert!(this.flash.review.is_none());
                assert!(!this.flash.acknowledged && !this.flash.risk_acknowledged);
                assert!(!this.flash.auto && !this.flash.auto_requested);
                assert!(
                    this.flash.pending.is_none()
                        && this.flash.job.is_none()
                        && this.flash.leases.is_empty()
                );
                assert_eq!(this.flash.ota_step, OtaStep::Mapping);
                assert!(this.flash.error.is_some());
            });
            window.dispatch_event(
                MouseDownEvent {
                    button: MouseButton::Left,
                    position,
                    modifiers,
                    click_count: 1,
                    first_mouse: false,
                }
                .to_platform_input(),
                cx,
            );
            window.dispatch_event(
                MouseUpEvent {
                    button: MouseButton::Left,
                    position,
                    modifiers,
                    click_count: 1,
                }
                .to_platform_input(),
                cx,
            );
            workbench.update(cx, |this, _| {
                assert!(
                    this.flash.review.is_some(),
                    "the previously rendered Start is downgraded to review"
                );
                assert!(
                    this.flash.pending.is_none()
                        && this.flash.job.is_none()
                        && this.flash.leases.is_empty()
                );
            });
        })
        .unwrap();
    }
}

#[gpui_kit::test]
fn multi_file_drop_prepares_and_assigns_without_advancing_or_reserving(
    cx: &mut gpui_kit::TestAppContext,
) {
    use crate::application::{FeaturePage, test_support::offline_workbench};
    use gpui_kit::{ExternalPaths, VisualContext as _};
    use std::time::{Duration, Instant};

    let _session = crate::test_support::UiTestSession::new(cx);
    let runtime = crate::app_runtime::AppRuntime::new().unwrap();
    let executor = runtime.executor();
    let (dir, mut state) = setup();
    state.ota_step = OtaStep::Firmware;
    let paths: Vec<_> = ["a.hex", "b.hex", "c.hex", "d.hex"]
        .into_iter()
        .map(|name| file(dir.path(), name, HEX))
        .collect();
    let cx = cx.add_empty_window();
    let workbench = cx.new_window_entity(move |window, cx| {
        let mut this =
            offline_workbench(window, cx, crate::sessions::SessionManager::new(), executor);
        this.active_page = FeaturePage::Ota;
        this.flash = state;
        this
    });
    workbench.update(cx, |this, cx| {
        this.drop_firmware(&ExternalPaths(paths.clone().into()), cx);
        assert!(this.flash.job.is_some());
        assert!(this.flash.pending.is_none() && this.flash.leases.is_empty());
        assert_eq!(this.flash.ota_step, OtaStep::Firmware);
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    let result = loop {
        if let Some((result, _)) = workbench.update(cx, |this, _| this.flash.poll_job()) {
            break result.unwrap();
        }
        assert!(Instant::now() < deadline, "batch preparation must finish");
        std::thread::sleep(Duration::from_millis(2));
    };
    let Payload::OtaBatch(prepared) = result else {
        panic!("expected batch preparation")
    };
    workbench.update(cx, |this, _| {
        this.flash.apply_ota_batch(prepared);
        for (index, path) in paths.iter().take(3).enumerate() {
            let key = ota_assignment_key(&this.flash.targets[index]);
            assert_eq!(
                this.flash.assignments[&key].resource.as_deref(),
                Some(&Resource::File(path.clone()))
            );
        }
        assert!(matches!(
            outcomes(&this.flash, &paths[3]),
            [BatchOutcome::NoMatch]
        ));
        assert_eq!(this.flash.ota_step, OtaStep::Firmware);
        assert!(
            this.flash.review.is_none()
                && this.flash.pending.is_none()
                && this.flash.leases.is_empty()
        );
        assert!(this.flash.job.is_none());
    });
}
