//! Stateless typed views over preserved native reports. Formatting is demand-driven.
mod control;
mod events;
mod presentation;
mod sensor;
#[cfg(test)]
mod tests;

use super::{Direction, Interface, Record, RecordData};
pub use control::{Command, CommandAck, Ota, WomResult};
pub use events::{EventSource, TrackerEvent};
pub use presentation::event_kind_label;
pub(crate) use sensor::BatteryTemperature;
pub use sensor::MainSensor;
use slimenrf_recording::capture::protocol::{self, ParsedReport};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    Sensor,
    Identity,
    Command,
    Event,
    Ota,
    Lifecycle,
    Unknown,
    Malformed,
}
impl Category {
    pub const ALL: [Self; 8] = [
        Self::Sensor,
        Self::Identity,
        Self::Command,
        Self::Event,
        Self::Ota,
        Self::Lifecycle,
        Self::Unknown,
        Self::Malformed,
    ];
    pub fn label(self) -> String {
        crate::i18n::tr(match self {
            Self::Sensor => "hid.category.sensor",
            Self::Identity => "hid.category.identity",
            Self::Command => "hid.category.command",
            Self::Event => "hid.category.event",
            Self::Ota => "hid.category.ota",
            Self::Lifecycle => "hid.category.connection",
            Self::Unknown => "hid.category.unknown",
            Self::Malformed => "hid.category.malformed",
        })
    }
}
#[derive(Clone, Debug, PartialEq)]
pub enum DecodedData {
    Sensor(MainSensor),
    Raw(ParsedReport),
    Identity { address_wire: [u8; 6] },
    Ack(CommandAck),
    Command(Command),
    Event(TrackerEvent),
    Ota(Ota),
    Padding,
    Open,
    Close,
    Error(String),
    Unknown { record_type: Option<u8> },
    Malformed { reason: MalformedReason },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MalformedReason {
    MainLength(usize),
    OutLength(usize),
    ReportId(u8),
    Raw(String),
    ZeroQuaternion,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriteOutcome {
    pub attempted: usize,
    pub transferred: Option<usize>,
    pub error: Option<String>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct DecodedRecord {
    pub byte_offset: usize,
    pub byte_len: usize,
    pub tracker_id: Option<u8>,
    pub category: Category,
    pub event_kind: Option<u8>,
    pub data: DecodedData,
    pub write_outcome: Option<WriteOutcome>,
}
impl DecodedRecord {
    fn new(
        offset: usize,
        len: usize,
        tracker_id: Option<u8>,
        category: Category,
        data: DecodedData,
    ) -> Self {
        let event_kind = if let DecodedData::Event(event) = &data {
            Some(event.kind)
        } else {
            None
        };
        Self {
            byte_offset: offset,
            byte_len: len,
            tracker_id,
            category,
            event_kind,
            data,
            write_outcome: None,
        }
    }
}

/// Fixed report capacity; consumes without allocating a collection.
pub type DecodedRecords = std::iter::Flatten<std::array::IntoIter<Option<DecodedRecord>, 4>>;
type Rows = [Option<DecodedRecord>; 4];
fn one(row: DecodedRecord) -> Rows {
    [Some(row), None, None, None]
}
pub fn decode(record: &Record) -> DecodedRecords {
    let rows = match &record.data {
        RecordData::Open => one(DecodedRecord::new(
            0,
            0,
            None,
            Category::Lifecycle,
            DecodedData::Open,
        )),
        RecordData::Close => one(DecodedRecord::new(
            0,
            0,
            None,
            Category::Lifecycle,
            DecodedData::Close,
        )),
        RecordData::Error { message } => one(DecodedRecord::new(
            0,
            0,
            None,
            Category::Lifecycle,
            DecodedData::Error(message.clone()),
        )),
        RecordData::Report {
            direction,
            bytes,
            transferred,
            error,
        } => {
            let mut rows = decode_report(record.source.interface, *direction, bytes.as_slice());
            if *direction == Direction::Out {
                // The DTO decodes attempted output, never claims a failed/short write was sent.
                for row in rows.iter_mut().flatten() {
                    row.write_outcome = Some(WriteOutcome {
                        attempted: bytes.as_slice().len(),
                        transferred: *transferred,
                        error: error.clone(),
                    });
                }
            }
            rows
        }
    };
    rows.into_iter().flatten()
}
fn malformed(len: usize, reason: MalformedReason) -> Rows {
    one(DecodedRecord::new(
        0,
        len,
        None,
        Category::Malformed,
        DecodedData::Malformed { reason },
    ))
}
fn decode_report(interface: Interface, direction: Direction, b: &[u8]) -> Rows {
    if direction == Direction::Out {
        if b.len() != 65 {
            return malformed(b.len(), MalformedReason::OutLength(b.len()));
        }
        if b[0] != 0 {
            return malformed(b.len(), MalformedReason::ReportId(b[0]));
        }
        let p = &b[1..];
        let (tracker, category, data) = if interface == Interface::Main && p[0] == 254 {
            let command = Command::parse(p);
            (
                command.tracker_id(),
                Category::Command,
                DecodedData::Command(command),
            )
        } else if interface == Interface::Main && matches!(p[0], 0xf0..=0xf7) {
            let ota = Ota::outbound(p);
            let target = ota.target();
            (
                (target < 0xfe).then_some(target),
                Category::Ota,
                DecodedData::Ota(ota),
            )
        } else {
            (
                None,
                Category::Unknown,
                DecodedData::Unknown {
                    record_type: Some(p[0]),
                },
            )
        };
        // Include the report-ID prefix in the inspected raw bytes.
        return one(DecodedRecord::new(0, b.len(), tracker, category, data));
    }
    if interface == Interface::Raw {
        // Avoid allocating the parser's error string for unsupported high-rate traffic.
        if b.len() == 64 && !matches!(b[0], 0x10 | 0x12 | 0x13 | 0x14) {
            return one(DecodedRecord::new(
                0,
                b.len(),
                None,
                Category::Unknown,
                DecodedData::Unknown {
                    record_type: Some(b[0]),
                },
            ));
        }
        return match protocol::parse_hid(b) {
            Ok(parsed) => one(DecodedRecord::new(
                0,
                b.len(),
                Some(parsed.slot()),
                Category::Sensor,
                DecodedData::Raw(parsed),
            )),
            Err(reason) => {
                let tracker = (b.len() == 64).then(|| b[1]);
                one(DecodedRecord::new(
                    0,
                    b.len(),
                    tracker,
                    Category::Malformed,
                    DecodedData::Malformed {
                        reason: MalformedReason::Raw(reason),
                    },
                ))
            }
        };
    }
    if b.len() != 64 {
        return malformed(b.len(), MalformedReason::MainLength(b.len()));
    }
    let mut rows = [None, None, None, None];
    for (index, wire) in b.as_chunks::<16>().0.iter().enumerate() {
        if wire.iter().all(|v| *v == 0) {
            continue;
        }
        let (tracker, category, data) = decode_main(wire);
        rows[index] = Some(DecodedRecord::new(index * 16, 16, tracker, category, data));
    }
    rows
}
fn decode_main(b: &[u8; 16]) -> (Option<u8>, Category, DecodedData) {
    if let Some(sensor) = MainSensor::parse(b) {
        if matches!(b[0], 1 | 4) && b[2..10].iter().all(|v| *v == 0) {
            return (
                Some(b[1]),
                Category::Malformed,
                DecodedData::Malformed {
                    reason: MalformedReason::ZeroQuaternion,
                },
            );
        }
        return (Some(b[1]), Category::Sensor, DecodedData::Sensor(sensor));
    }
    if let Some(event) = TrackerEvent::parse(b) {
        return (
            Some(event.tracker_id),
            Category::Event,
            DecodedData::Event(event),
        );
    }
    match b[0] {
        255 => (
            Some(b[1]),
            Category::Identity,
            DecodedData::Identity {
                address_wire: b[2..8].try_into().unwrap(),
            },
        ),
        251 => {
            let ack = CommandAck::parse(b);
            let tracker = if let Some(WomResult::Tracker { tracker_id, .. }) = ack.wom {
                Some(tracker_id)
            } else {
                None
            };
            (tracker, Category::Command, DecodedData::Ack(ack))
        }
        254 => {
            let command = Command::parse(b);
            (
                command.tracker_id(),
                Category::Command,
                DecodedData::Command(command),
            )
        }
        0xf0..=0xf7 => {
            let ota = Ota::inbound(b);
            let target = ota.target();
            (
                (target < 0xfe).then_some(target),
                Category::Ota,
                DecodedData::Ota(ota),
            )
        }
        0xf8 => (None, Category::Unknown, DecodedData::Padding),
        _ => (
            None,
            Category::Unknown,
            DecodedData::Unknown {
                record_type: Some(b[0]),
            },
        ),
    }
}
