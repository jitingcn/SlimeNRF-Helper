use super::load;
use super::test_support::{CSV, META, cancel};
use crate::capture::Gyro;
use std::path::Path;

fn assert_synthetic_pair(csv: &Path, metadata: &Path) {
    std::fs::write(csv, CSV).unwrap();
    std::fs::write(metadata, META).unwrap();
    for path in [csv, metadata] {
        let recording = load(path, &cancel()).unwrap();
        let track = &recording.tracks[0];
        assert_eq!(track.samples.len(), 4);
        assert_eq!(track.samples[0].sequence, 0);
        assert_eq!(track.samples[0].gyro, Gyro::RatesDps([-12.5, 25., -50.]));
        assert_eq!(track.metadata.raw_tx_hz, Some(204.8));
        assert_eq!(track.samples[0].temperature_c, None);
        assert!(!track.samples[2].acceleration_fresh);
    }
}

#[test]
fn metadata_selection_removes_exactly_the_final_suffix() {
    let directory = tempfile::tempdir().unwrap();
    assert_synthetic_pair(
        &directory.path().join("温度.meta.txt.csv"),
        &directory.path().join("温度.meta.txt.meta.txt"),
    );
}

#[cfg(unix)]
#[test]
fn metadata_selection_preserves_non_utf8_basename() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let directory = tempfile::tempdir().unwrap();
    let csv = directory
        .path()
        .join(OsString::from_vec(b"synthetic-\xff.csv".to_vec()));
    let metadata = directory
        .path()
        .join(OsString::from_vec(b"synthetic-\xff.meta.txt".to_vec()));
    assert_eq!(
        super::recording_input_path(&metadata).as_ref(),
        csv.as_path()
    );
    // Native macOS volumes reject this filename before the recording can be opened.
    #[cfg(not(target_os = "macos"))]
    assert_synthetic_pair(&csv, &metadata);
}
