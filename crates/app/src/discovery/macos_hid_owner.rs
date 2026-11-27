//! Read-only, best-effort attribution for a failed Darwin HID open.
use super::macos_serial::Object;
use core_foundation::base::{CFType, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use io_kit_sys::hid::keys::kIOHIDOptionsTypeSeizeDevice;
use io_kit_sys::{IORegistryEntryIDMatching, IOServiceGetMatchingService, kIOMasterPortDefault};
use std::ffi::CStr;

pub(super) fn exclusive_client(path: &CStr) -> Option<String> {
    // hidapi's DevSrvsID is the registry ID of the HID service, not the USB device.
    let id = path
        .to_str()
        .ok()?
        .strip_prefix("DevSrvsID:")?
        .parse()
        .ok()?;
    // SAFETY: the matching call consumes the dictionary; Object owns the returned service.
    let service = unsafe {
        let matching = IORegistryEntryIDMatching(id);
        if matching.is_null() {
            return None;
        }
        Object::new(IOServiceGetMatchingService(kIOMasterPortDefault, matching))?
    };
    let mut clients = service.children()?;
    let mut owner = None;
    for client in clients.by_ref() {
        if !client.conforms_to(c"IOHIDLibUserClient") {
            continue;
        }
        let state = client
            .property("DebugState")
            .and_then(CFType::downcast_into::<CFDictionary>);
        if state.as_ref().is_some_and(holds_exclusive_access) {
            if owner.is_some() {
                return None;
            }
            owner = Some(client.string("IOUserClientCreator")?);
        }
    }
    if clients.is_valid() { owner } else { None }
}

fn holds_exclusive_access(state: &CFDictionary) -> bool {
    let field = |key| {
        let value = state.find(CFString::from_static_string(key).as_CFTypeRef())?;
        // SAFETY: IOKit serializes registry properties as CF values; state retains the value.
        Some(unsafe { CFType::wrap_under_get_rule(*value) })
    };
    let flag = |key| field(key)?.downcast_into::<CFBoolean>().map(bool::from);
    // ClientSeized marks a *blocked* client. Its requested options alone do not prove a grant.
    flag("ClientOpened") == Some(true)
        && flag("ClientSeized") == Some(false)
        && field("ClientOptions")
            .and_then(CFType::downcast_into::<CFNumber>)
            .and_then(|number| number.to_i64())
            .and_then(|bits| u32::try_from(bits).ok())
            .is_some_and(|bits| bits & kIOHIDOptionsTypeSeizeDevice != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exclusive_owner_is_not_a_shared_closed_or_blocked_client() {
        for (opened, seized, options, expected) in [
            (true, false, 1, true),
            (true, false, 0, false),
            (true, true, 1, false),
            (false, false, 1, false),
        ] {
            let state = CFDictionary::from_CFType_pairs(&[
                (
                    CFString::from_static_string("ClientOpened"),
                    CFBoolean::from(opened).as_CFType(),
                ),
                (
                    CFString::from_static_string("ClientSeized"),
                    CFBoolean::from(seized).as_CFType(),
                ),
                (
                    CFString::from_static_string("ClientOptions"),
                    CFNumber::from(options).as_CFType(),
                ),
            ])
            .into_untyped();
            assert_eq!(holds_exclusive_access(&state), expected);
        }
    }
}
