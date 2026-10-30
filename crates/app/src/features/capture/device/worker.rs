//! Thread ownership, cancellation, immutable publication, and mandatory join.
use super::collection::{collect, expected_roster};
use super::io::{ConsoleInitialization, NativeIo};
use super::{CaptureOutcome, CapturePhase, CaptureRequest, CaptureSnapshot};
use crate::sessions::{CaptureConsoleProbe, ReservedConsoleSource};
use parking_lot::Mutex;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};

pub(crate) enum CaptureConsolePreparation {
    Existing(CaptureConsoleProbe),
    Temporary(ReservedConsoleSource),
}

struct ConsoleRelease {
    closed: mpsc::Receiver<()>,
    released: mpsc::Sender<Result<(), String>>,
}

#[cfg(test)]
pub(crate) struct TestConsoleReleaseWorker {
    pub(crate) worker: CaptureWorker,
    pub(crate) closed: mpsc::Sender<()>,
    pub(crate) released: mpsc::Receiver<Result<(), String>>,
    pub(crate) unblock: mpsc::Sender<()>,
}

pub struct CaptureWorker {
    cancel: Arc<AtomicBool>,
    state: Arc<Mutex<Arc<CaptureSnapshot>>>,
    handle: Option<JoinHandle<Result<CaptureOutcome, String>>>,
    console_release: Option<ConsoleRelease>,
    console_cancel: Option<Arc<AtomicBool>>,
}
impl CaptureWorker {
    /// Call only after the receiver and raw serial reservations are ready.
    pub(crate) fn start(
        request: CaptureRequest,
        console: Option<CaptureConsolePreparation>,
    ) -> Result<Self, String> {
        let expected = expected_roster(&request)?;
        let cancel = Arc::new(AtomicBool::new(false));
        let state = Arc::new(Mutex::new(Arc::new(CaptureSnapshot::default())));
        let worker_cancel = Arc::clone(&cancel);
        let worker_state = Arc::clone(&state);
        let (console, console_release, console_cancel) = match console {
            None => (None, None, None),
            Some(CaptureConsolePreparation::Existing(probe)) => {
                let probe_cancel = probe.cancel_token();
                (
                    Some(ConsoleInitialization::Existing(probe)),
                    None,
                    Some(probe_cancel),
                )
            }
            Some(CaptureConsolePreparation::Temporary(source)) => {
                let (closed, closed_rx) = mpsc::channel();
                let (released, released_rx) = mpsc::channel();
                (
                    Some(ConsoleInitialization::Temporary {
                        source,
                        closed,
                        released: released_rx,
                    }),
                    Some(ConsoleRelease {
                        closed: closed_rx,
                        released,
                    }),
                    None,
                )
            }
        };
        let handle = thread::Builder::new()
            .name("raw-capture".into())
            .spawn(move || {
                let result = NativeIo::open(&request.source, console).and_then(|mut io| {
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
            console_release,
            console_cancel,
        })
    }
    pub(crate) fn console_closed(&self) -> bool {
        self.console_release
            .as_ref()
            .is_some_and(|release| match release.closed.try_recv() {
                Ok(()) | Err(mpsc::TryRecvError::Disconnected) => true,
                Err(mpsc::TryRecvError::Empty) => false,
            })
    }

    pub(crate) fn console_released(&mut self, result: Result<(), String>) {
        if let Some(release) = self.console_release.take() {
            let _ = release.released.send(result);
        }
    }

    /// Stable immutable publication; polling clones only the Arc, never tracks or previews.
    pub fn snapshot(&self) -> Arc<CaptureSnapshot> {
        Arc::clone(&self.state.lock())
    }
    pub fn stop(&self) {
        self.cancel.store(true, Ordering::Release);
        if let Some(cancel) = &self.console_cancel {
            cancel.store(true, Ordering::Release);
        }
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
                console_release: None,
                console_cancel: None,
            },
            release,
        )
    }

    #[cfg(test)]
    pub(crate) fn test_console_release_worker() -> TestConsoleReleaseWorker {
        let (mut worker, unblock) = Self::test_blocked_worker();
        let (closed, closed_rx) = mpsc::channel();
        let (released, released_rx) = mpsc::channel();
        worker.console_release = Some(ConsoleRelease {
            closed: closed_rx,
            released,
        });
        TestConsoleReleaseWorker {
            worker,
            closed,
            released: released_rx,
            unblock,
        }
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
