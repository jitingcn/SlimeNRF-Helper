use super::{
    Action, FlashState, ImageCategory, ImageDestination, Mode, Payload, artifact_firmware_hints,
    firmware_filter_matches, firmware_page_matches, make_image, ota_target_key, validate_ota_image,
};
use crate::ui::{FeaturePage, Workbench};
use crate::{
    board_catalog::{Catalog, FirmwareHint},
    firmware_source::{Artifact, SourceChannel, Version, is_bootloader_update},
};
use gpui_kit::{
    AnyWindowHandle, AppContext as _, Bounds, Entity, Role, TestAppContext, Window, WindowBounds,
    WindowOptions, px, size, test::TestWindowExt as _,
};
use std::{
    collections::{BTreeSet, HashMap},
    sync::Arc,
    time::{Duration, Instant},
};

fn version(channel: SourceChannel, id: u64) -> Version {
    Version {
        channel,
        id,
        name: "0.11.1-slimenrf".into(),
        published_at: "2026-10-08T19:40:32Z".into(),
        commit: None,
        url: "https://github.com/jitingcn/Adafruit_nRF52_Bootloader/releases/tag/0.11.1-slimenrf"
            .into(),
    }
}

fn artifact(channel: SourceChannel, id: u64) -> Artifact {
    Artifact { name: "update-slimenrf_promicro_bootloader-0.11.1-slimenrf_nosd.uf2".into(), variant: "Bootloader".into(),
        size: 74752, version: version(channel, id), url: "https://github.com/jitingcn/Adafruit_nRF52_Bootloader/releases/download/0.11.1-slimenrf/update-slimenrf_promicro_bootloader-0.11.1-slimenrf_nosd.uf2".into(),
        sha256: Some("f25b3d0b9dfdc6a52c5e07c94be28e6571558f11a88fcdfe6bc094f1ab3f2d27".into()), id: 622822338 }
}

#[test]
fn bootloader_category_reconciles_all_remote_caches_and_restores_application_channel() {
    let mut state = FlashState::default();
    state.prepare_remote(Mode::Uf2, &ImageDestination::Global);
    state.select_channel(SourceChannel::TrackerCi);
    state.versions = vec![version(SourceChannel::TrackerCi, 42)];
    state.artifacts = vec![artifact(SourceChannel::TrackerCi, 42)];
    state.selected_version = Some(42);
    state.latest_version = Some((SourceChannel::TrackerCi, 42));
    state.variant_filter = Some("Standard".into());
    state.review = Some(Action::Reboot(vec![]));
    state.acknowledged = true;
    state.auto = true;
    state.auto_requested = true;
    state.select_category(ImageCategory::Bootloader);
    assert_eq!(
        state.source_channel(Mode::Uf2),
        SourceChannel::BootloaderRelease
    );
    assert_eq!(state.source_channel(Mode::Esb), SourceChannel::TrackerCi);
    assert_eq!(
        state.source_channel(Mode::OpenDfu),
        SourceChannel::TrackerCi
    );
    assert!(state.versions.is_empty() && state.artifacts.is_empty());
    assert!(state.latest_version.is_none() && state.selected_version.is_none());
    assert!(state.variant_filter.is_none());
    assert!(state.review.is_none() && !state.acknowledged && !state.auto && !state.auto_requested);
    state.select_channel(SourceChannel::ReceiverCi);
    assert_eq!(
        state.source_channel(Mode::Uf2),
        SourceChannel::BootloaderRelease
    );
    state.select_channel(SourceChannel::BootloaderCi);
    assert_eq!(state.source_channel(Mode::Uf2), SourceChannel::BootloaderCi);
    assert_eq!(state.application_channel, SourceChannel::TrackerCi);
    assert_eq!(
        state.source_channel(Mode::OpenDfu),
        SourceChannel::TrackerCi
    );
    state.select_category(ImageCategory::Application);
    assert_eq!(state.source_channel(Mode::Uf2), SourceChannel::TrackerCi);
    assert!(state.versions.is_empty() && state.artifacts.is_empty());
    for channel in [
        SourceChannel::BootloaderRelease,
        SourceChannel::BootloaderCi,
    ] {
        state.select_channel(channel);
        assert_eq!(state.source_channel(Mode::Uf2), SourceChannel::TrackerCi);
    }
    state.select_category(ImageCategory::Bootloader);
    assert_eq!(
        state.source_channel(Mode::Uf2),
        SourceChannel::BootloaderRelease
    );
}

#[test]
fn category_round_trip_rejects_late_application_payloads_without_revoking_new_review() {
    let mut state = FlashState::default();
    state.prepare_remote(Mode::Uf2, &ImageDestination::Global);
    let old = state.remote_request(Mode::Uf2);
    state.select_category(ImageCategory::Bootloader);
    state.select_category(ImageCategory::Application);
    let chosen = make_image(
        "chosen.hex".into(),
        b":0410000001020304E2\n:00000001FF\n".to_vec(),
        "manual".into(),
        "chosen-digest".into(),
        true,
    );
    state.image = Some(chosen);
    state.review = Some(Action::Reboot(vec![]));
    state.acknowledged = true;
    state.apply_remote(
        Mode::Uf2,
        Payload::Versions(old.clone(), vec![version(SourceChannel::Release, 42)]),
    );
    state.apply_remote(
        Mode::Uf2,
        Payload::Latest(old.clone(), version(SourceChannel::Release, 42)),
    );
    state.apply_remote(
        Mode::Uf2,
        Payload::Artifacts(old.clone(), vec![artifact(SourceChannel::Release, 42)]),
    );
    state.apply_remote(Mode::Uf2, Payload::RemoteImages(old, vec![]));
    assert!(state.versions.is_empty() && state.artifacts.is_empty());
    assert!(state.latest_version.is_none());
    assert_eq!(state.image.as_ref().unwrap().digest, "chosen-digest");
    assert!(state.review.is_some() && state.acknowledged);
}

#[test]
fn current_bootloader_latest_and_artifacts_are_admitted_but_other_source_is_not() {
    let mut state = FlashState::default();
    state.select_category(ImageCategory::Bootloader);
    state.prepare_remote(Mode::Uf2, &ImageDestination::Global);
    let latest = version(SourceChannel::BootloaderRelease, 42);
    let request = state.remote_request(Mode::Uf2);
    state.apply_remote(
        Mode::Uf2,
        Payload::Latest(request.clone(), version(SourceChannel::Release, 42)),
    );
    assert!(state.versions.is_empty());
    state.apply_remote(Mode::Uf2, Payload::Latest(request, latest.clone()));
    assert!(state.is_latest(Mode::Uf2, &latest));
    assert!(!state.is_latest(Mode::OpenDfu, &latest));
    state.select_version(42);
    let request = state.remote_request(Mode::Uf2);
    state.apply_remote(
        Mode::Uf2,
        Payload::Artifacts(request.clone(), vec![artifact(SourceChannel::Release, 42)]),
    );
    assert!(state.artifacts.is_empty());
    state.apply_remote(
        Mode::Uf2,
        Payload::Artifacts(
            request,
            vec![artifact(SourceChannel::BootloaderRelease, 42)],
        ),
    );
    assert_eq!(state.artifacts.len(), 1);
}

#[test]
fn changed_version_source_and_update_method_reject_original_async_request() {
    let mut state = FlashState::default();
    state.prepare_remote(Mode::Uf2, &ImageDestination::Global);
    state.select_version(42);
    let original = state.remote_request(Mode::Uf2);
    assert!(!state.accepts_remote(Mode::OpenDfu, &original));
    state.select_version(43);
    assert!(!state.accepts_remote(Mode::Uf2, &original));
    state.select_version(42);
    assert!(!state.accepts_remote(Mode::Uf2, &original));
    let before_source = state.remote_request(Mode::Uf2);
    state.select_channel(SourceChannel::TrackerCi);
    state.select_channel(SourceChannel::Release);
    assert!(!state.accepts_remote(Mode::Uf2, &before_source));
    let before_mode = state.remote_request(Mode::Uf2);
    state.transition_mode(Mode::OpenDfu);
    state.transition_mode(Mode::Uf2);
    assert!(!state.accepts_remote(Mode::Uf2, &before_mode));
}

#[test]
fn invalid_selected_configuration_never_authorizes_default_remote_repositories() {
    let mut state = FlashState::default();
    assert!(state.remote_sources().is_none());
    state.catalog_loaded = true;
    state.catalog = Some(Catalog::default());
    state.catalog_error = Some("Invalid selected configuration".into());
    assert!(state.remote_sources().is_none());
    state.catalog_error = None;
    let sources = state.remote_sources().unwrap();
    assert!(std::sync::Arc::ptr_eq(
        &sources,
        &state.catalog.as_ref().unwrap().remote_firmware
    ));
}

#[test]
fn bootloader_remote_eligibility_never_promotes_hex_zip_or_unknown_board() {
    assert!(is_bootloader_update(
        "update-slimenrf_promicro_bootloader-0.11.1_nosd.uf2"
    ));
    for name in [
        "slimenrf_promicro_bootloader-0.11.1_nosd.uf2",
        "update-slimenrf_promicro_bootloader-0.11.1_nosd.hex",
        "update-slimenrf_promicro_bootloader-0.11.1_nosd.zip",
        "update-mcuboot.uf2",
        "firmware.uf2",
    ] {
        assert!(!is_bootloader_update(name), "{name}");
    }
    let targets: [(String, Option<crate::board_catalog::EsbOtaKind>); 1] =
        [("promicro_uf2/nrf52840/spi".into(), None)];
    let known = [FirmwareHint {
        board_target: targets[0].0.clone(),
        kind: None,
        variant: "Bootloader".into(),
        mcuboot: false,
        bootloader: true,
    }];
    assert!(firmware_filter_matches(
        &known,
        "Bootloader",
        &targets,
        true,
        None
    ));
    assert!(!firmware_filter_matches(
        &[],
        "Bootloader",
        &targets,
        true,
        None
    ));
    let unknown = [FirmwareHint {
        board_target: "unconfigured-board".into(),
        ..known[0].clone()
    }];
    assert!(!firmware_filter_matches(
        &unknown,
        "Bootloader",
        &targets,
        true,
        None
    ));
}

#[test]
fn remote_worker_finishes_and_joins_after_category_change_without_installing_old_result() {
    use crate::update::Job;
    use std::{
        sync::mpsc,
        thread,
        time::{Duration, Instant},
    };
    let mut state = FlashState::default();
    state.prepare_remote(Mode::Uf2, &ImageDestination::Global);
    let request = state.remote_request(Mode::Uf2);
    let (release, wait) = mpsc::channel();
    state.job = Some(
        Job::start(move |_, _| {
            wait.recv().unwrap();
            Ok(Payload::Versions(
                request,
                vec![version(SourceChannel::Release, 42)],
            ))
        })
        .unwrap(),
    );
    state.select_category(ImageCategory::Bootloader);
    assert!(state.job.is_some());
    assert!(state.busy());
    release.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let payload = loop {
        if let Some(result) = state.job.as_mut().unwrap().poll() {
            break result.unwrap();
        }
        assert!(Instant::now() < deadline, "remote worker did not finish");
        thread::sleep(Duration::from_millis(1));
    };
    state.job = None;
    state.apply_remote(Mode::Uf2, payload);
    assert!(!state.busy());
    assert!(state.versions.is_empty());
    assert_eq!(
        state.source_channel(Mode::Uf2),
        SourceChannel::BootloaderRelease
    );
}

#[test]
fn bootloader_release_ci_switches_reject_stale_results_and_accept_only_current_channel() {
    for (previous, current) in [
        (
            SourceChannel::BootloaderRelease,
            SourceChannel::BootloaderCi,
        ),
        (
            SourceChannel::BootloaderCi,
            SourceChannel::BootloaderRelease,
        ),
    ] {
        let mut state = FlashState::default();
        state.select_category(ImageCategory::Bootloader);
        state.prepare_remote(Mode::Uf2, &ImageDestination::Global);
        state.select_channel(previous);
        state.select_version(42);
        let stale = state.remote_request(Mode::Uf2);
        state.versions = vec![version(previous, 42)];
        state.artifacts = vec![artifact(previous, 42)];
        state.latest_version = Some((previous, 42));

        state.select_channel(current);
        assert!(state.versions.is_empty() && state.artifacts.is_empty());
        assert!(state.selected_version.is_none() && state.latest_version.is_none());
        state.select_version(42);
        state.review = Some(Action::Reboot(vec![]));
        state.acknowledged = true;
        state.apply_remote(
            Mode::Uf2,
            Payload::Versions(stale.clone(), vec![version(previous, 42)]),
        );
        state.apply_remote(
            Mode::Uf2,
            Payload::Latest(stale.clone(), version(previous, 42)),
        );
        state.apply_remote(
            Mode::Uf2,
            Payload::Artifacts(stale.clone(), vec![artifact(previous, 42)]),
        );
        state.apply_remote(Mode::Uf2, Payload::RemoteImages(stale, vec![]));
        assert!(state.versions.is_empty() && state.artifacts.is_empty());
        assert!(state.latest_version.is_none());
        assert!(state.review.is_some() && state.acknowledged);

        let request = state.remote_request(Mode::Uf2);
        state.apply_remote(
            Mode::Uf2,
            Payload::Latest(request.clone(), version(previous, 42)),
        );
        assert!(state.versions.is_empty());
        state.apply_remote(Mode::Uf2, Payload::Latest(request, version(current, 42)));
        assert!(state.is_latest(Mode::Uf2, &version(current, 42)));
        assert!(!state.is_latest(Mode::OpenDfu, &version(current, 42)));
        state.select_version(42);
        let request = state.remote_request(Mode::Uf2);
        state.apply_remote(
            Mode::Uf2,
            Payload::Artifacts(request.clone(), vec![artifact(previous, 42)]),
        );
        assert!(state.artifacts.is_empty());
        state.apply_remote(
            Mode::Uf2,
            Payload::Artifacts(request, vec![artifact(current, 42)]),
        );
        assert_eq!(state.artifacts.len(), 1);
        let before_category = state.remote_request(Mode::Uf2);
        state.select_category(ImageCategory::Application);
        state.select_category(ImageCategory::Bootloader);
        assert!(!state.accepts_remote(Mode::Uf2, &before_category));
        assert_eq!(
            state.source_channel(Mode::Uf2),
            SourceChannel::BootloaderRelease
        );
    }
}

fn bootloader_ci_catalog() -> Catalog {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("slimenrf-helper.yaml");
    std::fs::write(
        &config,
        br#"{"schema_version":1,"firmware_root":".","board_targets":{
            "shared":{"bootloader_board":"slimenrf_promicro","bootloader_mcu":"nrf52840","targets":[
                {"name":"board/spi","firmware":"tracker.uf2","esb_ota_kind":"tracker"},
                {"name":"board/i2c","firmware":"receiver.uf2","esb_ota_kind":"receiver"}
            ]},
            "other":{"bootloader_board":"other_board","bootloader_mcu":"nrf52840",
                "name":"other/board","firmware":"other.uf2"}
        }}"#,
    )
    .unwrap();
    Catalog::load(&config).unwrap()
}

#[test]
fn bootloader_ci_exact_board_container_is_visible_only_for_uf2_bootloader() {
    let catalog = bootloader_ci_catalog();
    let mut artifact = artifact(SourceChannel::BootloaderCi, 42);
    artifact.name = "slimenrf_promicro".into();
    let hints = artifact_firmware_hints(&catalog, &artifact);
    assert_eq!(
        hints,
        catalog.match_filename("update-slimenrf_promicro_bootloader-main_nosd.uf2")
    );
    assert_eq!(hints.len(), 2);
    assert!(hints.iter().all(|hint| {
        hint.bootloader && hint.kind.is_none() && !hint.mcuboot && hint.variant == "Bootloader"
    }));
    let targets = [(
        "board/spi".into(),
        Some(crate::board_catalog::EsbOtaKind::Tracker),
    )];
    assert!(firmware_filter_matches(
        &hints,
        &artifact.variant,
        &targets,
        true,
        None
    ));
    for mode in [Mode::Uf2, Mode::OpenDfu, Mode::Esb, Mode::Ble] {
        for category in [ImageCategory::Application, ImageCategory::Bootloader] {
            assert_eq!(
                firmware_page_matches(
                    mode,
                    category,
                    &artifact.name,
                    &hints,
                    std::iter::empty(),
                    true,
                ),
                mode == Mode::Uf2 && category == ImageCategory::Bootloader,
            );
        }
    }
    assert!(!firmware_filter_matches(
        &hints,
        &artifact.variant,
        &[("other/board".into(), None)],
        true,
        None
    ));
    for name in [
        "unknown_board",
        "SLIMENRF_PROMICRO",
        "slimenrf_promicro.zip",
        "path/slimenrf_promicro",
    ] {
        artifact.name = name.into();
        let unknown = artifact_firmware_hints(&catalog, &artifact);
        assert!(unknown.is_empty(), "{name}");
        assert!(!firmware_page_matches(
            Mode::Uf2,
            ImageCategory::Bootloader,
            &artifact.name,
            &unknown,
            std::iter::empty(),
            true,
        ));
    }
    artifact.name = "update-slimenrf_promicro_bootloader-main_nosd.uf2".into();
    assert!(artifact_firmware_hints(&catalog, &artifact).is_empty());
    artifact.name = "other_board".into();
    let other = artifact_firmware_hints(&catalog, &artifact);
    assert_eq!(other.len(), 1);
    assert!(!firmware_filter_matches(
        &other,
        &artifact.variant,
        &targets,
        true,
        None
    ));
    artifact.name = "slimenrf_promicro".into();
    for channel in [
        SourceChannel::Release,
        SourceChannel::BootloaderRelease,
        SourceChannel::TrackerCi,
        SourceChannel::ReceiverCi,
    ] {
        artifact.version.channel = channel;
        assert!(artifact_firmware_hints(&catalog, &artifact).is_empty());
    }
    assert!(catalog.match_filename("slimenrf_promicro").is_empty());
}

#[test]
fn bootloader_ci_member_identity_is_reclassified_instead_of_inheriting_container() {
    let catalog = bootloader_ci_catalog();
    let mut artifact = artifact(SourceChannel::BootloaderCi, 42);
    artifact.name = "slimenrf_promicro".into();
    let advertised = artifact_firmware_hints(&catalog, &artifact);
    let mut state = FlashState {
        catalog: Some(catalog),
        ..FlashState::default()
    };
    state.select_category(ImageCategory::Bootloader);
    let targets = [("board/spi".into(), None)];
    for (name, matches) in [
        ("update-unknown_bootloader-main_nosd.uf2", false),
        ("update-other_board_bootloader-main_nosd.uf2", false),
        ("update-slimenrf_promicro_bootloader-main_nosd.uf2", true),
    ] {
        let mut member = make_image(
            name.into(),
            vec![],
            "CI archive member".into(),
            "fixture-digest".into(),
            false,
        );
        member.hints = advertised.clone().into();
        state.apply_images(ImageDestination::Global, vec![member]);
        let actual = &state.images[0].hints;
        assert_eq!(
            actual.as_ref(),
            state.catalog.as_ref().unwrap().match_filename(name),
            "{name}"
        );
        assert_eq!(
            firmware_filter_matches(actual, "Bootloader", &targets, true, None),
            matches,
            "{name}"
        );
        if !matches {
            assert_ne!(actual.as_ref(), advertised, "{name}");
        }
    }
}

fn chrysalis_ci_artifact() -> Artifact {
    Artifact {
        name: "SlimeNRF_ProMicro_Chrysalis_Tracker".into(),
        variant: "Standard".into(),
        size: 753283,
        version: Version {
            channel: SourceChannel::TrackerCi,
            id: 39204764885,
            name: "#381 feat(boards): add sensor power and oscillator GPIOs to xiao nRF54L15".into(),
            published_at: "2026-10-19T05:28:45Z".into(),
            commit: Some("74e3bd79f16c59e23d1e5d681c8ebed89103a015".into()),
            url: "https://github.com/jitingcn/SlimeVR-Tracker-nRF/actions/runs/39204764885".into(),
        },
        url: "https://nightly.link/jitingcn/SlimeVR-Tracker-nRF/actions/runs/39204764885/SlimeNRF_ProMicro_Chrysalis_Tracker.zip".into(),
        sha256: Some("97714454b58227fa92f848369ff176193644828617efb59c3e48de4fb3e71d05".into()),
        id: 12164658909,
    }
}

fn chrysalis_tracker() -> crate::esb_ota::Target {
    crate::esb_ota::Target {
        receiver: crate::domain::ReceiverEndpoint {
            key: "receiver".into(),
            path: b"receiver".to_vec(),
            label: "receiver".into(),
            serial_number: None,
        },
        id: 0,
        address: Some("112233445500".into()),
        info: crate::esb_ota::FirmwareInfo {
            version: "1".into(),
            build_date: "2026-10-19".into(),
            firmware_size: 4,
            bootloader: "adafruit_uf2".into(),
            protocol_version: 1,
            board_target: "promicro_uf2/nrf52840/chrysalis".into(),
            flash_base: 0x1000,
        },
    }
}

#[test]
fn tracker_ci_chrysalis_container_survives_matching_board_filter() {
    use crate::board_catalog::EsbOtaKind;

    let catalog = Catalog::load_embedded().unwrap();
    let artifact = chrysalis_ci_artifact();
    let tracker = chrysalis_tracker();
    let targets = [(
        "promicro_uf2/nrf52840/chrysalis".into(),
        Some(EsbOtaKind::Tracker),
    )];
    let hints = artifact_firmware_hints(&catalog, &artifact);

    assert_eq!(
        hints,
        [FirmwareHint {
            board_target: "promicro_uf2/nrf52840/chrysalis".into(),
            kind: Some(EsbOtaKind::Tracker),
            variant: "Standard".into(),
            mcuboot: false,
            bootloader: false,
        }]
    );
    assert!(firmware_page_matches(
        Mode::Esb,
        ImageCategory::Application,
        &artifact.name,
        &hints,
        std::iter::once(&tracker),
        true,
    ));
    assert!(
        firmware_filter_matches(&hints, &artifact.variant, &targets, true, Some("Standard"),),
        "Tracker CI run 381 must advertise its exact Chrysalis firmware"
    );

    let mut other = artifact.clone();
    other.name = "SlimeNRF_ProMicro_StackedSmol_Tracker_SPI".into();
    other.id = 12165366208;
    other.size = 752326;
    other.sha256 = Some("ae0d8f4c8231130d841cffbf9ca678a6da71cb74af9e24fb49ac15edb782fc4f".into());
    other.url = "https://nightly.link/jitingcn/SlimeVR-Tracker-nRF/actions/runs/39204764885/SlimeNRF_ProMicro_StackedSmol_Tracker_SPI.zip".into();
    let other_hints = artifact_firmware_hints(&catalog, &other);
    assert_eq!(other_hints[0].board_target, "promicro_uf2/nrf52840/spi");
    assert_eq!(other_hints.len(), 1);
    assert!(firmware_filter_matches(
        &other_hints,
        "Standard",
        &[(
            "promicro_uf2/nrf52840/spi".into(),
            Some(EsbOtaKind::Tracker)
        )],
        true,
        Some("Standard"),
    ));
    assert!(!firmware_filter_matches(
        &other_hints,
        "Standard",
        &targets,
        true,
        Some("Standard"),
    ));
    assert!(!firmware_page_matches(
        Mode::Esb,
        ImageCategory::Application,
        &other.name,
        &other_hints,
        std::iter::once(&tracker),
        true,
    ));
    assert!(!firmware_filter_matches(
        &hints,
        "Standard",
        &[(
            "promicro_uf2/nrf52840/chrysalis".into(),
            Some(EsbOtaKind::Receiver)
        )],
        true,
        Some("Standard"),
    ));
    assert!(!firmware_filter_matches(
        &hints,
        "Standard",
        &targets,
        true,
        Some("DataCollect CDC"),
    ));
    let mut signed = tracker;
    signed.info.bootloader = "mcuboot".into();
    assert!(!firmware_page_matches(
        Mode::Esb,
        ImageCategory::Application,
        &artifact.name,
        &hints,
        std::iter::once(&signed),
        true,
    ));
}

#[test]
fn ci_advertisements_preserve_complete_stems_and_release_filename_rules() {
    let catalog = Catalog::load_embedded().unwrap();
    let mut artifact = chrysalis_ci_artifact();
    let expected = catalog.match_filename("SlimeNRF_ProMicro_Chrysalis_Tracker.uf2");
    for name in [
        "SlimeNRF_ProMicro_Chrysalis_Tracker-1.2.3",
        "SlimeNRF_ProMicro_Chrysalis_Tracker_v1.2.3-rc.1",
        "SlimeNRF_ProMicro_Chrysalis_Tracker.uf2",
        "SlimeNRF_ProMicro_Chrysalis_Tracker.hex",
    ] {
        artifact.name = name.into();
        assert_eq!(
            artifact_firmware_hints(&catalog, &artifact),
            expected,
            "{name}"
        );
    }
    for name in [
        "SlimeNRF_ProMicro_Chrysalis_Tracker.txt",
        "SlimeNRF_ProMicro_Chrysalis_Tracker.exe",
        "SlimeNRF_ProMicro_Chrysalis_Tracker.uf2.zip",
        "SlimeNRF_ProMicro_Chrysalis_Tracker.bin",
        "SlimeNRF_ProMicro_Chrysalis_Tracker_update",
        "SlimeNRF_ProMicro_Chrysalis_Tracker-1.2.3-Pro",
        "SlimeNRF_ProMicro_Chrysalis_Tracker_Unknown",
        "Unknown_Tracker",
    ] {
        artifact.name = name.into();
        assert!(
            artifact_firmware_hints(&catalog, &artifact).is_empty(),
            "{name}"
        );
    }
    artifact.name = "SlimeNRF_ProMicro_Chrysalis_Tracker_DataCollect_CDC".into();
    let category_hints = artifact_firmware_hints(&catalog, &artifact);
    assert_eq!(category_hints[0].variant, "DataCollect CDC");
    assert!(!firmware_page_matches(
        Mode::Esb,
        ImageCategory::Application,
        &artifact.name,
        &category_hints,
        std::iter::once(&chrysalis_tracker()),
        true,
    ));
    artifact.version.channel = SourceChannel::ReceiverCi;
    artifact.name = "SlimeNRF_ProMicro_Chrysalis_Tracker".into();
    assert_eq!(artifact_firmware_hints(&catalog, &artifact), expected);
    for channel in [SourceChannel::Release, SourceChannel::BootloaderRelease] {
        artifact.version.channel = channel;
        artifact.name = "SlimeNRF_ProMicro_Chrysalis_Tracker".into();
        assert!(artifact_firmware_hints(&catalog, &artifact).is_empty());
        artifact.name.push_str(".uf2");
        assert_eq!(artifact_firmware_hints(&catalog, &artifact), expected);
    }
    assert!(
        catalog
            .match_filename("SlimeNRF_ProMicro_Chrysalis_Tracker")
            .is_empty()
    );
}

#[test]
fn ci_container_hints_cannot_authorize_unknown_or_other_board_members() {
    let catalog = Catalog::load_embedded().unwrap();
    let advertised = artifact_firmware_hints(&catalog, &chrysalis_ci_artifact());
    let tracker = chrysalis_tracker();
    let mut state = FlashState {
        catalog: Some(catalog),
        targets: vec![tracker.clone()],
        selected_targets: [ota_target_key(&tracker)].into(),
        ..FlashState::default()
    };
    state.reconcile_groups(None);
    let destination = state.destination(Mode::Esb).unwrap();

    for name in [
        "Unknown_Tracker.hex",
        "SlimeNRF_ProMicro_StackedSmol_Tracker_SPI.hex",
        "SlimeNRF_ProMicro_Chrysalis_Tracker.hex",
    ] {
        let mut member = make_image(
            name.into(),
            b":0410000001020304E2\n:00000001FF\n".to_vec(),
            "CI archive member".into(),
            "fixture-digest".into(),
            false,
        );
        member.hints = advertised.clone().into();
        state.apply_images(destination.clone(), vec![member]);
        if name == "SlimeNRF_ProMicro_Chrysalis_Tracker.hex" {
            assert_eq!(state.displayed_image(Mode::Esb).unwrap().name, name);
            assert_eq!(validate_ota_image(&state.images[0], &tracker), Ok(()));
        } else {
            assert!(state.displayed_image(Mode::Esb).is_none(), "{name}");
            assert!(
                validate_ota_image(&state.images[0], &tracker).is_err(),
                "{name}"
            );
        }
    }
}

#[test]
fn ci_advertisement_deduplication_keeps_distinct_roles() {
    use crate::board_catalog::EsbOtaKind;

    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("slimenrf-helper.yaml");
    std::fs::write(
        &config,
        br#"{"schema_version":1,"firmware_root":".","application_targets":{
            "tracker":{"name":"custom/board","esb_ota_kind":"tracker","firmware_aliases":["Shared_Application.uf2"]},
            "receiver":{"name":"custom/board","esb_ota_kind":"receiver","firmware_aliases":["Shared_Application.hex"]}
        }}"#,
    )
    .unwrap();
    let catalog = Catalog::load(&config).unwrap();
    let mut artifact = chrysalis_ci_artifact();
    artifact.name = "Shared_Application".into();
    assert_eq!(
        artifact_firmware_hints(&catalog, &artifact),
        [
            FirmwareHint {
                board_target: "custom/board".into(),
                kind: Some(EsbOtaKind::Tracker),
                variant: "Standard".into(),
                mcuboot: false,
                bootloader: false,
            },
            FirmwareHint {
                board_target: "custom/board".into(),
                kind: Some(EsbOtaKind::Receiver),
                variant: "Standard".into(),
                mcuboot: false,
                bootloader: false,
            },
        ]
    );
}

#[test]
fn current_fox_ci_advertisements_preserve_board_role_and_variant_filters() {
    use crate::board_catalog::EsbOtaKind;

    let catalog = Catalog::load_embedded().unwrap();
    for (name, board, kind, variant, channel, older_board) in [
        (
            "FoxApplication_FoxSnackLiteV3_Tracker",
            "foxsnacklitev3_uf2/nrf52840",
            EsbOtaKind::Tracker,
            "Standard",
            SourceChannel::TrackerCi,
            "foxsnacklite_uf2/nrf52840",
        ),
        (
            "SlimeNRF_FoxDongleNEO_Receiver",
            "foxdongleneo_uf2/nrf52840",
            EsbOtaKind::Receiver,
            "Standard",
            SourceChannel::ReceiverCi,
            "foxdongle_uf2/nrf52840",
        ),
        (
            "SlimeNRF_FoxDongleNEO_Receiver_DataCollect_CDC",
            "foxdongleneo_uf2/nrf52840",
            EsbOtaKind::Receiver,
            "DataCollect CDC",
            SourceChannel::ReceiverCi,
            "foxdongle33_uf2/nrf52833",
        ),
    ] {
        let mut artifact = chrysalis_ci_artifact();
        artifact.name = name.into();
        artifact.variant = variant.into();
        artifact.version.channel = channel;
        let hints = artifact_firmware_hints(&catalog, &artifact);
        assert_eq!(
            hints,
            [FirmwareHint {
                board_target: board.into(),
                kind: Some(kind),
                variant: variant.into(),
                mcuboot: false,
                bootloader: false,
            }],
            "{name}"
        );
        let mut target = chrysalis_tracker();
        target.info.board_target = board.into();
        if kind == EsbOtaKind::Receiver {
            target.id = crate::esb_ota::RECEIVER_ID;
            target.address = None;
        }
        assert!(firmware_page_matches(
            Mode::Esb,
            ImageCategory::Application,
            name,
            &hints,
            std::iter::once(&target),
            true,
        ));
        assert!(firmware_filter_matches(
            &hints,
            variant,
            &[(board.into(), Some(kind))],
            true,
            Some(variant),
        ));
        assert!(!firmware_filter_matches(
            &hints,
            variant,
            &[(older_board.into(), Some(kind))],
            true,
            Some(variant),
        ));
        let wrong_role = if kind == EsbOtaKind::Tracker {
            EsbOtaKind::Receiver
        } else {
            EsbOtaKind::Tracker
        };
        assert!(!firmware_filter_matches(
            &hints,
            variant,
            &[(board.into(), Some(wrong_role))],
            true,
            Some(variant),
        ));
        let other_variant = if variant == "Standard" {
            "DataCollect CDC"
        } else {
            "Standard"
        };
        assert!(!firmware_filter_matches(
            &hints,
            variant,
            &[(board.into(), Some(kind))],
            true,
            Some(other_variant),
        ));
        target.info.board_target = older_board.into();
        assert!(!firmware_page_matches(
            Mode::Esb,
            ImageCategory::Application,
            name,
            &hints,
            std::iter::once(&target),
            true,
        ));
        artifact.version.channel = SourceChannel::Release;
        assert!(artifact_firmware_hints(&catalog, &artifact).is_empty());
    }
}

#[test]
fn synthetic_fox_targets_enforce_method_and_role_admission() {
    use crate::board_catalog::EsbOtaKind;

    let catalog = Catalog::load_embedded().unwrap();
    for (name, board, kind, variant, uf2, esb) in [
        (
            "FoxApplication_FoxSnackLiteV3_Tracker_DataCollect_CDC.uf2",
            "foxsnacklitev3_uf2/nrf52840",
            EsbOtaKind::Tracker,
            "DataCollect CDC",
            false,
            false,
        ),
        (
            "FoxApplication_FoxSnackLiteV3_Tracker.hex",
            "foxsnacklitev3_uf2/nrf52840",
            EsbOtaKind::Tracker,
            "Standard",
            false,
            true,
        ),
        (
            "SlimeNRF_FoxDongleNEO_Receiver_DataCollect_HID.uf2",
            "foxdongleneo_uf2/nrf52840",
            EsbOtaKind::Receiver,
            "DataCollect HID",
            false,
            false,
        ),
        (
            "SlimeNRF_FoxDongleNEO_Receiver.hex",
            "foxdongleneo_uf2/nrf52840",
            EsbOtaKind::Receiver,
            "Standard",
            false,
            true,
        ),
        (
            "SlimeNRF_FoxDongleNEO_Receiver_DataCollect_CDC.uf2",
            "foxdongleneo_uf2/nrf52840",
            EsbOtaKind::Receiver,
            "DataCollect CDC",
            true,
            true,
        ),
    ] {
        let hints = catalog.match_filename(name);
        assert_eq!(
            hints,
            [FirmwareHint {
                board_target: board.into(),
                kind: Some(kind),
                variant: variant.into(),
                mcuboot: false,
                bootloader: false,
            }],
            "{name}"
        );
        let mut target = chrysalis_tracker();
        target.info.board_target = board.into();
        if kind == EsbOtaKind::Receiver {
            target.id = crate::esb_ota::RECEIVER_ID;
            target.address = None;
        }
        for (mode, method, expected) in [(Mode::Uf2, "UF2", uf2), (Mode::Esb, "ESB", esb)] {
            assert_eq!(
                firmware_page_matches(
                    mode,
                    ImageCategory::Application,
                    name,
                    &hints,
                    std::iter::once(&target),
                    false,
                ),
                expected,
                "{name} in {method}"
            );
        }
        let mut wrong_role = target.clone();
        if kind == EsbOtaKind::Receiver {
            wrong_role.id = 0;
            wrong_role.address = Some("112233445500".into());
        } else {
            wrong_role.id = crate::esb_ota::RECEIVER_ID;
            wrong_role.address = None;
        }
        assert!(
            !firmware_page_matches(
                Mode::Esb,
                ImageCategory::Application,
                name,
                &hints,
                std::iter::once(&wrong_role),
                false,
            ),
            "{name} with the wrong role"
        );
        target.info.bootloader = "mcuboot".into();
        assert!(
            !firmware_page_matches(
                Mode::Esb,
                ImageCategory::Application,
                name,
                &hints,
                std::iter::once(&target),
                false,
            ),
            "{name} with MCUboot"
        );
    }
}

#[test]
fn current_fox_ci_members_are_independently_authorized_for_both_roles() {
    let mut bytes = vec![0; 512];
    for (offset, value) in [
        (0, 0x0a324655u32),
        (4, 0x9e5d5157),
        (8, 0x2000),
        (12, 0x1000),
        (16, 256),
        (20, 0),
        (24, 1),
        (28, 0xada52840),
        (508, 0x0ab16f30),
    ] {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    for (advertisement, board, receiver, older_name) in [
        (
            "FoxApplication_FoxSnackLiteV3_Tracker",
            "foxsnacklitev3_uf2/nrf52840",
            false,
            "FoxApplication_FoxSnackLite_Tracker.uf2",
        ),
        (
            "SlimeNRF_FoxDongleNEO_Receiver",
            "foxdongleneo_uf2/nrf52840",
            true,
            "SlimeNRF_FoxDongle_Receiver.uf2",
        ),
        (
            "SlimeNRF_FoxDongleNEO_Receiver_DataCollect_CDC",
            "foxdongleneo_uf2/nrf52840",
            true,
            "SlimeNRF_FoxDongle33_Receiver_DataCollect_CDC.uf2",
        ),
    ] {
        let catalog = Catalog::load_embedded().unwrap();
        let mut artifact = chrysalis_ci_artifact();
        artifact.name = advertisement.into();
        artifact.version.channel = if receiver {
            SourceChannel::ReceiverCi
        } else {
            SourceChannel::TrackerCi
        };
        let advertised = artifact_firmware_hints(&catalog, &artifact);
        assert_eq!(advertised.len(), 1, "{advertisement}");
        let mut target = chrysalis_tracker();
        target.info.board_target = board.into();
        if receiver {
            target.id = crate::esb_ota::RECEIVER_ID;
            target.address = None;
        }
        let mut state = FlashState {
            catalog: Some(catalog),
            targets: vec![target.clone()],
            selected_targets: [ota_target_key(&target)].into(),
            ..FlashState::default()
        };
        state.reconcile_groups(None);
        let destination = state.destination(Mode::Esb).unwrap();
        let expected_name = format!("{advertisement}.uf2");
        for name in ["Unknown_Fox.uf2", older_name, expected_name.as_str()] {
            let mut member = make_image(
                name.into(),
                bytes.clone(),
                "CI archive member".into(),
                "fixture-digest".into(),
                false,
            );
            assert!(member.validation_error.is_none(), "{name}");
            member.hints = advertised.clone().into();
            state.apply_images(destination.clone(), vec![member]);
            if name == expected_name {
                assert_eq!(state.displayed_image(Mode::Esb).unwrap().name, name);
                assert_eq!(validate_ota_image(&state.images[0], &target), Ok(()));
                let mut wrong_role = target.clone();
                wrong_role.id = if receiver {
                    0
                } else {
                    crate::esb_ota::RECEIVER_ID
                };
                assert!(validate_ota_image(&state.images[0], &wrong_role).is_err());
            } else {
                assert!(state.displayed_image(Mode::Esb).is_none(), "{name}");
                assert!(
                    validate_ota_image(&state.images[0], &target).is_err(),
                    "{name}"
                );
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct ArtifactSearchTargetFacts {
    key: String,
    receiver_path: Vec<u8>,
    address: Option<String>,
    board_target: String,
    bootloader: String,
}

fn artifact_search_target_facts(target: &crate::esb_ota::Target) -> ArtifactSearchTargetFacts {
    ArtifactSearchTargetFacts {
        key: ota_target_key(target),
        receiver_path: target.receiver.path.clone(),
        address: target.address.clone(),
        board_target: target.info.board_target.clone(),
        bootloader: target.info.bootloader.clone(),
    }
}

struct ArtifactSearchFixture {
    window: AnyWindowHandle,
    workbench: Entity<Workbench>,
    search_entity: gpui_kit::EntityId,
    artifacts: Vec<Artifact>,
    image: super::Image,
    destination: ImageDestination,
    selected_volumes: BTreeSet<String>,
    selected_targets: BTreeSet<String>,
    target_facts: Vec<ArtifactSearchTargetFacts>,
    tracker_selection: BTreeSet<String>,
    active_group: Option<String>,
    group_generations: HashMap<String, u64>,
    remote_generation: u64,
    _directory: tempfile::TempDir,
    _runtime: crate::app_runtime::AppRuntime,
}

impl ArtifactSearchFixture {
    fn new(cx: &mut TestAppContext, mode: Mode) -> Self {
        let directory = tempfile::tempdir().expect("create synthetic UF2 directory");
        std::fs::write(
            directory.path().join("INFO_UF2.TXT"),
            "Board-ID: nRF52840-SlimeNRFTracker-ProMicro\n",
        )
        .expect("write synthetic UF2 marker");
        let volume = crate::uf2::inspect_volume(directory.path(), "INFO_UF2.TXT")
            .expect("inspect synthetic UF2 volume");
        let image = make_image(
            "previously-downloaded.hex".into(),
            b":0410000001020304E2\n:00000001FF\n".to_vec(),
            "previously downloaded fixture".into(),
            "previous-digest".into(),
            false,
        );
        let version = version(SourceChannel::Release, 42);
        let artifact = |id, name: &str, variant: &str, size| Artifact {
            name: name.into(),
            variant: variant.into(),
            size,
            version: version.clone(),
            // These URLs fail validation before any request. The first row also
            // exceeds the size limit, giving a different error if indices shift.
            url: format!("http://127.0.0.1:1/artifact-{id}"),
            sha256: None,
            id,
        };
        let artifacts = vec![
            artifact(
                101,
                "SlimeNRF_ProMicro_Chrysalis_Tracker.uf2",
                "Standard",
                70_000_000,
            ),
            artifact(
                102,
                "SlimeNRF_ProMicro_Chrysalis_Tracker-1.2.3.uf2",
                "Standard",
                64,
            ),
            artifact(103, "SlimeNRF_FoxDongleNEO_Receiver.uf2", "Standard", 64),
            artifact(
                104,
                "SlimeNRF_FoxDongleNEO_Receiver_DataCollect_CDC.uf2",
                "DataCollect CDC",
                64,
            ),
        ];
        let mut flash = FlashState {
            mode,
            catalog: Some(Catalog::load_embedded().expect("load embedded firmware catalog")),
            catalog_loaded: true,
            remote_view: true,
            channel: SourceChannel::Release,
            application_channel: SourceChannel::Release,
            image: Some(image.clone()),
            images: vec![image.clone()],
            next_scan: Instant::now() + Duration::from_secs(60),
            ..FlashState::default()
        };
        if mode == Mode::Uf2 {
            flash.selected_volumes.insert(volume.identity.clone());
            flash.volumes.push(volume);
        } else {
            let tracker = chrysalis_tracker();
            flash.selected_targets.insert(ota_target_key(&tracker));
            flash
                .tracker_selection
                .insert(format!("tracker:{}:{}", tracker.receiver.key, tracker.id));
            flash.targets.push(tracker);
            flash.reconcile_groups(None);
            let group = flash
                .active_group
                .clone()
                .expect("selected tracker has a firmware group");
            flash.assignments.insert(group, image.clone());
            flash.ota_step = super::OtaStep::Firmware;
        }
        let destination = flash
            .destination(mode)
            .expect("firmware destination exists");
        flash.prepare_remote(mode, &destination);
        let request = flash.remote_request(mode);
        flash.apply_remote(mode, Payload::Latest(request, version));
        flash.select_version(42);
        let request = flash.remote_request(mode);
        flash.apply_remote(mode, Payload::Artifacts(request, artifacts.clone()));
        flash.images_destination = destination.clone();
        let selected_volumes = flash.selected_volumes.clone();
        let selected_targets = flash.selected_targets.clone();
        let target_facts = flash
            .targets
            .iter()
            .map(artifact_search_target_facts)
            .collect();
        let tracker_selection = flash.tracker_selection.clone();
        let active_group = flash.active_group.clone();
        let group_generations = flash.group_generations.clone();
        let remote_generation = flash.remote_generation;
        let runtime = crate::app_runtime::AppRuntime::new().expect("create isolated I/O runtime");
        let executor = runtime.executor();
        let selected = tracker_selection.clone();
        let preferences_path = directory.path().join("preferences.json");
        let (window, workbench) = cx.update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        Default::default(),
                        size(px(1600.), px(1100.)),
                    ))),
                    ..Default::default()
                },
                cx,
                move |window, cx| {
                    cx.new(|cx| {
                        let mut this = crate::ui::test_support::offline_workbench(
                            window,
                            cx,
                            crate::sessions::SessionManager::new(),
                            executor,
                        );
                        this.preferences_io =
                            crate::ui::preferences::PreferenceIo::for_test(preferences_path);
                        this.active_page = if mode == Mode::Uf2 {
                            FeaturePage::Uf2
                        } else {
                            FeaturePage::Ota
                        };
                        this.selected = selected;
                        this.flash = flash;
                        this
                    })
                },
            )
            .expect("open real firmware Workbench")
        });
        cx.update_window(window, |_, window, _| window.activate_window())
            .expect("activate firmware test window");
        cx.run_until_parked();
        let search_entity = cx.update(|cx| workbench.read(cx).firmware_search.entity_id());
        Self {
            window,
            workbench,
            search_entity,
            artifacts,
            image,
            destination,
            selected_volumes,
            selected_targets,
            target_facts,
            tracker_selection,
            active_group,
            group_generations,
            remote_generation,
            _directory: directory,
            _runtime: runtime,
        }
    }

    fn assert_preserved(&self, cx: &gpui_kit::App) {
        let workbench = self.workbench.read(cx);
        assert_eq!(workbench.firmware_search.entity_id(), self.search_entity);
        assert_eq!(workbench.selected, self.tracker_selection);
        let state = &workbench.flash;
        assert_eq!(state.selected_version, Some(42));
        assert_eq!(state.versions.len(), 1);
        assert_eq!(state.versions[0].id, 42);
        assert_eq!(state.latest_version, Some((SourceChannel::Release, 42)));
        assert_eq!(state.remote_generation, self.remote_generation);
        assert_eq!(state.channel, SourceChannel::Release);
        assert!(state.remote_destination.eq(&self.destination));
        assert!(state.images_destination.eq(&self.destination));
        assert_eq!(state.selected_volumes, self.selected_volumes);
        assert_eq!(state.selected_targets, self.selected_targets);
        assert_eq!(
            state
                .targets
                .iter()
                .map(artifact_search_target_facts)
                .collect::<Vec<_>>(),
            self.target_facts
        );
        assert_eq!(state.tracker_selection, self.tracker_selection);
        assert_eq!(state.active_group, self.active_group);
        assert_eq!(state.group_generations, self.group_generations);
        assert_eq!(state.artifacts.len(), self.artifacts.len());
        for (actual, expected) in state.artifacts.iter().zip(&self.artifacts) {
            assert_eq!(actual.id, expected.id);
            assert_eq!(actual.name, expected.name);
            assert_eq!(actual.url, expected.url);
            assert_eq!(actual.size, expected.size);
        }
        assert_eq!(state.images.len(), 1);
        assert!(Arc::ptr_eq(&state.images[0].bytes, &self.image.bytes));
        assert!(Arc::ptr_eq(
            &state.image.as_ref().expect("selected image remains").bytes,
            &self.image.bytes
        ));
        if let Some(group) = &self.active_group {
            assert_eq!(state.assignments.len(), 1);
            assert!(Arc::ptr_eq(
                &state.assignments[group].bytes,
                &self.image.bytes
            ));
        }
        assert!(
            state.job.is_none(),
            "typing or clearing a query must not start a firmware network job"
        );
        assert!(state.error.is_none());
    }
}

fn artifact_button_id(artifact: &Artifact) -> String {
    format!(
        "firmware-artifact-{:?}-{}-{}",
        artifact.version.channel, artifact.version.id, artifact.id
    )
}

fn assert_artifact_rows(window: &Window, fixture: &ArtifactSearchFixture, expected: &[usize]) {
    let mut previous_top = None;
    for (index, artifact) in fixture.artifacts.iter().enumerate() {
        let row = window.try_find(artifact_button_id(artifact));
        if !expected.contains(&index) {
            assert!(
                row.is_none(),
                "unexpected artifact at original index {index}"
            );
            continue;
        }
        let row = row.expect("expected original artifact is rendered");
        let label = format!(
            "Load firmware {} · {} · {} · {} B",
            artifact.version.name,
            artifact.name,
            super::firmware_variant_label(&artifact.variant),
            artifact.size
        );
        assert_eq!(row.role(), Some(Role::Button));
        assert_eq!(row.label(), Some(label.as_str()));
        assert!(
            row.visible(),
            "artifact at original index {index} must be visible"
        );
        if let Some(previous_top) = previous_top {
            assert!(
                previous_top < row.bounds().top(),
                "filtered rows must retain their original artifact order"
            );
        }
        previous_top = Some(row.bounds().top());
    }
}

fn replace_artifact_query(window: &mut Window, cx: &mut gpui_kit::App, query: &str) {
    window.click("firmware-artifact-search", cx);
    window.press("secondary-a", cx);
    if !query.is_empty() {
        window.input(query, cx);
    } else {
        window.press("backspace", cx);
    }
    assert_eq!(window.find("firmware-artifact-search").value(), Some(query));
}

fn exercise_artifact_search(cx: &mut TestAppContext, mode: Mode) {
    let _ui_session = crate::ui::test_support::UiTestSession::new(cx);
    let mut fixture = ArtifactSearchFixture::new(cx, mode);
    cx.update_window(fixture.window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("firmware-artifact-search").visible());
        assert_eq!(
            window.find("firmware-artifact-search").placeholder(),
            Some("Search firmware files")
        );
        assert_eq!(
            window.find("firmware-version-Release-42").checked(),
            Some(true)
        );
        assert_artifact_rows(window, &fixture, &[0, 1]);
        fixture.assert_preserved(cx);

        replace_artifact_query(window, cx, "  cHrYsAlIs  ");
        assert_artifact_rows(window, &fixture, &[0, 1]);
        fixture.assert_preserved(cx);
        replace_artifact_query(window, cx, "  1.2.3  ");
        assert_artifact_rows(window, &fixture, &[1]);
        fixture.assert_preserved(cx);
        replace_artifact_query(window, cx, "  sTaNdArD  ");
        assert_artifact_rows(window, &fixture, &[0, 1]);
        fixture.assert_preserved(cx);
        replace_artifact_query(window, cx, "   ");
        assert_artifact_rows(window, &fixture, &[0, 1]);
        assert!(window.try_find("firmware-artifact-search-empty").is_none());
        assert!(fixture.workbench.read(cx).flash.matched_only);
        assert!(fixture.workbench.read(cx).flash.variant_filter.is_none());
        fixture.assert_preserved(cx);

        replace_artifact_query(window, cx, "  sTaNdArD  ");
        let firmware_page = if mode == Mode::Uf2 {
            "nav-uf2"
        } else {
            "nav-esb-ota"
        };
        // Prevent actual device discovery while exercising the real language controls.
        fixture
            .workbench
            .update(cx, |this, _| this.scanning_inventory = true);
        window.click("nav-settings", cx);
        window.click("settings-language-simplified-chinese", cx);
        assert!(fixture.workbench.read(cx).inventory_task.is_none());
        window.click(firmware_page, cx);
        assert_eq!(
            fixture.workbench.read(cx).flash.remote_generation,
            fixture.remote_generation + 2
        );
        fixture.remote_generation += 2;
        assert_eq!(
            window.find("firmware-artifact-search").value(),
            Some("  sTaNdArD  ")
        );
        assert_eq!(
            window.find("firmware-artifact-search").placeholder(),
            Some("搜索固件文件")
        );
        assert_artifact_rows(window, &fixture, &[]);
        assert!(window.find("firmware-artifact-search-empty").visible());
        fixture.assert_preserved(cx);
        replace_artifact_query(window, cx, " 普通 ");
        assert_artifact_rows(window, &fixture, &[0, 1]);
        fixture.assert_preserved(cx);
        window.click("nav-settings", cx);
        window.click("settings-language-english", cx);
        assert!(fixture.workbench.read(cx).inventory_task.is_none());
        window.click(firmware_page, cx);
        assert_eq!(
            fixture.workbench.read(cx).flash.remote_generation,
            fixture.remote_generation + 2
        );
        fixture.remote_generation += 2;
        assert_eq!(
            window.find("firmware-artifact-search").value(),
            Some(" 普通 ")
        );
        assert_eq!(
            window.find("firmware-artifact-search").placeholder(),
            Some("Search firmware files")
        );
        fixture
            .workbench
            .update(cx, |this, _| this.scanning_inventory = false);
        fixture.assert_preserved(cx);
        assert_artifact_rows(window, &fixture, &[]);
        let empty = window.find("firmware-artifact-search-empty");
        assert_eq!(empty.role(), Some(Role::Status));
        assert_eq!(empty.label(), Some("No firmware files match your search."));
        assert!(window.try_find("firmware-artifact-no-compatible").is_none());
        window.click("firmware-artifact-clear-search", cx);
        assert_eq!(window.find("firmware-artifact-search").value(), Some(""));
        assert_eq!(
            window.find("firmware-artifact-search").focused(),
            Some(true)
        );
        assert_artifact_rows(window, &fixture, &[0, 1]);
        fixture.assert_preserved(cx);

        replace_artifact_query(window, cx, "nothing-can-match-this");
        assert_artifact_rows(window, &fixture, &[]);
        assert!(window.find("firmware-artifact-search-empty").visible());
        window.click("firmware-artifact-clear-search", cx);
        assert_artifact_rows(window, &fixture, &[0, 1]);
        fixture.assert_preserved(cx);

        if mode == Mode::Uf2 {
            window.click("firmware-matched", cx);
            assert_eq!(window.find("firmware-matched").checked(), Some(false));
            replace_artifact_query(window, cx, "  fOxDoNgLeNeO  ");
            assert_artifact_rows(window, &fixture, &[2, 3]);
            fixture.assert_preserved(cx);
            window.click(("firmware-variant-filter", 0usize), cx);
            assert_artifact_rows(window, &fixture, &[2]);
            window.click(("firmware-variant-filter", 1usize), cx);
            assert_artifact_rows(window, &fixture, &[3]);
            assert_eq!(
                fixture.workbench.read(cx).flash.variant_filter.as_deref(),
                Some("DataCollect CDC")
            );
            window.click("firmware-matched", cx);
            assert_artifact_rows(window, &fixture, &[]);
            let incompatible = window.find("firmware-artifact-no-compatible");
            assert_eq!(incompatible.role(), Some(Role::Status));
            assert_eq!(
                incompatible.label(),
                Some("No compatible firmware for this category and target")
            );
            assert!(window.try_find("firmware-artifact-search-empty").is_none());
            assert!(window.try_find("firmware-artifact-clear-search").is_none());
            assert!(fixture.workbench.read(cx).flash.matched_only);
            assert_eq!(
                fixture.workbench.read(cx).flash.variant_filter.as_deref(),
                Some("DataCollect CDC")
            );
            fixture.assert_preserved(cx);
            window.click("firmware-clear-filters", cx);
            assert_artifact_rows(window, &fixture, &[2, 3]);
            window.click("firmware-matched", cx);
            assert_artifact_rows(window, &fixture, &[]);
            assert!(window.find("firmware-artifact-search-empty").visible());
        } else {
            window.click(("firmware-variant-filter", 0usize), cx);
            assert_eq!(
                fixture.workbench.read(cx).flash.variant_filter.as_deref(),
                Some("Standard")
            );
            window.click("firmware-matched", cx);
            replace_artifact_query(window, cx, "foxdongleneo");
            assert_artifact_rows(window, &fixture, &[]);
            assert!(window.find("firmware-artifact-search-empty").visible());
            assert!(
                window.try_find("firmware-artifact-no-compatible").is_none(),
                "ESB method and target eligibility still precede search"
            );
            assert!(!fixture.workbench.read(cx).flash.matched_only);
            assert_eq!(
                fixture.workbench.read(cx).flash.variant_filter.as_deref(),
                Some("Standard")
            );
        }
        fixture.assert_preserved(cx);
        window.click("firmware-artifact-clear-search", cx);
        assert_artifact_rows(window, &fixture, &[0, 1]);
        replace_artifact_query(window, cx, "  1.2.3  ");
        assert_artifact_rows(window, &fixture, &[1]);
        fixture.assert_preserved(cx);

        window.click(artifact_button_id(&fixture.artifacts[1]), cx);
        assert!(
            fixture.workbench.read(cx).flash.job.is_some(),
            "the actual filtered artifact button must start its retained worker"
        );
    })
    .expect("interact with firmware search");
    cx.run_until_parked();

    fixture.workbench.update(cx, |this, _| {
        let mut completed = None;
        crate::sessions::test_support::until(|| {
            completed = this.flash.poll_job();
            completed.is_some()
        });
        let (result, _) = completed.expect("filtered artifact worker joined");
        match result {
            Err(error) => assert_eq!(
                error, "Firmware source or redirect is not an approved HTTPS upstream host",
                "the second original artifact must be selected, not oversized index zero"
            ),
            Ok(_) => panic!("invalid test URL must be rejected before any network request"),
        }
    });
    cx.update(|cx| fixture.assert_preserved(cx));
}

#[gpui_kit::test]
fn uf2_online_artifact_search_preserves_filters_selection_and_original_identity(
    cx: &mut TestAppContext,
) {
    exercise_artifact_search(cx, Mode::Uf2);
}

#[gpui_kit::test]
fn esb_online_artifact_search_preserves_filters_assignment_and_original_identity(
    cx: &mut TestAppContext,
) {
    exercise_artifact_search(cx, Mode::Esb);
}
