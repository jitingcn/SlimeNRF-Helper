use super::support::target;
use crate::ui::flash::{Action, FlashState, OtaQueryPlan};
use crate::update::UpdateOutcome;
use crate::{
    app_runtime::AppRuntime,
    firmware_update::{ResultIdentity, UpdateCompletion},
    settings::{Language, Preferences},
    ui::{
        FeaturePage, Workbench,
        flash::{ActivityKind, Mode, Payload, ResultTarget},
        test_support::{UiTestSession, offline_workbench},
    },
    update::{Job, Progress},
};
use gpui_kit::{
    AnyWindowHandle, AppContext as _, Bounds, Context, Entity, IntoElement, Pixels, Render,
    ScrollDelta, ScrollHandle, SharedString, TestAppContext, Window, WindowBounds, WindowOptions,
    div, point,
    prelude::*,
    px, size,
    test::{TestSupportExt as _, TestWindowExt as _},
};
use std::{
    path::PathBuf,
    sync::{atomic::Ordering, mpsc},
    time::Duration,
};

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

#[gpui_kit::test]
fn activation_summary_is_neutral_in_both_locales_and_preserves_evidence(cx: &mut TestAppContext) {
    use crate::ui::flash::progress::{activation_report_label, activation_report_tooltip};
    use crate::update::{ActivationSubmission, ReceiverActivationReport, ReceiverAvailability};

    let _ui_session = UiTestSession::new(cx);
    let pending = ReceiverActivationReport {
        submission: ActivationSubmission::Sent,
        availability: ReceiverAvailability::Unconfirmed("disconnect detail".into()),
    };
    let detected = ReceiverActivationReport {
        submission: ActivationSubmission::Accepted,
        availability: ReceiverAvailability::Detected {
            prior_diagnostic: "earlier disconnect".into(),
        },
    };
    let uncertain = ReceiverActivationReport {
        submission: ActivationSubmission::DeliveryUncertain("short HID write".into()),
        availability: ReceiverAvailability::Responding,
    };
    for (locale, summary, phase, pending_label, detected_label, uncertain_label) in [
        (
            "en",
            "Transfer complete; activation not confirmed",
            "Activation unconfirmed",
            "Activation submitted · Receiver availability unconfirmed",
            "Activation accepted · Receiver detected after update",
            "Activation delivery uncertain · Receiver responding",
        ),
        (
            "zh-CN",
            "传输完成；激活未确认",
            "激活未确认",
            "已提交激活 · 尚未确认接收器是否可用",
            "已接受激活 · 更新后已检测到接收器",
            "激活送达情况不确定 · 接收器已响应",
        ),
    ] {
        rust_i18n::set_locale(locale);
        assert_eq!(
            rust_i18n::t!("firmware.result.activation.unconfirmed"),
            summary
        );
        assert_eq!(
            rust_i18n::t!("firmware.phase.activation_unconfirmed"),
            phase
        );
        assert_eq!(activation_report_label(&pending), pending_label);
        assert_eq!(activation_report_label(&detected), detected_label);
        assert_eq!(activation_report_label(&uncertain), uncertain_label);
        assert!(
            activation_report_tooltip(&pending, "Receiver".into()).contains("disconnect detail")
        );
        assert!(
            activation_report_tooltip(&detected, "Receiver".into()).contains("earlier disconnect")
        );
        assert!(
            activation_report_tooltip(&uncertain, "Receiver".into()).contains("short HID write")
        );
    }
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

#[test]
fn wired_percentage_is_unknown_without_total_and_never_rounds_up_early() {
    use crate::ui::flash::progress::wired_transfer_percent;

    assert_eq!(wired_transfer_percent(&Progress::stage("waiting")), None);
    assert_eq!(
        wired_transfer_percent(&Progress::bytes("waiting", 5, 0)),
        None
    );
    assert_eq!(
        wired_transfer_percent(&Progress::bytes("writing", 0, 102400)),
        Some(0.)
    );
    assert_eq!(
        wired_transfer_percent(&Progress::bytes("writing", 65536, 102400)),
        Some(64.)
    );
    assert_eq!(
        wired_transfer_percent(&Progress::bytes("writing", u64::MAX - 1, u64::MAX)),
        Some(100.0_f32.next_down())
    );
    assert_eq!(
        wired_transfer_percent(&Progress::bytes("writing", 102400, 102400)),
        Some(100.)
    );
    assert_eq!(
        wired_transfer_percent(&Progress::bytes("writing", 102401, 102400)),
        Some(100.)
    );
}

struct ProgressHarness {
    workbench: Entity<Workbench>,
    scroll: ScrollHandle,
}

impl Render for ProgressHarness {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.workbench.update(cx, |this, cx| {
            let mut content = div()
                .id("progress-fixture-viewport")
                .test_support()
                .size_full()
                .min_w_0()
                .min_h_0()
                .flex()
                .flex_col()
                .gap_3()
                .overflow_y_scroll()
                .overflow_x_hidden()
                .track_scroll(&self.scroll);
            if this.flash.shows_activity(this.flash_mode()) {
                content = content.child(this.flash_activity(cx));
            }
            if this.flash.shows_results(this.flash_mode()) {
                content = content.child(this.flash_results(cx));
            }
            if this.flash.floating_activity(this.flash_mode()) {
                content = content.child(this.flash_activity_toast(cx));
            }
            div().size_full().p_3().child(content)
        })
    }
}

struct ProgressFixture {
    window: AnyWindowHandle,
    harness: Entity<ProgressHarness>,
    workbench: Entity<Workbench>,
    scroll: ScrollHandle,
}

fn progress_fixture(
    cx: &mut TestAppContext,
    runtime: &AppRuntime,
    preferences_path: PathBuf,
) -> ProgressFixture {
    let executor = runtime.executor();
    let scroll = ScrollHandle::new();
    let view_scroll = scroll.clone();
    let (window, harness) = cx.update(|cx| {
        gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    Default::default(),
                    size(px(740.), px(540.)),
                ))),
                ..Default::default()
            },
            cx,
            move |window, cx| {
                let workbench = cx.new(|cx| {
                    let mut this = offline_workbench(
                        window,
                        cx,
                        crate::sessions::SessionManager::new(),
                        executor,
                    );
                    this.active_page = FeaturePage::Uf2;
                    this.preferences_io =
                        crate::ui::preferences::PreferenceIo::for_test(preferences_path);
                    this
                });
                cx.new(|_| ProgressHarness {
                    workbench,
                    scroll: view_scroll,
                })
            },
        )
        .expect("open the production firmware activity and results components")
    });
    let workbench = cx.update(|cx| harness.read(cx).workbench.clone());
    cx.update_window(window, |_, window, _| window.activate_window())
        .expect("activate the firmware fixture window");
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        assert!(window.is_window_active(), "fixture window must be active");
        window.render_frame(cx);
    })
    .expect("render the activated firmware fixture");
    ProgressFixture {
        window,
        harness,
        workbench,
        scroll,
    }
}

fn settle_progress(cx: &mut TestAppContext, fixture: &ProgressFixture) {
    fixture.harness.update(cx, |_, cx| cx.notify());
    for _ in 0..2 {
        cx.run_until_parked();
        cx.update_window(fixture.window, |_, window, cx| {
            window.simulate_next_frame(cx);
            window.render_frame(cx);
        })
        .expect("settle firmware fixture frames");
    }
}

fn assert_horizontal_containment(
    window: &Window,
    outer: Bounds<Pixels>,
    inner: Bounds<Pixels>,
    context: &str,
) {
    assert!(
        inner.size.width > px(0.)
            && window.pixel_snap(inner.left()) >= window.pixel_snap(outer.left())
            && window.pixel_snap(inner.right()) <= window.pixel_snap(outer.right()),
        "{context} must fit horizontally: outer {outer:?}, inner {inner:?}",
    );
}

fn assert_vertical_containment(
    window: &Window,
    outer: Bounds<Pixels>,
    inner: Bounds<Pixels>,
    context: &str,
) {
    assert!(
        inner.size.height > px(0.)
            && window.pixel_snap(inner.top()) >= window.pixel_snap(outer.top())
            && window.pixel_snap(inner.bottom()) <= window.pixel_snap(outer.bottom()),
        "{context} must fit vertically: outer {outer:?}, inner {inner:?}",
    );
}

fn publish_progress(
    cx: &mut TestAppContext,
    fixture: &ProgressFixture,
    updates: &mpsc::Sender<Progress>,
    published: &mpsc::Receiver<()>,
    progress: Progress,
) {
    updates.send(progress).expect("request synthetic progress");
    published
        .recv_timeout(Duration::from_secs(3))
        .expect("synthetic worker publishes progress");
    fixture.workbench.update(cx, |this, cx| {
        assert!(this.poll_flash());
        assert!(this.flash.job.is_some());
        assert!(this.flash.results.is_empty());
        cx.notify();
    });
    settle_progress(cx, fixture);
}

fn assert_live_progress_layout(
    window: &Window,
    expected_stage: &str,
    expected_status: &str,
    cancel_label: &str,
) -> Pixels {
    let viewport = window.find("progress-fixture-viewport").bounds();
    let message = window.find("flash-progress-message");
    let bar = window.find("flash-progress-bar");
    let cancel = window.find("flash-cancel");
    assert_eq!(message.role(), Some(gpui_kit::Role::Status));
    assert_eq!(message.label(), Some(expected_status));
    assert_eq!(bar.role(), Some(gpui_kit::Role::ProgressIndicator));
    assert_eq!(bar.label(), Some(expected_stage));
    assert_eq!(cancel.label(), Some(cancel_label));
    assert!(message.visible() && bar.visible() && cancel.visible());
    for (name, bounds) in [
        ("progress message", message.bounds()),
        ("progress bar", bar.bounds()),
        ("cancel", cancel.bounds()),
    ] {
        assert_horizontal_containment(window, viewport, bounds, name);
        assert_vertical_containment(window, viewport, bounds, name);
    }
    assert!(bar.bounds().size.width > px(120.));
    assert!(bar.bounds().size.height <= px(10.));
    assert!(message.bounds().bottom() <= bar.bounds().top());
    assert!(bar.bounds().bottom() <= cancel.bounds().top());
    assert!(window.try_find("flash-results-panel").is_none());
    viewport.size.width
}

#[gpui_kit::test]
fn synthetic_uf2_progress_cancellation_and_results_survive_locales_and_resize(
    cx: &mut TestAppContext,
) {
    // The runtime outlives the session's real window/context cleanup, even on unwind.
    let runtime = AppRuntime::new().expect("create isolated I/O runtime");
    let private_preferences = tempfile::tempdir().expect("create isolated preference directory");
    let _ui_session = UiTestSession::new(cx);
    let fixture = progress_fixture(
        cx,
        &runtime,
        private_preferences.path().join("preferences.json"),
    );
    cx.simulate_window_scale_factor_change(fixture.window, 1.25);

    // Inspect only private temporary markers; neither discovery nor any firmware write runs.
    let directories: Vec<_> = (0..3)
        .map(|_| tempfile::tempdir().expect("create synthetic UF2 directory"))
        .collect();
    let volumes: Vec<_> = directories
        .iter()
        .enumerate()
        .map(|(index, directory)| {
            std::fs::write(
                directory.path().join("INFO_UF2.TXT"),
                "Board-ID: synthetic\n",
            )
            .expect("write synthetic UF2 marker");
            let mut volume = crate::uf2::inspect_volume(directory.path(), "INFO_UF2.TXT")
                .expect("freeze synthetic UF2 identity");
            // These are presentation-only names; the worker never accesses the paths.
            volume.label = format!("Synthetic target {}", index + 1);
            volume.mount = PathBuf::from(format!("fixture/target-{}", index + 1));
            volume
        })
        .collect();
    let ids: Vec<_> = volumes
        .iter()
        .map(|volume| SharedString::from(format!("firmware-result-{}", volume.identity)))
        .collect();
    let details_id = SharedString::from(format!("firmware-result-details-{}", volumes[1].identity));
    let diagnostic = "The synthetic target disconnected after the bytes were submitted; verify the firmware version before retrying. "
        .repeat(12);
    let mut report = uf2_pending_report();
    report.diagnostic = diagnostic.clone();
    let outcomes = [
        UpdateOutcome::Completed,
        UpdateOutcome::Uf2CopyUnconfirmed(report),
        UpdateOutcome::Failed("Synthetic target disconnected before the copy completed".into()),
    ];

    for (language, cancel_label, dismiss_label, waiting_label) in [
        (
            Language::English,
            "Cancel",
            "Dismiss results",
            "Waiting for final result",
        ),
        (
            Language::SimplifiedChinese,
            "取消",
            "关闭结果",
            "等待最终结果",
        ),
    ] {
        let preferences = Preferences {
            language,
            ..Preferences::default()
        };
        cx.update(|cx| {
            crate::ui::theme::apply(&preferences, cx);
            gpui_kit::component::Theme::update(cx, |theme| theme.font_size = px(20.));
        });
        let expected: Vec<_> = volumes
            .iter()
            .cloned()
            .zip(outcomes.iter().cloned())
            .map(|(volume, outcome)| (ResultTarget::from(ResultIdentity::Uf2(volume)), outcome))
            .collect();
        let worker_results: Vec<_> = volumes
            .iter()
            .cloned()
            .zip(outcomes.iter().cloned())
            .map(|(volume, outcome)| (ResultIdentity::Uf2(volume), outcome))
            .collect();
        let (updates, wait_for_update) = mpsc::channel();
        let (published, progress_published) = mpsc::channel();
        let (cancelled, cancel_observed) = mpsc::channel();
        let (release, wait_for_release) = mpsc::channel();
        let job = Job::start(move |cancel, emit| {
            while !cancel.load(Ordering::Acquire) {
                match wait_for_update.recv_timeout(Duration::from_millis(1)) {
                    Ok(progress) => {
                        emit(progress);
                        let _ = published.send(());
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        return Err("synthetic progress channel closed".to_owned());
                    }
                }
            }
            let _ = cancelled.send(());
            wait_for_release
                .recv_timeout(Duration::from_secs(5))
                .map_err(|_| "synthetic worker was not released".to_owned())?;
            Ok(Payload::Update(UpdateCompletion::Results(worker_results)))
        })
        .expect("start synthetic held firmware job");
        fixture.workbench.update(cx, |this, cx| {
            this.preferences = preferences;
            this.flash = FlashState {
                activity_origin: Mode::Uf2,
                operation_origin: Mode::Uf2,
                activity_kind: ActivityKind::Operation,
                result_success: "firmware.result.copy_completed",
                job: Some(job),
                ..FlashState::default()
            };
            cx.notify();
        });
        cx.simulate_window_resize(fixture.window, size(px(740.), px(540.)));
        fixture.scroll.set_offset(Default::default());
        publish_progress(
            cx,
            &fixture,
            &updates,
            &progress_published,
            Progress::stage("Preparing firmware"),
        );
        cx.update_window(fixture.window, |_, window, _| {
            assert_eq!(window.rem_size(), px(20.), "{language:?}");
            assert_live_progress_layout(
                window,
                "Preparing firmware",
                "Preparing firmware",
                cancel_label,
            );
        })
        .expect("inspect unknown-total preparation");
        publish_progress(
            cx,
            &fixture,
            &updates,
            &progress_published,
            Progress::bytes("Writing test.uf2", 0, 102400),
        );
        cx.update_window(fixture.window, |_, window, _| {
            assert_live_progress_layout(
                window,
                "Writing test.uf2",
                "Writing test.uf2 · 0 / 102400 B (0%)",
                cancel_label,
            );
        })
        .expect("inspect transfer start");
        publish_progress(
            cx,
            &fixture,
            &updates,
            &progress_published,
            Progress::bytes("Writing test.uf2", 65536, 102400),
        );
        let wide = cx
            .update_window(fixture.window, |_, window, _| {
                assert_live_progress_layout(
                    window,
                    "Writing test.uf2",
                    "Writing test.uf2 · 65536 / 102400 B (64%)",
                    cancel_label,
                )
            })
            .expect("inspect wide transferring layout");
        publish_progress(
            cx,
            &fixture,
            &updates,
            &progress_published,
            Progress::bytes("Writing test.uf2", 102400, 102400),
        );
        let full_status = format!("Writing test.uf2 · 102400 / 102400 B (100%) · {waiting_label}");

        // This intentionally stresses the component below the application's minimum window.
        cx.simulate_window_resize(fixture.window, size(px(420.), px(360.)));
        settle_progress(cx, &fixture);
        cx.update_window(fixture.window, |_, window, cx| {
            let narrow =
                assert_live_progress_layout(window, "Writing test.uf2", &full_status, cancel_label);
            assert!(narrow < wide, "{language:?}");
            window.click("flash-cancel", cx);
        })
        .expect("click the real narrow-layout cancel button");
        cancel_observed
            .recv_timeout(Duration::from_secs(3))
            .expect("the actual worker receives cancellation");
        fixture.workbench.update(cx, |this, cx| {
            assert!(this.poll_flash());
            assert!(
                this.flash.job.is_some() && this.flash.running_operation(Mode::Uf2),
                "cancellation must not release the still-held worker"
            );
            assert!(this.flash.results.is_empty());
            cx.notify();
        });
        settle_progress(cx, &fixture);
        cx.update_window(fixture.window, |_, window, _| {
            assert_live_progress_layout(window, "Writing test.uf2", &full_status, cancel_label);
        })
        .expect("the full transfer remains pending until the held worker joins");
        release.send(()).expect("release the synthetic worker");
        fixture.workbench.update(cx, |this, cx| {
            crate::sessions::test_support::until(|| {
                this.poll_flash();
                this.flash.job.is_none()
            });
            assert!(!this.flash.running_operation(Mode::Uf2));
            assert!(!this.flash.shows_activity(Mode::Uf2));
            assert!(
                this.flash.last_progress.is_none(),
                "UF2 results replace Writing"
            );
            assert!(this.flash.shows_results(Mode::Uf2));
            assert_eq!(this.flash.results, expected);
            assert!(this.flash.volume_scan.is_none());
            assert!(!this.flash.catalog_loaded);
            cx.notify();
        });
        settle_progress(cx, &fixture);

        cx.update_window(fixture.window, |_, window, cx| {
            assert!(window.try_find("flash-progress-message").is_none());
            assert!(window.try_find("flash-progress-bar").is_none());
            assert!(window.try_find("flash-cancel").is_none());
            let viewport = window.find("progress-fixture-viewport").bounds();
            let panel = window.find("flash-results-panel").bounds();
            let dismiss = window.find("flash-results-dismiss");
            let success = window.find(ids[0].clone()).bounds();
            let pending = window.find(ids[1].clone()).bounds();
            let details = window.find(details_id.clone()).bounds();
            assert_eq!(dismiss.label(), Some(dismiss_label), "{language:?}");
            assert_horizontal_containment(window, viewport, panel, "results panel");
            assert_horizontal_containment(window, panel, dismiss.bounds(), "dismiss");
            assert_vertical_containment(window, viewport, dismiss.bounds(), "dismiss");
            for (name, bounds) in [
                ("success", success),
                ("pending", pending),
                ("diagnostic", details),
            ] {
                assert_horizontal_containment(window, panel, bounds, name);
            }
            assert!(success.bottom() <= pending.top(), "{language:?}");
            assert!(pending.bottom() <= details.top(), "{language:?}");
            assert!(
                details.size.height > success.size.height * 2.,
                "the long diagnostic must wrap in the narrow results panel: {language:?}"
            );

            let extent = fixture.scroll.max_offset().y;
            assert!(
                extent > px(0.),
                "long diagnostic must overflow the viewport"
            );
            window.scroll(
                "progress-fixture-viewport",
                ScrollDelta::Pixels(point(px(0.), -extent * 2.)),
                cx,
            );
            assert!(fixture.scroll.offset().y < px(0.));
            let viewport = window.find("progress-fixture-viewport").bounds();
            let failed = window.find(ids[2].clone());
            let panel = window.find("flash-results-panel").bounds();
            let details = window.find(details_id.clone()).bounds();
            assert_horizontal_containment(window, panel, failed.bounds(), "failure");
            assert!(details.bottom() <= failed.bounds().top(), "{language:?}");
            assert!(failed.visible(), "scrolling must reveal the failed result");
            assert_vertical_containment(window, viewport, failed.bounds(), "scrolled failure");
            window.scroll(
                "progress-fixture-viewport",
                ScrollDelta::Pixels(point(px(0.), extent * 2.)),
                cx,
            );
            let dismiss = window.find("flash-results-dismiss");
            assert!(dismiss.visible(), "scrolling back must reveal dismiss");
            assert_vertical_containment(window, viewport, dismiss.bounds(), "restored dismiss");
            window.click("flash-results-dismiss", cx);
        })
        .expect("scroll and dismiss actual mixed result components");
        settle_progress(cx, &fixture);
        fixture.workbench.read_with(cx, |this, _| {
            assert!(this.flash.operation_results_dismissed);
            assert!(!this.flash.shows_results(Mode::Uf2));
            assert!(this.flash.last_progress.is_none());
            assert_eq!(
                this.flash.results, expected,
                "dismissal must hide the results without erasing any outcomes"
            );
            assert!(matches!(
                &this.flash.results[1].1,
                UpdateOutcome::Uf2CopyUnconfirmed(report) if report.diagnostic == diagnostic
            ));
        });
        cx.update_window(fixture.window, |_, window, _| {
            assert!(window.try_find("flash-results-panel").is_none());
            assert!(window.try_find("flash-results-dismiss").is_none());
            assert!(window.try_find("flash-progress-message").is_none());
            assert!(window.try_find("flash-progress-bar").is_none());
        })
        .expect("inspect dismissed results");
    }
}

#[gpui_kit::test]
fn synthetic_opendfu_progress_tracks_transfer_until_the_worker_joins(cx: &mut TestAppContext) {
    let runtime = AppRuntime::new().expect("create isolated I/O runtime");
    let private_preferences = tempfile::tempdir().expect("create isolated preference directory");
    let _ui_session = UiTestSession::new(cx);
    let fixture = progress_fixture(
        cx,
        &runtime,
        private_preferences.path().join("preferences.json"),
    );
    let preferences = Preferences {
        language: Language::English,
        ..Preferences::default()
    };
    cx.update(|cx| crate::ui::theme::apply(&preferences, cx));
    let (updates, wait_for_update) = mpsc::channel();
    let (published, progress_published) = mpsc::channel();
    let job = Job::start(move |cancel, emit| {
        loop {
            if cancel.load(Ordering::Acquire) {
                return Err("synthetic OpenDFU worker cancelled".to_owned());
            }
            match wait_for_update.recv_timeout(Duration::from_millis(1)) {
                Ok(progress) => {
                    emit(progress);
                    let _ = published.send(());
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
        Ok(Payload::Update(UpdateCompletion::Results(vec![(
            ResultIdentity::Serial("fixture-port".into()),
            UpdateOutcome::Completed,
        )])))
    })
    .expect("start synthetic OpenDFU worker");
    fixture.workbench.update(cx, |this, cx| {
        this.preferences = preferences;
        this.flash = FlashState {
            mode: Mode::OpenDfu,
            activity_origin: Mode::OpenDfu,
            operation_origin: Mode::OpenDfu,
            activity_kind: ActivityKind::Operation,
            job: Some(job),
            ..FlashState::default()
        };
        cx.notify();
    });

    for (progress, stage, status) in [
        (
            Progress::stage("Waiting for the receiver console"),
            "Waiting for the receiver console",
            "Waiting for the receiver console",
        ),
        (
            Progress::bytes("Writing test.bin", 1024, 0),
            "Writing test.bin",
            "Writing test.bin · 1024 B",
        ),
        (
            Progress::bytes("Connecting to OpenDFU: test.bin", 0, 4096),
            "Connecting to OpenDFU: test.bin",
            "Connecting to OpenDFU: test.bin · 0 / 4096 B (0%)",
        ),
        (
            Progress::bytes("Writing test.bin", 1024, 4096),
            "Writing test.bin",
            "Writing test.bin · 1024 / 4096 B (25%)",
        ),
        (
            Progress::bytes("Writing test.bin", 4095, 4096),
            "Writing test.bin",
            "Writing test.bin · 4095 / 4096 B (99%)",
        ),
        (
            Progress::bytes("Writing test.bin", 4096, 4096),
            "Writing test.bin",
            "Writing test.bin · 4096 / 4096 B (100%) · Waiting for final result",
        ),
    ] {
        publish_progress(cx, &fixture, &updates, &progress_published, progress);
        cx.update_window(fixture.window, |_, window, _| {
            assert_live_progress_layout(window, stage, status, "Cancel");
        })
        .expect("inspect the live OpenDFU stage");
    }
    fixture.workbench.read_with(cx, |this, _| {
        assert!(this.flash.running_operation(Mode::OpenDfu));
        assert!(this.flash.results.is_empty());
    });
    drop(updates);
    fixture.workbench.update(cx, |this, cx| {
        crate::sessions::test_support::until(|| {
            this.poll_flash();
            this.flash.job.is_none()
        });
        assert!(!this.flash.running_operation(Mode::OpenDfu));
        assert_eq!(
            this.flash.results,
            vec![(
                ResultTarget::from(ResultIdentity::Serial("fixture-port".into())),
                UpdateOutcome::Completed,
            )]
        );
        cx.notify();
    });
    settle_progress(cx, &fixture);
    cx.update_window(fixture.window, |_, window, _| {
        assert!(window.try_find("flash-progress-bar").is_none());
        assert!(window.try_find("flash-cancel").is_none());
        assert!(window.find("flash-results-panel").visible());
        assert!(window.find("firmware-result-fixture-port").visible());
    })
    .expect("inspect the joined OpenDFU result");
}

#[gpui_kit::test]
fn query_progress_keeps_item_counts_without_a_wired_byte_bar(cx: &mut TestAppContext) {
    let runtime = AppRuntime::new().expect("create isolated I/O runtime");
    let private_preferences = tempfile::tempdir().expect("create isolated preference directory");
    let _ui_session = UiTestSession::new(cx);
    let fixture = progress_fixture(
        cx,
        &runtime,
        private_preferences.path().join("preferences.json"),
    );
    let (published, progress_published) = mpsc::channel();
    let job = Job::start(move |cancel, emit| {
        emit(Progress::bytes("Querying firmware information", 2, 8));
        let _ = published.send(());
        while !cancel.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_millis(1));
        }
        Ok(Payload::Update(UpdateCompletion::Queries(
            crate::firmware_update::QueryResults {
                targets: Vec::new(),
                failures: Vec::new(),
            },
        )))
    })
    .expect("start synthetic query");
    fixture.workbench.update(cx, |this, cx| {
        this.active_page = FeaturePage::Ota;
        this.flash = FlashState {
            mode: Mode::Esb,
            activity_origin: Mode::Esb,
            activity_kind: ActivityKind::Query,
            job: Some(job),
            ..FlashState::default()
        };
        cx.notify();
    });
    progress_published
        .recv_timeout(Duration::from_secs(3))
        .expect("publish synthetic query counts");
    fixture.workbench.update(cx, |this, cx| {
        assert!(this.poll_flash());
        assert!(this.flash.floating_activity(Mode::Esb));
        cx.notify();
    });
    settle_progress(cx, &fixture);
    cx.update_window(fixture.window, |_, window, cx| {
        let message = window.find("flash-progress-message");
        assert_eq!(
            message.label(),
            Some("Querying firmware information · 2 / 8 (25%)")
        );
        assert!(message.visible());
        assert!(window.try_find("flash-progress-bar").is_none());
        window.click("flash-cancel", cx);
    })
    .expect("inspect and cancel the query activity");
    fixture.workbench.update(cx, |this, cx| {
        crate::sessions::test_support::until(|| {
            this.poll_flash();
            this.flash.job.is_none()
        });
        assert!(!this.flash.floating_activity(Mode::Esb));
        cx.notify();
    });
}
