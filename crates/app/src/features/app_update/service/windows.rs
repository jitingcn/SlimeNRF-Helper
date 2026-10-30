//! Private, same-volume Windows portable replacement transaction.
//!
//! The shutdown worker prepares and commits the helper only after retained work
//! has joined. Replacement still waits for the original process to exit.

pub(crate) mod cleanup;
mod pe;

use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write},
    os::windows::{
        ffi::{OsStrExt, OsStringExt},
        fs::{MetadataExt, OpenOptionsExt},
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
        process::CommandExt,
    },
    path::{Component, Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        Mutex, OnceLock,
        mpsc::{self, Receiver, RecvTimeoutError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use windows_sys::Win32::{
    Foundation::{FILETIME, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0},
    Security::Cryptography::{BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom},
    Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, DELETE, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES, FILE_RENAME_INFO, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE, FileRenameInfo, FindClose, FindFirstFileW,
        GetFileInformationByHandle, ReplaceFileW, SetFileInformationByHandle, WIN32_FIND_DATAW,
    },
    System::Pipes::{CreatePipe, PIPE_NOWAIT, PeekNamedPipe, SetNamedPipeHandleState},
    System::Threading::{
        CREATE_NO_WINDOW, GetCurrentProcess, GetProcessTimes, INFINITE, OpenProcess,
        PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
        QueryFullProcessImageNameW, WaitForSingleObject,
    },
    UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MB_TOPMOST, MessageBoxW},
};

use super::{DownloadedUpdate, release::Target};

const HELPER_FLAG: &str = "--slimenrf-internal-update-helper";
const INTERNAL_PREFIX: &str = "--slimenrf-internal-update-";
const DIRECTORY_PREFIX: &str = ".slimenrf-update-";
const HELPER_NAME: &str = "helper.exe";
const STAGED_NAME: &str = "replacement.exe";
const BACKUP_NAME: &str = "previous.exe";
const READY: &str = "READY";
const ACK: &str = "COMMITTED";
const COMMIT: u8 = 0xa5;
const MAX_CONFIG: usize = 1024 * 1024;
const MAX_HANDSHAKE_LINE: u64 = 4096;
const MAX_PIPE_WRITE_CHUNK: usize = 4096;
const PIPE_WRITE_POLL_INTERVAL: Duration = Duration::from_millis(5);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
const COMMIT_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const REAP_TIMEOUT: Duration = Duration::from_secs(3);
const POLL_INTERVAL: Duration = Duration::from_millis(50);

// Initialization is deliberately explicit: a late lazy lookup could pin a
// replacement placed at this path after the original application started.
static RUNNING_IMAGE: OnceLock<Result<RunningImage, String>> = OnceLock::new();

fn official_build() -> bool {
    !cfg!(debug_assertions)
        && !cfg!(test)
        && !env!("SLIMENRF_BUILD_OFFICIAL_TAG").is_empty()
        && matches!(env!("SLIMENRF_BUILD_OFFICIAL_PRERELEASE"), "true" | "false")
}

/// Freeze the image at startup, before the UI or any background runtime exists.
pub(crate) fn freeze_running_image() {
    if official_build() {
        let _ = RUNNING_IMAGE.get_or_init(|| {
            let path = std::env::current_exe()
                .map_err(|error| format!("Cannot locate the running executable: {error}"))?;
            let path = fs::canonicalize(path)
                .map_err(|error| format!("Cannot resolve the running executable: {error}"))?;
            capture_image(&path)
        });
    }
}

pub(crate) fn install_eligible() -> bool {
    official_build() && RUNNING_IMAGE.get().is_some_and(Result::is_ok)
}

pub(crate) fn installed_prerelease() -> bool {
    official_build()
        && env!("SLIMENRF_BUILD_OFFICIAL_PRERELEASE")
            .parse::<bool>()
            .unwrap_or(false)
}

pub(super) fn validate_payload(path: &Path) -> Result<(), String> {
    pe::validate_payload(path)
}

/// Recognize the private mode before GPUI or the Tokio runtime starts.
/// Any malformed invocation in this namespace fails closed.
pub(crate) fn dispatch(arguments: &[OsString]) -> Option<i32> {
    match private_mode(arguments) {
        PrivateMode::Ordinary => None,
        PrivateMode::Invalid => Some(2),
        PrivateMode::Helper => Some(if helper_main().is_ok() { 0 } else { 1 }),
    }
}

#[derive(Debug, PartialEq, Eq)]
enum PrivateMode {
    Ordinary,
    Helper,
    Invalid,
}

fn private_mode(arguments: &[OsString]) -> PrivateMode {
    if !arguments
        .iter()
        .any(|arg| arg.to_string_lossy().starts_with(INTERNAL_PREFIX))
    {
        PrivateMode::Ordinary
    } else if arguments.len() == 1 && arguments[0] == HELPER_FLAG {
        PrivateMode::Helper
    } else {
        PrivateMode::Invalid
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct FileIdentity {
    volume: u32,
    index: u64,
    created: u64,
    size: u64,
    last_write: u64,
}
struct RunningImage {
    path: PathBuf,
    identity: FileIdentity,
    file: Mutex<File>,
}

#[derive(Debug, Deserialize, Serialize)]
struct HelperConfig {
    parent_pid: u32,
    parent_created: u64,
    nonce: [u8; 16],
    locale: String,
    target: Vec<u16>,
    final_name: Vec<u16>,
    working_directory: Vec<u16>,
    arguments: Vec<Vec<u16>>,
    target_identity: FileIdentity,
    size: u64,
    sha256: [u8; 32],
}

fn wide(value: &std::ffi::OsStr) -> Vec<u16> {
    value.encode_wide().collect()
}

fn path_from_wide(value: &[u16]) -> Result<PathBuf, String> {
    if value.is_empty() || value.contains(&0) {
        return Err("Invalid path in update handoff".into());
    }
    Ok(PathBuf::from(OsString::from_wide(value)))
}

fn file_time(value: FILETIME) -> u64 {
    (u64::from(value.dwHighDateTime) << 32) | u64::from(value.dwLowDateTime)
}

fn process_creation(handle: HANDLE) -> Result<u64, String> {
    let mut created = FILETIME::default();
    let mut exited = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: the handle refers to a live process and all output pointers are valid.
    if unsafe { GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) } == 0 {
        return Err(format!(
            "Cannot inspect update parent process: {}",
            io::Error::last_os_error()
        ));
    }
    Ok(file_time(created))
}

fn regular_file(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("Cannot inspect {}: {error}", path.display()))?;
    if !metadata.is_file() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(format!(
            "{} is not a regular, non-reparse file",
            path.display()
        ));
    }
    Ok(())
}

fn open_regular(path: &Path, sharing: u32) -> Result<(File, BY_HANDLE_FILE_INFORMATION), String> {
    regular_file(path)?;
    let file = OpenOptions::new()
        .read(true)
        .share_mode(sharing)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(|error| format!("Cannot inspect {}: {error}", path.display()))?;
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: file owns a valid Windows file handle and info is writable.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle().cast(), &mut info) } == 0 {
        return Err(format!(
            "Cannot identify {}: {}",
            path.display(),
            io::Error::last_os_error()
        ));
    }
    if info.dwFileAttributes & (FILE_ATTRIBUTE_REPARSE_POINT | FILE_ATTRIBUTE_DIRECTORY) != 0 {
        return Err(format!(
            "{} is not a regular, non-reparse file",
            path.display()
        ));
    }
    Ok((file, info))
}

fn file_identity(path: &Path) -> Result<FileIdentity, String> {
    let (_file, info) = open_regular(path, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)?;
    identity_from_info(path, &info)
}

fn optional_identity(path: &Path) -> Result<Option<FileIdentity>, String> {
    match fs::symlink_metadata(path) {
        Ok(_) => file_identity(path).map(Some),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("Cannot inspect {}: {error}", path.display())),
    }
}

fn authorized_stage_identity(
    path: &Path,
    admitted: Option<&FileIdentity>,
    size: u64,
    sha256: &[u8; 32],
) -> Result<Option<FileIdentity>, String> {
    let actual = optional_identity(path)?;
    let Some(expected) = admitted else {
        return Ok(None);
    };
    if !actual.is_some_and(|identity| same_file_id(&identity, expected)) {
        return Ok(None);
    }
    // A changed stage must remain evidence, not become eligible for cleanup.
    Ok(verify_staged(path, size, sha256)
        .ok()
        .filter(|identity| same_file_id(identity, expected)))
}

fn identity_from_info(
    path: &Path,
    info: &BY_HANDLE_FILE_INFORMATION,
) -> Result<FileIdentity, String> {
    let index = (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow);
    if index == 0 {
        return Err(format!("{} has no usable file identity", path.display()));
    }
    Ok(FileIdentity {
        volume: info.dwVolumeSerialNumber,
        index,
        created: file_time(info.ftCreationTime),
        size: (u64::from(info.nFileSizeHigh) << 32) | u64::from(info.nFileSizeLow),
        last_write: file_time(info.ftLastWriteTime),
    })
}

fn capture_image(path: &Path) -> Result<RunningImage, String> {
    // Permit another instance to replace the pathname, but prevent later
    // in-place writes from changing the image retained for the helper.
    let (file, info) = open_regular(path, FILE_SHARE_READ | FILE_SHARE_DELETE)?;
    Ok(RunningImage {
        path: path.to_path_buf(),
        identity: identity_from_info(path, &info)?,
        file: Mutex::new(file),
    })
}

fn verify_staged(path: &Path, size: u64, expected: &[u8; 32]) -> Result<FileIdentity, String> {
    // Deny writes/deletes while hashing and parsing; this handle is dropped on
    // return, before ReplaceFileW opens the replacement with no sharing.
    let (mut file, info) = open_regular(path, FILE_SHARE_READ)?;
    let mut digest = Sha256::new();
    let mut count = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let n = file
            .read(&mut buffer)
            .map_err(|error| format!("Cannot read staged update: {error}"))?;
        if n == 0 {
            break;
        }
        count = count
            .checked_add(n as u64)
            .ok_or("Staged update is too large")?;
        if count > size {
            return Err("Staged update size changed".into());
        }
        digest.update(&buffer[..n]);
    }
    let actual: [u8; 32] = digest.finalize().into();
    if count != size || &actual != expected {
        return Err("Staged update no longer matches the GitHub SHA-256 digest".into());
    }
    validate_payload(path)?;
    identity_from_info(path, &info)
}

fn copy_staged(source: &Path, destination: &Path) -> Result<(), String> {
    let mut source = File::open(source)
        .map_err(|error| format!("Cannot open update staging source: {error}"))?;
    copy_reader(&mut source, destination)
}

fn copy_reader(source: &mut impl Read, destination: &Path) -> Result<(), String> {
    let mut destination = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .map_err(|error| format!("Cannot create staged update file: {error}"))?;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = source
            .read(&mut buffer)
            .map_err(|error| format!("Cannot read staging source: {error}"))?;
        if count == 0 {
            break;
        }
        destination
            .write_all(&buffer[..count])
            .map_err(|error| format!("Cannot write update staging: {error}"))?;
    }
    destination
        .sync_all()
        .map_err(|error| format!("Cannot flush update staging: {error}"))
}

fn copy_running_helper(image: &RunningImage, destination: &Path) -> Result<(), String> {
    if file_identity(&image.path)? != image.identity {
        return Err(
            "The executable changed after this application started; the update was not installed"
                .into(),
        );
    }
    let mut original = image
        .file
        .lock()
        .map_err(|_| "The retained executable handle is unavailable")?;
    original
        .seek(SeekFrom::Start(0))
        .map_err(|error| format!("Cannot seek the retained executable: {error}"))?;
    copy_reader(&mut *original, destination)?;
    if file_identity(&image.path)? != image.identity {
        return Err("The executable changed while preparing the update".into());
    }
    Ok(())
}

struct Helper {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    events: Receiver<Result<String, String>>,
    reader: Option<JoinHandle<()>>,
    directory: Option<tempfile::TempDir>,
    nonce: [u8; 16],
}

fn response(label: &str, nonce: &[u8; 16]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(label.len() + 33);
    encoded.push_str(label);
    encoded.push(' ');
    for byte in nonce {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn random_nonce() -> Result<[u8; 16], String> {
    let mut nonce = [0_u8; 16];
    // SAFETY: the system RNG writes exactly the length of the provided buffer.
    let status = unsafe {
        BCryptGenRandom(
            std::ptr::null_mut(),
            nonce.as_mut_ptr(),
            nonce.len() as u32,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    };
    if status < 0 {
        return Err(format!(
            "Cannot generate update handoff nonce: NTSTATUS {status:#x}"
        ));
    }
    Ok(nonce)
}

impl Helper {
    fn wait_event(&self, expected: &str) -> Result<(), String> {
        let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
        loop {
            if Instant::now() >= deadline {
                return Err("The update helper did not respond in time".into());
            }
            match self.events.recv_timeout(POLL_INTERVAL) {
                Ok(Ok(message)) if message == expected => return Ok(()),
                Ok(Ok(message)) => {
                    return Err(format!("Update helper rejected the request: {message}"));
                }
                Ok(Err(error)) => return Err(error),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    return Err("The update helper exited unexpectedly".into());
                }
            }
        }
    }

    fn send_commit(&mut self) -> Result<(), String> {
        let stdin = self.stdin.as_mut().ok_or("Update helper is unavailable")?;
        stdin
            .write_all(&[COMMIT])
            .and_then(|()| stdin.write_all(&self.nonce))
            .and_then(|()| stdin.flush())
            .map_err(|error| format!("Cannot commit the update: {error}"))?;
        self.wait_event(&response(ACK, &self.nonce))
    }

    fn reap(&mut self, terminate: bool) {
        drop(self.stdin.take());
        if let Some(mut child) = self.child.take() {
            if terminate {
                let _ = child.kill();
            }
            let deadline = Instant::now() + REAP_TIMEOUT;
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) if Instant::now() < deadline => thread::sleep(POLL_INTERVAL),
                    _ => {
                        let _ = child.kill();
                        let _ = child.wait();
                        break;
                    }
                }
            }
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        drop(self.directory.take());
    }

    fn detach_committed(&mut self) {
        drop(self.stdin.take());
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        // The helper retains the transaction until a relaunched process accepts
        // responsibility for cleanup after the helper exits.
        if let Some(directory) = self.directory.take() {
            let _ = directory.keep();
        }
        drop(self.child.take());
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        if self.child.is_some() {
            self.reap(false);
        }
    }
}

/// Prepare and commit on the shutdown worker after all retained work has joined.
pub(crate) fn install_after_shutdown(downloaded: DownloadedUpdate) -> Result<(), String> {
    let mut helper = prepare_helper(&downloaded)?;
    let result = helper.send_commit();
    if result.is_ok() {
        helper.detach_committed();
    } else {
        helper.reap(true);
    }
    result
}

fn prepare_helper(downloaded: &DownloadedUpdate) -> Result<Helper, String> {
    if !install_eligible() {
        return Err("This build can check for updates but cannot install them".into());
    }
    let image = RUNNING_IMAGE
        .get()
        .and_then(|result| result.as_ref().ok())
        .ok_or("The running executable was not captured at startup")?;
    let target = &image.path;
    let parent = target
        .parent()
        .ok_or("The executable has no parent directory")?;
    let final_name = Target::current()?.installation_name(
        target.file_name().ok_or("The executable has no filename")?,
        &downloaded.release,
    )?;
    let final_target = destination_from_name(target, &wide(&final_name))?;
    resolve_destination(target, &final_target)?;
    if file_identity(target)? != image.identity {
        return Err(
            "The executable changed after this application started; the update was not installed"
                .into(),
        );
    }
    let target_identity = image.identity.clone();
    let directory = tempfile::Builder::new()
        .prefix(DIRECTORY_PREFIX)
        .tempdir_in(parent)
        .map_err(|error| format!("Cannot create update staging beside the executable: {error}"))?;
    let stage = directory.path().join(STAGED_NAME);
    copy_staged(downloaded.file.path(), &stage)?;
    verify_staged(
        &stage,
        downloaded.release.asset.size,
        &downloaded.release.asset.sha256,
    )?;
    let helper_path = directory.path().join(HELPER_NAME);
    copy_running_helper(image, &helper_path)?;
    let config = HelperConfig {
        parent_pid: std::process::id(),
        // SAFETY: GetCurrentProcess returns a process pseudo-handle valid for this call.
        parent_created: process_creation(unsafe { GetCurrentProcess() })?,
        nonce: random_nonce()?,
        locale: rust_i18n::locale().to_string(),
        target: wide(target.as_os_str()),
        final_name: wide(&final_name),
        working_directory: wide(
            std::env::current_dir()
                .map_err(|error| format!("Cannot retain the current directory: {error}"))?
                .as_os_str(),
        ),
        arguments: std::env::args_os().skip(1).map(|arg| wide(&arg)).collect(),
        target_identity,
        size: downloaded.release.asset.size,
        sha256: downloaded.release.asset.sha256,
    };
    let bytes = serde_json::to_vec(&config)
        .map_err(|error| format!("Cannot encode update handoff: {error}"))?;
    if bytes.len() > MAX_CONFIG {
        return Err("Update handoff arguments are too large".into());
    }
    let mut child = Command::new(&helper_path)
        .arg(HELPER_FLAG)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|error| format!("Cannot start the update helper: {error}"))?;
    let stdin = child.stdin.take().expect("piped helper stdin");
    let stdout = child.stdout.take().expect("piped helper stdout");
    let (sender, events) = mpsc::channel();
    let reader = match thread::Builder::new()
        .name("app-update-handshake".into())
        .spawn(move || {
            let mut stdout = BufReader::new(stdout);
            for _ in 0..2 {
                let mut line = String::new();
                let event = match (&mut stdout).take(MAX_HANDSHAKE_LINE).read_line(&mut line) {
                    Ok(0) => Err("The update helper closed its handshake".into()),
                    Ok(_) if line.ends_with('\n') => {
                        Ok(line.trim_end_matches(['\r', '\n']).to_owned())
                    }
                    Ok(_) => Err("The update helper response was too long or incomplete".into()),
                    Err(error) => Err(format!("Cannot read update helper response: {error}")),
                };
                let end = event.is_err();
                if sender.send(event).is_err() || end {
                    break;
                }
            }
        }) {
        Ok(reader) => reader,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("Cannot start update handshake: {error}"));
        }
    };
    let mut helper = Helper {
        child: Some(child),
        stdin: Some(stdin),
        events,
        reader: Some(reader),
        directory: Some(directory),
        nonce: config.nonce,
    };
    let result = (|| {
        let stdin = helper.stdin.as_mut().expect("helper has stdin");
        stdin
            .write_all(&(bytes.len() as u32).to_le_bytes())
            .and_then(|()| stdin.write_all(&bytes))
            .and_then(|()| stdin.flush())
            .map_err(|error| format!("Cannot send update handoff: {error}"))?;
        helper.wait_event(&response(READY, &helper.nonce))
    })();
    if let Err(error) = result {
        helper.reap(true);
        return Err(error);
    }
    Ok(helper)
}

fn open_parent(config: &HelperConfig, target: &Path) -> Result<OwnedHandle, String> {
    // SAFETY: OpenProcess accepts these flags and a PID; the returned handle is
    // checked and becomes uniquely owned immediately afterwards.
    let raw = unsafe {
        OpenProcess(
            PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
            0,
            config.parent_pid,
        )
    };
    if raw.is_null() {
        return Err(format!(
            "Cannot open the original process: {}",
            io::Error::last_os_error()
        ));
    }
    // SAFETY: raw is a non-null owned process handle from OpenProcess.
    let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
    if process_creation(handle.as_raw_handle().cast())? != config.parent_created {
        return Err("The original process identity changed".into());
    }
    let mut image = vec![0_u16; 32768];
    let mut size = image.len() as u32;
    // SAFETY: the process handle is valid and image has capacity for size UTF-16 units.
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
            "Cannot identify the original process image: {}",
            io::Error::last_os_error()
        ));
    }
    image.truncate(size as usize);
    let image = path_from_wide(&image)?;
    if file_identity(&image)? != config.target_identity
        || file_identity(target)? != config.target_identity
    {
        return Err("The original executable identity changed".into());
    }
    Ok(handle)
}

fn private_directory(helper: &Path) -> Result<PathBuf, String> {
    if helper.file_name().is_none_or(|name| name != HELPER_NAME) {
        return Err("Invalid update helper location".into());
    }
    let directory = helper.parent().ok_or("Invalid update helper directory")?;
    if !directory
        .file_name()
        .is_some_and(|name| name.to_string_lossy().starts_with(DIRECTORY_PREFIX))
    {
        return Err("Invalid update helper directory".into());
    }
    let metadata = fs::symlink_metadata(directory)
        .map_err(|error| format!("Cannot inspect staging directory: {error}"))?;
    if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err("Update staging is not a regular directory".into());
    }
    regular_file(helper)?;
    Ok(directory.to_path_buf())
}

fn validate_layout(directory: &Path, target: &Path) -> Result<(), String> {
    regular_file(target)?;
    let target_parent = target.parent().ok_or("Invalid executable directory")?;
    let actual_parent = fs::canonicalize(directory.parent().ok_or("Invalid staging directory")?)
        .map_err(|error| format!("Cannot resolve staging directory: {error}"))?;
    let target_parent = fs::canonicalize(target_parent)
        .map_err(|error| format!("Cannot resolve executable directory: {error}"))?;
    if actual_parent != target_parent {
        return Err("Update staging is not beside the executable".into());
    }
    Ok(())
}

fn destination_from_name(target: &Path, name: &[u16]) -> Result<PathBuf, String> {
    let name = path_from_wide(name)?;
    let mut components = name.components();
    if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
        return Err("Invalid update destination filename".into());
    }
    let original = target.file_name().ok_or("The executable has no filename")?;
    if name.as_os_str() != original {
        let platform = Target::current()?;
        if !platform.is_canonical_asset_name(original)
            || !platform.is_canonical_asset_name(name.as_os_str())
        {
            return Err("Invalid update destination for this executable".into());
        }
    }
    Ok(target.with_file_name(name))
}

fn directory_entry_name(path: &Path) -> Option<OsString> {
    // FindFirstFile searches both long and short names, so reject wildcard
    // patterns and compare the actual directory entry returned by the OS.
    if wide(path.file_name()?)
        .iter()
        .any(|unit| *unit == b'*' as u16 || *unit == b'?' as u16)
    {
        return None;
    }
    let path = nul_wide(path).ok()?;
    let mut data = WIN32_FIND_DATAW::default();
    // SAFETY: path is NUL-terminated and data points to writable storage.
    let search = unsafe { FindFirstFileW(path.as_ptr(), &mut data) };
    if search == INVALID_HANDLE_VALUE {
        return None;
    }
    // SAFETY: search is a valid FindFirstFile search handle.
    unsafe { FindClose(search) };
    let end = data.cFileName.iter().position(|unit| *unit == 0)?;
    Some(OsString::from_wide(&data.cFileName[..end]))
}

fn same_directory_entry(target: &Path, destination: &Path) -> bool {
    if target.parent() != destination.parent() {
        return false;
    }
    let Some((left, right)) = directory_entry_name(target).zip(directory_entry_name(destination))
    else {
        return false;
    };
    if left != right
        || !file_identity(target)
            .ok()
            .zip(file_identity(destination).ok())
            .is_some_and(|(left, right)| left == right)
    {
        return false;
    }
    let Some(name) = left.to_str() else {
        return false;
    };
    let Some(parent) = target.parent() else {
        return false;
    };
    let Ok(entries) = fs::read_dir(parent) else {
        return false;
    };
    // Case-sensitive directories may contain distinct entries differing only
    // in case, including hardlinks. Refuse an ambiguous case-insensitive match.
    let mut matches = 0;
    for entry in entries {
        let Ok(entry) = entry else {
            return false;
        };
        if entry
            .file_name()
            .to_str()
            .is_some_and(|entry| entry.eq_ignore_ascii_case(name))
        {
            matches += 1;
            if matches > 1 {
                return false;
            }
        }
    }
    matches == 1
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Destination {
    InPlace,
    Vacant,
    SameEntry,
}

fn resolve_destination(target: &Path, destination: &Path) -> Result<Destination, String> {
    if destination == target {
        return Ok(Destination::InPlace);
    }
    match fs::symlink_metadata(destination) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Destination::Vacant),
        Err(error) => Err(format!(
            "Cannot inspect update destination {}: {error}",
            destination.display()
        )),
        Ok(metadata) => {
            // Matching file IDs are insufficient: distinct hardlinks are
            // separate entries and must remain collisions.
            if metadata.is_file()
                && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0
                && same_directory_entry(target, destination)
            {
                return Ok(Destination::SameEntry);
            }
            Err(format!(
                "The update destination already exists: {}",
                destination.display()
            ))
        }
    }
}

fn read_config(input: &mut impl Read) -> Result<HelperConfig, String> {
    let mut length = [0_u8; 4];
    input
        .read_exact(&mut length)
        .map_err(|error| format!("Cannot read update handoff: {error}"))?;
    let length = u32::from_le_bytes(length) as usize;
    if length == 0 || length > MAX_CONFIG {
        return Err("Invalid update handoff length".into());
    }
    let mut bytes = vec![0; length];
    input
        .read_exact(&mut bytes)
        .map_err(|error| format!("Cannot read update handoff: {error}"))?;
    serde_json::from_slice(&bytes).map_err(|error| format!("Invalid update handoff: {error}"))
}

fn send_line(output: &mut impl Write, message: &str) -> io::Result<()> {
    writeln!(output, "{}", message.replace(['\r', '\n'], " "))?;
    output.flush()
}

fn read_commit(input: &mut impl Read, pipe: HANDLE, parent: HANDLE, nonce: &[u8; 16]) -> bool {
    let deadline = Instant::now() + COMMIT_TIMEOUT;
    loop {
        if Instant::now() >= deadline {
            return false;
        }
        let mut available = 0_u32;
        // SAFETY: pipe is inherited stdin; available is a writable u32 and
        // no data buffer is requested. Anonymous pipes support PeekNamedPipe.
        let ok = unsafe {
            PeekNamedPipe(
                pipe,
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                &mut available,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return false;
        }
        if available > 17 {
            return false;
        }
        if available == 17 {
            let mut command = [0_u8; 17];
            return input.read_exact(&mut command).is_ok()
                && command[0] == COMMIT
                && command[1..] == nonce[..];
        }
        // SAFETY: parent is the already authenticated process handle.
        if unsafe { WaitForSingleObject(parent, POLL_INTERVAL.as_millis() as u32) }
            != windows_sys::Win32::Foundation::WAIT_TIMEOUT
        {
            return false;
        }
    }
}

fn abandon_uncommitted(
    parent: &OwnedHandle,
    config: &HelperConfig,
    target: &Path,
    helper: &Path,
    directory: &Path,
    stage_identity: &FileIdentity,
) {
    // SAFETY: this is the exact authenticated parent handle; a zero timeout
    // only observes whether its process has already exited.
    match unsafe { WaitForSingleObject(parent.as_raw_handle().cast(), 0) } {
        WAIT_OBJECT_0 => {}
        windows_sys::Win32::Foundation::WAIT_TIMEOUT => {
            // The still-running parent owns and joins its uncommitted helper.
            return;
        }
        _ => {
            show_install_failure(
                &config.locale,
                &format!(
                    "Cannot verify whether the original process exited: {}. No update was installed. Remove the retained temporary directory after this helper exits: {}",
                    io::Error::last_os_error(),
                    directory.display()
                ),
            );
            return;
        }
    }
    let result = if is_original(target, &config.target_identity) {
        launch(
            target,
            config,
            helper,
            directory,
            cleanup::TakeoverMode::CleanupOnly,
            LaunchImage::Original(&config.target_identity),
            Some(stage_identity),
        )
    } else {
        Err(LaunchFailure::Spawn(
            "The original executable identity changed".into(),
        ))
    };
    if let Err(error) = result {
        show_install_failure(
            &config.locale,
            &format!(
                "{error}. No update was installed. After this helper exits, remove its retained temporary directory manually: {}",
                directory.display()
            ),
        );
    }
}

fn helper_main() -> Result<(), String> {
    let mut input = io::stdin().lock();
    let mut output = io::stdout().lock();
    let helper = std::env::current_exe()
        .map_err(|error| format!("Cannot locate private helper: {error}"))?;
    let directory = private_directory(&helper)?;
    let preparation = (|| {
        let config = read_config(&mut input)?;
        let target = path_from_wide(&config.target)?;
        validate_layout(&directory, &target)?;
        let final_target = destination_from_name(&target, &config.final_name)?;
        resolve_destination(&target, &final_target)?;
        let parent = open_parent(&config, &target)?;
        let stage = directory.join(STAGED_NAME);
        let stage_identity = verify_staged(&stage, config.size, &config.sha256)?;
        Ok::<_, String>((config, target, final_target, parent, stage_identity))
    })();
    let (config, target, final_target, parent, stage_identity) = match preparation {
        Ok(value) => value,
        Err(error) => {
            if send_line(&mut output, &error).is_err() {
                show_failure(&format!(
                    "The update helper could not contact its parent: {error}. After it exits, remove its retained temporary directory manually: {}",
                    directory.display()
                ));
            }
            return Err(error);
        }
    };
    if send_line(&mut output, &response(READY, &config.nonce)).is_err() {
        abandon_uncommitted(
            &parent,
            &config,
            &target,
            &helper,
            &directory,
            &stage_identity,
        );
        return Ok(());
    }
    if !read_commit(
        &mut input,
        io::stdin().as_raw_handle().cast(),
        parent.as_raw_handle().cast(),
        &config.nonce,
    ) {
        abandon_uncommitted(
            &parent,
            &config,
            &target,
            &helper,
            &directory,
            &stage_identity,
        );
        return Ok(());
    }
    if send_line(&mut output, &response(ACK, &config.nonce)).is_err() {
        abandon_uncommitted(
            &parent,
            &config,
            &target,
            &helper,
            &directory,
            &stage_identity,
        );
        return Ok(());
    }
    drop(output);
    // SAFETY: parent is an owned handle to the exact verified process. Waiting
    // for that handle prevents PID reuse from authorizing a premature replace.
    if unsafe { WaitForSingleObject(parent.as_raw_handle().cast(), INFINITE) } != WAIT_OBJECT_0 {
        show_install_failure(
            &config.locale,
            &format!(
                "Cannot wait for the original application to exit: {}. No update was installed. Remove the retained temporary directory after this helper exits: {}",
                io::Error::last_os_error(),
                directory.display(),
            ),
        );
        return Err("Cannot wait for original process".into());
    }
    let outcome = install_after_exit(&config, &target, &final_target, &directory, &stage_identity);
    if let Err(error) = &outcome {
        let message = if error.cleanup_taken {
            error.message.clone()
        } else if error.preserve_backup {
            format!(
                "{} The backup and temporary files were retained at {}; do not remove them until the original executable has been recovered.",
                error.message,
                directory.display(),
            )
        } else {
            format!(
                "{} After this helper exits, remove its retained temporary directory manually: {}",
                error.message,
                directory.display(),
            )
        };
        show_install_failure(&config.locale, &message);
    }
    outcome.map_err(|error| error.message)
}

fn nul_wide(path: &Path) -> Result<Vec<u16>, String> {
    let mut units = wide(path.as_os_str());
    if units.contains(&0) {
        return Err("Update path contains a NUL character".into());
    }
    units.push(0);
    Ok(units)
}

fn replace_file(target: &Path, replacement: &Path, backup: Option<&Path>) -> Result<(), String> {
    let target = nul_wide(target)?;
    let replacement = nul_wide(replacement)?;
    let backup = backup.map(nul_wide).transpose()?;
    // SAFETY: all strings are NUL-terminated UTF-16 buffers, optional pointers
    // are valid for the duration of the call, and reserved arguments are null.
    let ok = unsafe {
        ReplaceFileW(
            target.as_ptr(),
            replacement.as_ptr(),
            backup
                .as_ref()
                .map_or(std::ptr::null(), |value| value.as_ptr()),
            0,
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error().to_string());
    }
    Ok(())
}

fn open_rename_source(source: &Path, expected: &FileIdentity, exact: bool) -> Result<File, String> {
    let file = OpenOptions::new()
        .access_mode(DELETE | FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(source)
        .map_err(|error| format!("Cannot open {} for rename: {error}", source.display()))?;
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: file owns a valid handle and info is a writable output buffer.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle().cast(), &mut info) } == 0 {
        return Err(format!(
            "Cannot identify {}: {}",
            source.display(),
            io::Error::last_os_error()
        ));
    }
    if info.dwFileAttributes & (FILE_ATTRIBUTE_REPARSE_POINT | FILE_ATTRIBUTE_DIRECTORY) != 0 {
        return Err(format!(
            "{} is not a regular, non-reparse file",
            source.display()
        ));
    }
    let actual = identity_from_info(source, &info)?;
    let matches = if exact {
        &actual == expected
    } else {
        same_file_id(&actual, expected)
    };
    if !matches {
        return Err(format!("The identity of {} changed", source.display()));
    }
    Ok(file)
}

fn rename_open_file(source: &File, destination: &Path) -> Result<(), String> {
    let name = nul_wide(destination)?;
    let name_bytes = (name.len() - 1)
        .checked_mul(std::mem::size_of::<u16>())
        .and_then(|length| u32::try_from(length).ok())
        .ok_or("Update destination filename is too long")?;
    let length = std::mem::offset_of!(FILE_RENAME_INFO, FileName)
        .checked_add(name.len() * std::mem::size_of::<u16>())
        .ok_or("Update destination filename is too long")?
        .max(std::mem::size_of::<FILE_RENAME_INFO>());
    let buffer_size =
        u32::try_from(length).map_err(|_| "Update destination filename is too long")?;
    // usize storage provides the alignment required by FILE_RENAME_INFO.
    let mut storage = vec![0_usize; length.div_ceil(std::mem::size_of::<usize>())];
    let info = storage.as_mut_ptr().cast::<FILE_RENAME_INFO>();
    // SAFETY: storage is aligned and large enough for the header and UTF-16
    // name, including its terminator. Zero initialization sets ReplaceIfExists
    // to false and RootDirectory to null.
    unsafe {
        (*info).FileNameLength = name_bytes;
        std::ptr::copy_nonoverlapping(
            name.as_ptr(),
            std::ptr::addr_of_mut!((*info).FileName).cast::<u16>(),
            name.len(),
        );
        if SetFileInformationByHandle(
            source.as_raw_handle().cast(),
            FileRenameInfo,
            info.cast(),
            buffer_size,
        ) == 0
        {
            return Err(format!(
                "Cannot rename to {} without replacing an existing entry: {}",
                destination.display(),
                io::Error::last_os_error()
            ));
        }
    }
    Ok(())
}

fn move_verified_no_replace(
    source: &Path,
    destination: &Path,
    expected: &FileIdentity,
    exact: bool,
) -> Result<(), String> {
    let file = open_rename_source(source, expected, exact)?;
    rename_open_file(&file, destination)
}

fn same_file_id(left: &FileIdentity, right: &FileIdentity) -> bool {
    left.volume == right.volume && left.index == right.index
}

fn is_original(path: &Path, expected: &FileIdentity) -> bool {
    file_identity(path).is_ok_and(|identity| &identity == expected)
}

fn restore_original(
    target: &Path,
    backup: &Path,
    expected: &FileIdentity,
    allowed_replacement: Option<&FileIdentity>,
) -> Result<(), String> {
    if is_original(target, expected) {
        return Ok(());
    }
    // Keep the checked backup handle pinned through recovery so a substituted
    // pathname can never be moved over the user's executable.
    let backup_guard = open_rename_source(backup, expected, true).map_err(|error| {
        format!(
            "The original executable could not be safely located: {error}. Backup retained at: {}",
            backup.display()
        )
    })?;
    match fs::symlink_metadata(target) {
        Ok(metadata)
            if metadata.is_file()
                && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0 =>
        {
            let replacement = allowed_replacement.ok_or_else(|| {
                format!(
                    "The executable path now names a different file. Backup retained at: {}",
                    backup.display()
                )
            })?;
            let stage = backup
                .parent()
                .ok_or("The backup has no parent directory")?
                .join(STAGED_NAME);
            move_verified_no_replace(target, &stage, replacement, false).map_err(|error| {
                format!(
                    "Cannot safely retain the replacement during recovery: {error}. Backup retained at: {}",
                    backup.display()
                )
            })?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Ok(_) => {
            return Err(format!(
                "The executable path is no longer a regular file. Backup retained at: {}",
                backup.display()
            ));
        }
        Err(error) => {
            return Err(format!(
                "Cannot inspect the executable during recovery: {error}. Backup retained at: {}",
                backup.display()
            ));
        }
    }
    rename_open_file(&backup_guard, target).map_err(|error| {
        format!(
            "Cannot restore the original executable: {error}. Backup retained at: {}",
            backup.display()
        )
    })?;
    if !is_original(target, expected) {
        return Err(format!(
            "Cannot confirm the restored executable. Backup retained at: {}",
            backup.display()
        ));
    }
    Ok(())
}

#[derive(Debug)]
enum LaunchFailure {
    Spawn(String),
    Handoff(String),
    Unsafe(String),
}

impl std::fmt::Display for LaunchFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(message) | Self::Handoff(message) | Self::Unsafe(message) => {
                formatter.write_str(message)
            }
        }
    }
}

#[derive(Clone, Copy)]
enum LaunchImage<'a> {
    Original(&'a FileIdentity),
    Replacement(&'a FileIdentity),
}

// CreatePipe makes synchronous anonymous handles. Unlike std's child pipes,
// these do not depend on its private overlapped-I/O implementation.
fn create_protocol_pipe() -> io::Result<(File, File)> {
    let mut read = std::ptr::null_mut();
    let mut write = std::ptr::null_mut();
    // SAFETY: both output pointers are valid; null security attributes create
    // non-inheritable handles, which Command duplicates for child stdio.
    if unsafe { CreatePipe(&mut read, &mut write, std::ptr::null(), 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a successful CreatePipe returns two independently owned handles.
    Ok(unsafe { (File::from_raw_handle(read), File::from_raw_handle(write)) })
}

fn set_pipe_nonblocking_write(write: &File) -> io::Result<()> {
    let mode = PIPE_NOWAIT;
    // SAFETY: write is a live anonymous pipe handle, which Windows explicitly
    // permits here; its byte stream type is unchanged.
    if unsafe {
        SetNamedPipeHandleState(
            write.as_raw_handle().cast(),
            &mode,
            std::ptr::null(),
            std::ptr::null(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn pause_before_deadline(deadline: Instant, interval: Duration) -> bool {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return false;
    }
    thread::sleep(remaining.min(interval));
    true
}

fn write_pipe_until(write: &mut File, mut bytes: &[u8], deadline: Instant) -> Result<(), String> {
    let mut chunk_size = MAX_PIPE_WRITE_CHUNK;
    while !bytes.is_empty() {
        if Instant::now() >= deadline {
            return Err("Cleanup handoff timed out while sending".into());
        }
        let requested = chunk_size.min(bytes.len());
        let count = match write.write(&bytes[..requested]) {
            Ok(count) => count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => 0,
            Err(error) => return Err(format!("Cannot send cleanup handoff: {error}")),
        };
        if count == 0 {
            // A full PIPE_NOWAIT buffer may accept none of a large write.
            // Reduce the request to make progress even on a smaller buffer.
            if requested > 1 {
                chunk_size = requested / 2;
            } else if !pause_before_deadline(deadline, PIPE_WRITE_POLL_INTERVAL) {
                return Err("Cleanup handoff timed out while sending".into());
            }
        } else {
            bytes = &bytes[count..];
            chunk_size = (chunk_size * 2).min(MAX_PIPE_WRITE_CHUNK);
        }
    }
    Ok(())
}

fn read_pipe_response_until(
    read: &mut File,
    expected: &str,
    deadline: Instant,
) -> Result<(), String> {
    let mut line = [0_u8; MAX_HANDSHAKE_LINE as usize];
    let mut length = 0;
    loop {
        if Instant::now() >= deadline {
            return Err("Cleanup acknowledgement timed out".into());
        }
        let mut available = 0_u32;
        // SAFETY: read is the sole reader of this live pipe and available is
        // writable; no data buffer is requested.
        if unsafe {
            PeekNamedPipe(
                read.as_raw_handle().cast(),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                &mut available,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(format!(
                "Cannot read cleanup acknowledgement: {}",
                io::Error::last_os_error()
            ));
        }
        if available == 0 {
            if !pause_before_deadline(deadline, POLL_INTERVAL) {
                return Err("Cleanup acknowledgement timed out".into());
            }
            continue;
        }
        let count = (available as usize).min(line.len() - length);
        let received = read
            .read(&mut line[length..length + count])
            .map_err(|error| format!("Cannot read cleanup acknowledgement: {error}"))?;
        if received == 0 {
            return Err("The restarted process closed its cleanup acknowledgement".into());
        }
        length += received;
        if let Some(end) = line[..length].iter().position(|byte| *byte == b'\n') {
            let response = std::str::from_utf8(&line[..=end])
                .map_err(|_| "Invalid cleanup acknowledgement".to_string())?;
            if response.trim_end_matches(['\r', '\n']) == expected {
                return Ok(());
            }
            return Err(format!("Cleanup handoff was rejected: {}", response.trim()));
        }
        if length == line.len() {
            return Err("Cleanup acknowledgement is too long".into());
        }
    }
}

fn launch(
    target: &Path,
    config: &HelperConfig,
    helper: &Path,
    directory: &Path,
    mode: cleanup::TakeoverMode,
    admitted: LaunchImage<'_>,
    admitted_stage: Option<&FileIdentity>,
) -> Result<(), LaunchFailure> {
    // Keep this handle open through CreateProcess so neither an in-place
    // writer nor a rename can substitute the executable we just admitted.
    let (target_guard, info) =
        open_regular(target, FILE_SHARE_READ).map_err(LaunchFailure::Unsafe)?;
    let actual = identity_from_info(target, &info).map_err(LaunchFailure::Unsafe)?;
    match admitted {
        LaunchImage::Original(expected) if &actual != expected => {
            return Err(LaunchFailure::Unsafe(
                "The original executable identity changed".into(),
            ));
        }
        LaunchImage::Replacement(expected) => {
            if !same_file_id(&actual, expected) {
                return Err(LaunchFailure::Unsafe(
                    "The replacement executable identity changed".into(),
                ));
            }
            let verified = verify_staged(target, config.size, &config.sha256)
                .map_err(LaunchFailure::Unsafe)?;
            if !same_file_id(&actual, &verified) {
                return Err(LaunchFailure::Unsafe(
                    "The replacement executable changed during verification".into(),
                ));
            }
        }
        LaunchImage::Original(_) => {}
    }
    let backup_identity =
        optional_identity(&directory.join(BACKUP_NAME)).map_err(LaunchFailure::Unsafe)?;
    match admitted {
        LaunchImage::Original(_) if backup_identity.is_some() => {
            return Err(LaunchFailure::Unsafe(
                "An unexpected backup appeared before restart".into(),
            ));
        }
        LaunchImage::Replacement(_)
            if backup_identity.as_ref() != Some(&config.target_identity) =>
        {
            return Err(LaunchFailure::Unsafe(
                "The original backup identity changed before restart".into(),
            ));
        }
        _ => {}
    }
    let prepare = || -> Result<_, String> {
        let nonce = random_nonce()?;
        let takeover = cleanup::TakeoverConfig {
            version: cleanup::PROTOCOL_VERSION,
            helper_pid: std::process::id(),
            // SAFETY: this pseudo-handle is valid for the current process.
            helper_created: process_creation(unsafe { GetCurrentProcess() })?,
            helper_path: wide(helper.as_os_str()),
            helper_identity: file_identity(helper)?,
            directory_identity: cleanup::directory_identity(directory)?,
            stage_identity: authorized_stage_identity(
                &directory.join(STAGED_NAME),
                admitted_stage,
                config.size,
                &config.sha256,
            )?,
            backup_identity,
            target: wide(target.as_os_str()),
            target_identity: actual.clone(),
            nonce,
            locale: config.locale.clone(),
            mode,
            previous_env: std::env::var_os(cleanup::ENV_MARKER).map(|value| wide(&value)),
        };
        let bytes = serde_json::to_vec(&takeover)
            .map_err(|error| format!("Cannot encode cleanup handoff: {error}"))?;
        if bytes.len() > MAX_CONFIG {
            return Err("Cleanup handoff arguments are too large".into());
        }
        let cwd = path_from_wide(&config.working_directory)?;
        if config
            .arguments
            .iter()
            .any(|argument| argument.contains(&0))
        {
            return Err("Invalid original application argument".into());
        }
        Ok((nonce, bytes, cwd))
    };
    let (nonce, bytes, cwd) = prepare().map_err(LaunchFailure::Spawn)?;
    let (child_stdin, mut stdin) = create_protocol_pipe().map_err(|error| {
        LaunchFailure::Spawn(format!("Cannot create cleanup handoff input: {error}"))
    })?;
    let (mut stdout, child_stdout) = create_protocol_pipe().map_err(|error| {
        LaunchFailure::Spawn(format!("Cannot create cleanup handoff output: {error}"))
    })?;
    set_pipe_nonblocking_write(&stdin).map_err(|error| {
        LaunchFailure::Spawn(format!("Cannot configure cleanup handoff input: {error}"))
    })?;
    let mut command = Command::new(target);
    command
        .current_dir(cwd)
        .env(cleanup::ENV_MARKER, cleanup::marker_value(mode, &nonce))
        .stdin(Stdio::from(child_stdin))
        .stdout(Stdio::from(child_stdout))
        .stderr(Stdio::null());
    for argument in &config.arguments {
        command.arg(OsString::from_wide(argument));
    }
    let child = command.spawn().map_err(|error| {
        LaunchFailure::Spawn(format!("Cannot restart {}: {error}", target.display()))
    })?;
    drop(command);
    drop(target_guard);
    let pid = child.id();
    let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
    write_pipe_until(&mut stdin, &(bytes.len() as u32).to_le_bytes(), deadline)
        .and_then(|()| write_pipe_until(&mut stdin, &bytes, deadline))
        .map_err(|error| LaunchFailure::Handoff(format!("Started process {pid}, but {error}")))?;
    drop(stdin);
    let expected = response(cleanup::READY_LABEL, &nonce);
    read_pipe_response_until(&mut stdout, &expected, deadline)
        .map_err(|error| LaunchFailure::Handoff(format!("Started process {pid}, but {error}")))?;

    // A late ACK cannot authorize deletion: publish this nonce only after the
    // ACK was observed. The child checks it after this helper has exited.
    let mut marker = tempfile::NamedTempFile::new_in(directory).map_err(|error| {
        LaunchFailure::Handoff(format!(
            "Started process {pid}, but cannot stage cleanup authorization: {error}"
        ))
    })?;
    marker
        .write_all(&nonce)
        .and_then(|()| marker.as_file().sync_all())
        .map_err(|error| {
            LaunchFailure::Handoff(format!(
                "Started process {pid}, but cannot flush cleanup authorization: {error}"
            ))
        })?;
    marker
        .persist_noclobber(directory.join(cleanup::COMMIT_NAME))
        .map_err(|error| {
            LaunchFailure::Handoff(format!(
                "Started process {pid}, but cannot publish cleanup authorization: {}",
                error.error
            ))
        })?;
    Ok(())
}

#[derive(Debug)]
struct InstallFailure {
    message: String,
    preserve_backup: bool,
    cleanup_taken: bool,
}

fn backup_present(path: &Path) -> bool {
    !matches!(fs::symlink_metadata(path), Err(error) if error.kind() == io::ErrorKind::NotFound)
}

fn install_after_exit(
    config: &HelperConfig,
    target: &Path,
    final_target: &Path,
    directory: &Path,
    admitted_stage: &FileIdentity,
) -> Result<(), InstallFailure> {
    let stage = directory.join(STAGED_NAME);
    let backup = directory.join(BACKUP_NAME);
    let helper = directory.join(HELPER_NAME);
    if !is_original(target, &config.target_identity) {
        return Err(InstallFailure {
            message: "The executable changed while the application was closing; the update was not installed".into(),
            preserve_backup: backup_present(&backup),
            cleanup_taken: false,
        });
    }
    if backup_present(&backup) {
        return Err(InstallFailure {
            message: "An unexpected backup already exists in the private update directory".into(),
            preserve_backup: true,
            cleanup_taken: false,
        });
    }
    let verified_stage = verify_staged(&stage, config.size, &config.sha256).and_then(|identity| {
        if same_file_id(&identity, admitted_stage) {
            Ok(identity)
        } else {
            Err("The staged executable identity changed; the update was not installed".into())
        }
    });
    let stage_identity = match verified_stage {
        Ok(identity) => identity,
        Err(error) => {
            let result = launch(
                target,
                config,
                &helper,
                directory,
                cleanup::TakeoverMode::Restart,
                LaunchImage::Original(&config.target_identity),
                None,
            );
            let cleanup_taken = result.is_ok();
            return Err(InstallFailure {
                message: match result {
                    Err(restart) => format!("{error}. {restart}"),
                    Ok(()) => error,
                },
                preserve_backup: backup_present(&backup),
                cleanup_taken,
            });
        }
    };
    let restart = |path: &Path| {
        let current = file_identity(path).map_err(LaunchFailure::Unsafe)?;
        let admitted = if current == config.target_identity {
            LaunchImage::Original(&config.target_identity)
        } else if same_file_id(&current, &stage_identity) {
            LaunchImage::Replacement(&stage_identity)
        } else {
            return Err(LaunchFailure::Unsafe(
                "The executable path now names a different file".into(),
            ));
        };
        launch(
            path,
            config,
            &helper,
            directory,
            cleanup::TakeoverMode::Restart,
            admitted,
            Some(&stage_identity),
        )
    };
    publish_replacement(
        (target, final_target),
        &stage,
        &backup,
        &config.target_identity,
        &stage_identity,
        replace_file,
        restart,
    )
}

fn recovery_failure(
    target: &Path,
    backup: &Path,
    identity: &FileIdentity,
    stage_identity: &FileIdentity,
    reason: String,
    launch: &mut impl FnMut(&Path) -> Result<(), LaunchFailure>,
) -> InstallFailure {
    match restore_original(target, backup, identity, Some(stage_identity)) {
        Err(recovery) => InstallFailure {
            message: format!("{reason}. {recovery}"),
            preserve_backup: true,
            cleanup_taken: false,
        },
        Ok(()) if backup_present(backup) => InstallFailure {
            message: format!(
                "{reason}. An unexpected backup remains at {}",
                backup.display()
            ),
            preserve_backup: true,
            cleanup_taken: false,
        },
        Ok(()) => match launch(target) {
            Ok(()) => InstallFailure {
                message: format!("{reason}. The original application was restored."),
                preserve_backup: false,
                cleanup_taken: true,
            },
            Err(restart) => InstallFailure {
                message: format!(
                    "{reason}. The original executable was restored at {}. {restart}",
                    target.display()
                ),
                preserve_backup: backup_present(backup),
                cleanup_taken: false,
            },
        },
    }
}

fn restore_renamed_original(
    target: &Path,
    final_target: &Path,
    stage: &Path,
    backup: &Path,
    identity: &FileIdentity,
    stage_identity: &FileIdentity,
) -> Result<(), String> {
    let backup_guard = open_rename_source(backup, identity, true)?;
    match fs::symlink_metadata(target) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Ok(_) if same_directory_entry(target, final_target) => {}
        Err(error) => return Err(format!("Cannot inspect the original path: {error}")),
        Ok(_) => return Err("The original executable path is occupied".into()),
    }
    let replacement = open_rename_source(final_target, stage_identity, false)?;
    rename_open_file(&replacement, stage)?;
    rename_open_file(&backup_guard, target)?;
    if !is_original(target, identity) {
        return Err("Cannot confirm the restored original executable".into());
    }
    Ok(())
}

fn publish_replacement(
    (target, final_target): (&Path, &Path),
    stage: &Path,
    backup: &Path,
    identity: &FileIdentity,
    stage_identity: &FileIdentity,
    mut replace: impl FnMut(&Path, &Path, Option<&Path>) -> Result<(), String>,
    mut launch: impl FnMut(&Path) -> Result<(), LaunchFailure>,
) -> Result<(), InstallFailure> {
    // Revalidate after hashing too, directly before the destructive OS call.
    if !is_original(target, identity) {
        return Err(InstallFailure {
            message: "The executable changed while the application was closing; the update was not installed".into(),
            preserve_backup: backup_present(backup),
            cleanup_taken: false,
        });
    }
    if backup_present(backup) {
        return Err(InstallFailure {
            message: "An unexpected backup already exists in the private update directory".into(),
            preserve_backup: true,
            cleanup_taken: false,
        });
    }
    if !file_identity(stage).is_ok_and(|current| same_file_id(&current, stage_identity)) {
        return Err(InstallFailure {
            message: "The staged executable identity changed; the update was not installed".into(),
            preserve_backup: false,
            cleanup_taken: false,
        });
    }
    let destination = match resolve_destination(target, final_target) {
        Ok(destination) => destination,
        Err(error) => {
            return Err(recovery_failure(
                target,
                backup,
                identity,
                stage_identity,
                error,
                &mut launch,
            ));
        }
    };
    if let Err(error) = replace(target, stage, Some(backup)) {
        return Err(recovery_failure(
            target,
            backup,
            identity,
            stage_identity,
            format!("The update could not replace the executable: {error}"),
            &mut launch,
        ));
    }
    if !file_identity(target).is_ok_and(|current| same_file_id(&current, stage_identity))
        || !is_original(backup, identity)
    {
        return Err(InstallFailure {
            message: "The replacement or backup identity changed; no application was launched"
                .into(),
            preserve_backup: true,
            cleanup_taken: false,
        });
    }
    if destination != Destination::InPlace {
        let renamed = match destination {
            Destination::SameEntry => {
                open_rename_source(target, stage_identity, false).and_then(|replacement| {
                    // The destination is an alias for the current entry, so
                    // make both moves without replacement while retaining the
                    // same identity-pinned handle.
                    rename_open_file(&replacement, stage)?;
                    rename_open_file(&replacement, final_target)
                })
            }
            Destination::Vacant => {
                move_verified_no_replace(target, final_target, stage_identity, false)
            }
            Destination::InPlace => unreachable!(),
        };
        if let Err(error) = renamed {
            return Err(recovery_failure(
                target,
                backup,
                identity,
                stage_identity,
                format!("The update could not be renamed: {error}"),
                &mut launch,
            ));
        }
        let original_path_safe = match destination {
            Destination::SameEntry => {
                same_directory_entry(target, final_target)
                    && directory_entry_name(final_target).as_deref() == final_target.file_name()
            }
            Destination::Vacant => !backup_present(target),
            Destination::InPlace => unreachable!(),
        };
        if !file_identity(final_target).is_ok_and(|current| same_file_id(&current, stage_identity))
            || !is_original(backup, identity)
            || !original_path_safe
        {
            return Err(InstallFailure {
                message: "The renamed executable, original path or backup changed; no application was launched".into(),
                preserve_backup: true,
                cleanup_taken: false,
            });
        }
    }
    match launch(final_target) {
        Ok(()) => Ok(()),
        Err(LaunchFailure::Spawn(error)) => {
            let reason = format!("The updated application could not start: {error}");
            if destination != Destination::InPlace
                && let Err(recovery) = restore_renamed_original(
                    target,
                    final_target,
                    stage,
                    backup,
                    identity,
                    stage_identity,
                )
            {
                return Err(InstallFailure {
                    message: format!("{reason}. Cannot safely restore the original: {recovery}"),
                    preserve_backup: true,
                    cleanup_taken: false,
                });
            }
            Err(recovery_failure(
                target,
                backup,
                identity,
                stage_identity,
                reason,
                &mut launch,
            ))
        }
        Err(LaunchFailure::Handoff(error)) => Err(InstallFailure {
            message: format!(
                "The updated application started, but cleanup handoff failed: {error}. No rollback was attempted; the original backup is at {}.",
                backup.display()
            ),
            preserve_backup: true,
            cleanup_taken: false,
        }),
        Err(LaunchFailure::Unsafe(error)) => Err(InstallFailure {
            message: format!(
                "The executable could not be safely launched: {error}. No rollback was attempted; the original backup is at {}.",
                backup.display()
            ),
            preserve_backup: true,
            cleanup_taken: false,
        }),
    }
}

fn show_install_failure(locale: &str, error: &str) {
    let message = rust_i18n::t!(
        "app.settings.update.install_error",
        locale = locale,
        error = error
    )
    .to_string();
    show_failure(&message);
}

pub(crate) fn show_failure(message: &str) {
    let text: Vec<u16> = OsString::from(message)
        .encode_wide()
        .chain(Some(0))
        .collect();
    let title: Vec<u16> = OsString::from("SlimeNRF Helper")
        .encode_wide()
        .chain(Some(0))
        .collect();
    // SAFETY: strings are NUL-terminated, and a null owner is valid here.
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONERROR | MB_TOPMOST,
        );
    }
}

#[cfg(test)]
mod tests;
