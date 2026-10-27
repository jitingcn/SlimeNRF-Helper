//! Original native HID traffic: bounded subscribers and independently owned raw-file writers.
pub mod decode;
mod format;
mod hub;
mod io;
mod model;
pub use hub::{Subscription, publish, subscribe};
pub use io::{Recorder, save_snapshot};
pub use model::{
    Direction, Interface, Record, RecordData, ReportBytes, Source, Summary, TraceItem,
};
use std::sync::Arc;

/// Native and host diagnostic producers share this path. Raw framing is untouched.
pub fn publish_report(
    source: &Arc<Source>,
    direction: Direction,
    bytes: &[u8],
    transferred: Option<usize>,
    error: Option<String>,
) -> Result<(), String> {
    if !hub::global().enabled() {
        return Ok(());
    }
    let bytes = ReportBytes::new(bytes)?;
    publish(
        source,
        RecordData::Report {
            direction,
            bytes,
            transferred,
            error,
        },
    );
    Ok(())
}

/// Wraps the existing handle; never creates a reader or changes transport outcomes.
pub struct TracedHid {
    device: hidapi::HidDevice,
    source: Arc<Source>,
}
impl TracedHid {
    pub fn new(device: hidapi::HidDevice, source: Source) -> Self {
        let source = Arc::new(source);
        publish(&source, RecordData::Open);
        Self { device, source }
    }
    pub fn read_timeout(&self, bytes: &mut [u8], timeout_ms: i32) -> hidapi::HidResult<usize> {
        let result = self.device.read_timeout(bytes, timeout_ms);
        if hub::global().enabled() {
            match &result {
                Ok(0) => {}
                Ok(n) => self.report(Direction::In, &bytes[..*n], Some(*n), None),
                Err(e) => publish(
                    &self.source,
                    RecordData::Error {
                        message: format!("HID read: {e}"),
                    },
                ),
            }
        }
        result
    }
    pub fn write(&self, bytes: &[u8]) -> hidapi::HidResult<usize> {
        let result = self.device.write(bytes);
        if hub::global().enabled() {
            match &result {
                Ok(n) => self.report(Direction::Out, bytes, Some(*n), None),
                Err(e) => self.report(Direction::Out, bytes, None, Some(e.to_string())),
            }
        }
        result
    }
    fn report(
        &self,
        direction: Direction,
        bytes: &[u8],
        transferred: Option<usize>,
        error: Option<String>,
    ) {
        if let Err(message) = publish_report(&self.source, direction, bytes, transferred, error) {
            publish(&self.source, RecordData::Error { message });
        }
    }
}
impl Drop for TracedHid {
    fn drop(&mut self) {
        publish(&self.source, RecordData::Close);
    }
}
