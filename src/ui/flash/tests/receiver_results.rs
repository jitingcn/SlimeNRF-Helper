use super::support::{local_image, target};
use crate::app_runtime::{AppRuntime, Executor};
use crate::discovery::ReceiverMonitor;
use crate::domain::{Inventory, ReceiverEndpoint, SerialEndpoint, SerialPortKind};
use crate::esb_ota::{RECEIVER_ID, Target};
use crate::firmware_update::{QueryResults, ResultIdentity, UpdateCompletion};
use crate::sessions::test_support::{display_manager, until};
use crate::ui::flash::progress::result_receiver_groups;
use crate::ui::flash::{
    Action, ActivityKind, Lease, Mode, OperationPins, OtaCounts, OtaPlanGroup, OtaPresentation,
    OtaQueryPlan, Payload,
};
use crate::ui::test_support::offline_workbench;
use crate::ui::{FeaturePage, Workbench};
use crate::update::{
    ActivationSubmission, Job, OtaPhase, OtaTargetProgress, Progress, ReceiverActivationReport,
    ReceiverAvailability, Uf2CopyReport, Uf2FinalizationStage, UpdateOutcome,
};
use gpui_kit::{Entity, TestAppContext, VisualContext as _, VisualTestContext};
use std::sync::{atomic::Ordering, mpsc};
use std::time::{Duration, Instant};

const SERIAL: &str = "0123456789ABCDEF";
const OTHER_SERIAL: &str = "FEDCBA9876543210";
const DIAGNOSTIC: &str = "activation-time receiver reopen timed out";

fn receiver(path: &str, serial: Option<&str>) -> ReceiverEndpoint {
    ReceiverEndpoint {
        key: path.into(),
        path: path.as_bytes().to_vec(),
        label: "Synthetic receiver".into(),
        serial_number: serial.map(str::to_owned),
    }
}

fn device(endpoint: &ReceiverEndpoint, id: u8) -> Target {
    Target {
        receiver: endpoint.clone(),
        ..target(&endpoint.key, id, "synthetic-board")
    }
}

fn inventory(receivers: Vec<ReceiverEndpoint>) -> Inventory {
    Inventory {
        receivers,
        ..Inventory::default()
    }
}

fn unconfirmed() -> UpdateOutcome {
    UpdateOutcome::ActivationUnconfirmed(ReceiverActivationReport {
        submission: ActivationSubmission::Accepted,
        availability: ReceiverAvailability::Unconfirmed(DIAGNOSTIC.into()),
    })
}

fn detected() -> UpdateOutcome {
    UpdateOutcome::ActivationUnconfirmed(ReceiverActivationReport {
        submission: ActivationSubmission::Accepted,
        availability: ReceiverAvailability::Detected {
            prior_diagnostic: DIAGNOSTIC.into(),
        },
    })
}

fn offline(cx: &mut VisualTestContext, executor: Executor) -> Entity<Workbench> {
    cx.new_window_entity(move |window, cx| {
        let mut this = offline_workbench(window, cx, display_manager(), executor);
        this.monitor = ReceiverMonitor::fixture(
            receiver("synthetic-old-native-path", Some(SERIAL)),
            0,
            "112233445566",
            true,
        );
        this.inventory_generation = 1;
        this
    })
}

fn prepare_operation(this: &mut Workbench, rows: &[(ResultIdentity, UpdateOutcome)]) -> Progress {
    let targets: Vec<_> = rows
        .iter()
        .filter_map(|(identity, _)| match identity {
            ResultIdentity::Esb(target) => Some(target.clone()),
            _ => None,
        })
        .collect();
    this.flash.mode = Mode::Esb;
    this.flash.activity_origin = Mode::Esb;
    this.flash.activity_kind = ActivityKind::Operation;
    this.flash
        .prepare_action_results(&Action::EsbGroups(vec![OtaPlanGroup {
            targets,
            image: local_image("synthetic.hex"),
        }]));
    let mut progress = Progress::stage("");
    progress.ota = rows
        .iter()
        .filter_map(|(identity, outcome)| {
            let ResultIdentity::Esb(target) = identity else {
                return None;
            };
            Some(OtaTargetProgress {
                receiver_key: target.receiver.key.clone(),
                id: target.id,
                phase: match outcome {
                    UpdateOutcome::Completed => OtaPhase::Completed,
                    UpdateOutcome::ActivationUnconfirmed(_) => OtaPhase::ActivationUnconfirmed,
                    UpdateOutcome::Failed(_) => OtaPhase::Failed,
                    UpdateOutcome::Uf2CopyUnconfirmed(_) => {
                        unreachable!("UF2 outcome on ESB fixture")
                    }
                },
                preparation: None,
                error: None,
                completed: 4,
                total: 4,
                bytes_per_second: None,
                eta_seconds: None,
            })
        })
        .collect();
    progress
}

fn hold_operation(
    this: &mut Workbench,
    rows: Vec<(ResultIdentity, UpdateOutcome)>,
) -> mpsc::Sender<()> {
    assert!(this.flash.job.is_none());
    let progress = prepare_operation(this, &rows);
    let (release, wait) = mpsc::channel();
    this.flash.job = Some(
        Job::start(move |_, emit| {
            emit(progress);
            wait.recv_timeout(Duration::from_secs(3)).unwrap();
            Ok(Payload::Update(UpdateCompletion::Results(rows)))
        })
        .unwrap(),
    );
    release
}

fn join_operation(this: &mut Workbench, release: mpsc::Sender<()>) {
    release.send(()).unwrap();
    until(|| {
        let _ = this.poll_flash();
        this.flash.job.is_none()
    });
}

fn assert_idle(this: &mut Workbench) {
    assert!(this.flash.job.is_none());
    assert!(this.flash.pending.is_none());
    let _ = this.poll_flash();
    assert!(this.flash.job.is_none());
    assert!(this.flash.pending.is_none());
    assert!(this.flash.volume_scan.is_none());
    assert!(this.flash.default_job.is_none());
    assert!(this.flash.leases.is_empty());
    assert!(!this.flash.busy());
}

#[gpui_kit::test]
fn fresh_hid_presence_on_a_new_path_retains_the_unverified_terminal_batch(cx: &mut TestAppContext) {
    let _ui_session = crate::ui::test_support::UiTestSession::new(cx);
    let runtime = AppRuntime::new().unwrap();

    let cx = cx.add_empty_window();
    let workbench = offline(cx, runtime.executor());
    workbench.update(cx, |this, _| {
        let original = receiver("synthetic-old-native-path", Some(SERIAL));
        let mut rows: Vec<_> = (0..7)
            .map(|id| {
                (
                    ResultIdentity::Esb(device(&original, id)),
                    UpdateOutcome::Completed,
                )
            })
            .collect();
        rows.push((
            ResultIdentity::Esb(device(&original, RECEIVER_ID)),
            unconfirmed(),
        ));
        let release = hold_operation(this, rows);
        assert!(this.flash.receiver_result_joined_at.is_none());
        assert!(this.flash.results.is_empty());
        join_operation(this, release);
        let joined_at = this.flash.receiver_result_joined_at.unwrap();
        let frozen = this.flash.results[7].0.clone();
        assert_eq!(this.flash.results[7].1, unconfirmed());
        assert_eq!(this.flash.ota_presentation, OtaPresentation::Finished);
        assert!(!this.flash.running_operation(Mode::Esb));
        assert!(!this.flash.shows_activity(Mode::Esb));
        assert!(!this.flash.ota_shows_next());
        assert!(this.flash.shows_results(Mode::Esb));

        assert!(
            this.apply_inventory_result(
                1,
                joined_at + Duration::from_nanos(1),
                Ok(Inventory::default())
            )
            .is_some()
        );
        assert_eq!(this.flash.results[7].1, unconfirmed());
        let moved = receiver("synthetic-new-native-path", Some(" 0123456789abcdef "));
        let mut fresh = inventory(vec![moved.clone()]);
        fresh.serial_enumeration_failed = true;
        fresh
            .warnings
            .push("synthetic serial backend unavailable".into());
        this.scanning_inventory = true;
        assert_eq!(
            this.apply_inventory_result(1, joined_at + Duration::from_nanos(2), Ok(fresh)),
            Some(true)
        );
        assert!(!this.scanning_inventory);
        assert!(this.inventory_error.is_none());
        assert!(this.inventory.serial_enumeration_failed);
        assert_eq!(this.inventory.receivers[0].path, moved.path);
        assert_eq!(this.flash.results[7].0, frozen);
        assert_eq!(this.flash.results[7].1, detected());
        assert!(this.flash.receiver_result_joined_at.is_none());
        assert_eq!(
            OtaCounts::from_progress(&this.flash.last_progress.as_ref().unwrap().ota),
            OtaCounts {
                completed: 7,
                activation_unconfirmed: 1,
                ..OtaCounts::default()
            }
        );
        let progress = &this.flash.last_progress.as_ref().unwrap().ota;
        assert!(
            progress[..7]
                .iter()
                .all(|target| target.phase == OtaPhase::Completed)
        );
        assert_eq!(progress[7].phase, OtaPhase::ActivationUnconfirmed);
        let groups = result_receiver_groups(&this.flash.results);
        assert_eq!(groups.len(), 1);
        let group = &groups[original.key.as_str()];
        assert_eq!(group.len(), 8);
        assert_eq!(
            group
                .iter()
                .filter(|(_, outcome)| matches!(outcome, UpdateOutcome::Completed))
                .count(),
            7
        );
        assert_eq!(group[7].1, &detected());
        assert_eq!(this.flash.ota_presentation, OtaPresentation::Finished);
        assert_idle(this);

        assert!(
            this.apply_inventory_result(
                1,
                joined_at + Duration::from_nanos(3),
                Ok(Inventory::default())
            )
            .is_some()
        );
        assert_eq!(this.flash.results[7].1, detected());
        assert_idle(this);
    });
}

#[gpui_kit::test]
fn only_a_successful_scan_started_after_the_actual_join_can_observe_presence(
    cx: &mut TestAppContext,
) {
    let _ui_session = crate::ui::test_support::UiTestSession::new(cx);
    let runtime = AppRuntime::new().unwrap();

    let cx = cx.add_empty_window();
    let workbench = offline(cx, runtime.executor());
    workbench.update(cx, |this, _| {
        let original = receiver("synthetic-old-native-path", Some(SERIAL));
        let moved = receiver("synthetic-new-native-path", Some(SERIAL));
        let release = hold_operation(
            this,
            vec![(
                ResultIdentity::Esb(device(&original, RECEIVER_ID)),
                unconfirmed(),
            )],
        );
        let prejoin_scan = Instant::now();
        assert_eq!(
            this.apply_inventory_result(1, prejoin_scan, Ok(inventory(vec![moved.clone()]))),
            Some(true)
        );
        let _ = this.poll_flash();
        assert!(this.flash.job.is_some());
        assert!(this.flash.results.is_empty());
        assert!(this.flash.receiver_result_joined_at.is_none());
        join_operation(this, release);
        let joined_at = this.flash.receiver_result_joined_at.unwrap();
        assert!(prejoin_scan <= joined_at);
        let retained = this.flash.results.clone();
        for scan_started_at in [prejoin_scan, joined_at - Duration::from_nanos(1), joined_at] {
            this.scanning_inventory = true;
            assert_eq!(
                this.apply_inventory_result(1, scan_started_at, Ok(inventory(vec![moved.clone()]))),
                Some(false)
            );
            assert!(!this.scanning_inventory);
            assert_eq!(this.flash.results, retained);
            assert_eq!(this.flash.receiver_result_joined_at, Some(joined_at));
        }
        this.scanning_inventory = true;
        assert_eq!(
            this.apply_inventory_result(
                1,
                joined_at + Duration::from_nanos(1),
                Err("synthetic whole inventory scan failed".into()),
            ),
            Some(true)
        );
        assert!(!this.scanning_inventory);
        assert!(this.inventory_error.is_some());
        assert_eq!(this.inventory.receivers[0].path, moved.path);
        assert_eq!(this.flash.results, retained);
        assert_eq!(this.flash.receiver_result_joined_at, Some(joined_at));

        this.scanning_inventory = true;
        assert_eq!(
            this.apply_inventory_result(
                0,
                joined_at + Duration::from_nanos(2),
                Ok(inventory(vec![moved.clone()]))
            ),
            None
        );
        assert!(this.scanning_inventory);
        this.shutting_down = true;
        assert_eq!(
            this.apply_inventory_result(
                1,
                joined_at + Duration::from_nanos(2),
                Ok(inventory(vec![moved.clone()]))
            ),
            None
        );
        assert!(this.scanning_inventory);
        this.shutting_down = false;
        assert_eq!(this.flash.results, retained);
        assert_eq!(
            this.apply_inventory_result(
                1,
                joined_at + Duration::from_nanos(3),
                Ok(inventory(vec![moved]))
            ),
            Some(true)
        );
        assert_eq!(this.flash.results[0].1, detected());
        assert!(this.inventory_error.is_none());
        assert_idle(this);
    });
}

#[gpui_kit::test]
fn frozen_and_current_receiver_identity_must_be_valid_unique_and_unambiguous(
    cx: &mut TestAppContext,
) {
    let _ui_session = crate::ui::test_support::UiTestSession::new(cx);
    let runtime = AppRuntime::new().unwrap();

    let cx = cx.add_empty_window();
    let workbench = offline(cx, runtime.executor());
    workbench.update(cx, |this, _| {
        let moved = receiver("synthetic-new-native-path", Some(SERIAL));
        let different = receiver("synthetic-other-path", Some(OTHER_SERIAL));
        let conflicting_key = ReceiverEndpoint {
            key: moved.key.clone(),
            ..different.clone()
        };
        let conflicting_path = ReceiverEndpoint {
            path: moved.path.clone(),
            ..different.clone()
        };
        let cases = vec![
            ("missing frozen serial", None, vec![moved.clone()]),
            ("short frozen serial", Some("01234567"), vec![moved.clone()]),
            (
                "nonhex frozen serial",
                Some("G123456789ABCDEF"),
                vec![moved.clone()],
            ),
            (
                "missing current serial",
                Some(SERIAL),
                vec![receiver(&moved.key, None)],
            ),
            (
                "short current serial",
                Some(SERIAL),
                vec![receiver(&moved.key, Some("01234567"))],
            ),
            (
                "nonhex current serial",
                Some(SERIAL),
                vec![receiver(&moved.key, Some("G123456789ABCDEF"))],
            ),
            (
                "duplicate normalized serial",
                Some(SERIAL),
                vec![
                    moved.clone(),
                    receiver("synthetic-duplicate", Some(" 0123456789abcdef ")),
                ],
            ),
            (
                "conflicting key occupant",
                Some(SERIAL),
                vec![moved.clone(), conflicting_key],
            ),
            (
                "conflicting path occupant",
                Some(SERIAL),
                vec![moved.clone(), conflicting_path],
            ),
            ("different receiver", Some(SERIAL), vec![different]),
            (
                "same path replacement",
                Some(SERIAL),
                vec![receiver("synthetic-old-native-path", Some(OTHER_SERIAL))],
            ),
        ];
        for (case, frozen_serial, current) in cases {
            let frozen = receiver("synthetic-old-native-path", frozen_serial);
            let release = hold_operation(
                this,
                vec![(
                    ResultIdentity::Esb(device(&frozen, RECEIVER_ID)),
                    unconfirmed(),
                )],
            );
            join_operation(this, release);
            let boundary = this.flash.receiver_result_joined_at;
            assert_eq!(boundary.is_some(), frozen_serial == Some(SERIAL), "{case}");
            let retained = this.flash.results.clone();
            assert!(
                this.apply_inventory_result(
                    1,
                    Instant::now() + Duration::from_nanos(1),
                    Ok(inventory(current))
                )
                .is_some()
            );
            assert_eq!(this.flash.results, retained, "{case}");
            assert_eq!(this.flash.results[0].1, unconfirmed(), "{case}");
            assert_eq!(this.flash.receiver_result_joined_at, boundary, "{case}");
            assert_idle(this);
        }
    });
}

#[gpui_kit::test]
fn presence_does_not_reclassify_failure_completion_tracker_uf2_or_responding_results(
    cx: &mut TestAppContext,
) {
    let _ui_session = crate::ui::test_support::UiTestSession::new(cx);
    let runtime = AppRuntime::new().unwrap();

    let cx = cx.add_empty_window();
    let workbench = offline(cx, runtime.executor());
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("INFO_UF2.TXT"),
        "Board-ID: synthetic\n",
    )
    .unwrap();
    let volume = crate::uf2::inspect_volume(directory.path(), "INFO_UF2.TXT").unwrap();
    workbench.update(cx, |this, _| {
        let endpoint = receiver("synthetic-current-path", Some(SERIAL));
        let unresolved = receiver("synthetic-absent-path", Some(OTHER_SERIAL));
        let responding = UpdateOutcome::ActivationUnconfirmed(ReceiverActivationReport {
            submission: ActivationSubmission::Sent,
            availability: ReceiverAvailability::Responding,
        });
        let uf2 = UpdateOutcome::Uf2CopyUnconfirmed(Uf2CopyReport {
            completed: 4,
            total: 4,
            stage: Uf2FinalizationStage::SyncAll,
            os_error: Some(433),
            diagnostic: "synthetic UF2 disappearance".into(),
        });
        let release = hold_operation(
            this,
            vec![
                (
                    ResultIdentity::Esb(device(&endpoint, RECEIVER_ID)),
                    UpdateOutcome::Failed("synthetic transfer failed".into()),
                ),
                (
                    ResultIdentity::Esb(device(&endpoint, RECEIVER_ID)),
                    UpdateOutcome::Completed,
                ),
                (
                    ResultIdentity::Esb(device(&endpoint, RECEIVER_ID)),
                    responding.clone(),
                ),
                (ResultIdentity::Esb(device(&endpoint, 0)), unconfirmed()),
                (ResultIdentity::Uf2(volume), uf2.clone()),
                (
                    ResultIdentity::Serial("synthetic-console".into()),
                    unconfirmed(),
                ),
                (
                    ResultIdentity::Esb(device(&unresolved, RECEIVER_ID)),
                    unconfirmed(),
                ),
            ],
        );
        join_operation(this, release);
        let joined_at = this.flash.receiver_result_joined_at.unwrap();
        let retained = this.flash.results.clone();
        assert!(
            this.apply_inventory_result(
                1,
                joined_at + Duration::from_nanos(1),
                Ok(inventory(vec![endpoint]))
            )
            .is_some()
        );
        assert_eq!(this.flash.results, retained);
        assert_eq!(
            this.flash.results[0].1,
            UpdateOutcome::Failed("synthetic transfer failed".into())
        );
        assert_eq!(this.flash.results[1].1, UpdateOutcome::Completed);
        assert_eq!(this.flash.results[2].1, responding);
        assert_eq!(this.flash.results[3].1, unconfirmed());
        let generic: Vec<_> = this
            .flash
            .results
            .iter()
            .filter(|(target, _)| target.receiver.is_none())
            .collect();
        assert_eq!(generic.len(), 2);
        assert_eq!(generic[0].1, uf2);
        assert_eq!(generic[1].1, unconfirmed());
        assert_eq!(result_receiver_groups(&this.flash.results).len(), 2);
        assert_eq!(this.flash.receiver_result_joined_at, Some(joined_at));
        assert!(
            this.apply_inventory_result(
                1,
                joined_at + Duration::from_nanos(2),
                Ok(Inventory::default()),
            )
            .is_some()
        );
        assert_eq!(this.flash.results, retained);
        assert_idle(this);
    });
}

#[gpui_kit::test]
fn stale_fw_info_jobs_neither_upgrade_nor_rearm_receiver_result_observation(
    cx: &mut TestAppContext,
) {
    let _ui_session = crate::ui::test_support::UiTestSession::new(cx);
    let runtime = AppRuntime::new().unwrap();

    let cx = cx.add_empty_window();
    let workbench = offline(cx, runtime.executor());
    workbench.update(cx, |this, _| {
        let endpoint = receiver("synthetic-old-native-path", Some(SERIAL));
        let queried = device(&endpoint, RECEIVER_ID);
        let release = hold_operation(
            this,
            vec![(ResultIdentity::Esb(queried.clone()), unconfirmed())],
        );
        join_operation(this, release);
        let joined_at = this.flash.receiver_result_joined_at.unwrap();
        for expected in [unconfirmed(), detected()] {
            this.flash
                .prepare_action_results(&Action::QueryGroups(OtaQueryPlan {
                    groups: vec![(endpoint.clone(), vec![], true)],
                    automatic: false,
                    incremental: false,
                }));
            this.flash.activity_origin = Mode::Esb;
            this.flash.activity_kind = ActivityKind::Query;
            let stale_info = queried.clone();
            this.flash.job = Some(
                Job::start(move |_, _| {
                    Ok(Payload::Update(UpdateCompletion::Queries(QueryResults {
                        targets: vec![stale_info],
                        failures: vec![],
                    })))
                })
                .unwrap(),
            );
            until(|| {
                let _ = this.poll_flash();
                this.flash.job.is_none()
            });
            assert_eq!(this.flash.targets.len(), 1);
            assert_eq!(this.flash.targets[0].info.version, "1");
            assert_eq!(this.flash.targets[0].info.protocol_version, 1);
            assert_eq!(this.flash.targets[0].info.board_target, "synthetic-board");
            assert_eq!(this.flash.results[0].1, expected);
            assert_eq!(this.flash.ota_presentation, OtaPresentation::Finished);
            if matches!(
                expected,
                UpdateOutcome::ActivationUnconfirmed(ReceiverActivationReport {
                    availability: ReceiverAvailability::Unconfirmed(_),
                    ..
                })
            ) {
                assert_eq!(this.flash.receiver_result_joined_at, Some(joined_at));
                assert!(
                    this.apply_inventory_result(
                        1,
                        joined_at + Duration::from_nanos(1),
                        Ok(inventory(vec![endpoint.clone()]))
                    )
                    .is_some()
                );
                assert_eq!(this.flash.results[0].1, detected());
            } else {
                assert!(this.flash.receiver_result_joined_at.is_none());
            }
            assert_idle(this);
        }
    });
}

#[gpui_kit::test]
fn cancellation_keeps_receiver_pins_and_hid_lease_until_the_worker_releases_and_joins(
    cx: &mut TestAppContext,
) {
    let _ui_session = crate::ui::test_support::UiTestSession::new(cx);
    let runtime = AppRuntime::new().unwrap();

    let cx = cx.add_empty_window();
    let workbench = offline(cx, runtime.executor());
    workbench.update(cx, |this, _| {
        let endpoint = receiver("synthetic-old-native-path", Some(SERIAL));
        let alias = SerialEndpoint {
            port_name: "synthetic-renumbered-console".into(),
            label: "Synthetic receiver console".into(),
            manufacturer: None,
            product: None,
            kind: SerialPortKind::Usb,
            serial_number: Some(SERIAL.into()),
            vid: Some(0x1209),
            pid: Some(0x7690),
            usb_interface: None,
        };
        let rows = vec![(
            ResultIdentity::Esb(device(&endpoint, RECEIVER_ID)),
            unconfirmed(),
        )];
        let progress = prepare_operation(this, &rows);
        this.flash.operation_pins = OperationPins::freeze(
            ["112233445566".into()].into(),
            [&endpoint],
            &this.manager,
            &[],
        );
        this.flash
            .leases
            .push(Lease::Hid(this.monitor.reserve(&endpoint).unwrap()));
        let (cancelled, saw_cancel) = mpsc::channel();
        let (release, wait) = mpsc::channel();
        this.flash.job = Some(
            Job::start(move |cancel, emit| {
                emit(progress);
                until(|| cancel.load(Ordering::Acquire));
                cancelled.send(()).unwrap();
                wait.recv_timeout(Duration::from_secs(3)).unwrap();
                Ok(Payload::Update(UpdateCompletion::Results(rows)))
            })
            .unwrap(),
        );
        this.flash.cancel_activity();
        saw_cancel.recv_timeout(Duration::from_secs(3)).unwrap();
        let prejoin_scan = Instant::now();
        assert!(
            this.apply_inventory_result(
                1,
                prejoin_scan,
                Ok(inventory(vec![receiver(
                    "synthetic-new-native-path",
                    Some(SERIAL)
                )]))
            )
            .is_some()
        );
        let _ = this.poll_flash();
        assert!(this.flash.job.is_some());
        assert!(this.flash.results.is_empty());
        assert!(this.flash.receiver_result_joined_at.is_none());
        assert!(this.flash.blocks_address("112233445566"));
        assert!(
            this.flash
                .operation_pins
                .blocks_port(&alias.port_name, Some(&alias))
        );
        assert_eq!(this.flash.leases.len(), 1);
        assert!(this.monitor.is_reserved(&endpoint));
        assert!(this.monitor.reserve(&endpoint).is_err());
        assert!(this.flash_only_check_receiver(&endpoint.key).is_err());
        join_operation(this, release);
        let joined_at = this.flash.receiver_result_joined_at.unwrap();
        assert_eq!(this.flash.results[0].1, unconfirmed());
        assert!(!this.flash.blocks_address("112233445566"));
        assert!(
            !this
                .flash
                .operation_pins
                .blocks_port(&alias.port_name, Some(&alias))
        );
        assert!(!this.monitor.is_reserved(&endpoint));
        assert!(this.flash_only_check_receiver(&endpoint.key).is_ok());
        let token = this.monitor.reserve(&endpoint).unwrap();
        this.monitor.release(&token).unwrap();
        assert!(
            this.apply_inventory_result(
                1,
                prejoin_scan,
                Ok(inventory(vec![receiver(
                    "synthetic-new-native-path",
                    Some(SERIAL)
                )]))
            )
            .is_some()
        );
        assert_eq!(this.flash.results[0].1, unconfirmed());
        assert!(
            this.apply_inventory_result(
                1,
                joined_at + Duration::from_nanos(1),
                Ok(inventory(vec![receiver(
                    "synthetic-new-native-path",
                    Some(SERIAL)
                )]))
            )
            .is_some()
        );
        assert_eq!(this.flash.results[0].1, detected());
        assert_idle(this);
    });
}

#[gpui_kit::test]
fn navigation_preserves_the_join_boundary_and_global_inventory_updates_off_page(
    cx: &mut TestAppContext,
) {
    let _ui_session = crate::ui::test_support::UiTestSession::new(cx);
    let runtime = AppRuntime::new().unwrap();

    let cx = cx.add_empty_window();
    let workbench = offline(cx, runtime.executor());
    workbench.update(cx, |this, _| {
        let endpoint = receiver("synthetic-old-native-path", Some(SERIAL));
        let release = hold_operation(
            this,
            vec![(
                ResultIdentity::Esb(device(&endpoint, RECEIVER_ID)),
                unconfirmed(),
            )],
        );
        join_operation(this, release);
        let joined_at = this.flash.receiver_result_joined_at.unwrap();
        this.flash.dismiss_results(Mode::Uf2);
        assert_eq!(this.flash.receiver_result_joined_at, Some(joined_at));
        for page in [FeaturePage::Home, FeaturePage::Settings] {
            this.set_feature_page(page);
            let _ = this.poll_flash();
            assert_eq!(this.flash.receiver_result_joined_at, Some(joined_at));
            assert_eq!(this.flash.results[0].1, unconfirmed());
        }
        assert!(
            this.apply_inventory_result(
                1,
                joined_at + Duration::from_nanos(1),
                Ok(inventory(vec![receiver(
                    "synthetic-new-native-path",
                    Some(SERIAL)
                )]))
            )
            .is_some()
        );
        assert_eq!(this.flash.results[0].1, detected());
        this.set_feature_page(FeaturePage::DeviceSetup);
        assert!(this.flash.shows_results(Mode::Esb));
        assert_eq!(this.flash.ota_presentation, OtaPresentation::Finished);
        assert_eq!(this.flash.results[0].1, detected());
        assert_idle(this);
    });
}

#[gpui_kit::test]
fn dismiss_and_new_update_disable_observation_without_erasing_retained_detection(
    cx: &mut TestAppContext,
) {
    let _ui_session = crate::ui::test_support::UiTestSession::new(cx);
    let runtime = AppRuntime::new().unwrap();

    let cx = cx.add_empty_window();
    let workbench = offline(cx, runtime.executor());
    workbench.update(cx, |this, _| {
        let endpoint = receiver("synthetic-old-native-path", Some(SERIAL));
        for already_detected in [false, true] {
            for dismiss in [false, true] {
                let release = hold_operation(
                    this,
                    vec![(
                        ResultIdentity::Esb(device(&endpoint, RECEIVER_ID)),
                        unconfirmed(),
                    )],
                );
                join_operation(this, release);
                let joined_at = this.flash.receiver_result_joined_at.unwrap();
                if already_detected {
                    assert!(
                        this.apply_inventory_result(
                            1,
                            joined_at + Duration::from_nanos(1),
                            Ok(inventory(vec![endpoint.clone()]))
                        )
                        .is_some()
                    );
                }
                let expected = if already_detected {
                    detected()
                } else {
                    unconfirmed()
                };
                if dismiss {
                    this.flash.dismiss_results(Mode::Esb);
                    assert!(!this.flash.shows_results(Mode::Esb));
                    assert_eq!(this.flash.ota_presentation, OtaPresentation::Finished);
                } else {
                    this.flash.ota_new_update();
                    assert_eq!(this.flash.ota_presentation, OtaPresentation::Configuring);
                }
                assert!(this.flash.receiver_result_joined_at.is_none());
                this.set_feature_page(FeaturePage::Home);
                for receivers in [vec![], vec![endpoint.clone()]] {
                    assert!(
                        this.apply_inventory_result(
                            1,
                            joined_at + Duration::from_nanos(2),
                            Ok(inventory(receivers))
                        )
                        .is_some()
                    );
                    assert_eq!(this.flash.results[0].1, expected);
                    assert!(this.flash.receiver_result_joined_at.is_none());
                    if dismiss {
                        assert!(!this.flash.shows_results(Mode::Esb));
                    }
                }
                this.set_feature_page(FeaturePage::DeviceSetup);
                assert_eq!(this.flash.results[0].1, expected);
                if dismiss {
                    assert!(!this.flash.shows_results(Mode::Esb));
                }
                assert_idle(this);
            }
        }
    });
}

#[gpui_kit::test]
fn a_replacement_operation_clears_observation_and_rejects_scans_from_the_previous_run(
    cx: &mut TestAppContext,
) {
    let _ui_session = crate::ui::test_support::UiTestSession::new(cx);
    let runtime = AppRuntime::new().unwrap();

    let cx = cx.add_empty_window();
    let workbench = offline(cx, runtime.executor());
    workbench.update(cx, |this, _| {
        let endpoint = receiver("synthetic-old-native-path", Some(SERIAL));
        let release = hold_operation(
            this,
            vec![(
                ResultIdentity::Esb(device(&endpoint, RECEIVER_ID)),
                unconfirmed(),
            )],
        );
        join_operation(this, release);
        let old_join = this.flash.receiver_result_joined_at.unwrap();
        let old_scan = Instant::now();
        let release = hold_operation(
            this,
            vec![(
                ResultIdentity::Esb(device(&endpoint, RECEIVER_ID)),
                unconfirmed(),
            )],
        );
        assert!(this.flash.receiver_result_joined_at.is_none());
        assert!(this.flash.results.is_empty());
        assert!(
            this.apply_inventory_result(1, old_scan, Ok(inventory(vec![endpoint.clone()])))
                .is_some()
        );
        assert!(this.flash.receiver_result_joined_at.is_none());
        join_operation(this, release);
        let new_join = this.flash.receiver_result_joined_at.unwrap();
        assert!(old_join <= old_scan && old_scan <= new_join);
        assert!(
            this.apply_inventory_result(1, old_scan, Ok(inventory(vec![endpoint.clone()])))
                .is_some()
        );
        assert_eq!(this.flash.results[0].1, unconfirmed());
        assert_eq!(this.flash.receiver_result_joined_at, Some(new_join));
        assert!(
            this.apply_inventory_result(
                1,
                new_join + Duration::from_nanos(1),
                Ok(inventory(vec![endpoint]))
            )
            .is_some()
        );
        assert_eq!(this.flash.results[0].1, detected());
        assert_idle(this);
    });
}

#[gpui_kit::test]
fn each_frozen_receiver_resolves_independently_and_detection_remains_historical(
    cx: &mut TestAppContext,
) {
    let _ui_session = crate::ui::test_support::UiTestSession::new(cx);
    let runtime = AppRuntime::new().unwrap();

    let cx = cx.add_empty_window();
    let workbench = offline(cx, runtime.executor());
    workbench.update(cx, |this, _| {
        let first = receiver("synthetic-old-native-path", Some(SERIAL));
        let second = receiver("synthetic-second-path", Some(OTHER_SERIAL));
        let uncertain = UpdateOutcome::ActivationUnconfirmed(ReceiverActivationReport {
            submission: ActivationSubmission::DeliveryUncertain(
                "synthetic short activation write".into(),
            ),
            availability: ReceiverAvailability::Unconfirmed(
                "synthetic second receiver absent during activation".into(),
            ),
        });
        let release = hold_operation(
            this,
            vec![
                (
                    ResultIdentity::Esb(device(&first, RECEIVER_ID)),
                    unconfirmed(),
                ),
                (
                    ResultIdentity::Esb(device(&second, RECEIVER_ID)),
                    uncertain.clone(),
                ),
            ],
        );
        join_operation(this, release);
        let joined_at = this.flash.receiver_result_joined_at.unwrap();
        assert!(
            this.apply_inventory_result(
                1,
                joined_at + Duration::from_nanos(1),
                Ok(inventory(vec![first]))
            )
            .is_some()
        );
        assert_eq!(this.flash.results[0].1, detected());
        assert_eq!(this.flash.results[1].1, uncertain);
        assert_eq!(this.flash.receiver_result_joined_at, Some(joined_at));
        assert!(
            this.apply_inventory_result(
                1,
                joined_at + Duration::from_nanos(2),
                Ok(inventory(vec![second]))
            )
            .is_some()
        );
        assert_eq!(this.flash.results[0].1, detected());
        assert_eq!(
            this.flash.results[1].1,
            UpdateOutcome::ActivationUnconfirmed(ReceiverActivationReport {
                submission: ActivationSubmission::DeliveryUncertain(
                    "synthetic short activation write".into()
                ),
                availability: ReceiverAvailability::Detected {
                    prior_diagnostic: "synthetic second receiver absent during activation".into(),
                },
            })
        );
        assert!(this.flash.receiver_result_joined_at.is_none());
        assert_eq!(result_receiver_groups(&this.flash.results).len(), 2);
        assert_idle(this);
    });
}
