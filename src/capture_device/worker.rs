//! Thread ownership, cancellation, immutable publication, and mandatory join.
use super::collection::{collect, expected_roster};
use super::io::NativeIo;
use super::{CaptureOutcome, CapturePhase, CaptureRequest, CaptureSnapshot};
use parking_lot::Mutex;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
pub struct CaptureWorker {
    cancel: Arc<AtomicBool>,
    state: Arc<Mutex<Arc<CaptureSnapshot>>>,
    handle: Option<JoinHandle<Result<CaptureOutcome, String>>>,
}
impl CaptureWorker {
    /// Call only after both external reservations are ready. Retain them through `join`.
    pub fn start(request: CaptureRequest) -> Result<Self, String> {
        let expected = expected_roster(&request)?;
        let cancel = Arc::new(AtomicBool::new(false));
        let state = Arc::new(Mutex::new(Arc::new(CaptureSnapshot::default())));
        let worker_cancel = Arc::clone(&cancel);
        let worker_state = Arc::clone(&state);
        let handle = thread::Builder::new()
            .name("raw-capture".into())
            .spawn(move || {
                let result = NativeIo::open(&request.source).and_then(|mut io| {
                    collect(&request, expected, &mut io, &worker_cancel, &worker_state)
                });
                if let Err(error) = &result {
                    let mut state = worker_state.lock();
                    let snapshot = Arc::make_mut(&mut *state);
                    snapshot.phase = CapturePhase::Failed;
                    snapshot.warnings.push(error.clone());
                }
                result
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            cancel,
            state,
            handle: Some(handle),
        })
    }
    /// Stable immutable publication; polling clones only the Arc, never tracks or previews.
    pub fn snapshot(&self) -> Arc<CaptureSnapshot> {
        Arc::clone(&self.state.lock())
    }
    pub fn stop(&self) {
        self.cancel.store(true, Ordering::Release);
    }
    pub fn is_finished(&self) -> bool {
        self.handle.as_ref().is_none_or(JoinHandle::is_finished)
    }
    /// Nonblocking; None means the caller must retain the worker AND leases.
    pub fn join(&mut self) -> Option<Result<CaptureOutcome, String>> {
        if !self.is_finished() {
            return None;
        }
        self.handle.take().map(|h| {
            h.join()
                .unwrap_or_else(|_| Err("Capture worker panicked".into()))
        })
    }
    #[cfg(test)]
    pub(crate) fn test_blocked_worker() -> (Self, std::sync::mpsc::Sender<()>) {
        let (release, wait) = std::sync::mpsc::channel();
        let handle = thread::spawn(move || {
            wait.recv().map_err(|e| e.to_string())?;
            Err("Test worker released".into())
        });
        (
            Self {
                cancel: Arc::new(AtomicBool::new(false)),
                state: Arc::new(Mutex::new(Arc::new(CaptureSnapshot::default()))),
                handle: Some(handle),
            },
            release,
        )
    }
}
impl Drop for CaptureWorker {
    fn drop(&mut self) {
        self.stop();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests;
