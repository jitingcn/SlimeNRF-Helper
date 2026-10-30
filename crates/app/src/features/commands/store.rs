use crate::features::commands::model::{
    MoveDirection, SaveError, SavedCommandDraft, SavedCommandId, SavedCommands,
};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

const MAX_STORE_BYTES: usize = 1_048_576;

pub(crate) fn default_path() -> Result<PathBuf, String> {
    crate::storage_path::preferences().map(|path| path.with_file_name("custom-shortcuts.json"))
}

#[derive(Clone, Debug)]
pub(crate) enum ShortcutEdit {
    Save(SavedCommandDraft),
    Remove(SavedCommandId),
    SetPinned {
        id: SavedCommandId,
        pinned: bool,
    },
    Move {
        id: SavedCommandId,
        direction: MoveDirection,
    },
}

#[derive(Debug)]
pub(crate) enum ShortcutStoreError {
    Validation(SaveError),
    Persistence(String),
}

#[derive(Debug)]
pub(crate) struct ShortcutStore {
    path: PathBuf,
    snapshot: Arc<SavedCommands>,
}

#[derive(Serialize, Deserialize)]
struct StoreDocument {
    version: u32,
    #[serde(flatten)]
    library: SavedCommands,
}

impl ShortcutStore {
    pub(crate) fn load(path: PathBuf) -> Result<Self, String> {
        let snapshot = Arc::new(read_store(&path)?);
        Ok(Self { path, snapshot })
    }

    pub(crate) fn snapshot(&self) -> Arc<SavedCommands> {
        Arc::clone(&self.snapshot)
    }

    pub(crate) fn apply(
        &mut self,
        edit: ShortcutEdit,
    ) -> Result<Option<SavedCommandId>, ShortcutStoreError> {
        let mut next = read_store(&self.path).map_err(ShortcutStoreError::Persistence)?;
        let (id, changed) = match edit {
            ShortcutEdit::Save(draft) => (
                Some(
                    next.save_draft(draft)
                        .map_err(ShortcutStoreError::Validation)?,
                ),
                true,
            ),
            ShortcutEdit::Remove(id) => (None, next.remove(id)),
            ShortcutEdit::SetPinned { id, pinned } => (None, next.set_pinned(id, pinned)),
            ShortcutEdit::Move { id, direction } => (None, next.move_card(id, direction)),
        };
        if !changed {
            self.snapshot = Arc::new(next);
            return Ok(id);
        }
        next.validate().map_err(ShortcutStoreError::Validation)?;
        let document = StoreDocument {
            version: 1,
            library: next,
        };
        let bytes = serde_json::to_vec_pretty(&document)
            .map_err(|error| ShortcutStoreError::Persistence(save_error(&self.path, error)))?;
        if bytes.len() >= MAX_STORE_BYTES {
            return Err(ShortcutStoreError::Persistence(crate::i18n::tr(
                "shortcuts.store.error.too_large",
            )));
        }
        persist(&self.path, &bytes).map_err(ShortcutStoreError::Persistence)?;
        self.snapshot = Arc::new(document.library);
        Ok(id)
    }
}

fn read_store(path: &Path) -> Result<SavedCommands, String> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(SavedCommands::default());
        }
        Err(error) => {
            return Err(rust_i18n::t!(
                "shortcuts.store.error.read",
                path = path.display(),
                error = error
            )
            .to_string());
        }
    };
    let invalid = |error: String| {
        rust_i18n::t!(
            "shortcuts.store.error.invalid",
            path = path.display(),
            error = error
        )
        .to_string()
    };
    let metadata = file
        .metadata()
        .map_err(|error| invalid(error.to_string()))?;
    if !metadata.is_file() {
        return Err(invalid(crate::i18n::tr(
            "shortcuts.store.error.regular_file",
        )));
    }
    if metadata.len() > MAX_STORE_BYTES as u64 {
        return Err(invalid(crate::i18n::tr("shortcuts.store.error.too_large")));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_STORE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| invalid(error.to_string()))?;
    if bytes.len() > MAX_STORE_BYTES {
        return Err(invalid(crate::i18n::tr("shortcuts.store.error.too_large")));
    }
    let document: StoreDocument =
        serde_json::from_slice(&bytes).map_err(|error| invalid(error.to_string()))?;
    if document.version != 1 {
        return Err(invalid(
            rust_i18n::t!("shortcuts.store.error.version", version = document.version).to_string(),
        ));
    }
    document
        .library
        .validate()
        .map_err(|error| invalid(format!("{error:?}")))?;
    Ok(document.library)
}

fn save_error(path: &Path, error: impl std::fmt::Display) -> String {
    rust_i18n::t!(
        "shortcuts.store.error.save",
        path = path.display(),
        error = error
    )
    .to_string()
}

fn persist(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let save = || -> Result<(), Box<dyn std::error::Error>> {
        fs::create_dir_all(parent)?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        temporary.write_all(bytes)?;
        temporary.write_all(b"\n")?;
        temporary.as_file().sync_all()?;
        temporary.persist(path)?;
        Ok(())
    };
    save().map_err(|error| save_error(path, error))
}

#[cfg(test)]
mod tests;
