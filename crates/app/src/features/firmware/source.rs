//! Public upstream firmware acquisition. No files are executed or extracted to disk.
use std::sync::atomic::{AtomicBool, Ordering};

use crate::{
    features::firmware::remote_config::{GithubRepository, RemoteConfig},
    update::Progress,
};

mod catalog;
mod download;
mod http;
#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) use catalog::is_bootloader_update;
pub(crate) use catalog::variant;
use catalog::{latest_with, list_artifacts_with, list_versions_with};
use download::download_with;
use http::Network;

const MAX_DOWNLOAD: usize = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceChannel {
    Release,
    BootloaderRelease,
    BootloaderCi,
    TrackerCi,
    ReceiverCi,
}

impl SourceChannel {
    pub(crate) fn is_release(self) -> bool {
        matches!(self, Self::Release | Self::BootloaderRelease)
    }

    pub(crate) fn is_bootloader(self) -> bool {
        matches!(self, Self::BootloaderRelease | Self::BootloaderCi)
    }

    fn repo(self, config: &RemoteConfig) -> &GithubRepository {
        match self {
            Self::Release => &config.application.releases,
            Self::BootloaderRelease => &config.bootloader.releases,
            Self::BootloaderCi => &config.bootloader.ci,
            Self::TrackerCi => &config.application.tracker_ci,
            Self::ReceiverCi => &config.application.receiver_ci,
        }
    }

    fn api(self, config: &RemoteConfig) -> String {
        let repo = self.repo(config);
        format!("https://api.github.com/repos/{}/{}", repo.owner, repo.name)
    }
}

#[derive(Clone, Debug)]
pub struct Version {
    pub channel: SourceChannel,
    pub id: u64,
    pub name: String,
    pub published_at: String,
    pub commit: Option<String>,
    pub url: String,
}

#[derive(Clone, Debug)]
pub struct Artifact {
    pub name: String,
    /// A display hint, never a board compatibility assertion.
    pub variant: String,
    pub size: u64,
    pub version: Version,
    pub url: String,
    /// Digest of the downloaded asset/archive, not of an extracted member.
    pub sha256: Option<String>,
    pub id: u64,
}

#[derive(Clone, Debug)]
pub struct DownloadedFile {
    pub name: String,
    pub bytes: Vec<u8>,
    pub sha256: String,
    pub provenance: String,
}

/// Lists compatible releases or successful primary-branch firmware builds, newest first.
/// Application receiver releases share the configured application release repository.
/// Bootloader channels use their own repositories and have no application date/run threshold.
pub async fn list_versions(
    client: &reqwest::Client,
    config: &RemoteConfig,
    channel: SourceChannel,
    cancel: &AtomicBool,
    progress: &mut (dyn FnMut(Progress) + Send),
) -> Result<Vec<Version>, String> {
    list_versions_with(
        config,
        &Network::new(client.clone()),
        channel,
        cancel,
        progress,
    )
    .await
}

/// Latest means the newest successful version with an available firmware artifact.
/// In particular, an expired CI run never becomes a fabricated latest download.
pub async fn latest(
    client: &reqwest::Client,
    config: &RemoteConfig,
    channel: SourceChannel,
    cancel: &AtomicBool,
    progress: &mut (dyn FnMut(Progress) + Send),
) -> Result<Version, String> {
    latest_with(
        config,
        &Network::new(client.clone()),
        channel,
        cancel,
        progress,
    )
    .await
}

pub async fn list_artifacts(
    client: &reqwest::Client,
    config: &RemoteConfig,
    version: &Version,
    cancel: &AtomicBool,
    progress: &mut (dyn FnMut(Progress) + Send),
) -> Result<Vec<Artifact>, String> {
    list_artifacts_with(
        config,
        &Network::new(client.clone()),
        version,
        cancel,
        progress,
    )
    .await
}

/// Downloads a release image/package or all eligible files in a CI archive.
/// The caller must explicitly select a member and independently check target compatibility.
pub async fn download(
    client: &reqwest::Client,
    config: &RemoteConfig,
    artifact: &Artifact,
    cancel: &AtomicBool,
    progress: &mut (dyn FnMut(Progress) + Send),
) -> Result<Vec<DownloadedFile>, String> {
    download_with(
        config,
        &Network::new(client.clone()),
        artifact,
        cancel,
        progress,
    )
    .await
}

fn cancelled(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) {
        Err("Firmware acquisition cancelled".into())
    } else {
        Ok(())
    }
}
