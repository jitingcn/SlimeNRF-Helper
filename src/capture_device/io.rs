//! The sole native control/data handles and injectable read/write clock seam.
use super::{CaptureSource, CaptureTransport, discover_sources};
use crate::hid_log::{Source, TracedHid};
use hidapi::{HidApi, HidDevice};
use std::ffi::CString;
use std::io::Read;
use std::time::{Duration, Instant};
// This seam injects actual reads/writes/time, not a separate pretend recording path.
pub(super) trait CaptureIo {
    fn now(&self) -> Duration;
    fn read_control(&mut self, report: &mut [u8; 65], timeout_ms: i32) -> Result<usize, String>;
    fn write_control(&mut self, report: &[u8; 65]) -> Result<(), String>;
    fn read_data(&mut self, bytes: &mut [u8; 4096]) -> Result<usize, String>;
    fn cdc(&self) -> bool;
    fn validate_source(&mut self, source: &CaptureSource) -> Result<(), String>;
}
enum DataIo {
    Hid(TracedHid),
    Serial(Box<dyn serialport::SerialPort>),
}
pub(super) struct NativeIo {
    control: TracedHid,
    data: DataIo,
    epoch: Instant,
}
impl NativeIo {
    pub(super) fn open(source: &CaptureSource) -> Result<Self, String> {
        if !discover_sources()?.iter().any(|s| source.matches(s)) {
            return Err(
                "Capture source disappeared, changed, or is no longer uniquely associated".into(),
            );
        }
        let api = HidApi::new().map_err(|e| e.to_string())?;
        let open = |path: &[u8]| -> Result<HidDevice, String> {
            let path = CString::new(path).map_err(|_| "Invalid HID path")?;
            api.open_path(&path).map_err(|e| e.to_string())
        };
        // Establish the input stream before any radio-changing command.
        let data = match &source.transport {
            CaptureTransport::RawHid { path } => DataIo::Hid(TracedHid::new(
                open(path)?,
                Source::raw(&source.receiver, path),
            )),
            CaptureTransport::Cdc { endpoint } => {
                let port_name = &endpoint.port_name;
                let mut port = serialport::new(port_name, 115200)
                    .timeout(Duration::from_millis(10))
                    .open()
                    .map_err(|e| format!("Cannot open {port_name}: {e}"))?;
                port.write_data_terminal_ready(true)
                    .map_err(|e| e.to_string())?;
                port.write_request_to_send(true)
                    .map_err(|e| e.to_string())?;
                DataIo::Serial(port)
            }
        };
        let control = TracedHid::new(open(&source.receiver.path)?, Source::main(&source.receiver));
        Ok(Self {
            control,
            data,
            epoch: Instant::now(),
        })
    }
}
impl CaptureIo for NativeIo {
    fn validate_source(&mut self, source: &CaptureSource) -> Result<(), String> {
        if discover_sources()?
            .iter()
            .any(|current| source.matches(current))
        {
            Ok(())
        } else {
            Err("Capture source changed before START".into())
        }
    }
    fn now(&self) -> Duration {
        self.epoch.elapsed()
    }
    fn read_control(&mut self, report: &mut [u8; 65], timeout_ms: i32) -> Result<usize, String> {
        self.control
            .read_timeout(report, timeout_ms)
            .map_err(|e| format!("Control read failed: {e}"))
    }
    fn write_control(&mut self, report: &[u8; 65]) -> Result<(), String> {
        match self.control.write(report) {
            Ok(65) => Ok(()),
            Ok(n) => Err(format!(
                "Partial HID control write ({n}/65); command was not retried"
            )),
            Err(e) => Err(format!("Control write failed (not retried): {e}")),
        }
    }
    fn read_data(&mut self, bytes: &mut [u8; 4096]) -> Result<usize, String> {
        match &mut self.data {
            DataIo::Hid(device) => device
                .read_timeout(&mut bytes[..65], 10)
                .map_err(|e| format!("Raw HID read failed: {e}")),
            DataIo::Serial(port) => match port.read(bytes) {
                Ok(n) => Ok(n),
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                    ) =>
                {
                    Ok(0)
                }
                Err(e) => Err(format!("CDC read failed: {e}")),
            },
        }
    }
    fn cdc(&self) -> bool {
        matches!(self.data, DataIo::Serial(_))
    }
}
