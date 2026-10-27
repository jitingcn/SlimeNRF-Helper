//! Exclusive START/STOP lifecycle and recording finalization.
use super::io::CaptureIo;
use super::{
    CaptureMode, CaptureOutcome, CapturePhase, CaptureRequest, CaptureSnapshot, LiveTrack,
};
use crate::domain::normalize_address;
use parking_lot::Mutex;
use slimenrf_recording::capture::format::Recorder;
use slimenrf_recording::capture::protocol::CdcParser;
use slimenrf_recording::capture::{CalibrationSnapshot, SensorMetadata, TrackIdentity};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

mod control;
mod stream;
#[cfg(test)]
pub(super) mod tests;

const ACK_TIMEOUT: Duration = Duration::from_secs(2);
const IDENTITY_TIMEOUT: Duration = Duration::from_secs(3);
const WARMUP: Duration = Duration::from_secs(5);
const DRAIN: Duration = Duration::from_millis(200);
const PREVIEW: usize = 256;
pub(super) fn expected_roster(request: &CaptureRequest) -> Result<BTreeMap<u8, String>, String> {
    let mut roster = BTreeMap::new();
    for (slot, address) in &request.roster {
        let address = normalize_address(address)
            .filter(|a| a != "000000000000")
            .ok_or("Capture requires a positive RF address for each frozen slot")?;
        if *slot >= 16 || roster.insert(*slot, address).is_some() {
            return Err("Invalid or duplicate frozen RF slot".into());
        }
    }
    match &request.mode {
        CaptureMode::Single { slot, rf_address } => {
            let address = normalize_address(rf_address).ok_or("Invalid selected RF address")?;
            if roster.get(slot) != Some(&address) {
                return Err("Selected RF identity is not in the frozen roster".into());
            }
        }
        CaptureMode::AllPaired { .. } if roster.is_empty() => {
            return Err("No paired RF identities are known".into());
        }
        _ => {}
    }
    Ok(roster)
}
struct Session<'a> {
    request: &'a CaptureRequest,
    expected: BTreeMap<u8, String>,
    seen: BTreeSet<u8>,
    recorder: Option<Recorder>,
    snapshot: CaptureSnapshot,
    parser: CdcParser,
    seq: u8,
    start: Option<Duration>,
    last_seq: [Option<u16>; 16],
    repairs: BTreeSet<(u8, u8, u8)>,
    repair_after: Duration,
    data_failed: bool,
    sink_failed: bool,
    registration_first: Option<u8>,
    registration_cycle: bool,
    last_publish: Duration,
    start_attempted: bool,
    start_rejected: bool,
    cleanup_error: Option<String>,
    metadata_disabled: bool,
    identity_failed: bool,
    recorded_samples: u64,
    snapshot_dirty: bool,
}
pub(super) fn collect(
    request: &CaptureRequest,
    expected: BTreeMap<u8, String>,
    io: &mut impl CaptureIo,
    cancel: &AtomicBool,
    shared: &Mutex<Arc<CaptureSnapshot>>,
) -> Result<CaptureOutcome, String> {
    let mut recorder = Recorder::create(&request.destination, request.source.receiver.key.clone())?;
    let mut tracks = Vec::new();
    for (slot, address) in &expected {
        if matches!(&request.mode, CaptureMode::Single { slot: selected, .. } if selected != slot) {
            continue;
        }
        let identity = TrackIdentity {
            receiver: request.source.receiver.key.clone(),
            slot: *slot,
            rf_address: Some(address.clone()),
        };
        if let Err(error) = recorder.register_track(identity.clone()) {
            let finish = recorder.finish();
            return Err(match finish {
                Ok(_) => error,
                Err(finish) => format!("{error}; finalization: {finish}"),
            });
        }
        tracks.push(LiveTrack {
            identity,
            metadata: SensorMetadata::default(),
            calibration: CalibrationSnapshot::default(),
            samples: 0,
            latest: VecDeque::with_capacity(PREVIEW),
        });
    }
    let mut session = Session {
        request,
        expected,
        seen: BTreeSet::new(),
        recorder: Some(recorder),
        snapshot: CaptureSnapshot {
            phase: CapturePhase::Starting,
            tracks,
            ..CaptureSnapshot::default()
        },
        parser: CdcParser::default(),
        seq: 0,
        start: None,
        last_seq: [None; 16],
        repairs: BTreeSet::new(),
        repair_after: Duration::MAX,
        data_failed: false,
        sink_failed: false,
        registration_first: None,
        registration_cycle: false,
        last_publish: Duration::ZERO,
        start_attempted: false,
        start_rejected: false,
        cleanup_error: None,
        metadata_disabled: false,
        identity_failed: false,
        recorded_samples: 0,
        snapshot_dirty: true,
    };
    let result = (|| {
        let deadline = io.now() + IDENTITY_TIMEOUT;
        loop {
            if cancel.load(Ordering::Acquire) {
                return Err("Capture start cancelled".into());
            }
            let mut input = [0; 65];
            let n = io.read_control(&mut input, 10)?;
            session.control(&input[..n], None)?;
            let ready = match &request.mode {
                CaptureMode::Single { slot, .. } => session.seen.contains(slot),
                CaptureMode::AllPaired { .. } => {
                    session
                        .expected
                        .keys()
                        .all(|slot| session.seen.contains(slot))
                        && session.registration_cycle
                }
            };
            if ready {
                break;
            }
            if io.now() >= deadline {
                return Err("Could not freshly confirm frozen RF slots before START".into());
            }
            session.publish(io, shared);
        }
        // Discard stale input from a previous session before START, with a hard bound.
        let flush_deadline = io.now() + DRAIN;
        loop {
            if cancel.load(Ordering::Acquire) {
                return Err("Capture start cancelled".into());
            }
            let mut bytes = [0; 4096];
            if io.read_data(&mut bytes)? == 0 {
                break;
            }
            if io.now() >= flush_deadline {
                return Err(
                    "Raw source is already streaming; stop its existing owner first".into(),
                );
            }
        }
        if cancel.load(Ordering::Acquire) {
            return Err("Capture start cancelled".into());
        }
        io.validate_source(&request.source)?;
        let mut input = [0; 65];
        let n = io.read_control(&mut input, 0)?;
        session.control(&input[..n], None)?;
        session.start = Some(io.now());
        session.repair_after = io.now() + WARMUP;
        match request.mode {
            CaptureMode::Single { slot, .. } => {
                session.command(io, 214, &[slot], Some(cancel), shared)?
            }
            CaptureMode::AllPaired { rate_hz } => {
                session.command(io, 221, &[rate_hz, 0], Some(cancel), shared)?
            }
        };
        session.snapshot.phase = CapturePhase::WarmingUp;
        session.warn("START accepted/queued is not evidence of samples; firmware admission warmup is 5 seconds".into());
        while !cancel.load(Ordering::Acquire) {
            let mut input = [0; 65];
            let n = io.read_control(&mut input, 0)?;
            session.control(&input[..n], None)?;
            session.pump_data(io)?;
            if !session.metadata_disabled && io.now() >= session.repair_after {
                session.repair_after = io.now() + ACK_TIMEOUT;
                if let Some(args) = session.missing_request() {
                    // Optional repair rejection/timeout leaves metadata incomplete, not invented.
                    if let Err(error) = session.command(io, 223, &args, Some(cancel), shared) {
                        if !session.metadata_disabled {
                            return Err(error);
                        }
                        session.warn(format!(
                            "Metadata repair disabled for this capture: {error}"
                        ));
                    }
                }
            }
            session.publish(io, shared);
        }
        Ok(())
    })();
    let mut error = result.err();
    session.snapshot.phase = CapturePhase::Stopping;
    if session.start_attempted && !session.start_rejected {
        let opcode = if matches!(request.mode, CaptureMode::Single { .. }) {
            215
        } else {
            222
        };
        if let Err(stop_error) = session.command(io, opcode, &[], None, shared) {
            session.warn(stop_error.clone());
            error = Some(match error {
                Some(previous) => format!("{previous}; STOP: {stop_error}"),
                None => stop_error,
            });
        }
        if let Some(cleanup) = session.cleanup_error.take() {
            error = Some(match error {
                Some(previous) => format!("{previous}; drain: {cleanup}"),
                None => cleanup,
            });
        }
        let deadline = io.now() + DRAIN;
        while !session.data_failed && io.now() < deadline {
            if let Err(drain_error) = session.pump_data(io) {
                session.warn(drain_error.clone());
                if error.is_none() {
                    error = Some(drain_error);
                }
                break;
            }
        }
    }
    if let Some(error) = &error {
        session.warn(error.clone());
    }
    if session.snapshot.counters.malformed > 0 {
        session.warn(format!(
            "{} malformed raw frames were rejected; their slot/sample timing may be unknown",
            session.snapshot.counters.malformed
        ));
    }
    let incomplete: Vec<_> = session
        .snapshot
        .tracks
        .iter()
        .filter(|track| {
            track.samples > 0
                && (track.metadata.raw_tx_hz.is_none()
                    || track.calibration.accel.is_none()
                    || track.calibration.mag.is_none()
                    || track.calibration.gyro_bias_dps.is_none()
                    || track.calibration.gyro_sensitivity.is_none()
                    || track.calibration.thermal.mode.is_none()
                    || !track.calibration.thermal.complete())
        })
        .map(|track| track.identity.slot)
        .collect();
    for slot in incomplete {
        session.warn(format!("Tracker {slot}: metadata/calibration snapshot is incomplete; missing values were not inferred"));
    }
    for warning in &session.snapshot.warnings {
        if let Err(write_error) = session
            .recorder
            .as_mut()
            .expect("active recorder")
            .warning(warning)
        {
            if error.is_none() {
                error = Some(write_error);
            }
            break;
        }
    }
    let summary = session.recorder.take().expect("active recorder").finish();
    let outcome = match summary {
        Ok(summary) => CaptureOutcome {
            path: summary.path,
            samples: summary.samples,
            error,
        },
        Err(finish) => {
            let message = match error {
                Some(error) => format!(
                    "{error}; finalizing {} failed: {finish}",
                    request.destination.display()
                ),
                None => format!(
                    "Finalizing {} failed: {finish}",
                    request.destination.display()
                ),
            };
            session.warn(message.clone());
            // A poisoned recorder intentionally omits further event bytes. Keep its path
            // available for prefix recovery, without claiming successful durable finalization.
            CaptureOutcome {
                path: request.destination.clone(),
                samples: session.recorded_samples,
                error: Some(message),
            }
        }
    };
    session.snapshot.phase = if outcome.error.is_some() {
        CapturePhase::Failed
    } else {
        CapturePhase::Finished
    };
    session.publish(io, shared);
    Ok(outcome)
}
