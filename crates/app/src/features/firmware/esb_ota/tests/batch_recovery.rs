use super::{
    Emulator, FlashGroup, RECEIVER_ID, Session, failure, flash_groups_with, flash_with, image,
    target,
};
use crate::update::{OtaPreparation, UpdateOutcome};
use std::{
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

#[test]
fn recovery_separates_nonempty_tracker_batches_across_image_and_board_plans() {
    let cancel = AtomicBool::new(false);
    let first = [target(0), target(1), target(2)];
    let skipped = [target(3)];
    let mut other_board = [target(4)];
    other_board[0].info.board_target = "other/nrf52833".into();
    let receiver = [target(RECEIVER_ID)];
    let small = image(3);
    let large = image(5);
    let mut io = Emulator::new(&[0, 1, 2, 3, 4]);
    io.firmware.insert(4, other_board[0].info.clone());
    io.silent_query = Some(3);
    let mut session = Session::new(io, &cancel);
    let groups = [
        &FlashGroup {
            targets: &receiver,
            image: &small,
            board: &receiver[0].info.board_target,
        },
        &FlashGroup {
            targets: &first,
            image: &small,
            board: &first[0].info.board_target,
        },
        &FlashGroup {
            targets: &skipped,
            image: &large,
            board: &skipped[0].info.board_target,
        },
        &FlashGroup {
            targets: &other_board,
            image: &large,
            board: &other_board[0].info.board_target,
        },
    ];
    let outcomes = flash_groups_with(&mut session, &groups, &mut |_| {}).unwrap();
    assert!(outcomes.iter().all(|result| {
        matches!(result.outcome, UpdateOutcome::Completed) == (result.target.id != 3)
    }));

    let io = &session.io;
    let [
        (0, first),
        (1, peer),
        (2, next),
        (4, other),
        (RECEIVER_ID, local),
    ] = io.begin_writes.as_slice()
    else {
        panic!("Unexpected BEGIN writes: {:?}", io.begin_writes);
    };
    let last_preflight = io.query_writes.iter().find(|(id, _)| *id == 4).unwrap().1;
    let recovery = Duration::from_secs(2);
    assert!(*first - last_preflight < recovery);
    assert_eq!(first, peer);
    assert_eq!(*next - io.completions[1].1, recovery);
    assert_eq!(*other - io.completions[2].1, recovery);
    assert!(*local - io.completions[3].1 < recovery);
    assert!(io.abort_writes.is_empty());
}

#[test]
fn last_tracker_batch_returns_without_waiting_or_delaying_a_new_operation() {
    let cancel = AtomicBool::new(false);
    let mut session = Session::new(Emulator::new(&[0, 1]), &cancel);
    let firmware = image(3);
    let first = flash_with(
        &mut session,
        &[target(0)],
        &firmware,
        "test/nrf52840",
        &mut |_| {},
    )
    .unwrap();
    assert!(matches!(first[0].outcome, UpdateOutcome::Completed));
    let completed = session.io.completions[0].1;
    assert_eq!(session.io.time, completed);

    let next = flash_with(
        &mut session,
        &[target(1)],
        &firmware,
        "test/nrf52840",
        &mut |_| {},
    )
    .unwrap();
    assert!(matches!(next[0].outcome, UpdateOutcome::Completed));
    assert!(session.io.begin_writes[1].1 - completed < Duration::from_secs(2));
    assert_eq!(session.io.time, session.io.completions[1].1);
}

#[test]
fn cancellation_during_recovery_keeps_prior_success_and_starts_no_new_transfer() {
    let cancel = Arc::new(AtomicBool::new(false));
    let mut io = Emulator::new(&[0, 1, 2]);
    io.cancel_during_activation = Some((Duration::from_millis(500), cancel.clone()));
    let mut session = Session::new(io, &cancel);
    let mut events = Vec::new();
    let outcomes = flash_with(
        &mut session,
        &[target(0), target(1), target(2)],
        &image(3),
        "test/nrf52840",
        &mut |event| events.push(event),
    )
    .unwrap();
    assert!(
        outcomes[..2]
            .iter()
            .all(|result| matches!(result.outcome, UpdateOutcome::Completed))
    );
    assert!(failure(&outcomes[2].outcome).contains("cancelled"));
    assert!(events.iter().any(|event| {
        event
            .ota
            .iter()
            .any(|row| row.id == 2 && row.preparation == Some(OtaPreparation::ReceiverRecovery))
    }));
    let io = &session.io;
    let elapsed = io.time - io.completions[1].1;
    assert!(!elapsed.is_zero() && elapsed < Duration::from_secs(2));
    assert_eq!(io.begin_writes.len(), 2);
    assert!(io.abort_writes.is_empty());
}
