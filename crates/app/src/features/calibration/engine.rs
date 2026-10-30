mod direct;
mod session;
mod wireless;
mod worker;

pub(crate) use session::{CalibrationSession, GuideSelection, ReviewTicket, selections};
pub(crate) use worker::CalibrationWorker;

#[cfg(test)]
pub(crate) mod fixture {
    pub(crate) use super::direct::DirectIo;
    pub(crate) use super::wireless::WirelessIo;
    pub(crate) enum Transport {
        Direct(Box<dyn DirectIo + Send>),
        Wireless(Box<dyn WirelessIo + Send>),
    }
    pub(crate) type Factory =
        std::sync::Arc<dyn Fn(&super::RunRequest) -> Result<Transport, String> + Send + Sync>;
}

use crate::domain::{ReceiverEndpoint, SerialEndpoint};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Axis {
    X,
    Y,
    Z,
}
impl Axis {
    pub(crate) fn index(self) -> u8 {
        match self {
            Self::X => 0,
            Self::Y => 1,
            Self::Z => 2,
        }
    }
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::X => "x",
            Self::Y => "y",
            Self::Z => "z",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Revolutions(u16);
impl Revolutions {
    pub(crate) fn new(value: u16) -> Result<Self, String> {
        if (1..=100).contains(&value) {
            Ok(Self(value))
        } else {
            Err("Revolutions must be between 1 and 100".into())
        }
    }
    pub(crate) fn get(self) -> u16 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CalibrationTask {
    Stationary,
    Sensitivity {
        axis: Axis,
        revolutions: Revolutions,
    },
}

#[derive(Clone, Debug)]
pub(crate) enum Target {
    Direct {
        endpoint: SerialEndpoint,
        generation: Option<u64>,
        address: Option<String>,
    },
    Wireless {
        receiver: ReceiverEndpoint,
        slot: u8,
        address: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RunMode {
    Execute,
    Inspect,
    Recover,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct OperationIdentity {
    pub session: u32,
    pub operation: u16,
}

#[derive(Clone, Debug)]
pub(crate) struct RunRequest {
    pub id: u64,
    pub target: Target,
    pub task: CalibrationTask,
    pub mode: RunMode,
    pub recovery: Option<OperationIdentity>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Dispatch {
    NotAttempted,
    Attempted,
    Written,
    ConsoleAccepted,
    ReceiverConfirmed,
    Rejected,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    Preparing,
    WaitingStill,
    Retrimming,
    Collecting,
    WaitingRotation,
    Recording,
    Validating,
    ReadingBack,
    CandidateQueued,
    Finished,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Application {
    Unconfirmed,
    CandidateQueued,
    Applied,
    Rejected,
    Failed,
    Cancelled,
    Conflicting,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Persistence {
    Unknown,
    StorageError,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ObservationEnd {
    Complete,
    Stopped,
    Deadline,
    Disconnected,
    Unsupported,
    Ambiguous,
    PreparationFailed,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ConsoleValues {
    pub native_open: u64,
    pub bias: Option<[f64; 3]>,
    pub scales: Option<[f64; 3]>,
    pub query_start_ms: u64,
    pub query_end_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Comparison {
    pub non_target_scales_changed: [bool; 3],
    pub applied_bias_changed: bool,
    pub complete: bool,
}

pub(crate) fn compare(
    task: CalibrationTask,
    before: &ConsoleValues,
    after: &ConsoleValues,
) -> Comparison {
    let mut result = Comparison {
        non_target_scales_changed: [false; 3],
        applied_bias_changed: false,
        complete: false,
    };
    if let CalibrationTask::Sensitivity { axis, .. } = task {
        if let (Some(a), Some(b)) = (before.scales, after.scales) {
            for index in 0..3 {
                result.non_target_scales_changed[index] = index != usize::from(axis.index())
                    && display_value(a[index]) != display_value(b[index]);
            }
        }
        if let (Some(a), Some(b)) = (before.bias, after.bias) {
            result.applied_bias_changed = a
                .into_iter()
                .zip(b)
                .any(|(a, b)| display_value(a) != display_value(b));
        }
        result.complete = before.scales.is_some()
            && after.scales.is_some()
            && before.bias.is_some()
            && after.bias.is_some();
    }
    result
}
fn display_value(value: f64) -> i64 {
    (value * 100_000.).round() as i64
}

#[derive(Clone, Debug)]
pub(crate) struct Snapshot {
    // Worker publication tracks authoritative observations, not polling or elapsed time.
    observation_revision: u64,
    pub id: u64,
    pub native_open: u64,
    pub target: Target,
    pub task: CalibrationTask,
    pub mode: RunMode,
    pub dispatch: Dispatch,
    pub phase: Phase,
    pub application: Application,
    pub persistence: Persistence,
    pub baseline: Option<ConsoleValues>,
    pub readback: Option<ConsoleValues>,
    pub readback_incomplete: bool,
    pub identity: Option<String>,
    pub stationary_supported: bool,
    pub sensitivity_supported: bool,
    pub association: Option<OperationIdentity>,
    pub uncertainty: Option<String>,
    pub end: Option<ObservationEnd>,
    pub elapsed_ms: u64,
    pub joined: bool,
    pub acknowledged: bool,
    pub cleanup_error: Option<String>,
}
impl Snapshot {
    fn observe_change(&mut self) {
        self.observation_revision = self.observation_revision.wrapping_add(1);
    }

    pub(crate) fn needs_acknowledgement(&self) -> bool {
        self.dispatch != Dispatch::NotAttempted
            && !matches!(
                self.application,
                Application::Applied | Application::Rejected | Application::Failed
            )
    }
}

pub(crate) enum Fact {
    NativeOpen(u64),
    DispatchAttempt,
    Written,
    ConsoleAccepted,
    ReceiverConfirmed,
    DispatchRejected,
    Phase(Phase),
    Application(Application),
    StorageError,
    Baseline(ConsoleValues),
    Readback(ConsoleValues),
    ReadbackIncomplete,
    Identity {
        address: String,
        stationary: bool,
        sensitivity: bool,
    },
    Association(OperationIdentity),
    Uncertain(String),
    CleanupError(String),
}

pub(crate) struct Engine {
    pub snapshot: Snapshot,
    absolute_deadline: u64,
    candidate_deadline: Option<u64>,
    tail_deadline: Option<u64>,
}
impl Engine {
    pub(crate) fn new(request: RunRequest) -> Self {
        Self {
            absolute_deadline: if request.mode == RunMode::Execute {
                180_000
            } else {
                30_000
            },
            candidate_deadline: None,
            tail_deadline: None,
            snapshot: Snapshot {
                observation_revision: 0,
                id: request.id,
                native_open: 0,
                target: request.target,
                task: request.task,
                mode: request.mode,
                dispatch: Dispatch::NotAttempted,
                phase: Phase::Preparing,
                application: Application::Unconfirmed,
                persistence: Persistence::Unknown,
                baseline: None,
                readback: None,
                readback_incomplete: false,
                identity: None,
                stationary_supported: false,
                sensitivity_supported: false,
                association: request.recovery,
                uncertainty: None,
                end: None,
                elapsed_ms: 0,
                joined: false,
                acknowledged: false,
                cleanup_error: None,
            },
        }
    }
    pub(crate) fn recover(request: RunRequest, previous: Snapshot) -> Self {
        let mut engine = Self::new(request);
        engine.snapshot.dispatch = previous.dispatch;
        engine.snapshot.application = previous.application;
        engine.snapshot.persistence = previous.persistence;
        engine.snapshot.baseline = previous.baseline;
        engine.snapshot.readback = previous.readback;
        engine.snapshot.identity = previous.identity;
        engine.snapshot.uncertainty = previous.uncertainty;
        engine.snapshot.observe_change();
        engine
    }
    pub(crate) fn fact(&mut self, fact: Fact, now_ms: u64) {
        self.snapshot.elapsed_ms = now_ms;
        let changed = match fact {
            Fact::NativeOpen(incarnation) => {
                let changed = self.snapshot.native_open != incarnation;
                self.snapshot.native_open = incarnation;
                changed
            }
            Fact::DispatchAttempt => {
                if self.snapshot.dispatch == Dispatch::NotAttempted {
                    self.absolute_deadline = now_ms.saturating_add(180_000);
                    self.snapshot.dispatch = Dispatch::Attempted;
                    true
                } else {
                    false
                }
            }
            Fact::Written => {
                if !matches!(
                    self.snapshot.dispatch,
                    Dispatch::ReceiverConfirmed | Dispatch::ConsoleAccepted | Dispatch::Written
                ) {
                    self.snapshot.dispatch = Dispatch::Written;
                    true
                } else {
                    false
                }
            }
            Fact::ReceiverConfirmed => {
                let changed = self.snapshot.dispatch != Dispatch::ReceiverConfirmed;
                self.snapshot.dispatch = Dispatch::ReceiverConfirmed;
                changed
            }
            Fact::ConsoleAccepted => {
                let changed = self.snapshot.dispatch != Dispatch::ConsoleAccepted;
                self.snapshot.dispatch = Dispatch::ConsoleAccepted;
                changed
            }
            Fact::DispatchRejected => {
                let changed = self.snapshot.dispatch != Dispatch::Rejected;
                self.snapshot.dispatch = Dispatch::Rejected;
                changed
            }
            Fact::Phase(phase) => {
                if (self.snapshot.application == Application::Unconfirmed
                    || phase == Phase::ReadingBack)
                    && self.snapshot.phase != phase
                {
                    self.snapshot.phase = phase;
                    true
                } else {
                    false
                }
            }
            Fact::Application(application) => {
                let old = self.snapshot.application;
                let old_phase = self.snapshot.phase;
                let terminal = |value| {
                    matches!(
                        value,
                        Application::Applied
                            | Application::Rejected
                            | Application::Failed
                            | Application::Cancelled
                    )
                };
                if old != Application::Conflicting {
                    if application == Application::Conflicting
                        || (terminal(old) && terminal(application) && old != application)
                    {
                        self.snapshot.application = Application::Conflicting;
                    } else if !terminal(old) && application != Application::Unconfirmed {
                        self.snapshot.application = application;
                    }
                }
                if application == Application::CandidateQueued && self.candidate_deadline.is_none()
                {
                    self.candidate_deadline = Some(now_ms.saturating_add(20_000));
                }
                if terminal(application) && self.tail_deadline.is_none() {
                    self.tail_deadline = Some(now_ms.saturating_add(20_000));
                }
                if self.snapshot.application == Application::CandidateQueued {
                    self.snapshot.phase = Phase::CandidateQueued;
                } else if self.snapshot.application != Application::Unconfirmed {
                    self.snapshot.phase = Phase::Finished;
                }
                old != self.snapshot.application || old_phase != self.snapshot.phase
            }
            Fact::StorageError => {
                let changed = self.snapshot.persistence != Persistence::StorageError;
                self.snapshot.persistence = Persistence::StorageError;
                changed
            }
            Fact::Baseline(mut values) => {
                values.native_open = self.snapshot.native_open;
                let changed = self.snapshot.baseline.as_ref() != Some(&values);
                self.snapshot.baseline = Some(values);
                changed
            }
            Fact::Readback(mut values) => {
                values.native_open = self.snapshot.native_open;
                let changed = self.snapshot.readback.as_ref() != Some(&values)
                    || self.snapshot.readback_incomplete;
                self.snapshot.readback = Some(values);
                self.snapshot.readback_incomplete = false;
                changed
            }
            Fact::ReadbackIncomplete => {
                let changed = !self.snapshot.readback_incomplete;
                self.snapshot.readback_incomplete = true;
                changed
            }
            Fact::Identity {
                address,
                stationary,
                sensitivity,
            } => {
                let changed = self.snapshot.identity.as_ref() != Some(&address)
                    || self.snapshot.stationary_supported != stationary
                    || self.snapshot.sensitivity_supported != sensitivity;
                self.snapshot.identity = Some(address);
                self.snapshot.stationary_supported = stationary;
                self.snapshot.sensitivity_supported = sensitivity;
                changed
            }
            Fact::Association(identity) => {
                let changed = self.snapshot.association != Some(identity);
                self.snapshot.association = Some(identity);
                changed
            }
            Fact::Uncertain(reason) => {
                if self.snapshot.uncertainty.is_none() {
                    self.snapshot.uncertainty = Some(reason);
                    true
                } else {
                    false
                }
            }
            Fact::CleanupError(error) => {
                let changed = self.snapshot.cleanup_error.as_ref() != Some(&error);
                self.snapshot.cleanup_error = Some(error);
                changed
            }
        };
        if changed {
            self.snapshot.observe_change();
        }
    }
    pub(crate) fn expired(&self, now_ms: u64) -> bool {
        now_ms >= self.absolute_deadline
            || self
                .tail_deadline
                .or(self.candidate_deadline)
                .is_some_and(|deadline| now_ms >= deadline)
    }
    pub(crate) fn deadline_end(&self, now_ms: u64) -> ObservationEnd {
        if self
            .tail_deadline
            .or(self.candidate_deadline)
            .is_some_and(|deadline| now_ms >= deadline)
        {
            ObservationEnd::Complete
        } else {
            ObservationEnd::Deadline
        }
    }
    pub(crate) fn finish(&mut self, end: ObservationEnd, now_ms: u64) {
        self.snapshot.elapsed_ms = now_ms;
        self.snapshot.end = Some(end);
        self.snapshot.observe_change();
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Application, Axis, CalibrationTask, ConsoleValues, Dispatch, Engine, Fact, ObservationEnd,
        Persistence, Revolutions, RunMode, RunRequest, Target, compare,
    };
    use crate::domain::{SerialEndpoint, SerialPortKind};
    fn engine() -> Engine {
        Engine::new(RunRequest {
            id: 1,
            target: Target::Direct {
                endpoint: SerialEndpoint {
                    port_name: "fixture".into(),
                    label: "Fixture tracker".into(),
                    manufacturer: None,
                    product: None,
                    kind: SerialPortKind::Usb,
                    serial_number: Some("synthetic".into()),
                    vid: Some(1),
                    pid: Some(2),
                    usb_interface: None,
                },
                generation: None,
                address: Some("010203040506".into()),
            },
            task: CalibrationTask::Stationary,
            mode: RunMode::Execute,
            recovery: None,
        })
    }
    #[test]
    fn queued_readback_and_acknowledgement_never_prove_application() {
        let mut engine = engine();
        engine.fact(Fact::DispatchAttempt, 0);
        engine.fact(Fact::Written, 1);
        engine.fact(Fact::Application(Application::CandidateQueued), 100);
        engine.fact(
            Fact::Readback(ConsoleValues {
                native_open: 0,
                bias: Some([1., 2., 3.]),
                scales: Some([1.; 3]),
                query_start_ms: 100,
                query_end_ms: 200,
            }),
            200,
        );
        engine.finish(ObservationEnd::Complete, 20_100);
        engine.snapshot.acknowledged = true;
        assert_eq!(engine.snapshot.application, Application::CandidateQueued);
        assert_eq!(engine.snapshot.persistence, Persistence::Unknown);
        assert_eq!(engine.snapshot.dispatch, Dispatch::Written);
    }
    #[test]
    fn terminal_tail_is_fixed_and_storage_failure_sticky() {
        let mut engine = engine();
        engine.fact(Fact::StorageError, 10);
        engine.fact(Fact::Application(Application::Applied), 100);
        engine.fact(Fact::Application(Application::Applied), 19_000);
        assert!(!engine.expired(20_099));
        assert!(engine.expired(20_100));
        assert_eq!(engine.snapshot.application, Application::Applied);
        assert_eq!(engine.snapshot.persistence, Persistence::StorageError);
    }
    #[test]
    fn first_applied_terminal_starts_a_new_fixed_tail_after_candidate_queueing() {
        let mut engine = engine();
        engine.fact(Fact::DispatchAttempt, 0);
        engine.fact(Fact::Written, 0);
        engine.fact(Fact::Application(Application::CandidateQueued), 0);
        assert_eq!(engine.snapshot.application, Application::CandidateQueued);
        assert!(!engine.expired(18_999));
        engine.fact(Fact::Application(Application::Applied), 19_000);
        engine.fact(Fact::StorageError, 19_000);
        assert_eq!(engine.snapshot.application, Application::Applied);
        assert_eq!(engine.snapshot.persistence, Persistence::StorageError);
        assert!(!engine.expired(20_000));
        engine.fact(Fact::Application(Application::Applied), 20_000);
        engine.fact(Fact::Application(Application::Applied), 38_999);
        assert!(!engine.expired(38_999));
        assert!(engine.expired(39_000));
        assert_eq!(engine.deadline_end(39_000), ObservationEnd::Complete);
        assert_eq!(engine.snapshot.application, Application::Applied);
        assert_eq!(engine.snapshot.persistence, Persistence::StorageError);
    }
    #[test]
    fn unchanged_target_is_valid_and_other_observed_changes_are_discrepancies() {
        for axis in [Axis::X, Axis::Y, Axis::Z] {
            let task = CalibrationTask::Sensitivity {
                axis,
                revolutions: Revolutions::new(5).unwrap(),
            };
            let before = ConsoleValues {
                native_open: 0,
                bias: Some([0.; 3]),
                scales: Some([1.; 3]),
                query_start_ms: 0,
                query_end_ms: 1,
            };
            assert_eq!(
                compare(task, &before, &before).non_target_scales_changed,
                [false; 3]
            );
            let mut after = before.clone();
            after.bias = Some([0.00001, 0., 0.]);
            let mut scales = [1.; 3];
            scales[(usize::from(axis.index()) + 1) % 3] = 1.00001;
            after.scales = Some(scales);
            let comparison = compare(task, &before, &after);
            assert!(comparison.complete);
            assert!(comparison.applied_bias_changed);
            assert!(comparison.non_target_scales_changed[(usize::from(axis.index()) + 1) % 3]);
        }
    }
}
