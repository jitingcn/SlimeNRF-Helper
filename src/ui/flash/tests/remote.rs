use super::support::{finish_internal_default, internal_default_state, local_image, target};
use crate::firmware_source::{Artifact, SourceChannel, Version};
use crate::ui::flash::{
    Action, FlashState, Mode, ota_assignment_key, ota_target_key, plan_ota_groups,
};
use std::sync::Arc;

#[test]
fn source_tabs_preserve_assignment_review_remote_cache_and_internal_default() {
    let (_directory, mut state) = internal_default_state();
    finish_internal_default(&mut state);
    let destination = state.destination(Mode::Esb).unwrap();
    let image = state.displayed_image(Mode::Esb).unwrap().clone();
    let key = state.active_group.clone().unwrap();
    let version = Version {
        channel: SourceChannel::TrackerCi,
        id: 42,
        name: "build".into(),
        published_at: String::new(),
        commit: None,
        url: "https://example.invalid/build".into(),
    };
    state.remote_destination = destination.clone();
    state.channel = version.channel;
    state.versions = vec![version.clone()];
    state.latest_version = Some((version.channel, version.id));
    state.selected_version = Some(version.id);
    state.artifacts = vec![Artifact {
        name: "board.zip".into(),
        variant: "Standard".into(),
        size: 512,
        version,
        url: "https://example.invalid/board.zip".into(),
        sha256: None,
        id: 7,
    }];
    state.images_destination = destination.clone();
    state.images = vec![image.clone(), local_image("alternative.hex")];
    state.review = Some(Action::EsbGroups(plan_ota_groups(&state).unwrap()));
    state.acknowledged = true;
    let generation = state.group_generations[&key];
    assert!(!state.ota_defaults[&key].explicit);
    assert!(!state.remote_view);
    for remote in [true, false, true] {
        state.show_remote(remote);
        assert_eq!(state.remote_view, remote);
        assert!(state.destination(Mode::Esb).as_ref() == Some(&destination));
        assert!(state.remote_destination == destination);
        assert_eq!(state.channel, SourceChannel::TrackerCi);
        assert_eq!(state.versions.len(), 1);
        assert_eq!(state.artifacts.len(), 1);
        assert_eq!(state.selected_version, Some(42));
        assert_eq!(state.images.len(), 2);
        assert!(state.images_destination == destination);
        assert_eq!(state.group_generations[&key], generation);
        assert!(!state.ota_defaults[&key].explicit);
        assert!(Arc::ptr_eq(
            &state.displayed_image(Mode::Esb).unwrap().bytes,
            &image.bytes
        ));
        assert!(state.review.is_some());
        assert!(state.acknowledged);
        assert!(state.is_latest(Mode::Esb, &state.versions[0]));
        assert!(!state.busy());
    }
}

#[test]
fn latest_badge_requires_confirmed_channel_destination_and_group_generation() {
    let a = target("a", 0, "one");
    let b = target("b", 0, "two");
    let a_key = ota_assignment_key(&a);
    let b_key = ota_assignment_key(&b);
    let mut state = FlashState {
        selected_targets: [ota_target_key(&a), ota_target_key(&b)].into(),
        targets: vec![a, b],
        ..FlashState::default()
    };
    state.reconcile_groups(Some(a_key.clone()));
    let destination = state.destination(Mode::Esb).unwrap();
    state.prepare_remote(Mode::Esb, &destination);
    let version = Version {
        channel: SourceChannel::Release,
        id: 42,
        name: "release".into(),
        published_at: String::new(),
        commit: None,
        url: "https://example.invalid/release".into(),
    };
    state.versions = vec![version.clone()];
    assert!(!state.is_latest(Mode::Esb, &version));
    state.apply_latest(Mode::Esb, destination.clone(), version.clone());
    assert!(state.is_latest(Mode::Esb, &version));
    let mut other = version.clone();
    other.id += 1;
    assert!(!state.is_latest(Mode::Esb, &other));
    state.select_channel(SourceChannel::TrackerCi);
    state.apply_latest(Mode::Esb, destination.clone(), version.clone());
    assert!(state.latest_version.is_none());
    state.select_channel(SourceChannel::Release);
    assert!(!state.is_latest(Mode::Esb, &version));
    state.apply_latest(Mode::Esb, destination.clone(), version.clone());
    state.reconcile_groups(Some(b_key));
    assert!(!state.is_latest(Mode::Esb, &version));
    state.apply_latest(Mode::Esb, destination.clone(), version.clone());
    assert!(state.latest_version.is_none());
    state.prepare_remote(Mode::Esb, &state.destination(Mode::Esb).unwrap());
    state.reconcile_groups(Some(a_key));
    assert!(!state.is_latest(Mode::Esb, &version));
    state.prepare_remote(Mode::Esb, &state.destination(Mode::Esb).unwrap());
    state.apply_latest(Mode::Esb, destination.clone(), version.clone());
    assert!(state.is_latest(Mode::Esb, &version));
    let selected = state.selected_targets.clone();
    state.selected_targets.clear();
    state.reconcile_groups(None);
    state.selected_targets = selected;
    state.reconcile_groups(None);
    state.prepare_remote(Mode::Esb, &state.destination(Mode::Esb).unwrap());
    state.apply_latest(Mode::Esb, destination, version.clone());
    assert!(!state.is_latest(Mode::Esb, &version));
}

#[test]
fn changing_version_clears_previous_artifacts_before_loading() {
    let version = Version {
        channel: SourceChannel::Release,
        id: 1,
        name: "version-a".into(),
        published_at: String::new(),
        commit: None,
        url: "https://example.invalid/a".into(),
    };
    let mut state = FlashState {
        selected_version: Some(version.id),
        artifacts: vec![Artifact {
            name: "version-a.uf2".into(),
            variant: "Standard".into(),
            size: 512,
            version,
            url: "https://example.invalid/a.uf2".into(),
            sha256: None,
            id: 10,
        }],
        ..FlashState::default()
    };
    state.select_version(2);
    assert_eq!(state.selected_version, Some(2));
    assert!(state.artifacts.is_empty());
    assert!(!state.busy());
}
