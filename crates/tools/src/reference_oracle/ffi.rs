use super::trace::Event;
use libloading::Library;
use std::ffi::c_int;
use std::marker::PhantomData;
use std::path::Path;
use std::rc::Rc;

type Init = unsafe extern "C" fn();
type Step = unsafe extern "C" fn(*mut f32, *mut f32, *mut f32, f32, f32, c_int, *mut f32);

/// Owns the library containing the exact wrapper compiled by the caller.
pub(super) struct Oracle {
    init: Init,
    step: Step,
    // This field outlives the copied function pointers and every call through them.
    _library: Library,
    // The C library has process-global state, including across duplicate loads.
    _single_threaded: PhantomData<Rc<()>>,
}

impl Oracle {
    pub(super) fn load(path: &Path) -> Result<Self, String> {
        // SAFETY: `path` is the unique temporary shared object just compiled from the
        // maintained wrapper and the caller-supplied, trusted firmware source. Loading
        // that source is the explicit purpose of this developer-only command.
        let library = unsafe { Library::new(path) }
            .map_err(|error| format!("loading {}: {error}", path.display()))?;
        // SAFETY: both symbols are defined with these exact C signatures by WRAPPER in
        // this module's parent. The function pointers stay owned together with `library`.
        let init = unsafe { library.get::<Init>(b"oracle_init\0") }
            .map_err(|error| format!("loading oracle_init: {error}"))?;
        let init = *init;
        // SAFETY: see above; the compiled WRAPPER defines oracle_step's argument layout.
        let step = unsafe { library.get::<Step>(b"oracle_step\0") }
            .map_err(|error| format!("loading oracle_step: {error}"))?;
        let step = *step;
        Ok(Self {
            init,
            step,
            _library: library,
            _single_threaded: PhantomData,
        })
    }

    pub(super) fn init(&mut self) {
        // SAFETY: the owned library is live, the signature matches the compiled wrapper,
        // and exclusive access prevents simultaneous mutation of its static state.
        unsafe { (self.init)() };
    }

    pub(super) fn step(&mut self, event: &Event) -> [f32; 22] {
        let mut gyro = event.gyro.map(|value| value as f32);
        let mut acceleration = event.acceleration.map(|value| value as f32);
        let mut mag = event.mag.map(|value| value as f32);
        let mut output = [0.0; 22];
        // SAFETY: these are initialized, writable arrays of exactly the sizes used by
        // oracle_step. They remain alive for the call; no pointer is retained. The
        // library stays loaded, and exclusive access serializes its static state.
        unsafe {
            (self.step)(
                gyro.as_mut_ptr(),
                acceleration.as_mut_ptr(),
                mag.as_mut_ptr(),
                event.mag_dt as f32,
                event.temperature as f32,
                c_int::from(event.thermal_on),
                output.as_mut_ptr(),
            );
        }
        output
    }
}
