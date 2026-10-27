use crate::device_registry::{
    DeviceKey, DeviceObservation, DeviceRecord, DeviceRegistry, MAX_RECORDS,
    RegistryValidationError,
};
use serde::{Deserialize, Serialize, Serializer};
use std::{
    collections::BTreeSet,
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

const MAX_STORE_BYTES: usize = 1_048_576;
const STORE_VERSION: u32 = 1;

pub(crate) fn default_path() -> Result<PathBuf, String> {
    super::config_path().map(|path| path.with_file_name("device-registry.json"))
}

#[derive(Clone, Debug)]
pub(crate) enum RegistryEdit {
    Observe {
        observations: Vec<DeviceObservation>,
        // Only a clean inventory proving every known carrier absent can release a key.
        absent: BTreeSet<DeviceKey>,
    },
    Rename {
        key: DeviceKey,
        nickname: String,
    },
    Forget(DeviceKey),
    Remember(DeviceObservation),
}

#[derive(Debug)]
pub(crate) enum RegistryStoreError {
    Validation(RegistryValidationError),
    Persistence(RegistryPersistenceError),
}

#[derive(Debug)]
pub(crate) enum RegistryPersistenceError {
    Read { path: PathBuf, error: String },
    Invalid { path: PathBuf, error: String },
    UnsupportedVersion { version: u32 },
    TooLarge,
    NotRegularFile,
    Save { path: PathBuf, error: String },
}

impl From<RegistryValidationError> for RegistryStoreError {
    fn from(error: RegistryValidationError) -> Self {
        Self::Validation(error)
    }
}

impl From<RegistryPersistenceError> for RegistryStoreError {
    fn from(error: RegistryPersistenceError) -> Self {
        Self::Persistence(error)
    }
}

#[derive(Debug)]
pub(crate) struct RegistryStore {
    path: PathBuf,
    snapshot: Arc<DeviceRegistry>,
    suppressed: BTreeSet<DeviceKey>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StoreDocument {
    version: u32,
    records: Vec<DeviceRecord>,
}

#[derive(Serialize)]
struct SavedDocument<'a> {
    version: u32,
    records: SavedRecords<'a>,
}

struct SavedRecords<'a>(&'a DeviceRegistry);

impl Serialize for SavedRecords<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.0.records())
    }
}

impl RegistryStore {
    pub(crate) fn load(path: PathBuf) -> Result<Self, RegistryStoreError> {
        let snapshot = Arc::new(read_store(&path)?);
        Ok(Self {
            path,
            snapshot,
            suppressed: BTreeSet::new(),
        })
    }

    pub(crate) fn snapshot(&self) -> Arc<DeviceRegistry> {
        Arc::clone(&self.snapshot)
    }

    pub(crate) fn apply(&mut self, edit: RegistryEdit) -> Result<(), RegistryStoreError> {
        if let RegistryEdit::Forget(key) = &edit
            && self.suppressed.len() == MAX_RECORDS
            && !self.suppressed.contains(key)
        {
            return Err(RegistryValidationError::TooManyRecords.into());
        }
        if let RegistryEdit::Observe {
            observations,
            absent,
        } = &edit
        {
            if absent.len() > MAX_RECORDS {
                return Err(RegistryValidationError::TooManyRecords.into());
            }
            validate_batch(observations)?;
            let changed = observations
                .iter()
                .try_fold(false, |changed, observation| {
                    if self.suppressed.contains(&observation.key) {
                        Ok(changed)
                    } else {
                        self.snapshot
                            .observation_changes(observation)
                            .map(|next| changed || next)
                    }
                })?;
            if !changed {
                self.release_absent(absent, observations);
                return Ok(());
            }
        }

        let mut next = read_store(&self.path)?;
        let changed = match &edit {
            RegistryEdit::Observe { observations, .. } => {
                let mut changed = false;
                for observation in observations {
                    if !self.suppressed.contains(&observation.key) {
                        changed |= next.observe(observation)?;
                    }
                }
                changed
            }
            RegistryEdit::Rename { key, nickname } => next.rename(*key, nickname)?,
            RegistryEdit::Forget(key) => next.forget(*key),
            RegistryEdit::Remember(observation) => next.observe(observation)?,
        };
        if changed {
            let document = SavedDocument {
                version: STORE_VERSION,
                records: SavedRecords(&next),
            };
            let bytes = serde_json::to_vec_pretty(&document)
                .map_err(|error| save_error(&self.path, error))?;
            if bytes.len() >= MAX_STORE_BYTES {
                return Err(RegistryPersistenceError::TooLarge.into());
            }
            persist(&self.path, &bytes)?;
        }
        self.snapshot = Arc::new(next);
        match edit {
            RegistryEdit::Forget(key) => {
                self.suppressed.insert(key);
            }
            RegistryEdit::Remember(observation) => {
                self.suppressed.remove(&observation.key);
            }
            RegistryEdit::Observe {
                observations,
                absent,
            } => self.release_absent(&absent, &observations),
            RegistryEdit::Rename { .. } => {}
        }
        Ok(())
    }

    fn release_absent(&mut self, absent: &BTreeSet<DeviceKey>, observed: &[DeviceObservation]) {
        for key in absent {
            if !observed.iter().any(|observation| observation.key == *key) {
                self.suppressed.remove(key);
            }
        }
    }
}

fn validate_batch(observations: &[DeviceObservation]) -> Result<(), RegistryValidationError> {
    if observations.len() > MAX_RECORDS {
        return Err(RegistryValidationError::TooManyRecords);
    }
    if observations
        .windows(2)
        .all(|pair| pair[0].key < pair[1].key)
    {
        return Ok(());
    }
    let mut keys = BTreeSet::new();
    for observation in observations {
        if !keys.insert(observation.key) {
            return Err(RegistryValidationError::DuplicateIdentity);
        }
    }
    Ok(())
}

fn read_store(path: &Path) -> Result<DeviceRegistry, RegistryPersistenceError> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(DeviceRegistry::default());
        }
        Err(error) => {
            return Err(RegistryPersistenceError::Read {
                path: path.to_owned(),
                error: error.to_string(),
            });
        }
    };
    let invalid = |error: String| RegistryPersistenceError::Invalid {
        path: path.to_owned(),
        error,
    };
    let metadata = file
        .metadata()
        .map_err(|error| invalid(error.to_string()))?;
    if !metadata.is_file() {
        return Err(RegistryPersistenceError::NotRegularFile);
    }
    if metadata.len() > MAX_STORE_BYTES as u64 {
        return Err(RegistryPersistenceError::TooLarge);
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_STORE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| invalid(error.to_string()))?;
    if bytes.len() > MAX_STORE_BYTES {
        return Err(RegistryPersistenceError::TooLarge);
    }
    let document: StoreDocument =
        serde_json::from_slice(&bytes).map_err(|error| invalid(error.to_string()))?;
    if document.version != STORE_VERSION {
        return Err(RegistryPersistenceError::UnsupportedVersion {
            version: document.version,
        });
    }
    DeviceRegistry::from_records(document.records).map_err(|error| invalid(format!("{error:?}")))
}

fn save_error(path: &Path, error: impl std::fmt::Display) -> RegistryPersistenceError {
    RegistryPersistenceError::Save {
        path: path.to_owned(),
        error: error.to_string(),
    }
}

fn persist(path: &Path, bytes: &[u8]) -> Result<(), RegistryPersistenceError> {
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
