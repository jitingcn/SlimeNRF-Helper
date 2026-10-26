use super::{ARCHIVE, MEMBERS, MISSING_CONFIGURED_MEMBERS, Member, Resource};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Cursor, Read},
    sync::atomic::{AtomicBool, Ordering},
};

pub struct ResourceBytes {
    pub bytes: Vec<u8>,
    pub digest: String,
    sha256: [u8; 32],
}

impl Resource {
    pub fn read(&self, max: u64, cancel: &AtomicBool) -> Result<ResourceBytes, String> {
        check_cancel(cancel)?;
        if self.is_intentionally_omitted() {
            return Err("Firmware is intentionally not embedded in this build; choose a local file or remote firmware, or supply an external slimenrf-helper.yaml with local firmware paths".into());
        }
        match self {
            Self::File(path) => {
                let file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
                let len = file.metadata().map_err(|e| e.to_string())?.len();
                read_bounded(file, len, max, cancel).map_err(|e| format!("{}: {e}", path.display()))
            }
            Self::Bundled(name) => read_member(ARCHIVE, MEMBERS, name, max, cancel)
                .map_err(|e| format!("Bundled resource {name}: {e}")),
        }
    }
}

fn check_cancel(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Acquire) {
        Err("Cancelled".into())
    } else {
        Ok(())
    }
}

pub(super) fn read_bounded(
    mut reader: impl Read,
    declared_len: u64,
    max: u64,
    cancel: &AtomicBool,
) -> Result<ResourceBytes, String> {
    check_cancel(cancel)?;
    if declared_len > max {
        return Err(format!("Resource exceeds {max} byte limit"));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(declared_len).map_err(|e| e.to_string())?);
    let mut digest = Sha256::new();
    let mut chunk = [0u8; 32 * 1024];
    loop {
        check_cancel(cancel)?;
        // Read at most one byte beyond the bound, including growing files.
        let remaining = max.saturating_sub(bytes.len() as u64);
        let limit = remaining.saturating_add(1).min(chunk.len() as u64) as usize;
        let count = reader
            .read(&mut chunk[..limit])
            .map_err(|e| e.to_string())?;
        check_cancel(cancel)?;
        if count == 0 {
            break;
        }
        if count as u64 > remaining {
            return Err(format!("Resource exceeds {max} byte limit"));
        }
        bytes.extend_from_slice(&chunk[..count]);
        digest.update(&chunk[..count]);
    }
    let digest = digest.finalize();
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(digest.len() * 2);
    for byte in &digest {
        text.push(HEX[(byte >> 4) as usize] as char);
        text.push(HEX[(byte & 0x0f) as usize] as char);
    }
    Ok(ResourceBytes {
        bytes,
        digest: text,
        sha256: digest.into(),
    })
}

pub(super) fn read_member(
    archive: &[u8],
    members: &[Member],
    name: &str,
    max: u64,
    cancel: &AtomicBool,
) -> Result<ResourceBytes, String> {
    check_cancel(cancel)?;
    let member = members.iter().find(|member| member.name == name)
        .ok_or_else(|| {
            if MISSING_CONFIGURED_MEMBERS.iter().any(|missing| missing.eq_ignore_ascii_case(name)) {
                "Configured firmware was absent from the build inputs; supply the correct firmware with an external slimenrf-helper.yaml"
            } else {
                "Unavailable in this build; supply an external configuration and the correct firmware"
            }
        })?;
    if member.len > max {
        return Err(format!("Resource exceeds {max} byte limit"));
    }
    let mut archive = zip::ZipArchive::new(Cursor::new(archive)).map_err(|e| e.to_string())?;
    let file = archive.by_name(member.name).map_err(|e| e.to_string())?;
    if file.size() != member.len {
        return Err("Bundled member length does not match its index".into());
    }
    let content = read_bounded(file, member.len, member.len.min(max), cancel)?;
    if content.bytes.len() as u64 != member.len {
        return Err("Bundled member length does not match its index".into());
    }
    if content.sha256 != member.sha256 {
        return Err("Bundled member SHA-256 does not match its index".into());
    }
    check_cancel(cancel)?;
    Ok(content)
}
