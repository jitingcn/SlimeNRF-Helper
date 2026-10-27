use std::ffi::c_void;
use std::mem::{ManuallyDrop, size_of};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};

use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    CM_NOTIFY_ACTION, CM_NOTIFY_ACTION_DEVICEINTERFACEARRIVAL,
    CM_NOTIFY_ACTION_DEVICEINTERFACEREMOVAL, CM_NOTIFY_EVENT_DATA, CM_NOTIFY_FILTER,
    CM_NOTIFY_FILTER_0, CM_NOTIFY_FILTER_0_0, CM_NOTIFY_FILTER_TYPE_DEVICEINTERFACE,
    CM_Register_Notification, CM_Unregister_Notification, CR_SUCCESS, HCMNOTIFICATION,
};
use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::Ioctl::GUID_DEVINTERFACE_COMPORT;

/// One registration and one coalesced bit, irrespective of the event rate.
/// No interface paths, device handles, queued work, or UI references escape here.
pub(super) struct SerialNotifications {
    handle: HCMNOTIFICATION,
    dirty: ManuallyDrop<Box<AtomicBool>>,
}

impl SerialNotifications {
    pub(super) fn register() -> Result<Self, u32> {
        let dirty = Box::new(AtomicBool::new(false));
        let filter = CM_NOTIFY_FILTER {
            cbSize: size_of::<CM_NOTIFY_FILTER>() as u32,
            FilterType: CM_NOTIFY_FILTER_TYPE_DEVICEINTERFACE,
            u: CM_NOTIFY_FILTER_0 {
                DeviceInterface: CM_NOTIFY_FILTER_0_0 {
                    ClassGuid: GUID_DEVINTERFACE_COMPORT,
                },
            },
            ..Default::default()
        };
        let mut handle = ptr::null_mut();
        // SAFETY: the filter and output are valid for this call. The boxed
        // atomic has a stable address even if callbacks start before return,
        // and remains alive until successful unregistration drains callbacks.
        let result = unsafe {
            CM_Register_Notification(
                &filter,
                ptr::from_ref(dirty.as_ref()).cast(),
                Some(notify),
                &mut handle,
            )
        };
        if result != CR_SUCCESS {
            // No registration exists; the caller keeps its periodic fallback.
            return Err(result);
        }
        Ok(Self {
            handle,
            dirty: ManuallyDrop::new(dirty),
        })
    }

    pub(super) fn take_dirty(&self) -> bool {
        self.dirty.swap(false, Ordering::Relaxed)
    }
}

impl Drop for SerialNotifications {
    fn drop(&mut self) {
        // SAFETY: this is the uniquely owned registration, and drop never runs
        // inside its callback. Unregistration waits for outstanding callbacks.
        let result = unsafe { CM_Unregister_Notification(self.handle) };
        if result == CR_SUCCESS {
            // SAFETY: no callback can access this allocation after successful
            // unregistration. ManuallyDrop prevents a second destruction.
            unsafe { ManuallyDrop::drop(&mut self.dirty) };
        }
        // On an unexpected unregister failure, retain the single atomic rather
        // than free memory Windows might still reference. Never retry in drop.
    }
}

unsafe extern "system" fn notify(
    _handle: HCMNOTIFICATION,
    context: *const c_void,
    action: CM_NOTIFY_ACTION,
    _event_data: *const CM_NOTIFY_EVENT_DATA,
    _event_data_size: u32,
) -> u32 {
    if matches!(
        action,
        CM_NOTIFY_ACTION_DEVICEINTERFACEARRIVAL | CM_NOTIFY_ACTION_DEVICEINTERFACEREMOVAL
    ) {
        // SAFETY: registration supplies a non-null, stable AtomicBool, retained
        // until unregister has waited for this callback. No payload is read.
        unsafe { &*context.cast::<AtomicBool>() }.store(true, Ordering::Relaxed);
    }
    ERROR_SUCCESS
}

#[cfg(test)]
mod tests {
    use super::notify;
    use std::ptr;
    use std::sync::atomic::{AtomicBool, Ordering};
    use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
        CM_NOTIFY_ACTION_DEVICEINTERFACEARRIVAL, CM_NOTIFY_ACTION_DEVICEINTERFACEREMOVAL,
        CM_NOTIFY_ACTION_DEVICEQUERYREMOVE,
    };
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;

    #[test]
    fn interface_events_coalesce_without_consuming_payloads() {
        let dirty = AtomicBool::new(false);
        for action in [
            CM_NOTIFY_ACTION_DEVICEINTERFACEARRIVAL,
            CM_NOTIFY_ACTION_DEVICEINTERFACEREMOVAL,
            CM_NOTIFY_ACTION_DEVICEINTERFACEARRIVAL,
        ] {
            // SAFETY: the atomic lives through the synchronous callback; the
            // unused native handle and payload may be null.
            let result = unsafe {
                notify(
                    ptr::null_mut(),
                    ptr::from_ref(&dirty).cast(),
                    action,
                    ptr::null(),
                    0,
                )
            };
            assert_eq!(result, ERROR_SUCCESS);
        }
        assert!(dirty.swap(false, Ordering::Relaxed));
        assert!(!dirty.swap(false, Ordering::Relaxed));
        // SAFETY: same live context; unrelated actions must not request a scan.
        unsafe {
            notify(
                ptr::null_mut(),
                ptr::from_ref(&dirty).cast(),
                CM_NOTIFY_ACTION_DEVICEQUERYREMOVE,
                ptr::null(),
                0,
            );
        }
        assert!(!dirty.load(Ordering::Relaxed));
    }
}
