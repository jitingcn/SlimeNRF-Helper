use super::super::ReceiverOpenError;
use super::{
    ABORT, ACTIVATE, BEGIN, DATA, Emulator, FlashGroup, RECEIVER_ID, STATUS, Session, Transport,
    VERIFY, failure, flash_groups_with, flash_with, image, info, status, target,
};
use crate::update::{
    OtaFailure, OtaFailureKind, OtaPhase, OtaProgressError, OtaStatusPhase, UpdateOutcome,
};
use std::{sync::atomic::AtomicBool, time::Duration};

struct BeginFixture {
    io: Emulator,
    slot: u8,
    replacement: Option<[u8; 16]>,
    muted_until: Option<Duration>,
    delayed: Option<(Duration, Vec<[u8; 16]>)>,
    lose_abort: bool,
}

impl BeginFixture {
    fn new(ids: &[u8], slot: u8) -> Self {
        Self {
            io: Emulator::new(ids),
            slot,
            replacement: None,
            muted_until: None,
            delayed: None,
            lose_abort: false,
        }
    }
}

impl Transport for BeginFixture {
    fn write(&mut self, report: &[u8; 64]) -> Result<(), String> {
        if report[..2] == [ABORT, self.slot] && self.lose_abort {
            self.io.writes.push(*report);
            self.io.abort_writes.push((self.slot, self.io.time));
            // Unlike an ambiguous return after delivery, this ABORT is lost;
            // the unconfirmed slot remains a member of the shared DATA ring.
            return Err("HID write: ABORT did not reach the receiver".into());
        }
        self.io.write(report)?;
        if report[..2] == [BEGIN, self.slot] {
            if let Some(status) = self.replacement {
                *self.io.incoming.back_mut().unwrap() = status;
            }
            if let Some((delay, records)) = self.delayed.take() {
                let at = self.io.time + delay;
                self.muted_until = Some(at);
                self.io.delayed_records = Some((at, records));
            }
        }
        Ok(())
    }

    fn read(&mut self, frame: &mut [u8; 64], timeout_ms: i32) -> Result<usize, String> {
        let length = self.io.read(frame, timeout_ms)?;
        if self.muted_until.is_some_and(|at| self.io.time < at) {
            for record in frame[..length].as_chunks_mut::<16>().0 {
                if record[..2] == [STATUS, self.slot] && matches!(record[2], 1 | 2) {
                    *record = [0; 16];
                }
            }
        }
        Ok(length)
    }

    fn now(&self) -> Duration {
        self.io.now()
    }

    fn reconnect(
        &mut self,
        cancel: &AtomicBool,
        deadline: Duration,
    ) -> Result<(), ReceiverOpenError> {
        self.io.reconnect(cancel, deadline)
    }
}

pub(super) fn ota_failure(outcome: &UpdateOutcome) -> &OtaFailure {
    match outcome {
        UpdateOutcome::OtaFailed(failure) => failure,
        other => panic!("Expected typed OTA failure, got {other:?}"),
    }
}

fn begin_status(id: u8, code: u8, detail: u8) -> [u8; 16] {
    let mut record = status(id, code, 0, 0);
    record[11] = detail;
    record
}

#[test]
fn missing_begin_blocks_shared_data_in_this_batch_and_later_images_even_if_abort_is_lost() {
    let cancel = AtomicBool::new(false);
    let mut io = BeginFixture::new(&[0, 1, 2], 1);
    io.muted_until = Some(Duration::MAX);
    io.lose_abort = true;
    let mut session = Session::new(io, &cancel);
    let first = image(2);
    let later = image(3);
    let groups = [
        FlashGroup {
            targets: &[target(RECEIVER_ID)],
            image: &later,
            board: "test/nrf52840",
        },
        FlashGroup {
            targets: &[target(0), target(1)],
            image: &first,
            board: "test/nrf52840",
        },
        FlashGroup {
            targets: &[target(2)],
            image: &later,
            board: "test/nrf52840",
        },
    ];
    let outcomes = flash_groups_with(
        &mut session,
        &[&groups[0], &groups[1], &groups[2]],
        &mut |_| {},
    )
    .unwrap();
    assert!(matches!(outcomes[0].outcome, UpdateOutcome::Completed));
    assert!(matches!(
        ota_failure(&outcomes[1].outcome).kind,
        OtaFailureKind::BeginPeerUnconfirmed { slots: 0b10 }
    ));
    let unconfirmed = ota_failure(&outcomes[2].outcome);
    assert_eq!(unconfirmed.kind, OtaFailureKind::BeginUnconfirmed);
    assert!(
        unconfirmed
            .abort_error
            .as_deref()
            .is_some_and(|error| error.contains("did not reach"))
    );
    assert!(matches!(
        ota_failure(&outcomes[3].outcome).kind,
        OtaFailureKind::BeginBatchSkipped { slots: 0b10 }
    ));
    let io = &session.io.io;
    assert!(!io.writes.iter().any(|r| r[0] == BEGIN && r[1] == 2));
    assert!(
        !io.writes
            .iter()
            .any(|r| matches!(r[0], DATA | VERIFY | ACTIVATE) && r[1] != RECEIVER_ID)
    );
    assert_eq!(
        io.abort_writes
            .iter()
            .map(|&(id, _)| id)
            .collect::<Vec<_>>(),
        [0, 1]
    );
    assert_eq!(
        session.next_begin_at,
        io.abort_writes[1].1 + Duration::from_secs(10)
    );
    assert!(
        io.staging[&1].data.is_empty(),
        "the unconfirmed participant never receives firmware DATA"
    );
}

#[test]
fn direct_begin_refusal_allows_the_ready_peer_without_aborting_the_refused_slot() {
    let cancel = AtomicBool::new(false);
    for detail in [1, 0] {
        let mut io = BeginFixture::new(&[0, 1], 1);
        io.io.ready_failure = Some(1);
        io.replacement = Some(begin_status(1, 0x10, detail));
        let mut session = Session::new(io, &cancel);
        let previous_admission = session.next_begin_at;
        let mut live_refusal = None;
        let outcomes = flash_with(
            &mut session,
            &[target(0), target(1)],
            &image(2),
            "test/nrf52840",
            &mut |progress| {
                if progress.completed == 0
                    && let Some(row) = progress
                        .ota
                        .into_iter()
                        .find(|row| row.id == 1 && row.phase == OtaPhase::Failed)
                {
                    live_refusal = row.error;
                }
            },
        )
        .unwrap();
        assert!(matches!(outcomes[0].outcome, UpdateOutcome::Completed));
        let refusal = ota_failure(&outcomes[1].outcome);
        assert_eq!(
            live_refusal,
            Some(OtaProgressError::Failure(refusal.clone()))
        );
        if detail == 1 {
            assert_eq!(refusal.kind, OtaFailureKind::TrackerBeginNeedsDirectLink);
            assert!(session.io.io.abort_writes.is_empty());
            assert_eq!(session.next_begin_at, previous_admission);
        } else {
            assert_eq!(
                refusal.kind,
                OtaFailureKind::OperationRefused {
                    phase: OtaStatusPhase::Begin,
                    detail: 0,
                }
            );
            let &[(id, aborted_at)] = session.io.io.abort_writes.as_slice() else {
                panic!("expected exactly one targeted legacy ABORT");
            };
            assert_eq!(id, 1);
            assert_eq!(session.next_begin_at, aborted_at + Duration::from_secs(10));
        }
    }
}

#[test]
fn begin_busy_stays_pending_until_ready_and_does_not_retry_the_ready_peer() {
    let cancel = AtomicBool::new(false);
    let mut io = BeginFixture::new(&[0, 1], 1);
    io.replacement = Some(begin_status(1, 0, 2));
    io.delayed = Some((Duration::from_millis(3200), vec![status(1, 1, 0, 0)]));
    let mut session = Session::new(io, &cancel);
    let outcomes = flash_with(
        &mut session,
        &[target(0), target(1)],
        &image(2),
        "test/nrf52840",
        &mut |_| {},
    )
    .unwrap();
    assert!(
        outcomes
            .iter()
            .all(|row| matches!(row.outcome, UpdateOutcome::Completed))
    );
    let io = &session.io.io;
    assert_eq!(
        io.begin_writes
            .iter()
            .map(|&(id, _)| id)
            .collect::<Vec<_>>(),
        [0, 1, 1]
    );
    assert!(io.abort_writes.is_empty());
}

#[test]
fn late_terminal_supersedes_ready_while_awaiting_begin_peer() {
    let cancel = AtomicBool::new(false);
    let mut io = BeginFixture::new(&[0, 1], 1);
    io.delayed = Some((
        Duration::from_millis(300),
        vec![status(0, 0x13, 0, 0), status(1, 1, 0, 0)],
    ));
    let mut session = Session::new(io, &cancel);
    let outcomes = flash_with(
        &mut session,
        &[target(0), target(1)],
        &image(2),
        "test/nrf52840",
        &mut |_| {},
    )
    .unwrap();
    assert!(failure(&outcomes[0].outcome).contains("exceeds device capacity"));
    assert!(matches!(outcomes[1].outcome, UpdateOutcome::Completed));
    let io = &session.io.io;
    assert_eq!(io.abort_writes.len(), 1);
    assert_eq!(io.abort_writes[0].0, 0);
    assert!(
        !io.writes
            .iter()
            .any(|r| matches!(r[0], DATA | VERIFY | ACTIVATE) && r[1] == 0)
    );
}

#[test]
fn complete_during_begin_is_not_success_or_permission_to_transfer() {
    let cancel = AtomicBool::new(false);
    let mut io = BeginFixture::new(&[0], 0);
    let image = image(2);
    io.io.ready_failure = Some(0);
    io.replacement = Some(status(0, 6, 2, image.data.len()));
    let mut session = Session::new(io, &cancel);
    let outcomes = flash_with(
        &mut session,
        &[target(0)],
        &image,
        "test/nrf52840",
        &mut |_| {},
    )
    .unwrap();
    assert!(failure(&outcomes[0].outcome).contains("before transfer"));
    let io = &session.io.io;
    assert_eq!(io.begin_writes.len(), 1);
    assert_eq!(io.abort_writes.len(), 1);
    assert!(
        !io.writes
            .iter()
            .any(|r| matches!(r[0], DATA | VERIFY | ACTIVATE))
    );
}

#[test]
fn begin_with_nonzero_counters_cannot_authorize_shared_data() {
    let cancel = AtomicBool::new(false);
    let mut io = BeginFixture::new(&[0, 1, 2], 1);
    io.replacement = Some(status(1, 2, 1, 60));
    let mut session = Session::new(io, &cancel);
    let outcomes = flash_with(
        &mut session,
        &[target(0), target(1), target(2)],
        &image(2),
        &info().board_target,
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(
        ota_failure(&outcomes[0].outcome).kind,
        OtaFailureKind::BeginPeerUnconfirmed { slots: 0b10 }
    );
    let retained = ota_failure(&outcomes[1].outcome);
    assert_eq!(retained.kind, OtaFailureKind::BeginUnconfirmed);
    assert!(
        retained
            .diagnostic
            .as_deref()
            .is_some_and(|error| error.contains("retained an earlier OTA session"))
    );
    assert_eq!(
        ota_failure(&outcomes[2].outcome).kind,
        OtaFailureKind::BeginBatchSkipped { slots: 0b10 }
    );
    let io = &session.io.io;
    assert_eq!(io.begin_writes.len(), 2);
    assert_eq!(
        io.abort_writes
            .iter()
            .map(|&(id, _)| id)
            .collect::<Vec<_>>(),
        [0, 1]
    );
    assert!(
        !io.writes
            .iter()
            .any(|r| matches!(r[0], DATA | VERIFY | ACTIVATE))
    );
}

#[test]
fn direct_link_detail_during_activation_stays_generic_and_keeps_cleanup() {
    let cancel = AtomicBool::new(false);
    let mut io = Emulator::new(&[0]);
    io.activation_status = Some(begin_status(0, 0x10, 1));
    let mut session = Session::new(io, &cancel);
    let outcomes = flash_with(
        &mut session,
        &[target(0)],
        &image(2),
        &info().board_target,
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(
        ota_failure(&outcomes[0].outcome).kind,
        OtaFailureKind::OperationRefused {
            phase: OtaStatusPhase::Activate,
            detail: 1,
        }
    );
    assert_eq!(session.io.abort_writes.len(), 1);
    assert_eq!(session.io.abort_writes[0].0, 0);
}
