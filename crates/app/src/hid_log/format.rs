use super::{Direction, Record, RecordData, Source, Summary, TraceItem};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    io::{BufWriter, Write},
};

pub(super) const SCHEMA_VERSION: u32 = 1;
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum Row {
    Header {
        format: String,
        schema_version: u32,
        app_version: String,
        created_utc: String,
        monotonic_epoch_utc: String,
        timing_unit: String,
        snapshot: bool,
    },
    Source {
        source: Source,
    },
    Record {
        sequence: u64,
        elapsed_us: u64,
        connection_id: u64,
        data: Data,
    },
    Gap {
        count: u64,
    },
    Discontinuity {
        previous_sequence: u64,
        next_sequence: u64,
    },
    End {
        summary: Summary,
    },
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum Data {
    Open,
    Close,
    Error {
        message: String,
    },
    Report {
        direction: Direction,
        bytes: Vec<u8>,
        length: usize,
        framing: Framing,
        transferred: Option<usize>,
        error: Option<String>,
    },
}
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum Framing {
    NativeInput,
    ReportIdPrefix,
}

/// All allocation/serialization and disk I/O lives on the owning file worker.
pub(super) struct Writer<W: Write> {
    output: BufWriter<W>,
    row: Vec<u8>,
    sources: HashSet<u64>,
    summary: Summary,
}
impl<W: Write> Writer<W> {
    pub(super) fn new(output: W, epoch_utc: &str, snapshot: bool) -> Result<Self, String> {
        let mut writer = Self {
            output: BufWriter::new(output),
            row: Vec::with_capacity(1024),
            sources: HashSet::new(),
            summary: Summary::default(),
        };
        writer.write(&Row::Header {
            format: "slimenrf-hidlog".into(),
            schema_version: SCHEMA_VERSION,
            app_version: env!("CARGO_PKG_VERSION").into(),
            created_utc: chrono::Utc::now().to_rfc3339(),
            monotonic_epoch_utc: epoch_utc.into(),
            timing_unit: "microseconds".into(),
            snapshot,
        })?;
        Ok(writer)
    }
    fn write(&mut self, row: &Row) -> Result<(), String> {
        self.row.clear();
        serde_json::to_writer(&mut self.row, row).map_err(|e| format!("Encode HID log: {e}"))?;
        self.row.push(b'\n');
        self.output
            .write_all(&self.row)
            .map_err(|e| format!("Write HID log: {e}"))
    }
    pub(super) fn item(&mut self, item: &TraceItem) -> Result<(), String> {
        match item {
            TraceItem::Record(record) => self.record(record),
            TraceItem::Gap { count } => {
                self.write(&Row::Gap { count: *count })?;
                self.summary.dropped = self.summary.dropped.saturating_add(*count);
                Ok(())
            }
        }
    }
    pub(super) fn record(&mut self, record: &Record) -> Result<(), String> {
        if self.sources.insert(record.source.connection_id) {
            self.write(&Row::Source {
                source: (*record.source).clone(),
            })?;
            self.summary.sources += 1;
        }
        let data = match &record.data {
            RecordData::Open => Data::Open,
            RecordData::Close => Data::Close,
            RecordData::Error { message } => Data::Error {
                message: message.clone(),
            },
            RecordData::Report {
                direction,
                bytes,
                transferred,
                error,
            } => Data::Report {
                direction: *direction,
                bytes: bytes.as_slice().to_vec(),
                length: bytes.as_slice().len(),
                framing: match direction {
                    Direction::In => Framing::NativeInput,
                    Direction::Out => Framing::ReportIdPrefix,
                },
                transferred: *transferred,
                error: error.clone(),
            },
        };
        self.write(&Row::Record {
            sequence: record.sequence,
            elapsed_us: record.elapsed_us,
            connection_id: record.source.connection_id,
            data,
        })?;
        self.summary.records += 1;
        if let RecordData::Report { bytes, .. } = &record.data {
            self.summary.reports += 1;
            self.summary.bytes += bytes.as_slice().len() as u64;
        }
        Ok(())
    }
    pub(super) fn discontinuity(
        &mut self,
        previous_sequence: u64,
        next_sequence: u64,
    ) -> Result<(), String> {
        self.write(&Row::Discontinuity {
            previous_sequence,
            next_sequence,
        })
    }
    pub(super) fn flush(&mut self) -> Result<(), String> {
        self.output
            .flush()
            .map_err(|e| format!("Flush HID log: {e}"))
    }
    pub(super) fn finish(mut self) -> Result<(W, Summary), String> {
        self.write(&Row::End {
            summary: self.summary.clone(),
        })?;
        self.flush()?;
        let output = self
            .output
            .into_inner()
            .map_err(|e| format!("Finish HID log: {e}"))?;
        Ok((output, self.summary))
    }
}
