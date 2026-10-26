use super::super::native::serial_open_error;
use std::io::{self};

#[test]
fn serial_open_error_preserves_port_and_original_description_for_all_kinds() {
    for kind in [
        serialport::ErrorKind::Io(io::ErrorKind::PermissionDenied),
        serialport::ErrorKind::NoDevice,
        serialport::ErrorKind::Unknown,
        serialport::ErrorKind::InvalidInput,
        serialport::ErrorKind::Io(io::ErrorKind::TimedOut),
        serialport::ErrorKind::Io(io::ErrorKind::NotFound),
        serialport::ErrorKind::Io(io::ErrorKind::Other),
    ] {
        for description in ["拒绝访问，找不到指定的设备", "access denied or port in use"]
        {
            let error = serialport::Error::new(kind, description);
            let message = serial_open_error("COM39", &error).to_string();
            assert!(message.contains("COM39"));
            assert!(message.contains(description));
        }
    }
}
