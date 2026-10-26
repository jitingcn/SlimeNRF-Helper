use super::ReplayParams;
use std::{ffi::c_void, ptr::NonNull};

unsafe extern "C" {
    fn slime_vqf_create(params: *const f32, gyr_dt: f32, acc_dt: f32, mag_dt: f32) -> *mut c_void;
    fn slime_vqf_destroy(filter: *mut c_void);
    fn slime_vqf_reset(filter: *mut c_void);
    fn slime_vqf_gyr(filter: *mut c_void, gyro: *const f32);
    fn slime_vqf_acc(filter: *mut c_void, accel: *const f32);
    fn slime_vqf_mag(filter: *mut c_void, mag: *const f32, dt: f32);
    fn slime_vqf_state(filter: *mut c_void, output: *mut f32);
}

/// One exclusively owned C filter. Construct and use on the replay worker;
/// neither the pointer nor C's compiler-dependent structs cross the safe API.
pub(super) struct Filter(NonNull<c_void>);
impl Filter {
    pub fn new(p: &ReplayParams, gyro_dt: f32, accel_dt: f32, mag_dt: f32) -> Result<Self, String> {
        let params = p.values();
        // The C constructor copies all values and owns its only allocation.
        NonNull::new(unsafe { slime_vqf_create(params.as_ptr(), gyro_dt, accel_dt, mag_dt) })
            .map(Self)
            .ok_or_else(|| "Unable to allocate firmware VQF state".to_owned())
    }
    pub fn reset(&mut self) {
        unsafe { slime_vqf_reset(self.0.as_ptr()) }
    }
    pub fn gyro(&mut self, v: [f32; 3]) {
        unsafe { slime_vqf_gyr(self.0.as_ptr(), v.as_ptr()) }
    }
    pub fn accel(&mut self, v: [f32; 3]) {
        unsafe { slime_vqf_acc(self.0.as_ptr(), v.as_ptr()) }
    }
    pub fn mag(&mut self, v: [f32; 3], dt: f32) {
        unsafe { slime_vqf_mag(self.0.as_ptr(), v.as_ptr(), dt) }
    }
    pub fn state(&mut self) -> [f32; 22] {
        let mut result = [0.; 22];
        unsafe { slime_vqf_state(self.0.as_ptr(), result.as_mut_ptr()) };
        result
    }
}
impl Drop for Filter {
    fn drop(&mut self) {
        unsafe { slime_vqf_destroy(self.0.as_ptr()) }
    }
}
