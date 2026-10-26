//! Background firmware operations and retained worker ownership.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpdateOutcome {
    Completed,
    ActivationUnconfirmed(ReceiverActivationReport),
    Uf2CopyUnconfirmed(Uf2CopyReport),
    Failed(String),
}

/// Full host-write acceptance with device disappearance during UF2 finalization.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Uf2CopyReport {
    pub completed: u64,
    pub total: u64,
    pub stage: Uf2FinalizationStage,
    pub os_error: Option<i32>,
    pub diagnostic: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Uf2FinalizationStage {
    Flush,
    SyncAll,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReceiverActivationReport {
    pub submission: ActivationSubmission,
    pub availability: ReceiverAvailability,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ActivationSubmission {
    Sent,
    DeliveryUncertain(String),
    Accepted,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReceiverAvailability {
    Responding,
    /// A control interface was enumerated, without verifying a firmware response.
    Detected {
        prior_diagnostic: String,
    },
    Unconfirmed(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OtaPhase {
    Queued,
    Preparing,
    Transferring,
    Verifying,
    Activating,
    Completed,
    ActivationUnconfirmed,
    Failed,
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OtaPreparation {
    ReceiverRecovery,
    QueryingDevice,
    StartingUpdate,
}

#[derive(Clone, Debug)]
pub struct OtaTargetProgress {
    pub receiver_key: String,
    pub id: u8,
    pub phase: OtaPhase,
    pub preparation: Option<OtaPreparation>,
    pub error: Option<String>,
    pub completed: u64,
    pub total: u64,
    pub bytes_per_second: Option<u64>,
    pub eta_seconds: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct Progress {
    pub message: String,
    pub completed: u64,
    pub total: u64,
    pub ota: Vec<OtaTargetProgress>,
}

impl Progress {
    pub fn bytes(message: impl Into<String>, completed: u64, total: u64) -> Self {
        Self {
            message: message.into(),
            completed,
            total,
            ota: Vec::new(),
        }
    }

    pub fn stage(message: impl Into<String>) -> Self {
        Self::bytes(message, 0, 0)
    }
}

use futures_util::FutureExt;
use parking_lot::Mutex;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};
use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll, Wake, Waker},
};

use crate::app_runtime::Executor;

/// Coalesced progress remains observable without scheduling UI work from Tokio.
#[derive(Clone)]
pub(crate) struct ProgressReporter(Arc<Mutex<Option<Progress>>>);

impl ProgressReporter {
    pub(crate) fn emit(&self, progress: Progress) {
        *self.0.lock() = Some(progress);
    }
}

enum Worker<T> {
    Blocking(JoinHandle<Result<T, String>>),
    Async(tokio::task::JoinHandle<Result<T, String>>),
}

impl<T> Worker<T> {
    fn is_finished(&self) -> bool {
        match self {
            Self::Blocking(worker) => worker.is_finished(),
            Self::Async(worker) => worker.is_finished(),
        }
    }

    fn join(self) -> Result<T, String> {
        match self {
            Self::Blocking(worker) => worker
                .join()
                .unwrap_or_else(|_| Err("Firmware worker panicked".into())),
            Self::Async(mut worker) => {
                // Shutdown calls this on its cleanup thread, never in GPUI's loop.
                // Retain and poll the actual join handle: dropping it would detach
                // cancellation cleanup and could release device leases too early.
                struct JoinWake(thread::Thread);
                impl Wake for JoinWake {
                    fn wake(self: Arc<Self>) {
                        self.0.unpark();
                    }
                }
                let waker = Waker::from(Arc::new(JoinWake(thread::current())));
                let mut cx = Context::from_waker(&waker);
                loop {
                    match Pin::new(&mut worker).poll(&mut cx) {
                        Poll::Ready(result) => {
                            break result.unwrap_or_else(|error| {
                                Err(format!("Async firmware worker failed: {error}"))
                            });
                        }
                        Poll::Pending => thread::park(),
                    }
                }
            }
        }
    }
}

/// A worker is retained until its result is joined, including during app shutdown.
pub struct Job<T> {
    cancel: Arc<AtomicBool>,
    worker: Option<Worker<T>>,
    progress_slot: Arc<Mutex<Option<Progress>>>,
    pub progress: Option<Progress>,
}

impl<T: Send + 'static> Job<T> {
    pub fn start(
        work: impl FnOnce(&AtomicBool, &mut dyn FnMut(Progress)) -> Result<T, String> + Send + 'static,
    ) -> Result<Self, String> {
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let progress_slot = Arc::new(Mutex::new(None));
        let worker_progress = progress_slot.clone();
        let worker = thread::Builder::new()
            .name("firmware-worker".into())
            .spawn(move || {
                work(&worker_cancel, &mut |value| {
                    *worker_progress.lock() = Some(value);
                })
            })
            .map_err(|error| error.to_string())?;
        Ok(Self {
            cancel,
            worker: Some(Worker::Blocking(worker)),
            progress_slot,
            progress: None,
        })
    }

    pub(crate) fn start_async<F>(
        executor: &Executor,
        work: impl FnOnce(Arc<AtomicBool>, ProgressReporter) -> F + Send + 'static,
    ) -> Self
    where
        F: Future<Output = Result<T, String>> + Send + 'static,
    {
        let cancel = Arc::new(AtomicBool::new(false));
        let progress_slot = Arc::new(Mutex::new(None));
        let future = work(cancel.clone(), ProgressReporter(progress_slot.clone()));
        let worker = executor.spawn(future);
        Self {
            cancel,
            worker: Some(Worker::Async(worker)),
            progress_slot,
            progress: None,
        }
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }

    pub fn poll(&mut self) -> Option<Result<T, String>> {
        if let Some(value) = self.progress_slot.lock().take() {
            self.progress = Some(value);
        }
        if !self
            .worker
            .as_ref()
            .is_some_and(|worker| worker.is_finished())
        {
            return None;
        }
        let worker = self.worker.take().unwrap();
        // is_finished above guarantees this join cannot wait on the UI thread.
        let result = match worker {
            Worker::Async(worker) => worker
                .now_or_never()
                .expect("finished async worker must be ready")
                .unwrap_or_else(|error| Err(format!("Async firmware worker failed: {error}"))),
            worker => worker.join(),
        };
        if let Some(value) = self.progress_slot.lock().take() {
            self.progress = Some(value);
        }
        Some(result)
    }

    pub fn shutdown(mut self) {
        self.cancel();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl<T> Drop for Job<T> {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Job, OtaPhase, OtaPreparation, OtaTargetProgress, Progress};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    #[test]
    fn async_cancel_retains_worker_until_cleanup_and_owned_resources_finish() {
        let runtime = crate::app_runtime::AppRuntime::new().unwrap();
        let executor = runtime.executor();
        let released = Arc::new(AtomicBool::new(false));
        struct Lease(Arc<AtomicBool>);
        impl Drop for Lease {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }
        let lease = Lease(released.clone());
        let (started, running) = std::sync::mpsc::channel();
        let (cleaning, cleanup_started) = std::sync::mpsc::channel();
        let (release, cleanup_done) = tokio::sync::oneshot::channel();
        let mut job = Job::start_async(&executor, move |cancel, progress| async move {
            let _lease = lease;
            started.send(()).unwrap();
            while !cancel.load(Ordering::Acquire) {
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
            progress.emit(Progress::stage("Cleaning up"));
            cleaning.send(()).unwrap();
            cleanup_done.await.unwrap();
            progress.emit(Progress::stage("Cleanup finished"));
            Ok(17)
        });
        running
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        job.cancel();
        cleanup_started
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        assert!(job.poll().is_none(), "cancellation is not a joined result");
        assert!(!released.load(Ordering::Acquire));
        assert_eq!(job.progress.as_ref().unwrap().message, "Cleaning up");
        release.send(()).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let result = loop {
            if let Some(result) = job.poll() {
                break result;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "async job did not join"
            );
            std::thread::yield_now();
        };
        assert_eq!(result.unwrap(), 17);
        assert!(released.load(Ordering::Acquire));
        assert_eq!(job.progress.as_ref().unwrap().message, "Cleanup finished");
        assert!(job.poll().is_none());
        drop(job);
        runtime.shutdown();
    }

    #[test]
    fn dropping_async_job_cancels_and_joins_cleanup_without_detaching() {
        let runtime = crate::app_runtime::AppRuntime::new().unwrap();
        let ended = Arc::new(AtomicBool::new(false));
        let worker_ended = ended.clone();
        let job = Job::start_async(&runtime.executor(), move |cancel, _| async move {
            while !cancel.load(Ordering::Acquire) {
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
            // Cancellation-independent cleanup is still awaited by the owner.
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            worker_ended.store(true, Ordering::Release);
            Ok(())
        });
        drop(job);
        assert!(ended.load(Ordering::Acquire));
        runtime.shutdown();
    }

    #[test]
    fn async_panics_are_joined_as_errors() {
        let runtime = crate::app_runtime::AppRuntime::new().unwrap();
        let mut job = Job::<()>::start_async(&runtime.executor(), |_, _| async {
            panic!("test async worker panic");
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let result = loop {
            if let Some(result) = job.poll() {
                break result;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "async panic did not join"
            );
            std::thread::yield_now();
        };
        assert!(result.unwrap_err().contains("Async firmware worker failed"));
        drop(job);
        runtime.shutdown();
    }

    #[test]
    fn shutdown_cancels_and_joins_before_returning() {
        let ended = Arc::new(AtomicBool::new(false));
        let worker_ended = ended.clone();
        let job = Job::start(move |cancel, _| {
            while !cancel.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
            worker_ended.store(true, Ordering::Release);
            Ok(())
        })
        .unwrap();
        job.shutdown();
        assert!(ended.load(Ordering::Acquire));
    }

    #[test]
    fn polling_retains_the_real_result_and_last_progress() {
        let mut job = Job::start(|_, progress| {
            progress(Progress::bytes("validated", 4, 4));
            Ok(42)
        })
        .unwrap();
        let result = loop {
            if let Some(result) = job.poll() {
                break result;
            }
            std::thread::yield_now();
        };
        assert_eq!(result.unwrap(), 42);
        assert_eq!(job.progress.as_ref().unwrap().message, "validated");
        assert!(job.poll().is_none());
    }

    #[test]
    fn coalesced_live_worker_snapshots_retain_terminal_error_and_preparation() {
        let (published, ready) = std::sync::mpsc::channel();
        let (release, resume) = std::sync::mpsc::channel();
        let mut job = Job::start(move |_, emit| {
            let mut snapshot = Progress::stage("Updating ESB devices");
            snapshot.ota.push(OtaTargetProgress {
                receiver_key: "receiver-a".into(),
                id: 0,
                phase: OtaPhase::Failed,
                preparation: None,
                error: Some("Target 0: CRC mismatch".into()),
                completed: 100,
                total: 100,
                bytes_per_second: None,
                eta_seconds: None,
            });
            emit(snapshot.clone());
            snapshot.ota[0].error =
                Some("Target 0: CRC mismatch; OTA abort failed: disconnected".into());
            let mut active = snapshot.ota[0].clone();
            active.id = 1;
            active.phase = OtaPhase::Preparing;
            active.preparation = Some(OtaPreparation::ReceiverRecovery);
            active.error = None;
            active.completed = 0;
            snapshot.ota.push(active);
            emit(snapshot);
            published.send(()).unwrap();
            resume.recv().unwrap();
            Ok(())
        })
        .unwrap();
        ready.recv().unwrap();
        let live_result = job.poll();
        release.send(()).unwrap();
        assert!(live_result.is_none());
        let rows = &job.progress.as_ref().unwrap().ota;
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[0].error.as_deref(),
            Some("Target 0: CRC mismatch; OTA abort failed: disconnected")
        );
        assert_eq!(rows[1].preparation, Some(OtaPreparation::ReceiverRecovery));
        job.shutdown();
    }

    #[test]
    fn dropping_a_worker_never_detaches_its_owner() {
        let ended = Arc::new(AtomicBool::new(false));
        let worker_ended = ended.clone();
        let job = Job::start(move |cancel, _| {
            while !cancel.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
            worker_ended.store(true, Ordering::Release);
            Ok(())
        })
        .unwrap();
        drop(job);
        assert!(ended.load(Ordering::Acquire));
    }
}
