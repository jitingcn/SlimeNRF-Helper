use super::{
    Action, FlashState, ImageCategory, ImageDestination, Mode, Payload, artifact_firmware_hints,
    firmware_filter_matches, firmware_page_matches, make_image, ota_target_key, validate_ota_image,
};
use crate::{
    board_catalog::{Catalog, FirmwareHint},
    firmware_source::{Artifact, SourceChannel, Version, is_bootloader_update},
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
