use super::Workbench;
use crate::features::settings::store::{self as settings, Preferences};
use futures_util::{FutureExt as _, future::Shared};
use gpui_kit::{BackgroundExecutor, Context, Task};
use parking_lot::Mutex;
use std::sync::Arc;

#[derive(Default)]
pub(super) struct PreferenceIo {
    target: SaveTarget,
    stopped: bool,
    pending: Option<Preferences>,
    active: Option<ActiveSave>,
    completion: Option<Task<()>>,
    saved: Option<Preferences>,
    #[cfg(test)]
    test_executor: Option<BackgroundExecutor>,
    pub(super) load_error: Option<String>,
    pub(super) save_error: Option<String>,
}

struct ActiveSave {
    join: Shared<Task<()>>,
    output: Arc<Mutex<Option<Result<Preferences, String>>>>,
    executor: BackgroundExecutor,
}

#[derive(Clone, Default)]
enum SaveTarget {
    #[default]
    User,
    #[cfg(test)]
    Path(std::path::PathBuf),
}

impl SaveTarget {
    fn save(&self, preferences: &Preferences) -> Result<(), String> {
        match self {
            Self::User => settings::save(preferences),
            #[cfg(test)]
            Self::Path(path) => settings::save_to_for_test(path, preferences),
        }
    }
}

impl PreferenceIo {
    pub(super) fn new(load_error: Option<String>) -> Self {
        Self {
            load_error,
            ..Self::default()
        }
    }

    #[cfg(test)]
    pub(super) fn for_test(path: std::path::PathBuf) -> Self {
        Self {
            target: SaveTarget::Path(path),
            ..Self::new(None)
        }
    }

    #[cfg(test)]
    pub(super) fn for_test_with_executor(
        path: std::path::PathBuf,
        executor: BackgroundExecutor,
    ) -> Self {
        Self {
            test_executor: Some(executor),
            ..Self::for_test(path)
        }
    }

    pub(super) fn busy(&self) -> bool {
        self.active.is_some() || self.pending.is_some()
    }

    pub(super) fn latest_snapshot_saved(&self, preferences: &Preferences) -> bool {
        !self.busy() && self.save_error.is_none() && self.saved.as_ref() == Some(preferences)
    }

    fn save(
        &mut self,
        preferences: Preferences,
        executor: &BackgroundExecutor,
    ) -> Option<Shared<Task<()>>> {
        if self.stopped {
            return None;
        }
        self.pending = Some(preferences);
        self.start_pending(executor)
    }

    fn start_pending(&mut self, executor: &BackgroundExecutor) -> Option<Shared<Task<()>>> {
        if self.active.is_some() {
            return None;
        }
        let preferences = self.pending.take()?;
        let target = self.target.clone();
        let output = Arc::new(Mutex::new(None));
        let worker_output = output.clone();
        let join = executor
            .spawn(async move {
                *worker_output.lock() = Some(target.save(&preferences).map(|()| preferences));
            })
            .shared();
        self.active = Some(ActiveSave {
            join: join.clone(),
            output,
            executor: executor.clone(),
        });
        Some(join)
    }

    fn finish(&mut self) -> Option<Shared<Task<()>>> {
        let active = self
            .active
            .take()
            .expect("completed preference save remains owned");
        self.completion = None;
        let result = active
            .output
            .lock()
            .take()
            .expect("joined preference save publishes its result");
        // Only the newest accepted snapshot can change the displayed save state.
        // Keep both errors visible while its replacement or retry is in flight.
        if self.pending.is_none() {
            match result {
                Ok(preferences) => {
                    self.saved = Some(preferences);
                    self.load_error = None;
                    self.save_error = None;
                }
                Err(error) => {
                    self.saved = None;
                    self.save_error = Some(error);
                }
            }
        }
        self.start_pending(&active.executor)
    }

    fn shutdown(&mut self, executor: &BackgroundExecutor) -> Option<Shared<Task<()>>> {
        self.stopped = true;
        self.completion = None;
        let active = self.active.take();
        let pending = self.pending.take();
        if active.is_none() && pending.is_none() {
            return None;
        }
        let target = self.target.clone();
        Some(
            executor
                .spawn(async move {
                    let mut result = Ok(());
                    if let Some(active) = active {
                        active.join.await;
                        result = active
                            .output
                            .lock()
                            .take()
                            .expect("joined preference save publishes its result")
                            .map(|_| ());
                    }
                    if let Some(preferences) = pending {
                        result = target.save(&preferences);
                    }
                    if let Err(error) = result {
                        eprintln!("Cannot save preferences during shutdown: {error}");
                    }
                })
                .shared(),
        )
    }
}

impl Workbench {
    pub(super) fn save_preferences(&mut self, cx: &mut Context<Self>) {
        let executor = cx.background_executor().clone();
        #[cfg(test)]
        let executor = self
            .preferences_io
            .test_executor
            .clone()
            .unwrap_or(executor);
        self.save_preferences_with_executor(&executor, cx);
    }

    fn save_preferences_with_executor(
        &mut self,
        executor: &BackgroundExecutor,
        cx: &mut Context<Self>,
    ) {
        if self.shutting_down {
            return;
        }
        if let Some(join) = self.preferences_io.save(self.preferences.clone(), executor) {
            self.watch_preferences_save(join, cx);
        }
        cx.notify();
    }

    fn watch_preferences_save(&mut self, join: Shared<Task<()>>, cx: &mut Context<Self>) {
        self.preferences_io.completion = Some(cx.spawn(async move |this, cx| {
            join.await;
            let _ = this.update(cx, |this, cx| {
                if this.shutting_down {
                    return;
                }
                if let Some(join) = this.preferences_io.finish() {
                    this.watch_preferences_save(join, cx);
                }
                cx.notify();
            });
        }));
    }

    pub(super) fn preferences_shutdown(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<Shared<Task<()>>> {
        self.preferences_io.shutdown(cx.background_executor())
    }
}

#[cfg(test)]
#[path = "preferences/tests.rs"]
mod tests;
