use super::{
    MAX_STORE_BYTES, RegistryEdit, RegistryPersistenceError, RegistryStore, RegistryStoreError,
};
use crate::device_registry::{
    DeviceKey, DeviceObservation, MAX_RECORDS, RegistryValidationError, ReportedMetadata,
};
use std::{collections::BTreeSet, fs};

fn observed(address: &str) -> DeviceObservation {
    DeviceObservation {
        key: DeviceKey::tracker_address(address).unwrap(),
        last_receiver: None,
        reported: ReportedMetadata {
            name: Some("SlimeNRF Tracker".into()),
            board: Some("synthetic-board".into()),
            target: Some("synthetic-target".into()),
            firmware: None,
        },
    }
}

fn observe(store: &mut RegistryStore, observations: Vec<DeviceObservation>) {
    store
        .apply(RegistryEdit::Observe {
            observations,
            absent: BTreeSet::new(),
        })
        .unwrap();
}

#[test]
fn missing_file_loads_without_writing_and_records_survive_reload() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("device-registry.json");
    let mut store = RegistryStore::load(path.clone()).unwrap();
    assert_eq!(store.snapshot().records().len(), 0);
    assert!(!path.exists());
    let tracker = observed("112233445566");
    let receiver = DeviceObservation {
        key: DeviceKey::receiver_serial("0011223344556677").unwrap(),
        reported: ReportedMetadata::default(),
        last_receiver: None,
    };
    observe(&mut store, vec![tracker.clone(), receiver.clone()]);
    store
        .apply(RegistryEdit::Rename {
            key: tracker.key,
            nickname: "左脚".into(),
        })
        .unwrap();
    let loaded = RegistryStore::load(path.clone()).unwrap().snapshot();
    assert_eq!(loaded.records().len(), 2);
    assert_eq!(
        loaded.get(&tracker.key).unwrap().nickname.as_deref(),
        Some("左脚")
    );
    assert_eq!(loaded.get(&receiver.key).unwrap().nickname, None);
    assert_eq!(
        loaded.get(&tracker.key).unwrap().reported.board.as_deref(),
        Some("synthetic-board")
    );
    let document: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(document["version"], 1);
    assert_eq!(
        document["records"][0]["key"]["kind"],
        "slimenrf_receiver_usb_serial_v1"
    );
    store
        .apply(RegistryEdit::Rename {
            key: tracker.key,
            nickname: "".into(),
        })
        .unwrap();
    store.apply(RegistryEdit::Forget(receiver.key)).unwrap();
    let loaded = RegistryStore::load(path).unwrap().snapshot();
    assert_eq!(loaded.records().len(), 1);
    assert_eq!(loaded.get(&tracker.key).unwrap().nickname, None);
}

#[test]
fn continuous_observations_do_not_recreate_forgotten_records() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("device-registry.json");
    let mut store = RegistryStore::load(path.clone()).unwrap();
    let tracker = observed("112233445566");
    observe(&mut store, vec![tracker.clone()]);
    store
        .apply(RegistryEdit::Rename {
            key: tracker.key,
            nickname: "Old name".into(),
        })
        .unwrap();
    store.apply(RegistryEdit::Forget(tracker.key)).unwrap();
    let forgotten = fs::read(&path).unwrap();
    for _ in 0..5 {
        observe(&mut store, vec![tracker.clone()]);
    }
    assert_eq!(store.snapshot().get(&tracker.key), None);
    assert_eq!(fs::read(&path).unwrap(), forgotten);
    store
        .apply(RegistryEdit::Remember(tracker.clone()))
        .unwrap();
    assert_eq!(store.snapshot().get(&tracker.key).unwrap().nickname, None);
    assert_eq!(
        store
            .snapshot()
            .get(&tracker.key)
            .unwrap()
            .reported
            .target
            .as_deref(),
        Some("synthetic-target")
    );
    assert_eq!(
        RegistryStore::load(path)
            .unwrap()
            .snapshot()
            .records()
            .len(),
        1
    );
}

#[test]
fn only_explicit_absence_or_remember_releases_continuous_forget_suppression() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("device-registry.json");
    let mut store = RegistryStore::load(path).unwrap();
    let tracker = observed("112233445566");
    observe(&mut store, vec![tracker.clone()]);
    store.apply(RegistryEdit::Forget(tracker.key)).unwrap();
    observe(&mut store, Vec::new());
    observe(&mut store, vec![tracker.clone()]);
    assert_eq!(store.snapshot().get(&tracker.key), None);
    store
        .apply(RegistryEdit::Observe {
            observations: vec![tracker.clone()],
            absent: BTreeSet::from([tracker.key]),
        })
        .unwrap();
    observe(&mut store, vec![tracker.clone()]);
    assert_eq!(store.snapshot().get(&tracker.key), None);
    store
        .apply(RegistryEdit::Observe {
            observations: Vec::new(),
            absent: BTreeSet::from([tracker.key]),
        })
        .unwrap();
    observe(&mut store, vec![tracker.clone()]);
    assert_eq!(store.snapshot().get(&tracker.key).unwrap().nickname, None);
}

#[test]
fn restart_ends_suppression_without_restoring_deleted_nickname() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("device-registry.json");
    let mut store = RegistryStore::load(path.clone()).unwrap();
    let tracker = observed("112233445566");
    observe(&mut store, vec![tracker.clone()]);
    store
        .apply(RegistryEdit::Rename {
            key: tracker.key,
            nickname: "Deleted".into(),
        })
        .unwrap();
    store.apply(RegistryEdit::Forget(tracker.key)).unwrap();
    let mut restarted = RegistryStore::load(path).unwrap();
    observe(&mut restarted, vec![tracker.clone()]);
    assert_eq!(
        restarted.snapshot().get(&tracker.key).unwrap().nickname,
        None
    );
}

#[test]
fn changed_observations_reread_external_records_and_keep_user_nicknames() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("device-registry.json");
    let mut local = RegistryStore::load(path.clone()).unwrap();
    let mut tracker = observed("112233445566");
    observe(&mut local, vec![tracker.clone()]);
    let mut external = RegistryStore::load(path.clone()).unwrap();
    external
        .apply(RegistryEdit::Rename {
            key: tracker.key,
            nickname: "External name".into(),
        })
        .unwrap();
    let other = observed("AABBCCDDEEFF");
    observe(&mut external, vec![other.clone()]);
    tracker.reported.firmware = Some("260923".into());
    observe(&mut local, vec![tracker.clone()]);
    let snapshot = local.snapshot();
    assert_eq!(snapshot.records().len(), 2);
    assert_eq!(
        snapshot.get(&tracker.key).unwrap().nickname.as_deref(),
        Some("External name")
    );
    assert_eq!(
        snapshot
            .get(&tracker.key)
            .unwrap()
            .reported
            .firmware
            .as_deref(),
        Some("260923")
    );
    assert_eq!(
        snapshot.get(&other.key).unwrap().reported.board.as_deref(),
        Some("synthetic-board")
    );
    assert_eq!(RegistryStore::load(path).unwrap().snapshot(), snapshot);
}

#[test]
fn unchanged_observation_does_no_filesystem_read_or_replacement() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("device-registry.json");
    let mut store = RegistryStore::load(path.clone()).unwrap();
    let tracker = observed("112233445566");
    observe(&mut store, vec![tracker.clone()]);
    let saved = store.snapshot();
    fs::write(&path, "{external-corruption").unwrap();
    observe(&mut store, vec![tracker.clone()]);
    assert_eq!(store.snapshot(), saved);
    assert!(std::sync::Arc::ptr_eq(&store.snapshot(), &saved));
    assert_eq!(fs::read_to_string(&path).unwrap(), "{external-corruption");
    assert!(matches!(
        store.apply(RegistryEdit::Rename {
            key: tracker.key,
            nickname: "Must not overwrite".into()
        }),
        Err(RegistryStoreError::Persistence(_))
    ));
    assert_eq!(store.snapshot(), saved);
    assert_eq!(fs::read_to_string(path).unwrap(), "{external-corruption");
}

#[test]
fn corrupt_unsupported_and_invalid_documents_are_never_overwritten() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("device-registry.json");
    let mut store = RegistryStore::load(path.clone()).unwrap();
    let tracker = observed("112233445566");
    observe(&mut store, vec![tracker.clone()]);
    let saved = store.snapshot();
    for content in [
        "{broken",
        "null",
        r#"{"version":2,"records":[]}"#,
        r#"{"records":[]}"#,
        r#"{"version":1,"records":[{}]}"#,
        r#"{"version":1,"records":[],"unknown":true}"#,
    ] {
        fs::write(&path, content).unwrap();
        assert!(RegistryStore::load(path.clone()).is_err());
        assert!(matches!(
            store.apply(RegistryEdit::Forget(tracker.key)),
            Err(RegistryStoreError::Persistence(_))
        ));
        assert_eq!(store.snapshot(), saved);
        assert_eq!(fs::read_to_string(&path).unwrap(), content);
    }
}

#[test]
fn duplicate_noncanonical_or_unsupported_keys_and_invalid_text_fail_loading() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("device-registry.json");
    let mut store = RegistryStore::load(path.clone()).unwrap();
    let tracker = observed("AABBCCDDEEFF");
    observe(&mut store, vec![tracker.clone()]);
    let saved = store.snapshot();
    let original: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    for mutation in 0..7 {
        let mut document = original.clone();
        match mutation {
            0 => {
                let duplicate = document["records"][0].clone();
                document["records"].as_array_mut().unwrap().push(duplicate);
            }
            1 => document["records"][0]["key"]["value"] = "aabbccddeeff".into(),
            2 => document["records"][0]["key"]["kind"] = "tracker-slot".into(),
            3 => document["records"][0]["key"]["value"] = "COM9".into(),
            4 => document["records"][0]["nickname"] = " padded ".into(),
            5 => document["records"][0]["reported"]["board"] = "bad\ntext".into(),
            _ => document["records"][0]["reported"]["firmware"] = "x".repeat(513).into(),
        }
        let bytes = serde_json::to_vec(&document).unwrap();
        fs::write(&path, &bytes).unwrap();
        assert!(RegistryStore::load(path.clone()).is_err());
        assert!(
            store
                .apply(RegistryEdit::Rename {
                    key: tracker.key,
                    nickname: "No overwrite".into()
                })
                .is_err()
        );
        assert_eq!(store.snapshot(), saved);
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }
}

#[test]
fn invalid_edits_preserve_committed_memory_and_bytes() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("device-registry.json");
    let mut store = RegistryStore::load(path.clone()).unwrap();
    let tracker = observed("112233445566");
    observe(&mut store, vec![tracker.clone()]);
    let saved = store.snapshot();
    let bytes = fs::read(&path).unwrap();
    assert!(matches!(
        store.apply(RegistryEdit::Rename {
            key: tracker.key,
            nickname: "x".repeat(81)
        }),
        Err(RegistryStoreError::Validation(
            RegistryValidationError::InvalidNickname
        ))
    ));
    assert!(matches!(
        store.apply(RegistryEdit::Observe {
            observations: vec![tracker.clone(), tracker.clone()],
            absent: BTreeSet::new()
        }),
        Err(RegistryStoreError::Validation(
            RegistryValidationError::DuplicateIdentity
        ))
    ));
    let mut invalid = tracker;
    invalid.reported.board = Some("x".repeat(513));
    assert!(matches!(
        store.apply(RegistryEdit::Remember(invalid)),
        Err(RegistryStoreError::Validation(
            RegistryValidationError::InvalidMetadata
        ))
    ));
    assert_eq!(store.snapshot(), saved);
    assert_eq!(fs::read(path).unwrap(), bytes);
}

#[test]
fn oversized_and_nonregular_documents_remain_untouched() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("device-registry.json");
    let file = fs::File::create(&path).unwrap();
    file.set_len(MAX_STORE_BYTES as u64 + 1).unwrap();
    drop(file);
    assert!(matches!(
        RegistryStore::load(path.clone()),
        Err(RegistryStoreError::Persistence(
            RegistryPersistenceError::TooLarge
        ))
    ));
    assert_eq!(
        fs::metadata(path).unwrap().len(),
        MAX_STORE_BYTES as u64 + 1
    );
    assert!(RegistryStore::load(directory.path().to_path_buf()).is_err());
}

#[test]
fn oversized_output_preserves_the_last_committed_snapshot_and_file() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("device-registry.json");
    let mut store = RegistryStore::load(path.clone()).unwrap();
    observe(&mut store, vec![observed("112233445566")]);
    let saved = store.snapshot();
    let bytes = fs::read(&path).unwrap();
    let observations = (0..600)
        .map(|index| DeviceObservation {
            key: DeviceKey::tracker_address(&format!("AABB{index:08X}")).unwrap(),
            last_receiver: None,
            reported: ReportedMetadata {
                name: Some("n".repeat(512)),
                board: Some("b".repeat(512)),
                target: Some("t".repeat(512)),
                firmware: Some("f".repeat(512)),
            },
        })
        .collect();
    assert!(matches!(
        store.apply(RegistryEdit::Observe {
            observations,
            absent: BTreeSet::new()
        }),
        Err(RegistryStoreError::Persistence(
            RegistryPersistenceError::TooLarge
        ))
    ));
    assert_eq!(store.snapshot(), saved);
    assert_eq!(fs::read(path).unwrap(), bytes);
}

#[cfg(windows)]
#[test]
fn denied_replacement_preserves_forget_and_remember_state_until_success() {
    use std::os::windows::fs::OpenOptionsExt;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("device-registry.json");
    let mut store = RegistryStore::load(path.clone()).unwrap();
    let tracker = observed("112233445566");
    observe(&mut store, vec![tracker.clone()]);
    let saved = store.snapshot();
    let bytes = fs::read(&path).unwrap();
    let lock = fs::OpenOptions::new()
        .read(true)
        .share_mode(0x1 | 0x2)
        .open(&path)
        .unwrap();
    assert!(matches!(
        store.apply(RegistryEdit::Forget(tracker.key)),
        Err(RegistryStoreError::Persistence(
            RegistryPersistenceError::Save { .. }
        ))
    ));
    assert_eq!(store.snapshot(), saved);
    assert_eq!(fs::read(&path).unwrap(), bytes);
    drop(lock);
    let mut changed = tracker.clone();
    changed.reported.firmware = Some("260923".into());
    observe(&mut store, vec![changed]);
    assert_eq!(
        store
            .snapshot()
            .get(&tracker.key)
            .unwrap()
            .reported
            .firmware
            .as_deref(),
        Some("260923")
    );
    store.apply(RegistryEdit::Forget(tracker.key)).unwrap();
    let forgotten = fs::read(&path).unwrap();
    let lock = fs::OpenOptions::new()
        .read(true)
        .share_mode(0x1 | 0x2)
        .open(&path)
        .unwrap();
    assert!(matches!(
        store.apply(RegistryEdit::Remember(tracker.clone())),
        Err(RegistryStoreError::Persistence(
            RegistryPersistenceError::Save { .. }
        ))
    ));
    observe(&mut store, vec![tracker.clone()]);
    assert_eq!(store.snapshot().get(&tracker.key), None);
    assert_eq!(fs::read(&path).unwrap(), forgotten);
    drop(lock);
    store
        .apply(RegistryEdit::Remember(tracker.clone()))
        .unwrap();
    assert_eq!(store.snapshot().get(&tracker.key).unwrap().nickname, None);
}

#[test]
fn suppression_capacity_fails_preserving_records_without_silent_eviction() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("device-registry.json");
    let mut store = RegistryStore::load(path.clone()).unwrap();
    for index in 0..MAX_RECORDS {
        let key = DeviceKey::tracker_address(&format!("AABB{index:08X}")).unwrap();
        store.apply(RegistryEdit::Forget(key)).unwrap();
    }
    let tracker = observed("112233445566");
    observe(&mut store, vec![tracker.clone()]);
    let saved = store.snapshot();
    let bytes = fs::read(&path).unwrap();
    assert!(matches!(
        store.apply(RegistryEdit::Forget(tracker.key)),
        Err(RegistryStoreError::Validation(
            RegistryValidationError::TooManyRecords
        ))
    ));
    assert_eq!(store.snapshot(), saved);
    assert_eq!(fs::read(&path).unwrap(), bytes);
    let first = DeviceKey::tracker_address("AABB00000000").unwrap();
    observe(&mut store, vec![observed(first.value())]);
    assert_eq!(store.snapshot().get(&first), None);
    store
        .apply(RegistryEdit::Observe {
            observations: Vec::new(),
            absent: BTreeSet::from([first]),
        })
        .unwrap();
    store.apply(RegistryEdit::Forget(tracker.key)).unwrap();
    assert_eq!(store.snapshot().get(&tracker.key), None);
    assert_eq!(
        RegistryStore::load(path)
            .unwrap()
            .snapshot()
            .records()
            .len(),
        0
    );
}

#[test]
fn legacy_records_preserve_all_metadata_when_last_receiver_is_first_saved() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("device-registry.json");
    let legacy = r#"{"version":1,"records":[{"key":{"kind":"slimenrf_tracker_rf_deviceaddr_v1","value":"112233445566"},"nickname":"Left foot","reported":{"name":"Original tracker","board":"board-1","target":"target-1","firmware":"260923"}}]}"#;
    fs::write(&path, legacy).unwrap();
    let mut store = RegistryStore::load(path.clone()).unwrap();
    let key = DeviceKey::tracker_address("112233445566").unwrap();
    assert_eq!(store.snapshot().get(&key).unwrap().last_receiver, None);
    assert_eq!(fs::read_to_string(&path).unwrap(), legacy);
    let receiver = DeviceKey::receiver_serial("0011223344556677").unwrap();
    observe(
        &mut store,
        vec![DeviceObservation {
            key,
            reported: ReportedMetadata::default(),
            last_receiver: Some(receiver),
        }],
    );
    let loaded = RegistryStore::load(path).unwrap().snapshot();
    let record = loaded.get(&key).unwrap();
    assert_eq!(record.nickname.as_deref(), Some("Left foot"));
    assert_eq!(record.reported.name.as_deref(), Some("Original tracker"));
    assert_eq!(record.reported.board.as_deref(), Some("board-1"));
    assert_eq!(record.reported.target.as_deref(), Some("target-1"));
    assert_eq!(record.reported.firmware.as_deref(), Some("260923"));
    assert_eq!(record.last_receiver, Some(receiver));
    assert_eq!(loaded.records().len(), 1);
}

#[test]
fn receiver_move_persists_and_unknown_evidence_coalesces_after_restart() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("device-registry.json");
    let mut store = RegistryStore::load(path.clone()).unwrap();
    let mut tracker = observed("112233445566");
    let first = DeviceKey::receiver_serial("0011223344556677").unwrap();
    let second = DeviceKey::receiver_serial("8899AABBCCDDEEFF").unwrap();
    tracker.last_receiver = Some(first);
    observe(&mut store, vec![tracker.clone()]);
    tracker.last_receiver = Some(second);
    observe(&mut store, vec![tracker.clone()]);
    let mut restarted = RegistryStore::load(path.clone()).unwrap();
    let saved = restarted.snapshot();
    let bytes = fs::read(&path).unwrap();
    assert_eq!(saved.get(&tracker.key).unwrap().last_receiver, Some(second));
    observe(&mut restarted, vec![tracker.clone()]);
    tracker.last_receiver = None;
    observe(&mut restarted, vec![tracker.clone()]);
    assert!(std::sync::Arc::ptr_eq(&restarted.snapshot(), &saved));
    assert_eq!(fs::read(&path).unwrap(), bytes);
    restarted.apply(RegistryEdit::Forget(tracker.key)).unwrap();
    tracker.last_receiver = Some(first);
    observe(&mut restarted, vec![tracker.clone()]);
    assert_eq!(restarted.snapshot().get(&tracker.key), None);
    restarted
        .apply(RegistryEdit::Remember(tracker.clone()))
        .unwrap();
    assert_eq!(
        restarted
            .snapshot()
            .get(&tracker.key)
            .unwrap()
            .last_receiver,
        Some(first)
    );
}

#[test]
fn invalid_saved_receiver_relations_do_not_replace_committed_state_or_file() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("device-registry.json");
    let mut store = RegistryStore::load(path.clone()).unwrap();
    let tracker = observed("112233445566");
    observe(&mut store, vec![tracker.clone()]);
    let saved = store.snapshot();
    let original: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let receiver = serde_json::json!({
        "kind": "slimenrf_receiver_usb_serial_v1",
        "value": "0011223344556677"
    });
    for mutation in 0..4 {
        let mut document = original.clone();
        document["records"][0]["last_receiver"] = receiver.clone();
        match mutation {
            0 => document["records"][0]["last_receiver"] = document["records"][0]["key"].clone(),
            1 => document["records"][0]["key"] = receiver.clone(),
            2 => document["records"][0]["last_receiver"]["value"] = "COM9".into(),
            _ => document["records"][0]["last_receiver"]["nickname"] = "Unsupported".into(),
        }
        let bytes = serde_json::to_vec(&document).unwrap();
        fs::write(&path, &bytes).unwrap();
        assert!(RegistryStore::load(path.clone()).is_err());
        assert!(matches!(
            store.apply(RegistryEdit::Rename {
                key: tracker.key,
                nickname: "Must not replace invalid data".into()
            }),
            Err(RegistryStoreError::Persistence(_))
        ));
        assert_eq!(store.snapshot(), saved);
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }
}

#[cfg(windows)]
#[test]
fn denied_receiver_move_preserves_previous_relation_until_atomic_save_succeeds() {
    use std::os::windows::fs::OpenOptionsExt;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("device-registry.json");
    let mut store = RegistryStore::load(path.clone()).unwrap();
    let mut tracker = observed("112233445566");
    tracker.last_receiver = DeviceKey::receiver_serial("0011223344556677");
    observe(&mut store, vec![tracker.clone()]);
    let saved = store.snapshot();
    let bytes = fs::read(&path).unwrap();
    let lock = fs::OpenOptions::new()
        .read(true)
        .share_mode(0x1 | 0x2)
        .open(&path)
        .unwrap();
    tracker.last_receiver = DeviceKey::receiver_serial("8899AABBCCDDEEFF");
    assert!(matches!(
        store.apply(RegistryEdit::Remember(tracker.clone())),
        Err(RegistryStoreError::Persistence(
            RegistryPersistenceError::Save { .. }
        ))
    ));
    assert_eq!(store.snapshot(), saved);
    assert_eq!(fs::read(&path).unwrap(), bytes);
    drop(lock);
    observe(&mut store, vec![tracker.clone()]);
    assert_eq!(
        RegistryStore::load(path)
            .unwrap()
            .snapshot()
            .get(&tracker.key)
            .unwrap()
            .last_receiver,
        DeviceKey::receiver_serial("8899AABBCCDDEEFF")
    );
}
