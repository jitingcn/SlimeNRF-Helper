//! Darwin serial callout and USB function discovery through IOKit.
use std::collections::HashMap;
use std::ffi::CStr;
use std::num::NonZeroU32;
use std::ptr;

use core_foundation::base::{CFType, TCFType};
use core_foundation::data::{CFData, CFDataRef};
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use io_kit_sys::keys::kIOServicePlane;
use io_kit_sys::ret::kIOReturnSuccess;
use io_kit_sys::serial::keys::kIOSerialBSDServiceValue;
use io_kit_sys::types::io_object_t;
use io_kit_sys::{
    IOIteratorIsValid, IOIteratorNext, IOObjectConformsTo, IOObjectRelease,
    IORegistryEntryCreateCFProperty, IORegistryEntryGetChildIterator,
    IORegistryEntryGetParentEntry, IORegistryEntryGetRegistryEntryID, IOServiceGetMatchingServices,
    IOServiceMatching, kIOMasterPortDefault,
};

use super::{RECEIVER_PID, RECEIVER_VID, macos_usb_cdc, serial_label};
use crate::domain::{InterfaceDescription, SerialEndpoint, SerialPortKind, UsbInterface};
use crate::i18n::tr;

unsafe extern "C" {
    fn slimenrf_copy_usb_configuration(service: io_object_t, configuration: u8) -> CFDataRef;
}

type Configurations = HashMap<(u64, u8), Option<CFData>>;

pub(super) fn endpoints() -> Result<Vec<SerialEndpoint>, String> {
    let mut iterator = 0;
    // SAFETY: the service class is a static C string, and the lookup consumes
    // the returned matching dictionary. `iterator` is valid output storage.
    let result = unsafe {
        let matching = IOServiceMatching(kIOSerialBSDServiceValue);
        if matching.is_null() {
            return Err("Cannot create IOKit serial service match".into());
        }
        IOServiceGetMatchingServices(kIOMasterPortDefault, matching, &mut iterator)
    };
    let mut services = Objects::new(iterator)
        .filter(|_| result == kIOReturnSuccess)
        .ok_or_else(|| format!("Cannot enumerate IOKit serial services ({result:#010x})"))?;
    let mut configurations = Configurations::new();
    let mut endpoints = (&mut services)
        .filter_map(|service| endpoint(&service, &mut configurations))
        .collect::<Vec<_>>();
    if !services.is_valid() {
        return Err("IOKit serial services changed during enumeration".into());
    }
    endpoints.sort_by(|left, right| left.port_name.cmp(&right.port_name));
    Ok(endpoints)
}

fn endpoint(service: &Object, configurations: &mut Configurations) -> Option<SerialEndpoint> {
    // The dial-in `/dev/tty.*` name addresses the same serial service. Use
    // IOCalloutDevice only; two aliases must not become two reservable ports.
    let port_name = service.string("IOCalloutDevice")?;
    if !port_name.starts_with("/dev/cu.") {
        return None;
    }
    let mut endpoint = SerialEndpoint {
        label: String::new(),
        port_name,
        manufacturer: None,
        product: None,
        kind: SerialPortKind::Unknown,
        serial_number: None,
        vid: None,
        pid: None,
        usb_interface: None,
    };
    let mut interface = None;
    for parent in std::iter::successors(service.parent(), Object::parent) {
        if interface.is_none() && parent.is_usb_interface() {
            interface = Some(parent);
        } else if parent.conforms_to(c"IOUSBHostDevice") || parent.conforms_to(c"IOUSBDevice") {
            endpoint.vid = parent.number("idVendor");
            endpoint.pid = parent.number("idProduct");
            let (Some(vid), Some(pid)) = (endpoint.vid, endpoint.pid) else {
                return None;
            };
            endpoint.kind = SerialPortKind::Usb;
            endpoint.manufacturer = parent
                .string("kUSBVendorString")
                .or_else(|| parent.string("USB Vendor Name"))
                .map(String::into_boxed_str);
            endpoint.product = parent
                .string("kUSBProductString")
                .or_else(|| parent.string("USB Product Name"))
                .map(String::into_boxed_str);
            endpoint.serial_number = parent
                .string("kUSBSerialNumberString")
                .or_else(|| parent.string("USB Serial Number"));
            let number = interface
                .as_ref()
                .and_then(|interface| interface.number("bInterfaceNumber"));
            let description = if (vid, pid) == (RECEIVER_VID, RECEIVER_PID) {
                interface
                    .as_ref()
                    .zip(number)
                    .and_then(|(interface, number)| {
                        receiver_description(&parent, interface, number, configurations)
                    })
                    .unwrap_or(InterfaceDescription::Ambiguous)
            } else {
                interface.as_ref().and_then(interface_name).map_or(
                    InterfaceDescription::Unavailable,
                    InterfaceDescription::Value,
                )
            };
            endpoint.usb_interface = Some(UsbInterface {
                number,
                description,
            });
            break;
        } else if parent.conforms_to(c"IOBluetoothSerialClient") {
            endpoint.kind = SerialPortKind::Bluetooth;
            break;
        } else if parent.conforms_to(c"IOPCIDevice") {
            endpoint.kind = SerialPortKind::Pci;
            break;
        }
    }
    // An interface without its USB device is an incomplete attachment, not a
    // generic serial endpoint with less restrictive admission.
    if interface.is_some() && endpoint.kind != SerialPortKind::Usb {
        return None;
    }
    let kind_label = match endpoint.kind {
        SerialPortKind::Bluetooth => Some(tr("devices.serial_kind.bluetooth")),
        SerialPortKind::Pci => Some(tr("devices.serial_kind.pci")),
        _ => None,
    };
    endpoint.label = serial_label(
        &endpoint.port_name,
        endpoint
            .product
            .as_deref()
            .or(endpoint.manufacturer.as_deref())
            .or(kind_label.as_deref()),
    );
    Some(endpoint)
}

fn receiver_description(
    device: &Object,
    interface: &Object,
    number: u8,
    configurations: &mut Configurations,
) -> Option<InterfaceDescription> {
    match interface.number::<u8>("bInterfaceClass")? {
        0x02 if interface.number("bInterfaceSubClass") == Some(0x02_u8) => {
            described_interface(interface, interface.number("iInterface")?)
        }
        0x0a => {
            let configuration = interface.number("bConfigurationValue")?;
            let key = (device.id()?, configuration);
            let cached = configurations
                .entry(key)
                .or_insert_with(|| device.configuration(configuration))
                .as_ref()?;
            let control = macos_usb_cdc::control_interface(cached.bytes(), number)?;
            let mut children = device.children()?;
            let mut siblings = children.by_ref().filter(|sibling| {
                sibling.is_usb_interface()
                    && sibling.number("bConfigurationValue") == Some(configuration)
                    && sibling.number("bInterfaceNumber") == Some(control.number)
                    && sibling.number("bAlternateSetting") == Some(0_u8)
            });
            let sibling = siblings.next()?;
            if siblings.next().is_some()
                || !children.is_valid()
                || sibling.number("iInterface") != Some(control.string_index)
                || sibling.number("bInterfaceClass") != Some(0x02_u8)
                || sibling.number("bInterfaceSubClass") != Some(0x02_u8)
            {
                return None;
            }
            described_interface(&sibling, control.string_index)
        }
        _ => None,
    }
}

fn described_interface(interface: &Object, string_index: u8) -> Option<InterfaceDescription> {
    if string_index == 0 {
        Some(InterfaceDescription::Unavailable)
    } else {
        interface_name(interface).map(InterfaceDescription::Value)
    }
}

fn interface_name(interface: &Object) -> Option<Box<str>> {
    interface
        .string("kUSBString")
        .or_else(|| interface.string("USB Interface Name"))
        .map(String::into_boxed_str)
}

// IOKit returns owned send rights for each iterator, service, and parent entry.
pub(super) struct Object(NonZeroU32);

impl Object {
    pub(super) fn new(raw: io_object_t) -> Option<Self> {
        NonZeroU32::new(raw).map(Self)
    }

    fn raw(&self) -> io_object_t {
        self.0.get()
    }

    fn id(&self) -> Option<u64> {
        let mut id = 0;
        // SAFETY: `self` retains the registry entry and `id` is writable.
        (unsafe { IORegistryEntryGetRegistryEntryID(self.raw(), &mut id) } == kIOReturnSuccess)
            .then_some(id)
    }

    pub(super) fn conforms_to(&self, class: &CStr) -> bool {
        // SAFETY: `self` retains the object and the C API only reads `class`.
        unsafe { IOObjectConformsTo(self.raw(), class.as_ptr().cast_mut()) != 0 }
    }

    fn is_usb_interface(&self) -> bool {
        self.conforms_to(c"IOUSBHostInterface") || self.conforms_to(c"IOUSBInterface")
    }

    fn parent(&self) -> Option<Self> {
        let mut parent = 0;
        // SAFETY: both the entry and static plane are valid; IOKit returns an
        // owned parent right on success.
        let result =
            unsafe { IORegistryEntryGetParentEntry(self.raw(), kIOServicePlane, &mut parent) };
        if result != kIOReturnSuccess {
            return None;
        }
        Self::new(parent)
    }

    pub(super) fn children(&self) -> Option<Objects> {
        let mut iterator = 0;
        // SAFETY: the object is live, the SDK treats the plane as const, and the
        // returned owned iterator writes to initialized storage.
        let result = unsafe {
            IORegistryEntryGetChildIterator(self.raw(), kIOServicePlane.cast_mut(), &mut iterator)
        };
        if result != kIOReturnSuccess {
            return None;
        }
        Objects::new(iterator)
    }

    pub(super) fn property(&self, key: &'static str) -> Option<CFType> {
        let key = CFString::from_static_string(key);
        // SAFETY: `self` owns a live right and `key` stays valid for this call;
        // a non-null returned CF object follows the Create Rule.
        unsafe {
            let value = IORegistryEntryCreateCFProperty(
                self.raw(),
                key.as_concrete_TypeRef(),
                ptr::null(),
                0,
            );
            (!value.is_null()).then(|| CFType::wrap_under_create_rule(value))
        }
    }

    pub(super) fn string(&self, key: &'static str) -> Option<String> {
        let value = self.property(key)?.downcast_into::<CFString>()?.to_string();
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else if trimmed.len() == value.len() {
            Some(value)
        } else {
            Some(trimmed.to_owned())
        }
    }

    fn number<T: TryFrom<i64>>(&self, key: &'static str) -> Option<T> {
        T::try_from(self.property(key)?.downcast_into::<CFNumber>()?.to_i64()?).ok()
    }

    fn configuration(&self, value: u8) -> Option<CFData> {
        // SAFETY: the IOKit USB device entry is retained by `self`. The shim
        // returns either null or an independently owned Core Foundation copy.
        unsafe {
            let data = slimenrf_copy_usb_configuration(self.raw(), value);
            (!data.is_null()).then(|| CFData::wrap_under_create_rule(data))
        }
    }
}

impl Drop for Object {
    fn drop(&mut self) {
        // SAFETY: each Object owns exactly one IOKit right.
        unsafe { IOObjectRelease(self.raw()) };
    }
}

pub(super) struct Objects(Object);

impl Objects {
    fn new(raw: io_object_t) -> Option<Self> {
        Object::new(raw).map(Self)
    }

    pub(super) fn is_valid(&self) -> bool {
        // SAFETY: `self` owns an iterator obtained from IOKit.
        unsafe { IOIteratorIsValid(self.0.raw()) != 0 }
    }
}

impl Iterator for Objects {
    type Item = Object;

    fn next(&mut self) -> Option<Self::Item> {
        // SAFETY: the iterator is live and a nonzero result is a retained entry.
        Object::new(unsafe { IOIteratorNext(self.0.raw()) })
    }
}
