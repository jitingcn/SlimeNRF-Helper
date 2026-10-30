use crate::features::commands::{
    model::{SaveError, SavedCommandCard, SavedCommandId, SavedCommands},
    store::{self as storage, ShortcutEdit, ShortcutStore, ShortcutStoreError},
};
use futures_util::{FutureExt as _, future::Shared};
use gpui_kit::{BackgroundExecutor, Task};
use parking_lot::Mutex;
use std::sync::Arc;

#[derive(Default)]
pub(crate) struct CommandLibrary {
    attempted: bool,
    stopped: bool,
    store: Option<ShortcutStore>,
    snapshot: Option<Arc<SavedCommands>>,
    active: Option<StoreJob>,
    completion: Option<Task<()>>,
    error: Option<String>,
    #[cfg(test)]
    store_path: Option<std::path::PathBuf>,
}

struct StoreJob {
    join: Shared<Task<()>>,
    output: Arc<Mutex<Option<StoreJobResult>>>,
}

enum StoreJobResult {
    Loaded(Result<ShortcutStore, String>),
    Changed {
        store: ShortcutStore,
        result: Result<Option<SavedCommandId>, ShortcutStoreError>,
    },
}

pub(crate) enum LibraryCompletion {
    Loaded,
    Changed(Option<SavedCommandId>),
    Validation(SaveError),
    Failed,
}

impl CommandLibrary {
    pub(crate) fn initialize(&mut self, executor: &BackgroundExecutor) -> Option<Shared<Task<()>>> {
        if self.stopped || self.attempted || self.busy() {
            return None;
        }
        self.attempted = true;
        #[cfg(test)]
        let store_path = self.store_path.clone();
        Some(self.start_job(
            move || {
                #[cfg(test)]
                let path = store_path.map_or_else(storage::default_path, Ok);
                #[cfg(not(test))]
                let path = storage::default_path();
                StoreJobResult::Loaded(path.and_then(ShortcutStore::load))
            },
            executor,
        ))
    }

    pub(crate) fn reload(&mut self, executor: &BackgroundExecutor) -> Option<Shared<Task<()>>> {
        if self.stopped || self.busy() || self.has_store() {
            return None;
        }
        self.attempted = false;
        self.initialize(executor)
    }

    pub(crate) fn apply(
        &mut self,
        edit: ShortcutEdit,
        executor: &BackgroundExecutor,
    ) -> Option<Shared<Task<()>>> {
        if self.stopped || self.busy() {
            return None;
        }
        let mut store = self.store.take()?;
        self.error = None;
        Some(self.start_job(
            move || {
                let result = store.apply(edit);
                StoreJobResult::Changed { store, result }
            },
            executor,
        ))
    }

    fn start_job(
        &mut self,
        operation: impl FnOnce() -> StoreJobResult + Send + 'static,
        executor: &BackgroundExecutor,
    ) -> Shared<Task<()>> {
        let output = Arc::new(Mutex::new(None));
        let worker_output = Arc::clone(&output);
        let join = executor
            .spawn(async move {
                *worker_output.lock() = Some(operation());
            })
            .shared();
        self.active = Some(StoreJob {
            join: join.clone(),
            output,
        });
        join
    }

    pub(crate) fn retain_completion(&mut self, completion: Task<()>) {
        self.completion = Some(completion);
    }

    pub(crate) fn finish(&mut self) -> LibraryCompletion {
        let active = self
            .active
            .take()
            .expect("completed store operation remains owned");
        self.completion = None;
        let result = active
            .output
            .lock()
            .take()
            .expect("retained store operation completes with a result");
        match result {
            StoreJobResult::Loaded(Ok(store)) => {
                self.snapshot = Some(store.snapshot());
                self.store = Some(store);
                self.error = None;
                LibraryCompletion::Loaded
            }
            StoreJobResult::Loaded(Err(error)) => {
                self.error = Some(error);
                LibraryCompletion::Failed
            }
            StoreJobResult::Changed { store, result } => {
                let completion = match result {
                    Ok(saved_id) => {
                        self.snapshot = Some(store.snapshot());
                        self.error = None;
                        LibraryCompletion::Changed(saved_id)
                    }
                    Err(ShortcutStoreError::Validation(error)) => {
                        LibraryCompletion::Validation(error)
                    }
                    Err(ShortcutStoreError::Persistence(error)) => {
                        self.error = Some(error);
                        LibraryCompletion::Failed
                    }
                };
                self.store = Some(store);
                completion
            }
        }
    }

    pub(crate) fn shutdown(&mut self, executor: &BackgroundExecutor) -> Option<Task<()>> {
        self.stopped = true;
        self.completion = None;
        self.active.take().map(|active| {
            executor.spawn(async move {
                active.join.await;
            })
        })
    }

    pub(crate) fn busy(&self) -> bool {
        // A finished worker still belongs to the library until its foreground
        // completion applies the snapshot, or shutdown takes the retained join.
        self.active.is_some()
    }

    pub(crate) fn has_store(&self) -> bool {
        self.store.is_some()
    }

    pub(crate) fn snapshot(&self) -> Option<&SavedCommands> {
        self.snapshot.as_deref()
    }

    pub(crate) fn card(&self, id: SavedCommandId) -> Option<&SavedCommandCard> {
        self.snapshot()?.get(id)
    }

    pub(crate) fn has_pinned_cards(&self) -> bool {
        self.snapshot()
            .is_some_and(|snapshot| snapshot.cards().iter().any(|card| card.pinned))
    }

    pub(crate) fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub(crate) fn set_error(&mut self, error: String) {
        self.error = Some(error);
    }

    pub(crate) fn clear_error(&mut self) {
        self.error = None;
    }

    #[cfg(test)]
    pub(crate) fn for_test(path: std::path::PathBuf, store: Option<ShortcutStore>) -> Self {
        Self {
            attempted: store.is_some(),
            snapshot: store.as_ref().map(ShortcutStore::snapshot),
            store,
            store_path: Some(path),
            ..Self::default()
        }
    }

    #[cfg(test)]
    pub(crate) fn retained_join_for_test(&self) -> Option<Shared<Task<()>>> {
        self.active.as_ref().map(|active| active.join.clone())
    }
}
