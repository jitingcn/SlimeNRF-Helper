use crate::{
    features::firmware::board_catalog::valid_filename,
    update::{Progress, Uf2CopyReport, Uf2FinalizationStage},
};
use slimenrf_firmware_image::image::{
    MAX_CONTAINER_SIZE, Uf2ContentError, Uf2ImagePurpose, inspect_uf2, validate_uf2_content,
};
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
use std::collections::BTreeSet;
#[cfg(target_os = "macos")]
use std::ffi::CString;
#[cfg(not(target_os = "macos"))]
use std::fs::{self, OpenOptions};
#[cfg(any(not(target_os = "macos"), test))]
use std::path::Path;
use std::{
    fs::File,
    io::{self, Read, Write},
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
};

#[cfg(test)]
mod authorization_tests;
#[cfg(test)]
mod completion_tests;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

/// Host-side copy completion is separate from verification of the running firmware.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CopyOutcome {
    Completed,
    ConfirmationPending(Uf2CopyReport),
}

trait CopyFile: Write {
    fn sync_all(&self) -> io::Result<()>;
}

impl CopyFile for File {
    fn sync_all(&self) -> io::Result<()> {
        File::sync_all(self)
    }
}

#[derive(Clone, Debug)]
pub struct Uf2Volume {
    pub mount: PathBuf,
    pub label: String,
    pub board_id: Option<String>,
    pub identity: String,
    #[cfg(not(target_os = "macos"))]
    info_filename: String,
    #[cfg(target_os = "linux")]
    linux_identity: linux::Identity,
    #[cfg(target_os = "macos")]
    info_filename: CString,
    #[cfg(target_os = "macos")]
    macos_identity: macos::Identity,
    #[cfg(target_os = "macos")]
    marker_checksum: u32,
}

struct Marker {
    board_id: Option<String>,
    checksum: u32,
}

impl Marker {
    fn from_info(file: File, name: impl std::fmt::Display) -> Result<Self, String> {
        let bytes = read_bounded(file, name, 64 * 1024)?;
        let info = std::str::from_utf8(&bytes).map_err(|_| "UF2 information file is not UTF-8")?;
        let board_id = info.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            (key.trim().eq_ignore_ascii_case("Board-ID") && !value.trim().is_empty())
                .then(|| value.trim().to_owned())
        });
        Ok(Self {
            board_id,
            checksum: crc32fast::hash(&bytes),
        })
    }

    fn from_current(file: io::Result<File>, info_filename: &str) -> Result<Self, String> {
        // Reboot validates the whole CURRENT.UF2 image; identity uses its first block.
        let mut block = [0; 512];
        file.and_then(|mut file| file.read_exact(&mut block))
            .map_err(|e| format!("No readable {info_filename} or CURRENT.UF2 marker: {e}"))?;
        Ok(Self {
            board_id: None,
            checksum: crc32fast::hash(&block),
        })
    }
}

fn read_bounded(
    file: File,
    name: impl std::fmt::Display,
    maximum: usize,
) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    file.take(maximum as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("Cannot read {name}: {e}"))?;
    if bytes.len() > maximum {
        return Err(format!("{name} exceeds its size limit"));
    }
    Ok(bytes)
}

/// Freeze an explicit volume or a local directory used by the copy-path tests.
#[cfg(any(not(target_os = "macos"), test))]
pub fn inspect_volume(mount: &Path, info_filename: &str) -> Result<Uf2Volume, String> {
    #[cfg(target_os = "macos")]
    {
        macos::inspect_fixture(mount, info_filename)
    }
    #[cfg(not(target_os = "macos"))]
    {
        inspect(
            mount,
            info_filename,
            #[cfg(target_os = "linux")]
            None,
        )
    }
}

#[cfg(not(target_os = "macos"))]
fn inspect(
    mount: &Path,
    info_filename: &str,
    #[cfg(target_os = "linux")] expected: Option<&linux::Identity>,
) -> Result<Uf2Volume, String> {
    if !valid_filename(info_filename) {
        return Err("Invalid UF2 information filename".into());
    }
    let mount = fs::canonicalize(mount).map_err(|e| format!("UF2 volume is unavailable: {e}"))?;
    let metadata = fs::metadata(&mount).map_err(|e| format!("Cannot inspect UF2 volume: {e}"))?;
    if !metadata.is_dir() {
        return Err("UF2 mount is not a directory".into());
    }
    #[cfg(target_os = "linux")]
    let linux_identity = linux::Identity::inspect(&mount, &metadata, expected)?;
    let info_path = mount.join(info_filename);
    let marker = if info_path.is_file() {
        let file = File::open(&info_path)
            .map_err(|e| format!("Cannot open {}: {e}", info_path.display()))?;
        Marker::from_info(file, info_path.display())?
    } else {
        Marker::from_current(File::open(mount.join("CURRENT.UF2")), info_filename)?
    };
    #[cfg(target_os = "linux")]
    let (label, native_identity) = linux_identity.volume_identity();
    #[cfg(not(target_os = "linux"))]
    let (label, native_identity) = volume_identity(&mount, &metadata)?;
    let identity = format!(
        "{}|{}|{:08x}",
        mount.display(),
        native_identity,
        marker.checksum
    );
    Ok(Uf2Volume {
        mount,
        label,
        board_id: marker.board_id,
        identity,
        info_filename: info_filename.into(),
        #[cfg(target_os = "linux")]
        linux_identity,
    })
}

#[cfg(windows)]
fn volume_identity(mount: &Path, metadata: &fs::Metadata) -> Result<(String, String), String> {
    use std::{
        os::windows::{ffi::OsStrExt, fs::MetadataExt},
        ptr,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        GetVolumeInformationW, GetVolumeNameForVolumeMountPointW, GetVolumePathNameW,
    };
    let path: Vec<u16> = mount.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut root = vec![0u16; 32768];
    if unsafe { GetVolumePathNameW(path.as_ptr(), root.as_mut_ptr(), root.len() as u32) } == 0 {
        return Err(format!(
            "Cannot resolve volume root: {}",
            std::io::Error::last_os_error()
        ));
    }
    let mut label = [0u16; 261];
    let mut guid = [0u16; 128];
    let mut serial = 0;
    if unsafe {
        GetVolumeInformationW(
            root.as_ptr(),
            label.as_mut_ptr(),
            label.len() as u32,
            &mut serial,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            0,
        )
    } == 0
    {
        return Err(format!(
            "Cannot identify volume: {}",
            std::io::Error::last_os_error()
        ));
    }
    if unsafe {
        GetVolumeNameForVolumeMountPointW(root.as_ptr(), guid.as_mut_ptr(), guid.len() as u32)
    } == 0
    {
        return Err(format!(
            "Cannot identify volume GUID: {}",
            std::io::Error::last_os_error()
        ));
    }
    let decode = |s: &[u16]| {
        String::from_utf16_lossy(&s[..s.iter().position(|ch| *ch == 0).unwrap_or(s.len())])
    };
    Ok((
        decode(&label),
        format!(
            "{}:{serial:08x}:{}",
            decode(&guid),
            metadata.creation_time()
        ),
    ))
}
#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn volume_identity(mount: &Path, metadata: &fs::Metadata) -> Result<(String, String), String> {
    use std::os::unix::fs::MetadataExt;
    Ok((
        mount
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        format!("{}:{}", metadata.dev(), metadata.ino()),
    ))
}
#[cfg(not(any(windows, unix)))]
fn volume_identity(_: &Path, _: &fs::Metadata) -> Result<(String, String), String> {
    Err("UF2 volume identity is unsupported on this platform".into())
}

pub fn scan_with_info(info_filename: &str) -> Result<Vec<Uf2Volume>, String> {
    if !valid_filename(info_filename) {
        return Err("Invalid UF2 information filename".into());
    }
    #[cfg(target_os = "linux")]
    {
        linux::scan(info_filename)
    }
    #[cfg(target_os = "macos")]
    {
        macos::scan(info_filename)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        scan_mount_candidates(info_filename)
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn scan_mount_candidates(info_filename: &str) -> Result<Vec<Uf2Volume>, String> {
    let mut seen = BTreeSet::new();
    let mut volumes = Vec::new();
    for mount in mount_candidates()? {
        if !(mount.join(info_filename).is_file() || mount.join("CURRENT.UF2").is_file()) {
            continue;
        }
        if let Ok(volume) = inspect_volume(&mount, info_filename)
            && seen.insert(volume.mount.clone())
        {
            volumes.push(volume);
        }
    }
    Ok(volumes)
}
#[cfg(windows)]
fn mount_candidates() -> Result<Vec<PathBuf>, String> {
    use windows_sys::Win32::Storage::FileSystem::{GetDriveTypeW, GetLogicalDrives};
    let mask = unsafe { GetLogicalDrives() };
    if mask == 0 {
        return Err(format!(
            "Cannot enumerate volumes: {}",
            std::io::Error::last_os_error()
        ));
    }
    let mut paths = Vec::new();
    for i in 0..26 {
        if mask & (1 << i) != 0 {
            let root = [b'A' as u16 + i, b':' as u16, b'\\' as u16, 0];
            if unsafe { GetDriveTypeW(root.as_ptr()) } == 2 {
                paths.push(PathBuf::from(String::from_utf16_lossy(&root[..3])));
            }
        }
    }
    Ok(paths)
}
#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn mount_candidates() -> Result<Vec<PathBuf>, String> {
    let mut paths = Vec::new();
    // Other Unix platforms retain their directory-based discovery separately.
    for root in ["/Volumes", "/media", "/run/media", "/mnt"] {
        let path = PathBuf::from(root);
        paths.push(path.clone());
        if let Ok(children) = fs::read_dir(path) {
            for child in children.flatten() {
                if !child.path().is_dir() {
                    continue;
                }
                paths.push(child.path());
                if let Ok(grandchildren) = fs::read_dir(child.path()) {
                    paths.extend(
                        grandchildren
                            .flatten()
                            .map(|entry| entry.path())
                            .filter(|path| path.is_dir()),
                    );
                }
            }
        }
    }
    Ok(paths)
}
#[cfg(not(any(windows, unix)))]
fn mount_candidates() -> Result<Vec<PathBuf>, String> {
    Err("UF2 discovery is unsupported on this platform".into())
}

fn cancelled(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Acquire) {
        Err(
            "UF2 operation cancelled; the device may contain a partial image and need reflashing"
                .into(),
        )
    } else {
        Ok(())
    }
}
#[cfg(target_os = "macos")]
pub fn revalidate(volume: &Uf2Volume) -> Result<(), String> {
    macos::revalidate(volume)
}

#[cfg(not(target_os = "macos"))]
pub fn revalidate(volume: &Uf2Volume) -> Result<(), String> {
    let current = inspect(
        &volume.mount,
        &volume.info_filename,
        #[cfg(target_os = "linux")]
        Some(&volume.linux_identity),
    )?;
    if current.identity != volume.identity
        || current.board_id != volume.board_id
        || current.label != volume.label
    {
        return Err("UF2 volume identity changed after selection; select the device again".into());
    }
    Ok(())
}
pub fn validate_for_volume(
    volume: &Uf2Volume,
    bytes: &[u8],
    purpose: Uf2ImagePurpose,
) -> Result<(), String> {
    // Board-ID mismatch takes precedence over application capacity errors.
    // The family on this error is content metadata, never device authorization.
    let (family, capacity_error) = match validate_uf2_content(bytes, purpose) {
        Ok(facts) => (facts.expected_application_family_id, None),
        Err(error @ Uf2ContentError::FlashCapacityExceeded { family_id, .. }) => {
            (family_id, Some(error))
        }
        Err(error) => return Err(error.into()),
    };
    if let Some(board) = &volume.board_id {
        let board = board.to_ascii_lowercase();
        if (board.contains("nrf52840") && family != 0xada5_2840)
            || (board.contains("nrf52833") && family != 0x621e_937a)
        {
            return Err(format!(
                "UF2 family {family:#010x} does not match Board-ID {board}"
            ));
        }
    }
    match capacity_error {
        Some(error) => Err(error.into()),
        None => Ok(()),
    }
}

pub fn write(
    volume: &Uf2Volume,
    name: &str,
    bytes: &[u8],
    purpose: Uf2ImagePurpose,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(Progress),
) -> Result<CopyOutcome, String> {
    write_with(volume, name, bytes, purpose, cancel, progress, |file| file)
}

fn write_with<W: CopyFile>(
    volume: &Uf2Volume,
    name: &str,
    bytes: &[u8],
    purpose: Uf2ImagePurpose,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(Progress),
    wrap: impl FnOnce(File) -> W,
) -> Result<CopyOutcome, String> {
    cancelled(cancel)?;
    validate_for_volume(volume, bytes, purpose)?;
    copy_to_volume(volume, name, bytes, cancel, progress, wrap)
}

// The callers freeze image authorization: write validates that purpose, while
// reboot only echoes structurally valid CURRENT.UF2 from this device.
fn copy_to_volume<W: CopyFile>(
    volume: &Uf2Volume,
    name: &str,
    bytes: &[u8],
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(Progress),
    wrap: impl FnOnce(File) -> W,
) -> Result<CopyOutcome, String> {
    cancelled(cancel)?;
    if !valid_filename(name) || !name.to_ascii_lowercase().ends_with(".uf2") {
        return Err("UF2 destination must be a plain .uf2 filename".into());
    }
    #[cfg(target_os = "macos")]
    let is_info_filename = volume
        .info_filename
        .as_bytes()
        .eq_ignore_ascii_case(name.as_bytes());
    #[cfg(not(target_os = "macos"))]
    let is_info_filename = volume.info_filename.eq_ignore_ascii_case(name);
    if name.eq_ignore_ascii_case("CURRENT.UF2") || is_info_filename {
        return Err("Refusing to overwrite a UF2 identity/source marker".into());
    }
    revalidate(volume)?;
    cancelled(cancel)?;
    // Never truncate an existing file or follow a destination symlink. Re-enter
    // bootloader mode after a partial write instead of blindly replaying it.
    #[cfg(target_os = "macos")]
    let file = macos::create_destination(volume, name);
    #[cfg(not(target_os = "macos"))]
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(volume.mount.join(name));
    let file = file.map_err(|e| {
        format!(
            "Cannot create {} (re-enter bootloader mode if this file already exists): {e}",
            volume.mount.join(name).display()
        )
    })?;
    let mut file = wrap(file);
    let total = bytes.len() as u64;
    let mut completed = 0;
    progress(Progress::bytes(format!("Writing {name}"), completed, total));
    for chunk in bytes.chunks(64 * 1024) {
        cancelled(cancel)?;
        revalidate(volume)?;
        let mut remaining = chunk;
        while !remaining.is_empty() {
            match file.write(remaining) {
                Ok(0) => {
                    return Err(format!(
                        "UF2 copy failed after {completed}/{total} bytes: {}",
                        io::Error::from(io::ErrorKind::WriteZero)
                    ));
                }
                Ok(written) => {
                    completed += written as u64;
                    remaining = &remaining[written..];
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    return Err(format!(
                        "UF2 copy failed after {completed}/{total} bytes: {error}"
                    ));
                }
            }
        }
        progress(Progress::bytes(format!("Writing {name}"), completed, total));
    }
    cancelled(cancel)?;
    for stage in [Uf2FinalizationStage::Flush, Uf2FinalizationStage::SyncAll] {
        let result = match stage {
            Uf2FinalizationStage::Flush => file.flush(),
            Uf2FinalizationStage::SyncAll => file.sync_all(),
        };
        if let Err(error) = result {
            if device_disappeared(&error) {
                return Ok(CopyOutcome::ConfirmationPending(Uf2CopyReport {
                    completed,
                    total,
                    stage,
                    os_error: error.raw_os_error(),
                    diagnostic: error.to_string(),
                }));
            }
            return Err(format!(
                "UF2 {stage:?} failed after {completed}/{total} host-written bytes: {error}"
            ));
        }
    }
    progress(Progress::bytes(
        "UF2 copy completed; waiting for the device to restart is separate from firmware verification",
        completed,
        total,
    ));
    Ok(CopyOutcome::Completed)
}

fn device_disappeared(error: &io::Error) -> bool {
    let Some(code) = error.raw_os_error() else {
        return false;
    };
    #[cfg(windows)]
    {
        matches!(code, 433 | 1167)
    }
    // ENODEV / ENXIO on the explicitly supported Unix targets; never admit EIO.
    #[cfg(target_os = "linux")]
    {
        matches!(code, 19 | 6)
    }
    #[cfg(target_os = "macos")]
    {
        matches!(code, 19 | 6)
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        let _ = code;
        false
    }
}

pub fn reboot(
    volume: &Uf2Volume,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(Progress),
) -> Result<CopyOutcome, String> {
    reboot_with(volume, cancel, progress, |file| file)
}

fn reboot_with<W: CopyFile>(
    volume: &Uf2Volume,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(Progress),
    wrap: impl FnOnce(File) -> W,
) -> Result<CopyOutcome, String> {
    cancelled(cancel)?;
    revalidate(volume)?;
    let source = volume.mount.join("CURRENT.UF2");
    #[cfg(target_os = "macos")]
    let file = macos::open_current(volume)?;
    #[cfg(not(target_os = "macos"))]
    let file = {
        let metadata = fs::symlink_metadata(&source)
            .map_err(|e| format!("Cannot inspect {}: {e}", source.display()))?;
        if !metadata.file_type().is_file() {
            return Err("CURRENT.UF2 must be a regular file on the selected UF2 volume".into());
        }
        File::open(&source).map_err(|e| format!("Cannot open {}: {e}", source.display()))?
    };
    let current = read_bounded(file, source.display(), MAX_CONTAINER_SIZE)?;
    revalidate(volume)?;
    cancelled(cancel)?;
    // CURRENT is supplied by this frozen device, and its family may be a USB
    // VID/PID rather than an application family. Validate the whole container,
    // not the application whitelist. Never choose any other UF2 as a fallback.
    inspect_uf2(&current).map_err(String::from)?;
    copy_to_volume(volume, "REBOOT.UF2", &current, cancel, progress, wrap)
}

#[cfg(test)]
mod tests {
    use super::{inspect_volume, reboot, revalidate, write};
    use slimenrf_firmware_image::image::Uf2ImagePurpose;
    use std::{
        fs,
        sync::atomic::{AtomicBool, Ordering},
    };
    pub(super) fn payload(blocks: usize) -> Vec<u8> {
        payload_with_family(blocks, 0xada52840)
    }
    fn payload_with_family(blocks: usize, family: u32) -> Vec<u8> {
        let mut result = vec![0; blocks * 512];
        for (index, b) in result.as_chunks_mut::<512>().0.iter_mut().enumerate() {
            for (offset, value) in [
                (0, 0x0a324655u32),
                (4, 0x9e5d5157),
                (8, 0x2000),
                (12, 0x1000 + index as u32 * 256),
                (16, 256),
                (20, index as u32),
                (24, blocks as u32),
                (28, family),
                (508, 0x0ab16f30),
            ] {
                b[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
            }
        }
        result
    }
    #[test]
    fn writes_exact_bytes_and_rejects_traversal_replacement_and_family() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("INFO_UF2.TXT"), "Board-ID: nRF52840-test\n").unwrap();
        let volume = inspect_volume(dir.path(), "INFO_UF2.TXT").unwrap();
        let bytes = payload(1);
        let cancel = AtomicBool::new(false);
        write(
            &volume,
            "firmware.uf2",
            &bytes,
            Uf2ImagePurpose::Application,
            &cancel,
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(fs::read(dir.path().join("firmware.uf2")).unwrap(), bytes);
        assert!(
            write(
                &volume,
                "firmware.uf2",
                &bytes,
                Uf2ImagePurpose::Application,
                &cancel,
                &mut |_| {}
            )
            .is_err()
        );
        assert!(
            write(
                &volume,
                "../other.uf2",
                &bytes,
                Uf2ImagePurpose::Application,
                &cancel,
                &mut |_| {}
            )
            .is_err()
        );
        assert!(
            write(
                &volume,
                "CURRENT.UF2",
                &bytes,
                Uf2ImagePurpose::Application,
                &cancel,
                &mut |_| {}
            )
            .is_err()
        );
        assert!(!dir.path().join("CURRENT.UF2").exists());
        let mut wrong = bytes.clone();
        wrong[28..32].copy_from_slice(&0x621e937au32.to_le_bytes());
        assert!(
            write(
                &volume,
                "wrong.uf2",
                &wrong,
                Uf2ImagePurpose::Application,
                &cancel,
                &mut |_| {}
            )
            .is_err()
        );
        fs::write(
            dir.path().join("INFO_UF2.TXT"),
            "Board-ID: nRF52833-replacement\n",
        )
        .unwrap();
        assert!(revalidate(&volume).is_err());
        assert!(
            write(
                &volume,
                "new.uf2",
                &bytes,
                Uf2ImagePurpose::Application,
                &cancel,
                &mut |_| {}
            )
            .is_err()
        );
    }
    #[test]
    fn cancellation_stops_before_create_and_between_chunks() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("INFO_UF2.TXT"), "Board-ID: nRF52840-test").unwrap();
        let volume = inspect_volume(dir.path(), "INFO_UF2.TXT").unwrap();
        let bytes = payload(200);
        let cancel = AtomicBool::new(true);
        assert!(
            write(
                &volume,
                "cancel.uf2",
                &bytes,
                Uf2ImagePurpose::Application,
                &cancel,
                &mut |_| {}
            )
            .is_err()
        );
        assert!(!dir.path().join("cancel.uf2").exists());
        cancel.store(false, Ordering::Release);
        assert!(
            write(
                &volume,
                "partial.uf2",
                &bytes,
                Uf2ImagePurpose::Application,
                &cancel,
                &mut |p| {
                    if p.completed > 0 {
                        cancel.store(true, Ordering::Release);
                    }
                }
            )
            .is_err()
        );
        assert_eq!(
            fs::metadata(dir.path().join("partial.uf2")).unwrap().len(),
            65536
        );
    }
    #[test]
    fn reboot_echoes_board_vid_pid_family_but_normal_write_rejects_it() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("INFO_UF2.TXT"),
            "Board-ID: slimevr-styria-mini\n",
        )
        .unwrap();
        let bytes = payload_with_family(200, 0x239a00b3);
        fs::write(dir.path().join("CURRENT.UF2"), &bytes).unwrap();
        let volume = inspect_volume(dir.path(), "INFO_UF2.TXT").unwrap();
        let cancel = AtomicBool::new(false);
        let error = write(
            &volume,
            "firmware.uf2",
            &bytes,
            Uf2ImagePurpose::Application,
            &cancel,
            &mut |_| {},
        )
        .unwrap_err();
        assert!(error.contains("0x239a00b3"), "{error}");
        assert!(!dir.path().join("firmware.uf2").exists());
        let mut completed = Vec::new();
        reboot(&volume, &cancel, &mut |p| {
            assert_eq!(p.total, bytes.len() as u64);
            completed.push(p.completed);
        })
        .unwrap();
        assert_eq!(completed, [0, 65536, 102400, 102400]);
        assert_eq!(fs::read(dir.path().join("REBOOT.UF2")).unwrap(), bytes);
        assert_eq!(fs::read(dir.path().join("CURRENT.UF2")).unwrap(), bytes);
    }

    #[test]
    fn reboot_rejects_malformed_current_and_never_uses_another_image() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("INFO_UF2.TXT"),
            "Board-ID: slimevr-styria-mini\n",
        )
        .unwrap();
        let volume = inspect_volume(dir.path(), "INFO_UF2.TXT").unwrap();
        let cancel = AtomicBool::new(false);
        fs::write(dir.path().join("other.uf2"), payload(1)).unwrap();
        let error = reboot(&volume, &cancel, &mut |_| {}).unwrap_err();
        assert!(error.contains("CURRENT.UF2"), "{error}");
        assert!(!dir.path().join("REBOOT.UF2").exists());
        let mut malformed = payload_with_family(2, 0x239a00b3);
        malformed[1020..1024].fill(0);
        fs::write(dir.path().join("CURRENT.UF2"), malformed).unwrap();
        assert!(reboot(&volume, &cancel, &mut |_| {}).is_err());
        assert!(!dir.path().join("REBOOT.UF2").exists());
    }

    #[test]
    fn reboot_preserves_cancel_identity_and_existing_destination_guards() {
        let dir = tempfile::tempdir().unwrap();
        let info = dir.path().join("INFO_UF2.TXT");
        fs::write(&info, "Board-ID: slimevr-styria-mini\n").unwrap();
        fs::write(
            dir.path().join("CURRENT.UF2"),
            payload_with_family(1, 0x239a00b3),
        )
        .unwrap();
        let volume = inspect_volume(dir.path(), "INFO_UF2.TXT").unwrap();
        let cancel = AtomicBool::new(true);
        let error = reboot(&volume, &cancel, &mut |_| {}).unwrap_err();
        assert!(error.contains("cancelled"), "{error}");
        assert!(!dir.path().join("REBOOT.UF2").exists());
        cancel.store(false, Ordering::Release);
        fs::write(&info, "Board-ID: replacement\n").unwrap();
        let error = reboot(&volume, &cancel, &mut |_| {}).unwrap_err();
        assert!(error.contains("identity changed"), "{error}");
        assert!(!dir.path().join("REBOOT.UF2").exists());
        fs::write(&info, "Board-ID: slimevr-styria-mini\n").unwrap();
        fs::write(dir.path().join("REBOOT.UF2"), b"existing destination").unwrap();
        let error = reboot(&volume, &cancel, &mut |_| {}).unwrap_err();
        assert!(error.contains("Cannot create"), "{error}");
        assert_eq!(
            fs::read(dir.path().join("REBOOT.UF2")).unwrap(),
            b"existing destination"
        );
    }

    #[test]
    fn reboot_revalidates_identity_between_chunks() {
        let dir = tempfile::tempdir().unwrap();
        let info = dir.path().join("INFO_UF2.TXT");
        fs::write(&info, "Board-ID: slimevr-styria-mini\n").unwrap();
        fs::write(
            dir.path().join("CURRENT.UF2"),
            payload_with_family(200, 0x239a00b3),
        )
        .unwrap();
        let volume = inspect_volume(dir.path(), "INFO_UF2.TXT").unwrap();
        let error = reboot(&volume, &AtomicBool::new(false), &mut |p| {
            if p.completed > 0 {
                fs::write(&info, "Board-ID: replacement\n").unwrap();
            }
        })
        .unwrap_err();
        assert!(error.contains("identity changed"), "{error}");
        assert_eq!(
            fs::metadata(dir.path().join("REBOOT.UF2")).unwrap().len(),
            65536
        );
    }

    #[cfg(unix)]
    #[test]
    fn reboot_rejects_linked_source_and_destination() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("INFO_UF2.TXT"),
            "Board-ID: slimevr-styria-mini\n",
        )
        .unwrap();
        let bytes = payload_with_family(1, 0x239a00b3);
        let other_path = other.path().join("other.uf2");
        fs::write(&other_path, &bytes).unwrap();
        symlink(&other_path, dir.path().join("CURRENT.UF2")).unwrap();
        let volume = inspect_volume(dir.path(), "INFO_UF2.TXT").unwrap();
        let cancel = AtomicBool::new(false);
        let error = reboot(&volume, &cancel, &mut |_| {}).unwrap_err();
        assert!(error.contains("regular file"), "{error}");
        assert!(!dir.path().join("REBOOT.UF2").exists());
        fs::remove_file(dir.path().join("CURRENT.UF2")).unwrap();
        fs::write(dir.path().join("CURRENT.UF2"), &bytes).unwrap();
        symlink(&other_path, dir.path().join("REBOOT.UF2")).unwrap();
        let error = reboot(&volume, &cancel, &mut |_| {}).unwrap_err();
        assert!(error.contains("Cannot create"), "{error}");
        assert_eq!(fs::read(other_path).unwrap(), bytes);
    }
}
