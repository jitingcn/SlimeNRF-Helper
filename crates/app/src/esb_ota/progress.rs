//! UI-only telemetry; never controls transport pacing or protocol decisions.
use super::{PAYLOAD, Target, TargetOutcome, TransferState};
use crate::update::{OtaPhase, OtaPreparation, OtaTargetProgress, Progress, UpdateOutcome};
use std::time::Duration;

const PUBLISH_INTERVAL: Duration = Duration::from_millis(100);
const WARMUP: Duration = Duration::from_millis(250);
const STALL: Duration = Duration::from_secs(1);
const WINDOW: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Default)]
struct Sample {
    at: Duration,
    bytes: u64,
}

#[derive(Default)]
struct Rate {
    samples: [Sample; 32],
    start: usize,
    len: usize,
    last_advance: Duration,
    bytes: u64,
    advances: usize,
}

impl Rate {
    fn reset(&mut self, now: Duration, bytes: u64) {
        self.start = 0;
        self.len = 1;
        self.samples[0] = Sample { at: now, bytes };
        self.last_advance = now;
        self.bytes = bytes;
        self.advances = 0;
    }

    fn observe(&mut self, now: Duration, bytes: u64) {
        if self.len == 0 || now.saturating_sub(self.last_advance) >= STALL {
            self.reset(now, bytes);
            return;
        }
        if bytes > self.bytes {
            self.last_advance = now;
            self.bytes = bytes;
            self.advances += 1;
        }
        while self.len > 1 && now.saturating_sub(self.samples[self.start].at) > WINDOW {
            self.start = (self.start + 1) % self.samples.len();
            self.len -= 1;
        }
        let last = (self.start + self.len - 1) % self.samples.len();
        if now.saturating_sub(self.samples[last].at) >= PUBLISH_INTERVAL {
            if self.len == self.samples.len() {
                self.start = (self.start + 1) % self.samples.len();
                self.len -= 1;
            }
            let next = (self.start + self.len) % self.samples.len();
            self.samples[next] = Sample { at: now, bytes };
            self.len += 1;
        }
    }

    fn estimate(&self, now: Duration, completed: u64, total: u64) -> Option<(u64, u64)> {
        if self.len == 0
            || self.advances < 2
            || completed >= total
            || now.saturating_sub(self.last_advance) >= STALL
        {
            return None;
        }
        let first = self.samples[self.start];
        let elapsed = now.saturating_sub(first.at);
        if elapsed < WARMUP {
            return None;
        }
        let rate = (u128::from(completed.saturating_sub(first.bytes)) * 1_000_000_000
            / elapsed.as_nanos()) as u64;
        (rate > 0).then(|| (rate, (total - completed).div_ceil(rate)))
    }
}

struct Entry {
    value: OtaTargetProgress,
    rate: Rate,
}

pub(super) struct Telemetry {
    entries: Vec<Entry>,
    last_publish: Option<Duration>,
    dirty: bool,
}

impl Telemetry {
    pub(super) fn new<'a>(targets: impl IntoIterator<Item = &'a Target>) -> Self {
        Self {
            entries: targets
                .into_iter()
                .map(|target| Entry {
                    value: OtaTargetProgress {
                        receiver_key: target.receiver.key.clone(),
                        id: target.id,
                        phase: OtaPhase::Queued,
                        preparation: None,
                        error: None,
                        completed: 0,
                        total: 0,
                        bytes_per_second: None,
                        eta_seconds: None,
                    },
                    rate: Rate::default(),
                })
                .collect(),
            last_publish: None,
            dirty: true,
        }
    }

    pub(super) fn phase(&mut self, index: usize, phase: OtaPhase, now: Duration) {
        let entry = &mut self.entries[index];
        if entry.value.phase != phase {
            entry.value.phase = phase;
            entry.value.preparation = None;
            entry.value.error = None;
            entry.value.bytes_per_second = None;
            entry.value.eta_seconds = None;
            entry.rate.reset(now, entry.value.completed);
            self.dirty = true;
        }
    }

    pub(super) fn prepare(&mut self, index: usize, total: usize, now: Duration) {
        self.entries[index].value.total = total as u64;
        self.phase(index, OtaPhase::Preparing, now);
    }

    pub(super) fn preparation(&mut self, index: usize, stage: OtaPreparation, now: Duration) {
        self.phase(index, OtaPhase::Preparing, now);
        let value = &mut self.entries[index].value;
        if value.preparation != Some(stage) {
            value.preparation = Some(stage);
            self.dirty = true;
        }
    }

    pub(super) fn clear_preparation(&mut self, index: usize) {
        if self.entries[index].value.preparation.take().is_some() {
            self.dirty = true;
        }
    }

    pub(super) fn transfer(&mut self, states: &[TransferState], now: Duration) {
        for state in states {
            let entry = &mut self.entries[state.index];
            let completed = ((state.next * PAYLOAD) as u64).min(entry.value.total);
            entry.value.completed = entry.value.completed.max(completed);
            entry.rate.observe(now, entry.value.completed);
        }
    }

    pub(super) fn verified_bytes(&mut self, index: usize) {
        let value = &mut self.entries[index].value;
        if value.completed != value.total {
            value.completed = value.total;
            self.dirty = true;
        }
    }

    pub(super) fn fail(&mut self, index: usize, error: &str, now: Duration) {
        if self.entries[index].value.phase == OtaPhase::ActivationUnconfirmed {
            return;
        }
        self.phase(
            index,
            if error.contains("cancelled") {
                OtaPhase::Cancelled
            } else {
                OtaPhase::Failed
            },
            now,
        );
        let value = &mut self.entries[index].value;
        if value.error.as_deref() != Some(error) {
            value.error = Some(error.to_owned());
            self.dirty = true;
        }
    }

    pub(super) fn outcomes(&mut self, results: &[TargetOutcome], now: Duration) {
        for (index, result) in results.iter().enumerate() {
            match &result.outcome {
                UpdateOutcome::Completed => {
                    self.entries[index].value.completed = self.entries[index].value.total;
                    self.phase(index, OtaPhase::Completed, now);
                }
                UpdateOutcome::ActivationUnconfirmed(_) => {
                    self.phase(index, OtaPhase::ActivationUnconfirmed, now);
                }
                // UF2 storage finalization is not an ESB protocol phase.
                UpdateOutcome::Uf2CopyUnconfirmed(_) => {}
                UpdateOutcome::Failed(error) if error != "Not started" => {
                    self.fail(index, error, now)
                }
                UpdateOutcome::Failed(_) => {}
            }
        }
    }

    pub(super) fn fail_pending(&mut self, error: &str, now: Duration) {
        for index in 0..self.entries.len() {
            if !matches!(
                self.entries[index].value.phase,
                OtaPhase::Completed
                    | OtaPhase::ActivationUnconfirmed
                    | OtaPhase::Failed
                    | OtaPhase::Cancelled
            ) {
                self.fail(index, error, now);
            }
        }
    }

    pub(super) fn publish(&mut self, now: Duration, progress: &mut dyn FnMut(Progress)) {
        // Do this before cloning identities, allocating the snapshot or formatting text.
        if !self.dirty
            && self
                .last_publish
                .is_some_and(|last| now.saturating_sub(last) < PUBLISH_INTERVAL)
        {
            return;
        }
        self.last_publish = Some(now);
        self.dirty = false;
        let ota: Vec<_> = self
            .entries
            .iter_mut()
            .map(|entry| {
                let estimate = if entry.value.phase == OtaPhase::Transferring {
                    entry
                        .rate
                        .estimate(now, entry.value.completed, entry.value.total)
                } else {
                    None
                };
                entry.value.bytes_per_second = estimate.map(|value| value.0);
                entry.value.eta_seconds = estimate.map(|value| value.1);
                entry.value.clone()
            })
            .collect();
        let completed = ota
            .iter()
            .filter(|target| target.phase == OtaPhase::Completed)
            .count() as u64;
        progress(Progress {
            message: "Updating ESB devices".into(),
            completed,
            total: ota.len() as u64,
            ota,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::{Rate, Telemetry};
    use crate::update::{
        ActivationSubmission, OtaPhase, OtaPreparation, ReceiverActivationReport,
        ReceiverAvailability, UpdateOutcome,
    };
    use std::time::Duration;

    #[test]
    fn rate_warms_up_bounds_history_ignores_duplicates_and_rewarms_after_stall() {
        let mut rate = Rate::default();
        rate.reset(Duration::ZERO, 0);
        for tick in 1..=2 {
            let now = Duration::from_millis(tick * 100);
            rate.observe(now, tick * 100);
            assert_eq!(rate.estimate(now, tick * 100, 10_000), None);
        }
        rate.observe(Duration::from_millis(300), 300);
        assert_eq!(
            rate.estimate(Duration::from_millis(300), 300, 10_300),
            Some((1000, 10))
        );
        rate.observe(Duration::from_millis(400), 300);
        assert_eq!(
            rate.estimate(Duration::from_millis(400), 300, 10_300),
            Some((750, 14))
        );
        rate.observe(Duration::from_millis(1300), 300);
        assert_eq!(
            rate.estimate(Duration::from_millis(1300), 300, 10_300),
            None
        );
        rate.observe(Duration::from_millis(1400), 400);
        rate.observe(Duration::from_millis(1500), 500);
        assert_eq!(
            rate.estimate(Duration::from_millis(1500), 500, 10_300),
            None
        );
        rate.observe(Duration::from_millis(1600), 600);
        assert_eq!(
            rate.estimate(Duration::from_millis(1600), 600, 10_600),
            Some((1000, 10))
        );
        for tick in 17..=100 {
            let now = Duration::from_millis(tick * 100);
            let bytes = tick * 100 - 1000;
            rate.observe(now, bytes);
            assert_eq!(rate.estimate(now, bytes, bytes + 1000), Some((1000, 1)));
        }
        assert!(rate.len <= 21);
        assert_eq!(rate.estimate(Duration::from_secs(10), 9000, 9000), None);
    }

    #[test]
    fn snapshots_are_throttled_but_phase_changes_are_never_delayed() {
        let target = super::super::tests::target(0);
        let mut telemetry = Telemetry::new(&[target]);
        let mut events = Vec::new();
        let mut emit = |value| events.push(value);
        telemetry.publish(Duration::ZERO, &mut emit);
        telemetry.prepare(0, 6000, Duration::ZERO);
        telemetry.publish(Duration::ZERO, &mut emit);
        telemetry.phase(0, OtaPhase::Transferring, Duration::ZERO);
        telemetry.publish(Duration::ZERO, &mut emit);
        let mut state = super::super::TransferState {
            index: 0,
            id: 0,
            next: 0,
            ring: 0,
            verified: false,
        };
        for tick in 1..=20 {
            state.next = tick;
            let now = Duration::from_millis(tick as u64 * 5);
            telemetry.transfer(std::slice::from_ref(&state), now);
            telemetry.publish(now, &mut emit);
        }
        telemetry.fail(0, "CRC mismatch", Duration::from_millis(100));
        telemetry.publish(Duration::from_millis(100), &mut emit);
        assert_eq!(events.len(), 5);
        assert_eq!(events[3].ota[0].completed, 1200);
        assert_eq!(events[4].ota[0].phase, OtaPhase::Failed);
        assert_eq!(events[4].ota[0].eta_seconds, None);
        assert_eq!(events[4].ota[0].error.as_deref(), Some("CRC mismatch"));
    }

    #[test]
    fn preparation_changes_publish_immediately_without_invented_progress() {
        let mut telemetry = Telemetry::new(&[super::super::tests::target(0)]);
        let mut events = Vec::new();
        telemetry.prepare(0, 6000, Duration::ZERO);
        telemetry.publish(Duration::ZERO, &mut |value| events.push(value));
        for stage in [
            OtaPreparation::ReceiverRecovery,
            OtaPreparation::QueryingDevice,
            OtaPreparation::StartingUpdate,
        ] {
            telemetry.preparation(0, stage, Duration::ZERO);
            telemetry.publish(Duration::ZERO, &mut |value| events.push(value));
            let row = &events.last().unwrap().ota[0];
            assert_eq!(row.phase, OtaPhase::Preparing);
            assert_eq!(row.preparation, Some(stage));
            assert_eq!(row.completed, 0);
            assert_eq!(row.bytes_per_second, None);
            assert_eq!(row.eta_seconds, None);
            telemetry.preparation(0, stage, Duration::ZERO);
            telemetry.publish(Duration::ZERO, &mut |value| events.push(value));
        }
        assert_eq!(events.len(), 4);
        telemetry.clear_preparation(0);
        telemetry.publish(Duration::ZERO, &mut |value| events.push(value));
        assert_eq!(events.len(), 5);
        assert_eq!(events[4].ota[0].preparation, None);
        telemetry.preparation(0, OtaPreparation::StartingUpdate, Duration::ZERO);
        telemetry.phase(0, OtaPhase::Transferring, Duration::ZERO);
        telemetry.publish(Duration::ZERO, &mut |value| events.push(value));
        assert_eq!(events[5].ota[0].preparation, None);
    }

    #[test]
    fn cumulative_errors_survive_peer_progress_and_cleanup_replacements_publish() {
        let targets = [
            super::super::tests::target(0),
            super::super::tests::target(1),
        ];
        let mut telemetry = Telemetry::new(&targets);
        let mut events = Vec::new();
        telemetry.preparation(0, OtaPreparation::QueryingDevice, Duration::ZERO);
        telemetry.fail(0, "Target 0: CRC mismatch", Duration::ZERO);
        telemetry.publish(Duration::ZERO, &mut |value| events.push(value));
        telemetry.preparation(1, OtaPreparation::StartingUpdate, Duration::ZERO);
        telemetry.publish(Duration::ZERO, &mut |value| events.push(value));
        assert_eq!(
            events[1].ota[0].error.as_deref(),
            Some("Target 0: CRC mismatch")
        );
        assert_eq!(events[1].ota[0].preparation, None);
        let cleanup = "Target 0: CRC mismatch; OTA abort failed: disconnected";
        let mut outcomes = vec![
            super::super::TargetOutcome {
                target: targets[0].clone(),
                outcome: UpdateOutcome::Failed(cleanup.into()),
            },
            super::super::TargetOutcome {
                target: targets[1].clone(),
                outcome: UpdateOutcome::Failed("Not started".into()),
            },
        ];
        telemetry.outcomes(&outcomes, Duration::ZERO);
        telemetry.publish(Duration::ZERO, &mut |value| events.push(value));
        assert_eq!(events.len(), 3);
        assert_eq!(events[2].ota[0].error.as_deref(), Some(cleanup));
        telemetry.outcomes(&outcomes, Duration::ZERO);
        telemetry.publish(Duration::ZERO, &mut |value| events.push(value));
        assert_eq!(events.len(), 3);
        telemetry.fail_pending("cancelled", Duration::ZERO);
        telemetry.publish(Duration::ZERO, &mut |value| events.push(value));
        assert_eq!(events[3].ota[0].error.as_deref(), Some(cleanup));
        assert_eq!(events[3].ota[1].phase, OtaPhase::Cancelled);
        assert_eq!(events[3].ota[1].error.as_deref(), Some("cancelled"));
        assert_eq!(events[3].ota[1].preparation, None);
        outcomes[0].outcome = UpdateOutcome::Completed;
        telemetry.outcomes(&outcomes, Duration::ZERO);
        telemetry.prepare(1, 6000, Duration::ZERO);
        telemetry.publish(Duration::ZERO, &mut |value| events.push(value));
        assert_eq!(events[4].ota[0].phase, OtaPhase::Completed);
        assert_eq!(events[4].ota[0].error, None);
        assert_eq!(events[4].ota[1].phase, OtaPhase::Preparing);
        assert_eq!(events[4].ota[1].error, None);
    }

    #[test]
    fn submitted_receiver_activation_is_terminal_through_pending_failure_and_cancellation() {
        let targets = [
            super::super::tests::target(super::super::RECEIVER_ID),
            super::super::tests::target(1),
        ];
        let mut telemetry = Telemetry::new(&targets);
        telemetry.prepare(0, 6000, Duration::ZERO);
        telemetry.phase(0, OtaPhase::Activating, Duration::ZERO);
        let outcomes = [
            super::super::TargetOutcome {
                target: targets[0].clone(),
                outcome: UpdateOutcome::ActivationUnconfirmed(ReceiverActivationReport {
                    submission: ActivationSubmission::Accepted,
                    availability: ReceiverAvailability::Responding,
                }),
            },
            super::super::TargetOutcome {
                target: targets[1].clone(),
                outcome: UpdateOutcome::Failed("Not started".into()),
            },
        ];
        telemetry.outcomes(&outcomes, Duration::ZERO);
        telemetry.fail_pending("cancelled", Duration::ZERO);
        telemetry.fail(0, "cancelled", Duration::ZERO);
        let mut events = Vec::new();
        telemetry.publish(Duration::ZERO, &mut |value| events.push(value));
        let snapshot = &events[0];
        assert_eq!(snapshot.completed, 0);
        assert_eq!(snapshot.total, 2);
        assert_eq!(snapshot.ota[0].phase, OtaPhase::ActivationUnconfirmed);
        assert_eq!(snapshot.ota[0].error, None);
        assert_eq!(snapshot.ota[0].preparation, None);
        assert_eq!(snapshot.ota[0].bytes_per_second, None);
        assert_eq!(snapshot.ota[0].eta_seconds, None);
        assert_eq!(snapshot.ota[1].phase, OtaPhase::Cancelled);
        assert_eq!(snapshot.ota[1].error.as_deref(), Some("cancelled"));
    }

    #[test]
    fn uf2_copy_uncertainty_does_not_fabricate_an_esb_terminal_phase() {
        use crate::update::{Uf2CopyReport, Uf2FinalizationStage};
        let targets = [super::super::tests::target(1)];
        let mut telemetry = Telemetry::new(&targets);
        telemetry.prepare(0, 512, Duration::ZERO);
        telemetry.phase(0, OtaPhase::Transferring, Duration::ZERO);
        telemetry.outcomes(
            &[super::super::TargetOutcome {
                target: targets[0].clone(),
                outcome: UpdateOutcome::Uf2CopyUnconfirmed(Uf2CopyReport {
                    completed: 512,
                    total: 512,
                    stage: Uf2FinalizationStage::Flush,
                    os_error: Some(433),
                    diagnostic: "device disappeared".into(),
                }),
            }],
            Duration::ZERO,
        );
        let mut events = Vec::new();
        telemetry.publish(Duration::ZERO, &mut |value| events.push(value));
        assert_eq!(events[0].ota[0].phase, OtaPhase::Transferring);
        assert_eq!(events[0].completed, 0);
        assert_eq!(events[0].ota[0].error, None);
    }
}
