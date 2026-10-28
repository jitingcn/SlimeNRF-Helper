use std::cmp::Ordering;

use semver::Version;
use serde::Deserialize;

/// Stable releases exclude both GitHub prereleases and SemVer prerelease suffixes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Channel {
    Stable,
    Preview,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CheckResult {
    Available(Box<Release>),
    ReleasePage(Box<ReleaseInfo>),
    UpToDate,
    NoCompatibleRelease,
    NoEligibleRelease,
}

/// Public release metadata carries no authority to download or install an asset.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReleaseInfo {
    pub(crate) version: String,
    pub(crate) tag: String,
    pub(crate) prerelease: bool,
    pub(crate) notes: String,
    pub(crate) html_url: String,
}

/// A release and its exact asset are frozen together at check time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Release {
    pub(crate) version: String,
    pub(crate) tag: String,
    pub(crate) prerelease: bool,
    pub(crate) notes: String,
    pub(crate) html_url: String,
    pub(crate) asset: Asset,
    pub(super) id: u64,
    pub(super) github_prerelease: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Asset {
    pub(crate) id: u64,
    pub(crate) name: String,
    pub(crate) url: String,
    pub(crate) size: u64,
    pub(crate) sha256: [u8; 32],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Architecture {
    X64,
    Arm64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Variant {
    Full,
    NoFirmware,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Target {
    pub(super) arch: Architecture,
    pub(super) variant: Variant,
}

impl Target {
    pub(super) fn current() -> Result<Self, String> {
        let arch = match std::env::consts::ARCH {
            "x86_64" => Architecture::X64,
            "aarch64" => Architecture::Arm64,
            _ => return Err("This processor architecture has no Windows portable update".into()),
        };
        let variant = if cfg!(feature = "embed-firmware") {
            Variant::Full
        } else {
            Variant::NoFirmware
        };
        Ok(Self { arch, variant })
    }

    fn asset_name(self, version: &Version) -> String {
        let arch = match self.arch {
            Architecture::X64 => "x64",
            Architecture::Arm64 => "arm64",
        };
        let variant = match self.variant {
            Variant::Full => "",
            Variant::NoFirmware => "-no-firmware",
        };
        format!("slimenrf-helper-{version}-windows-{arch}{variant}.exe")
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct ApiRelease {
    pub(super) id: u64,
    pub(super) tag_name: String,
    pub(super) draft: bool,
    pub(super) prerelease: bool,
    #[serde(default)]
    pub(super) body: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct ApiAsset {
    pub(super) id: u64,
    pub(super) name: String,
    pub(super) state: String,
    pub(super) size: u64,
    #[serde(default)]
    pub(super) digest: Option<String>,
}

#[derive(Debug)]
pub(super) struct Candidate {
    pub(super) id: u64,
    pub(super) version: Version,
    pub(super) tag: String,
    pub(super) prerelease: bool,
    github_prerelease: bool,
    notes: String,
}

impl Candidate {
    pub(super) fn into_info(self) -> ReleaseInfo {
        ReleaseInfo {
            version: self.version.to_string(),
            html_url: release_page_url(&self.tag),
            tag: self.tag,
            prerelease: self.prerelease,
            notes: self.notes,
        }
    }
}

fn release_page_url(tag: &str) -> String {
    format!("https://github.com/jitingcn/SlimeNRF-Helper/releases/tag/{tag}")
}

/// Limit accepted payloads independently of any upstream Content-Length.
pub(super) const MAX_ASSET_SIZE: u64 = 512 * 1024 * 1024;
const MAX_NOTES: usize = 64 * 1024;
const MAX_TAG_BYTES: usize = 256;

pub(super) fn eligible_version(release: &ApiRelease, channel: Channel) -> Option<Version> {
    if release.draft || release.id == 0 || release.tag_name.len() > MAX_TAG_BYTES {
        return None;
    }
    let raw = release
        .tag_name
        .strip_prefix('v')
        .unwrap_or(&release.tag_name);
    let version = Version::parse(raw).ok()?;
    if channel == Channel::Stable && (release.prerelease || !version.pre.is_empty()) {
        return None;
    }
    Some(version)
}

pub(super) fn classify(
    release: ApiRelease,
    channel: Channel,
    current: &Version,
) -> (bool, Option<Candidate>) {
    let Some(version) = eligible_version(&release, channel) else {
        return (false, None);
    };
    if version.cmp_precedence(current) != Ordering::Greater {
        return (true, None);
    }
    let mut notes = release.body.unwrap_or_default();
    if notes.len() > MAX_NOTES {
        let mut end = MAX_NOTES;
        while !notes.is_char_boundary(end) {
            end -= 1;
        }
        notes.truncate(end);
    }
    (
        true,
        Some(Candidate {
            id: release.id,
            prerelease: release.prerelease || !version.pre.is_empty(),
            github_prerelease: release.prerelease,
            version,
            tag: release.tag_name,
            notes,
        }),
    )
}

#[cfg(test)]
fn candidate(release: ApiRelease, channel: Channel, current: &Version) -> Option<Candidate> {
    classify(release, channel, current).1
}

fn parse_digest(digest: Option<&str>) -> Result<[u8; 32], String> {
    let hex = digest
        .and_then(|value| value.strip_prefix("sha256:"))
        .filter(|value| value.len() == 64)
        .ok_or("Release asset has no valid GitHub SHA-256 digest")?;
    let mut parsed = [0u8; 32];
    for (index, pair) in hex.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        let nibble = |value: u8| match value {
            b'0'..=b'9' => Some(value - b'0'),
            b'a'..=b'f' => Some(value - b'a' + 10),
            b'A'..=b'F' => Some(value - b'A' + 10),
            _ => None,
        };
        parsed[index] = (nibble(pair[0]).ok_or("Release asset has an invalid SHA-256 digest")?
            << 4)
            | nibble(pair[1]).ok_or("Release asset has an invalid SHA-256 digest")?;
    }
    Ok(parsed)
}

pub(super) fn make_release(
    candidate: Candidate,
    assets: Vec<ApiAsset>,
    target: Target,
) -> Result<Option<Release>, String> {
    let expected = target.asset_name(&candidate.version);
    let mut matches = assets.into_iter().filter(|asset| asset.name == expected);
    let Some(asset) = matches.next() else {
        return Ok(None);
    };
    if matches.next().is_some() {
        return Err("Release contains multiple assets with the expected portable filename".into());
    }
    if asset.id == 0 || asset.state != "uploaded" {
        return Err("The expected release asset is not fully uploaded or has an invalid ID".into());
    }
    if asset.size == 0 || asset.size > MAX_ASSET_SIZE {
        return Err("The expected release asset has an invalid or excessive size".into());
    }
    let sha256 = parse_digest(asset.digest.as_deref())?;
    let asset = Asset {
        id: asset.id,
        name: asset.name,
        url: format!(
            "https://api.github.com/repos/jitingcn/SlimeNRF-Helper/releases/assets/{}",
            asset.id
        ),
        size: asset.size,
        sha256,
    };
    Ok(Some(Release {
        version: candidate.version.to_string(),
        html_url: release_page_url(&candidate.tag),
        tag: candidate.tag,
        prerelease: candidate.prerelease,
        notes: candidate.notes,
        asset,
        id: candidate.id,
        github_prerelease: candidate.github_prerelease,
    }))
}

pub(super) fn revalidate(
    selected: &Release,
    release: ApiRelease,
    assets: Vec<ApiAsset>,
    target: Target,
) -> Result<(), String> {
    if release.id != selected.id
        || release.tag_name != selected.tag
        || release.prerelease != selected.github_prerelease
    {
        return Err("The selected release changed; check for updates again".into());
    }
    let version = eligible_version(&release, Channel::Preview)
        .ok_or("The selected release is no longer published; check for updates again")?;
    if version.to_string() != selected.version {
        return Err("The selected release version changed; check for updates again".into());
    }
    let candidate = Candidate {
        id: release.id,
        prerelease: release.prerelease || !version.pre.is_empty(),
        github_prerelease: release.prerelease,
        version,
        tag: release.tag_name,
        notes: String::new(),
    };
    let refreshed = make_release(candidate, assets, target)?
        .ok_or("The selected release asset is no longer available; check for updates again")?;
    if refreshed.asset != selected.asset {
        return Err("The selected release asset changed; check for updates again".into());
    }
    Ok(())
}

#[cfg(test)]
#[path = "release_tests.rs"]
mod tests;
