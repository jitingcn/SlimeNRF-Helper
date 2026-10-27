use super::{MAX_STORE_BYTES, ShortcutEdit, ShortcutStore, ShortcutStoreError};
use crate::shortcuts::{
    CommandCardIcon, MoveDirection, SavedCommandDraft, SavedCommandId, TargetMode, ValidationReason,
};
use std::fs;

fn draft(title: &str) -> SavedCommandDraft {
    SavedCommandDraft {
        id: None,
        title: title.into(),
        description: "An optional description".into(),
        icon: Some(CommandCardIcon::Terminal),
        pinned: true,
        target_mode: TargetMode::ReceiverConsoles,
        commands_text: "status\ninfo\n".into(),
    }
}

fn save(store: &mut ShortcutStore, title: &str) -> SavedCommandId {
    store
        .apply(ShortcutEdit::Save(draft(title)))
        .unwrap()
        .unwrap()
}

#[test]
fn missing_store_loads_without_writing_and_crud_survives_reload() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("custom-shortcuts.json");
    let mut store = ShortcutStore::load(path.clone()).unwrap();
    assert!(store.snapshot().cards().is_empty());
    assert!(!path.exists());
    let first = save(&mut store, "First");
    let second = save(&mut store, "Second");
    let mut edit = draft("Renamed");
    edit.id = Some(first);
    edit.commands_text = "status".into();
    store.apply(ShortcutEdit::Save(edit)).unwrap();
    store
        .apply(ShortcutEdit::SetPinned {
            id: first,
            pinned: false,
        })
        .unwrap();
    store
        .apply(ShortcutEdit::Move {
            id: second,
            direction: MoveDirection::Up,
        })
        .unwrap();
    let loaded = ShortcutStore::load(path.clone()).unwrap().snapshot();
    assert_eq!(
        loaded
            .cards()
            .iter()
            .map(|card| card.id)
            .collect::<Vec<_>>(),
        vec![second, first]
    );
    assert_eq!(loaded.get(first).unwrap().title, "Renamed");
    assert!(!loaded.get(first).unwrap().pinned);
    assert_eq!(loaded.get(first).unwrap().commands_text(), "status");
    assert_eq!(
        loaded.get(second).unwrap().icon,
        Some(CommandCardIcon::Terminal)
    );
    store.apply(ShortcutEdit::Remove(first)).unwrap();
    let loaded = ShortcutStore::load(path).unwrap().snapshot();
    assert!(loaded.get(first).is_none());
    assert_eq!(loaded.cards().len(), 1);
}

#[test]
fn concurrent_external_edits_are_reloaded_before_mutation() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("custom-shortcuts.json");
    let mut first = ShortcutStore::load(path.clone()).unwrap();
    let original = save(&mut first, "Original");
    let mut second = ShortcutStore::load(path).unwrap();
    let external = save(&mut second, "External");
    first
        .apply(ShortcutEdit::SetPinned {
            id: original,
            pinned: false,
        })
        .unwrap();
    let snapshot = first.snapshot();
    assert_eq!(snapshot.cards().len(), 2);
    assert_eq!(snapshot.get(external).unwrap().title, "External");
    assert!(!snapshot.get(original).unwrap().pinned);
}

#[test]
fn invalid_draft_preserves_snapshot_and_disk_with_a_structured_error() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("custom-shortcuts.json");
    let mut store = ShortcutStore::load(path.clone()).unwrap();
    save(&mut store, "Original");
    let original = fs::read(&path).unwrap();
    let snapshot = store.snapshot();
    let mut input = draft("Uncommitted");
    input.commands_text = "status\n\ninfo".into();
    let Err(ShortcutStoreError::Validation(error)) = store.apply(ShortcutEdit::Save(input)) else {
        panic!("Invalid command framing was not rejected");
    };
    assert_eq!(error.line, Some(2));
    assert_eq!(error.reason, ValidationReason::BlankLine);
    assert_eq!(store.snapshot(), snapshot);
    assert_eq!(fs::read(path).unwrap(), original);
}

#[test]
fn damaged_or_unsupported_documents_are_never_overwritten() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("custom-shortcuts.json");
    let mut store = ShortcutStore::load(path.clone()).unwrap();
    save(&mut store, "Original");
    let snapshot = store.snapshot();
    for content in [
        "{broken",
        "null",
        r#"{"version":2,"next_id":1,"cards":[]}"#,
        r#"{"version":1,"next_id":0,"cards":[]}"#,
        r#"{"version":1,"next_id":1,"cards":[{}]}"#,
    ] {
        fs::write(&path, content).unwrap();
        assert!(ShortcutStore::load(path.clone()).is_err());
        assert!(matches!(
            store.apply(ShortcutEdit::Save(draft("Replacement"))),
            Err(ShortcutStoreError::Persistence(_))
        ));
        assert_eq!(store.snapshot(), snapshot);
        assert_eq!(fs::read_to_string(&path).unwrap(), content);
    }
}

#[test]
fn oversized_and_non_regular_documents_are_rejected() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("custom-shortcuts.json");
    let file = fs::File::create(&path).unwrap();
    file.set_len(MAX_STORE_BYTES as u64 + 1).unwrap();
    drop(file);
    assert!(ShortcutStore::load(path.clone()).is_err());
    assert_eq!(
        fs::metadata(path).unwrap().len(),
        MAX_STORE_BYTES as u64 + 1
    );
    assert!(ShortcutStore::load(directory.path().to_path_buf()).is_err());
}

#[test]
fn oversized_serialization_preserves_the_last_saved_library() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("custom-shortcuts.json");
    let mut store = ShortcutStore::load(path.clone()).unwrap();
    let commands = vec!["x".repeat(16_000); 4].join("\n");
    for index in 0..30 {
        let previous = store.snapshot();
        let previous_bytes = fs::read(&path).ok();
        let mut large = draft(&format!("Large {index}"));
        large.commands_text = commands.clone();
        match store.apply(ShortcutEdit::Save(large)) {
            Ok(Some(_)) => {}
            Err(ShortcutStoreError::Persistence(_)) => {
                assert_eq!(store.snapshot(), previous);
                assert_eq!(fs::read(path).ok(), previous_bytes);
                return;
            }
            other => panic!("Unexpected mutation result: {other:?}"),
        }
    }
    panic!("The serialized store limit was not enforced");
}

#[cfg(windows)]
#[test]
fn denied_atomic_replacement_keeps_the_previous_snapshot_and_disk() {
    use std::os::windows::fs::OpenOptionsExt;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("custom-shortcuts.json");
    let mut store = ShortcutStore::load(path.clone()).unwrap();
    save(&mut store, "Original");
    let snapshot = store.snapshot();
    let bytes = fs::read(&path).unwrap();
    let lock = fs::OpenOptions::new()
        .read(true)
        .share_mode(0x1 | 0x2)
        .open(&path)
        .unwrap();
    let input = draft("Uncommitted");
    assert!(matches!(
        store.apply(ShortcutEdit::Save(input.clone())),
        Err(ShortcutStoreError::Persistence(_))
    ));
    assert_eq!(store.snapshot(), snapshot);
    assert_eq!(fs::read(&path).unwrap(), bytes);
    drop(lock);
    store.apply(ShortcutEdit::Save(input)).unwrap();
    assert_eq!(
        ShortcutStore::load(path).unwrap().snapshot().cards().len(),
        2
    );
}

#[test]
fn loading_checks_model_invariants_instead_of_trusting_serde() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("custom-shortcuts.json");
    let mut store = ShortcutStore::load(path.clone()).unwrap();
    save(&mut store, "Original");
    let snapshot = store.snapshot();
    let original: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(original["version"], 1);
    for mutation in 0..3 {
        let mut invalid = original.clone();
        match mutation {
            0 => {
                let duplicate = invalid["cards"][0].clone();
                invalid["cards"].as_array_mut().unwrap().push(duplicate);
            }
            1 => invalid["cards"][0]["lines"][0] = "status\nreboot".into(),
            _ => invalid["next_id"] = 0.into(),
        }
        let bytes = serde_json::to_vec(&invalid).unwrap();
        fs::write(&path, &bytes).unwrap();
        assert!(ShortcutStore::load(path.clone()).is_err());
        assert!(
            store
                .apply(ShortcutEdit::Save(draft("Replacement")))
                .is_err()
        );
        assert_eq!(store.snapshot(), snapshot);
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }
}

#[test]
fn valid_no_op_edits_refresh_the_snapshot_from_another_store() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("custom-shortcuts.json");
    let mut displayed = ShortcutStore::load(path.clone()).unwrap();
    let first = save(&mut displayed, "First");
    let second = save(&mut displayed, "Second");
    let mut external = ShortcutStore::load(path.clone()).unwrap();

    external.apply(ShortcutEdit::Remove(first)).unwrap();
    let bytes = fs::read(&path).unwrap();
    displayed.apply(ShortcutEdit::Remove(first)).unwrap();
    assert_eq!(displayed.snapshot(), external.snapshot());
    assert!(displayed.snapshot().get(first).is_none());
    assert_eq!(fs::read(&path).unwrap(), bytes);

    external
        .apply(ShortcutEdit::SetPinned {
            id: second,
            pinned: false,
        })
        .unwrap();
    let bytes = fs::read(&path).unwrap();
    displayed
        .apply(ShortcutEdit::SetPinned {
            id: second,
            pinned: false,
        })
        .unwrap();
    assert_eq!(displayed.snapshot(), external.snapshot());
    assert!(!displayed.snapshot().get(second).unwrap().pinned);
    assert_eq!(fs::read(&path).unwrap(), bytes);

    let third = save(&mut external, "Third");
    external
        .apply(ShortcutEdit::Move {
            id: third,
            direction: MoveDirection::Up,
        })
        .unwrap();
    let bytes = fs::read(&path).unwrap();
    displayed
        .apply(ShortcutEdit::Move {
            id: third,
            direction: MoveDirection::Up,
        })
        .unwrap();
    assert_eq!(displayed.snapshot(), external.snapshot());
    assert_eq!(displayed.snapshot().cards()[0].title, "Third");
    assert_eq!(fs::read(&path).unwrap(), bytes);
}
