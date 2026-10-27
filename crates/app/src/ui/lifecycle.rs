use super::{Workbench, flash, hid_log};
use futures_util::{FutureExt as _, future::Shared};
use gpui_kit::{self as gpui, Context, Task};
use std::sync::Arc;

#[derive(Default)]
struct ShutdownPayload {
    calibration: Option<crate::calibration::CalibrationWorker>,
    capture: Option<crate::capture_device::CaptureWorker>,
    replay: Vec<std::thread::JoinHandle<()>>,
    hid_log: Option<hid_log::HidLogCleanup>,
    firmware: [Option<crate::update::Job<flash::Payload>>; 2],
    default_image: Option<crate::update::Job<flash::Image>>,
    sessions: Option<std::thread::JoinHandle<()>>,
    discovery: Option<std::thread::JoinHandle<()>>,
    files: Vec<Shared<Task<()>>>,
    _background: Option<gpui::BackgroundExecutor>,
}

impl ShutdownPayload {
    fn join(self) {
        drop(self.calibration);
        drop(self.capture);
        for worker in self.replay {
            let _ = worker.join();
        }
        if let Some(cleanup) = self.hid_log {
            cleanup.join();
        }
        for job in self.firmware.into_iter().flatten() {
            job.shutdown();
        }
        if let Some(job) = self.default_image {
            job.shutdown();
        }
        if let Some(worker) = self.sessions {
            let _ = worker.join();
        }
        if let Some(worker) = self.discovery {
            let _ = worker.join();
        }
        if self.files.is_empty() {
            return;
        }
        use std::{
            future::Future as _,
            task::{Context, Poll, Wake, Waker},
        };
        struct JoinWake(std::thread::Thread);
        impl Wake for JoinWake {
            fn wake(self: Arc<Self>) {
                self.0.unpark();
            }
        }
        let waker = Waker::from(Arc::new(JoinWake(std::thread::current())));
        let mut context = Context::from_waker(&waker);
        for task in self.files {
            let mut task = std::pin::pin!(task);
            loop {
                match task.as_mut().poll(&mut context) {
                    Poll::Ready(()) => break,
                    Poll::Pending => std::thread::park(),
                }
            }
        }
    }
}

#[derive(Default)]
enum ShutdownTransfer {
    #[default]
    Empty,
    Pending(ShutdownPayload),
    Deferred(ShutdownPayload),
}

#[derive(Default)]
pub(super) struct ShutdownHost {
    payload: parking_lot::Mutex<ShutdownTransfer>,
    worker: parking_lot::Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl ShutdownHost {
    fn start(self: &Arc<Self>, payload: ShutdownPayload) -> tokio::sync::oneshot::Receiver<()> {
        self.start_with(payload, |host, finished| {
            std::thread::Builder::new()
                .name("application-shutdown".into())
                .spawn(move || {
                    host.take_payload().join();
                    let _ = finished.send(());
                })
        })
    }

    fn start_with(
        self: &Arc<Self>,
        payload: ShutdownPayload,
        spawn: impl FnOnce(
            Arc<Self>,
            tokio::sync::oneshot::Sender<()>,
        ) -> std::io::Result<std::thread::JoinHandle<()>>,
    ) -> tokio::sync::oneshot::Receiver<()> {
        *self.payload.lock() = ShutdownTransfer::Pending(payload);
        let (finished, permission_to_exit) = tokio::sync::oneshot::channel();
        match spawn(self.clone(), finished) {
            Ok(worker) => *self.worker.lock() = Some(worker),
            Err(error) => {
                let mut slot = self.payload.lock();
                if let ShutdownTransfer::Pending(payload) = std::mem::take(&mut *slot) {
                    *slot = ShutdownTransfer::Deferred(payload);
                }
                eprintln!("Cannot start application shutdown worker: {error}");
            }
        }
        permission_to_exit
    }

    fn take_payload(&self) -> ShutdownPayload {
        match std::mem::take(&mut *self.payload.lock()) {
            ShutdownTransfer::Pending(payload) | ShutdownTransfer::Deferred(payload) => payload,
            ShutdownTransfer::Empty => ShutdownPayload::default(),
        }
    }

    pub(super) fn finish(&self) {
        let worker = self.worker.lock().take();
        if let Some(worker) = worker {
            let _ = worker.join();
        }
        self.take_payload().join();
    }
}

impl Workbench {
    pub(super) fn shutdown_cleanup(&mut self, cx: &mut Context<Self>) -> Shared<Task<()>> {
        if let Some(cleanup) = &self.shutdown_cleanup {
            return cleanup.clone();
        }
        self.shutting_down = true;
        self.stop_custom_commands();
        let mut accepted_files = self.shutdown_terminal_exports();
        accepted_files.extend(self.shutdown_temperature(cx));
        accepted_files.extend(self.shutdown_replay_presets());
        accepted_files.extend(self.saved_commands_shutdown(cx).map(|task| task.shared()));
        accepted_files.extend(self.registry_shutdown(cx).map(|task| task.shared()));
        accepted_files.extend(self.preferences_shutdown(cx));
        self.shutdown_rssi(cx);
        self.shutdown_notices(cx);
        let (firmware_cleanup, default_image_cleanup) = self.flash.stop_for_shutdown();
        let payload = ShutdownPayload {
            calibration: self.shutdown_calibration(),
            capture: self.shutdown_capture(),
            replay: self.shutdown_replay(),
            hid_log: Some(self.shutdown_hid_log()),
            firmware: firmware_cleanup,
            default_image: default_image_cleanup,
            sessions: self.manager.shutdown(),
            discovery: self.monitor.shutdown(),
            files: accepted_files,
            _background: Some(cx.background_executor().clone()),
        };
        let cleanup_finished = self.shutdown_host.start(payload);
        let cleanup = cx
            .background_executor()
            .spawn(async move {
                let _ = cleanup_finished.await;
            })
            .shared();
        self.shutdown_cleanup = Some(cleanup.clone());
        cx.notify();
        cleanup
    }

    pub(super) fn begin_shutdown(&mut self, cx: &mut Context<Self>) {
        if self.shutting_down {
            return;
        }
        let cleanup = self.shutdown_cleanup(cx);
        self._shutdown_task = Some(cx.spawn(async move |_, cx| {
            cleanup.await;
            cx.update(|cx| cx.quit());
        }));
    }
}

#[cfg(test)]
pub(super) fn join_background(task: futures_util::future::Shared<gpui_kit::Task<()>>) {
    ShutdownPayload {
        files: vec![task],
        ..ShutdownPayload::default()
    }
    .join();
}

#[cfg(test)]
mod tests;
