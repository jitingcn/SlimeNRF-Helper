use super::{Engine, Fact, ObservationEnd, RunRequest, Snapshot, Target, direct, wireless};
use crate::{domain::ReceiverEndpoint, sessions::ReservedConsoleSource};
use parking_lot::Mutex;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::JoinHandle,
};

static NEXT_NATIVE_OPEN: AtomicU64 = AtomicU64::new(1);

pub(super) enum Access {
    Direct(ReservedConsoleSource),
    Wireless(ReceiverEndpoint),
}

pub(crate) struct CalibrationWorker {
    stop: Arc<AtomicBool>,
    snapshot: Arc<Mutex<Arc<Snapshot>>>,
    handle: Option<JoinHandle<()>>,
}
impl CalibrationWorker {
    pub(super) fn start(
        request: RunRequest,
        access: Access,
        previous: Option<Snapshot>,
    ) -> Result<Self, String> {
        Self::spawn(
            request,
            previous,
            move |request, engine, stop, publish| match access {
                Access::Direct(source) => {
                    let Target::Direct { endpoint, .. } = &request.target else {
                        return Err("Mismatched calibration console capability".into());
                    };
                    if stop.load(Ordering::Acquire) {
                        engine.finish(ObservationEnd::Stopped, 0);
                        return Ok(());
                    }
                    let inventory = crate::discovery::scan()?;
                    if inventory.serial_enumeration_failed
                        || !inventory
                            .ports
                            .iter()
                            .any(|current| super::session::same_endpoint(endpoint, current))
                    {
                        return Err(
                            "Tracker USB endpoint disappeared or changed before native open".into(),
                        );
                    }
                    let console = source.open()?;
                    let mut io = direct::NativeDirectIo::new(console);
                    direct::run(request, &mut io, engine, stop, publish)
                }
                Access::Wireless(receiver) => {
                    if stop.load(Ordering::Acquire) {
                        engine.finish(ObservationEnd::Stopped, 0);
                        return Ok(());
                    }
                    let mut io = wireless::NativeWirelessIo::open(&receiver)?;
                    wireless::run(request, &mut io, engine, stop, publish)
                }
            },
        )
    }
    fn spawn<F>(request: RunRequest, previous: Option<Snapshot>, run: F) -> Result<Self, String>
    where
        F: FnOnce(&RunRequest, &mut Engine, &AtomicBool, &dyn Fn(&Snapshot)) -> Result<(), String>
            + Send
            + 'static,
    {
        let mut current = NEXT_NATIVE_OPEN.load(Ordering::Relaxed);
        let native_open = loop {
            let next = current
                .checked_add(1)
                .ok_or_else(|| "Calibration native-open identifier exhausted".to_owned())?;
            match NEXT_NATIVE_OPEN.compare_exchange_weak(
                current,
                next,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(incarnation) => break incarnation,
                Err(observed) => current = observed,
            }
        };
        let mut engine = previous.map_or_else(
            || Engine::new(request.clone()),
            |snapshot| Engine::recover(request.clone(), snapshot),
        );
        engine.fact(Fact::NativeOpen(native_open), 0);
        let snapshot = Arc::new(Mutex::new(Arc::new(engine.snapshot.clone())));
        let publication = snapshot.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let control = stop.clone();
        let handle = std::thread::Builder::new()
            .name("calibration-observer".into())
            .spawn(move || {
                let mut engine = engine;
                let publish = |snapshot: &Snapshot| {
                    let mut current = publication.lock();
                    if current.observation_revision != snapshot.observation_revision {
                        *current = Arc::new(snapshot.clone());
                    }
                };
                if let Err(error) = run(&request, &mut engine, &control, &publish) {
                    let end = if engine.snapshot.dispatch == super::Dispatch::NotAttempted {
                        ObservationEnd::PreparationFailed
                    } else {
                        ObservationEnd::Disconnected
                    };
                    let elapsed = engine.snapshot.elapsed_ms;
                    engine.fact(Fact::Uncertain(error), elapsed);
                    engine.finish(end, elapsed);
                }
                if engine.snapshot.end.is_none() {
                    engine.finish(ObservationEnd::Complete, engine.snapshot.elapsed_ms);
                }
                publish(&engine.snapshot);
            })
            .map_err(|error| error.to_string())?;
        Ok(Self {
            stop,
            snapshot,
            handle: Some(handle),
        })
    }
    pub(crate) fn stop(&self) {
        self.stop.store(true, Ordering::Release);
    }
    pub(super) fn stopping(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }
    pub(crate) fn snapshot(&self) -> Arc<Snapshot> {
        self.snapshot.lock().clone()
    }
    pub(super) fn join(&mut self) -> bool {
        if !self.handle.as_ref().is_some_and(JoinHandle::is_finished) {
            return false;
        }
        if let Some(handle) = self.handle.take()
            && handle.join().is_err()
        {
            let mut snapshot = self.snapshot().as_ref().clone();
            snapshot.end = Some(ObservationEnd::Disconnected);
            snapshot.uncertainty = Some("Calibration observer stopped unexpectedly".into());
            snapshot.observe_change();
            *self.snapshot.lock() = Arc::new(snapshot);
        }
        true
    }
    #[cfg(test)]
    pub(super) fn scripted<F>(
        request: RunRequest,
        previous: Option<Snapshot>,
        run: F,
    ) -> Result<Self, String>
    where
        F: FnOnce(&RunRequest, &mut Engine, &AtomicBool, &dyn Fn(&Snapshot)) -> Result<(), String>
            + Send
            + 'static,
    {
        Self::spawn(request, previous, run)
    }
    #[cfg(test)]
    pub(super) fn fixture(
        request: RunRequest,
        previous: Option<Snapshot>,
        transport: super::fixture::Transport,
    ) -> Result<Self, String> {
        Self::scripted(
            request,
            previous,
            move |request, engine, stop, publish| match transport {
                super::fixture::Transport::Direct(mut io) => {
                    direct::run(request, &mut *io, engine, stop, publish)
                }
                super::fixture::Transport::Wireless(mut io) => {
                    wireless::run(request, &mut *io, engine, stop, publish)
                }
            },
        )
    }
}
impl Drop for CalibrationWorker {
    fn drop(&mut self) {
        self.stop();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}
