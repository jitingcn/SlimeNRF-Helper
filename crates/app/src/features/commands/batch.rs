//! Frozen, direct serial command batches. A successful receipt confirms a
//! transport write only; it does not acknowledge firmware command execution.

use crate::i18n::tr;
use crate::sessions::{ConnectionState, SessionManager, WriteReceipt};

pub struct Candidate {
    pub label: String,
    pub port: Option<String>,
    pub exclusion: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub port: String,
    pub generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Excluded {
    pub label: String,
    pub reason: String,
}

#[derive(Clone, Debug)]
pub struct BatchSnapshot {
    pub command: String,
    pub targets: Vec<Target>,
    pub excluded: Vec<Excluded>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Queued,
    Written,
    Failed(String),
}

pub struct TargetOutcome {
    pub label: String,
    pub outcome: Outcome,
    receipt: Option<WriteReceipt>,
}

pub struct BatchRun {
    pub command: String,
    pub targets: Vec<TargetOutcome>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct RunId(std::num::NonZeroU64);

impl RunId {
    pub(crate) fn new(value: u64) -> Option<Self> {
        std::num::NonZeroU64::new(value).map(Self)
    }

    pub(crate) fn get(self) -> u64 {
        self.0.get()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CommandStepResult {
    NotSubmitted,
    Queued,
    Written,
    Rejected(String),
    WriteFailed(String),
    Skipped(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CommandTargetResult {
    pub(crate) label: String,
    pub(crate) port: String,
    pub(crate) steps: Vec<CommandStepResult>,
}

#[derive(Clone, Debug)]
pub(crate) struct CommandTargetPlan {
    pub(crate) label: String,
    pub(crate) endpoint: crate::domain::SerialEndpoint,
    pub(crate) receiver: Option<crate::domain::ReceiverEndpoint>,
    pub(crate) mode: crate::features::commands::model::TargetMode,
    pub(crate) generation: u64,
    pub(crate) steps: Vec<crate::features::commands::policy::CustomCommandSnapshot>,
}

#[derive(Debug)]
enum CommandTargetProgress {
    Ready(usize),
    Queued { step: usize, receipt: WriteReceipt },
    Finished,
}

#[derive(Debug)]
struct CommandTargetState {
    plan: CommandTargetPlan,
    progress: CommandTargetProgress,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CommandOrigin {
    SavedCommand,
    Terminal { source_port: String },
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct CommandRunSummary {
    pub(crate) written: usize,
    pub(crate) pending: usize,
    pub(crate) failed: usize,
}

#[derive(Debug)]
pub(crate) struct CommandRun {
    id: RunId,
    origin: CommandOrigin,
    title: String,
    targets: Vec<CommandTargetResult>,
    states: Vec<CommandTargetState>,
}

impl CommandRun {
    pub(crate) fn new(
        id: RunId,
        origin: CommandOrigin,
        title: String,
        targets: Vec<CommandTargetPlan>,
    ) -> Self {
        let results = targets
            .iter()
            .map(|target| CommandTargetResult {
                label: target.label.clone(),
                port: target.endpoint.port_name.clone(),
                steps: vec![CommandStepResult::NotSubmitted; target.steps.len()],
            })
            .collect();
        let states = targets
            .into_iter()
            .map(|plan| {
                let progress = if plan.steps.is_empty() {
                    CommandTargetProgress::Finished
                } else {
                    CommandTargetProgress::Ready(0)
                };
                CommandTargetState { plan, progress }
            })
            .collect();
        Self {
            id,
            origin,
            title,
            targets: results,
            states,
        }
    }

    pub(crate) fn with_excluded(mut self, excluded: Vec<Excluded>) -> Self {
        self.targets
            .extend(excluded.into_iter().map(|entry| CommandTargetResult {
                label: entry.label,
                port: String::new(),
                steps: vec![CommandStepResult::Rejected(entry.reason)],
            }));
        self
    }

    pub(crate) fn id(&self) -> RunId {
        self.id
    }

    pub(crate) fn origin(&self) -> &CommandOrigin {
        &self.origin
    }

    pub(crate) fn summary(&self) -> CommandRunSummary {
        let mut summary = CommandRunSummary::default();
        for target in &self.targets {
            if target.steps.iter().any(|step| {
                matches!(
                    step,
                    CommandStepResult::Rejected(_)
                        | CommandStepResult::WriteFailed(_)
                        | CommandStepResult::Skipped(_)
                )
            }) {
                summary.failed += 1;
            } else if target
                .steps
                .iter()
                .all(|step| matches!(step, CommandStepResult::Written))
            {
                summary.written += 1;
            } else {
                summary.pending += 1;
            }
        }
        summary
    }

    pub(crate) fn title(&self) -> &str {
        &self.title
    }

    pub(crate) fn pending(&self) -> bool {
        self.states
            .iter()
            .any(|state| !matches!(state.progress, CommandTargetProgress::Finished))
    }

    pub(crate) fn targets(&self) -> &[CommandTargetResult] {
        &self.targets
    }

    pub(crate) fn cancel(&mut self) {
        for (state, target) in self.states.iter_mut().zip(&mut self.targets) {
            skip_not_submitted(
                &mut target.steps,
                &tr("serial.session.error.send_cancelled"),
            );
            if matches!(state.progress, CommandTargetProgress::Ready(_)) {
                state.progress = CommandTargetProgress::Finished;
            }
        }
    }

    /// Polls accepted writes before submitting at most one new line per target.
    /// A transport write result does not acknowledge firmware command execution.
    pub(crate) fn advance(
        &mut self,
        mut submit: impl FnMut(
            &CommandTargetPlan,
            &crate::features::commands::policy::CustomCommandSnapshot,
        ) -> Result<WriteReceipt, String>,
    ) -> bool {
        let mut changed = false;
        for (state, target) in self.states.iter_mut().zip(&mut self.targets) {
            if let CommandTargetProgress::Queued { step, receipt } = &mut state.progress {
                let Some(result) = receipt.poll() else {
                    continue;
                };
                let step = *step;
                state.progress = match result {
                    Ok(()) => {
                        target.steps[step] = CommandStepResult::Written;
                        if matches!(
                            target.steps.get(step + 1),
                            Some(CommandStepResult::NotSubmitted)
                        ) {
                            CommandTargetProgress::Ready(step + 1)
                        } else {
                            CommandTargetProgress::Finished
                        }
                    }
                    Err(error) => {
                        skip_not_submitted(&mut target.steps[step + 1..], &error);
                        target.steps[step] = CommandStepResult::WriteFailed(error);
                        CommandTargetProgress::Finished
                    }
                };
                changed = true;
            }
            let step = match &state.progress {
                CommandTargetProgress::Ready(step) => *step,
                _ => continue,
            };
            state.progress = match submit(&state.plan, &state.plan.steps[step]) {
                Ok(receipt) => {
                    target.steps[step] = CommandStepResult::Queued;
                    CommandTargetProgress::Queued { step, receipt }
                }
                Err(error) => {
                    skip_not_submitted(&mut target.steps[step + 1..], &error);
                    target.steps[step] = CommandStepResult::Rejected(error);
                    CommandTargetProgress::Finished
                }
            };
            changed = true;
        }
        changed
    }
}

fn skip_not_submitted(steps: &mut [CommandStepResult], reason: &str) {
    for step in steps {
        if matches!(step, CommandStepResult::NotSubmitted) {
            *step = CommandStepResult::Skipped(reason.to_owned());
        }
    }
}

impl TargetOutcome {
    pub fn submitted(label: String, result: Result<WriteReceipt, String>) -> Self {
        let (outcome, receipt) = submission(result);
        Self {
            label,
            outcome,
            receipt,
        }
    }
}

impl BatchSnapshot {
    pub fn capture(
        command: String,
        candidates: impl IntoIterator<Item = Candidate>,
        manager: &SessionManager,
    ) -> Self {
        Self::capture_with(command, candidates, |port| {
            manager
                .session(port)
                .map(|session| (session.port.as_str(), session.state, session.generation))
        })
    }

    fn capture_with<'a>(
        command: String,
        candidates: impl IntoIterator<Item = Candidate>,
        mut lookup: impl FnMut(&str) -> Option<(&'a str, ConnectionState, u64)>,
    ) -> Self {
        let mut targets: Vec<Target> = Vec::new();
        let mut excluded = Vec::new();
        for candidate in candidates {
            let reason = if let Some(reason) = candidate.exclusion {
                reason
            } else if let Some(port) = candidate.port {
                match lookup(&port) {
                    Some((port, ConnectionState::Connected, generation)) => {
                        // Use the session's canonical name, so aliases resolving
                        // to one direct session cannot cause duplicate writes.
                        if !targets.iter().any(|target| target.port == port) {
                            targets.push(Target {
                                port: port.to_owned(),
                                generation,
                            });
                        }
                        continue;
                    }
                    _ => tr("serial.batch.error.not_connected"),
                }
            } else {
                tr("serial.batch.error.no_direct_port")
            };
            excluded.push(Excluded {
                label: candidate.label,
                reason,
            });
        }
        Self {
            command,
            targets,
            excluded,
        }
    }
}

impl BatchRun {
    /// Returns true only when at least one receipt reaches its final result.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        for target in &mut self.targets {
            changed |= poll_receipt(&mut target.outcome, &mut target.receipt, WriteReceipt::poll);
        }
        changed
    }

    pub fn pending(&self) -> bool {
        self.targets
            .iter()
            .any(|target| matches!(target.outcome, Outcome::Queued))
    }
}

pub(crate) fn send_if_current<R>(
    target: &Target,
    command: &str,
    current: Option<(ConnectionState, u64)>,
    send: impl FnOnce(&str, &str) -> Result<R, String>,
) -> Result<R, String> {
    let Some((state, generation)) = current else {
        return Err(tr("serial.batch.error.disconnected"));
    };
    if generation != target.generation {
        return Err(tr("serial.batch.error.connection_changed"));
    }
    if state != ConnectionState::Connected {
        return Err(tr("serial.batch.error.disconnected"));
    }
    send(&target.port, command)
}

fn submission<R>(result: Result<R, String>) -> (Outcome, Option<R>) {
    match result {
        Ok(receipt) => (Outcome::Queued, Some(receipt)),
        Err(error) => (Outcome::Failed(error), None),
    }
}

fn poll_receipt<R>(
    outcome: &mut Outcome,
    receipt: &mut Option<R>,
    poll: impl FnOnce(&mut R) -> Option<Result<(), String>>,
) -> bool {
    let Some(result) = receipt.as_mut().and_then(poll) else {
        return false;
    };
    *receipt = None;
    *outcome = match result {
        Ok(()) => Outcome::Written,
        Err(error) => Outcome::Failed(error),
    };
    true
}

#[cfg(test)]
mod tests {
    use super::{
        BatchRun, BatchSnapshot, Candidate, Outcome, Target, TargetOutcome, poll_receipt,
        send_if_current, submission,
    };
    use crate::sessions::ConnectionState;

    fn candidate(label: &str, port: Option<&str>) -> Candidate {
        Candidate {
            label: label.to_owned(),
            port: port.map(str::to_owned),
            exclusion: None,
        }
    }

    #[test]
    fn snapshot_freezes_command_and_selection() {
        let mut command = "status".to_owned();
        let mut selection = vec!["COM1"];
        let snapshot = BatchSnapshot::capture_with(
            command.clone(),
            selection.iter().map(|port| candidate(port, Some(port))),
            |_| Some(("COM1", ConnectionState::Connected, 7)),
        );
        command.clear();
        command.push_str("reboot");
        selection.clear();
        selection.push("COM2");

        assert_eq!(snapshot.command, "status");
        assert_eq!(
            snapshot.targets,
            vec![Target {
                port: "COM1".into(),
                generation: 7
            }]
        );
        assert!(snapshot.excluded.is_empty());
        let cloned = snapshot.clone();
        assert_eq!(cloned.command, snapshot.command);
        assert_eq!(cloned.targets, snapshot.targets);
    }

    #[test]
    fn receiver_and_serial_candidates_share_one_canonical_target() {
        let snapshot = BatchSnapshot::capture_with(
            "status".into(),
            [
                candidate("Receiver", Some("COM1")),
                candidate("Serial", Some("com1")),
            ],
            |_| Some(("COM1", ConnectionState::Connected, 3)),
        );
        assert_eq!(
            snapshot.targets,
            vec![Target {
                port: "COM1".into(),
                generation: 3
            }]
        );
        assert!(snapshot.excluded.is_empty());
    }

    #[test]
    fn offline_and_unsupported_candidates_are_excluded() {
        let snapshot = BatchSnapshot::capture_with(
            "status".into(),
            [
                candidate("Disconnected", Some("COM1")),
                candidate("Connecting", Some("COM2")),
                candidate("Closing", Some("COM3")),
                candidate("Faulted", Some("COM4")),
                candidate("Unknown", Some("COM5")),
                candidate("Wireless tracker", None),
            ],
            |port| match port {
                "COM1" => Some(("COM1", ConnectionState::Disconnected, 1)),
                "COM2" => Some(("COM2", ConnectionState::Connecting, 2)),
                "COM3" => Some(("COM3", ConnectionState::Closing, 3)),
                "COM4" => Some(("COM4", ConnectionState::Faulted, 4)),
                _ => None,
            },
        );
        assert!(snapshot.targets.is_empty());
        assert_eq!(snapshot.excluded.len(), 6);
        assert_eq!(snapshot.excluded[0].label, "Disconnected");
    }

    #[test]
    fn explicit_exclusion_overrides_port_and_connection_state() {
        let snapshot = BatchSnapshot::capture_with(
            "status".into(),
            [Some("COM1"), None].map(|port| Candidate {
                exclusion: Some("No tracker command route".into()),
                ..candidate("Tracker", port)
            }),
            |_| panic!("Explicitly excluded candidates must not resolve a session"),
        );
        assert!(snapshot.targets.is_empty());
        assert_eq!(snapshot.excluded.len(), 2);
        assert!(
            snapshot
                .excluded
                .iter()
                .all(|entry| entry.reason == "No tracker command route")
        );
    }

    #[test]
    fn changed_generation_or_disconnected_target_never_sends() {
        let target = Target {
            port: "COM1".into(),
            generation: 7,
        };
        for current in [
            None,
            Some((ConnectionState::Connected, 8)),
            Some((ConnectionState::Disconnected, 7)),
            Some((ConnectionState::Connecting, 7)),
            Some((ConnectionState::Closing, 7)),
            Some((ConnectionState::Faulted, 7)),
        ] {
            let result: Result<(), String> = send_if_current(&target, "status", current, |_, _| {
                panic!("Stale or unavailable targets must not send")
            });
            assert!(result.is_err());
        }
    }

    #[test]
    fn matching_generation_sends_frozen_command_once_to_exact_port() {
        let target = Target {
            port: "COM1".into(),
            generation: 7,
        };
        let mut sends = 0;
        let result = send_if_current(
            &target,
            "status",
            Some((ConnectionState::Connected, 7)),
            |port, command| {
                sends += 1;
                assert_eq!(port, "COM1");
                assert_eq!(command, "status");
                Ok(42)
            },
        );
        assert_eq!(result, Ok(42));
        assert_eq!(sends, 1);
    }

    #[test]
    fn submission_failure_is_final_without_a_receipt() {
        let (mut outcome, mut receipt) = submission::<()>(Err("Queue full".into()));
        assert_eq!(outcome, Outcome::Failed("Queue full".into()));
        assert!(!poll_receipt(&mut outcome, &mut receipt, |_| panic!(
            "No receipt"
        )));
        assert_eq!(outcome, Outcome::Failed("Queue full".into()));
    }

    #[test]
    fn queued_write_waits_for_one_final_transport_result() {
        for result in [Ok(()), Err("Write failed".to_owned())] {
            let (mut outcome, mut receipt) = submission(Ok(()));
            assert_eq!(outcome, Outcome::Queued);
            assert!(!poll_receipt(&mut outcome, &mut receipt, |_| None));
            assert_eq!(outcome, Outcome::Queued);
            assert!(receipt.is_some());
            let expected = match &result {
                Ok(()) => Outcome::Written,
                Err(error) => Outcome::Failed(error.clone()),
            };
            assert!(poll_receipt(&mut outcome, &mut receipt, |_| Some(result)));
            assert_eq!(outcome, expected);
            assert!(receipt.is_none());
            assert!(!poll_receipt(&mut outcome, &mut receipt, |_| panic!(
                "Already settled"
            )));
            assert_eq!(outcome, expected);
        }
    }

    #[test]
    fn pending_depends_only_on_queued_targets() {
        let mut run = BatchRun {
            command: "status".into(),
            targets: Vec::new(),
        };
        assert!(!run.pending());
        assert!(!run.poll());
        run.targets.push(TargetOutcome {
            label: "COM1".into(),
            outcome: Outcome::Written,
            receipt: None,
        });
        assert!(!run.pending());
        run.targets[0].outcome = Outcome::Failed("Write failed".into());
        assert!(!run.pending());
        run.targets[0].outcome = Outcome::Queued;
        assert!(run.pending());
    }
}

#[cfg(test)]
mod command_run_tests {
    use super::{
        CommandOrigin, CommandRun, CommandStepResult, CommandTargetPlan, CommandTargetResult,
        Excluded, RunId,
    };
    use crate::domain::{DeviceRole, SerialEndpoint, SerialPortKind};
    use crate::features::commands::model::TargetMode;
    use crate::features::commands::policy::{CustomCommandSnapshot, RouteEvidence};
    use crate::sessions::WriteReceipt;
    use crate::sessions::test_support::write_receipt;
    use std::collections::VecDeque;

    fn target(port: &str, generation: u64, lines: &[&str]) -> CommandTargetPlan {
        let evidence = RouteEvidence::Direct {
            port,
            generation,
            connected: true,
            role: DeviceRole::Tracker,
        };
        CommandTargetPlan {
            label: format!("Target {port}"),
            endpoint: SerialEndpoint {
                port_name: port.into(),
                label: format!("Endpoint {port}"),
                manufacturer: None,
                product: None,
                kind: SerialPortKind::Usb,
                serial_number: Some(format!("fixture-{port}")),
                vid: Some(0x1209),
                pid: Some(0x7690),
                usb_interface: None,
            },
            receiver: None,
            mode: TargetMode::ConnectedTerminals,
            generation,
            steps: lines
                .iter()
                .map(|line| CustomCommandSnapshot::capture(line, evidence).unwrap())
                .collect(),
        }
    }

    fn submit_next(
        target: &CommandTargetPlan,
        step: &CustomCommandSnapshot,
        submitted: &mut Vec<(String, String)>,
        receipts: &mut VecDeque<WriteReceipt>,
    ) -> Result<WriteReceipt, String> {
        assert_eq!(step.port(), target.endpoint.port_name);
        submitted.push((step.port().to_owned(), step.command().to_owned()));
        Ok(receipts
            .pop_front()
            .expect("one receipt per expected write"))
    }

    #[test]
    fn run_ids_reject_zero_and_preserve_nonzero_values() {
        assert_eq!(RunId::new(0), None);
        assert_eq!(RunId::new(1).unwrap().get(), 1);
        assert_eq!(RunId::new(u64::MAX).unwrap().get(), u64::MAX);
    }

    #[test]
    fn run_exposes_frozen_display_data_before_submission() {
        let id = RunId::new(42).unwrap();
        let run = CommandRun::new(
            id,
            CommandOrigin::SavedCommand,
            "Inspect devices".into(),
            vec![target("COM1", 7, &["info", "battery"])],
        );

        assert_eq!(run.id(), id);
        assert_eq!(run.title(), "Inspect devices");
        assert!(run.pending());
        assert_eq!(
            run.targets(),
            &[CommandTargetResult {
                label: "Target COM1".into(),
                port: "COM1".into(),
                steps: vec![
                    CommandStepResult::NotSubmitted,
                    CommandStepResult::NotSubmitted,
                ],
            }]
        );
    }

    #[test]
    fn independent_targets_wait_for_their_own_one_shot_receipts() {
        let mut run = CommandRun::new(
            RunId::new(1).unwrap(),
            CommandOrigin::SavedCommand,
            "Inspect".into(),
            vec![
                target("COM1", 7, &["info", "battery"]),
                target("COM2", 9, &["status", "version"]),
            ],
        );
        let (first_a, complete_first_a) = write_receipt();
        let (first_b, complete_first_b) = write_receipt();
        let (second_a, complete_second_a) = write_receipt();
        let (second_b, complete_second_b) = write_receipt();
        let mut receipts = VecDeque::from([first_a, first_b, second_a, second_b]);
        let mut submitted = Vec::new();

        assert!(
            run.advance(|target, step| {
                submit_next(target, step, &mut submitted, &mut receipts)
            })
        );
        assert_eq!(
            submitted,
            vec![
                ("COM1".into(), "info".into()),
                ("COM2".into(), "status".into()),
            ]
        );
        for target in run.targets() {
            assert_eq!(
                target.steps,
                vec![CommandStepResult::Queued, CommandStepResult::NotSubmitted]
            );
        }
        assert!(!run.advance(|_, _| panic!("both targets still own pending receipts")));

        complete_first_a.send(Ok(())).unwrap();
        assert!(
            run.advance(|target, step| {
                submit_next(target, step, &mut submitted, &mut receipts)
            })
        );
        assert_eq!(
            run.targets()[0].steps,
            vec![CommandStepResult::Written, CommandStepResult::Queued]
        );
        assert_eq!(
            run.targets()[1].steps,
            vec![CommandStepResult::Queued, CommandStepResult::NotSubmitted]
        );
        assert_eq!(
            submitted,
            vec![
                ("COM1".into(), "info".into()),
                ("COM2".into(), "status".into()),
                ("COM1".into(), "battery".into()),
            ]
        );
        assert!(!run.advance(|_, _| panic!("a settled receipt cannot be reused")));

        complete_first_b.send(Ok(())).unwrap();
        assert!(
            run.advance(|target, step| {
                submit_next(target, step, &mut submitted, &mut receipts)
            })
        );
        assert_eq!(
            submitted,
            vec![
                ("COM1".into(), "info".into()),
                ("COM2".into(), "status".into()),
                ("COM1".into(), "battery".into()),
                ("COM2".into(), "version".into()),
            ]
        );
        complete_second_b.send(Ok(())).unwrap();
        assert!(run.advance(|_, _| panic!("COM2 has no remaining command")));
        assert_eq!(
            run.targets()[1].steps,
            vec![CommandStepResult::Written, CommandStepResult::Written]
        );
        assert!(run.pending());
        assert!(!run.advance(|_, _| panic!("COM1 still owns its second receipt")));

        complete_second_a.send(Ok(())).unwrap();
        assert!(run.advance(|_, _| panic!("both command sequences are complete")));
        assert_eq!(
            run.targets()[0].steps,
            vec![CommandStepResult::Written, CommandStepResult::Written]
        );
        assert!(!run.pending());
        assert!(!run.advance(|_, _| panic!("completed runs must not submit again")));
        assert!(receipts.is_empty());
    }

    #[test]
    fn rejected_submission_skips_only_that_targets_tail_without_retrying() {
        let mut run = CommandRun::new(
            RunId::new(2).unwrap(),
            CommandOrigin::SavedCommand,
            "Inspect".into(),
            vec![
                target("COM1", 7, &["info", "battery", "status"]),
                target("COM2", 9, &["info", "battery", "status"]),
            ],
        );
        let (receipt, complete) = write_receipt();
        let mut receipt = Some(receipt);
        let mut submitted = Vec::new();

        assert!(run.advance(|target, step| {
            submitted.push((step.port().to_owned(), step.command().to_owned()));
            if target.endpoint.port_name == "COM1" {
                Err("Queue full".into())
            } else {
                Ok(receipt.take().unwrap())
            }
        }));
        assert_eq!(
            run.targets()[0].steps,
            vec![
                CommandStepResult::Rejected("Queue full".into()),
                CommandStepResult::Skipped("Queue full".into()),
                CommandStepResult::Skipped("Queue full".into()),
            ]
        );
        assert_eq!(
            run.targets()[1].steps,
            vec![
                CommandStepResult::Queued,
                CommandStepResult::NotSubmitted,
                CommandStepResult::NotSubmitted,
            ]
        );
        assert!(run.pending());
        assert!(!run.advance(|_, _| panic!("rejected targets never retry")));

        complete.send(Ok(())).unwrap();
        assert!(run.advance(|target, step| {
            assert_eq!(target.generation, 9);
            submitted.push((step.port().to_owned(), step.command().to_owned()));
            Err("Connection generation changed".into())
        }));
        assert_eq!(
            run.targets()[1].steps,
            vec![
                CommandStepResult::Written,
                CommandStepResult::Rejected("Connection generation changed".into()),
                CommandStepResult::Skipped("Connection generation changed".into()),
            ]
        );
        assert_eq!(
            submitted,
            vec![
                ("COM1".into(), "info".into()),
                ("COM2".into(), "info".into()),
                ("COM2".into(), "battery".into()),
            ]
        );
        assert!(!run.pending());
        assert!(!run.advance(|_, _| panic!("a late rejection is final")));
    }

    #[test]
    fn failed_write_retains_uncertainty_and_other_targets_continue() {
        let mut run = CommandRun::new(
            RunId::new(3).unwrap(),
            CommandOrigin::SavedCommand,
            "Inspect".into(),
            vec![
                target("COM1", 7, &["info", "battery", "status"]),
                target("COM2", 9, &["info", "battery"]),
            ],
        );
        let (first_a, complete_first_a) = write_receipt();
        let (first_b, complete_first_b) = write_receipt();
        let (second_b, complete_second_b) = write_receipt();
        let mut receipts = VecDeque::from([first_a, first_b, second_b]);
        let mut submitted = Vec::new();
        assert!(
            run.advance(|target, step| {
                submit_next(target, step, &mut submitted, &mut receipts)
            })
        );

        complete_first_a
            .send(Err("Partial write after 3 bytes".into()))
            .unwrap();
        complete_first_b.send(Ok(())).unwrap();
        assert!(
            run.advance(|target, step| {
                submit_next(target, step, &mut submitted, &mut receipts)
            })
        );
        assert_eq!(
            run.targets()[0].steps,
            vec![
                CommandStepResult::WriteFailed("Partial write after 3 bytes".into()),
                CommandStepResult::Skipped("Partial write after 3 bytes".into()),
                CommandStepResult::Skipped("Partial write after 3 bytes".into()),
            ]
        );
        assert_eq!(
            run.targets()[1].steps,
            vec![CommandStepResult::Written, CommandStepResult::Queued]
        );
        assert_eq!(
            submitted,
            vec![
                ("COM1".into(), "info".into()),
                ("COM2".into(), "info".into()),
                ("COM2".into(), "battery".into()),
            ]
        );

        complete_second_b.send(Ok(())).unwrap();
        assert!(run.advance(|_, _| panic!("failed target tails stay skipped")));
        assert_eq!(
            run.targets()[1].steps,
            vec![CommandStepResult::Written, CommandStepResult::Written]
        );
        assert!(!run.pending());
        assert!(!run.advance(|_, _| panic!("transport failures are never retried")));
    }

    #[test]
    fn cancelling_before_submission_skips_every_step() {
        let mut run = CommandRun::new(
            RunId::new(4).unwrap(),
            CommandOrigin::SavedCommand,
            "Inspect".into(),
            vec![target("COM1", 7, &["info", "battery"])],
        );
        run.cancel();
        run.cancel();

        assert!(matches!(
            run.targets()[0].steps.as_slice(),
            [CommandStepResult::Skipped(first), CommandStepResult::Skipped(second)]
                if !first.is_empty() && !second.is_empty()
        ));
        assert!(!run.pending());
        assert!(!run.advance(|_, _| panic!("cancelled commands must never submit")));
    }

    #[test]
    fn cancelling_keeps_accepted_receipts_and_preserves_their_final_results() {
        let mut run = CommandRun::new(
            RunId::new(5).unwrap(),
            CommandOrigin::SavedCommand,
            "Inspect".into(),
            vec![
                target("COM1", 7, &["info", "battery"]),
                target("COM2", 9, &["info", "battery", "status"]),
            ],
        );
        let (first_a, complete_first_a) = write_receipt();
        let (first_b, complete_first_b) = write_receipt();
        let mut receipts = VecDeque::from([first_a, first_b]);
        let mut submitted = Vec::new();
        assert!(
            run.advance(|target, step| {
                submit_next(target, step, &mut submitted, &mut receipts)
            })
        );
        run.cancel();
        run.cancel();

        assert!(matches!(
            run.targets()[0].steps.as_slice(),
            [CommandStepResult::Queued, CommandStepResult::Skipped(reason)]
                if !reason.is_empty()
        ));
        assert!(matches!(
            run.targets()[1].steps.as_slice(),
            [CommandStepResult::Queued, CommandStepResult::Skipped(first), CommandStepResult::Skipped(second)]
                if !first.is_empty() && !second.is_empty()
        ));
        assert!(run.pending());
        assert!(!run.advance(|_, _| panic!("cancellation prevents every later submission")));

        complete_first_a.send(Ok(())).unwrap();
        complete_first_b
            .send(Err("Partial write after cancellation".into()))
            .unwrap();
        assert!(run.advance(|_, _| panic!("cancelled tails must not be submitted")));
        assert!(matches!(
            run.targets()[0].steps.as_slice(),
            [CommandStepResult::Written, CommandStepResult::Skipped(reason)]
                if !reason.is_empty()
        ));
        assert!(matches!(
            run.targets()[1].steps.as_slice(),
            [CommandStepResult::WriteFailed(error), CommandStepResult::Skipped(first), CommandStepResult::Skipped(second)]
                if !error.is_empty() && !first.is_empty() && !second.is_empty()
        ));
        assert!(!run.pending());
        assert_eq!(
            submitted,
            vec![
                ("COM1".into(), "info".into()),
                ("COM2".into(), "info".into()),
            ]
        );
        let results = run.targets().to_vec();
        run.cancel();
        assert!(!run.advance(|_, _| panic!("settled cancellation is final")));
        assert_eq!(run.targets(), results);
    }

    #[test]
    fn empty_runs_and_targets_are_already_settled() {
        for targets in [Vec::new(), vec![target("COM1", 7, &[])]] {
            let mut run = CommandRun::new(
                RunId::new(6).unwrap(),
                CommandOrigin::SavedCommand,
                "Empty".into(),
                targets,
            );
            assert!(!run.pending());
            assert!(!run.advance(|_, _| panic!("empty plans have no commands")));
            run.cancel();
            assert!(!run.pending());
        }
    }

    #[test]
    fn initial_exclusions_remain_display_only_while_executable_targets_progress() {
        let mut run = CommandRun::new(
            RunId::new(7).unwrap(),
            CommandOrigin::SavedCommand,
            "Inspect".into(),
            vec![target("COM1", 7, &["info", "battery"])],
        )
        .with_excluded(vec![Excluded {
            label: "Unavailable receiver".into(),
            reason: "Console association is ambiguous".into(),
        }]);
        let excluded = CommandTargetResult {
            label: "Unavailable receiver".into(),
            port: String::new(),
            steps: vec![CommandStepResult::Rejected(
                "Console association is ambiguous".into(),
            )],
        };
        assert_eq!(run.targets()[1], excluded);
        let (receipt, complete) = write_receipt();
        let mut receipts = VecDeque::from([receipt]);
        let mut submitted = Vec::new();

        assert!(
            run.advance(|target, step| {
                submit_next(target, step, &mut submitted, &mut receipts)
            })
        );
        run.cancel();
        assert_eq!(run.targets()[1], excluded);
        complete.send(Ok(())).unwrap();
        assert!(run.advance(|_, _| panic!("display-only exclusions cannot submit")));
        assert_eq!(run.targets().len(), 2);
        assert_eq!(run.targets()[0].label, "Target COM1");
        assert_eq!(run.targets()[0].port, "COM1");
        assert!(matches!(
            run.targets()[0].steps.as_slice(),
            [CommandStepResult::Written, CommandStepResult::Skipped(reason)]
                if !reason.is_empty()
        ));
        assert_eq!(run.targets()[1], excluded);
        assert_eq!(submitted, vec![("COM1".into(), "info".into())]);
        assert!(!run.pending());
        assert!(!run.advance(|_, _| panic!("exclusions do not create scheduling state")));
    }

    #[test]
    fn exclusion_only_run_is_settled_and_cancellation_preserves_its_reason() {
        let mut run = CommandRun::new(
            RunId::new(8).unwrap(),
            CommandOrigin::SavedCommand,
            "Unavailable".into(),
            Vec::new(),
        )
        .with_excluded(vec![Excluded {
            label: "Raw data interface".into(),
            reason: "Not a command console".into(),
        }]);
        assert!(!run.pending());
        assert!(!run.advance(|_, _| panic!("excluded targets never submit")));
        run.cancel();

        assert_eq!(
            run.targets(),
            &[CommandTargetResult {
                label: "Raw data interface".into(),
                port: String::new(),
                steps: vec![CommandStepResult::Rejected("Not a command console".into())],
            }]
        );
        assert!(!run.pending());
    }
}
