use core_foundation::{
    ConcreteCFType,
    base::{CFAllocatorRef, CFType, CFTypeRef, TCFType},
    boolean::CFBoolean,
    dictionary::{CFDictionary, CFDictionaryGetValue, CFDictionaryRef},
    string::{CFString, CFStringRef},
    url::{CFURL, CFURLRef},
};
use io_kit_sys::{
    IOObjectConformsTo, IOObjectRelease, IORegistryEntryGetRegistryEntryID, types::io_service_t,
};
use std::{
    ffi::{CStr, CString, c_char},
    path::Path,
    ptr,
};

#[link(name = "DiskArbitration", kind = "framework")]
unsafe extern "C" {
    fn DASessionCreate(allocator: CFAllocatorRef) -> CFTypeRef;
    fn DADiskCreateFromVolumePath(
        allocator: CFAllocatorRef,
        session: CFTypeRef,
        path: CFURLRef,
    ) -> CFTypeRef;
    fn DADiskCreateFromBSDName(
        allocator: CFAllocatorRef,
        session: CFTypeRef,
        name: *const c_char,
    ) -> CFTypeRef;
    fn DADiskGetBSDName(disk: CFTypeRef) -> *const c_char;
    fn DADiskCopyDescription(disk: CFTypeRef) -> CFDictionaryRef;
    fn DADiskCopyIOMedia(disk: CFTypeRef) -> io_service_t;

    #[link_name = "kDADiskDescriptionVolumeKindKey"]
    static VOLUME_KIND: CFStringRef;
    #[link_name = "kDADiskDescriptionVolumePathKey"]
    static VOLUME_PATH: CFStringRef;
    #[link_name = "kDADiskDescriptionVolumeNameKey"]
    static VOLUME_NAME: CFStringRef;
    #[link_name = "kDADiskDescriptionDeviceProtocolKey"]
    static DEVICE_PROTOCOL: CFStringRef;
    #[link_name = "kDADiskDescriptionMediaWritableKey"]
    static MEDIA_WRITABLE: CFStringRef;
}

struct MediaService(io_service_t);

impl MediaService {
    fn registry_id(&self) -> Option<u64> {
        let mut id = 0;
        // SAFETY: the service is retained, id is writable, and IOKit only reads the class name.
        unsafe {
            (IOObjectConformsTo(self.0, c"IOMedia".as_ptr().cast_mut()) != 0
                && IORegistryEntryGetRegistryEntryID(self.0, &mut id) == 0)
                .then_some(id)
        }
    }
}

impl Drop for MediaService {
    fn drop(&mut self) {
        // SAFETY: DADiskCopyIOMedia transferred this reference to us.
        unsafe { IOObjectRelease(self.0) };
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Media {
    bsd_name: CString,
    registry_id: u64,
    pub(super) label: String,
}

pub(super) struct Session(CFType);

impl Session {
    pub(super) fn new() -> Option<Self> {
        // SAFETY: a null allocator requests the default Core Foundation allocator.
        let session = unsafe { DASessionCreate(ptr::null()) };
        // SAFETY: a non-null session is a retained Core Foundation object.
        (!session.is_null()).then(|| Self(unsafe { CFType::wrap_under_create_rule(session) }))
    }

    pub(super) fn media_at(&self, path: &Path) -> Option<Media> {
        let url = CFURL::from_path(path, true)?;
        // SAFETY: the session and the file URL are live for this call.
        let disk = unsafe {
            DADiskCreateFromVolumePath(
                ptr::null(),
                self.0.as_CFTypeRef(),
                url.as_concrete_TypeRef(),
            )
        };
        if disk.is_null() {
            return None;
        }
        // SAFETY: a non-null disk is a retained Core Foundation object.
        let disk = unsafe { CFType::wrap_under_create_rule(disk) };
        let (registry_id, label) = inspect(&disk, path)?;
        // SAFETY: a non-null BSD name is a nul-terminated string owned by `disk`.
        let name = unsafe {
            let name = DADiskGetBSDName(disk.as_CFTypeRef());
            (!name.is_null()).then(|| CStr::from_ptr(name))?
        };
        Some(Media {
            bsd_name: name.to_owned(),
            registry_id,
            label,
        })
    }

    pub(super) fn is_current(&self, path: &Path, media: &Media) -> bool {
        // SAFETY: the session is live and bsd_name is a nul-terminated name from Disk Arbitration.
        let disk = unsafe {
            DADiskCreateFromBSDName(ptr::null(), self.0.as_CFTypeRef(), media.bsd_name.as_ptr())
        };
        if disk.is_null() {
            return false;
        }
        // SAFETY: a non-null disk is a retained Core Foundation object.
        let disk = unsafe { CFType::wrap_under_create_rule(disk) };
        inspect(&disk, path).is_some_and(|(registry_id, label)| {
            registry_id == media.registry_id && label == media.label
        })
    }
}

fn inspect(disk: &CFType, path: &Path) -> Option<(u64, String)> {
    // SAFETY: the argument is a live DADiskRef from the create functions above.
    let description = unsafe { DADiskCopyDescription(disk.as_CFTypeRef()) };
    if description.is_null() {
        return None;
    }
    // SAFETY: the disk returned a retained description dictionary.
    let description: CFDictionary = unsafe { CFDictionary::wrap_under_create_rule(description) };
    // SAFETY: each key is a constant CFStringRef from Disk Arbitration.
    let (mount, kind, protocol, writable) = unsafe {
        (
            value::<CFURL>(&description, VOLUME_PATH)?.to_path()?,
            value::<CFString>(&description, VOLUME_KIND)?,
            value::<CFString>(&description, DEVICE_PROTOCOL)?,
            value::<CFBoolean>(&description, MEDIA_WRITABLE)?,
        )
    };
    if !eligible(
        path,
        &mount,
        &kind.to_string(),
        &protocol.to_string(),
        writable.into(),
    ) {
        return None;
    }
    // SAFETY: disk is a live DADiskRef; a nonzero service is returned retained.
    let service = unsafe { DADiskCopyIOMedia(disk.as_CFTypeRef()) };
    if service == 0 {
        return None;
    }
    let registry_id = MediaService(service).registry_id()?;
    // SAFETY: VOLUME_NAME is a constant CFStringRef from Disk Arbitration.
    let label = unsafe { value::<CFString>(&description, VOLUME_NAME) }
        .map(|label| label.to_string())
        .unwrap_or_default();
    Some((registry_id, label))
}

fn value<T: ConcreteCFType>(dictionary: &CFDictionary, key: CFStringRef) -> Option<T> {
    // SAFETY: the dictionary is live, and key is a Disk Arbitration CFString constant.
    let pointer = unsafe { CFDictionaryGetValue(dictionary.as_concrete_TypeRef(), key.cast()) };
    if pointer.is_null() {
        return None;
    }
    // SAFETY: Disk Arbitration description values are live CF objects retained by the dictionary.
    unsafe { CFType::wrap_under_get_rule(pointer) }.downcast_into()
}

fn eligible(path: &Path, reported: &Path, kind: &str, protocol: &str, writable: bool) -> bool {
    reported == path
        && writable
        && protocol == "USB"
        && matches!(kind, "msdos" | "exfat" | "msdos_fskit" | "exfat_fskit")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_the_disk_arbitration_kind_path_transport_and_write_flag() {
        let path = Path::new("/Volumes/TRACKER 1");
        assert!(eligible(path, path, "msdos", "USB", true));
        assert!(eligible(path, path, "msdos_fskit", "USB", true));
        assert!(eligible(path, path, "exfat", "USB", true));
        assert!(!eligible(path, path, "apfs", "USB", true));
        assert!(!eligible(path, path, "msdos", "USB", false));
        assert!(!eligible(path, path, "msdos", "Disk Image", true));
        assert!(!eligible(path, path, "msdos", "Virtual Interface", true));
        assert!(!eligible(
            path,
            Path::new("/Volumes/TRACKER"),
            "msdos",
            "USB",
            true,
        ));
    }
}
