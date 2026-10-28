//! GitHub release checks and verified portable application downloads.
mod http;
mod release;
#[cfg(windows)]
pub(crate) mod windows;

pub(crate) use release::{Channel, CheckResult, Release, ReleaseInfo};

use std::{sync::Arc, sync::atomic::AtomicBool};

#[cfg(test)]
use release::Asset;
use semver::Version;
use tempfile::NamedTempFile;

use crate::{
    app_runtime::Executor,
    update::{Progress, ProgressReporter},
};
use http::{Network, cancelled};
use release::{ApiAsset, ApiRelease, Target, classify, make_release, revalidate};

const REPOSITORY_API: &str = "https://api.github.com/repos/jitingcn/SlimeNRF-Helper";
const PER_PAGE: usize = 100;
const MAX_PAGES: usize = 10;
const MAX_CANDIDATE_ASSET_LOOKUPS: usize = 64;

#[derive(Clone, Copy)]
enum CheckKind {
    Portable(Target),
    ReleasePage,
}

fn portable_installation_enabled() -> bool {
    #[cfg(windows)]
    {
        windows::install_eligible()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

fn current_check_kind() -> CheckKind {
    if portable_installation_enabled()
        && let Ok(target) = Target::current()
    {
        return CheckKind::Portable(target);
    }
    CheckKind::ReleasePage
}

/// Whether the running build and target can install a Windows portable update.
pub(crate) fn installable_now() -> bool {
    matches!(current_check_kind(), CheckKind::Portable(_))
}

#[cfg(test)]
mod tests;

/// Owns the verified temporary file; dropping it removes the download.
#[cfg_attr(
    not(windows),
    allow(
        dead_code,
        reason = "Only the Windows installer consumes downloaded files"
    )
)]
#[derive(Debug)]
pub(crate) struct DownloadedUpdate {
    pub(crate) release: Release,
    pub(crate) file: NamedTempFile,
}

/// Checks GitHub releases, requiring an exact asset only for install-capable builds.
pub(crate) async fn check(
    executor: Executor,
    channel: Channel,
    cancel: Arc<AtomicBool>,
    progress: ProgressReporter,
) -> Result<CheckResult, String> {
    cancelled(&cancel)?;
    let current = Version::parse(env!("CARGO_PKG_VERSION"))
        .map_err(|_| "The installed application version is not a valid SemVer version")?;
    let kind = current_check_kind();
    progress.emit(Progress::stage("Checking app releases"));
    check_with(
        &Network::new(executor.http_client()),
        channel,
        kind,
        &current,
        cancel,
    )
    .await
}

async fn check_with(
    network: &Network,
    channel: Channel,
    kind: CheckKind,
    current: &Version,
    cancel: Arc<AtomicBool>,
) -> Result<CheckResult, String> {
    let mut eligible = false;
    let mut candidates = Vec::new();
    let mut complete = false;
    for page in 1..=MAX_PAGES {
        let url = format!("{REPOSITORY_API}/releases?per_page={PER_PAGE}&page={page}");
        let releases: Vec<ApiRelease> = network.json(&url, Arc::clone(&cancel)).await?;
        let count = releases.len();
        if count > PER_PAGE {
            return Err("GitHub returned too many app releases on one page".into());
        }
        for release in releases {
            let (admitted, candidate) = classify(release, channel, current);
            eligible |= admitted;
            if let Some(candidate) = candidate {
                candidates.push(candidate);
            }
        }
        if count < PER_PAGE {
            complete = true;
            break;
        }
    }
    if !complete {
        return Err("GitHub app release history exceeds the safe page limit".into());
    }
    if candidates.is_empty() {
        return Ok(if eligible {
            CheckResult::UpToDate
        } else {
            CheckResult::NoEligibleRelease
        });
    }
    let target = match kind {
        CheckKind::ReleasePage => {
            cancelled(&cancel)?;
            let newest = candidates
                .into_iter()
                .max_by(|left, right| {
                    left.version
                        .cmp_precedence(&right.version)
                        .then_with(|| left.id.cmp(&right.id))
                })
                .expect("a nonempty candidate list has a newest release");
            return Ok(CheckResult::ReleasePage(Box::new(newest.into_info())));
        }
        CheckKind::Portable(target) => target,
    };
    candidates.sort_by(|left, right| {
        right
            .version
            .cmp_precedence(&left.version)
            .then_with(|| right.id.cmp(&left.id))
    });
    for (index, candidate) in candidates.into_iter().enumerate() {
        if index == MAX_CANDIDATE_ASSET_LOOKUPS {
            return Err("Too many newer app releases to check safely; try again later".into());
        }
        cancelled(&cancel)?;
        let assets: Vec<ApiAsset> = assets(network, candidate.id, Arc::clone(&cancel)).await?;
        if let Some(release) = make_release(candidate, assets, target)? {
            return Ok(CheckResult::Available(Box::new(release)));
        }
    }
    Ok(CheckResult::NoCompatibleRelease)
}

async fn assets(
    network: &Network,
    release_id: u64,
    cancel: Arc<AtomicBool>,
) -> Result<Vec<ApiAsset>, String> {
    let mut assets = Vec::new();
    for page in 1..=MAX_PAGES {
        let url = format!(
            "{REPOSITORY_API}/releases/{release_id}/assets?per_page={PER_PAGE}&page={page}"
        );
        let values: Vec<ApiAsset> = network.json(&url, Arc::clone(&cancel)).await?;
        let count = values.len();
        if count > PER_PAGE {
            return Err("GitHub returned too many app assets on one page".into());
        }
        assets.extend(values);
        if count < PER_PAGE {
            return Ok(assets);
        }
    }
    Err("GitHub app release assets exceed the safe page limit".into())
}

/// Revalidates the frozen release and streams its verified payload into a temporary file.
pub(crate) async fn download(
    executor: Executor,
    release: Release,
    cancel: Arc<AtomicBool>,
    progress: ProgressReporter,
) -> Result<DownloadedUpdate, String> {
    let CheckKind::Portable(target) = current_check_kind() else {
        return Err("This build can check for updates but cannot download or install them".into());
    };
    cancelled(&cancel)?;
    progress.emit(Progress::stage("Rechecking app release"));
    download_with(
        &Network::new(executor.http_client()),
        release,
        target,
        cancel,
        move |completed, total| {
            progress.emit(Progress::bytes("Downloading app update", completed, total));
        },
    )
    .await
}

async fn download_with(
    network: &Network,
    release: Release,
    target: Target,
    cancel: Arc<AtomicBool>,
    progress: impl FnMut(u64, u64),
) -> Result<DownloadedUpdate, String> {
    cancelled(&cancel)?;
    let url = format!("{REPOSITORY_API}/releases/{}", release.id);
    let current: ApiRelease = network.json(&url, Arc::clone(&cancel)).await?;
    let current_assets = assets(network, release.id, Arc::clone(&cancel)).await?;
    revalidate(&release, current, current_assets, target)?;
    cancelled(&cancel)?;
    let file = network
        .download(&release.asset, Arc::clone(&cancel), progress)
        .await?;
    cancelled(&cancel)?;
    Ok(DownloadedUpdate { release, file })
}

#[cfg(test)]
pub(crate) fn test_release() -> Release {
    Release {
        version: "9.0.0".into(),
        tag: "v9.0.0".into(),
        prerelease: false,
        notes: "Synthetic release notes".into(),
        html_url: "https://github.com/jitingcn/SlimeNRF-Helper/releases/tag/v9.0.0".into(),
        asset: Asset {
            id: 2,
            name: "slimenrf-helper-9.0.0-windows-x64-no-firmware.exe".into(),
            url: format!("{REPOSITORY_API}/releases/assets/2"),
            size: 3,
            sha256: [0; 32],
        },
        id: 1,
        github_prerelease: false,
    }
}

#[cfg(test)]
pub(crate) fn test_release_info() -> ReleaseInfo {
    let release = test_release();
    ReleaseInfo {
        version: release.version,
        tag: release.tag,
        prerelease: release.prerelease,
        notes: release.notes,
        html_url: release.html_url,
    }
}

#[cfg(test)]
pub(crate) fn test_downloaded_update() -> DownloadedUpdate {
    use std::io::Write;
    let mut file = NamedTempFile::new().expect("create synthetic app update fixture");
    file.write_all(b"abc")
        .expect("write synthetic app update fixture");
    DownloadedUpdate {
        release: test_release(),
        file,
    }
}
