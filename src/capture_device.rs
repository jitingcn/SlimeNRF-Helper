//! Exclusive, real-I/O collection. The caller owns receiver/CDC reservations until join.
//! Main HID is read only here while reserved; raw HID is a separate input collection.
use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::Duration;

use slimenrf_recording::capture::{CalibrationSnapshot, Sample, SensorMetadata, TrackIdentity};

mod collection;
mod io;
mod source;
mod worker;

pub use source::{CaptureSource, CaptureTransport, discover_sources};
pub use worker::CaptureWorker;

#[derive(Clone, Debug)]
pub enum CaptureMode {
    Single { slot: u8, rf_address: String },
    AllPaired { rate_hz: u8 },
}
#[derive(Clone, Debug)]
pub struct CaptureRequest {
    pub source: CaptureSource,
    pub mode: CaptureMode,
    pub destination: PathBuf,
    /// Frozen positive RF identities, not the transient post-reservation monitor snapshot.
    pub roster: Vec<(u8, String)>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapturePhase {
    Opening,
    Starting,
    WarmingUp,
    WaitingForSamples,
    Receiving,
    Stopping,
    Finished,
    Failed,
}
#[derive(Clone, Debug, Default)]
pub struct CaptureCounters {
    pub reports: u64,
    pub samples: u64,
    pub malformed: u64,
    /// Forward missing sequence numbers observed; late retransmissions may subsequently fill them.
    pub sequence_gaps: u64,
}
#[derive(Clone, Debug)]
pub struct LiveTrack {
    pub identity: TrackIdentity,
    pub metadata: SensorMetadata,
    pub calibration: CalibrationSnapshot,
    pub samples: u64,
    pub latest: VecDeque<Sample>,
}
#[derive(Clone, Debug)]
pub struct CaptureSnapshot {
    pub phase: CapturePhase,
    pub counters: CaptureCounters,
    pub elapsed: Duration,
    pub rate_hz: f64,
    pub tracks: Vec<LiveTrack>,
    pub warnings: Vec<String>,
}
impl Default for CaptureSnapshot {
    fn default() -> Self {
        Self {
            phase: CapturePhase::Opening,
            counters: CaptureCounters::default(),
            elapsed: Duration::ZERO,
            rate_hz: 0.0,
            tracks: Vec::new(),
            warnings: Vec::new(),
        }
    }
}
#[derive(Clone, Debug)]
pub struct CaptureOutcome {
    pub path: PathBuf,
    pub samples: u64,
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::{CaptureCounters, CapturePhase, CaptureSnapshot};
    #[test]
    fn snapshot_counters_start_at_real_zero() {
        let snapshot = CaptureSnapshot::default();
        assert_eq!(snapshot.phase, CapturePhase::Opening);
        assert_eq!(
            snapshot.counters.samples,
            CaptureCounters::default().samples
        );
        assert!(snapshot.tracks.is_empty());
    }
}
