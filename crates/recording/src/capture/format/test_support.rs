use super::MAX_SAMPLES;
use super::csv::load_csv;
use crate::capture::protocol::ParsedReport;
use crate::capture::{Gyro, Sample, Track, TrackIdentity};
use std::io::Cursor;
use std::sync::atomic::AtomicBool;
pub(super) const CSV: &str = include_str!("../fixtures/decimated_excerpt.csv");
pub(super) const META: &str = include_str!("../fixtures/decimated_excerpt.meta.txt");
pub(super) fn cancel() -> AtomicBool {
    AtomicBool::new(false)
}
pub(super) fn identity(receiver: &str) -> TrackIdentity {
    TrackIdentity {
        receiver: receiver.into(),
        slot: 3,
        rf_address: Some("AABBCCDD".into()),
    }
}
pub(super) fn observation(sequence: u16) -> ParsedReport {
    ParsedReport::Sample {
        slot: 3,
        sample: Sample {
            sequence,
            gyro: Gyro::IntegratedQuat([1., 0., 0., 0.]),
            temperature_c: Some(-3.25),
            ..Sample::default()
        },
    }
}
pub(super) fn fixture() -> Track {
    load_csv(
        Cursor::new(CSV),
        META,
        "synthetic-decimated-fixture",
        &cancel(),
        &mut MAX_SAMPLES.clone(),
    )
    .unwrap()
}
