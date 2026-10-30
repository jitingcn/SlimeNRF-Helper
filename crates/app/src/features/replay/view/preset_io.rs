use super::ReplayFeature;
use crate::i18n::tr;
use futures_util::{FutureExt as _, future::Shared};
use gpui_kit::{BackgroundExecutor, Context, Task};
use parking_lot::Mutex;
use slimenrf_recording::replay::{
    ReplayParams,
    presets::{self, Preset, PresetStore},
};
use std::{io::Read as _, path::PathBuf, sync::Arc};

pub(super) enum PresetEdit {
    Save { name: String, params: ReplayParams },
    Rename { old: String, new: String },
    Delete { name: String },
}

#[derive(Default)]
pub(super) struct PresetIo {
    attempted: bool,
    stopped: bool,
    store: Option<PresetStore>,
    snapshot: Arc<[Preset]>,
    selected_custom: Option<String>,
    selection_epoch: u64,
    pub(super) load_error: Option<String>,
    active: Option<ActiveIo>,
    completion: Option<Task<()>>,
}

struct ActiveIo {
    join: Shared<Task<()>>,
    output: Arc<Mutex<Option<Outcome>>>,
    importing: bool,
}

struct StoreSnapshot {
    store: PresetStore,
    display: Arc<[Preset]>,
}

struct ChangedPreset {
    display: Arc<[Preset]>,
    selected: Option<String>,
}

enum Operation {
    Load(Option<PathBuf>),
    Edit {
        store: PresetStore,
        edit: PresetEdit,
        selection_epoch: u64,
    },
    Import {
        path: PathBuf,
        base: ReplayParams,
        generation: u64,
    },
    Export {
        path: PathBuf,
        params: ReplayParams,
    },
}

enum Outcome {
    Loaded(Result<StoreSnapshot, String>),
    Changed {
        store: PresetStore,
        result: Result<ChangedPreset, String>,
        selection_epoch: u64,
    },
    Imported {
        result: Result<ReplayParams, String>,
        generation: u64,
    },
    Exported(Result<(), String>),
}

pub(super) enum PresetCompletion {
    Quiet,
    Changed,
    Imported(ReplayParams),
    Exported,
    Error(String),
}

impl PresetIo {
    pub(super) fn busy(&self) -> bool {
        self.active.is_some()
    }

    pub(super) fn importing(&self) -> bool {
        self.active.as_ref().is_some_and(|active| active.importing)
    }

    pub(super) fn can_mutate(&self) -> bool {
        !self.stopped && !self.busy() && self.store.is_some()
    }

    pub(super) fn snapshot(&self) -> &[Preset] {
        &self.snapshot
    }

    pub(super) fn select(&mut self, custom_name: Option<String>) {
        self.selected_custom = custom_name;
        self.selection_epoch = self.selection_epoch.wrapping_add(1);
    }

    pub(super) fn selected_custom_name(&self) -> Option<&str> {
        self.selected_custom.as_deref()
    }

    pub(super) fn load(
        &mut self,
        path: Option<PathBuf>,
        executor: &BackgroundExecutor,
    ) -> Option<Shared<Task<()>>> {
        if self.attempted || self.stopped || self.busy() {
            return None;
        }
        self.attempted = true;
        Some(self.start(Operation::Load(path), executor))
    }

    pub(super) fn edit(
        &mut self,
        edit: PresetEdit,
        executor: &BackgroundExecutor,
    ) -> Option<Shared<Task<()>>> {
        if !self.can_mutate() {
            return None;
        }
        let store = self
            .store
            .take()
            .expect("admitted preset mutation has a store");
        Some(self.start(
            Operation::Edit {
                store,
                edit,
                selection_epoch: self.selection_epoch,
            },
            executor,
        ))
    }

    pub(super) fn import(
        &mut self,
        path: PathBuf,
        base: ReplayParams,
        generation: u64,
        executor: &BackgroundExecutor,
    ) -> Option<Shared<Task<()>>> {
        if self.stopped || self.busy() {
            return None;
        }
        Some(self.start(
            Operation::Import {
                path,
                base,
                generation,
            },
            executor,
        ))
    }

    pub(super) fn export(
        &mut self,
        path: PathBuf,
        params: ReplayParams,
        executor: &BackgroundExecutor,
    ) -> Option<Shared<Task<()>>> {
        if self.stopped || self.busy() {
            return None;
        }
        Some(self.start(Operation::Export { path, params }, executor))
    }

    fn start(&mut self, operation: Operation, executor: &BackgroundExecutor) -> Shared<Task<()>> {
        let importing = matches!(&operation, Operation::Import { .. });
        let output = Arc::new(Mutex::new(None));
        let worker_output = Arc::clone(&output);
        let join = executor
            .spawn(async move {
                let result = operation.run();
                *worker_output.lock() = Some(result);
            })
            .shared();
        self.active = Some(ActiveIo {
            join: join.clone(),
            output,
            importing,
        });
        join
    }

    fn finish(&mut self, generation: u64) -> PresetCompletion {
        let active = self
            .active
            .take()
            .expect("completed preset IO remains owned");
        self.completion = None;
        let outcome = active
            .output
            .lock()
            .take()
            .expect("joined preset IO publishes its result");
        match outcome {
            Outcome::Loaded(Ok(loaded)) => {
                self.snapshot = loaded.display;
                self.store = Some(loaded.store);
                self.load_error = None;
                PresetCompletion::Quiet
            }
            Outcome::Loaded(Err(error)) => {
                self.load_error = Some(error);
                PresetCompletion::Quiet
            }
            Outcome::Changed {
                store,
                result,
                selection_epoch,
            } => {
                self.store = Some(store);
                match result {
                    Ok(changed) => {
                        self.snapshot = changed.display;
                        if selection_epoch == self.selection_epoch {
                            self.selected_custom = changed.selected;
                        }
                        if self.selected_custom_name().is_some_and(|name| {
                            !self.snapshot.iter().any(|preset| preset.name == name)
                        }) {
                            self.selected_custom = None;
                        }
                        PresetCompletion::Changed
                    }
                    Err(error) => PresetCompletion::Error(error),
                }
            }
            Outcome::Imported {
                result,
                generation: captured,
            } if captured == generation => match result {
                Ok(params) => PresetCompletion::Imported(params),
                Err(error) => PresetCompletion::Error(error),
            },
            Outcome::Imported { .. } => PresetCompletion::Quiet,
            Outcome::Exported(Ok(())) => PresetCompletion::Exported,
            Outcome::Exported(Err(error)) => PresetCompletion::Error(error),
        }
    }

    pub(super) fn shutdown(&mut self) -> Vec<Shared<Task<()>>> {
        self.stopped = true;
        self.completion = None;
        self.active
            .take()
            .map(|active| active.join)
            .into_iter()
            .collect()
    }
}

impl Operation {
    fn run(self) -> Outcome {
        match self {
            Self::Load(path) => {
                let path = path.or_else(|| {
                    directories::ProjectDirs::from("org", "SlimeNRF", "SlimeNRF Helper")
                        .map(|dirs| dirs.config_dir().join("vqf-presets.json"))
                });
                Outcome::Loaded(PresetStore::load(path).map(|store| StoreSnapshot {
                    display: Arc::from(store.presets()),
                    store,
                }))
            }
            Self::Edit {
                mut store,
                edit,
                selection_epoch,
            } => {
                let result = match edit {
                    PresetEdit::Save { name, params } => store
                        .save(&name, &params)
                        .map(|()| Some(name.trim().into())),
                    PresetEdit::Rename { old, new } => {
                        store.rename(&old, &new).map(|()| Some(new.trim().into()))
                    }
                    PresetEdit::Delete { name } => store.delete(&name).map(|()| None),
                };
                let result = result.map(|selected| ChangedPreset {
                    display: Arc::from(store.presets()),
                    selected,
                });
                Outcome::Changed {
                    store,
                    result,
                    selection_epoch,
                }
            }
            Self::Import {
                path,
                base,
                generation,
            } => {
                let result = (|| {
                    let file = std::fs::File::open(path).map_err(|error| error.to_string())?;
                    let mut yaml = String::new();
                    file.take(65_537)
                        .read_to_string(&mut yaml)
                        .map_err(|error| error.to_string())?;
                    if yaml.len() > 65_536 {
                        return Err(tr("replay.preset.error.yaml_too_large"));
                    }
                    presets::merge_yaml(&base, &yaml)
                })();
                Outcome::Imported { result, generation }
            }
            Self::Export { path, params } => {
                Outcome::Exported(presets::to_yaml(&params).and_then(|yaml| {
                    crate::file_export::write(&path, yaml.as_bytes())
                        .map_err(|error| error.to_string())
                }))
            }
        }
    }
}

impl ReplayFeature {
    pub(super) fn initialize_replay_presets(&mut self, cx: &mut Context<Self>) {
        if self.stopped {
            return;
        }
        if let Some(join) = self.state.presets.load(None, cx.background_executor()) {
            self.watch_replay_preset_io(join, cx);
        }
    }

    pub(super) fn watch_replay_preset_io(
        &mut self,
        join: Shared<Task<()>>,
        cx: &mut Context<Self>,
    ) {
        let generation = self.state.generation;
        let window = self.window;
        self.state.presets.completion = Some(cx.spawn(async move |this, cx| {
            join.await;
            let _ = window.update(cx, |_, window, cx| {
                let _ = this.update(cx, |this, cx| {
                    if this.stopped {
                        return;
                    }
                    let completion = this.state.presets.finish(this.state.generation);
                    if generation != this.state.generation {
                        cx.notify();
                        return;
                    }
                    match completion {
                        PresetCompletion::Quiet => {}
                        PresetCompletion::Changed => {
                            this.state.notice = Some(tr("replay.preset.saved"));
                            this.state.error = None;
                        }
                        PresetCompletion::Imported(params) => {
                            this.state.options.params = params;
                            this.state.field_subscriptions.clear();
                            this.state.fields.clear();
                            this.initialize_view(window, cx);
                            this.replay_settings_changed(cx);
                            this.state.notice = Some(tr("replay.preset.yaml_imported"));
                        }
                        PresetCompletion::Exported => {
                            this.state.notice = Some(tr("replay.preset.yaml_exported"));
                            this.state.error = None;
                        }
                        PresetCompletion::Error(error) => this.state.error = Some(error),
                    }
                    cx.notify();
                });
            });
        }));
        cx.notify();
    }
}

#[cfg(test)]
mod tests;
