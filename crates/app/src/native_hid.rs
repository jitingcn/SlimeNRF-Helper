//! Native HID inventory and opens; readers own their handles independently.
use std::ffi::CStr;

use hidapi::{HidApi, HidDevice, HidResult};

// The Darwin C backend shares an unsynchronized error buffer between
// enumeration and opens. Keep those calls serialized, not device I/O.
#[cfg(target_os = "macos")]
static DARWIN_API: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

pub(crate) fn enumerate() -> HidResult<HidApi> {
    #[cfg(target_os = "macos")]
    let _guard = DARWIN_API.lock();
    HidApi::new()
}

pub(crate) fn open_path(api: &HidApi, path: &CStr) -> HidResult<HidDevice> {
    #[cfg(target_os = "macos")]
    let _guard = DARWIN_API.lock();
    api.open_path(path)
}
