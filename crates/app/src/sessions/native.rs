use super::IO_TIMEOUT;
#[cfg(windows)]
use super::WRITE_DEADLINE;
use crate::i18n::tr;
use serialport::SerialPort;
use std::io::{self, Read, Write};
use std::sync::Arc;

pub(super) trait Transport: Read + Write + Send {}
impl<T: Read + Write + Send> Transport for T {}
pub(super) type Factory = Arc<dyn Fn(&str) -> io::Result<Box<dyn Transport>> + Send + Sync>;

pub(super) fn open(port: &str) -> io::Result<Box<dyn Transport>> {
    let mut serial = serialport::new(port, 115_200)
        .data_bits(serialport::DataBits::Eight)
        .parity(serialport::Parity::None)
        .stop_bits(serialport::StopBits::One)
        .flow_control(serialport::FlowControl::None)
        .timeout(IO_TIMEOUT)
        .open_native()
        .map_err(|error| serial_open_error(port, &error))?;
    #[cfg(windows)]
    configure_windows_write_timeout(&mut serial, port)?;
    // Match pyserial's default line levels; Windows native open leaves
    // both low, and CDC firmware can withhold output until DTR is high.
    serial.write_data_terminal_ready(true).map_err(|error| {
        io::Error::other(
            rust_i18n::t!(
                "serial.transport.error.assert_dtr",
                port = port,
                error = error
            )
            .to_string(),
        )
    })?;
    serial.write_request_to_send(true).map_err(|error| {
        io::Error::other(
            rust_i18n::t!(
                "serial.transport.error.assert_rts",
                port = port,
                error = error
            )
            .to_string(),
        )
    })?;
    Ok(Box::new(serial) as Box<dyn Transport>)
}

#[cfg(windows)]
fn configure_windows_write_timeout(serial: &mut serialport::COMPort, port: &str) -> io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Devices::Communication::{
        COMMTIMEOUTS, GetCommTimeouts, SetCommTimeouts,
    };

    let handle = serial.as_raw_handle();
    let mut timeouts = COMMTIMEOUTS::default();
    // SAFETY: the exclusively borrowed COMPort owns a live handle for both calls,
    // and the timeout structure remains valid throughout each synchronous call.
    if unsafe { GetCommTimeouts(handle, &mut timeouts) } == 0 {
        let error = io::Error::last_os_error();
        return Err(io::Error::new(
            error.kind(),
            rust_i18n::t!(
                "serial.transport.error.read_timeouts",
                port = port,
                error = error
            )
            .to_string(),
        ));
    }
    // serialport::set_timeout couples reads and writes. Keep its responsive
    // 30 ms read fields, but give native writes the command's existing budget.
    // Configure once before Opened; calling set_timeout later would undo this.
    timeouts.WriteTotalTimeoutMultiplier = 0;
    timeouts.WriteTotalTimeoutConstant = WRITE_DEADLINE.as_millis() as u32;
    // SAFETY: same live handle and initialized timeout structure as above.
    if unsafe { SetCommTimeouts(handle, &timeouts) } == 0 {
        let error = io::Error::last_os_error();
        return Err(io::Error::new(
            error.kind(),
            rust_i18n::t!(
                "serial.transport.error.write_timeout",
                port = port,
                error = error
            )
            .to_string(),
        ));
    }
    Ok(())
}

pub(super) fn serial_open_error(port: &str, error: &serialport::Error) -> io::Error {
    let guidance = match error.kind() {
        serialport::ErrorKind::Io(io::ErrorKind::PermissionDenied) => {
            tr("serial.transport.guidance.permission_denied")
        }
        // On Windows, NoDevice also covers access denied and missing paths.
        // The error kind cannot establish that another application owns the port.
        serialport::ErrorKind::NoDevice | serialport::ErrorKind::Unknown => {
            tr("serial.transport.guidance.check_connection")
        }
        _ => String::new(),
    };
    io::Error::other(
        rust_i18n::t!(
            "serial.transport.error.open",
            port = port,
            error = error,
            guidance = guidance
        )
        .to_string(),
    )
}
