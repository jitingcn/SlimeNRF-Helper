use super::{Action, FlashState, Mode, OtaUpdateGuidance, ota_target_key, ota_update_guidance};
use crate::domain::ReceiverEndpoint;
use crate::esb_ota::{FirmwareInfo, RECEIVER_ID, Target};

fn target(id: u8, protocol_version: u8, bootloader: &str) -> Target {
    Target {
        receiver: ReceiverEndpoint {
            key: "receiver".into(),
            path: b"receiver".to_vec(),
            label: "Holyiot".into(),
            serial_number: None,
        },
        id,
        address: (id != RECEIVER_ID).then(|| "112233445566".into()),
        info: FirmwareInfo {
            version: "1".into(),
            build_date: "today".into(),
            firmware_size: 4,
            bootloader: bootloader.into(),
            protocol_version,
            board_target: "Holyiot".into(),
            flash_base: 0x1000,
        },
    }
}

#[test]
fn guidance_uses_reported_capabilities_not_holyiot_names() {
    for id in [RECEIVER_ID, 0] {
        for bootloader in ["none", "adafruit_uf2", "mcuboot"] {
            assert_eq!(ota_update_guidance(&target(id, 1, bootloader)), None);
        }
        for protocol in [1, 2] {
            assert_eq!(
                ota_update_guidance(&target(id, protocol, "nrf5_opendfu")),
                Some(OtaUpdateGuidance::OpenDfu)
            );
            assert_eq!(
                ota_update_guidance(&target(id, protocol, "unknown")),
                Some(OtaUpdateGuidance::DocumentedMethod)
            );
        }
        assert_eq!(
            ota_update_guidance(&target(id, 2, "adafruit_uf2")),
            Some(OtaUpdateGuidance::Uf2)
        );
        for bootloader in ["none", "mcuboot"] {
            let guidance = ota_update_guidance(&target(id, 2, bootloader)).unwrap();
            assert_eq!(guidance, OtaUpdateGuidance::DocumentedMethod);
            assert!(guidance.wired_mode().is_none());
        }
    }
}

#[test]
fn wired_navigation_preserves_selections_and_disarms_authorization_without_work() {
    for (bootloader, protocol, mode) in [
        ("nrf5_opendfu", 1, Mode::OpenDfu),
        ("adafruit_uf2", 2, Mode::Uf2),
    ] {
        let device = target(RECEIVER_ID, protocol, bootloader);
        let key = ota_target_key(&device);
        let mut state = FlashState {
            mode: Mode::Esb,
            targets: vec![device],
            selected_targets: ["selected-tracker".into()].into(),
            tracker_selection: ["tracker:receiver:0".into()].into(),
            selected_volumes: ["explicit-volume".into()].into(),
            selected_port: Some("explicit-port".into()),
            review: Some(Action::Reboot(Vec::new())),
            acknowledged: true,
            auto: true,
            auto_requested: true,
            ..FlashState::default()
        };
        let selected_mode = state.ota_wired_update_mode(&key);
        assert!(selected_mode == Some(mode));
        state.transition_mode(selected_mode.unwrap());
        assert!(state.mode == mode);
        assert!(state.review.is_none());
        assert!(!state.acknowledged && !state.auto && !state.auto_requested);
        assert_eq!(state.selected_targets, ["selected-tracker".into()].into());
        assert_eq!(
            state.tracker_selection,
            ["tracker:receiver:0".into()].into()
        );
        assert_eq!(state.selected_volumes, ["explicit-volume".into()].into());
        assert_eq!(state.selected_port.as_deref(), Some("explicit-port"));
        assert!(state.pending.is_none() && state.job.is_none() && state.chooser.is_none());
        assert!(!state.busy());
    }
}

#[test]
fn wired_navigation_rechecks_current_target_and_rejects_busy_or_unknown_routes() {
    let device = target(RECEIVER_ID, 1, "nrf5_opendfu");
    let key = ota_target_key(&device);
    let mut state = FlashState {
        mode: Mode::Esb,
        targets: vec![device],
        pending: Some(Action::Reboot(Vec::new())),
        acknowledged: true,
        ..FlashState::default()
    };
    assert!(state.ota_wired_update_mode(&key).is_none());
    assert!(state.mode == Mode::Esb && state.acknowledged);
    assert!(state.pending.is_some());
    state.pending = None;
    for bootloader in ["unknown", "none"] {
        state.targets[0].info.bootloader = bootloader.into();
        assert!(state.ota_wired_update_mode(&key).is_none());
        assert!(state.mode == Mode::Esb && state.acknowledged);
    }
    state.targets.clear();
    assert!(state.ota_wired_update_mode(&key).is_none());
    assert!(state.mode == Mode::Esb && state.acknowledged);
}

#[test]
fn receiver_update_stays_opt_in_and_rechecks_selection_guards() {
    let receiver = target(RECEIVER_ID, 1, "none");
    let receiver_key = ota_target_key(&receiver);
    let tracker = target(0, 1, "none");
    let tracker_key = ota_target_key(&tracker);
    let mut state = FlashState {
        targets: vec![receiver, tracker],
        ..FlashState::default()
    };
    assert!(state.selected_targets.is_empty());
    state.set_receiver_update_selected(&tracker_key, true);
    state.set_receiver_update_selected("missing", true);
    assert!(state.selected_targets.is_empty());
    state.targets[0].info.bootloader = "nrf5_opendfu".into();
    state.set_receiver_update_selected(&receiver_key, true);
    assert!(state.selected_targets.is_empty());
    state.targets[0].info.bootloader = "none".into();
    state.set_receiver_update_selected(&receiver_key, true);
    assert!(state.selected_targets.contains(&receiver_key));
    assert!(!state.selected_targets.contains(&tracker_key));
    state.pending = Some(Action::Reboot(Vec::new()));
    state.set_receiver_update_selected(&receiver_key, false);
    assert!(state.selected_targets.contains(&receiver_key));
    state.pending = None;
    state.set_receiver_update_selected(&receiver_key, false);
    assert!(state.selected_targets.is_empty());
}
