use super::{
    DeviceKey, DeviceObservation, DeviceRecord, DeviceRegistry, MAX_RECORDS, ObservedDevices,
    RegistryValidationError, ReportedMetadata,
};
use crate::domain::{DeviceIdentity, DeviceRole, Inventory, ReceiverEndpoint, TrackerLink};

fn receiver(path: &str, serial: Option<&str>) -> ReceiverEndpoint {
    ReceiverEndpoint {
        key: format!("hid:{path}"),
        path: path.as_bytes().to_vec(),
        label: "Synthetic receiver".into(),
        serial_number: serial.map(str::to_owned),
    }
}

fn tracker(receiver: &str, slot: u8, address: Option<&str>) -> TrackerLink {
    TrackerLink {
        receiver_key: receiver.into(),
        slot,
        address: address.map(str::to_owned),
        online: true,
    }
}

fn identity(address: &str) -> DeviceIdentity {
    DeviceIdentity {
        role: DeviceRole::Tracker,
        name: Some("SlimeNRF Tracker".into()),
        address: Some(address.into()),
        board: Some("board-v1".into()),
        target: Some("target-v1".into()),
        ..DeviceIdentity::default()
    }
}

fn observation(address: &str) -> DeviceObservation {
    DeviceObservation {
        key: DeviceKey::tracker_address(address).unwrap(),
        reported: ReportedMetadata::from_identity(&identity(address)),
        last_receiver: None,
    }
}

#[test]
fn known_protocol_keys_canonicalize_without_cross_namespace_guessing() {
    let receiver = DeviceKey::receiver_serial(" 0011aabb2233ccdd ").unwrap();
    let tracker = DeviceKey::tracker_address("aa-bb-22-33-cc-dd").unwrap();
    assert_eq!(receiver.role(), DeviceRole::Receiver);
    assert_eq!(receiver.value(), "0011AABB2233CCDD");
    assert_eq!(tracker.role(), DeviceRole::Tracker);
    assert_eq!(tracker.value(), "AABB2233CCDD");
    assert_eq!(
        receiver.to_string(),
        "slimenrf_receiver_usb_serial_v1:0011AABB2233CCDD"
    );
    assert_eq!(
        tracker.to_string(),
        "slimenrf_tracker_rf_deviceaddr_v1:AABB2233CCDD"
    );
    assert_ne!(receiver, tracker);
    for value in [
        "",
        "COM3",
        "001122334455",
        "001122334455667788",
        "00112233-4556677",
    ] {
        assert_eq!(DeviceKey::receiver_serial(value), None, "{value}");
    }
    assert_eq!(DeviceKey::tracker_address("0011223344556677"), None);
}

#[test]
fn direct_tracker_requires_explicit_role_and_reported_address() {
    let mut reported = identity("112233445566");
    assert_eq!(
        DeviceKey::direct_tracker(&reported).unwrap().value(),
        "112233445566"
    );
    reported.role = DeviceRole::Unknown;
    assert_eq!(DeviceKey::direct_tracker(&reported), None);
    reported.role = DeviceRole::Receiver;
    assert_eq!(DeviceKey::direct_tracker(&reported), None);
    reported.role = DeviceRole::Tracker;
    reported.address = None;
    assert_eq!(DeviceKey::direct_tracker(&reported), None);
    assert_eq!(DeviceKey::tracker(&tracker("receiver-a", 7, None)), None);
    assert_eq!(
        DeviceKey::tracker(&tracker("receiver-a", 16, Some("112233445566"))),
        None
    );
}

#[test]
fn receiver_requires_unique_current_serial_and_exact_endpoint_facts() {
    let current = receiver("one", Some("0011223344556677"));
    let mut inventory = Inventory {
        receivers: vec![current.clone()],
        ..Inventory::default()
    };
    assert_eq!(
        DeviceKey::receiver(&current, &inventory).unwrap().value(),
        "0011223344556677"
    );
    inventory
        .receivers
        .push(receiver("two", Some("0011223344556677")));
    assert_eq!(DeviceKey::receiver(&current, &inventory), None);
    inventory.receivers.pop();
    inventory.receivers[0].serial_number = Some("8899AABBCCDDEEFF".into());
    assert_eq!(DeviceKey::receiver(&current, &inventory), None);
    inventory.receivers[0] = receiver("replacement-path", Some("0011223344556677"));
    assert_eq!(DeviceKey::receiver(&current, &inventory), None);
    inventory.receivers[0] = current.clone();
    inventory.receivers.push(receiver("one", None));
    assert_eq!(DeviceKey::receiver(&current, &inventory), None);
}

#[test]
fn nickname_follows_actual_address_across_receiver_and_slot_changes() {
    let mut registry = DeviceRegistry::default();
    let first = tracker("receiver-a", 2, Some("112233445566"));
    let key = DeviceKey::tracker(&first).unwrap();
    registry.observe(&observation("112233445566")).unwrap();
    registry.rename(key, "Left foot").unwrap();

    let moved = tracker("receiver-b", 11, Some("11:22:33:44:55:66"));
    assert_eq!(DeviceKey::tracker(&moved), Some(key));
    registry
        .observe(&DeviceObservation {
            key: DeviceKey::tracker(&moved).unwrap(),
            reported: ReportedMetadata::default(),
            last_receiver: None,
        })
        .unwrap();
    assert_eq!(
        registry.get(&key).unwrap().nickname.as_deref(),
        Some("Left foot")
    );

    let replacement = tracker("receiver-b", 11, Some("AABBCCDDEEFF"));
    let replacement_key = DeviceKey::tracker(&replacement).unwrap();
    registry.observe(&observation("AABBCCDDEEFF")).unwrap();
    assert_eq!(registry.get(&replacement_key).unwrap().nickname, None);
    assert_eq!(
        registry.get(&key).unwrap().nickname.as_deref(),
        Some("Left foot")
    );
}

#[test]
fn receiver_nickname_survives_hid_path_change_without_merging_serials() {
    let original = receiver("path-a", Some("0011223344556677"));
    let mut inventory = Inventory {
        receivers: vec![original.clone()],
        ..Inventory::default()
    };
    let key = DeviceKey::receiver(&original, &inventory).unwrap();
    let mut registry = DeviceRegistry::default();
    registry
        .observe(&DeviceObservation {
            key,
            reported: ReportedMetadata::default(),
            last_receiver: None,
        })
        .unwrap();
    registry.rename(key, "Desk receiver").unwrap();
    inventory.receivers[0] = receiver("path-b", Some("0011223344556677"));
    let moved = DeviceKey::receiver(&inventory.receivers[0], &inventory).unwrap();
    assert_eq!(
        registry.get(&moved).unwrap().nickname.as_deref(),
        Some("Desk receiver")
    );
    inventory.receivers[0].serial_number = Some("8899AABBCCDDEEFF".into());
    let replacement = DeviceKey::receiver(&inventory.receivers[0], &inventory).unwrap();
    assert_ne!(replacement, key);
    assert_eq!(registry.get(&replacement), None);
}

#[test]
fn mutable_reports_update_in_place_and_missing_reports_retain_last_known_values() {
    let mut registry = DeviceRegistry::default();
    let mut report = observation("112233445566");
    registry.observe(&report).unwrap();
    registry.rename(report.key, "Ankle").unwrap();
    report.reported.board = Some("board-v2".into());
    report.reported.target = Some("target-v2".into());
    report.reported.firmware = Some("260923".into());
    assert_eq!(registry.observe(&report), Ok(true));
    assert_eq!(registry.observe(&report), Ok(false));
    assert_eq!(
        registry.observe(&DeviceObservation {
            key: report.key,
            reported: ReportedMetadata::default(),
            last_receiver: None,
        }),
        Ok(false)
    );
    let record = registry.get(&report.key).unwrap();
    assert_eq!(registry.records().len(), 1);
    assert_eq!(record.nickname.as_deref(), Some("Ankle"));
    assert_eq!(record.reported.board.as_deref(), Some("board-v2"));
    assert_eq!(record.reported.target.as_deref(), Some("target-v2"));
    assert_eq!(record.reported.firmware.as_deref(), Some("260923"));
}

#[test]
fn direct_and_forwarded_evidence_agree_but_duplicate_claims_are_ambiguous() {
    let link = tracker("receiver-a", 3, Some("112233445566"));
    let key = DeviceKey::tracker(&link).unwrap();
    let mut observed = ObservedDevices::default();
    observed.add_tracker(&link, None, ReportedMetadata::default());
    observed.add_direct_tracker(&identity("11:22:33:44:55:66"));
    assert_eq!(
        observed.get(&key).unwrap().reported.board.as_deref(),
        Some("board-v1")
    );
    assert_eq!(observed.observations().count(), 1);

    observed.add_tracker(
        &tracker("receiver-b", 9, Some("112233445566")),
        None,
        ReportedMetadata::default(),
    );
    assert_eq!(observed.get(&key), None);
    observed.add_direct_tracker(&identity("112233445566"));
    assert_eq!(observed.get(&key), None);
    observed.add_tracker(
        &tracker("receiver-b", 4, Some("AABBCCDDEEFF")),
        None,
        ReportedMetadata::default(),
    );
    assert_eq!(
        observed.into_observations(),
        vec![DeviceObservation {
            key: DeviceKey::tracker_address("AABBCCDDEEFF").unwrap(),
            reported: ReportedMetadata::default(),
            last_receiver: None,
        }]
    );
}

#[test]
fn duplicate_direct_sessions_do_not_select_an_arbitrary_nickname() {
    let key = DeviceKey::tracker_address("112233445566").unwrap();
    let mut observed = ObservedDevices::default();
    observed.add_direct_tracker(&identity("112233445566"));
    observed.add_direct_tracker(&identity("112233445566"));
    assert_eq!(observed.get(&key), None);
    observed.add_direct_tracker(&identity("AABBCCDDEEFF"));
    assert_eq!(observed.observations().count(), 1);
    assert_eq!(
        observed.observations().next().unwrap().key.value(),
        "AABBCCDDEEFF"
    );
}

#[test]
fn direct_metadata_precedence_does_not_depend_on_observation_order() {
    let key = DeviceKey::tracker_address("112233445566").unwrap();
    let mut observed = ObservedDevices::default();
    observed.add_direct_tracker(&identity("112233445566"));
    observed.add_tracker(
        &tracker("receiver-a", 2, Some("112233445566")),
        None,
        ReportedMetadata {
            board: Some("older-report".into()),
            firmware: Some("260923".into()),
            ..ReportedMetadata::default()
        },
    );
    let metadata = &observed.get(&key).unwrap().reported;
    assert_eq!(metadata.board.as_deref(), Some("board-v1"));
    assert_eq!(metadata.firmware.as_deref(), Some("260923"));
}

#[test]
fn rename_clear_and_invalid_text_preserve_other_identity_records() {
    let mut registry = DeviceRegistry::default();
    let first = observation("112233445566");
    let second = observation("AABBCCDDEEFF");
    registry.observe(&first).unwrap();
    registry.observe(&second).unwrap();
    registry.rename(first.key, "  左脚  ").unwrap();
    assert_eq!(
        registry.get(&first.key).unwrap().nickname.as_deref(),
        Some("左脚")
    );
    assert_eq!(registry.get(&second.key).unwrap().nickname, None);
    assert_eq!(
        registry.rename(first.key, "bad\nname"),
        Err(RegistryValidationError::InvalidNickname)
    );
    assert_eq!(
        registry.rename(first.key, &"x".repeat(81)),
        Err(RegistryValidationError::InvalidNickname)
    );
    assert_eq!(
        registry.get(&first.key).unwrap().nickname.as_deref(),
        Some("左脚")
    );
    registry.rename(first.key, "  ").unwrap();
    assert_eq!(registry.get(&first.key).unwrap().nickname, None);
    registry.forget(second.key);
    assert_eq!(
        registry.rename(second.key, "No recreation"),
        Err(RegistryValidationError::MissingRecord)
    );
    assert_eq!(registry.records().len(), 1);
}

#[test]
fn stored_model_rejects_duplicate_identity_and_noncanonical_nicknames() {
    let first = observation("112233445566");
    let record = DeviceRecord {
        key: first.key,
        nickname: Some("Left foot".into()),
        reported: first.reported,
        last_receiver: None,
    };
    assert_eq!(
        DeviceRegistry::from_records(vec![record.clone(), record.clone()]),
        Err(RegistryValidationError::DuplicateIdentity)
    );
    let mut invalid = record.clone();
    invalid.nickname = Some(" Left foot ".into());
    assert_eq!(
        DeviceRegistry::from_records(vec![invalid]),
        Err(RegistryValidationError::InvalidNickname)
    );
    let mut invalid = record;
    invalid.reported.target = Some("x".repeat(513));
    assert_eq!(
        DeviceRegistry::from_records(vec![invalid]),
        Err(RegistryValidationError::InvalidMetadata)
    );
}

#[test]
fn receiver_move_prefers_unique_recent_claim_over_old_offline_pairing() {
    let key = DeviceKey::tracker_address("112233445566").unwrap();
    let mut historical = tracker("old-receiver", 2, Some("112233445566"));
    historical.online = false;
    let recent = tracker("new-receiver", 11, Some("112233445566"));
    for historical_first in [false, true] {
        let mut observed = ObservedDevices::default();
        let old_metadata = ReportedMetadata {
            board: Some("historical-board".into()),
            firmware: Some("old-firmware".into()),
            ..ReportedMetadata::default()
        };
        let fresh_metadata = ReportedMetadata {
            board: Some("current-board".into()),
            firmware: Some("current-firmware".into()),
            ..ReportedMetadata::default()
        };
        if historical_first {
            observed.add_tracker(
                &historical,
                DeviceKey::receiver_serial("0011223344556677"),
                old_metadata.clone(),
            );
        }
        observed.add_tracker(
            &recent,
            DeviceKey::receiver_serial("8899AABBCCDDEEFF"),
            fresh_metadata,
        );
        if !historical_first {
            observed.add_tracker(
                &historical,
                DeviceKey::receiver_serial("0011223344556677"),
                old_metadata,
            );
        }
        assert_eq!(
            observed.get(&key).unwrap().last_receiver,
            DeviceKey::receiver_serial("8899AABBCCDDEEFF")
        );
        assert!(observed.is_current(&key));
        assert_eq!(
            observed.get(&key).unwrap().reported.board.as_deref(),
            Some("current-board")
        );
        assert_eq!(
            observed.get(&key).unwrap().reported.firmware.as_deref(),
            Some("current-firmware")
        );
        let mut registry = DeviceRegistry::default();
        registry.observe(&observation("112233445566")).unwrap();
        registry.rename(key, "Left foot").unwrap();
        registry.observe(observed.get(&key).unwrap()).unwrap();
        assert_eq!(registry.records().len(), 1);
        assert_eq!(
            registry.get(&key).unwrap().nickname.as_deref(),
            Some("Left foot")
        );
        assert_eq!(
            registry.get(&key).unwrap().reported.board.as_deref(),
            Some("current-board")
        );
    }
}

#[test]
fn historical_pairs_do_not_claim_presence_or_overwrite_last_reported_metadata() {
    let key = DeviceKey::tracker_address("112233445566").unwrap();
    let mut old = tracker("receiver-a", 2, Some("112233445566"));
    old.online = false;
    let mut observed = ObservedDevices::default();
    observed.add_tracker(
        &old,
        DeviceKey::receiver_serial("0011223344556677"),
        ReportedMetadata {
            board: Some("stale-board".into()),
            ..ReportedMetadata::default()
        },
    );
    assert_eq!(
        observed.get(&key).unwrap().reported,
        ReportedMetadata::default()
    );
    assert!(!observed.is_current(&key));
    let mut second = tracker("receiver-b", 9, Some("112233445566"));
    second.online = false;
    observed.add_tracker(
        &second,
        DeviceKey::receiver_serial("8899AABBCCDDEEFF"),
        ReportedMetadata::default(),
    );
    assert_eq!(observed.get(&key), None);
    assert!(!observed.is_current(&key));
    observed.add_direct_tracker(&identity("112233445566"));
    assert!(observed.is_current(&key));
    assert_eq!(
        observed.get(&key).unwrap().reported.board.as_deref(),
        Some("board-v1")
    );
}

#[test]
fn simultaneous_recent_claims_stay_ambiguous_despite_historical_or_direct_reports() {
    let key = DeviceKey::tracker_address("112233445566").unwrap();
    let mut observed = ObservedDevices::default();
    let mut old = tracker("old-receiver", 1, Some("112233445566"));
    old.online = false;
    observed.add_tracker(&old, None, ReportedMetadata::default());
    observed.add_tracker(
        &tracker("current-receiver-a", 7, Some("112233445566")),
        DeviceKey::receiver_serial("0011223344556677"),
        ReportedMetadata::default(),
    );
    observed.add_tracker(
        &tracker("current-receiver-b", 8, Some("112233445566")),
        DeviceKey::receiver_serial("8899AABBCCDDEEFF"),
        ReportedMetadata::default(),
    );
    observed.add_direct_tracker(&identity("112233445566"));
    assert_eq!(observed.get(&key), None);
    assert!(!observed.is_current(&key));
    observed.add_direct_tracker(&identity("AABBCCDDEEFF"));
    assert_eq!(observed.observations().count(), 1);
    assert_eq!(
        observed.observations().next().unwrap().key.value(),
        "AABBCCDDEEFF"
    );
}

#[test]
fn registry_capacity_refuses_new_identity_without_losing_existing_records() {
    let mut registry = DeviceRegistry::default();
    for index in 0..MAX_RECORDS {
        registry
            .observe(&observation(&format!("AABB{index:08X}")))
            .unwrap();
    }
    let key = DeviceKey::tracker_address("AABB00000000").unwrap();
    registry.rename(key, "First tracker").unwrap();
    assert_eq!(
        registry.observe(&observation("112233445566")),
        Err(RegistryValidationError::TooManyRecords)
    );
    assert_eq!(registry.records().len(), MAX_RECORDS);
    assert_eq!(
        registry.get(&key).unwrap().nickname.as_deref(),
        Some("First tracker")
    );
    assert_eq!(
        registry.get(&DeviceKey::tracker_address("112233445566").unwrap()),
        None
    );
}

#[test]
fn last_receiver_requires_tracker_owner_and_receiver_target_without_mutation() {
    let tracker = DeviceKey::tracker_address("112233445566").unwrap();
    let receiver = DeviceKey::receiver_serial("0011223344556677").unwrap();
    let mut registry = DeviceRegistry::default();
    let valid = DeviceObservation {
        key: tracker,
        reported: ReportedMetadata::default(),
        last_receiver: Some(receiver),
    };
    registry.observe(&valid).unwrap();
    let saved = registry.clone();
    for (key, last_receiver) in [
        (receiver, receiver),
        (tracker, tracker),
        (receiver, tracker),
    ] {
        let invalid = DeviceObservation {
            key,
            reported: ReportedMetadata::default(),
            last_receiver: Some(last_receiver),
        };
        assert_eq!(
            registry.observation_changes(&invalid),
            Err(RegistryValidationError::InvalidReceiver)
        );
        assert_eq!(
            registry.observe(&invalid),
            Err(RegistryValidationError::InvalidReceiver)
        );
        assert_eq!(
            DeviceRegistry::from_records(vec![DeviceRecord {
                key,
                nickname: None,
                reported: ReportedMetadata::default(),
                last_receiver: Some(last_receiver),
            }]),
            Err(RegistryValidationError::InvalidReceiver)
        );
        assert_eq!(registry, saved);
    }
    assert_eq!(
        registry.get(&tracker).unwrap().last_receiver,
        Some(receiver)
    );
}

#[test]
fn direct_offline_and_ambiguous_rf_evidence_preserve_last_receiver() {
    let first = DeviceKey::receiver_serial("0011223344556677").unwrap();
    let second = DeviceKey::receiver_serial("8899AABBCCDDEEFF").unwrap();
    let key = DeviceKey::tracker_address("112233445566").unwrap();
    let mut registry = DeviceRegistry::default();
    registry
        .observe(&DeviceObservation {
            key,
            reported: ReportedMetadata::default(),
            last_receiver: Some(second),
        })
        .unwrap();
    let mut offline = tracker("receiver-a", 2, Some("112233445566"));
    offline.online = false;
    let mut observed = ObservedDevices::default();
    observed.add_tracker(&offline, Some(first), ReportedMetadata::default());
    assert_eq!(observed.get(&key).unwrap().last_receiver, None);
    registry.observe(observed.get(&key).unwrap()).unwrap();
    observed.add_direct_tracker(&identity("112233445566"));
    assert_eq!(observed.get(&key).unwrap().last_receiver, None);
    registry.observe(observed.get(&key).unwrap()).unwrap();
    assert_eq!(registry.get(&key).unwrap().last_receiver, Some(second));

    observed.add_tracker(
        &tracker("receiver-a", 3, Some("112233445566")),
        Some(first),
        ReportedMetadata::default(),
    );
    observed.add_tracker(
        &tracker("receiver-b", 4, Some("112233445566")),
        Some(second),
        ReportedMetadata::default(),
    );
    assert_eq!(observed.get(&key), None);
    for report in observed.into_observations() {
        registry.observe(&report).unwrap();
    }
    assert_eq!(registry.get(&key).unwrap().last_receiver, Some(second));

    let mut duplicate_slots = ObservedDevices::default();
    for slot in [3, 9] {
        duplicate_slots.add_tracker(
            &tracker("receiver-a", slot, Some("112233445566")),
            Some(first),
            ReportedMetadata::default(),
        );
    }
    assert_eq!(duplicate_slots.get(&key), None);
    assert_eq!(registry.get(&key).unwrap().last_receiver, Some(second));
}
