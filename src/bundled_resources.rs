use serde::{Deserialize, Deserializer};
use std::path::{Path, PathBuf};

mod reading;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resource {
    File(PathBuf),
    Bundled(String),
}

// Configuration text always starts as a file reference. Only the embedded
// catalog's explicit conversion may turn it into a bundled resource.
impl<'de> Deserialize<'de> for Resource {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        PathBuf::deserialize(deserializer).map(Self::File)
    }
}

pub struct Member {
    name: &'static str,
    len: u64,
    sha256: [u8; 32],
}

static ARCHIVE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/firmware_bundle.zip"));
include!(concat!(env!("OUT_DIR"), "/firmware_bundle_index.rs"));

/// Enumerate actual archive members only, never configured-but-missing paths.
pub(crate) fn firmware_resources() -> impl Iterator<Item = Resource> {
    MEMBERS.iter().filter_map(|member| {
        member
            .name
            .strip_prefix("firmwares/")
            .filter(|filename| !filename.is_empty() && !filename.contains('/'))
            .map(|_| Resource::Bundled(member.name.to_owned()))
    })
}
impl Resource {
    pub fn bundled(name: &str) -> Result<Self, String> {
        canonical_member(name, MEMBERS).map(Self::Bundled)
    }

    pub fn filename(&self) -> Option<&str> {
        match self {
            Self::File(path) => path.file_name()?.to_str(),
            Self::Bundled(name) => name.rsplit('/').next(),
        }
    }

    pub fn is_empty(&self) -> bool {
        match self {
            Self::File(path) => path.as_os_str().is_empty(),
            Self::Bundled(name) => name.is_empty(),
        }
    }

    pub fn is_intentionally_omitted(&self) -> bool {
        !cfg!(feature = "embed-firmware")
            && matches!(self, Self::Bundled(name) if name.starts_with("firmwares/"))
    }

    pub fn resolve(&mut self, base: Option<&Path>) -> Result<(), String> {
        if let Self::File(path) = self {
            if let Some(base) = base {
                if path.is_relative() {
                    *path = base.join(&*path);
                }
            } else {
                *self = Self::bundled(path.to_str().ok_or("Invalid bundled resource filename")?)?;
            }
        }
        Ok(())
    }
}

fn canonical_member(name: &str, members: &[Member]) -> Result<String, String> {
    let normalized = name.replace('\\', "/");
    let normalized = normalized.strip_prefix("./").unwrap_or(&normalized);
    let firmware_filename = normalized
        .split_once('/')
        .and_then(|(directory, filename)| {
            (directory.eq_ignore_ascii_case("firmwares")
                && !matches!(filename, "" | "." | "..")
                && !filename.contains('/'))
            .then_some(filename)
        });
    if normalized.chars().any(|character| character.is_control())
        || normalized.contains(':')
        || !(normalized.eq_ignore_ascii_case("slimenrf-helper.yaml") || firmware_filename.is_some())
    {
        return Err(format!("Invalid bundled resource path: {name}"));
    }
    let mut matching = members
        .iter()
        .filter(|member| member.name.eq_ignore_ascii_case(normalized));
    let first = matching.next();
    if matching.next().is_some() {
        return Err(format!("Ambiguous bundled resource path: {name}"));
    }
    // Missing declared firmware stays visible in the catalog, but can never
    // fall back to a file on disk or a similarly named image.
    Ok(match first {
        Some(member) => member.name.to_owned(),
        None => match firmware_filename {
            Some(filename) => format!("firmwares/{filename}"),
            None => "slimenrf-helper.yaml".to_owned(),
        },
    })
}

#[cfg(test)]
mod tests;
