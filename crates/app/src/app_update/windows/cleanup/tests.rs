use super::super::{HELPER_NAME, wide};
use super::*;
use std::{io::Cursor, os::windows::ffi::OsStrExt, time::Duration};
use windows_sys::Win32::System::Ioctl::FSCTL_SET_REPARSE_POINT;

const NONCE: [u8; 16] = [0x3a; 16];
const HELPER_BYTES: &[u8] = b"isolated helper image";
const TARGET_BYTES: &[u8] = b"isolated target image";
const STAGE_BYTES: &[u8] = b"isolated staged image";
const BACKUP_BYTES: &[u8] = b"isolated original backup";

struct Fixture {
    root: tempfile::TempDir,
    directory: PathBuf,
    helper: PathBuf,
    target: PathBuf,
    config: TakeoverConfig,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("create isolated cleanup fixture");
        let directory = root.path().join(".slimenrf-update-fixture");
        fs::create_dir(&directory).expect("create isolated update directory");
        let target = root.path().join("application.exe");
        let helper = directory.join(HELPER_NAME);
        fs::write(&target, TARGET_BYTES).expect("write isolated target");
        fs::write(&helper, HELPER_BYTES).expect("write isolated helper");
        fs::write(directory.join(STAGED_NAME), STAGE_BYTES).expect("write isolated stage");
        fs::write(directory.join(BACKUP_NAME), BACKUP_BYTES).expect("write isolated backup");
        let config = TakeoverConfig {
            version: PROTOCOL_VERSION,
            helper_pid: 1234,
            helper_created: 5678,
            helper_path: wide(helper.as_os_str()),
            helper_identity: file_identity(&helper).expect("identify isolated helper"),
            directory_identity: directory_identity(&directory)
                .expect("identify isolated directory"),
            stage_identity: Some(file_identity(&directory.join(STAGED_NAME)).unwrap()),
            backup_identity: Some(file_identity(&directory.join(BACKUP_NAME)).unwrap()),
            target: wide(target.as_os_str()),
            target_identity: file_identity(&target).expect("identify isolated target"),
            nonce: NONCE,
            locale: "en".into(),
            mode: TakeoverMode::Restart,
            previous_env: Some(vec![0x61, 0xd800]),
        };
        Self {
            root,
            directory,
            helper,
            target,
            config,
        }
    }

    fn paths(&self) -> AuthorizedPaths {
        authorize_paths(&self.config, &self.target).expect("authorize isolated paths")
    }

    fn commit(&self, bytes: &[u8]) {
        fs::write(self.directory.join(COMMIT_NAME), bytes).expect("write isolated commit");
    }

    fn assert_original_files(&self) {
        assert_eq!(fs::read(&self.target).unwrap(), TARGET_BYTES);
        assert_eq!(fs::read(&self.helper).unwrap(), HELPER_BYTES);
        assert_eq!(
            fs::read(self.directory.join(STAGED_NAME)).unwrap(),
            STAGE_BYTES
        );
        assert_eq!(
            fs::read(self.directory.join(BACKUP_NAME)).unwrap(),
            BACKUP_BYTES
        );
    }
}

fn frame(bytes: &[u8]) -> Vec<u8> {
    let mut framed = Vec::with_capacity(4 + bytes.len());
    framed.extend_from_slice(&u32::try_from(bytes.len()).unwrap().to_le_bytes());
    framed.extend_from_slice(bytes);
    framed
}

#[test]
fn marker_is_exact_versioned_and_does_not_claim_unrelated_environment_values() {
    for mode in [TakeoverMode::Restart, TakeoverMode::CleanupOnly] {
        let value = marker_value(mode, &NONCE);
        assert_eq!(parse_marker(OsStr::new(&value)), Ok(Some((mode, NONCE))));
    }
    for ordinary in [
        "",
        "user value",
        "SLIMENRF-CLEANUP",
        "other-SLIMENRF-CLEANUP-V1-ONLY",
    ] {
        assert_eq!(parse_marker(OsStr::new(ordinary)), Ok(None), "{ordinary:?}");
    }
    let valid = marker_value(TakeoverMode::Restart, &NONCE);
    for malformed in [
        "SLIMENRF-CLEANUP-V2-RESTART 3a3a3a3a3a3a3a3a3a3a3a3a3a3a3a3a".to_string(),
        "SLIMENRF-CLEANUP-V1-RESTART".to_string(),
        format!("{valid} "),
        valid.to_uppercase(),
        valid[..valid.len() - 1].to_string(),
        format!("{}g", &valid[..valid.len() - 1]),
    ] {
        assert!(
            parse_marker(OsStr::new(&malformed)).is_err(),
            "{malformed:?}"
        );
    }
    let mut invalid_wide: Vec<u16> = "SLIMENRF-CLEANUP-V1-ONLY ".encode_utf16().collect();
    invalid_wide.push(0xd800);
    assert!(parse_marker(&OsString::from_wide(&invalid_wide)).is_err());
    assert_eq!(parse_marker(&OsString::from_wide(&[0xd800])), Ok(None));
}

#[test]
fn framed_config_is_bounded_strict_and_binds_version_mode_nonce_and_environment() {
    let fixture = Fixture::new();
    let bytes = serde_json::to_vec(&fixture.config).unwrap();
    let mut framed = frame(&bytes);
    framed.extend_from_slice(b"next message");
    let mut input = Cursor::new(framed);
    let parsed = read_config(&mut input).expect("read complete versioned config");
    assert_eq!(
        serde_json::to_value(&parsed).unwrap(),
        serde_json::to_value(&fixture.config).unwrap()
    );
    assert_eq!(
        &input.get_ref()[input.position() as usize..],
        b"next message"
    );
    assert!(check_handoff(&parsed, TakeoverMode::Restart, &NONCE).is_ok());
    assert!(check_handoff(&parsed, TakeoverMode::CleanupOnly, &NONCE).is_err());
    assert!(check_handoff(&parsed, TakeoverMode::Restart, &[0x3b; 16]).is_err());

    for length in 0..4 {
        assert!(read_config(&mut Cursor::new(&frame(&bytes)[..length])).is_err());
    }
    assert!(read_config(&mut Cursor::new(&frame(&bytes)[..bytes.len() + 3])).is_err());
    for length in [0_u32, u32::try_from(MAX_CONFIG + 1).unwrap()] {
        let mut input = Cursor::new(length.to_le_bytes());
        assert!(read_config(&mut input).is_err());
        assert_eq!(
            input.position(),
            4,
            "invalid length is rejected before a body read"
        );
    }
    for invalid in [b"null".as_slice(), b"{}", b"{", b"\xff"] {
        assert!(read_config(&mut Cursor::new(frame(invalid))).is_err());
    }
    for field in [
        "version",
        "stage_identity",
        "backup_identity",
        "previous_env",
    ] {
        let mut value = serde_json::to_value(&fixture.config).unwrap();
        value.as_object_mut().unwrap().remove(field);
        let bytes = serde_json::to_vec(&value).unwrap();
        assert!(
            read_config(&mut Cursor::new(frame(&bytes))).is_err(),
            "missing {field}"
        );
    }
    let mut value = serde_json::to_value(&fixture.config).unwrap();
    value["unexpected"] = serde_json::json!(true);
    assert!(
        read_config(&mut Cursor::new(frame(
            &serde_json::to_vec(&value).unwrap()
        )))
        .is_err()
    );
    let mut value = serde_json::to_value(&fixture.config).unwrap();
    value["version"] = serde_json::json!(2);
    let unsupported = read_config(&mut Cursor::new(frame(
        &serde_json::to_vec(&value).unwrap(),
    )))
    .unwrap();
    assert!(check_handoff(&unsupported, TakeoverMode::Restart, &NONCE).is_err());
    let mut value = serde_json::to_value(&fixture.config).unwrap();
    value["previous_env"] = serde_json::json!([65, 0, 66]);
    let invalid_env = read_config(&mut Cursor::new(frame(
        &serde_json::to_vec(&value).unwrap(),
    )))
    .unwrap();
    assert!(check_handoff(&invalid_env, TakeoverMode::Restart, &NONCE).is_err());
}

#[test]
fn paths_are_bound_to_the_sibling_directory_exact_image_and_frozen_identities() {
    let mut fixture = Fixture::new();
    let paths = fixture.paths();
    let renamed = fixture.root.path().join(".slimenrf-update-moved");
    assert!(
        fs::rename(&fixture.directory, &renamed).is_err(),
        "guard denies directory rename"
    );
    drop(paths);
    fs::rename(&fixture.directory, &renamed).expect("released guard permits directory rename");
    fs::rename(&renamed, &fixture.directory).expect("restore isolated directory path");

    let alias = fixture.root.path().join("hardlink.exe");
    fs::hard_link(&fixture.target, &alias).expect("create isolated executable hardlink");
    assert_eq!(
        file_identity(&alias).unwrap(),
        fixture.config.target_identity
    );
    assert!(
        authorize_paths(&fixture.config, &alias).is_err(),
        "a different path is not the running image"
    );
    fixture.config.directory_identity.index ^= 1;
    assert!(authorize_paths(&fixture.config, &fixture.target).is_err());
    fixture.config.directory_identity.index ^= 1;
    fixture.config.target_identity.index ^= 1;
    assert!(authorize_paths(&fixture.config, &fixture.target).is_err());
    fixture.config.target_identity.index ^= 1;

    let renamed_helper = fixture.directory.join("unrelated-helper.exe");
    fs::copy(&fixture.helper, &renamed_helper).expect("copy isolated helper");
    fixture.config.helper_path = wide(renamed_helper.as_os_str());
    assert!(
        authorize_paths(&fixture.config, &fixture.target).is_err(),
        "helper name is fixed"
    );
}

#[test]
fn directory_or_helper_replacement_revokes_path_authorization() {
    let mut fixture = Fixture::new();
    let old_helper = fixture.directory.join("old-helper.exe");
    fs::rename(&fixture.helper, &old_helper).expect("move isolated helper aside");
    fs::copy(&old_helper, &fixture.helper).expect("substitute identical helper bytes");
    assert_ne!(
        file_identity(&fixture.helper).unwrap(),
        fixture.config.helper_identity
    );
    assert!(authorize_paths(&fixture.config, &fixture.target).is_err());

    fixture.config.helper_identity = file_identity(&fixture.helper).unwrap();
    let old_directory = fixture.root.path().join(".slimenrf-update-original");
    fs::rename(&fixture.directory, &old_directory).expect("move isolated directory aside");
    fs::create_dir(&fixture.directory).expect("create substituted directory");
    fs::copy(old_directory.join(HELPER_NAME), &fixture.helper).expect("substitute helper pathname");
    fixture.config.helper_identity = file_identity(&fixture.helper).unwrap();
    assert_ne!(
        directory_identity(&fixture.directory).unwrap(),
        fixture.config.directory_identity
    );
    assert!(authorize_paths(&fixture.config, &fixture.target).is_err());
}

#[test]
fn a_staging_directory_must_be_next_to_the_target() {
    let mut fixture = Fixture::new();
    let other_parent = fixture.root.path().join("other-parent");
    fs::create_dir(&other_parent).unwrap();
    let other_directory = other_parent.join(".slimenrf-update-other");
    fs::create_dir(&other_directory).unwrap();
    let other_helper = other_directory.join(HELPER_NAME);
    fs::copy(&fixture.helper, &other_helper).unwrap();
    fixture.config.helper_path = wide(other_helper.as_os_str());
    fixture.config.helper_identity = file_identity(&other_helper).unwrap();
    fixture.config.directory_identity = directory_identity(&other_directory).unwrap();
    assert!(authorize_paths(&fixture.config, &fixture.target).is_err());
}

#[test]
fn absent_or_inexact_commit_removes_nothing_and_retains_backup() {
    for commit in [
        None,
        Some(vec![0x3b; 16]),
        Some(vec![0x3a; 15]),
        Some(vec![0x3a; 17]),
    ] {
        let fixture = Fixture::new();
        if let Some(bytes) = &commit {
            fixture.commit(bytes);
        }
        let result = cleanup_committed(fixture.paths(), &fixture.config);
        assert!(
            result.is_err(),
            "commit {commit:?} must not authorize deletion"
        );
        fixture.assert_original_files();
        if let Some(bytes) = commit {
            assert_eq!(
                fs::read(fixture.directory.join(COMMIT_NAME)).unwrap(),
                bytes
            );
        }
    }
}

#[test]
fn changed_guard_or_helper_prevents_all_committed_deletion() {
    let mut fixture = Fixture::new();
    fixture.commit(&NONCE);
    let paths = fixture.paths();
    fixture.config.directory_identity.created ^= 1;
    assert!(cleanup_committed(paths, &fixture.config).is_err());
    fixture.config.directory_identity.created ^= 1;
    fixture.assert_original_files();
    assert_eq!(
        fs::read(fixture.directory.join(COMMIT_NAME)).unwrap(),
        NONCE
    );

    let paths = fixture.paths();
    let old_helper = fixture.directory.join("old-helper.exe");
    fs::rename(&fixture.helper, &old_helper).unwrap();
    fs::copy(&old_helper, &fixture.helper).unwrap();
    assert!(cleanup_committed(paths, &fixture.config).is_err());
    fixture.assert_original_files();
    assert_eq!(
        fs::read(fixture.directory.join(COMMIT_NAME)).unwrap(),
        NONCE
    );
    assert_eq!(fs::read(old_helper).unwrap(), HELPER_BYTES);
}

#[test]
fn substituted_stage_or_backup_is_never_deleted() {
    for name in [STAGED_NAME, BACKUP_NAME] {
        let fixture = Fixture::new();
        fixture.commit(&NONCE);
        let paths = fixture.paths();
        let original = fixture.directory.join(format!("saved-{name}"));
        let replaced = fixture.directory.join(name);
        fs::rename(&replaced, &original).unwrap();
        // Identical bytes still belong to a different file object.
        fs::copy(&original, &replaced).unwrap();
        assert!(cleanup_committed(paths, &fixture.config).is_err(), "{name}");
        assert_eq!(fs::read(&replaced).unwrap(), fs::read(&original).unwrap());
        fixture.assert_original_files();
        assert_eq!(
            fs::read(fixture.directory.join(COMMIT_NAME)).unwrap(),
            NONCE
        );
    }
}

#[test]
fn optional_file_presence_is_frozen_and_missing_files_are_allowed_only_if_expected() {
    for name in [STAGED_NAME, BACKUP_NAME] {
        for expected_missing in [false, true] {
            let mut fixture = Fixture::new();
            fixture.commit(&NONCE);
            if expected_missing {
                if name == STAGED_NAME {
                    fixture.config.stage_identity = None;
                } else {
                    fixture.config.backup_identity = None;
                }
            } else {
                fs::remove_file(fixture.directory.join(name)).unwrap();
            }
            assert!(
                cleanup_committed(fixture.paths(), &fixture.config).is_err(),
                "{name}, expected_missing={expected_missing}"
            );
            assert_eq!(fs::read(&fixture.helper).unwrap(), HELPER_BYTES);
            assert_eq!(
                fs::read(fixture.directory.join(COMMIT_NAME)).unwrap(),
                NONCE
            );
            if expected_missing {
                assert!(fixture.directory.join(name).exists());
            }
        }
    }

    let mut fixture = Fixture::new();
    fs::remove_file(fixture.directory.join(STAGED_NAME)).unwrap();
    fixture.config.stage_identity = None;
    fixture.commit(&NONCE);
    cleanup_committed(fixture.paths(), &fixture.config)
        .expect("frozen absence permits removal of the other authorized files");
    assert!(!fixture.directory.exists());
}

#[test]
fn valid_commit_removes_only_fixed_files_then_the_empty_directory() {
    let fixture = Fixture::new();
    fixture.commit(&NONCE);
    cleanup_committed(fixture.paths(), &fixture.config)
        .expect("remove authorized isolated update files");
    assert!(!fixture.directory.exists());
    assert_eq!(fs::read(&fixture.target).unwrap(), TARGET_BYTES);
    assert!(fixture.root.path().exists());
}

#[test]
fn unrelated_file_is_never_removed_and_prevents_directory_removal() {
    let fixture = Fixture::new();
    let unrelated = fixture.directory.join("user-document.txt");
    fs::write(&unrelated, b"keep this user data").unwrap();
    fixture.commit(&NONCE);
    let error = cleanup_committed(fixture.paths(), &fixture.config)
        .expect_err("nonempty staging directory reports a cleanup failure");
    assert!(error.contains(&fixture.directory.display().to_string()));
    assert_eq!(fs::read(&unrelated).unwrap(), b"keep this user data");
    assert_eq!(fs::read(&fixture.target).unwrap(), TARGET_BYTES);
    for name in [HELPER_NAME, STAGED_NAME, BACKUP_NAME, COMMIT_NAME] {
        assert!(
            !fixture.directory.join(name).exists(),
            "only fixed names are removed"
        );
    }
}

#[link(name = "kernel32")]
unsafe extern "system" {
    #[link_name = "DeviceIoControl"]
    fn device_io_control(
        device: *mut std::ffi::c_void,
        code: u32,
        input: *const std::ffi::c_void,
        input_len: u32,
        output: *mut std::ffi::c_void,
        output_len: u32,
        returned: *mut u32,
        overlapped: *mut std::ffi::c_void,
    ) -> i32;
}

// A directory junction is a reparse point that needs no symlink privilege.
fn junction(link: &Path, destination: &Path) {
    let destination = fs::canonicalize(destination).unwrap();
    let destination: Vec<u16> = destination.as_os_str().encode_wide().collect();
    let prefix: Vec<u16> = r"\\?\".encode_utf16().collect();
    assert!(destination.starts_with(&prefix));
    let mut substitute: Vec<u16> = r"\??\".encode_utf16().collect();
    substitute.extend_from_slice(&destination[prefix.len()..]);
    let name_len = u16::try_from(substitute.len() * 2).unwrap();
    let mut buffer = Vec::new();
    buffer.extend_from_slice(&0xa000_0003_u32.to_le_bytes());
    buffer.extend_from_slice(&(name_len + 12).to_le_bytes());
    buffer.extend_from_slice(&0_u16.to_le_bytes());
    buffer.extend_from_slice(&0_u16.to_le_bytes());
    buffer.extend_from_slice(&name_len.to_le_bytes());
    buffer.extend_from_slice(&(name_len + 2).to_le_bytes());
    buffer.extend_from_slice(&0_u16.to_le_bytes());
    for unit in substitute {
        buffer.extend_from_slice(&unit.to_le_bytes());
    }
    buffer.extend_from_slice(&[0; 4]);
    fs::create_dir(link).unwrap();
    let handle = OpenOptions::new()
        .write(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
        .open(link)
        .expect("open isolated junction directory");
    let mut returned = 0;
    // SAFETY: the handle and initialized mount-point buffer are valid for this
    // synchronous control call; no output buffer or overlapped state is used.
    let result = unsafe {
        device_io_control(
            handle.as_raw_handle().cast(),
            FSCTL_SET_REPARSE_POINT,
            buffer.as_ptr().cast(),
            u32::try_from(buffer.len()).unwrap(),
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    assert_ne!(
        result,
        0,
        "create isolated junction: {}",
        io::Error::last_os_error()
    );
}

#[test]
fn reparse_points_cannot_authorize_or_redirect_cleanup() {
    let fixture = Fixture::new();
    let destination = fixture.root.path().join("do-not-delete");
    fs::create_dir(&destination).unwrap();
    let sentinel = destination.join("sentinel.txt");
    fs::write(&sentinel, b"outside data").unwrap();
    let link = fixture.root.path().join(".slimenrf-update-junction");
    junction(&link, &destination);
    assert!(
        directory_identity(&link).is_err(),
        "reparse directory is rejected"
    );
    fs::remove_dir(&link).unwrap();

    fixture.commit(&NONCE);
    fs::remove_file(fixture.directory.join(STAGED_NAME)).unwrap();
    let stage_link = fixture.directory.join(STAGED_NAME);
    junction(&stage_link, &destination);
    assert!(cleanup_committed(fixture.paths(), &fixture.config).is_err());
    assert_eq!(fs::read(&sentinel).unwrap(), b"outside data");
    assert_eq!(fs::read(&fixture.helper).unwrap(), HELPER_BYTES);
    assert_eq!(
        fs::read(fixture.directory.join(BACKUP_NAME)).unwrap(),
        BACKUP_BYTES
    );
    assert_eq!(
        fs::read(fixture.directory.join(COMMIT_NAME)).unwrap(),
        NONCE
    );
    fs::remove_dir(&stage_link).unwrap();
}

#[test]
fn worker_drop_joins_the_owned_thread() {
    let (entered, wait_for_entered) = mpsc::channel();
    let (release, wait_for_release) = mpsc::sync_channel::<()>(0);
    let worker_thread = thread::spawn(move || {
        entered.send(()).unwrap();
        wait_for_release.recv().unwrap();
    });
    wait_for_entered.recv().unwrap();
    let worker = CleanupWorker {
        thread: Some(worker_thread),
        directory: PathBuf::from("isolated-test-only"),
        locale: "en".into(),
    };
    let (finished, wait_for_finished) = mpsc::channel();
    let dropper = thread::spawn(move || {
        drop(worker);
        finished.send(()).unwrap();
    });
    assert!(matches!(
        wait_for_finished.recv_timeout(Duration::from_millis(50)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    release.send(()).unwrap();
    wait_for_finished
        .recv_timeout(Duration::from_secs(5))
        .expect("drop joins worker");
    dropper.join().unwrap();
}

#[test]
fn worker_drop_joins_during_panic_unwinding() {
    let (entered, wait_for_entered) = mpsc::channel();
    let (release, wait_for_release) = mpsc::sync_channel::<()>(0);
    let worker_thread = thread::spawn(move || {
        entered.send(()).unwrap();
        wait_for_release.recv().unwrap();
    });
    wait_for_entered.recv().unwrap();
    let worker = CleanupWorker {
        thread: Some(worker_thread),
        directory: PathBuf::from("isolated-test-only"),
        locale: "en".into(),
    };
    let (unwinding, wait_for_unwinding) = mpsc::channel();
    let (finished, wait_for_finished) = mpsc::channel();
    let dropper = thread::spawn(move || {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _worker = worker;
            unwinding.send(()).unwrap();
            panic!("exercise cleanup guard unwinding");
        }));
        finished.send(outcome.is_err()).unwrap();
    });
    wait_for_unwinding.recv().unwrap();
    assert!(matches!(
        wait_for_finished.recv_timeout(Duration::from_millis(50)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    release.send(()).unwrap();
    assert!(
        wait_for_finished
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
    );
    dropper.join().unwrap();
}

#[test]
fn readiness_failure_never_acknowledges_and_joins_the_cancelled_worker() {
    for timed_out in [false, true] {
        let (ready, readiness) = mpsc::sync_channel::<()>(1);
        let ready = if timed_out {
            Some(ready)
        } else {
            drop(ready);
            None
        };
        let (start, started) = mpsc::sync_channel::<()>(1);
        let (finished, worker_result) = mpsc::channel();
        let worker_thread = thread::spawn(move || {
            let _ = finished.send(started.recv().is_err());
        });
        let worker = CleanupWorker {
            thread: Some(worker_thread),
            directory: PathBuf::from("isolated-test-only"),
            locale: "en".into(),
        };
        let mut output = Vec::new();
        let result = acknowledge_worker(
            worker,
            readiness,
            start,
            &mut output,
            &NONCE,
            Duration::ZERO,
        );
        assert!(result.is_err(), "timed_out={timed_out}");
        assert!(output.is_empty(), "no ACK before worker readiness");
        assert_eq!(
            worker_result.try_recv(),
            Ok(true),
            "worker was cancelled and joined"
        );
        drop(ready);
    }
}

#[test]
fn failed_ack_cancels_ready_worker_without_waiting_for_helper() {
    struct BrokenOutput;
    impl Write for BrokenOutput {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "isolated broken pipe",
            ))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let (ready, readiness) = mpsc::sync_channel(1);
    let (start, started) = mpsc::sync_channel::<()>(1);
    let (finished, worker_result) = mpsc::channel();
    let worker_thread = thread::spawn(move || {
        if ready.send(()).is_ok() {
            let _ = finished.send(started.recv().is_err());
        }
    });
    let worker = CleanupWorker {
        thread: Some(worker_thread),
        directory: PathBuf::from("isolated-test-only"),
        locale: "en".into(),
    };
    let result = acknowledge_worker(
        worker,
        readiness,
        start,
        &mut BrokenOutput,
        &NONCE,
        Duration::from_secs(5),
    );
    assert!(result.is_err());
    assert_eq!(
        worker_result.try_recv(),
        Ok(true),
        "ready worker was cancelled and joined"
    );
}

#[test]
fn worker_is_activated_only_after_ready_ack_is_flushed() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    struct RecordingOutput {
        bytes: Vec<u8>,
        flushed: Arc<AtomicBool>,
    }
    impl Write for RecordingOutput {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            self.flushed.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    let flushed = Arc::new(AtomicBool::new(false));
    let worker_flushed = Arc::clone(&flushed);
    let (ready, readiness) = mpsc::sync_channel(1);
    let (start, started) = mpsc::sync_channel::<()>(1);
    let (finished, worker_result) = mpsc::channel();
    let worker_thread = thread::spawn(move || {
        if ready.send(()).is_ok() && started.recv().is_ok() {
            let _ = finished.send(worker_flushed.load(Ordering::SeqCst));
        }
    });
    let worker = CleanupWorker {
        thread: Some(worker_thread),
        directory: PathBuf::from("isolated-test-only"),
        locale: "en".into(),
    };
    let mut output = RecordingOutput {
        bytes: Vec::new(),
        flushed,
    };
    let worker = acknowledge_worker(
        worker,
        readiness,
        start,
        &mut output,
        &NONCE,
        Duration::from_secs(5),
    )
    .unwrap();
    assert_eq!(
        output.bytes,
        format!("{}\n", response(READY_LABEL, &NONCE)).into_bytes()
    );
    assert_eq!(worker_result.recv_timeout(Duration::from_secs(5)), Ok(true));
    drop(worker);
}
