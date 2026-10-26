//! Bounded paired CSV archives and transactional destination replacement.
use super::csv::{load_csv, write_csv};
use super::metadata::metadata_text;
use super::{
    MAX_INPUT_BYTES, MAX_META_BYTES, MAX_SAMPLES, MAX_TRACKS, cancelled, context_budget, err,
    restore_context, validate_identity,
};
use crate::capture::Recording;
use std::cell::Cell;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufReader, Read, Write};
use std::path::{Component, Path};
use std::sync::atomic::AtomicBool;

mod directory;
use directory::{Directory, DirectoryReader};

struct Member {
    index: usize,
    size: u64,
}

#[derive(Default)]
struct TrackMembers {
    csv: Option<Member>,
    metadata: Option<Member>,
}

struct ExportBytes<'a, W> {
    writer: &'a mut W,
    remaining: &'a mut u64,
}

impl<W: Write> Write for ExportBytes<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() as u64 > *self.remaining {
            return Err(std::io::Error::other(
                "Export exceeds 1 GiB total uncompressed limit",
            ));
        }
        let count = self.writer.write(bytes)?;
        *self.remaining -= count as u64;
        Ok(count)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.writer.flush()
    }
}

fn safe_zip_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 1024
        && !name.contains(['\\', ':', '\0'])
        && Path::new(name)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
        && !name
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
}

/// Enforce archive declarations against actual decompressor output too, so the
/// aggregate central-directory bound cannot be bypassed by false member sizes.
struct SizeChecked<R> {
    inner: R,
    remaining: u64,
}
impl<R: Read> Read for SizeChecked<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let count = self.inner.read(buffer)?;
        if count as u64 > self.remaining || (count == 0 && self.remaining != 0) {
            return Err(std::io::Error::other(
                "ZIP member size disagrees with its declaration",
            ));
        }
        self.remaining -= count as u64;
        Ok(count)
    }
}
pub(super) fn load_zip(path: &Path, cancel: &AtomicBool) -> Result<Recording, String> {
    let mut file = File::open(path).map_err(err)?;
    let directory = Directory::read(&mut file, cancel)?;
    let constructing = Cell::new(true);
    let reader = DirectoryReader::new(&mut file, &directory, &constructing);
    let mut archive = zip::ZipArchive::with_config(
        zip::read::Config {
            archive_offset: zip::read::ArchiveOffset::Known(0),
        },
        reader,
    )
    .map_err(err)?;
    constructing.set(false);
    if archive.len() != directory.count {
        return Err("ZIP member count is inconsistent or contains duplicate names".into());
    }
    let mut tracks = BTreeMap::<String, TrackMembers>::new();
    let mut total = 0u64;
    let mut csv_count = 0usize;
    for index in 0..archive.len() {
        cancelled(cancel)?;
        let file = archive.by_index_raw(index).map_err(err)?;
        let name = file.name().map_err(err)?;
        if !safe_zip_name(&name)
            || file.is_dir()
            || file.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000)
        {
            return Err(format!("Unsafe ZIP member: {name}"));
        }
        total = total.checked_add(file.size()).ok_or("ZIP size overflow")?;
        if total > MAX_INPUT_BYTES || file.size() > MAX_INPUT_BYTES {
            return Err("ZIP exceeds 1 GiB total uncompressed limit".into());
        }
        let member = Member {
            index,
            size: file.size(),
        };
        let target = if let Some(stem) = name.strip_suffix(".meta.txt") {
            if file.size() > MAX_META_BYTES as u64 {
                return Err("ZIP metadata exceeds limit".into());
            }
            &mut tracks.entry(stem.to_owned()).or_default().metadata
        } else if let Some(stem) = name.strip_suffix(".csv") {
            csv_count += 1;
            if csv_count > MAX_TRACKS {
                return Err("ZIP has too many recording tracks".into());
            }
            &mut tracks.entry(stem.to_owned()).or_default().csv
        } else {
            return Err(format!("Unsupported ZIP member: {name}"));
        };
        if target.replace(member).is_some() {
            return Err("Duplicate ZIP member".into());
        }
    }
    if csv_count == 0 || tracks.values().any(|track| track.csv.is_none()) {
        return Err("ZIP has no recordings or contains unpaired metadata".into());
    }
    let mut recording = Recording {
        provenance: vec![format!("CSV ZIP import: {}", path.display())],
        ..Recording::default()
    };
    let mut remaining = MAX_SAMPLES;
    for (stem, members) in tracks {
        cancelled(cancel)?;
        let mut metadata = String::new();
        if let Some(member) = members.metadata {
            SizeChecked {
                inner: archive.by_index(member.index).map_err(err)?,
                remaining: member.size,
            }
            .read_to_string(&mut metadata)
            .map_err(err)?;
        }
        let member = members.csv.ok_or("ZIP contains unpaired metadata")?;
        let source = format!("{}!{stem}", path.display());
        recording.tracks.push(load_csv(
            BufReader::new(SizeChecked {
                inner: archive.by_index(member.index).map_err(err)?,
                remaining: member.size,
            }),
            &metadata,
            &source,
            cancel,
            &mut remaining,
        )?);
    }
    restore_context(&mut recording)?;
    Ok(recording)
}
/// ZIP export is transactional: cancellation/failure never replaces the destination.
pub fn export_csv_zip(
    recording: &Recording,
    path: &Path,
    cancel: &AtomicBool,
) -> Result<(), String> {
    export_with_limit(recording, path, cancel, MAX_INPUT_BYTES)
}

fn export_with_limit(
    recording: &Recording,
    path: &Path,
    cancel: &AtomicBool,
    byte_limit: u64,
) -> Result<(), String> {
    cancelled(cancel)?;
    if recording.tracks.is_empty() || recording.tracks.len() > MAX_TRACKS {
        return Err("No recording tracks or too many tracks".into());
    }
    if recording
        .tracks
        .iter()
        .map(|t| t.samples.len())
        .sum::<usize>()
        > MAX_SAMPLES
    {
        return Err("Export sample limit exceeded".into());
    }
    context_budget(recording)?;
    let context = format!(
        "capture_warnings_json={}\ncapture_provenance_json={}\n",
        serde_json::to_string(&recording.warnings).map_err(err)?,
        serde_json::to_string(&recording.provenance).map_err(err)?
    );
    let mut remaining = byte_limit;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(err)?;
    {
        let mut zip = zip::ZipWriter::new(temp.as_file_mut());
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(0o600);
        for (index, track) in recording.tracks.iter().enumerate() {
            cancelled(cancel)?;
            validate_identity(&track.identity)?;
            let stem = format!("recording-{index}.tracker-{}", track.identity.slot);
            zip.start_file(format!("{stem}.csv"), options)
                .map_err(err)?;
            write_csv(
                &mut ExportBytes {
                    writer: &mut zip,
                    remaining: &mut remaining,
                },
                track,
                cancel,
            )?;
            zip.start_file(format!("{stem}.meta.txt"), options)
                .map_err(err)?;
            let mut text = metadata_text(track)?;
            if text
                .len()
                .checked_add(context.len())
                .is_none_or(|n| n > MAX_META_BYTES)
            {
                return Err("Export context exceeds metadata limit".into());
            }
            text.push_str(&context);
            ExportBytes {
                writer: &mut zip,
                remaining: &mut remaining,
            }
            .write_all(text.as_bytes())
            .map_err(err)?;
        }
        zip.finish().map_err(err)?;
    }
    cancelled(cancel)?;
    temp.as_file().sync_all().map_err(err)?;
    temp.persist(path).map_err(err)?;
    Ok(())
}

#[cfg(test)]
mod tests;
