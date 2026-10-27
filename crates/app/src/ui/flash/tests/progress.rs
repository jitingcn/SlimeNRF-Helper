use super::support::target;
use crate::ui::flash::{Action, FlashState, OtaQueryPlan};
use crate::update::UpdateOutcome;

fn ota_progress(target: &crate::esb_ota::Target) -> crate::update::OtaTargetProgress {
    crate::update::OtaTargetProgress {
        receiver_key: target.receiver.key.clone(),
        id: target.id,
        phase: crate::update::OtaPhase::Queued,
        preparation: None,
        error: None,
        completed: 0,
        total: 0,
        bytes_per_second: None,
        eta_seconds: None,
    }
}

#[test]
fn live_terminal_groups_use_receiver_identity_and_preserve_failure_tooltips() {
    use crate::update::OtaPhase;
    let mut other = target("b", 0, "board");
    other.receiver.label = "a".into();
    let devices = [
        target("a", 0, "board"),
        target("a", 1, "board"),
        other,
        target("b", 1, "board"),
    ];
    let mut progress = devices.iter().map(ota_progress).collect::<Vec<_>>();
    progress[0].phase = OtaPhase::Completed;
    progress[1].phase = OtaPhase::Transferring;
    progress[1].completed = 100;
    progress[1].total = 100;
    progress[2].phase = OtaPhase::Cancelled;
    progress[2].error = Some("cancelled".into());
    let state = FlashState {
        operation_targets: devices
            .iter()
            .map(crate::ui::flash::ResultTarget::ota)
            .collect(),
        ..FlashState::default()
    };
    let groups = crate::ui::flash::progress::progress_receiver_groups(&progress);
    assert_eq!(groups.len(), 2);
    assert_eq!(groups["a"].len(), 2);
    assert_eq!(groups["b"].len(), 1);
    assert!(crate::ui::flash::progress::ota_terminal(
        groups["a"][0].phase
    ));
    assert!(!crate::ui::flash::progress::ota_terminal(
        groups["a"][1].phase
    ));
    assert_eq!(state.progress_identity(groups["a"][0]).unwrap().key, "a:0");
    assert_eq!(state.progress_identity(groups["b"][0]).unwrap().key, "b:0");
    let mut cleanup_failure = progress[0].clone();
    cleanup_failure.phase = OtaPhase::Failed;
    cleanup_failure.error = Some("CRC mismatch; OTA abort failed: disconnected".into());
    progress[0] = cleanup_failure;
    let groups = crate::ui::flash::progress::progress_receiver_groups(&progress);
    assert_eq!(groups["a"][0].phase, OtaPhase::Failed);
    assert_eq!(groups["b"][0].phase, OtaPhase::Cancelled);
    assert_eq!(
        groups["a"][0].error.as_deref(),
        Some("CRC mismatch; OTA abort failed: disconnected")
    );
    assert_eq!(groups["b"][0].error.as_deref(), Some("cancelled"));
    let identity = state
        .progress_identity(groups["a"][0])
        .unwrap()
        .identity
        .clone();
    assert_eq!(
        crate::ui::flash::progress::ota_terminal_tooltip(groups["a"][0], identity.clone()),
        format!("{identity}\nCRC mismatch; OTA abort failed: disconnected")
    );
    assert_eq!(
        crate::ui::flash::progress::ota_terminal_tooltip(
            groups["b"][0],
            "Receiver b · Tracker 0".into()
        ),
        "Receiver b · Tracker 0\ncancelled"
    );
    progress[0].phase = OtaPhase::Completed;
    progress[0].error = None;
    assert_eq!(
        crate::ui::flash::progress::ota_terminal_tooltip(&progress[0], identity.clone()),
        identity
    );
}

#[test]
fn result_groups_use_receiver_identity_and_retain_compact_failure_metadata() {
    let mut results = Vec::new();
    for id in 0..8 {
        let device = target("a", id, "board");
        let result = if id == 7 {
            UpdateOutcome::Failed("ACTIVATE timeout".into())
        } else {
            UpdateOutcome::Completed
        };
        results.push((crate::ui::flash::ResultTarget::ota(&device), result));
    }
    let mut other = target("b", 7, "other-board");
    other.receiver.label = "a".into();
    results.push((
        crate::ui::flash::ResultTarget::ota(&other),
        UpdateOutcome::Completed,
    ));
    let groups = crate::ui::flash::progress::result_receiver_groups(&results);
    assert_eq!(groups.len(), 2);
    assert_eq!(
        groups["a"]
            .iter()
            .filter(|(_, result)| matches!(result, UpdateOutcome::Completed))
            .count(),
        7
    );
    assert_eq!(groups["a"][7].0.id, Some(7));
    assert_eq!(groups["a"][7].0.key, "a:7");
    assert!(groups["a"][7].0.identity.contains("112233445507"));
    assert_eq!(
        groups["a"][7].1,
        &UpdateOutcome::Failed("ACTIVATE timeout".into())
    );
    assert_eq!(groups["b"][0].0.key, "b:7");
}

#[test]
fn query_completion_preserves_mixed_operation_terminal_snapshots() {
    use crate::update::OtaPhase;
    let devices = [
        target("a", 0, "board"),
        target("a", 1, "board"),
        target("b", 0, "board"),
    ];
    let mut operation = devices.iter().map(ota_progress).collect::<Vec<_>>();
    operation[0].phase = OtaPhase::Completed;
    operation[0].completed = 100;
    operation[0].total = 100;
    operation[1].phase = OtaPhase::Failed;
    operation[1].error = Some("VERIFY rejected; OTA abort failed: disconnected".into());
    operation[2].phase = OtaPhase::Cancelled;
    operation[2].error = Some("cancelled".into());
    assert_eq!(
        crate::ui::flash::OtaCounts::from_progress(&operation),
        crate::ui::flash::OtaCounts {
            completed: 1,
            activation_unconfirmed: 0,
            failed: 1,
            cancelled: 1,
            active: 0,
            queued: 0,
        }
    );
    let mut progress = crate::update::Progress::stage("");
    progress.ota = operation;
    let mut state = FlashState {
        operation_targets: devices
            .iter()
            .map(crate::ui::flash::ResultTarget::ota)
            .collect(),
        last_progress: Some(progress),
        results: vec![(
            crate::ui::flash::ResultTarget::ota(&devices[0]),
            UpdateOutcome::Completed,
        )],
        ..FlashState::default()
    };
    state.prepare_action_results(&Action::QueryGroups(OtaQueryPlan {
        groups: vec![],
        automatic: true,
        incremental: false,
    }));
    state.activity_kind = crate::ui::flash::ActivityKind::Query;
    state.finish_job_progress(Some(crate::update::Progress::bytes("query", 1, 1)));
    assert_eq!(state.operation_targets.len(), 3);
    assert_eq!(state.results.len(), 1);
    assert_eq!(
        state.last_progress.as_ref().unwrap().ota[0].phase,
        OtaPhase::Completed
    );
    assert_eq!(
        state.last_progress.as_ref().unwrap().ota[1].phase,
        OtaPhase::Failed
    );
    assert_eq!(
        state.last_progress.as_ref().unwrap().ota[2].phase,
        OtaPhase::Cancelled
    );
    assert_eq!(
        state.last_progress.as_ref().unwrap().ota[1]
            .error
            .as_deref(),
        Some("VERIFY rejected; OTA abort failed: disconnected")
    );
    assert_eq!(
        state.last_progress.as_ref().unwrap().ota[2]
            .error
            .as_deref(),
        Some("cancelled")
    );
}

#[test]
fn execution_identities_are_presented_by_the_ui() {
    use crate::firmware_update::{QueryFailure, QueryResults, ResultIdentity};
    use crate::ui::flash::{OtaQueries, ResultTarget};

    let serial = ResultTarget::from(ResultIdentity::Serial("COM8".into()));
    assert_eq!(serial.key, "COM8");
    assert_eq!(serial.identity, "COM8");
    let ble = ResultTarget::from(ResultIdentity::Ble(crate::ble_ota::BleDevice {
        id: "native-device".into(),
        adapter_id: "adapter".into(),
        name: "Tracker".into(),
        address: "AA:BB".into(),
    }));
    assert_eq!(ble.key, "AA:BB");
    let device = target("a", 1, "board");

    let query = OtaQueries::from(QueryResults {
        targets: vec![device],
        failures: vec![
            QueryFailure {
                receiver_key: "a".into(),
                receiver_label: "Receiver A".into(),
                id: crate::esb_ota::RECEIVER_ID,
                error: "receiver timeout".into(),
            },
            QueryFailure {
                receiver_key: "a".into(),
                receiver_label: "Receiver A".into(),
                id: 1,
                error: "tracker timeout".into(),
            },
        ],
    });
    assert_eq!(query.targets.len(), 1);
    assert_eq!(
        query.failures[0].0,
        format!("a:{}", crate::esb_ota::RECEIVER_ID)
    );
    assert_eq!(query.failures[1].0, "a:1");
    assert_eq!(
        query.failures[1].1.1.as_ref().unwrap_err(),
        "tracker timeout"
    );
}

#[test]
fn unconfirmed_activation_finishes_without_failure_activity_or_next_action() {
    use crate::ui::flash::{ActivityKind, Mode, OtaCounts, OtaPresentation, ResultTarget};
    use crate::update::{
        ActivationSubmission, OtaPhase, Progress, ReceiverActivationReport, ReceiverAvailability,
    };
    let mut state = super::support::confirmed_ota_state();
    let action = state.take_confirmed_action().unwrap();
    state.activity_origin = Mode::Esb;
    state.activity_kind = ActivityKind::Operation;
    state.prepare_action_results(&action);
    let device = state
        .targets
        .iter()
        .find(|target| target.id == crate::esb_ota::RECEIVER_ID)
        .unwrap()
        .clone();
    let mut terminal = ota_progress(&device);
    terminal.phase = OtaPhase::ActivationUnconfirmed;
    let mut progress = Progress::stage("");
    progress.ota = vec![terminal];
    let report = ReceiverActivationReport {
        submission: ActivationSubmission::Accepted,
        availability: ReceiverAvailability::Responding,
    };
    state.finish_job_progress(Some(progress));
    state.apply_joined_operation_results(
        vec![(
            ResultTarget::ota(&device),
            UpdateOutcome::ActivationUnconfirmed(report.clone()),
        )],
        std::time::Instant::now(),
    );
    assert_eq!(state.ota_presentation, OtaPresentation::Finished);
    assert!(!state.busy());
    assert!(!state.running_operation(Mode::Esb));
    assert!(!state.floating_activity(Mode::Esb));
    assert!(!state.ota_shows_next());
    assert!(state.shows_results(Mode::Esb));
    assert!(!state.shows_activity(Mode::Esb));
    assert!(crate::ui::flash::progress::ota_terminal(
        OtaPhase::ActivationUnconfirmed
    ));
    assert_eq!(
        OtaCounts::from_progress(&state.last_progress.as_ref().unwrap().ota),
        OtaCounts {
            activation_unconfirmed: 1,
            ..OtaCounts::default()
        }
    );
    state.dismiss_results(Mode::Esb);
    assert!(!state.shows_results(Mode::Esb));
    assert!(!state.ota_shows_next());
    assert_eq!(
        state.results[0].1,
        UpdateOutcome::ActivationUnconfirmed(report)
    );
    state.ota_new_update();
    assert_eq!(state.ota_presentation, OtaPresentation::Configuring);
    assert!(state.ota_shows_next());
}

#[test]
fn activation_result_uses_submission_and_availability_facts_and_retains_detail() {
    use crate::ui::flash::progress::activation_report_tooltip;
    use crate::update::{ActivationSubmission, ReceiverActivationReport, ReceiverAvailability};
    let submitted = ReceiverActivationReport {
        submission: ActivationSubmission::Sent,
        availability: ReceiverAvailability::Unconfirmed("reopen timed out".into()),
    };
    let tooltip = activation_report_tooltip(&submitted, "Receiver identity".into());
    assert!(tooltip.starts_with("Receiver identity\n"));
    assert!(tooltip.contains("reopen timed out"));
    let uncertain = ReceiverActivationReport {
        submission: ActivationSubmission::DeliveryUncertain("short HID write".into()),
        availability: ReceiverAvailability::Responding,
    };
    let tooltip = activation_report_tooltip(&uncertain, "Receiver identity".into());
    assert!(tooltip.starts_with("Receiver identity\n"));
    assert!(tooltip.contains("short HID write"));
}

fn uf2_pending_report() -> crate::update::Uf2CopyReport {
    crate::update::Uf2CopyReport {
        completed: 102400,
        total: 102400,
        stage: crate::update::Uf2FinalizationStage::Flush,
        os_error: Some(433),
        diagnostic: "A device which does not exist was specified. (os error 433)".into(),
    }
}

#[test]
fn mixed_uf2_results_replace_writing_and_survive_volume_changes_and_dismissal() {
    use crate::firmware_update::ResultIdentity;
    use crate::ui::flash::{ActivityKind, ImageCategory, Mode, ResultTarget};
    use crate::update::Progress;
    for category in [ImageCategory::Application, ImageCategory::Bootloader] {
        let directories = (0..3)
            .map(|_| tempfile::tempdir().unwrap())
            .collect::<Vec<_>>();
        let volumes = directories
            .iter()
            .map(|directory| {
                std::fs::write(
                    directory.path().join("INFO_UF2.TXT"),
                    "Board-ID: synthetic\n",
                )
                .unwrap();
                crate::uf2::inspect_volume(directory.path(), "INFO_UF2.TXT").unwrap()
            })
            .collect::<Vec<_>>();
        let report = uf2_pending_report();
        let results = vec![
            (
                ResultTarget::from(ResultIdentity::Uf2(volumes[0].clone())),
                UpdateOutcome::Completed,
            ),
            (
                ResultTarget::from(ResultIdentity::Uf2(volumes[1].clone())),
                UpdateOutcome::Uf2CopyUnconfirmed(report.clone()),
            ),
            (
                ResultTarget::from(ResultIdentity::Reboot(volumes[2].clone())),
                UpdateOutcome::Failed("UF2 copy failed after 31/512 bytes: disconnected".into()),
            ),
        ];
        let mut state = FlashState {
            category,
            activity_origin: Mode::Uf2,
            operation_origin: Mode::Uf2,
            activity_kind: ActivityKind::Operation,
            volumes: volumes.clone(),
            selected_volumes: volumes
                .iter()
                .map(|volume| volume.identity.clone())
                .collect(),
            result_notes: [(volumes[1].identity.clone(), "not proof of success".into())].into(),
            ..FlashState::default()
        };
        state.finish_job_progress(Some(Progress::bytes("Writing REBOOT.UF2", 31, 512)));
        assert!(state.shows_activity(Mode::Uf2));
        state.apply_joined_operation_results(results.clone(), std::time::Instant::now());
        assert!(!state.busy());
        assert!(!state.running_operation(Mode::Uf2));
        assert!(!state.floating_activity(Mode::Uf2));
        assert!(!state.shows_activity(Mode::Uf2));
        assert!(
            state.last_progress.is_none(),
            "terminal results supersede Writing"
        );
        assert!(state.shows_results(Mode::Uf2));
        assert!(!state.shows_results(Mode::Esb));
        assert_eq!(state.results, results);
        for (result, volume) in state.results.iter().zip(&volumes) {
            assert_eq!(result.0.key, volume.identity);
            assert!(result.0.receiver.is_none());
        }
        assert_eq!(
            state
                .results
                .iter()
                .filter(|(_, outcome)| matches!(outcome, UpdateOutcome::Completed))
                .count(),
            1
        );
        assert!(crate::ui::flash::progress::result_receiver_groups(&state.results).is_empty());
        state.apply_volumes(vec![]);
        assert_eq!(
            state.results, results,
            "disappearance is not proof of firmware completion"
        );
        assert!(!state.shows_activity(Mode::Uf2));
        state.dismiss_results(Mode::Uf2);
        assert!(!state.shows_results(Mode::Uf2));
        state.apply_volumes(volumes);
        assert!(!state.shows_results(Mode::Uf2));
        assert!(!state.shows_activity(Mode::Uf2));
        assert_eq!(state.results, results);
        assert_eq!(
            state.results[1].1,
            UpdateOutcome::Uf2CopyUnconfirmed(report)
        );
    }
}

#[test]
fn every_authoritative_uf2_terminal_result_removes_stale_progress() {
    use crate::ui::flash::{ActivityKind, Mode, ResultTarget};
    use crate::update::Progress;
    for outcome in [
        UpdateOutcome::Completed,
        UpdateOutcome::Uf2CopyUnconfirmed(uf2_pending_report()),
        UpdateOutcome::Failed("UF2 copy failed after 65535/65536 bytes: disconnected".into()),
    ] {
        let mut state = FlashState {
            activity_origin: Mode::Uf2,
            operation_origin: Mode::Uf2,
            activity_kind: ActivityKind::Operation,
            ..FlashState::default()
        };
        state.finish_job_progress(Some(Progress::bytes("Writing firmware.uf2", 65535, 65536)));
        state.apply_joined_operation_results(
            vec![(ResultTarget::generic("disk".into(), "Disk".into()), outcome)],
            std::time::Instant::now(),
        );
        assert!(state.last_progress.is_none());
        assert!(!state.shows_activity(Mode::Uf2));
        assert!(state.shows_results(Mode::Uf2));
        state.dismiss_results(Mode::Uf2);
        assert!(!state.shows_results(Mode::Uf2));
        assert!(!state.shows_activity(Mode::Uf2));
    }
}

#[test]
fn active_uf2_job_keeps_its_live_progress_until_authoritative_results_arrive() {
    use crate::firmware_update::UpdateCompletion;
    use crate::ui::flash::{ActivityKind, Mode, Payload, ResultTarget};
    use crate::update::{Job, Progress};
    use std::sync::mpsc;
    let (finish, wait) = mpsc::channel();
    let mut state = FlashState {
        activity_origin: Mode::Uf2,
        operation_origin: Mode::Uf2,
        activity_kind: ActivityKind::Operation,
        job: Some(
            Job::start(move |_, _| {
                wait.recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap();
                Ok(Payload::Update(UpdateCompletion::Results(vec![])))
            })
            .unwrap(),
        ),
        ..FlashState::default()
    };
    state.job.as_mut().unwrap().progress =
        Some(Progress::bytes("Writing firmware.uf2", 65536, 102400));
    assert!(state.running_operation(Mode::Uf2));
    assert!(state.shows_activity(Mode::Uf2));
    assert_eq!(
        state
            .job
            .as_ref()
            .unwrap()
            .progress
            .as_ref()
            .unwrap()
            .completed,
        65536
    );
    finish.send(()).unwrap();
    while state.job.as_mut().unwrap().poll().is_none() {
        std::thread::yield_now();
    }
    let progress = state.job.as_mut().unwrap().progress.take();
    state.job = None;
    state.finish_job_progress(progress);
    state.apply_joined_operation_results(
        vec![(
            ResultTarget::generic("disk".into(), "Disk".into()),
            UpdateOutcome::Uf2CopyUnconfirmed(uf2_pending_report()),
        )],
        std::time::Instant::now(),
    );
    assert!(!state.running_operation(Mode::Uf2));
    assert!(!state.shows_activity(Mode::Uf2));
    assert!(state.last_progress.is_none());
}

#[test]
fn uf2_pending_details_retain_counts_and_os_diagnostic_for_each_finalization_stage() {
    let mut report = uf2_pending_report();
    for stage in [
        crate::update::Uf2FinalizationStage::Flush,
        crate::update::Uf2FinalizationStage::SyncAll,
    ] {
        report.stage = stage;
        let details = crate::ui::flash::progress::uf2_report_details(&report);
        assert!(details.contains("102400/102400"), "{details}");
        assert!(details.contains(&report.diagnostic), "{details}");
        assert!(details.contains("433"), "{details}");
    }
}
