use std::{collections::HashSet, sync::atomic::AtomicBool};

use chrono::{Days, NaiveDate};
use reqwest::Url;
use serde::Deserialize;

use super::download::parse_digest;
use super::http::{Fetch, checked_url, json};
use super::{
    Artifact, BootloaderNamePolicy, MAX_DOWNLOAD, Progress, SourceChannel, Version, cancelled,
};
use crate::features::firmware::remote_config::RemoteConfig;

const MAX_PAGES: usize = 20;
const PAGE_SIZE: usize = 100;
const CI_EVENTS: [&str; 2] = ["push", "workflow_dispatch"];
// The default GitHub retention period is only a search window, not an expiry rule.
const RECENT_CI_DAYS: Days = Days::new(90);

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

impl Release {
    fn key(&self) -> (&str, u64) {
        (self.published_at.as_deref().unwrap_or_default(), self.id)
    }

    fn into_version(self, channel: SourceChannel) -> Version {
        Version {
            channel,
            id: self.id,
            name: if self.prerelease {
                format!("{} (prerelease)", self.tag_name)
            } else {
                self.tag_name
            },
            published_at: self.published_at.unwrap_or_default(),
            // target_commitish is frequently a branch, not a resolved commit SHA.
            commit: None,
            url: self.html_url,
        }
    }
}

#[derive(Deserialize)]
struct ReleaseAsset {
    id: u64,
    name: String,
    size: u64,
    browser_download_url: String,
    #[serde(default)]
    state: ReleaseAssetState,
    digest: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "lowercase")]
enum ReleaseAssetState {
    #[default]
    Uploaded,
    #[serde(other)]
    Unavailable,
}

impl ReleaseAsset {
    fn eligible(&self, channel: SourceChannel, policy: BootloaderNamePolicy) -> bool {
        matches!(self.state, ReleaseAssetState::Uploaded)
            && release_asset(channel, &self.name, policy)
    }
}

#[derive(Deserialize)]
struct Runs {
    total_count: Option<usize>,
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
    name.rsplit_once('.').is_some_and(|(_, suffix)| {
        ["uf2", "hex", "bin", "zip"]
            .iter()
            .any(|ext| suffix.eq_ignore_ascii_case(ext))
    })
}

/// Recognizes canonical updater names; it does not establish device compatibility.
pub(crate) fn is_bootloader_update(name: &str) -> bool {
    crate::features::firmware::board_catalog::bootloader_board(name).is_some()
}

pub(super) fn bootloader_candidate(name: &str, policy: BootloaderNamePolicy) -> bool {
    is_bootloader_update(name)
        || (policy == BootloaderNamePolicy::AllowUnclassified
            && crate::features::firmware::board_catalog::valid_filename(name)
            && name.len() <= 1024
            && !name.chars().any(char::is_control)
            && name
                .rsplit_once('.')
                .is_some_and(|(_, ext)| ext.eq_ignore_ascii_case("uf2")))
}

fn release_asset(channel: SourceChannel, name: &str, policy: BootloaderNamePolicy) -> bool {
    if channel == SourceChannel::BootloaderRelease {
        bootloader_candidate(name, policy)
    } else {
        is_image(name)
    }
}

pub(crate) fn variant(name: &str) -> String {
    crate::features::firmware::board_catalog::filename_variant(name).into()
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

fn version_key(version: &Version) -> (&str, u64) {
    (&version.published_at, version.id)
}

fn newest_first(versions: &mut [Version]) {
    versions.sort_by(|a, b| b.published_at.cmp(&a.published_at).then(b.id.cmp(&a.id)));
}

async fn visit_releases(
    config: &RemoteConfig,
    fetch: &impl Fetch,
    channel: SourceChannel,
    cancel: &AtomicBool,
    progress: &mut (dyn FnMut(Progress) + Send),
    mut visit: impl FnMut(Release) + Send,
) -> Result<(), String> {
    let api = channel.api(config);
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
            if !release.draft
                && (channel != SourceChannel::Release || compatible_release(&release.tag_name))
            {
                visit(release);
            }
        }
        if count < PAGE_SIZE {
            return Ok(());
        }
    }
    Err("Release listing exceeds safe pagination limit".into())
}

fn append_ci_versions(
    channel: SourceChannel,
    runs: Vec<Run>,
    event: Option<&str>,
    seen: &mut HashSet<u64>,
    result: &mut Vec<Version>,
) {
    let (branch, workflow, minimum) = ci_policy(channel);
    for run in runs {
        if run.run_number < minimum
            || run.status != "completed"
            || run.conclusion.as_deref() != Some("success")
            || run.head_branch.as_deref() != Some(branch)
            || !CI_EVENTS.contains(&run.event.as_str())
            || event.is_some_and(|event| run.event != event)
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
}

async fn ci_versions_by_event(
    config: &RemoteConfig,
    fetch: &impl Fetch,
    channel: SourceChannel,
    before: Option<NaiveDate>,
    cancel: &AtomicBool,
    progress: &mut (dyn FnMut(Progress) + Send),
) -> Result<Vec<Version>, String> {
    let api = channel.api(config);
    let (branch, _, _) = ci_policy(channel);
    let bound = before.map_or_else(String::new, |date| format!("&created=%3C{date}"));
    let mut result = Vec::new();
    let mut seen = HashSet::new();
    for event in CI_EVENTS {
        for page in 1..=MAX_PAGES {
            let runs: Runs = json(
                fetch,
                &format!(
                    "{api}/actions/runs?branch={branch}&event={event}&status=success{bound}&per_page={PAGE_SIZE}&page={page}"
                ),
                cancel,
                progress,
            )
            .await?;
            let count = runs.workflow_runs.len();
            append_ci_versions(
                channel,
                runs.workflow_runs,
                Some(event),
                &mut seen,
                &mut result,
            );
            if count < PAGE_SIZE {
                break;
            }
            if page == MAX_PAGES {
                return Err("CI listing exceeds safe pagination limit".into());
            }
        }
    }
    newest_first(&mut result);
    Ok(result)
}

pub(super) async fn list_versions_with(
    config: &RemoteConfig,
    fetch: &impl Fetch,
    channel: SourceChannel,
    name_policy: BootloaderNamePolicy,
    cancel: &AtomicBool,
    progress: &mut (dyn FnMut(Progress) + Send),
) -> Result<Vec<Version>, String> {
    cancelled(cancel)?;
    if !channel.is_release() {
        return ci_versions_by_event(config, fetch, channel, None, cancel, progress).await;
    }
    let mut result = Vec::new();
    visit_releases(config, fetch, channel, cancel, progress, |release| {
        if release
            .assets
            .iter()
            .any(|asset| asset.eligible(channel, name_policy))
        {
            result.push(release.into_version(channel));
        }
    })
    .await?;
    newest_first(&mut result);
    Ok(result)
}

pub(super) async fn latest_with(
    config: &RemoteConfig,
    fetch: &impl Fetch,
    channel: SourceChannel,
    name_policy: BootloaderNamePolicy,
    cancel: &AtomicBool,
    progress: &mut (dyn FnMut(Progress) + Send),
    today: impl FnOnce() -> NaiveDate + Send,
) -> Result<(Version, Vec<Artifact>), String> {
    cancelled(cancel)?;
    let result = if channel.is_release() {
        latest_release_with(config, fetch, channel, name_policy, cancel, progress).await?
    } else {
        latest_ci_with(
            config,
            fetch,
            channel,
            name_policy,
            today() - RECENT_CI_DAYS,
            cancel,
            progress,
        )
        .await?
    };
    result.ok_or_else(|| {
        "No compatible firmware version with unexpired artifacts within the 64 MiB download limit is available".into()
    })
}

async fn latest_release_with(
    config: &RemoteConfig,
    fetch: &impl Fetch,
    channel: SourceChannel,
    name_policy: BootloaderNamePolicy,
    cancel: &AtomicBool,
    progress: &mut (dyn FnMut(Progress) + Send),
) -> Result<Option<(Version, Vec<Artifact>)>, String> {
    let mut embedded: Option<Release> = None;
    let mut needs_listing = Vec::new();
    // The release list is not ordered by publication date. Keep the best
    // embedded directory and check possible truncation only for newer releases.
    visit_releases(config, fetch, channel, cancel, progress, |release| {
        if embedded
            .as_ref()
            .is_some_and(|best| release.key() <= best.key())
        {
            return;
        }
        if release.assets.len() >= PAGE_SIZE {
            needs_listing.push(release.into_version(channel));
        } else if release
            .assets
            .iter()
            .any(|asset| asset.size <= MAX_DOWNLOAD as u64 && asset.eligible(channel, name_policy))
        {
            embedded = Some(release);
        }
    })
    .await?;
    newest_first(&mut needs_listing);
    for version in needs_listing {
        if embedded
            .as_ref()
            .is_some_and(|best| version_key(&version) <= best.key())
        {
            break;
        }
        let artifacts =
            list_artifacts_with(config, fetch, &version, name_policy, cancel, progress).await?;
        if has_downloadable_artifact(&artifacts) {
            return Ok(Some((version, artifacts)));
        }
    }
    let Some(mut release) = embedded else {
        return Ok(None);
    };
    let assets = std::mem::take(&mut release.assets);
    let version = release.into_version(channel);
    let mut artifacts = Vec::new();
    append_release_artifacts(&mut artifacts, assets, &version, name_policy)?;
    Ok(Some((version, sorted_artifacts(artifacts))))
}

async fn latest_ci_with(
    config: &RemoteConfig,
    fetch: &impl Fetch,
    channel: SourceChannel,
    name_policy: BootloaderNamePolicy,
    since: NaiveDate,
    cancel: &AtomicBool,
    progress: &mut (dyn FnMut(Progress) + Send),
) -> Result<Option<(Version, Vec<Artifact>)>, String> {
    let api = channel.api(config);
    let (branch, _, _) = ci_policy(channel);
    let recent: Runs = json(
        fetch,
        &format!(
            "{api}/actions/runs?branch={branch}&status=success&created=%3E%3D{since}&per_page={PAGE_SIZE}&page=1"
        ),
        cancel,
        progress,
    )
    .await?;
    // Only a complete window can certify the newest run without relying on API
    // order. The event-specific paginator covers larger or uncounted windows.
    let count = recent.workflow_runs.len();
    let before = if count <= PAGE_SIZE && recent.total_count == Some(count) {
        let mut versions = Vec::new();
        append_ci_versions(
            channel,
            recent.workflow_runs,
            None,
            &mut HashSet::new(),
            &mut versions,
        );
        newest_first(&mut versions);
        if let Some(result) =
            latest_ci_candidate(config, fetch, versions, name_policy, cancel, progress).await?
        {
            return Ok(Some(result));
        }
        Some(since)
    } else {
        None
    };
    let versions = ci_versions_by_event(config, fetch, channel, before, cancel, progress).await?;
    latest_ci_candidate(config, fetch, versions, name_policy, cancel, progress).await
}

async fn latest_ci_candidate(
    config: &RemoteConfig,
    fetch: &impl Fetch,
    versions: Vec<Version>,
    name_policy: BootloaderNamePolicy,
    cancel: &AtomicBool,
    progress: &mut (dyn FnMut(Progress) + Send),
) -> Result<Option<(Version, Vec<Artifact>)>, String> {
    for version in versions {
        let artifacts =
            list_artifacts_with(config, fetch, &version, name_policy, cancel, progress).await?;
        if has_downloadable_artifact(&artifacts) {
            return Ok(Some((version, artifacts)));
        }
    }
    Ok(None)
}

fn has_downloadable_artifact(artifacts: &[Artifact]) -> bool {
    artifacts
        .iter()
        .any(|artifact| artifact.size <= MAX_DOWNLOAD as u64)
}

fn append_release_artifacts(
    result: &mut Vec<Artifact>,
    assets: Vec<ReleaseAsset>,
    version: &Version,
    name_policy: BootloaderNamePolicy,
) -> Result<(), String> {
    for asset in assets {
        if !asset.eligible(version.channel, name_policy) {
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
    Ok(())
}

fn sorted_artifacts(mut artifacts: Vec<Artifact>) -> Vec<Artifact> {
    artifacts.sort_by(|a, b| a.name.cmp(&b.name));
    artifacts
}

pub(super) async fn list_artifacts_with(
    config: &RemoteConfig,
    fetch: &impl Fetch,
    version: &Version,
    name_policy: BootloaderNamePolicy,
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
            append_release_artifacts(&mut result, assets, version, name_policy)?;
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
    Ok(sorted_artifacts(result))
}
