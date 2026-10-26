use super::{
    Record, Subscription, Summary, TraceItem,
    format::Writer,
    hub::{self, Hub},
};
use std::{
    fs::File,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

/// Own until joined, including after stop. Creation and every file write run off the caller thread.
pub struct Recorder {
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<Result<Summary, String>>>,
}
impl Recorder {
    pub fn start(path: PathBuf) -> Result<Self, String> {
        Self::start_on(path, hub::global().clone())
    }
    pub(crate) fn start_on(path: PathBuf, hub: Arc<Hub>) -> Result<Self, String> {
        let subscription = hub.subscribe(16384);
        let stop = Arc::new(AtomicBool::new(false));
        let cancel = stop.clone();
        let worker = thread::Builder::new()
            .name("hid-log-writer".into())
            .spawn(move || {
                let file = create_file(&path)?;
                let mut writer = Writer::new(file, &hub.epoch_utc, false)?;
                drain(subscription, &cancel, &mut writer)?;
                let (file, summary) = writer.finish()?;
                file.sync_all().map_err(|e| format!("Sync HID log: {e}"))?;
                Ok(summary)
            })
            .map_err(|e| format!("Start HID recorder: {e}"))?;
        Ok(Self {
            stop,
            worker: Some(worker),
        })
    }
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Release);
    }
    pub fn is_finished(&self) -> bool {
        self.worker.as_ref().is_none_or(JoinHandle::is_finished)
    }
    pub fn join(mut self) -> Result<Summary, String> {
        self.stop();
        self.worker
            .take()
            .expect("owned recorder worker")
            .join()
            .map_err(|_| "HID recorder worker panicked".to_string())?
    }
}
impl Drop for Recorder {
    fn drop(&mut self) {
        self.stop();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn drain<W: std::io::Write>(
    mut subscription: Subscription,
    cancel: &AtomicBool,
    writer: &mut Writer<W>,
) -> Result<(), String> {
    let mut flushed = Instant::now();
    loop {
        let stopping = cancel.load(Ordering::Acquire);
        if stopping {
            subscription.stop();
        }
        // Bound each pass, so a busy producer cannot postpone stop indefinitely.
        let mut received = 0;
        while received < 1024 {
            let Some(item) = subscription.try_recv() else {
                break;
            };
            writer.item(&item)?;
            received += 1;
        }
        if stopping && received == 0 {
            break;
        }
        if flushed.elapsed() >= Duration::from_secs(1) {
            writer.flush()?;
            flushed = Instant::now();
        }
        if received == 0 {
            thread::sleep(Duration::from_millis(10));
        }
    }
    Ok(())
}

fn create_file(path: &Path) -> Result<File, String> {
    File::options()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                format!(
                    "HID log destination already exists; choose a new filename: {}",
                    path.display()
                )
            } else {
                format!("Create {}: {e}", path.display())
            }
        })
}

/// Snapshot export is deliberately synchronous: its owner must call it on a background worker.
pub fn save_snapshot(
    path: &Path,
    records: &[Arc<Record>],
    dropped: u64,
) -> Result<Summary, String> {
    let file = create_file(path)?;
    let mut writer = Writer::new(file, &hub::global().epoch_utc, true)?;
    if dropped != 0 {
        writer.item(&TraceItem::Gap { count: dropped })?;
    }
    let mut previous: Option<u64> = None;
    for record in records {
        if let Some(sequence) = previous {
            if record.sequence <= sequence {
                return Err("HID snapshot records must be in increasing sequence order".into());
            }
            if record.sequence != sequence.saturating_add(1) {
                writer.discontinuity(sequence, record.sequence)?;
            }
        }
        writer.record(record)?;
        previous = Some(record.sequence);
    }
    let (file, summary) = writer.finish()?;
    file.sync_all()
        .map_err(|e| format!("Sync HID snapshot: {e}"))?;
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::{Recorder, save_snapshot};
    use crate::domain::ReceiverEndpoint;
    use crate::hid_log::{
        Direction, RecordData, ReportBytes, Source,
        format::{Data, Framing, Row},
        hub::Hub,
    };
    use std::{
        fs,
        io::{self, Write},
        sync::Arc,
    };
    fn source() -> Arc<Source> {
        Arc::new(Source::main(&ReceiverEndpoint {
            key: "native".into(),
            label: "test receiver".into(),
            serial_number: Some("serial".into()),
            path: vec![255, 0, 5],
        }))
    }
    fn rows(path: &std::path::Path) -> Vec<Row> {
        fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
    #[test]
    fn continuous_roundtrip_preserves_bytes_framing_sources_errors_and_clean_end() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("raw.hidlog");
        let hub = Hub::new();
        let source = source();
        let recorder = Recorder::start_on(path.clone(), hub.clone()).unwrap();
        hub.publish(&source, RecordData::Open);
        let bytes: Vec<u8> = (0..65).collect();
        hub.publish(
            &source,
            RecordData::Report {
                direction: Direction::Out,
                bytes: ReportBytes::new(&bytes).unwrap(),
                transferred: Some(12),
                error: None,
            },
        );
        hub.publish(
            &source,
            RecordData::Report {
                direction: Direction::Out,
                bytes: ReportBytes::new(&bytes).unwrap(),
                transferred: None,
                error: Some("write failed".into()),
            },
        );
        let raw = Arc::new(Source::raw(
            &ReceiverEndpoint {
                key: "native".into(),
                label: "raw".into(),
                serial_number: None,
                path: vec![1],
            },
            &[9, 8],
        ));
        hub.publish(
            &raw,
            RecordData::Report {
                direction: Direction::In,
                bytes: ReportBytes::new(&[251, 0, 255]).unwrap(),
                transferred: Some(3),
                error: None,
            },
        );
        hub.publish(&source, RecordData::Close);
        recorder.stop();
        let summary = recorder.join().unwrap();
        assert_eq!(summary.records, 5);
        assert_eq!(summary.reports, 3);
        assert_eq!(summary.bytes, 133);
        assert_eq!(summary.sources, 2);
        assert_eq!(summary.dropped, 0);
        let rows = rows(&path);
        assert!(matches!(
            &rows[0],
            Row::Header {
                schema_version: 1,
                snapshot: false,
                ..
            }
        ));
        assert!(matches!(&rows[1], Row::Source { source: saved } if saved == source.as_ref()));
        let reports: Vec<_> = rows
            .iter()
            .filter_map(|row| {
                if let Row::Record {
                    data:
                        Data::Report {
                            bytes,
                            framing,
                            transferred,
                            error,
                            length,
                            ..
                        },
                    ..
                } = row
                {
                    Some((bytes, framing, transferred, error, length))
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(reports[0].0, &bytes);
        assert_eq!(*reports[0].1, Framing::ReportIdPrefix);
        assert_eq!(*reports[0].2, Some(12));
        assert_eq!(*reports[0].4, 65);
        assert_eq!(*reports[1].2, None);
        assert_eq!(reports[1].3.as_deref(), Some("write failed"));
        assert_eq!(*reports[2].1, Framing::NativeInput);
        let sequences: Vec<_> = rows
            .iter()
            .filter_map(|row| {
                if let Row::Record { sequence, .. } = row {
                    Some(*sequence)
                } else {
                    None
                }
            })
            .collect();
        assert!(sequences.windows(2).all(|s| s[0] < s[1]));
        assert!(matches!(rows.last(), Some(Row::End { summary: end }) if *end == summary));
    }
    #[test]
    fn writer_failure_cannot_produce_success_or_clean_end() {
        #[derive(Debug)]
        struct Fail;
        impl Write for Fail {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::other("disk full"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let writer = crate::hid_log::format::Writer::new(Fail, "epoch", false).unwrap();
        assert!(writer.finish().unwrap_err().contains("disk full"));
    }
    #[test]
    fn invalid_destination_is_owned_until_join_and_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let recorder =
            Recorder::start_on(dir.path().join("missing/log.hidlog"), Hub::new()).unwrap();
        recorder.stop();
        assert!(recorder.join().unwrap_err().contains("Create"));
    }
    #[test]
    fn stopped_writer_drains_queue_and_trailing_overflow_into_end_summary() {
        let hub = Hub::new();
        let source = source();
        let subscription = hub.subscribe(2);
        for _ in 0..9 {
            hub.publish(&source, RecordData::Open);
        }
        let mut writer =
            crate::hid_log::format::Writer::new(Vec::new(), &hub.epoch_utc, false).unwrap();
        super::drain(
            subscription,
            &std::sync::atomic::AtomicBool::new(true),
            &mut writer,
        )
        .unwrap();
        let (bytes, summary) = writer.finish().unwrap();
        assert_eq!(summary.records, 2);
        assert_eq!(summary.dropped, 7);
        let rows: Vec<Row> = std::str::from_utf8(&bytes)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert!(matches!(&rows[rows.len() - 2], Row::Gap { count: 7 }));
        assert!(matches!(rows.last(), Some(Row::End { summary: end }) if *end == summary));
    }
    #[test]
    fn snapshot_marks_loss_and_noncontiguous_selection_and_roundtrips_malformed_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("snapshot.hidlog");
        let hub = Hub::new();
        let source = source();
        let mut sub = hub.subscribe(4);
        hub.publish(&source, RecordData::Open);
        hub.publish(&source, RecordData::Close);
        hub.publish(
            &source,
            RecordData::Report {
                direction: Direction::In,
                bytes: ReportBytes::new(&[0, 255, 1, 2, 3]).unwrap(),
                transferred: Some(5),
                error: None,
            },
        );
        let mut records = Vec::new();
        while let Some(item) = sub.try_recv() {
            if let crate::hid_log::TraceItem::Record(r) = item {
                records.push(r);
            }
        }
        records.remove(1);
        let summary = save_snapshot(&path, &records, 7).unwrap();
        assert_eq!(summary.dropped, 7);
        assert_eq!(summary.records, 2);
        let rows = rows(&path);
        assert!(rows.iter().any(|r| matches!(r, Row::Gap { count: 7 })));
        assert!(rows.iter().any(|r| matches!(
            r,
            Row::Discontinuity {
                previous_sequence: 1,
                next_sequence: 3
            }
        )));
        assert!(matches!(rows.last(), Some(Row::End { .. })));
    }
    #[test]
    fn existing_logs_are_never_replaced_by_recorder_or_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("existing.hidlog");
        let original = b"original HID log bytes\n";
        fs::write(&path, original).unwrap();
        assert!(
            save_snapshot(&path, &[], 0)
                .unwrap_err()
                .contains("choose a new filename")
        );
        assert_eq!(fs::read(&path).unwrap(), original);
        let recorder = Recorder::start_on(path.clone(), Hub::new()).unwrap();
        assert!(
            recorder
                .join()
                .unwrap_err()
                .contains("choose a new filename")
        );
        assert_eq!(fs::read(&path).unwrap(), original);
    }
    #[test]
    fn active_recording_and_hardlink_alias_reject_snapshot_without_modifying_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("active.hidlog");
        let recorder = Recorder::start_on(path.clone(), Hub::new()).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let original = loop {
            if let Ok(bytes) = fs::read(&path)
                && !bytes.is_empty()
            {
                break bytes;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "active writer did not flush header"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        assert!(
            save_snapshot(&path, &[], 0)
                .unwrap_err()
                .contains("choose a new filename")
        );
        assert_eq!(fs::read(&path).unwrap(), original);
        let alias = dir.path().join("alias.hidlog");
        fs::hard_link(&path, &alias).unwrap();
        assert!(
            save_snapshot(&alias, &[], 0)
                .unwrap_err()
                .contains("choose a new filename")
        );
        assert_eq!(fs::read(&path).unwrap(), original);
        recorder.stop();
        recorder.join().unwrap();
        assert!(matches!(rows(&path).last(), Some(Row::End { .. })));
    }
    #[test]
    fn oversized_native_report_is_rejected_not_truncated() {
        assert!(ReportBytes::new(&[1; 66]).is_err());
        assert_eq!(ReportBytes::new(&[7; 65]).unwrap().as_slice(), &[7; 65]);
    }
}
