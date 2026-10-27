use crate::domain::ReceiverEndpoint;
use serde::{Deserialize, Serialize};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Interface {
    Main,
    Raw,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    In,
    Out,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    pub receiver_key: String,
    pub label: String,
    pub serial_number: Option<String>,
    pub path: Vec<u8>,
    pub interface: Interface,
    pub vendor_id: u16,
    pub product_id: u16,
    pub usage_page: u16,
    pub connection_id: u64,
}
static CONNECTION_ID: AtomicU64 = AtomicU64::new(1);
impl Source {
    pub fn main(receiver: &ReceiverEndpoint) -> Self {
        Self::interface(receiver, &receiver.path, Interface::Main, 1)
    }
    pub fn raw(receiver: &ReceiverEndpoint, path: &[u8]) -> Self {
        Self::interface(receiver, path, Interface::Raw, 0xff00)
    }
    fn interface(
        receiver: &ReceiverEndpoint,
        path: &[u8],
        interface: Interface,
        usage_page: u16,
    ) -> Self {
        Self {
            receiver_key: receiver.key.clone(),
            label: receiver.label.clone(),
            serial_number: receiver.serial_number.clone(),
            path: path.to_vec(),
            interface,
            vendor_id: 0x1209,
            product_id: 0x7690,
            usage_page,
            connection_id: CONNECTION_ID.fetch_add(1, Ordering::Relaxed),
        }
    }
}

/// Native HID buffers are at most 65 bytes, including the OUT report-ID prefix.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReportBytes {
    bytes: [u8; 65],
    len: u8,
}
impl ReportBytes {
    pub fn new(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > 65 {
            return Err(format!("HID report exceeds 65 bytes: {}", bytes.len()));
        }
        let mut report = Self {
            bytes: [0; 65],
            len: bytes.len() as u8,
        };
        report.bytes[..bytes.len()].copy_from_slice(bytes);
        Ok(report)
    }
    pub fn as_slice(&self) -> &[u8] {
        &self.bytes[..self.len as usize]
    }
}
#[derive(Clone, Debug)]
pub enum RecordData {
    Open,
    Close,
    Error {
        message: String,
    },
    Report {
        direction: Direction,
        bytes: ReportBytes,
        transferred: Option<usize>,
        error: Option<String>,
    },
}
#[derive(Clone, Debug)]
pub struct Record {
    pub sequence: u64,
    pub elapsed_us: u64,
    pub source: Arc<Source>,
    pub data: RecordData,
}
#[derive(Clone, Debug)]
pub enum TraceItem {
    Record(Arc<Record>),
    Gap { count: u64 },
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Summary {
    pub records: u64,
    pub reports: u64,
    pub bytes: u64,
    pub dropped: u64,
    pub sources: u64,
}
