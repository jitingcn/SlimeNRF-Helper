use crate::application::flash::{
    Action, EsbAutoQuery, FlashState, Image, Mode, OtaStep, available_query_groups, make_image,
    ota_assignment_key, ota_target_key, plan_ota_groups,
};
use crate::domain::{ReceiverEndpoint, TrackerLink};
use crate::features::firmware::esb_ota::{FirmwareInfo, RECEIVER_ID, Target};
use std::time::{Duration, Instant};

pub(super) fn target(receiver: &str, id: u8, board: &str) -> Target {
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

pub(super) fn local_image(name: &str) -> Image {
    make_image(
        name.into(),
        b":0410000001020304E2\n:00000001FF\n".to_vec(),
        "local".into(),
        String::new(),
        true,
    )
}

pub(super) fn ota_workflow_state() -> FlashState {
    let tracker = target("a", 0, "board");
    let receiver = target("b", RECEIVER_ID, "board");
    let mut state = FlashState {
        mode: Mode::Esb,
        selected_targets: [ota_target_key(&tracker), ota_target_key(&receiver)].into(),
        ..FlashState::default()
    };
    state.tracker_selection.insert("tracker:a:0".into());
    state.targets = vec![tracker, receiver];
    state.reconcile_groups(None);
    state
}

pub(super) fn confirmed_ota_state() -> FlashState {
    let mut state = ota_workflow_state();
    for device in &state.targets {
        state
            .assignments
            .insert(ota_assignment_key(device), local_image("firmware.hex"));
    }
    state.ota_step = OtaStep::Confirm;
    state.review = Some(Action::EsbGroups(plan_ota_groups(&state).unwrap()));
    state.acknowledged = true;
    state
}

pub(super) fn internal_default_state() -> (tempfile::TempDir, FlashState) {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("slimenrf-helper.yaml");
    std::fs::write(
        &config,
        br#"{"schema_version":1,"firmware_root":".","legacy_targets":{"board":{"targets":[
            {"name":"board","firmware":"board.hex","esb_ota_kind":"tracker"},
            {"name":"other","firmware":"other.hex","esb_ota_kind":"tracker"}
        ]}}}"#,
    )
    .unwrap();
    for name in ["board.hex", "other.hex"] {
        std::fs::write(
            directory.path().join(name),
            b":0410000001020304E2\n:00000001FF\n",
        )
        .unwrap();
    }
    let tracker = target("a", 0, "board");
    let mut state = FlashState {
        catalog: Some(crate::features::firmware::board_catalog::Catalog::load(&config).unwrap()),
        selected_targets: [ota_target_key(&tracker)].into(),
        tracker_selection: ["tracker:a:0".into()].into(),
        targets: vec![tracker],
        ..FlashState::default()
    };
    state.reconcile_groups(None);
    (directory, state)
}

pub(super) fn join_internal_default(state: &mut FlashState) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while state.default_job.is_some() {
        if state.poll_internal_default() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "default image worker did not finish"
        );
        std::thread::yield_now();
    }
    assert!(
        !state.poll_internal_default(),
        "idle polling must not repaint"
    );
}

pub(super) fn finish_internal_default(state: &mut FlashState) {
    assert!(state.start_internal_default());
    join_internal_default(state);
}

pub(super) fn query_link(receiver: &str, slot: u8) -> TrackerLink {
    TrackerLink {
        receiver_key: receiver.into(),
        slot,
        address: Some(format!("AABBCCDDEE{slot:02X}")),
        online: true,
    }
}

pub(super) fn seeded_query(
    receivers: &[ReceiverEndpoint],
    trackers: &[TrackerLink],
    now: Instant,
) -> EsbAutoQuery {
    let mut query = EsbAutoQuery::default();
    query.enter(true);
    for receiver in receivers {
        let links: Vec<_> = trackers
            .iter()
            .filter(|tracker| tracker.receiver_key == receiver.key)
            .cloned()
            .collect();
        query.observe(receiver, &links, now);
    }
    query.full_requested(&available_query_groups(receivers, trackers.iter()));
    query
}
