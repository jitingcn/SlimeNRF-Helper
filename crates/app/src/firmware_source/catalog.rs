use std::{collections::HashSet, sync::atomic::AtomicBool};

use reqwest::Url;
use serde::Deserialize;

use super::download::parse_digest;
use super::http::{Fetch, checked_url, json};
use super::{Artifact, MAX_DOWNLOAD, Progress, SourceChannel, Version, cancelled};
use crate::remote_config::RemoteConfig;

const MAX_PAGES: usize = 20;
const PAGE_SIZE: usize = 100;

#[derive(Deserialize)]
struct Release {
    id: u64,
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    published_at: Option<String>,
    html_url: String,
    #[serde(default)]
    assets: Vec<ReleaseAsset>,
}

#[derive(Deserialize)]
struct ReleaseAsset {
    id: u64,
    name: String,
    size: u64,
    browser_download_url: String,
    digest: Option<String>,
}

#[derive(Deserialize)]
struct Runs {
    workflow_runs: Vec<Run>,
}

#[derive(Deserialize)]
struct Run {
    id: u64,
    run_number: u64,
    display_title: String,
    created_at: String,
    head_sha: String,
    head_branch: Option<String>,
    event: String,
    status: String,
    conclusion: Option<String>,
    path: String,
    html_url: String,
}

#[derive(Deserialize)]
struct Artifacts {
    artifacts: Vec<CiArtifact>,
}

#[derive(Deserialize)]
struct CiArtifact {
    id: u64,
    name: String,
    size_in_bytes: u64,
    expired: bool,
    digest: Option<String>,
}

pub(super) fn compatible_release(tag: &str) -> bool {
    tag.get(..6)
        .and_then(|s| s.parse::<u32>().ok())
        .is_some_and(|n| n >= 260527)
}

pub(super) fn is_image(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [".uf2", ".hex", ".bin", ".zip"]
        .iter()
        .any(|ext| lower.ends_with(ext))
}

/// Only self-update UF2 assets are eligible for bootloader acquisition.
/// Full HEX images and DFU packages belong to other installation workflows.
pub(crate) fn is_bootloader_update(name: &str) -> bool {
    crate::board_catalog::bootloader_board(name).is_some()
}

fn release_asset(channel: SourceChannel, name: &str) -> bool {
    if channel == SourceChannel::BootloaderRelease {
        is_bootloader_update(name)
    } else {
        is_image(name)
    }
}

pub(crate) fn variant(name: &str) -> String {
    crate::board_catalog::filename_variant(name).into()
}

fn ci_policy(channel: SourceChannel) -> (&'static str, &'static str, u64) {
    match channel {
        SourceChannel::TrackerCi => ("dev", ".github/workflows/workflow.yml", 191),
        SourceChannel::ReceiverCi => ("dev", ".github/workflows/workflow.yml", 68),
        SourceChannel::BootloaderCi => ("master", ".github/workflows/githubci.yml", 0),
        SourceChannel::Release | SourceChannel::BootloaderRelease => {
            unreachable!("release channels do not list CI runs")
        }
    }
}

pub(super) async fn list_versions_with(
    config: &RemoteConfig,
    fetch: &impl Fetch,
    channel: SourceChannel,
    cancel: &AtomicBool,
    progress: &mut (dyn FnMut(Progress) + Send),
) -> Result<Vec<Version>, String> {
    cancelled(cancel)?;
    let api = channel.api(config);
    let mut result = Vec::new();
    if channel.is_release() {
        for page in 1..=MAX_PAGES {
            let releases: Vec<Release> = json(
                fetch,
                &format!("{api}/releases?per_page={PAGE_SIZE}&page={page}"),
                cancel,
                progress,
            )
            .await?;
            let count = releases.len();
            for release in releases {
                if release.draft
                    || (channel == SourceChannel::Release && !compatible_release(&release.tag_name))
                    || !release
                        .assets
                        .iter()
                        .any(|asset| release_asset(channel, &asset.name))
                {
                    continue;
                }
                result.push(Version {
                    channel,
                    id: release.id,
                    name: if release.prerelease {
                        format!("{} (prerelease)", release.tag_name)
                    } else {
                        release.tag_name
                    },
                    published_at: release.published_at.unwrap_or_default(),
                    // target_commitish is frequently a branch, not a resolved commit SHA.
                    commit: None,
                    url: release.html_url,
                });
            }
            if count < PAGE_SIZE {
                break;
            }
            if page == MAX_PAGES {
                return Err("Release listing exceeds safe pagination limit".into());
            }
        }
    } else {
        let (branch, workflow, minimum) = ci_policy(channel);
        let mut seen = HashSet::new();
        for event in ["push", "workflow_dispatch"] {
            for page in 1..=MAX_PAGES {
                let runs: Runs = json(
                    fetch,
                    &format!(
                        "{api}/actions/runs?branch={branch}&event={event}&status=success&per_page={PAGE_SIZE}&page={page}"
                    ),
                    cancel,
                    progress,
                ).await?;
                let count = runs.workflow_runs.len();
                for run in runs.workflow_runs {
                    if run.run_number < minimum
                        || run.status != "completed"
                        || run.conclusion.as_deref() != Some("success")
                        || run.head_branch.as_deref() != Some(branch)
                        || run.event != event
                        || run.path != workflow
                        || !seen.insert(run.id)
                    {
                        continue;
                    }
                    result.push(Version {
                        channel,
                        id: run.id,
                        name: format!("#{} {}", run.run_number, run.display_title),
                        published_at: run.created_at,
                        commit: Some(run.head_sha),
                        url: run.html_url,
                    });
                }
                if count < PAGE_SIZE {
                    break;
                }
                if page == MAX_PAGES {
                    return Err("CI listing exceeds safe pagination limit".into());
                }
            }
        }
    }
    result.sort_by(|a, b| b.published_at.cmp(&a.published_at).then(b.id.cmp(&a.id)));
    Ok(result)
}

pub(super) async fn latest_with(
    config: &RemoteConfig,
    fetch: &impl Fetch,
    channel: SourceChannel,
    cancel: &AtomicBool,
    progress: &mut (dyn FnMut(Progress) + Send),
) -> Result<Version, String> {
    for version in list_versions_with(config, fetch, channel, cancel, progress).await? {
        if list_artifacts_with(config, fetch, &version, cancel, progress)
            .await?
            .iter()
            .any(|artifact| artifact.size <= MAX_DOWNLOAD as u64)
        {
            return Ok(version);
        }
    }
    Err("No compatible firmware version with unexpired artifacts within the 64 MiB download limit is available".into())
}

pub(super) async fn list_artifacts_with(
    config: &RemoteConfig,
    fetch: &impl Fetch,
    version: &Version,
    cancel: &AtomicBool,
    progress: &mut (dyn FnMut(Progress) + Send),
) -> Result<Vec<Artifact>, String> {
    cancelled(cancel)?;
    let api = version.channel.api(config);
    let mut result = Vec::new();
    for page in 1..=MAX_PAGES {
        let count = if version.channel.is_release() {
            let assets: Vec<ReleaseAsset> = json(
                fetch,
                &format!(
                    "{api}/releases/{}/assets?per_page={PAGE_SIZE}&page={page}",
                    version.id
                ),
                cancel,
                progress,
            )
            .await?;
            let count = assets.len();
            for asset in assets {
                if !release_asset(version.channel, &asset.name) {
                    continue;
                }
                checked_url(&asset.browser_download_url)?;
                result.push(Artifact {
                    variant: variant(&asset.name),
                    name: asset.name,
                    size: asset.size,
                    version: version.clone(),
                    url: asset.browser_download_url,
                    sha256: parse_digest(asset.digest.as_deref())?,
                    id: asset.id,
                });
            }
            count
        } else {
            let artifacts: Artifacts = json(
                fetch,
                &format!(
                    "{api}/actions/runs/{}/artifacts?per_page={PAGE_SIZE}&page={page}",
                    version.id
                ),
                cancel,
                progress,
            )
            .await?;
            let count = artifacts.artifacts.len();
            for artifact in artifacts.artifacts {
                if artifact.expired
                    || (version.channel == SourceChannel::TrackerCi
                        && artifact.name.to_ascii_lowercase().contains("receiver"))
                {
                    continue;
                }
                // Use a URL path segment, not interpolation: artifact names may contain spaces or '#'.
                let repo = version.channel.repo(config);
                let mut url = Url::parse(&format!(
                    "https://nightly.link/{}/{}/actions/runs/{}/",
                    repo.owner, repo.name, version.id
                ))
                .map_err(|_| "Invalid CI source URL")?;
                url.path_segments_mut()
                    .map_err(|_| "Invalid CI URL path")?
                    .pop_if_empty()
                    .push(&format!("{}.zip", artifact.name));
                result.push(Artifact {
                    variant: if version.channel == SourceChannel::BootloaderCi {
                        "Bootloader".into()
                    } else {
                        variant(&artifact.name)
                    },
                    name: artifact.name,
                    size: artifact.size_in_bytes,
                    version: version.clone(),
                    url: url.into(),
                    sha256: parse_digest(artifact.digest.as_deref())?,
                    id: artifact.id,
                });
            }
            count
        };
        if count < PAGE_SIZE {
            break;
        }
        if page == MAX_PAGES {
            return Err("Artifact listing exceeds safe pagination limit".into());
        }
    }
    result.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(result)
}
