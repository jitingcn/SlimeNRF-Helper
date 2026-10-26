use super::{FINAL_EVENT_RESERVE, Recorder, load_native};
use crate::capture::format::test_support::{cancel, identity, observation};
use crate::capture::format::{MAX_INPUT_BYTES, MAX_SAMPLES, load};
use crate::capture::protocol::ParsedReport;
use std::io::{Cursor, Write};
#[test]
fn native_durable_roundtrip_interns_same_slot_distinct_receivers() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("capture.slimecap");
    let a = identity("long-receiver-A");
    let b = identity("long-receiver-B");
    let mut recorder = Recorder::create(&path, "reference source").unwrap();
    recorder.register_track(a.clone()).unwrap();
    recorder.register_track(b.clone()).unwrap();
    recorder.record(&a, &observation(65535)).unwrap();
    let mut boundary = observation(0);
    if let ParsedReport::Sample { sample, .. } = &mut boundary {
        sample.discontinuity = true;
    }
    recorder.record(&a, &boundary).unwrap();
    recorder.record(&b, &observation(7)).unwrap();
    recorder.warning("STOP acknowledgement absent").unwrap();
    let summary = recorder.finish().unwrap();
    assert_eq!(summary.samples, 3);
    let text = std::fs::read_to_string(&path).unwrap();
    assert_eq!(text.matches("long-receiver-A").count(), 1);
    let recording = load(&path, &cancel()).unwrap();
    assert_eq!(recording.tracks.len(), 2);
    assert_eq!(recording.tracks[0].identity, a);
    assert_eq!(recording.tracks[1].identity, b);
    assert!(recording.tracks[0].samples[1].discontinuity);
    assert_eq!(recording.tracks[0].samples[0].temperature_c, Some(-3.25));
    assert!(recording.warnings.iter().any(|w| w.contains("STOP")));
}

#[test]
fn interrupted_and_truncated_native_streams_are_recoverable() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("partial.slimecap");
    {
        let mut recorder = Recorder::create(&path, "test").unwrap();
        let id = identity("receiver");
        recorder.register_track(id.clone()).unwrap();
        recorder.record(&id, &observation(10)).unwrap();
    }
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    file.write_all(b"{\"event\":\"report\",").unwrap();
    drop(file);
    let recording = load(&path, &cancel()).unwrap();
    assert_eq!(recording.tracks[0].samples.len(), 1);
    assert!(recording.warnings.iter().any(|w| w.contains("unfinished")));
    assert!(
        recording
            .warnings
            .iter()
            .any(|w| w.contains("incomplete last"))
    );
}

#[test]
fn empty_finished_recording_is_valid_and_bad_version_is_rejected() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("empty.slimecap");
    let mut recorder = Recorder::create(&path, "test").unwrap();
    recorder.register_track(identity("r")).unwrap();
    recorder.finish().unwrap();
    assert!(load(&path, &cancel()).unwrap().tracks[0].samples.is_empty());
    assert!(load_native(Cursor::new(b"{\"event\":\"header\",\"format\":\"slimecap\",\"version\":999,\"identity\":\"x\"}\n"), &cancel()).is_err());
    let malformed_middle = b"{\"event\":\"header\",\"format\":\"slimecap\",\"version\":1,\"identity\":\"x\"}\n{broken\n{\"event\":\"end\",\"samples\":0}\n";
    assert!(load_native(Cursor::new(malformed_middle), &cancel()).is_err());
}
#[test]
fn recording_limits_refuse_whole_events_and_leave_room_for_finish() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("limit.slimecap");
    let id = identity("bounded-receiver");
    let mut recorder = Recorder::create(&path, "test").unwrap();
    recorder.register_track(id.clone()).unwrap();
    recorder.record(&id, &observation(1)).unwrap();
    let actual_samples = recorder.samples;
    recorder.samples = MAX_SAMPLES as u64;
    assert!(
        recorder
            .record(&id, &observation(2))
            .unwrap_err()
            .contains("sample limit")
    );
    recorder.samples = actual_samples;
    recorder.written_bytes = MAX_INPUT_BYTES - FINAL_EVENT_RESERVE - 1;
    assert!(
        recorder
            .record(&id, &observation(2))
            .unwrap_err()
            .contains("size limit")
    );
    assert!(!recorder.failed);
    recorder
        .warning("Recording stopped at its loadable size limit")
        .unwrap();
    recorder.finish().unwrap();
    let recording = load(&path, &cancel()).unwrap();
    assert_eq!(recording.tracks[0].samples.len(), 1);
    assert!(recording.warnings.iter().any(|w| w.contains("size limit")));
}

#[test]
fn genuine_sink_failure_prevents_diagnostics_or_end_from_appending() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("sink.slimecap");
    let id = identity("failed-receiver");
    let mut recorder = Recorder::create(&path, "test").unwrap();
    recorder.register_track(id.clone()).unwrap();
    // A read-only real file handle makes the production writer fail, without
    // a fake successful sink or a hardware command.
    recorder.writer = std::io::BufWriter::with_capacity(1, std::fs::File::open(&path).unwrap());
    let prefix = std::fs::read(&path).unwrap();
    assert!(recorder.record(&id, &observation(1)).is_err());
    assert!(recorder.failed);
    assert!(recorder.warning("write failed").is_err());
    assert!(recorder.finish().is_err());
    assert_eq!(std::fs::read(&path).unwrap(), prefix);
    let recovered = load(&path, &cancel()).unwrap();
    assert!(recovered.tracks[0].samples.is_empty());
    assert!(recovered.warnings.iter().any(|w| w.contains("unfinished")));
}

#[test]
fn full_native_diagnostic_budget_and_recovery_roundtrip_without_loss() {
    use crate::capture::format::export_csv_zip;

    let directory = tempfile::tempdir().unwrap();
    for truncated in [false, true] {
        let path = directory
            .path()
            .join(format!("recovery-{truncated}.slimecap"));
        let id = identity("synthetic-recovery-receiver");
        {
            let mut recorder = Recorder::create(&path, "synthetic recovery source").unwrap();
            recorder.register_track(id.clone()).unwrap();
            recorder.record(&id, &observation(7)).unwrap();
            for index in 0..4096 {
                recorder
                    .warning(&format!("synthetic warning {index}"))
                    .unwrap();
            }
        }
        if truncated {
            std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap()
                .write_all(b"{\"event\":\"report\",")
                .unwrap();
        }
        let recording = load(&path, &cancel()).unwrap();
        assert_eq!(
            recording.warnings.len(),
            if truncated { 4098 } else { 4097 }
        );
        assert_eq!(recording.warnings[0], "synthetic warning 0");
        assert_eq!(recording.warnings[4095], "synthetic warning 4095");
        assert_eq!(
            recording.warnings.last().unwrap(),
            "Recovered unfinished capture; no durable finish marker was present."
        );
        let exported = directory.path().join(format!("recovery-{truncated}.zip"));
        export_csv_zip(&recording, &exported, &cancel()).unwrap();
        let restored = load(&exported, &cancel()).unwrap();
        assert_eq!(restored.warnings, recording.warnings);
        assert_eq!(restored.provenance, recording.provenance);
        assert_eq!(restored.tracks[0].samples[0].sequence, 7);
    }
}

#[test]
fn native_context_byte_budget_counts_json_escapes_and_keeps_rejected_event_out() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("escaped-context.slimecap");
    let mut recorder = Recorder::create(&path, "synthetic context source").unwrap();
    recorder.register_track(identity("synthetic")).unwrap();
    let message = "\0".repeat(16 * 1024);
    for _ in 0..5 {
        recorder.warning(&message).unwrap();
    }
    assert!(
        recorder
            .warning(&message)
            .unwrap_err()
            .contains("context entry or byte limit")
    );
    recorder.finish().unwrap();
    let restored = load(&path, &cancel()).unwrap();
    assert_eq!(restored.warnings, vec![message; 5]);
}
