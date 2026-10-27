use super::support::{query_link, target};
use crate::firmware_update::{self, QueryResults, UpdateCompletion, UpdateRequest};
use crate::ui::flash::{
    Action, FlashState, Mode, OtaQueries, OtaQueryPlan, Payload, query_failure,
};
use std::sync::Arc;
use std::time::Duration;

#[test]
fn pending_handoff_floats_until_a_physical_operation_actually_starts() {
    use crate::ui::flash::ActivityKind;
    for action in [
        Action::QueryGroups(OtaQueryPlan {
            groups: vec![],
            automatic: false,
            incremental: false,
        }),
        Action::EsbGroups(vec![]),
    ] {
        let kind = ActivityKind::for_action(&action);
        let mut state = FlashState {
            activity_origin: Mode::Esb,
            activity_kind: kind,
            pending: Some(action),
            ..FlashState::default()
        };
        assert!(state.floating_activity(Mode::Esb));
        assert!(!state.shows_activity(Mode::Esb));
        assert!(!state.floating_activity(Mode::Uf2));
        let action = state.pending.take().unwrap();
        state.prepare_action_results(&action);
        state.job = Some(
            crate::update::Job::start(move |_, _| {
                if kind == ActivityKind::Query {
                    Ok(Payload::Update(UpdateCompletion::Queries(QueryResults {
                        targets: vec![],
                        failures: vec![],
                    })))
                } else {
                    Ok(Payload::Update(UpdateCompletion::Results(vec![])))
                }
            })
            .unwrap(),
        );
        assert_eq!(
            state.floating_activity(Mode::Esb),
            kind == ActivityKind::Query
        );
        assert_eq!(
            state.running_operation(Mode::Esb),
            kind == ActivityKind::Operation
        );
        assert_eq!(
            state.shows_activity(Mode::Esb),
            kind == ActivityKind::Operation
        );
        // Classification must not wait for the first progress publication.
        state.job.as_mut().unwrap().progress = Some(crate::update::Progress::stage("preparing"));
        assert_eq!(
            state.floating_activity(Mode::Esb),
            kind == ActivityKind::Query
        );
        while state.job.as_mut().unwrap().poll().is_none() {
            std::thread::yield_now();
        }
        let progress = state.job.as_mut().unwrap().progress.take();
        state.job = None;
        state.finish_job_progress(progress);
        assert_eq!(
            state.last_progress.is_some(),
            kind == ActivityKind::Operation
        );
        assert!(!state.floating_activity(Mode::Esb));
        assert_eq!(
            state.shows_activity(Mode::Esb),
            kind == ActivityKind::Operation
        );
    }
}

#[test]
fn preparation_lifetime_is_page_scoped_and_preserves_previous_operation() {
    use std::sync::{atomic::Ordering, mpsc};
    for origin in [Mode::Uf2, Mode::OpenDfu, Mode::Esb, Mode::Ble] {
        for fail in [false, true] {
            let (finish, wait) = mpsc::channel();
            let mut state = FlashState {
                mode: origin,
                activity_origin: origin,
                operation_origin: origin,
                last_progress: Some(crate::update::Progress::bytes("finished update", 10, 10)),
                results: vec![(
                    crate::ui::flash::ResultTarget::generic("device".into(), "Device".into()),
                    crate::update::UpdateOutcome::Completed,
                )],
                result_notes: [("device".into(), "retained note".into())].into(),
                ..FlashState::default()
            };
            state.job = Some(
                crate::update::Job::start(move |cancel, _| {
                    wait.recv_timeout(Duration::from_secs(5)).unwrap();
                    assert_eq!(cancel.load(Ordering::Acquire), fail);
                    if fail {
                        Err("download cancelled".into())
                    } else {
                        Ok(Payload::Ble(vec![]))
                    }
                })
                .unwrap(),
            );
            for mode in [Mode::Uf2, Mode::OpenDfu, Mode::Esb, Mode::Ble] {
                assert_eq!(state.floating_activity(mode), mode == origin);
                assert!(!state.running_operation(mode));
            }
            // Retained inline status stays visible even during read-only work.
            assert!(state.shows_activity(origin));
            assert!(state.shows_results(origin));
            state.job.as_mut().unwrap().progress =
                Some(crate::update::Progress::bytes("download", 5, 10));
            assert!(state.floating_activity(origin));
            if fail {
                state.cancel_activity();
                assert!(state.busy());
                assert!(state.floating_activity(origin));
                assert!(state.job.as_mut().unwrap().poll().is_none());
            }
            finish.send(()).unwrap();
            let result = loop {
                if let Some(result) = state.job.as_mut().unwrap().poll() {
                    break result;
                }
                std::thread::yield_now();
            };
            assert_eq!(result.is_err(), fail);
            let progress = state.job.as_mut().unwrap().progress.take();
            state.job = None;
            state.finish_job_progress(progress);
            assert!(!state.floating_activity(origin));
            assert_eq!(
                state.last_progress.as_ref().unwrap().message,
                "finished update"
            );
            assert_eq!(state.results[0].1, crate::update::UpdateOutcome::Completed);
            assert_eq!(state.result_notes["device"], "retained note");
        }
    }
}

#[test]
fn explicit_query_cancel_is_neutral_esb_only_and_preserves_uf2_outcome() {
    let mut state = FlashState::default();
    let receiver = target("a", 1, "board").receiver;
    state.results.push((
        crate::ui::flash::ResultTarget::generic("uf2".into(), "Disk".into()),
        crate::update::UpdateOutcome::Completed,
    ));
    state.set_error(Mode::Uf2, "retained UF2 error".into());
    state.pending = Some(Action::QueryGroups(OtaQueryPlan {
        groups: vec![],
        automatic: true,
        incremental: true,
    }));
    state.activity_origin = Mode::Esb;
    state.cancel_activity();
    state.pending = None;
    state.activity_kind = crate::ui::flash::ActivityKind::Preparation;
    assert!(state.query_cancelled);
    assert!(state.query_failures.is_empty());
    assert!(state.shows_activity(Mode::Esb));
    assert!(state.shows_results(Mode::Uf2));
    assert!(!state.shows_results(Mode::Esb));
    state.apply_queries(OtaQueries {
        targets: vec![],
        failures: vec![query_failure(&receiver, 1, "real timeout".into())],
    });
    assert!(state.shows_results(Mode::Esb));
    assert_eq!(state.results.len(), 1);
    assert_eq!(state.error.as_deref(), Some("retained UF2 error"));
    state.results.clear();
    state.error = None;
    assert!(!state.shows_activity(Mode::Uf2));
    assert!(!state.shows_results(Mode::Uf2));
}

#[test]
fn explicit_update_cancel_and_shutdown_still_cancel_owned_workers() {
    use std::sync::atomic::{AtomicBool, Ordering};
    for shutdown in [false, true] {
        let ended = Arc::new(AtomicBool::new(false));
        let worker_ended = ended.clone();
        let mut state = FlashState {
            activity_origin: Mode::Esb,
            activity_kind: crate::ui::flash::ActivityKind::Operation,
            job: Some(
                crate::update::Job::start(move |cancel, _| {
                    while !cancel.load(Ordering::Acquire) {
                        std::thread::yield_now();
                    }
                    worker_ended.store(true, Ordering::Release);
                    Ok(Payload::Update(UpdateCompletion::Results(vec![])))
                })
                .unwrap(),
            ),
            ..FlashState::default()
        };
        if shutdown {
            let (jobs, default_job) = state.stop_for_shutdown();
            for job in jobs.into_iter().flatten() {
                job.shutdown();
            }
            assert!(default_job.is_none());
        } else {
            state.cancel_activity();
            assert!(state.running_operation(Mode::Esb));
            assert!(!state.floating_activity(Mode::Esb));
            assert!(
                state.job.is_some(),
                "cancel must retain worker until joined"
            );
            state.job.take().unwrap().shutdown();
        }
        assert!(ended.load(Ordering::Acquire));
        assert!(
            !state.query_cancelled,
            "actual update cancellation is not query cancellation"
        );
    }
}

#[test]
fn running_query_cancel_joins_without_device_failures_or_other_page_status() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let receiver = target("a", 1, "board").receiver;
    let mut state = FlashState {
        activity_kind: crate::ui::flash::ActivityKind::Query,
        activity_origin: Mode::Esb,
        ..FlashState::default()
    };
    let ended = Arc::new(AtomicBool::new(false));
    let worker_ended = ended.clone();
    state.job = Some(
        crate::update::Job::start(move |cancel, progress| {
            while !cancel.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
            let result = firmware_update::execute(
                UpdateRequest::QueryGroups(vec![(receiver, vec![query_link("a", 1)], true)]),
                cancel,
                progress,
            )
            .map(Payload::Update);
            worker_ended.store(true, Ordering::Release);
            result
        })
        .unwrap(),
    );
    state.cancel_activity();
    assert!(state.busy());
    assert!(state.floating_activity(Mode::Esb));
    assert!(!state.floating_activity(Mode::Uf2));
    assert!(!state.shows_activity(Mode::Esb));
    let result = loop {
        if let Some(result) = state.job.as_mut().unwrap().poll() {
            break result;
        }
        std::thread::yield_now();
    };
    state.finish_job_progress(None);
    state.job = None;
    assert!(!state.floating_activity(Mode::Esb));
    let Payload::Update(UpdateCompletion::Queries(query)) = result.unwrap() else {
        panic!("query payload")
    };
    state.apply_queries(query.into());
    assert!(ended.load(Ordering::Acquire));
    assert!(state.query_cancelled);
    assert!(state.query_failures.is_empty());
    assert!(state.shows_activity(Mode::Esb));
    assert!(!state.shows_activity(Mode::Uf2));
    assert!(!state.shows_results(Mode::Uf2));
}
