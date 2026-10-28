//! Authenticated handoff of private update cleanup to the restarted process.

use std::{
    env,
    ffi::{OsStr, OsString},
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::windows::{
        ffi::OsStringExt,
        fs::OpenOptionsExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::{Path, PathBuf},
    sync::mpsc,
    thread::{self, JoinHandle},
    time::Duration,
};

use serde::{Deserialize, Serialize};
use windows_sys::Win32::{
    Foundation::WAIT_OBJECT_0,
    Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, DELETE, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
        FILE_DISPOSITION_INFO, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_READ_DATA, FILE_SHARE_READ,
        FILE_SHARE_WRITE, FileDispositionInfo, GetFileInformationByHandle,
        SetFileInformationByHandle,
    },
    System::Threading::{
        INFINITE, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        PROCESS_SYNCHRONIZE, QueryFullProcessImageNameW, WaitForSingleObject,
    },
};

use super::{
    BACKUP_NAME, FileIdentity, HANDSHAKE_TIMEOUT, MAX_CONFIG, STAGED_NAME, file_identity,
    file_time, identity_from_info, path_from_wide, private_directory, process_creation, response,
    send_line, show_failure, validate_layout,
};

pub(super) const ENV_MARKER: &str = "SLIMENRF_INTERNAL_UPDATE_CLEANUP";
pub(super) const COMMIT_NAME: &str = "cleanup.commit";
pub(super) const PROTOCOL_VERSION: u32 = 1;
pub(super) const READY_LABEL: &str = "SLIMENRF-CLEANUP-V1-READY";
const MARKER_NAMESPACE: &str = "SLIMENRF-CLEANUP-";
const RESTART_LABEL: &str = "SLIMENRF-CLEANUP-V1-RESTART";
const CLEANUP_ONLY_LABEL: &str = "SLIMENRF-CLEANUP-V1-ONLY";

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
pub(super) enum TakeoverMode {
    Restart,
    CleanupOnly,
}

pub(super) fn marker_value(mode: TakeoverMode, nonce: &[u8; 16]) -> String {
    response(
        match mode {
            TakeoverMode::Restart => RESTART_LABEL,
            TakeoverMode::CleanupOnly => CLEANUP_ONLY_LABEL,
        },
        nonce,
    )
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
pub(super) struct DirectoryIdentity {
    pub(super) volume: u32,
    pub(super) index: u64,
    pub(super) created: u64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TakeoverConfig {
    pub(super) version: u32,
    pub(super) helper_pid: u32,
    pub(super) helper_created: u64,
    pub(super) helper_path: Vec<u16>,
    pub(super) helper_identity: FileIdentity,
    pub(super) directory_identity: DirectoryIdentity,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub(super) stage_identity: Option<FileIdentity>,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub(super) backup_identity: Option<FileIdentity>,
    pub(super) target: Vec<u16>,
    pub(super) target_identity: FileIdentity,
    pub(super) nonce: [u8; 16],
    pub(super) locale: String,
    pub(super) mode: TakeoverMode,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub(super) previous_env: Option<Vec<u16>>,
}

fn deserialize_required_option<'de, T, D>(deserializer: D) -> Result<Option<T>, D::Error>
where
    T: Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    Option::<T>::deserialize(deserializer)
}

pub(crate) enum StartupTakeover {
    None,
    Continue(CleanupWorker),
    CleanupOnly(CleanupWorker),
}

/// Keeps the cleanup thread alive until ordinary shutdown or panic unwinding.
pub(crate) struct CleanupWorker {
    thread: Option<JoinHandle<()>>,
    directory: PathBuf,
    locale: String,
}

impl Drop for CleanupWorker {
    fn drop(&mut self) {
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            warn_cleanup(
                &self.locale,
                &self.directory,
                "The cleanup worker stopped unexpectedly",
            );
        }
    }
}

fn parse_marker(value: &OsStr) -> Result<Option<(TakeoverMode, [u8; 16])>, String> {
    if !value.to_string_lossy().starts_with(MARKER_NAMESPACE) {
        return Ok(None);
    }
    let invalid = || "Invalid or unsupported cleanup environment marker".to_string();
    let value = value.to_str().ok_or_else(invalid)?;
    let (mode, encoded) = if let Some(encoded) = value.strip_prefix("SLIMENRF-CLEANUP-V1-RESTART ")
    {
        (TakeoverMode::Restart, encoded)
    } else if let Some(encoded) = value.strip_prefix("SLIMENRF-CLEANUP-V1-ONLY ") {
        (TakeoverMode::CleanupOnly, encoded)
    } else {
        return Err(invalid());
    };
    if encoded.len() != 32 {
        return Err(invalid());
    }
    let mut nonce = [0_u8; 16];
    for (byte, pair) in nonce
        .iter_mut()
        .zip(encoded.as_bytes().as_chunks::<2>().0.iter())
    {
        let digit = |value| match value {
            b'0'..=b'9' => Some(value - b'0'),
            b'a'..=b'f' => Some(value - b'a' + 10),
            _ => None,
        };
        *byte = (digit(pair[0]).ok_or_else(invalid)? << 4) | digit(pair[1]).ok_or_else(invalid)?;
    }
    Ok(Some((mode, nonce)))
}

// V1 is a little-endian u32 byte length followed by exactly that many bytes
// of JSON; the required version, mode and nonce are checked against the marker.
fn read_config(input: &mut impl Read) -> Result<TakeoverConfig, String> {
    let mut length = [0_u8; 4];
    input
        .read_exact(&mut length)
        .map_err(|error| format!("Cannot read cleanup handoff: {error}"))?;
    let length = u32::from_le_bytes(length) as usize;
    if length == 0 || length > MAX_CONFIG {
        return Err("Invalid cleanup handoff length".into());
    }
    let mut bytes = vec![0; length];
    input
        .read_exact(&mut bytes)
        .map_err(|error| format!("Cannot read cleanup handoff: {error}"))?;
    serde_json::from_slice(&bytes).map_err(|error| format!("Invalid cleanup handoff: {error}"))
}

fn check_handoff(
    config: &TakeoverConfig,
    mode: TakeoverMode,
    nonce: &[u8; 16],
) -> Result<(), String> {
    if config.version != PROTOCOL_VERSION || config.mode != mode || &config.nonce != nonce {
        return Err("Cleanup handoff does not match its environment marker".into());
    }
    if config
        .previous_env
        .as_ref()
        .is_some_and(|value| value.contains(&0))
    {
        return Err("Invalid previous cleanup environment value".into());
    }
    Ok(())
}

fn handle_info(file: &File, path: &Path) -> Result<BY_HANDLE_FILE_INFORMATION, String> {
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: file owns an open handle and info is a valid output buffer.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle().cast(), &mut info) } == 0 {
        return Err(format!(
            "Cannot identify {}: {}",
            path.display(),
            io::Error::last_os_error()
        ));
    }
    Ok(info)
}

fn directory_identity_from_handle(file: &File, path: &Path) -> Result<DirectoryIdentity, String> {
    let info = handle_info(file, path)?;
    if info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY == 0
        || info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
    {
        return Err(format!("{} is not a non-reparse directory", path.display()));
    }
    let index = (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow);
    if index == 0 {
        return Err(format!(
            "{} has no usable directory identity",
            path.display()
        ));
    }
    Ok(DirectoryIdentity {
        volume: info.dwVolumeSerialNumber,
        index,
        created: file_time(info.ftCreationTime),
    })
}

fn open_directory(path: &Path, access: u32) -> Result<(File, DirectoryIdentity), String> {
    let file = OpenOptions::new()
        .access_mode(access)
        // The retained guard also requests directory-list access so this
        // no-delete-share mode participates in Windows sharing checks.
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(|error| format!("Cannot open update directory {}: {error}", path.display()))?;
    let identity = directory_identity_from_handle(&file, path)?;
    Ok((file, identity))
}

pub(super) fn directory_identity(path: &Path) -> Result<DirectoryIdentity, String> {
    open_directory(path, FILE_READ_ATTRIBUTES).map(|(_, identity)| identity)
}

struct AuthorizedPaths {
    directory: PathBuf,
    helper: PathBuf,
    guard: File,
}

fn same_path(left: &Path, right: &Path) -> Result<bool, String> {
    let left = fs::canonicalize(left)
        .map_err(|error| format!("Cannot resolve {}: {error}", left.display()))?;
    let right = fs::canonicalize(right)
        .map_err(|error| format!("Cannot resolve {}: {error}", right.display()))?;
    Ok(left == right)
}

fn authorize_paths(config: &TakeoverConfig, current_exe: &Path) -> Result<AuthorizedPaths, String> {
    let helper = path_from_wide(&config.helper_path)?;
    let target = path_from_wide(&config.target)?;
    let directory = private_directory(&helper)?;
    let (guard, identity) = open_directory(&directory, FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES)?;
    if identity != config.directory_identity {
        return Err("The update directory identity changed".into());
    }
    validate_layout(&directory, &target)?;
    if file_identity(&helper)? != config.helper_identity {
        return Err("The update helper identity changed".into());
    }
    if !same_path(&target, current_exe)?
        || file_identity(&target)? != config.target_identity
        || file_identity(current_exe)? != config.target_identity
    {
        return Err("The restarted executable identity or path changed".into());
    }
    Ok(AuthorizedPaths {
        directory,
        helper,
        guard,
    })
}

fn open_helper(config: &TakeoverConfig, helper: &Path) -> Result<OwnedHandle, String> {
    // SAFETY: OpenProcess takes a PID and flags; a successful handle is owned below.
    let raw = unsafe {
        OpenProcess(
            PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
            0,
            config.helper_pid,
        )
    };
    if raw.is_null() {
        return Err(format!(
            "Cannot open update helper process: {}",
            io::Error::last_os_error()
        ));
    }
    // SAFETY: raw is a non-null process handle newly returned by OpenProcess.
    let handle = unsafe { OwnedHandle::from_raw_handle(raw.cast()) };
    if process_creation(handle.as_raw_handle().cast())? != config.helper_created {
        return Err("The update helper process identity changed".into());
    }
    let mut image = vec![0_u16; 32768];
    let mut size = image.len() as u32;
    // SAFETY: handle is a live owned process handle and image has size writable units.
    if unsafe {
        QueryFullProcessImageNameW(
            handle.as_raw_handle().cast(),
            PROCESS_NAME_WIN32,
            image.as_mut_ptr(),
            &mut size,
        )
    } == 0
    {
        return Err(format!(
            "Cannot identify update helper process: {}",
            io::Error::last_os_error()
        ));
    }
    image.truncate(size as usize);
    let image = path_from_wide(&image)?;
    if !same_path(&image, helper)?
        || file_identity(&image)? != config.helper_identity
        || file_identity(helper)? != config.helper_identity
    {
        return Err("The update helper process image changed".into());
    }
    Ok(handle)
}

fn restore_environment(previous: Option<&[u16]>) {
    // SAFETY: startup_takeover is called before GPUI, the runtime, or any other
    // application threads exist. The name is constant and previous was checked
    // for embedded NULs; no concurrent environment reads or writes can occur.
    unsafe {
        match previous {
            Some(value) => env::set_var(ENV_MARKER, OsString::from_wide(value)),
            None => env::remove_var(ENV_MARKER),
        }
    }
}

/// Called before starting GPUI or any application threads.
pub(crate) fn startup_takeover() -> Result<StartupTakeover, String> {
    let result = (|| {
        let Some(value) = env::var_os(ENV_MARKER) else {
            return Ok(StartupTakeover::None);
        };
        let Some((mode, nonce)) = parse_marker(&value)? else {
            return Ok(StartupTakeover::None);
        };
        start_takeover(mode, nonce)
    })();
    if let Err(error) = &result {
        // Keep a malformed handoff's response within the helper's line bound.
        let short: String = error.chars().take(512).collect();
        let _ = send_line(&mut io::stdout().lock(), &format!("CLEANUP-ERROR {short}"));
    }
    result
}

fn start_takeover(mode: TakeoverMode, nonce: [u8; 16]) -> Result<StartupTakeover, String> {
    let config = read_config(&mut io::stdin().lock())?;
    check_handoff(&config, mode, &nonce)?;
    let current_exe = env::current_exe()
        .map_err(|error| format!("Cannot locate restarted executable: {error}"))?;
    let paths = authorize_paths(&config, &current_exe)?;
    let helper = open_helper(&config, &paths.helper)?;
    restore_environment(config.previous_env.as_deref());

    // The worker proves it has started before the acknowledgement, and the
    // activation gate prevents it from waiting for the helper until after ACK.
    let (ready, readiness) = mpsc::sync_channel(1);
    let (start, started) = mpsc::sync_channel(1);
    let directory = paths.directory.clone();
    let locale = config.locale.clone();
    let worker_directory = directory.clone();
    let worker_locale = locale.clone();
    let thread = thread::Builder::new()
        .name("update-cleanup".into())
        .spawn(move || {
            if ready.send(()).is_err() || started.recv().is_err() {
                return;
            }
            // SAFETY: helper remains an owned handle to the authenticated process.
            let wait = unsafe { WaitForSingleObject(helper.as_raw_handle().cast(), INFINITE) };
            let result = if wait == WAIT_OBJECT_0 {
                cleanup_committed(paths, &config)
            } else {
                Err(format!(
                    "Cannot wait for update helper: {}",
                    io::Error::last_os_error()
                ))
            };
            if let Err(error) = result {
                warn_cleanup(&worker_locale, &worker_directory, &error);
            }
        })
        .map_err(|error| format!("Cannot start update cleanup worker: {error}"))?;
    let worker = CleanupWorker {
        thread: Some(thread),
        directory,
        locale,
    };
    let worker = acknowledge_worker(
        worker,
        readiness,
        start,
        &mut io::stdout().lock(),
        &nonce,
        HANDSHAKE_TIMEOUT,
    )?;
    Ok(match mode {
        TakeoverMode::Restart => StartupTakeover::Continue(worker),
        TakeoverMode::CleanupOnly => StartupTakeover::CleanupOnly(worker),
    })
}

fn acknowledge_worker(
    worker: CleanupWorker,
    readiness: mpsc::Receiver<()>,
    start: mpsc::SyncSender<()>,
    output: &mut impl Write,
    nonce: &[u8; 16],
    timeout: Duration,
) -> Result<CleanupWorker, String> {
    if let Err(error) = readiness.recv_timeout(timeout) {
        drop(readiness);
        drop(start);
        drop(worker);
        return Err(format!(
            "Update cleanup worker did not become ready: {error}"
        ));
    }
    if let Err(error) = send_line(output, &response(READY_LABEL, nonce)) {
        drop(start);
        drop(worker);
        return Err(format!("Cannot acknowledge update cleanup: {error}"));
    }
    start
        .send(())
        .map_err(|_| "Update cleanup worker stopped before handoff".to_string())?;
    Ok(worker)
}

fn warn_cleanup(locale: &str, directory: &Path, error: &str) {
    let detail = format!(
        "{error}\nRetained update directory: {}",
        directory.display()
    );
    let message = rust_i18n::t!(
        "app.settings.update.cleanup_warning",
        locale = locale,
        error = detail
    )
    .to_string();
    show_failure(&message);
}

fn open_removal_file(
    path: &Path,
    read: bool,
) -> Result<Option<(File, BY_HANDLE_FILE_INFORMATION)>, String> {
    let access = DELETE | FILE_READ_ATTRIBUTES | if read { FILE_READ_DATA } else { 0 };
    let file = match OpenOptions::new()
        .access_mode(access)
        // No writes or deletes can race validation of an opened file.
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "Cannot open {} for cleanup: {error}",
                path.display()
            ));
        }
    };
    let info = handle_info(&file, path)?;
    if info.dwFileAttributes & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT) != 0 {
        return Err(format!(
            "{} is not a regular, non-reparse file",
            path.display()
        ));
    }
    Ok(Some((file, info)))
}

fn required_removal_file(
    path: &Path,
    read: bool,
) -> Result<(File, BY_HANDLE_FILE_INFORMATION), String> {
    open_removal_file(path, read)?
        .ok_or_else(|| format!("Required cleanup file {} is missing", path.display()))
}

fn authorized_optional_file(
    path: &Path,
    expected: Option<&FileIdentity>,
) -> Result<Option<File>, String> {
    match (expected, open_removal_file(path, false)?) {
        (None, None) => Ok(None),
        (Some(expected), Some((file, info))) if &identity_from_info(path, &info)? == expected => {
            Ok(Some(file))
        }
        _ => Err(format!(
            "The cleanup file identity changed: {}",
            path.display()
        )),
    }
}

fn authorized_commit(path: &Path, nonce: &[u8; 16]) -> Result<File, String> {
    let (mut file, _) = required_removal_file(path, true)?;
    let mut actual = [0_u8; 16];
    file.read_exact(&mut actual).map_err(|error| {
        format!(
            "Cannot read cleanup authorization {}: {error}",
            path.display()
        )
    })?;
    let mut extra = [0_u8; 1];
    if &actual != nonce
        || file.read(&mut extra).map_err(|error| {
            format!(
                "Cannot read cleanup authorization {}: {error}",
                path.display()
            )
        })? != 0
    {
        return Err(format!("Invalid cleanup authorization {}", path.display()));
    }
    Ok(file)
}

fn mark_deleted(file: &File, path: &Path) -> Result<(), String> {
    let info = FILE_DISPOSITION_INFO { DeleteFile: true };
    // SAFETY: the file was opened with DELETE access and info has the exact
    // FILE_DISPOSITION_INFO layout for this information class.
    if unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle().cast(),
            FileDispositionInfo,
            (&info as *const FILE_DISPOSITION_INFO).cast(),
            std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
    } == 0
    {
        return Err(format!(
            "Cannot remove {}: {}",
            path.display(),
            io::Error::last_os_error()
        ));
    }
    Ok(())
}

fn cleanup_committed(paths: AuthorizedPaths, config: &TakeoverConfig) -> Result<(), String> {
    let commit_path = paths.directory.join(COMMIT_NAME);
    // Nothing, including the helper, may be removed without the exact marker.
    let commit = authorized_commit(&commit_path, &config.nonce)?;
    if directory_identity_from_handle(&paths.guard, &paths.directory)? != config.directory_identity
        || directory_identity(&paths.directory)? != config.directory_identity
    {
        return Err("The guarded update directory identity changed".into());
    }
    let (helper, info) = required_removal_file(&paths.helper, false)?;
    if identity_from_info(&paths.helper, &info)? != config.helper_identity {
        return Err("The update helper identity changed before cleanup".into());
    }
    let staged_path = paths.directory.join(STAGED_NAME);
    let backup_path = paths.directory.join(BACKUP_NAME);
    let staged = authorized_optional_file(&staged_path, config.stage_identity.as_ref())?;
    let backup = authorized_optional_file(&backup_path, config.backup_identity.as_ref())?;

    for (path, file) in [
        (&staged_path, staged),
        (&backup_path, backup),
        (&paths.helper, Some(helper)),
        (&commit_path, Some(commit)),
    ] {
        if let Some(file) = file {
            mark_deleted(&file, path)?;
        }
    }

    // Reopen with DELETE access only after releasing the no-delete-share guard.
    // The new no-follow handle is checked against the frozen identity before
    // deletion, so replacing the pathname in this interval cannot authorize it.
    drop(paths.guard);
    let (directory, identity) = open_directory(&paths.directory, FILE_READ_ATTRIBUTES | DELETE)?;
    if identity != config.directory_identity {
        return Err("The update directory identity changed before removal".into());
    }
    mark_deleted(&directory, &paths.directory)
}

#[cfg(test)]
mod tests;
