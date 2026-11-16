use super::support::local_image;
use crate::application::flash::{Action, FlashState};
use slimenrf_firmware_image::image::Uf2ImagePurpose;

#[test]
fn volume_refresh_prunes_selection_invalidates_review_and_preserves_outcomes() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("INFO_UF2.TXT"), "Board-ID: nRF52840-test\n").unwrap();
    let volume =
        crate::features::firmware::uf2::inspect_volume(dir.path(), "INFO_UF2.TXT").unwrap();
    let mut state = FlashState {
        volumes: vec![volume.clone()],
        selected_volumes: [volume.identity.clone()].into(),
        review: Some(Action::Reboot(vec![volume.clone()])),
        acknowledged: true,
        auto_seen: ["volume:digest".to_string()].into(),
        results: vec![(
            crate::application::flash::ResultTarget::generic("device".into(), "device".into()),
            crate::update::UpdateOutcome::Completed,
        )],
        last_progress: Some(crate::update::Progress::bytes("complete", 1, 1)),
        ..FlashState::default()
    };
    state.apply_volumes(vec![volume.clone()]);
    assert!(state.review.is_some());
    assert_eq!(state.selected_live_volumes().len(), 1);
    state.apply_volumes(Vec::new());
    assert!(state.selected_live_volumes().is_empty());
    assert!(state.selected_volumes.is_empty());
    assert!(state.review.is_none());
    assert!(!state.acknowledged);
    assert_eq!(state.results.len(), 1);
    assert_eq!(state.last_progress.as_ref().unwrap().completed, 1);
    assert!(state.auto_seen.contains("volume:digest"));
    state.apply_volumes(vec![volume]);
    assert!(state.selected_live_volumes().is_empty());
}

#[test]
fn replacing_a_volume_at_the_same_mount_cannot_reuse_a_review() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("INFO_UF2.TXT"), "Board-ID: nRF52840-test\n").unwrap();
    let old = crate::features::firmware::uf2::inspect_volume(dir.path(), "INFO_UF2.TXT").unwrap();
    let mut replacement = old.clone();
    replacement.identity.push_str("-replacement");
    let mut state = FlashState {
        selected_volumes: [old.identity.clone()].into(),
        review: Some(Action::Uf2(
            vec![old],
            local_image("image.uf2"),
            Uf2ImagePurpose::Application,
        )),
        ..FlashState::default()
    };
    state.apply_volumes(vec![replacement]);
    assert!(state.review.is_none());
    assert!(state.selected_live_volumes().is_empty());
}

#[test]
fn automatic_copy_never_selects_a_new_or_replaced_volume() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    for directory in [&first, &second] {
        std::fs::write(
            directory.path().join("INFO_UF2.TXT"),
            "Board-ID: nRF52840-test\n",
        )
        .unwrap();
    }
    let old = crate::features::firmware::uf2::inspect_volume(first.path(), "INFO_UF2.TXT").unwrap();
    let added =
        crate::features::firmware::uf2::inspect_volume(second.path(), "INFO_UF2.TXT").unwrap();
    let mut state = FlashState {
        volumes: vec![old.clone()],
        selected_volumes: [old.identity.clone()].into(),
        auto: true,
        auto_seen: [format!("{}:digest", old.identity)].into(),
        ..FlashState::default()
    };
    state.apply_volumes(vec![old.clone(), added.clone()]);
    assert_eq!(state.selected_volumes.len(), 1);
    assert!(state.selected_volumes.contains(&old.identity));
    assert!(!state.selected_volumes.contains(&added.identity));
    state.apply_volumes(vec![added]);
    state.apply_volumes(vec![old]);
    assert!(
        state.selected_volumes.is_empty(),
        "a reconnect requires a new selection"
    );
}
