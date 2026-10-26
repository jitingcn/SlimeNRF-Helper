use std::borrow::Cow;
use std::collections::BTreeMap;
use std::mem::size_of;
use std::ptr;

use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    DICS_FLAG_GLOBAL, DIGCF_PRESENT, DIREG_DEV, GUID_DEVCLASS_PORTS, HDEVINFO, SP_DEVINFO_DATA,
    SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo, SetupDiGetClassDevsW,
    SetupDiGetDevicePropertyW, SetupDiOpenDevRegKey,
};
use windows_sys::Win32::Devices::Properties::{
    DEVPKEY_Device_BusReportedDeviceDesc, DEVPROP_TYPE_STRING,
};
use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
use windows_sys::Win32::System::Registry::{
    HKEY, KEY_QUERY_VALUE, REG_SZ, RegCloseKey, RegQueryValueExW,
};

use crate::domain::InterfaceDescription;

// Keep each background scan bounded; no names survive a hotplug between scans.
const MAX_PORT_DEVNODES: u32 = 4096;
const STRING_CAPACITY: usize = 1024;

struct DeviceInfoSet(HDEVINFO);

impl Drop for DeviceInfoSet {
    fn drop(&mut self) {
        // SAFETY: the set is valid and uniquely owned until this drop.
        unsafe { SetupDiDestroyDeviceInfoList(self.0) };
    }
}

struct DeviceRegistryKey(HKEY);

impl Drop for DeviceRegistryKey {
    fn drop(&mut self) {
        // SAFETY: the key is valid and uniquely owned until this drop.
        unsafe { RegCloseKey(self.0) };
    }
}

#[derive(Default)]
pub(super) struct PortNames(BTreeMap<String, InterfaceDescription>);

impl PortNames {
    /// One native enumeration for the whole scan, without opening any COM port.
    /// Read the property on the actual serial devnode, not a VID/PID-selected parent.
    pub(super) fn scan() -> Self {
        let mut names = Self::default();
        // SAFETY: the class GUID is valid, optional inputs are null, and only
        // present devices are requested. The returned set is owned below.
        let handle = unsafe {
            SetupDiGetClassDevsW(
                &GUID_DEVCLASS_PORTS,
                ptr::null(),
                ptr::null_mut(),
                DIGCF_PRESENT,
            )
        };
        if handle == INVALID_HANDLE_VALUE as HDEVINFO {
            return names;
        }
        let devices = DeviceInfoSet(handle);
        for index in 0..MAX_PORT_DEVNODES {
            let mut device = SP_DEVINFO_DATA {
                cbSize: size_of::<SP_DEVINFO_DATA>() as u32,
                ..Default::default()
            };
            // SAFETY: the set is live and the output structure has its required size.
            if unsafe { SetupDiEnumDeviceInfo(devices.0, index, &mut device) } == 0 {
                break;
            }
            if let Some(port) = port_name(&devices, &device) {
                names.record(&port, bus_description(&devices, &device));
            }
        }
        names
    }

    fn record(&mut self, port: &str, description: Option<String>) {
        let Some(key) = canonical_port(port) else {
            return;
        };
        // Multiple present devnodes claiming one port do not identify its name.
        self.0
            .entry(key.into_owned())
            .and_modify(|value| *value = InterfaceDescription::Ambiguous)
            .or_insert_with(|| {
                description
                    .filter(|value| !value.trim().is_empty())
                    .map(|value| InterfaceDescription::Value(value.into_boxed_str()))
                    .unwrap_or(InterfaceDescription::Unavailable)
            });
    }

    pub(super) fn description(&self, port: &str) -> Option<&str> {
        match self.0.get(canonical_port(port)?.as_ref())? {
            InterfaceDescription::Value(value) if informative_description(value) => Some(value),
            _ => None,
        }
    }

    pub(super) fn interface_description(&self, port: &str) -> InterfaceDescription {
        canonical_port(port)
            .and_then(|port| self.0.get(port.as_ref()))
            .cloned()
            .unwrap_or(InterfaceDescription::Unavailable)
    }
}

fn canonical_port(port: &str) -> Option<Cow<'_, str>> {
    let port = port.trim();
    let port = port.strip_prefix(r"\\.\").unwrap_or(port);
    let (prefix, number) = port.split_at_checked(3)?;
    if !prefix.eq_ignore_ascii_case("COM")
        || number.is_empty()
        || !number.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    Some(if prefix == "COM" {
        Cow::Borrowed(port)
    } else {
        Cow::Owned(port.to_ascii_uppercase())
    })
}

fn informative_description(description: &str) -> bool {
    let description = description.trim();
    !description.is_empty()
        && ![
            "CDC",
            "USB CDC",
            "CDC ACM",
            "USB Serial",
            "USB Serial Device",
            "USB Serial Port",
            "Serial Port",
            "USB Device",
            "USB Composite Device",
            "USB 串行设备",
            "USB串行设备",
            "USB 串行裝置",
            "USB 序列裝置",
        ]
        .iter()
        .any(|generic| description.eq_ignore_ascii_case(generic))
}

fn decode_string(buffer: &[u16], byte_length: u32) -> Option<String> {
    let byte_length = byte_length as usize;
    if byte_length == 0 || !byte_length.is_multiple_of(2) || byte_length / 2 > buffer.len() {
        return None;
    }
    let value = &buffer[..byte_length / 2];
    let terminator = value.iter().position(|unit| *unit == 0)?;
    String::from_utf16(&value[..terminator]).ok()
}

fn port_name(devices: &DeviceInfoSet, device: &SP_DEVINFO_DATA) -> Option<String> {
    // SAFETY: the live devnode belongs to this live set; only registry query
    // access is requested. No serial device handle or firmware command is used.
    let handle = unsafe {
        SetupDiOpenDevRegKey(
            devices.0,
            device,
            DICS_FLAG_GLOBAL,
            0,
            DIREG_DEV,
            KEY_QUERY_VALUE,
        )
    };
    if handle == INVALID_HANDLE_VALUE || handle.is_null() {
        return None;
    }
    let key = DeviceRegistryKey(handle);
    let mut buffer = [0u16; STRING_CAPACITY];
    let mut bytes = size_of::<[u16; STRING_CAPACITY]>() as u32;
    let mut value_type = 0;
    // SAFETY: all outputs point to initialized writable storage; bytes gives
    // the exact buffer capacity. Oversized/malformed values are rejected below.
    let result = unsafe {
        RegQueryValueExW(
            key.0,
            windows_sys::core::w!("PortName"),
            ptr::null(),
            &mut value_type,
            buffer.as_mut_ptr().cast(),
            &mut bytes,
        )
    };
    if result != 0 || value_type != REG_SZ {
        return None;
    }
    decode_string(&buffer, bytes)
}

fn bus_description(devices: &DeviceInfoSet, device: &SP_DEVINFO_DATA) -> Option<String> {
    let mut buffer = [0u16; STRING_CAPACITY];
    let mut bytes = 0;
    let mut value_type = 0;
    // SAFETY: the property key and devnode are valid, and the supplied byte
    // capacity matches the writable UTF-16 buffer. Failure retains the existing
    // serialport descriptor, including properties too large for this bound.
    let result = unsafe {
        SetupDiGetDevicePropertyW(
            devices.0,
            device,
            &DEVPKEY_Device_BusReportedDeviceDesc,
            &mut value_type,
            buffer.as_mut_ptr().cast(),
            size_of::<[u16; STRING_CAPACITY]>() as u32,
            &mut bytes,
            0,
        )
    };
    if result == 0 || value_type != DEVPROP_TYPE_STRING {
        return None;
    }
    decode_string(&buffer, bytes)
}

#[cfg(test)]
mod tests {
    use super::{PortNames, decode_string, informative_description};
    use crate::domain::InterfaceDescription;

    #[test]
    fn actual_port_mapping_prefers_informative_bus_names_without_role_inference() {
        let mut names = PortNames::default();
        names.record(
            "COM14",
            Some("SlimeNRF Tracker SK CheeseCake NRF P10".into()),
        );
        names.record("COM30", Some("SlimeNRF Receiver Styria R1".into()));
        names.record(
            "COM5",
            Some("SlimeNRF Receiver SK Cheesecake RECV33".into()),
        );
        assert_eq!(
            names.description(r"\\.\com14"),
            Some("SlimeNRF Tracker SK CheeseCake NRF P10")
        );
        assert_eq!(
            names.description("COM30"),
            Some("SlimeNRF Receiver Styria R1")
        );
        assert_eq!(
            names.description("COM5"),
            Some("SlimeNRF Receiver SK Cheesecake RECV33")
        );
        assert_eq!(names.description("COM15"), None);
    }

    #[test]
    fn generic_missing_and_ambiguous_names_preserve_existing_descriptors() {
        let mut names = PortNames::default();
        names.record("COM13", Some(" CDC ".into()));
        names.record("COM2", None);
        names.record("COM3", Some("First device".into()));
        names.record("com3", Some("Second device".into()));
        names.record("LPT1", Some("Parallel device".into()));
        assert_eq!(
            names.description("COM13").or(Some("J-Link")),
            Some("J-Link")
        );
        assert_eq!(
            names.description("COM2").or(Some("Existing")),
            Some("Existing")
        );
        assert_eq!(names.description("COM3"), None);
        assert_eq!(names.description("LPT1"), None);
        assert_eq!(
            names.interface_description("COM13"),
            InterfaceDescription::Value(" CDC ".into())
        );
        assert_eq!(
            names.interface_description("COM2"),
            InterfaceDescription::Unavailable
        );
        assert_eq!(
            names.interface_description("COM3"),
            InterfaceDescription::Ambiguous
        );
        assert_eq!(
            names.interface_description("COM99"),
            InterfaceDescription::Unavailable
        );
    }

    #[test]
    fn generic_filter_does_not_discard_descriptive_product_names() {
        for generic in ["", "  ", "cdc", "USB Serial Device", "USB 串行设备"] {
            assert!(!informative_description(generic));
        }
        assert!(informative_description("J-Link CDC"));
        assert!(informative_description("SlimeNRF Tracker"));
    }

    #[test]
    fn native_raw_description_survives_alias_mapping_and_duplicate_devnodes_reject_it() {
        let mut names = PortNames::default();
        names.record(r"\\.\com4", Some("SlimeNRF Raw Data CDC".into()));
        assert_eq!(
            names.interface_description("COM4"),
            InterfaceDescription::Value("SlimeNRF Raw Data CDC".into())
        );
        names.record("COM4", Some("SlimeNRF Raw Data CDC".into()));
        assert_eq!(
            names.interface_description("COM4"),
            InterfaceDescription::Ambiguous
        );
        assert_eq!(names.description("COM4"), None);
    }

    #[test]
    fn native_strings_require_bounded_terminated_utf16() {
        assert_eq!(decode_string(&[65, 0, 66], 4).as_deref(), Some("A"));
        assert_eq!(decode_string(&[65, 0], 5), None);
        assert_eq!(decode_string(&[65, 0], 6), None);
        assert_eq!(decode_string(&[65, 66], 4), None);
        assert_eq!(decode_string(&[0xd800, 0], 4), None);
        assert_eq!(decode_string(&[0], 0), None);
    }
}
