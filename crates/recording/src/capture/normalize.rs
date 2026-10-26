//! Per-track ordering with an inclusive 200-packet reorder window.
//! Gaps remain gaps: reconstruction and nominal-clock expansion belong to replay.
use super::Sample;
use std::collections::{BTreeMap, VecDeque};

pub const REORDER_LAG: i64 = 200;
pub const MAX_KNOWN_GAP: u16 = 500;

/// Reception telemetry is not part of a tracker observation; retransmissions can
/// have a different RSSI/receiver timestamp while carrying identical sensor data.
/// Derived discontinuities likewise are not wire observation identity. Explicit
/// input boundaries are handled before deduplication in `push`, so they remain
/// authoritative even when their sensor payload matches an earlier observation.
fn same_observation(a: &Sample, b: &Sample) -> bool {
    a.sequence == b.sequence
        && a.gyro == b.gyro
        && a.acceleration_g == b.acceleration_g
        && a.magnetic_gauss == b.magnetic_gauss
        && a.new_mag == b.new_mag
        && a.acceleration_fresh == b.acceleration_fresh
        && a.temperature_c == b.temperature_c
}

#[derive(Default)]
pub struct Normalizer {
    pending: BTreeMap<i64, Sample>,
    history: VecDeque<Sample>,
    highest: Option<(i64, u16)>,
    emitted: Option<i64>,
    last_sequence: Option<u16>,
}

impl Normalizer {
    fn emit_first(&mut self, emit: &mut impl FnMut(Sample)) {
        if let Some((index, mut sample)) = self.pending.pop_first() {
            if let Some(last) = self.last_sequence {
                let span = sample.sequence.wrapping_sub(last);
                if span == 0 || span > MAX_KNOWN_GAP + 1 {
                    sample.discontinuity = true;
                }
            }
            self.emitted = Some(index);
            self.last_sequence = Some(sample.sequence);
            self.history.push_back(sample);
            while self.history.len() > REORDER_LAG as usize + 1 {
                self.history.pop_front();
            }
            emit(sample);
        }
    }

    fn boundary(&mut self, mut sample: Sample, emit: &mut impl FnMut(Sample)) {
        while !self.pending.is_empty() {
            self.emit_first(emit);
        }
        self.history.clear();
        self.emitted = None;
        self.last_sequence = None;
        sample.discontinuity = true;
        self.highest = Some((0, sample.sequence));
        self.pending.insert(0, sample);
    }

    pub fn push(&mut self, sample: Sample, mut emit: impl FnMut(Sample)) {
        if sample.discontinuity {
            self.boundary(sample, &mut emit);
            return;
        }
        let Some((high, sequence)) = self.highest else {
            self.highest = Some((0, sample.sequence));
            self.pending.insert(0, sample);
            return;
        };
        let index = high + i64::from(sample.sequence.wrapping_sub(sequence) as i16);
        if let Some(existing) = self.pending.get(&index) {
            if !same_observation(existing, &sample) {
                self.boundary(sample, &mut emit);
            }
            return;
        }
        if self.emitted.is_some_and(|last| index <= last) || index < high - REORDER_LAG {
            if !self
                .history
                .iter()
                .any(|old| same_observation(old, &sample))
            {
                self.boundary(sample, &mut emit);
            }
            return;
        }
        self.pending.insert(index, sample);
        if index > high {
            self.highest = Some((index, sample.sequence));
        }
        let watermark = self.highest.unwrap().0 - REORDER_LAG;
        while self
            .pending
            .first_key_value()
            .is_some_and(|(index, _)| *index < watermark)
        {
            self.emit_first(&mut emit);
        }
    }

    /// Flush only at a true end or an explicit boundary, never after a read chunk.
    pub fn finish(&mut self, mut emit: impl FnMut(Sample)) {
        while !self.pending.is_empty() {
            self.emit_first(&mut emit);
        }
        self.highest = None;
        self.emitted = None;
        self.last_sequence = None;
        self.history.clear();
    }
}

#[cfg(test)]
pub fn normalize(samples: &[Sample]) -> Vec<Sample> {
    let mut result = Vec::with_capacity(samples.len());
    let mut normalizer = Normalizer::default();
    for sample in samples {
        normalizer.push(*sample, |sample| result.push(sample));
    }
    normalizer.finish(|sample| result.push(sample));
    result
}

#[cfg(test)]
mod tests {
    use super::{Normalizer, normalize};
    use crate::capture::{Gyro, Sample};
    fn row(sequence: u16) -> Sample {
        Sample {
            sequence,
            gyro: Gyro::RatesDps([1., 0., 0.]),
            ..Sample::default()
        }
    }
    #[test]
    fn wrap_and_reordering_do_not_add_boundaries() {
        let rows = normalize(&[row(65534), row(0), row(65535), row(1), row(0)]);
        assert_eq!(
            rows.iter().map(|r| r.sequence).collect::<Vec<_>>(),
            [65534, 65535, 0, 1]
        );
        assert!(rows.iter().all(|r| !r.discontinuity));
    }
    #[test]
    fn full_payload_conflict_is_a_boundary() {
        let mut conflict = row(8);
        conflict.temperature_c = Some(0.);
        let rows = normalize(&[row(8), conflict]);
        assert_eq!(rows.len(), 2);
        assert!(rows[1].discontinuity);
    }
    #[test]
    fn inclusive_lag_and_bounded_late_evidence() {
        let mut state = Normalizer::default();
        let mut out = Vec::new();
        state.push(row(200), |s| out.push(s));
        state.push(row(0), |s| out.push(s));
        assert!(out.is_empty());
        state.finish(|s| out.push(s));
        assert_eq!(out[0].sequence, 0);
        assert!(!out[1].discontinuity);
        let rows = normalize(&[row(300), row(99)]);
        assert!(rows[1].discontinuity);
    }
    #[test]
    fn known_gaps_and_unknown_time_are_distinct() {
        let rows = normalize(&[row(0), row(501), row(1003)]);
        assert!(!rows[1].discontinuity);
        assert!(rows[2].discontinuity);
    }
    #[test]
    fn receiver_telemetry_does_not_turn_retry_into_conflict() {
        let mut retry = row(1);
        retry.receiver_ticks = Some(20);
        retry.rssi = Some(-70);
        assert_eq!(normalize(&[row(1), retry]).len(), 1);
    }
    #[test]
    fn retransmission_of_derived_boundary_is_not_another_reset() {
        let rows = normalize(&[row(0), row(1000), row(1201), row(1000)]);
        assert_eq!(
            rows.iter().map(|r| r.sequence).collect::<Vec<_>>(),
            [0, 1000, 1201]
        );
        assert!(rows[1].discontinuity);
        assert!(!rows[2].discontinuity);
    }
    #[test]
    fn explicit_boundary_is_not_deduplicated_even_with_identical_payload() {
        let mut boundary = row(10);
        boundary.discontinuity = true;
        let rows = normalize(&[row(10), boundary]);
        assert_eq!(rows.len(), 2);
        assert!(rows[1].discontinuity);
    }
}
