use super::*;
use std::{io::Cursor, os::windows::fs::FileTimesExt, time::SystemTime};
use windows_sys::Win32::{
    Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, GetShortPathNameW,
    },
    System::{
        Ioctl::FSCTL_SET_REPARSE_POINT,
        Pipes::{CreatePipe, PeekNamedPipe},
    },
};

const ORIGINAL: &[u8] = b"synthetic original application";
const REPLACEMENT: &[u8] = b"synthetic replacement application";

struct Fixture {
    root: tempfile::TempDir,
    directory: PathBuf,
    target: PathBuf,
    stage: PathBuf,
    backup: PathBuf,
    identity: FileIdentity,
    stage_identity: FileIdentity,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("create isolated update fixture");
        let directory = root.path().join(".slimenrf-update-fixture");
        fs::create_dir(&directory).expect("create private staging directory");
        let target = root.path().join("application.exe");
        let stage = directory.join(STAGED_NAME);
        let backup = directory.join(BACKUP_NAME);
        fs::write(&target, ORIGINAL).expect("write original fixture");
        fs::write(&stage, REPLACEMENT).expect("write staged fixture");
        let identity = file_identity(&target).expect("identify original fixture");
        let stage_identity = file_identity(&stage).expect("identify staged fixture");
        Self {
            root,
            directory,
            target,
            stage,
            backup,
            identity,
            stage_identity,
        }
    }
}

fn config() -> HelperConfig {
    HelperConfig {
        parent_pid: 1234,
        parent_created: 4567,
        nonce: [0x42; 16],
        locale: "en".into(),
        target: wide(OsString::from(r"C:\fixture\application.exe").as_os_str()),
        final_name: wide(OsString::from("application.exe").as_os_str()),
        working_directory: wide(OsString::from(r"C:\fixture").as_os_str()),
        arguments: vec![wide(OsString::from("--example").as_os_str()), vec![0xd800]],
        target_identity: FileIdentity {
            volume: 7,
            index: 8,
            created: 12,
            size: 9,
            last_write: 10,
        },
        size: 11,
        sha256: [0x5a; 32],
    }
}

fn frame(bytes: &[u8]) -> Vec<u8> {
    let length = u32::try_from(bytes.len()).expect("fixture frame fits in u32");
    let mut framed = Vec::with_capacity(4 + bytes.len());
    framed.extend_from_slice(&length.to_le_bytes());
    framed.extend_from_slice(bytes);
    framed
}

struct HeaderOnly(Cursor<[u8; 4]>);

impl Read for HeaderOnly {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        assert!(
            self.0.position() < 4,
            "oversized frame must not read a body"
        );
        self.0.read(buffer)
    }
}

fn pipe() -> (File, File) {
    pipe_with_size(0)
}

fn pipe_with_size(size: u32) -> (File, File) {
    let mut read = std::ptr::null_mut();
    let mut write = std::ptr::null_mut();
    // SAFETY: both output pointers are valid and default security attributes are allowed.
    let ok = unsafe { CreatePipe(&mut read, &mut write, std::ptr::null(), size) };
    assert_ne!(
        ok,
        0,
        "create anonymous pipe: {}",
        io::Error::last_os_error()
    );
    // SAFETY: a successful CreatePipe returns two distinct owned handles.
    unsafe {
        (
            File::from_raw_handle(read.cast()),
            File::from_raw_handle(write.cast()),
        )
    }
}

fn try_queued_pipe_bytes(reader: &File) -> io::Result<u32> {
    let mut available = 0;
    // SAFETY: reader owns a live pipe handle and available is a valid output pointer.
    let ok = unsafe {
        PeekNamedPipe(
            reader.as_raw_handle().cast(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            &mut available,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(available)
}

fn queued_pipe_bytes(reader: &File) -> u32 {
    try_queued_pipe_bytes(reader).expect("inspect anonymous pipe")
}

fn assert_protocol_pipe_alive(reader: &mut File, writer: &mut File) {
    let queued = queued_pipe_bytes(reader);
    let mut pending = vec![0; usize::try_from(queued).expect("pipe byte count fits usize")];
    reader
        .read_exact(&mut pending)
        .expect("drain any remaining response bytes");

    let deadline = Instant::now() + Duration::from_secs(5);
    write_pipe_until(writer, b"still alive\n", deadline).expect("write through retained peer");
    read_pipe_response_until(reader, "still alive", deadline).expect("read through retained peer");
}

fn assert_timed_out_at_deadline(deadline: Instant, finished: Instant, error: &str) {
    assert!(error.contains("timed out"), "unexpected error: {error}");
    assert!(
        finished >= deadline,
        "returned before the deadline: {error}"
    );
    assert!(
        finished.duration_since(deadline) < Duration::from_secs(5),
        "timeout exceeded scheduling tolerance by {:?}: {error}",
        finished.duration_since(deadline)
    );
}

#[link(name = "kernel32")]
unsafe extern "system" {
    #[link_name = "DeviceIoControl"]
    fn device_io_control(
        device: HANDLE,
        code: u32,
        input: *const std::ffi::c_void,
        input_len: u32,
        output: *mut std::ffi::c_void,
        output_len: u32,
        returned: *mut u32,
        overlapped: *mut std::ffi::c_void,
    ) -> i32;
}

// A mount-point junction requires neither Developer Mode nor symlink privilege.
fn junction(link: &Path, destination: &Path) {
    const MOUNT_POINT_TAG: u32 = 0xa000_0003;
    let destination = fs::canonicalize(destination).expect("resolve junction destination");
    let destination: Vec<u16> = destination.as_os_str().encode_wide().collect();
    let extended_prefix: Vec<u16> = r"\\?\".encode_utf16().collect();
    assert!(
        destination.starts_with(&extended_prefix),
        "expected an extended Windows path"
    );
    let mut substitute: Vec<u16> = r"\??\".encode_utf16().collect();
    substitute.extend_from_slice(&destination[extended_prefix.len()..]);
    let name_len = u16::try_from(substitute.len() * 2).expect("junction name fits");
    let data_len = name_len.checked_add(12).expect("junction data fits");
    let print_offset = name_len.checked_add(2).expect("junction print offset fits");

    let mut buffer = Vec::new();
    buffer.extend_from_slice(&MOUNT_POINT_TAG.to_le_bytes());
    buffer.extend_from_slice(&data_len.to_le_bytes());
    buffer.extend_from_slice(&0_u16.to_le_bytes());
    buffer.extend_from_slice(&0_u16.to_le_bytes());
    buffer.extend_from_slice(&name_len.to_le_bytes());
    buffer.extend_from_slice(&print_offset.to_le_bytes());
    buffer.extend_from_slice(&0_u16.to_le_bytes());
    for unit in substitute {
        buffer.extend_from_slice(&unit.to_le_bytes());
    }
    buffer.extend_from_slice(&[0; 4]);

    fs::create_dir(link).expect("create junction directory");
    let handle = OpenOptions::new()
        .write(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
        .open(link)
        .expect("open junction directory without following it");
    let mut returned = 0;
    // SAFETY: the owned directory handle and the complete mount-point buffer are
    // valid for the synchronous control call; no output or OVERLAPPED is used.
    let ok = unsafe {
        device_io_control(
            handle.as_raw_handle().cast(),
            FSCTL_SET_REPARSE_POINT,
            buffer.as_ptr().cast(),
            u32::try_from(buffer.len()).expect("junction buffer fits"),
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    assert_ne!(ok, 0, "create junction: {}", io::Error::last_os_error());
    let metadata = fs::symlink_metadata(link).expect("inspect junction");
    assert_ne!(metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT, 0);
}

#[test]
fn private_mode_only_accepts_the_exact_standalone_helper_flag() {
    let ordinary = [
        vec![],
        vec![OsString::from("--version")],
        vec![OsString::from("ordinary--slimenrf-internal-update-helper")],
        vec![OsString::from_wide(&[0xd800])],
    ];
    for arguments in ordinary {
        assert_eq!(
            private_mode(&arguments),
            PrivateMode::Ordinary,
            "{arguments:?}"
        );
        assert_eq!(dispatch(&arguments), None, "{arguments:?}");
    }
    assert_eq!(
        private_mode(&[OsString::from(HELPER_FLAG)]),
        PrivateMode::Helper
    );

    let mut malformed_wide: Vec<u16> = INTERNAL_PREFIX.encode_utf16().collect();
    malformed_wide.push(0xd800);
    let invalid = [
        vec![OsString::from(INTERNAL_PREFIX)],
        vec![OsString::from(format!("{HELPER_FLAG}=extra"))],
        vec![OsString::from(format!("{INTERNAL_PREFIX}unknown"))],
        vec![OsString::from(HELPER_FLAG), OsString::from("extra")],
        vec![OsString::from("extra"), OsString::from(HELPER_FLAG)],
        vec![OsString::from(HELPER_FLAG), OsString::from(HELPER_FLAG)],
        vec![OsString::from_wide(&malformed_wide)],
    ];
    for arguments in invalid {
        assert_eq!(
            private_mode(&arguments),
            PrivateMode::Invalid,
            "{arguments:?}"
        );
        assert_eq!(dispatch(&arguments), Some(2), "{arguments:?}");
    }
}

#[test]
fn acknowledgements_are_bound_to_the_full_nonce_and_label() {
    let nonce = [0x12; 16];
    let other_nonce = [0x13; 16];
    assert_eq!(
        response(ACK, &nonce),
        "COMMITTED 12121212121212121212121212121212"
    );
    assert_ne!(response(READY, &nonce), response(ACK, &nonce));
    assert_ne!(response(ACK, &nonce), response(ACK, &other_nonce));

    for (incoming, accepted) in [
        (response(ACK, &nonce), true),
        (response(ACK, &other_nonce), false),
        (response(READY, &nonce), false),
        (format!("{} extra", response(ACK, &nonce)), false),
    ] {
        let (sender, events) = mpsc::channel();
        sender.send(Ok(incoming.clone())).expect("queue response");
        drop(sender);
        let helper = Helper {
            child: None,
            stdin: None,
            events,
            reader: None,
            directory: None,
            nonce,
        };
        assert_eq!(
            helper.wait_event(&response(ACK, &nonce)).is_ok(),
            accepted,
            "incoming response {incoming:?}"
        );
    }
}

#[test]
fn cleanup_ack_timeout_keeps_both_pipe_ends_usable() {
    let (mut reader, mut writer) = create_protocol_pipe().expect("create cleanup response pipe");
    set_pipe_nonblocking_write(&writer).expect("set response writer nonblocking");
    let expected = response(cleanup::READY_LABEL, &[0x42; 16]);

    let deadline = Instant::now() + Duration::from_millis(200);
    let result = read_pipe_response_until(&mut reader, &expected, deadline);
    let finished = Instant::now();
    let error = result.expect_err("a retained writer that withholds its ACK must time out");
    assert_timed_out_at_deadline(deadline, finished, &error);

    let late_ack = format!("{expected}\n");
    let deadline = Instant::now() + Duration::from_secs(5);
    write_pipe_until(&mut writer, late_ack.as_bytes(), deadline)
        .expect("write a late ACK through the retained peer");
    read_pipe_response_until(&mut reader, &expected, deadline)
        .expect("accept the late ACK through the original reader");
}

#[test]
fn cleanup_handoff_write_timeout_preserves_unread_pipe_and_writer() {
    let (mut reader, mut writer) = create_protocol_pipe().expect("create cleanup input pipe");
    set_pipe_nonblocking_write(&writer).expect("set handoff writer nonblocking");
    let seed = b"retained handoff prefix\n";
    write_pipe_until(&mut writer, seed, Instant::now() + Duration::from_secs(5))
        .expect("seed the unread pipe with a bounded write");

    // A large PIPE_NOWAIT write may make no progress even on an empty pipe.
    // Fill with small writes and probe a single byte to establish saturation.
    let fill = [0x5a; 512];
    let fill_deadline = Instant::now() + Duration::from_secs(5);
    let mut filled = 0_usize;
    let write_available = |writer: &mut File, bytes: &[u8]| match writer.write(bytes) {
        Ok(count) => count,
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => 0,
        Err(error) => panic!("cannot fill nonblocking pipe: {error}"),
    };
    loop {
        assert!(Instant::now() < fill_deadline, "pipe prefill timed out");
        let mut count = write_available(&mut writer, &fill);
        if count == 0 {
            count = write_available(&mut writer, &fill[..1]);
            if count == 0 {
                break;
            }
        }
        filled += count;
        assert!(
            filled <= MAX_CONFIG * 4,
            "fixture pipe exceeded the bounded prefill"
        );
    }
    let initially_queued = seed.len() + filled;
    assert_eq!(
        usize::try_from(queued_pipe_bytes(&reader)).expect("pipe byte count fits usize"),
        initially_queued,
        "the seed and filler must be queued before the timeout"
    );

    let bytes = vec![0x5a; MAX_CONFIG * 4];
    let deadline = Instant::now() + Duration::from_millis(200);
    let result = write_pipe_until(&mut writer, &bytes, deadline);
    let finished = Instant::now();
    let error = result.expect_err("an unread full pipe must time out");
    assert_timed_out_at_deadline(deadline, finished, &error);

    let queued = usize::try_from(queued_pipe_bytes(&reader)).expect("pipe byte count fits usize");
    assert!(
        queued >= initially_queued,
        "the timed-out write must not consume previously queued bytes"
    );
    let mut received = vec![0; queued];
    reader
        .read_exact(&mut received)
        .expect("drain the retained reader");
    assert_eq!(
        &received[..seed.len()],
        seed,
        "the seeded bytes must survive the timed-out write"
    );
    assert!(
        received[seed.len()..].iter().all(|byte| *byte == 0x5a),
        "the remaining bytes must belong to the original handoff"
    );
    assert_protocol_pipe_alive(&mut reader, &mut writer);
}

#[test]
fn cleanup_handoff_streams_64k_to_a_live_reader() {
    const PAYLOAD_LEN: usize = 64 * 1024;
    let payload: Vec<u8> = (0_u16..32 * 1024).flat_map(u16::to_le_bytes).collect();
    let (mut reader, writer) = create_protocol_pipe().expect("create cleanup input pipe");
    set_pipe_nonblocking_write(&writer).expect("set handoff writer nonblocking");
    let (ready_sender, ready_receiver) = mpsc::channel();

    thread::scope(|scope| {
        let peer = scope.spawn(move || -> Result<Vec<u8>, String> {
            let deadline = Instant::now() + Duration::from_secs(15);
            ready_sender
                .send(())
                .map_err(|error| format!("cannot signal reader readiness: {error}"))?;
            drop(ready_sender);

            let mut received = Vec::with_capacity(PAYLOAD_LEN);
            let mut buffer = [0_u8; 4096];
            while received.len() < PAYLOAD_LEN {
                if Instant::now() >= deadline {
                    return Err(format!("reader timed out after {} bytes", received.len()));
                }
                let available = try_queued_pipe_bytes(&reader)
                    .map_err(|error| format!("cannot inspect handoff pipe: {error}"))?;
                if available == 0 {
                    thread::sleep(Duration::from_millis(1));
                    continue;
                }
                // This thread is the sole reader, so a read capped at the peeked
                // byte count cannot wait for another thread to supply data.
                let count = usize::try_from(available)
                    .map_err(|error| format!("invalid pipe byte count: {error}"))?
                    .min(buffer.len())
                    .min(PAYLOAD_LEN - received.len());
                let count = reader
                    .read(&mut buffer[..count])
                    .map_err(|error| format!("cannot read handoff pipe: {error}"))?;
                if count == 0 {
                    return Err("handoff pipe closed before the complete payload".into());
                }
                received.extend_from_slice(&buffer[..count]);
            }
            Ok(received)
        });

        let mut writer = Some(writer);
        let result = match ready_receiver.recv_timeout(Duration::from_secs(5)) {
            Ok(()) => write_pipe_until(
                writer.as_mut().expect("writer is retained for handoff"),
                &payload,
                Instant::now() + Duration::from_secs(10),
            ),
            Err(error) => Err(format!("reader did not become ready: {error}")),
        };
        drop(ready_receiver);
        if result.is_err() {
            drop(writer.take());
        }
        // The peer owns the only reader and bounds its own polling; join it even
        // after a failed write before asserting or releasing the final writer.
        let received = peer.join();
        drop(writer);
        result.expect("stream the complete 64 KiB handoff before the deadline");
        let received = received
            .expect("reader thread must not panic")
            .expect("reader must receive the complete handoff before its deadline");
        assert_eq!(
            received, payload,
            "the live peer must receive every byte in order"
        );
    });
}

#[test]
fn cleanup_ack_reader_accepts_only_the_expected_complete_line() {
    let nonce = [0x42; 16];
    let expected = response(cleanup::READY_LABEL, &nonce);
    for (incoming, accepted) in [
        (format!("{expected}\n"), true),
        (format!("{expected}\r\n"), true),
        (
            format!("{}\n", response(cleanup::READY_LABEL, &[0x43; 16])),
            false,
        ),
        (format!("{}\n", response(ACK, &nonce)), false),
        (format!("{expected} extra\n"), false),
        ("\n".to_string(), false),
    ] {
        let (mut reader, mut writer) =
            create_protocol_pipe().expect("create cleanup response pipe");
        set_pipe_nonblocking_write(&writer).expect("set response writer nonblocking");
        let deadline = Instant::now() + Duration::from_secs(5);
        write_pipe_until(&mut writer, incoming.as_bytes(), deadline)
            .expect("enqueue complete response");
        let result = read_pipe_response_until(&mut reader, &expected, deadline);
        assert_eq!(
            result.is_ok(),
            accepted,
            "response {incoming:?} returned {result:?}"
        );
        if let Err(error) = result {
            assert!(
                !error.contains("timed out"),
                "complete malformed response must be rejected without waiting: {error}"
            );
        }
        assert_protocol_pipe_alive(&mut reader, &mut writer);
    }
}

#[test]
fn cleanup_ack_reader_bounds_unterminated_and_oversized_responses() {
    let expected = response(cleanup::READY_LABEL, &[0x42; 16]);
    let max_line = usize::try_from(MAX_HANDSHAKE_LINE).expect("line limit fits usize");
    let mut at_limit = expected.as_bytes().to_vec();
    at_limit.resize(max_line, b'\r');

    // A full buffer with no newline cannot become a valid bounded response.
    let (mut reader, mut writer) = create_protocol_pipe().expect("create cleanup response pipe");
    set_pipe_nonblocking_write(&writer).expect("set response writer nonblocking");
    let deadline = Instant::now() + Duration::from_secs(5);
    write_pipe_until(&mut writer, &at_limit, deadline).expect("enqueue full unterminated line");
    let error = read_pipe_response_until(&mut reader, &expected, deadline)
        .expect_err("a line without a terminator at the limit must be rejected");
    assert!(error.contains("too long"), "unexpected error: {error}");
    assert_protocol_pipe_alive(&mut reader, &mut writer);

    // Make the pipe large enough to enqueue an entire over-limit line without a peer thread.
    let mut oversized = at_limit;
    oversized.push(b'\n');
    let capacity = u32::try_from(oversized.len() * 2).expect("fixture capacity fits u32");
    let (mut reader, mut writer) = pipe_with_size(capacity);
    set_pipe_nonblocking_write(&writer).expect("set response writer nonblocking");
    let deadline = Instant::now() + Duration::from_secs(5);
    write_pipe_until(&mut writer, &oversized, deadline).expect("enqueue over-limit line");
    let error = read_pipe_response_until(&mut reader, &expected, deadline)
        .expect_err("a terminated line beyond the limit must be rejected");
    assert!(error.contains("too long"), "unexpected error: {error}");
    assert_protocol_pipe_alive(&mut reader, &mut writer);
}

#[test]
fn commit_requires_the_opcode_and_exact_nonce_on_the_real_pipe() {
    let nonce = [0x37; 16];
    let mut valid = [0_u8; 17];
    valid[0] = COMMIT;
    valid[1..].copy_from_slice(&nonce);
    let mut wrong_command = valid;
    wrong_command[0] ^= 1;
    let mut wrong_nonce = valid;
    wrong_nonce[16] ^= 1;

    for (bytes, accepted) in [(valid, true), (wrong_command, false), (wrong_nonce, false)] {
        let (mut reader, mut writer) = pipe();
        writer.write_all(&bytes).expect("queue complete commit");
        let pipe_handle = reader.as_raw_handle().cast();
        // SAFETY: the current-process pseudo-handle remains valid throughout the call.
        let parent = unsafe { GetCurrentProcess() };
        assert_eq!(
            read_commit(&mut reader, pipe_handle, parent, &nonce),
            accepted
        );
        drop(writer);
    }

    let (mut reader, writer) = pipe();
    drop(writer);
    let pipe_handle = reader.as_raw_handle().cast();
    // SAFETY: the current-process pseudo-handle remains valid throughout the call.
    let parent = unsafe { GetCurrentProcess() };
    assert!(
        !read_commit(&mut reader, pipe_handle, parent, &nonce),
        "EOF cannot commit"
    );
}

#[test]
fn config_reader_round_trips_and_stops_at_the_framed_boundary() {
    let expected = config();
    let bytes = serde_json::to_vec(&expected).expect("encode config fixture");
    let mut framed = frame(&bytes);
    framed.extend_from_slice(b"next message");
    let mut input = Cursor::new(framed);
    let actual = read_config(&mut input).expect("read valid framed config");
    assert_eq!(
        serde_json::to_value(actual).unwrap(),
        serde_json::to_value(expected).unwrap()
    );
    assert_eq!(
        &input.get_ref()[input.position() as usize..],
        b"next message"
    );
}

#[test]
fn config_reader_rejects_truncation_malformed_json_and_out_of_bounds_lengths() {
    let expected = config();
    let bytes = serde_json::to_vec(&expected).expect("encode config fixture");
    let valid = frame(&bytes);
    for length in 0..4 {
        assert!(
            read_config(&mut Cursor::new(&valid[..length])).is_err(),
            "header length {length}"
        );
    }
    for length in [4, valid.len() - 1] {
        assert!(
            read_config(&mut Cursor::new(&valid[..length])).is_err(),
            "frame length {length}"
        );
    }
    for bytes in [b"{".as_slice(), b"null", b"{}", b"\xff"] {
        assert!(
            read_config(&mut Cursor::new(frame(bytes))).is_err(),
            "malformed {bytes:?}"
        );
    }
    for length in [0_u32, u32::try_from(MAX_CONFIG + 1).unwrap()] {
        let mut input = HeaderOnly(Cursor::new(length.to_le_bytes()));
        let error = read_config(&mut input).expect_err("invalid length is rejected");
        assert!(
            error.contains("Invalid update handoff length"),
            "declared length {length}"
        );
        assert_eq!(
            input.0.position(),
            4,
            "reject the length before reading a body"
        );
    }

    let mut at_limit = bytes;
    at_limit.resize(MAX_CONFIG, b' ');
    let actual =
        read_config(&mut Cursor::new(frame(&at_limit))).expect("maximum frame is accepted");
    assert_eq!(
        serde_json::to_value(actual).unwrap(),
        serde_json::to_value(expected).unwrap()
    );
}

#[test]
fn wide_paths_reject_empty_or_embedded_nul_without_losing_windows_units() {
    assert!(path_from_wide(&[]).is_err());
    assert!(path_from_wide(&[b'a' as u16, 0, b'b' as u16]).is_err());
    let path = [b'a' as u16, 0xd800];
    assert_eq!(wide(path_from_wide(&path).unwrap().as_os_str()), path);
}

#[test]
fn file_identity_tracks_file_object_and_content_changes() {
    let fixture = Fixture::new();
    assert_eq!(file_identity(&fixture.target).unwrap(), fixture.identity);
    let hardlink = fixture.root.path().join("same-file.exe");
    fs::hard_link(&fixture.target, &hardlink).expect("create same-volume hard link");
    assert_eq!(file_identity(&hardlink).unwrap(), fixture.identity);

    let copy = fixture.root.path().join("copy.exe");
    fs::copy(&fixture.target, &copy).expect("copy fixture");
    assert_ne!(file_identity(&copy).unwrap(), fixture.identity);
    let renamed = fixture.root.path().join("renamed.exe");
    fs::rename(&fixture.target, &renamed).expect("rename original fixture");
    assert_eq!(file_identity(&renamed).unwrap(), fixture.identity);
    fs::write(&renamed, b"changed size").expect("change original fixture");
    assert!(!is_original(&renamed, &fixture.identity));
    assert!(file_identity(&fixture.target).is_err());
    assert!(regular_file(&fixture.directory).is_err());
}

#[test]
fn captured_running_image_copies_original_bytes_and_denies_in_place_writes() {
    let fixture = Fixture::new();
    let image = capture_image(&fixture.target).expect("capture synthetic running image");
    assert!(
        OpenOptions::new()
            .write(true)
            .open(&fixture.target)
            .is_err(),
        "the retained image handle must deny in-place writes"
    );
    let helper = fixture.directory.join(HELPER_NAME);
    copy_running_helper(&image, &helper).expect("copy the captured running image");
    assert_eq!(fs::read(&helper).unwrap(), ORIGINAL);
    assert!(is_original(&fixture.target, &fixture.identity));
    assert_eq!(fs::read(&fixture.stage).unwrap(), REPLACEMENT);
}

#[test]
fn replaced_running_image_path_cannot_supply_a_helper() {
    for replacement in [REPLACEMENT, ORIGINAL] {
        let fixture = Fixture::new();
        let image = capture_image(&fixture.target).expect("capture synthetic running image");
        let original = fixture.root.path().join("retained-original.exe");
        fs::rename(&fixture.target, &original).expect("move the retained image aside");
        fs::write(&fixture.target, replacement).expect("create a different file at the old path");
        assert_ne!(file_identity(&fixture.target).unwrap(), fixture.identity);
        let helper = fixture.directory.join(HELPER_NAME);
        let result = copy_running_helper(&image, &helper);
        assert!(
            result.is_err(),
            "a substituted image must not supply a helper"
        );
        assert!(
            !helper.exists(),
            "a rejected substitution must not produce a helper"
        );
        assert!(is_original(&original, &fixture.identity));
        assert_eq!(fs::read(&fixture.target).unwrap(), replacement);
        assert_eq!(fs::read(&fixture.stage).unwrap(), REPLACEMENT);
    }
}

#[test]
fn private_layout_rejects_junction_staging_and_unrelated_targets() {
    let fixture = Fixture::new();
    let helper = fixture.directory.join(HELPER_NAME);
    fs::write(&helper, ORIGINAL).expect("write helper fixture");
    assert_eq!(private_directory(&helper).unwrap(), fixture.directory);
    assert!(validate_layout(&fixture.directory, &fixture.target).is_ok());
    assert!(private_directory(&fixture.directory.join("other.exe")).is_err());

    let elsewhere = tempfile::tempdir().expect("create unrelated directory");
    let other_target = elsewhere.path().join("application.exe");
    fs::write(&other_target, ORIGINAL).expect("write unrelated target");
    assert!(validate_layout(&fixture.directory, &other_target).is_err());

    let link = fixture.root.path().join(".slimenrf-update-junction");
    junction(&link, &fixture.directory);
    assert!(regular_file(&link).is_err());
    assert!(file_identity(&link).is_err());
    assert!(private_directory(&link.join(HELPER_NAME)).is_err());
    assert_eq!(fs::read(&helper).unwrap(), ORIGINAL);
}

#[test]
fn failed_preparation_keeps_original_and_does_not_start_a_helper() {
    let fixture = Fixture::new();
    let copied_stage = fixture.directory.join("copied-replacement.exe");
    copy_staged(&fixture.stage, &copied_stage).expect("copy downloaded bytes");
    let error = verify_staged(&copied_stage, REPLACEMENT.len() as u64, &[0; 32])
        .expect_err("a changed download must fail before helper startup");
    assert!(error.contains("SHA-256"), "{error}");
    assert!(is_original(&fixture.target, &fixture.identity));
    assert_eq!(fs::read(&fixture.target).unwrap(), ORIGINAL);
    assert_eq!(fs::read(&fixture.stage).unwrap(), REPLACEMENT);
    assert!(!fixture.directory.join(HELPER_NAME).exists());
    assert!(!fixture.backup.exists());
}

#[test]
fn changed_staged_hash_or_length_cannot_publish() {
    let size = u64::try_from(REPLACEMENT.len()).expect("fixture size fits");
    let digest: [u8; 32] = Sha256::digest(REPLACEMENT).into();
    for (name, declared_size, declared_digest, expected_error) in [
        ("wrong digest", size, [0; 32], "SHA-256"),
        ("longer file", size - 1, digest, "size changed"),
        ("shorter file", size + 1, digest, "SHA-256"),
    ] {
        let fixture = Fixture::new();
        let error = verify_staged(&fixture.stage, declared_size, &declared_digest)
            .expect_err("changed stage must be rejected");
        assert!(error.contains(expected_error), "{name}: {error}");

        let mut handoff = config();
        handoff.target = wide(fixture.target.as_os_str());
        handoff.working_directory = wide(fixture.root.path().as_os_str());
        handoff.arguments.clear();
        handoff.target_identity = fixture.identity.clone();
        handoff.size = declared_size;
        handoff.sha256 = declared_digest;
        // This synthetic staging directory has no helper, so restart preparation
        // fails before any process can be started.
        let failure = install_after_exit(
            &handoff,
            &fixture.target,
            &fixture.target,
            &fixture.directory,
            &fixture.stage_identity,
        )
        .expect_err("changed stage must not be published");
        assert!(
            failure.message.contains(expected_error),
            "{name}: {}",
            failure.message
        );
        assert!(!failure.preserve_backup);
        assert!(is_original(&fixture.target, &fixture.identity), "{name}");
        assert_eq!(fs::read(&fixture.stage).unwrap(), REPLACEMENT, "{name}");
        assert!(!fixture.backup.exists(), "{name}");
    }
}

#[test]
fn native_success_replaces_target_keeps_original_backup_and_launches_new() {
    let fixture = Fixture::new();
    let mut launches = Vec::new();
    publish_replacement(
        (&fixture.target, &fixture.target),
        &fixture.stage,
        &fixture.backup,
        &fixture.identity,
        &fixture.stage_identity,
        replace_file,
        |target| {
            launches.push(fs::read(target).expect("read launched fixture"));
            Ok(())
        },
    )
    .expect("publish replacement");
    assert_eq!(launches, vec![REPLACEMENT.to_vec()]);
    assert_eq!(fs::read(&fixture.target).unwrap(), REPLACEMENT);
    assert_eq!(fs::read(&fixture.backup).unwrap(), ORIGINAL);
    assert!(is_original(&fixture.backup, &fixture.identity));
    assert!(!fixture.stage.exists());
}

#[test]
fn replaced_target_swapped_before_launch_is_not_rolled_back_or_launched() {
    let fixture = Fixture::new();
    let displaced = fixture.root.path().join("displaced-replacement.exe");
    let unrelated = b"unrelated target must survive";
    let failure = publish_replacement(
        (&fixture.target, &fixture.target),
        &fixture.stage,
        &fixture.backup,
        &fixture.identity,
        &fixture.stage_identity,
        |target, stage, backup| {
            replace_file(target, stage, backup)?;
            fs::rename(target, &displaced).map_err(|error| error.to_string())?;
            fs::write(target, unrelated).map_err(|error| error.to_string())?;
            Ok(())
        },
        |_| panic!("an unrelated target must not be launched"),
    )
    .expect_err("a target swapped after replacement must be rejected");
    assert!(failure.preserve_backup);
    assert!(!failure.cleanup_taken);
    assert_eq!(fs::read(&fixture.target).unwrap(), unrelated);
    assert!(is_original(&fixture.backup, &fixture.identity));
    assert_eq!(fs::read(&fixture.backup).unwrap(), ORIGINAL);
    assert_eq!(fs::read(&displaced).unwrap(), REPLACEMENT);
    assert!(!fixture.stage.exists());
}

#[test]
fn identity_violation_during_launch_never_rolls_back_or_relaunches() {
    let fixture = Fixture::new();
    let displaced = fixture.root.path().join("displaced-replacement.exe");
    let unrelated = b"unrelated target must survive";
    let mut attempts = 0;
    let failure = publish_replacement(
        (&fixture.target, &fixture.target),
        &fixture.stage,
        &fixture.backup,
        &fixture.identity,
        &fixture.stage_identity,
        replace_file,
        |target| {
            attempts += 1;
            assert_eq!(fs::read(target).unwrap(), REPLACEMENT);
            fs::rename(target, &displaced).expect("displace admitted replacement");
            fs::write(target, unrelated).expect("install unrelated target fixture");
            Err(LaunchFailure::Unsafe("target identity changed".into()))
        },
    )
    .expect_err("unsafe launch identity must be reported");
    assert_eq!(
        attempts, 1,
        "identity failure must not start a second launch"
    );
    assert!(failure.preserve_backup);
    assert!(!failure.cleanup_taken);
    assert_eq!(fs::read(&fixture.target).unwrap(), unrelated);
    assert!(is_original(&fixture.backup, &fixture.identity));
    assert_eq!(fs::read(&fixture.backup).unwrap(), ORIGINAL);
    assert_eq!(fs::read(&displaced).unwrap(), REPLACEMENT);
}

#[test]
fn unchanged_replacement_failure_relaunches_original_without_losing_stage() {
    let fixture = Fixture::new();
    let mut launches = Vec::new();
    let failure = publish_replacement(
        (&fixture.target, &fixture.target),
        &fixture.stage,
        &fixture.backup,
        &fixture.identity,
        &fixture.stage_identity,
        |target, stage, backup| {
            assert_eq!(target, fixture.target.as_path());
            assert_eq!(stage, fixture.stage.as_path());
            assert_eq!(backup, Some(fixture.backup.as_path()));
            Err("simulated replacement failure".into())
        },
        |target| {
            launches.push(fs::read(target).expect("read relaunched original"));
            Ok(())
        },
    )
    .expect_err("replacement failure must be reported");
    assert!(failure.message.contains("simulated replacement failure"));
    assert!(!failure.preserve_backup);
    assert_eq!(launches, vec![ORIGINAL.to_vec()]);
    assert!(is_original(&fixture.target, &fixture.identity));
    assert_eq!(fs::read(&fixture.stage).unwrap(), REPLACEMENT);
    assert!(!fixture.backup.exists());
}

#[test]
fn no_replace_move_preserves_existing_target_and_moves_only_to_absent_path() {
    let mut fixture = Fixture::new();
    OpenOptions::new()
        .write(true)
        .open(&fixture.stage)
        .expect("open stage to set creation time")
        .set_times(fs::FileTimes::new().set_created(SystemTime::UNIX_EPOCH))
        .expect("set distinct stage creation time");
    fixture.stage_identity =
        file_identity(&fixture.stage).expect("identify stage after timestamp change");
    assert_ne!(fixture.stage_identity.created, fixture.identity.created);
    assert!(
        move_verified_no_replace(
            &fixture.stage,
            &fixture.target,
            &fixture.stage_identity,
            false
        )
        .is_err(),
        "a preexisting target must not be overwritten"
    );
    assert!(is_original(&fixture.target, &fixture.identity));
    assert!(is_original(&fixture.stage, &fixture.stage_identity));
    assert_eq!(fs::read(&fixture.target).unwrap(), ORIGINAL);
    assert_eq!(fs::read(&fixture.stage).unwrap(), REPLACEMENT);

    let displaced = fixture.root.path().join("displaced-original.exe");
    fs::rename(&fixture.target, &displaced).expect("move original fixture aside");
    move_verified_no_replace(
        &fixture.stage,
        &fixture.target,
        &fixture.stage_identity,
        false,
    )
    .expect("move stage to the now-absent target");
    assert!(is_original(&displaced, &fixture.identity));
    let moved = file_identity(&fixture.target).expect("identify moved stage");
    assert!(same_file_id(&moved, &fixture.stage_identity));
    assert_eq!(fs::read(&fixture.target).unwrap(), REPLACEMENT);
    assert!(!fixture.stage.exists());
}

#[test]
fn partial_1177_failure_restores_missing_target_from_backup() {
    let fixture = Fixture::new();
    let mut launches = Vec::new();
    let failure = publish_replacement(
        (&fixture.target, &fixture.target),
        &fixture.stage,
        &fixture.backup,
        &fixture.identity,
        &fixture.stage_identity,
        |target, _stage, backup| {
            fs::rename(target, backup.expect("backup destination"))
                .expect("simulate target moved to backup");
            Err("simulated ERROR_UNABLE_TO_MOVE_REPLACEMENT_2 (1177)".into())
        },
        |target| {
            launches.push(fs::read(target).expect("read restored original"));
            Ok(())
        },
    )
    .expect_err("partial replacement failure must be reported");
    assert!(failure.message.contains("1177"));
    assert!(!failure.preserve_backup);
    assert_eq!(launches, vec![ORIGINAL.to_vec()]);
    assert!(is_original(&fixture.target, &fixture.identity));
    assert_eq!(fs::read(&fixture.stage).unwrap(), REPLACEMENT);
    assert!(!fixture.backup.exists());
}

#[test]
fn failed_new_launch_rolls_back_existing_target_and_relaunches_original() {
    let fixture = Fixture::new();
    let mut launches = Vec::new();
    let failure = publish_replacement(
        (&fixture.target, &fixture.target),
        &fixture.stage,
        &fixture.backup,
        &fixture.identity,
        &fixture.stage_identity,
        replace_file,
        |target| {
            launches.push(fs::read(target).expect("read attempted launch"));
            if launches.len() == 1 {
                Err(LaunchFailure::Spawn("simulated new launch failure".into()))
            } else {
                Ok(())
            }
        },
    )
    .expect_err("new launch failure must be reported");
    assert!(failure.message.contains("simulated new launch failure"));
    assert!(!failure.preserve_backup);
    assert!(
        failure.cleanup_taken,
        "the relaunched original accepted cleanup"
    );
    assert_eq!(launches, vec![REPLACEMENT.to_vec(), ORIGINAL.to_vec()]);
    assert!(is_original(&fixture.target, &fixture.identity));
    assert_eq!(fs::read(&fixture.target).unwrap(), ORIGINAL);
    assert!(!fixture.backup.exists());
}

#[test]
fn failed_handoff_after_spawn_preserves_new_target_and_original_backup() {
    let fixture = Fixture::new();
    let mut launches = Vec::new();
    let failure = publish_replacement(
        (&fixture.target, &fixture.target),
        &fixture.stage,
        &fixture.backup,
        &fixture.identity,
        &fixture.stage_identity,
        replace_file,
        |target| {
            launches.push(fs::read(target).expect("read launched fixture"));
            Err(LaunchFailure::Handoff(
                "simulated post-spawn handoff failure".into(),
            ))
        },
    )
    .expect_err("handoff failure must be reported");
    assert!(
        failure
            .message
            .contains("simulated post-spawn handoff failure")
    );
    assert!(failure.preserve_backup);
    assert!(
        !failure.cleanup_taken,
        "the failed handoff did not transfer cleanup"
    );
    assert_eq!(
        launches,
        vec![REPLACEMENT.to_vec()],
        "do not relaunch the original"
    );
    assert_eq!(fs::read(&fixture.target).unwrap(), REPLACEMENT);
    assert!(is_original(&fixture.backup, &fixture.identity));
    assert!(!fixture.stage.exists());
    assert!(fixture.directory.exists());
    assert_eq!(fs::read(&fixture.backup).unwrap(), ORIGINAL);
    assert_eq!(fs::read(&fixture.target).unwrap(), REPLACEMENT);
}

#[test]
fn failed_original_relaunch_is_reported_after_successful_rollback() {
    let fixture = Fixture::new();
    let mut launches = Vec::new();
    let failure = publish_replacement(
        (&fixture.target, &fixture.target),
        &fixture.stage,
        &fixture.backup,
        &fixture.identity,
        &fixture.stage_identity,
        replace_file,
        |target| {
            launches.push(fs::read(target).expect("read attempted launch"));
            Err(LaunchFailure::Spawn(
                if launches.len() == 1 {
                    "new launch failed"
                } else {
                    "old launch failed"
                }
                .into(),
            ))
        },
    )
    .expect_err("both launch failures must be reported");
    assert!(failure.message.contains("new launch failed"));
    assert!(failure.message.contains("old launch failed"));
    assert_eq!(launches, vec![REPLACEMENT.to_vec(), ORIGINAL.to_vec()]);
    assert!(is_original(&fixture.target, &fixture.identity));
    assert_eq!(fs::read(&fixture.target).unwrap(), ORIGINAL);
    assert!(!failure.preserve_backup);
    assert!(fixture.directory.exists());
    assert!(!failure.cleanup_taken);
}

#[test]
fn changed_target_identity_prevents_replacement_and_preserves_existing_backup() {
    for existing_backup in [false, true] {
        let fixture = Fixture::new();
        let moved = fixture.root.path().join("original-moved.exe");
        fs::rename(&fixture.target, &moved).expect("move original aside");
        fs::write(&fixture.target, ORIGINAL).expect("create same-content different file");
        assert_ne!(file_identity(&fixture.target).unwrap(), fixture.identity);
        if existing_backup {
            fs::write(&fixture.backup, b"existing backup").expect("write backup fixture");
        }
        let failure = publish_replacement(
            (&fixture.target, &fixture.target),
            &fixture.stage,
            &fixture.backup,
            &fixture.identity,
            &fixture.stage_identity,
            |_, _, _| panic!("replacement must not run for a changed identity"),
            |_| panic!("changed target must not be launched"),
        )
        .expect_err("changed target is rejected");
        assert_eq!(failure.preserve_backup, existing_backup);
        assert_eq!(fs::read(&fixture.target).unwrap(), ORIGINAL);
        assert!(is_original(&moved, &fixture.identity));
        assert_eq!(fs::read(&fixture.stage).unwrap(), REPLACEMENT);
        if existing_backup {
            assert_eq!(fs::read(&fixture.backup).unwrap(), b"existing backup");
        } else {
            assert!(!fixture.backup.exists());
        }
    }
}

#[test]
fn changed_staged_file_identity_prevents_replacement_and_launch() {
    let fixture = Fixture::new();
    let displaced = fixture.root.path().join("verified-stage.exe");
    fs::rename(&fixture.stage, &displaced).expect("move verified stage aside");
    fs::write(&fixture.stage, REPLACEMENT).expect("write a different same-content stage");
    assert_ne!(
        file_identity(&fixture.stage).unwrap(),
        fixture.stage_identity
    );
    let failure = publish_replacement(
        (&fixture.target, &fixture.target),
        &fixture.stage,
        &fixture.backup,
        &fixture.identity,
        &fixture.stage_identity,
        |_, _, _| panic!("a substituted stage must not be published"),
        |_| panic!("no application should launch for a substituted stage"),
    )
    .expect_err("a substituted stage is rejected");
    assert!(!failure.preserve_backup);
    assert!(!failure.cleanup_taken);
    assert!(is_original(&fixture.target, &fixture.identity));
    assert!(is_original(&displaced, &fixture.stage_identity));
    assert_eq!(fs::read(&fixture.stage).unwrap(), REPLACEMENT);
    assert!(!fixture.backup.exists());
}

#[test]
fn unexpected_existing_backup_is_never_replaced_or_launched() {
    let fixture = Fixture::new();
    fs::write(&fixture.backup, b"existing backup").expect("write backup fixture");
    let failure = publish_replacement(
        (&fixture.target, &fixture.target),
        &fixture.stage,
        &fixture.backup,
        &fixture.identity,
        &fixture.stage_identity,
        |_, _, _| panic!("an existing backup must not be overwritten"),
        |_| panic!("an unexpected backup must not launch an application"),
    )
    .expect_err("an existing backup is rejected");
    assert!(failure.preserve_backup);
    assert!(!failure.cleanup_taken);
    assert!(is_original(&fixture.target, &fixture.identity));
    assert_eq!(fs::read(&fixture.stage).unwrap(), REPLACEMENT);
    assert_eq!(fs::read(&fixture.backup).unwrap(), b"existing backup");
}

#[test]
fn failed_rollback_of_locked_target_preserves_backup() {
    let fixture = Fixture::new();
    let mut lock = None;
    let mut launches = 0;
    let failure = publish_replacement(
        (&fixture.target, &fixture.target),
        &fixture.stage,
        &fixture.backup,
        &fixture.identity,
        &fixture.stage_identity,
        replace_file,
        |target| {
            launches += 1;
            assert_eq!(fs::read(target).unwrap(), REPLACEMENT);
            lock = Some(
                OpenOptions::new()
                    .read(true)
                    .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
                    .open(target)
                    .expect("deny delete sharing during recovery"),
            );
            Err(LaunchFailure::Spawn("simulated new launch failure".into()))
        },
    )
    .expect_err("locked rollback must fail");
    assert_eq!(
        launches, 1,
        "never relaunch before the original is restored"
    );
    assert!(failure.preserve_backup);
    assert!(!failure.cleanup_taken);
    assert!(
        failure
            .message
            .contains(&fixture.backup.display().to_string())
    );
    assert_eq!(fs::read(&fixture.target).unwrap(), REPLACEMENT);
    assert!(is_original(&fixture.backup, &fixture.identity));
    assert!(fixture.directory.exists());
    assert_eq!(fs::read(&fixture.backup).unwrap(), ORIGINAL);
    drop(lock);
}

#[test]
fn recovery_rejects_reparse_target_and_keeps_the_original_backup() {
    let fixture = Fixture::new();
    let destination = fixture.root.path().join("unrelated-directory");
    fs::create_dir(&destination).expect("create unrelated junction destination");
    let sentinel = destination.join("keep.txt");
    fs::write(&sentinel, b"untouched").expect("write unrelated sentinel");

    let failure = publish_replacement(
        (&fixture.target, &fixture.target),
        &fixture.stage,
        &fixture.backup,
        &fixture.identity,
        &fixture.stage_identity,
        |target, _, backup| {
            fs::rename(target, backup.expect("backup destination"))
                .expect("move original to backup");
            junction(target, &destination);
            Err("simulated failure after target became a junction".into())
        },
        |_| panic!("reparse target must not be launched"),
    )
    .expect_err("reparse target is rejected");
    assert!(failure.preserve_backup);
    assert!(restore_original(&fixture.target, &fixture.backup, &fixture.identity, None).is_err());
    assert!(is_original(&fixture.backup, &fixture.identity));
    assert_eq!(fs::read(&sentinel).unwrap(), b"untouched");
    assert_eq!(fs::read(&fixture.stage).unwrap(), REPLACEMENT);
    assert_eq!(fs::read(&fixture.backup).unwrap(), ORIGINAL);
}

#[test]
fn recovery_never_substitutes_an_unrelated_backup() {
    for target_exists in [false, true] {
        let fixture = Fixture::new();
        let moved = fixture.root.path().join("original-moved.exe");
        fs::rename(&fixture.target, &moved).expect("move original aside");
        if target_exists {
            fs::write(&fixture.target, REPLACEMENT).expect("write unrelated target");
        }
        fs::write(&fixture.backup, b"unrelated backup").expect("write unrelated backup");
        assert!(
            restore_original(&fixture.target, &fixture.backup, &fixture.identity, None).is_err()
        );
        assert_eq!(fs::read(&fixture.backup).unwrap(), b"unrelated backup");
        assert!(is_original(&moved, &fixture.identity));
        if target_exists {
            assert_eq!(fs::read(&fixture.target).unwrap(), REPLACEMENT);
        } else {
            assert!(!fixture.target.exists());
        }
    }
}

fn canonical_test_name(version: &str) -> String {
    let arch = if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "x64"
    };
    let variant = if cfg!(feature = "embed-firmware") {
        ""
    } else {
        "-no-firmware"
    };
    format!("slimenrf-helper-{version}-windows-{arch}{variant}.exe")
}

#[test]
fn helper_destination_is_a_validated_sibling_name() {
    let root = tempfile::tempdir().unwrap();
    let original = root.path().join(canonical_test_name("0.1.0"));
    let new_name = canonical_test_name("2.0.0-rc.1+build.4");
    let new_path = root.path().join(&new_name);
    assert_eq!(
        destination_from_name(
            &original,
            &wide(OsString::from(new_name.as_str()).as_os_str())
        )
        .unwrap(),
        new_path
    );
    assert_eq!(
        destination_from_name(&original, &wide(original.file_name().unwrap())).unwrap(),
        original
    );
    for invalid in [
        "",
        ".",
        "..",
        r"..\other.exe",
        "custom.exe",
        "slimenrf-helper-v2.0.0-windows-x64.exe",
    ] {
        assert!(
            destination_from_name(&original, &wide(OsString::from(invalid).as_os_str())).is_err(),
            "invalid destination {invalid:?}"
        );
    }
    let absolute = fs::canonicalize(root.path()).unwrap().join("other.exe");
    assert!(destination_from_name(&original, &wide(absolute.as_os_str())).is_err());
    assert!(destination_from_name(&original, &[b'a' as u16, 0, b'b' as u16]).is_err());
    let custom = root.path().join("custom.exe");
    assert_eq!(
        destination_from_name(&custom, &wide(custom.file_name().unwrap())).unwrap(),
        custom
    );
    assert!(destination_from_name(&custom, &wide(OsString::from(new_name).as_os_str())).is_err());
}

#[test]
fn destination_alias_is_not_a_new_directory_entry() {
    let fixture = Fixture::new();
    assert_eq!(
        resolve_destination(&fixture.target, &fixture.target).unwrap(),
        Destination::InPlace
    );
    let alternate_case = fixture.root.path().join("APPLICATION.EXE");
    if alternate_case.exists() {
        assert_eq!(
            resolve_destination(&fixture.target, &alternate_case).unwrap(),
            Destination::SameEntry
        );
    }
    let long = nul_wide(&fixture.target).unwrap();
    let mut short = vec![0_u16; 32768];
    // SAFETY: long is NUL-terminated and short is a writable UTF-16 buffer.
    let length =
        unsafe { GetShortPathNameW(long.as_ptr(), short.as_mut_ptr(), short.len() as u32) };
    if length > 0 && (length as usize) < short.len() {
        let short = PathBuf::from(OsString::from_wide(&short[..length as usize]));
        let alias = fixture.target.with_file_name(short.file_name().unwrap());
        if alias != fixture.target {
            assert_eq!(
                resolve_destination(&fixture.target, &alias).unwrap(),
                Destination::SameEntry
            );
        }
    }
}

#[test]
fn existing_destination_entries_are_never_replaced_even_for_a_hardlink() {
    for kind in ["file", "hardlink", "directory", "junction"] {
        let fixture = Fixture::new();
        let destination = fixture.root.path().join("next.exe");
        let junction_target = fixture.root.path().join("junction-target");
        match kind {
            "file" => fs::write(&destination, b"foreign file").unwrap(),
            "hardlink" => fs::hard_link(&fixture.target, &destination).unwrap(),
            "directory" => fs::create_dir(&destination).unwrap(),
            "junction" => {
                fs::create_dir(&junction_target).unwrap();
                fs::write(junction_target.join("sentinel"), b"untouched").unwrap();
                junction(&destination, &junction_target);
            }
            _ => unreachable!(),
        }
        let mut launched = Vec::new();
        let failure = publish_replacement(
            (&fixture.target, &destination),
            &fixture.stage,
            &fixture.backup,
            &fixture.identity,
            &fixture.stage_identity,
            |_, _, _| panic!("a destination collision must precede replacement"),
            |path| {
                launched.push(path.to_path_buf());
                Ok(())
            },
        )
        .expect_err("existing destination must be rejected");
        assert!(
            failure.message.contains("already exists"),
            "{kind}: {failure:?}"
        );
        assert!(failure.cleanup_taken, "{kind}");
        assert_eq!(launched, vec![fixture.target.clone()], "{kind}");
        assert!(is_original(&fixture.target, &fixture.identity), "{kind}");
        assert_eq!(fs::read(&fixture.stage).unwrap(), REPLACEMENT, "{kind}");
        assert!(!fixture.backup.exists(), "{kind}");
        match kind {
            "file" => assert_eq!(fs::read(&destination).unwrap(), b"foreign file"),
            "hardlink" => assert!(is_original(&destination, &fixture.identity)),
            "directory" => assert!(destination.is_dir()),
            "junction" => assert_eq!(
                fs::read(junction_target.join("sentinel")).unwrap(),
                b"untouched"
            ),
            _ => unreachable!(),
        }
    }
}

#[test]
fn renamed_publication_launches_verified_destination_and_retains_backup() {
    let fixture = Fixture::new();
    let destination = fixture.root.path().join("next.exe");
    let mut launched = Vec::new();
    publish_replacement(
        (&fixture.target, &destination),
        &fixture.stage,
        &fixture.backup,
        &fixture.identity,
        &fixture.stage_identity,
        replace_file,
        |path| {
            launched.push(path.to_path_buf());
            assert_eq!(fs::read(path).unwrap(), REPLACEMENT);
            Ok(())
        },
    )
    .expect("publish and launch renamed replacement");
    assert_eq!(launched, vec![destination.clone()]);
    assert!(!fixture.target.exists());
    assert!(!fixture.stage.exists());
    assert_eq!(fs::read(&destination).unwrap(), REPLACEMENT);
    assert!(is_original(&fixture.backup, &fixture.identity));
}

#[test]
fn destination_created_after_preflight_causes_atomic_recovery_without_clobbering() {
    let fixture = Fixture::new();
    let destination = fixture.root.path().join("next.exe");
    let mut launched = Vec::new();
    let failure = publish_replacement(
        (&fixture.target, &destination),
        &fixture.stage,
        &fixture.backup,
        &fixture.identity,
        &fixture.stage_identity,
        |target, stage, backup| {
            replace_file(target, stage, backup)?;
            fs::write(&destination, b"concurrent foreign file").unwrap();
            Ok(())
        },
        |path| {
            launched.push(path.to_path_buf());
            Ok(())
        },
    )
    .expect_err("the atomic rename must reject the raced destination");
    assert!(failure.message.contains("renamed"), "{failure:?}");
    assert!(failure.cleanup_taken);
    assert_eq!(launched, vec![fixture.target.clone()]);
    assert!(is_original(&fixture.target, &fixture.identity));
    assert_eq!(fs::read(&destination).unwrap(), b"concurrent foreign file");
    assert_eq!(fs::read(&fixture.stage).unwrap(), REPLACEMENT);
    assert!(!fixture.backup.exists());
}

#[test]
fn failed_renamed_spawn_restores_old_name_and_retains_replacement_for_cleanup() {
    let fixture = Fixture::new();
    let destination = fixture.root.path().join("next.exe");
    let mut launched = Vec::new();
    let failure = publish_replacement(
        (&fixture.target, &destination),
        &fixture.stage,
        &fixture.backup,
        &fixture.identity,
        &fixture.stage_identity,
        replace_file,
        |path| {
            launched.push(path.to_path_buf());
            if path == destination.as_path() {
                assert_eq!(fs::read(path).unwrap(), REPLACEMENT);
                Err(LaunchFailure::Spawn("simulated spawn failure".into()))
            } else {
                assert_eq!(fs::read(path).unwrap(), ORIGINAL);
                Ok(())
            }
        },
    )
    .expect_err("failed spawn must restore the original");
    assert!(failure.cleanup_taken, "{failure:?}");
    assert!(!failure.preserve_backup);
    assert_eq!(launched, vec![destination.clone(), fixture.target.clone()]);
    assert!(is_original(&fixture.target, &fixture.identity));
    assert!(!destination.exists());
    assert_eq!(fs::read(&fixture.stage).unwrap(), REPLACEMENT);
    assert!(!fixture.backup.exists());
}

#[test]
fn renamed_post_spawn_handoff_failure_never_restores_or_relaunches() {
    let fixture = Fixture::new();
    let destination = fixture.root.path().join("next.exe");
    let mut launched = Vec::new();
    let failure = publish_replacement(
        (&fixture.target, &destination),
        &fixture.stage,
        &fixture.backup,
        &fixture.identity,
        &fixture.stage_identity,
        replace_file,
        |path| {
            launched.push(path.to_path_buf());
            Err(LaunchFailure::Handoff("withheld acknowledgement".into()))
        },
    )
    .expect_err("failed handoff must retain the new image and backup");
    assert!(failure.preserve_backup);
    assert!(!failure.cleanup_taken);
    assert_eq!(launched, vec![destination.clone()]);
    assert!(!fixture.target.exists());
    assert_eq!(fs::read(&destination).unwrap(), REPLACEMENT);
    assert!(is_original(&fixture.backup, &fixture.identity));
}

#[test]
fn renamed_recovery_never_moves_substituted_entries() {
    for substituted in ["destination", "backup", "original", "stage"] {
        let fixture = Fixture::new();
        let destination = fixture.root.path().join("next.exe");
        replace_file(&fixture.target, &fixture.stage, Some(&fixture.backup)).unwrap();
        move_verified_no_replace(
            &fixture.target,
            &destination,
            &fixture.stage_identity,
            false,
        )
        .unwrap();
        let displaced = fixture.root.path().join("displaced.exe");
        let foreign = b"foreign entry survives";
        match substituted {
            "destination" => {
                fs::rename(&destination, &displaced).unwrap();
                fs::write(&destination, foreign).unwrap();
            }
            "backup" => {
                fs::rename(&fixture.backup, &displaced).unwrap();
                fs::write(&fixture.backup, foreign).unwrap();
            }
            "original" => fs::write(&fixture.target, foreign).unwrap(),
            "stage" => fs::write(&fixture.stage, foreign).unwrap(),
            _ => unreachable!(),
        }
        assert!(
            restore_renamed_original(
                &fixture.target,
                &destination,
                &fixture.stage,
                &fixture.backup,
                &fixture.identity,
                &fixture.stage_identity,
            )
            .is_err(),
            "substituted {substituted} must prevent recovery"
        );
        let foreign_path = match substituted {
            "destination" => &destination,
            "backup" => &fixture.backup,
            "original" => &fixture.target,
            "stage" => &fixture.stage,
            _ => unreachable!(),
        };
        assert_eq!(fs::read(foreign_path).unwrap(), foreign, "{substituted}");
        if substituted == "backup" {
            assert!(is_original(&displaced, &fixture.identity));
        } else {
            assert!(is_original(&fixture.backup, &fixture.identity));
        }
        if substituted == "destination" {
            assert_eq!(fs::read(&displaced).unwrap(), REPLACEMENT);
        } else {
            assert_eq!(fs::read(&destination).unwrap(), REPLACEMENT);
        }
    }
}

#[test]
fn handle_bound_rename_rejects_substitution_and_pins_its_source() {
    let fixture = Fixture::new();
    let displaced = fixture.root.path().join("verified.exe");
    fs::rename(&fixture.stage, &displaced).unwrap();
    fs::write(&fixture.stage, b"foreign file").unwrap();
    let destination = fixture.root.path().join("next.exe");
    assert!(
        move_verified_no_replace(&fixture.stage, &destination, &fixture.stage_identity, false)
            .is_err()
    );
    assert_eq!(fs::read(&fixture.stage).unwrap(), b"foreign file");
    assert_eq!(fs::read(&displaced).unwrap(), REPLACEMENT);
    assert!(!destination.exists());

    let guard = open_rename_source(&displaced, &fixture.stage_identity, false).unwrap();
    assert!(
        fs::rename(&displaced, &destination).is_err(),
        "another pathname-based rename must not displace the admitted source"
    );
    rename_open_file(&guard, &destination).expect("the admitted handle can rename its own file");
    drop(guard);
    assert_eq!(fs::read(&destination).unwrap(), REPLACEMENT);
}

#[test]
fn cleanup_never_authorizes_a_substituted_or_unverified_stage() {
    let fixture = Fixture::new();
    let size = REPLACEMENT.len() as u64;
    let digest: [u8; 32] = Sha256::digest(REPLACEMENT).into();
    assert_eq!(
        authorized_stage_identity(&fixture.stage, Some(&fixture.stage_identity), size, &digest)
            .unwrap(),
        None,
        "an invalid PE must not be authorized for cleanup"
    );
    assert_eq!(
        authorized_stage_identity(&fixture.stage, None, size, &digest).unwrap(),
        None
    );
    let displaced = fixture.root.path().join("verified-stage.exe");
    fs::rename(&fixture.stage, &displaced).unwrap();
    fs::write(&fixture.stage, REPLACEMENT).unwrap();
    assert_eq!(
        authorized_stage_identity(&fixture.stage, Some(&fixture.stage_identity), size, &digest)
            .unwrap(),
        None,
        "even identical bytes do not authorize a different file"
    );
    assert_eq!(fs::read(&fixture.stage).unwrap(), REPLACEMENT);
    assert!(is_original(&displaced, &fixture.stage_identity));
}

#[test]
fn failed_renamed_spawn_preserves_foreign_paths_and_recovery_evidence() {
    for substituted in ["destination", "backup", "original", "stage"] {
        let fixture = Fixture::new();
        let destination = fixture.root.path().join("next.exe");
        let displaced = fixture.root.path().join("displaced.exe");
        let foreign = b"foreign entry survives";
        let mut launches = 0;
        let failure = publish_replacement(
            (&fixture.target, &destination),
            &fixture.stage,
            &fixture.backup,
            &fixture.identity,
            &fixture.stage_identity,
            replace_file,
            |path| {
                launches += 1;
                assert_eq!(path, destination.as_path());
                match substituted {
                    "destination" => {
                        fs::rename(&destination, &displaced).unwrap();
                        fs::write(&destination, foreign).unwrap();
                    }
                    "backup" => {
                        fs::rename(&fixture.backup, &displaced).unwrap();
                        fs::write(&fixture.backup, foreign).unwrap();
                    }
                    "original" => fs::write(&fixture.target, foreign).unwrap(),
                    "stage" => fs::write(&fixture.stage, foreign).unwrap(),
                    _ => unreachable!(),
                }
                Err(LaunchFailure::Spawn("simulated spawn failure".into()))
            },
        )
        .expect_err("substituted recovery path must stop rollback");
        assert_eq!(launches, 1, "{substituted}");
        assert!(failure.preserve_backup, "{substituted}: {failure:?}");
        assert!(!failure.cleanup_taken, "{substituted}");
        let foreign_path = match substituted {
            "destination" => &destination,
            "backup" => &fixture.backup,
            "original" => &fixture.target,
            "stage" => &fixture.stage,
            _ => unreachable!(),
        };
        assert_eq!(fs::read(foreign_path).unwrap(), foreign, "{substituted}");
        if substituted == "backup" {
            assert!(is_original(&displaced, &fixture.identity));
        } else {
            assert!(is_original(&fixture.backup, &fixture.identity));
        }
        if substituted == "destination" {
            assert_eq!(fs::read(&displaced).unwrap(), REPLACEMENT);
        } else {
            assert_eq!(fs::read(&destination).unwrap(), REPLACEMENT);
        }
    }
}

#[test]
fn revalidated_identical_pe_with_new_identity_is_not_admitted_after_handoff() {
    let fixture = Fixture::new();
    fs::remove_file(&fixture.stage).unwrap();
    fs::copy(std::env::current_exe().unwrap(), &fixture.stage).unwrap();
    let size = fs::metadata(&fixture.stage).unwrap().len();
    let mut file = File::open(&fixture.stage).unwrap();
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer).unwrap();
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    drop(file);
    let digest: [u8; 32] = hash.finalize().into();
    let admitted = verify_staged(&fixture.stage, size, &digest).unwrap();
    assert_eq!(
        authorized_stage_identity(&fixture.stage, Some(&admitted), size, &digest).unwrap(),
        Some(admitted.clone())
    );
    OpenOptions::new()
        .append(true)
        .open(&fixture.stage)
        .unwrap()
        .write_all(b"changed")
        .unwrap();
    assert!(same_file_id(
        &file_identity(&fixture.stage).unwrap(),
        &admitted
    ));
    assert_eq!(
        authorized_stage_identity(&fixture.stage, Some(&admitted), size, &digest).unwrap(),
        None,
        "an altered admitted file must remain evidence"
    );
    OpenOptions::new()
        .write(true)
        .open(&fixture.stage)
        .unwrap()
        .set_len(size)
        .unwrap();
    let displaced = fixture.root.path().join("admitted-stage.exe");
    fs::rename(&fixture.stage, &displaced).unwrap();
    fs::copy(&displaced, &fixture.stage).unwrap();
    let replacement_identity = verify_staged(&fixture.stage, size, &digest).unwrap();
    assert!(!same_file_id(&replacement_identity, &admitted));

    let mut handoff = config();
    handoff.target_identity = fixture.identity.clone();
    handoff.working_directory = wide(fixture.root.path().as_os_str());
    handoff.arguments.clear();
    handoff.size = size;
    handoff.sha256 = digest;
    let failure = install_after_exit(
        &handoff,
        &fixture.target,
        &fixture.target,
        &fixture.directory,
        &admitted,
    )
    .expect_err("same bytes at a substituted stage must not be installed");
    assert!(failure.message.contains("identity changed"), "{failure:?}");
    assert!(is_original(&fixture.target, &fixture.identity));
    assert!(same_file_id(
        &file_identity(&fixture.stage).unwrap(),
        &replacement_identity
    ));
    assert!(same_file_id(&file_identity(&displaced).unwrap(), &admitted));
    assert!(!fixture.backup.exists());
}

fn case_rename_fixture() -> (Fixture, PathBuf) {
    let mut fixture = Fixture::new();
    let original = fixture.root.path().join(canonical_test_name("2.0.0-Alpha"));
    let destination = fixture.root.path().join(canonical_test_name("2.0.0-alpha"));
    fs::rename(&fixture.target, &original).expect("give the original its canonical old name");
    fixture.target = original;
    (fixture, destination)
}

fn assert_entry_spelling(root: &Path, present: &Path, absent: &Path) {
    let names: Vec<OsString> = fs::read_dir(root)
        .expect("read the fixture directory")
        .map(|entry| entry.expect("read the fixture entry").file_name())
        .collect();
    assert!(
        names
            .iter()
            .any(|name| name == present.file_name().unwrap()),
        "the directory must contain the exact requested spelling: {names:?}"
    );
    assert!(
        names.iter().all(|name| name != absent.file_name().unwrap()),
        "the previous spelling must not remain as a directory entry: {names:?}"
    );
}

#[test]
fn case_only_update_publishes_the_exact_canonical_directory_entry() {
    let (fixture, destination) = case_rename_fixture();
    let mut launched = Vec::new();
    publish_replacement(
        (&fixture.target, &destination),
        &fixture.stage,
        &fixture.backup,
        &fixture.identity,
        &fixture.stage_identity,
        replace_file,
        |path| {
            launched.push(path.to_path_buf());
            assert_entry_spelling(fixture.root.path(), &destination, &fixture.target);
            assert_eq!(fs::read(path).unwrap(), REPLACEMENT);
            Ok(())
        },
    )
    .expect("publish the canonical name with its exact case");
    assert_eq!(launched, vec![destination.clone()]);
    assert_entry_spelling(fixture.root.path(), &destination, &fixture.target);
    assert!(is_original(&fixture.backup, &fixture.identity));
    assert!(!fixture.stage.exists());
}

#[test]
fn failed_case_only_spawn_restores_the_original_directory_entry_spelling() {
    let (fixture, destination) = case_rename_fixture();
    let mut launched = Vec::new();
    let failure = publish_replacement(
        (&fixture.target, &destination),
        &fixture.stage,
        &fixture.backup,
        &fixture.identity,
        &fixture.stage_identity,
        replace_file,
        |path| {
            launched.push(path.to_path_buf());
            if path == destination.as_path() {
                assert_entry_spelling(fixture.root.path(), &destination, &fixture.target);
                Err(LaunchFailure::Spawn("simulated spawn failure".into()))
            } else {
                assert_eq!(path, fixture.target.as_path());
                assert_entry_spelling(fixture.root.path(), &fixture.target, &destination);
                assert_eq!(fs::read(path).unwrap(), ORIGINAL);
                Ok(())
            }
        },
    )
    .expect_err("a failed spawn must restore the original spelling");
    assert!(failure.cleanup_taken, "{failure:?}");
    assert!(!failure.preserve_backup);
    assert_eq!(launched, vec![destination.clone(), fixture.target.clone()]);
    assert_entry_spelling(fixture.root.path(), &fixture.target, &destination);
    assert!(is_original(&fixture.target, &fixture.identity));
    assert_eq!(fs::read(&fixture.stage).unwrap(), REPLACEMENT);
    assert!(!fixture.backup.exists());
}

#[test]
fn case_only_rename_race_never_clobbers_a_new_entry_or_loses_the_backup() {
    let (fixture, destination) = case_rename_fixture();
    if resolve_destination(&fixture.target, &destination).unwrap() != Destination::SameEntry {
        // On a case-sensitive directory this is a normal vacant-name move,
        // already covered by destination_created_after_preflight.
        return;
    }
    replace_file(&fixture.target, &fixture.stage, Some(&fixture.backup)).unwrap();
    let replacement = open_rename_source(&fixture.target, &fixture.stage_identity, false).unwrap();
    rename_open_file(&replacement, &fixture.stage).unwrap();
    let displaced = fixture.root.path().join("displaced.exe");
    assert!(fs::rename(&fixture.stage, &displaced).is_err());
    fs::write(&destination, b"concurrent foreign file").unwrap();
    assert!(rename_open_file(&replacement, &destination).is_err());
    drop(replacement);

    let failure = recovery_failure(
        &fixture.target,
        &fixture.backup,
        &fixture.identity,
        &fixture.stage_identity,
        "case-only destination appeared".into(),
        &mut |_| panic!("a foreign original path must not be launched"),
    );
    assert!(failure.preserve_backup, "{failure:?}");
    assert!(!failure.cleanup_taken);
    assert_eq!(fs::read(&destination).unwrap(), b"concurrent foreign file");
    assert_eq!(fs::read(&fixture.stage).unwrap(), REPLACEMENT);
    assert!(is_original(&fixture.backup, &fixture.identity));
}

#[test]
fn case_only_post_spawn_handoff_failure_keeps_new_spelling_and_backup() {
    let (fixture, destination) = case_rename_fixture();
    let mut launches = 0;
    let failure = publish_replacement(
        (&fixture.target, &destination),
        &fixture.stage,
        &fixture.backup,
        &fixture.identity,
        &fixture.stage_identity,
        replace_file,
        |path| {
            launches += 1;
            assert_eq!(path, destination.as_path());
            Err(LaunchFailure::Handoff("withheld acknowledgement".into()))
        },
    )
    .expect_err("post-spawn failure must not roll back");
    assert_eq!(launches, 1);
    assert!(failure.preserve_backup);
    assert!(!failure.cleanup_taken);
    assert_entry_spelling(fixture.root.path(), &destination, &fixture.target);
    assert_eq!(fs::read(&destination).unwrap(), REPLACEMENT);
    assert!(is_original(&fixture.backup, &fixture.identity));
}

#[test]
fn case_only_stage_collision_leaves_foreign_file_and_backup_untouched() {
    let (fixture, destination) = case_rename_fixture();
    if resolve_destination(&fixture.target, &destination).unwrap() != Destination::SameEntry {
        return;
    }
    let failure = publish_replacement(
        (&fixture.target, &destination),
        &fixture.stage,
        &fixture.backup,
        &fixture.identity,
        &fixture.stage_identity,
        |target, stage, backup| {
            replace_file(target, stage, backup)?;
            fs::write(stage, b"foreign stage").unwrap();
            Ok(())
        },
        |_| panic!("a foreign stage must not be launched or overwritten"),
    )
    .expect_err("a stage collision must preserve the backup");
    assert!(failure.preserve_backup, "{failure:?}");
    assert!(!failure.cleanup_taken);
    assert_eq!(fs::read(&fixture.stage).unwrap(), b"foreign stage");
    assert_eq!(fs::read(&fixture.target).unwrap(), REPLACEMENT);
    assert!(is_original(&fixture.backup, &fixture.identity));
    assert_entry_spelling(fixture.root.path(), &fixture.target, &destination);
}

#[test]
fn failed_case_only_spawn_does_not_replace_substituted_recovery_entries() {
    for substituted in ["destination", "backup", "stage"] {
        let (fixture, destination) = case_rename_fixture();
        let displaced = fixture.root.path().join("displaced.exe");
        let foreign = b"foreign recovery entry";
        let mut launches = 0;
        let failure = publish_replacement(
            (&fixture.target, &destination),
            &fixture.stage,
            &fixture.backup,
            &fixture.identity,
            &fixture.stage_identity,
            replace_file,
            |path| {
                launches += 1;
                assert_eq!(path, destination.as_path());
                match substituted {
                    "destination" => {
                        fs::rename(&destination, &displaced).unwrap();
                        fs::write(&destination, foreign).unwrap();
                    }
                    "backup" => {
                        fs::rename(&fixture.backup, &displaced).unwrap();
                        fs::write(&fixture.backup, foreign).unwrap();
                    }
                    "stage" => fs::write(&fixture.stage, foreign).unwrap(),
                    _ => unreachable!(),
                }
                Err(LaunchFailure::Spawn("simulated spawn failure".into()))
            },
        )
        .expect_err("substituted recovery entries must stop rollback");
        assert_eq!(launches, 1, "{substituted}");
        assert!(failure.preserve_backup, "{substituted}: {failure:?}");
        assert!(!failure.cleanup_taken);
        let foreign_path = match substituted {
            "destination" => &destination,
            "backup" => &fixture.backup,
            "stage" => &fixture.stage,
            _ => unreachable!(),
        };
        assert_eq!(fs::read(foreign_path).unwrap(), foreign, "{substituted}");
        if substituted == "backup" {
            assert!(is_original(&displaced, &fixture.identity));
        } else {
            assert!(is_original(&fixture.backup, &fixture.identity));
        }
        if substituted == "destination" {
            assert_eq!(fs::read(&displaced).unwrap(), REPLACEMENT);
        } else {
            assert_eq!(fs::read(&destination).unwrap(), REPLACEMENT);
        }
    }
}
