use std::{collections::HashSet, io::Cursor, sync::atomic::AtomicBool};

use sha2::{Digest, Sha256};

use super::catalog::{is_bootloader_update, is_image};
use super::http::{Fetch, blocking_work, checked_url, consume_bounded, read_bounded};
use super::{Artifact, DownloadedFile, MAX_DOWNLOAD, Progress, SourceChannel, cancelled};
use crate::remote_config::RemoteConfig;

pub(super) const MAX_MEMBER: usize = 32 * 1024 * 1024;
const MAX_EXPANDED: u64 = 128 * 1024 * 1024;
const MAX_MEMBERS: usize = 1024;

pub(super) fn parse_digest(digest: Option<&str>) -> Result<Option<String>, String> {
    let Some(digest) = digest else {
        return Ok(None);
    };
    let raw = digest.strip_prefix("sha256:").unwrap_or(digest);
    if raw.len() != 64 || !raw.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("Upstream supplied an unsupported or malformed SHA-256 digest".into());
    }
    Ok(Some(raw.to_ascii_lowercase()))
}

pub(super) fn hash(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(bytes);
    let mut text = String::with_capacity(digest.len() * 2);
    for byte in digest {
        text.push(HEX[(byte >> 4) as usize] as char);
        text.push(HEX[(byte & 0x0f) as usize] as char);
    }
    text
}

pub(super) async fn download_with(
    config: &RemoteConfig,
    fetch: &impl Fetch,
    artifact: &Artifact,
    cancel: &AtomicBool,
    progress: &mut (dyn FnMut(Progress) + Send),
) -> Result<Vec<DownloadedFile>, String> {
    cancelled(cancel)?;
    if artifact.version.channel == SourceChannel::BootloaderRelease
        && !is_bootloader_update(&artifact.name)
    {
        return Err("Bootloader releases require an update-*bootloader*.uf2 asset".into());
    }
    if artifact.size > MAX_DOWNLOAD as u64 {
        return Err("Firmware artifact exceeds the 64 MiB download limit".into());
    }
    checked_url(&artifact.url)?;
    let is_ci = !artifact.version.channel.is_release();
    let (bytes, acquisition) = match fetch
        .fetch(&artifact.url, MAX_DOWNLOAD, cancel, progress)
        .await
    {
        Ok(bytes) => (bytes, artifact.url.clone()),
        Err(public_error) if is_ci && fetch.has_token() => {
            cancelled(cancel)?;
            let api_url = format!(
                "{}/actions/artifacts/{}/zip",
                artifact.version.channel.api(config),
                artifact.id
            );
            let bytes = fetch.fetch(&api_url, MAX_DOWNLOAD, cancel, progress).await
                .map_err(|error| format!("Public artifact download failed: {public_error}. GitHub authenticated fallback failed: {error}"))?;
            (bytes, api_url)
        }
        Err(error) => return Err(error),
    };
    let artifact = artifact.clone();
    blocking_work(cancel, progress, move |cancel, progress| {
        finish_download(bytes, &artifact, &acquisition, is_ci, cancel, progress)
    })
    .await
}

fn finish_download(
    bytes: Vec<u8>,
    artifact: &Artifact,
    acquisition: &str,
    is_ci: bool,
    cancel: &AtomicBool,
    progress: &mut (dyn FnMut(Progress) + Send),
) -> Result<Vec<DownloadedFile>, String> {
    cancelled(cancel)?;
    let digest = hash(&bytes);
    let expected = parse_digest(artifact.sha256.as_deref())?;
    if expected
        .as_deref()
        .is_some_and(|expected| expected != digest)
    {
        return Err(
            "Firmware SHA-256 mismatch; downloaded bytes do not match the upstream artifact".into(),
        );
    }
    // GitHub's CI size_in_bytes is informational, not guaranteed ZIP transport length.
    if !is_ci && artifact.size != bytes.len() as u64 {
        return Err("Release asset size differs from the published metadata".into());
    }
    let provenance = format!(
        "{} | {} | {} | {} | SHA-256 {} ({})",
        artifact.version.url,
        artifact.version.name,
        artifact
            .version
            .commit
            .as_deref()
            .unwrap_or("release asset"),
        acquisition,
        digest,
        if expected.is_some() {
            "matches upstream digest"
        } else {
            "computed locally; no upstream digest supplied"
        },
    );
    if is_ci {
        extract_archive(
            &bytes,
            &provenance,
            artifact.version.channel,
            cancel,
            progress,
        )
    } else {
        safe_member_name(&artifact.name)?;
        if bytes.is_empty() {
            return Err("Firmware asset is empty".into());
        }
        if artifact.name.to_ascii_lowercase().ends_with(".zip") {
            bounded_archive(&bytes, cancel)?;
        }
        Ok(vec![DownloadedFile {
            name: artifact.name.clone(),
            bytes,
            sha256: digest,
            provenance,
        }])
    }
}

fn safe_member_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name.len() > 1024
        || name.starts_with('/')
        || name.contains(['\\', ':'])
        || name.chars().any(|c| c.is_control())
        || name
            .trim_end_matches('/')
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err("Firmware archive contains an unsafe member path".into());
    }
    Ok(())
}

/// Validate firmware archive bounds before allocating ZIP member metadata.
pub(super) fn bounded_archive<'a>(
    bytes: &'a [u8],
    cancel: &AtomicBool,
) -> Result<zip::ZipArchive<Cursor<&'a [u8]>>, String> {
    cancelled(cancel)?;
    let mut archive = slimenrf_firmware_image::zip_envelope::open(
        bytes,
        slimenrf_firmware_image::zip_envelope::Limits {
            bytes: MAX_DOWNLOAD,
            members: MAX_MEMBERS,
            directory: 4 * 1024 * 1024,
        },
    )
    .map_err(String::from)?;
    let mut names = HashSet::new();
    let mut expanded = 0u64;
    for index in 0..archive.len() {
        cancelled(cancel)?;
        let file = archive
            .by_index(index)
            .map_err(|e| format!("Cannot read firmware ZIP member: {e}"))?;
        let name = file
            .name()
            .map_err(|e| format!("Cannot decode firmware ZIP member name: {e}"))?;
        safe_member_name(&name)?;
        if !names.insert(name.to_lowercase()) {
            return Err("Firmware ZIP contains duplicate member names".into());
        }
        if let Some(mode) = file.unix_mode() {
            let kind = mode & 0o170000;
            if kind != 0 && kind != 0o100000 && kind != 0o040000 {
                return Err("Firmware ZIP contains a link or special file".into());
            }
        }
        expanded = expanded
            .checked_add(file.size())
            .ok_or("Firmware ZIP size overflow")?;
        if file.size() > MAX_MEMBER as u64 || expanded > MAX_EXPANDED {
            return Err("Firmware ZIP exceeds safe expanded size limits".into());
        }
    }
    Ok(archive)
}

pub(super) fn extract_archive(
    bytes: &[u8],
    provenance: &str,
    channel: SourceChannel,
    cancel: &AtomicBool,
    progress: &mut (dyn FnMut(Progress) + Send),
) -> Result<Vec<DownloadedFile>, String> {
    let mut archive = bounded_archive(bytes, cancel)?;
    let mut output = Vec::new();
    for index in 0..archive.len() {
        cancelled(cancel)?;
        let mut file = archive
            .by_index(index)
            .map_err(|e| format!("Cannot read firmware ZIP member: {e}"))?;
        let name = file
            .name()
            .map_err(|e| format!("Cannot decode firmware ZIP member name: {e}"))?;
        if file.is_dir() || !is_image(&name) {
            continue;
        }
        let name = name.into_owned();
        let size = file.size();
        if size == 0 {
            return Err(format!("Firmware ZIP member {name} is empty"));
        }
        // Every firmware member is still checked, even when it is not an updater.
        // In particular, excluded nested DFU ZIPs retain their archive safety checks.
        let eligible = channel != SourceChannel::BootloaderCi
            || is_bootloader_update(name.rsplit('/').next().unwrap_or(&name));
        if !eligible && !name.to_ascii_lowercase().ends_with(".zip") {
            consume_bounded(&mut file, MAX_MEMBER, size, cancel, progress, |_| {})?;
            continue;
        }
        let data = read_bounded(&mut file, MAX_MEMBER, size, cancel, progress)?;
        // Keep a DFU ZIP intact; the selected transport owns its manifest semantics.
        if name.to_ascii_lowercase().ends_with(".zip") {
            bounded_archive(&data, cancel)?;
        }
        if !eligible {
            continue;
        }
        let sha256 = hash(&data);
        output.push(DownloadedFile {
            provenance: format!("{provenance} | member {name} | member SHA-256 {sha256}"),
            name,
            bytes: data,
            sha256,
        });
    }
    if output.is_empty() {
        if channel == SourceChannel::BootloaderCi {
            return Err("CI archive contains no bootloader self-update UF2 firmware files".into());
        }
        return Err("CI archive contains no UF2, HEX, BIN, or DFU ZIP firmware files".into());
    }
    output.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(output)
}
