use super::{
    CopyFile, CopyOutcome, Uf2Volume, authorization_tests::updater, inspect_volume, reboot_with,
    tests::payload, write_with,
};
use crate::update::Uf2FinalizationStage;
use slimenrf_firmware_image::image::{Nrf52Chip, Uf2ImagePurpose};
use std::{
    cell::Cell,
    fs::{self, File},
    io::{self, Write},
    rc::Rc,
    sync::atomic::{AtomicBool, Ordering},
};

#[cfg(windows)]
const DISCONNECT_CODES: &[i32] = &[433, 1167];
#[cfg(any(target_os = "linux", target_os = "macos"))]
const DISCONNECT_CODES: &[i32] = &[19, 6]; // ENODEV, ENXIO on these targets.
#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
const DISCONNECT_CODES: &[i32] = &[];

#[derive(Clone, Copy)]
enum Fault {
    None,
    WriteAfter(usize, Option<i32>),
    Flush(Option<i32>),
    Sync(Option<i32>),
}

fn injected_error(code: Option<i32>) -> io::Error {
    code.map_or_else(
        || io::Error::other("injected storage error"),
        io::Error::from_raw_os_error,
    )
}

struct FaultingFile {
    file: File,
    fault: Fault,
    written: usize,
    max_write: usize,
    finalizations: Rc<Cell<(u32, u32)>>,
}

impl Write for FaultingFile {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut length = bytes.len().min(self.max_write);
        if let Fault::WriteAfter(limit, code) = self.fault {
            if self.written == limit {
                return code.map_or(Ok(0), |code| Err(io::Error::from_raw_os_error(code)));
            }
            length = length.min(limit - self.written);
        }
        let written = self.file.write(&bytes[..length])?;
        self.written += written;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        let (flush, sync) = self.finalizations.get();
        self.finalizations.set((flush + 1, sync));
        if let Fault::Flush(code) = self.fault {
            return Err(injected_error(code));
        }
        self.file.flush()
    }
}

impl CopyFile for FaultingFile {
    fn sync_all(&self) -> io::Result<()> {
        let (flush, sync) = self.finalizations.get();
        self.finalizations.set((flush, sync + 1));
        if let Fault::Sync(code) = self.fault {
            return Err(injected_error(code));
        }
        self.file.sync_all()
    }
}

fn writer(
    fault: Fault,
    max_write: usize,
    finalizations: &Rc<Cell<(u32, u32)>>,
) -> impl FnOnce(File) -> FaultingFile {
    let finalizations = finalizations.clone();
    move |file| FaultingFile {
        file,
        fault,
        written: 0,
        max_write,
        finalizations,
    }
}

fn volume() -> (tempfile::TempDir, Uf2Volume) {
    let directory = tempfile::tempdir().unwrap();
    fs::write(
        directory.path().join("INFO_UF2.TXT"),
        "Board-ID: known-board\n",
    )
    .unwrap();
    let volume = inspect_volume(directory.path(), "INFO_UF2.TXT").unwrap();
    (directory, volume)
}

#[test]
fn successful_short_writes_run_real_copy_flush_and_sync() {
    let (directory, volume) = volume();
    let bytes = payload(200);
    let calls = Rc::new(Cell::new((0, 0)));
    let mut progress = Vec::new();
    let outcome = write_with(
        &volume,
        "firmware.uf2",
        &bytes,
        Uf2ImagePurpose::Application,
        &AtomicBool::new(false),
        &mut |next| progress.push(next),
        writer(Fault::None, 7, &calls),
    )
    .unwrap();
    assert_eq!(outcome, CopyOutcome::Completed);
    assert_eq!(calls.get(), (1, 1));
    assert_eq!(
        fs::read(directory.path().join("firmware.uf2")).unwrap(),
        bytes
    );
    assert_eq!(
        progress.iter().map(|p| p.completed).collect::<Vec<_>>(),
        [0, 65536, 102400, 102400]
    );
    assert!(
        progress
            .last()
            .unwrap()
            .message
            .starts_with("UF2 copy completed")
    );
}

#[test]
fn only_full_host_writes_then_device_disappearance_leave_confirmation_pending() {
    for (purpose, bytes) in [
        (Uf2ImagePurpose::Application, payload(200)),
        (
            Uf2ImagePurpose::Bootloader(Nrf52Chip::Nrf52833),
            updater(Nrf52Chip::Nrf52833),
        ),
        (
            Uf2ImagePurpose::Bootloader(Nrf52Chip::Nrf52840),
            updater(Nrf52Chip::Nrf52840),
        ),
    ] {
        for &code in DISCONNECT_CODES {
            for stage in [Uf2FinalizationStage::Flush, Uf2FinalizationStage::SyncAll] {
                let (directory, volume) = volume();
                let calls = Rc::new(Cell::new((0, 0)));
                let fault = match stage {
                    Uf2FinalizationStage::Flush => Fault::Flush(Some(code)),
                    Uf2FinalizationStage::SyncAll => Fault::Sync(Some(code)),
                };
                let outcome = write_with(
                    &volume,
                    "firmware.uf2",
                    &bytes,
                    purpose,
                    &AtomicBool::new(false),
                    &mut |_| {},
                    writer(fault, 31, &calls),
                )
                .unwrap();
                let CopyOutcome::ConfirmationPending(report) = outcome else {
                    panic!("must not claim completion for {purpose:?}, {stage:?}, {code}");
                };
                assert_eq!(report.completed, bytes.len() as u64);
                assert_eq!(report.total, bytes.len() as u64);
                assert_eq!(report.stage, stage);
                assert_eq!(report.os_error, Some(code));
                assert_eq!(
                    report.diagnostic,
                    io::Error::from_raw_os_error(code).to_string()
                );
                assert_eq!(
                    calls.get(),
                    (1, u32::from(stage == Uf2FinalizationStage::SyncAll))
                );
                assert_eq!(
                    fs::read(directory.path().join("firmware.uf2")).unwrap(),
                    bytes
                );
            }
        }
    }
}

#[test]
fn storage_errors_remain_failures_even_after_all_bytes_were_accepted() {
    #[cfg(windows)]
    let codes = [
        Some(1117),
        Some(23),
        Some(112),
        Some(5),
        Some(6),
        Some(21),
        Some(995),
        Some(1460),
        None,
    ];
    #[cfg(not(windows))]
    let codes = [
        Some(5),
        Some(28),
        Some(13),
        Some(30),
        Some(9),
        Some(110),
        Some(1167),
        Some(433),
        None,
    ];
    for code in codes {
        for stage in [Uf2FinalizationStage::Flush, Uf2FinalizationStage::SyncAll] {
            let (directory, volume) = volume();
            let bytes = payload(2);
            let calls = Rc::new(Cell::new((0, 0)));
            let fault = match stage {
                Uf2FinalizationStage::Flush => Fault::Flush(code),
                Uf2FinalizationStage::SyncAll => Fault::Sync(code),
            };
            let error = write_with(
                &volume,
                "firmware.uf2",
                &bytes,
                Uf2ImagePurpose::Application,
                &AtomicBool::new(false),
                &mut |_| {},
                writer(fault, 19, &calls),
            )
            .unwrap_err();
            assert!(error.contains(&format!("{stage:?}")), "{error}");
            assert!(error.contains("1024/1024 host-written bytes"), "{error}");
            assert!(error.contains(&injected_error(code).to_string()), "{error}");
            assert_eq!(
                fs::read(directory.path().join("firmware.uf2")).unwrap(),
                bytes
            );
        }
    }
}

#[test]
fn short_write_disappearance_and_write_zero_are_failures_without_finalization() {
    let bytes = payload(200);
    for limit in [0, 5, 65536, bytes.len() - 1] {
        for code in std::iter::once(None).chain(DISCONNECT_CODES.iter().copied().map(Some)) {
            let (directory, volume) = volume();
            let calls = Rc::new(Cell::new((0, 0)));
            let error = write_with(
                &volume,
                "firmware.uf2",
                &bytes,
                Uf2ImagePurpose::Application,
                &AtomicBool::new(false),
                &mut |_| {},
                writer(Fault::WriteAfter(limit, code), 11, &calls),
            )
            .unwrap_err();
            assert!(
                error.contains(&format!("after {limit}/{} bytes", bytes.len())),
                "{error}"
            );
            assert_eq!(calls.get(), (0, 0));
            assert_eq!(
                fs::read(directory.path().join("firmware.uf2")).unwrap(),
                bytes[..limit]
            );
            let replay = super::write(
                &volume,
                "firmware.uf2",
                &bytes,
                Uf2ImagePurpose::Application,
                &AtomicBool::new(false),
                &mut |_| {},
            )
            .unwrap_err();
            assert!(replay.contains("Cannot create"), "{replay}");
        }
    }
}

#[test]
fn cancellation_before_create_between_chunks_and_before_finalize_is_not_pending() {
    let bytes = payload(200);
    for cancel_after in [None, Some(0), Some(65536), Some(bytes.len() as u64)] {
        let (directory, volume) = volume();
        let calls = Rc::new(Cell::new((0, 0)));
        let cancel = AtomicBool::new(cancel_after.is_none());
        let error = write_with(
            &volume,
            "firmware.uf2",
            &bytes,
            Uf2ImagePurpose::Application,
            &cancel,
            &mut |next| {
                if cancel_after == Some(next.completed) {
                    cancel.store(true, Ordering::Release);
                }
            },
            writer(
                Fault::Flush(DISCONNECT_CODES.first().copied()),
                usize::MAX,
                &calls,
            ),
        )
        .unwrap_err();
        assert!(error.contains("cancelled"), "{error}");
        assert_eq!(calls.get(), (0, 0));
        let path = directory.path().join("firmware.uf2");
        match cancel_after {
            None => assert!(!path.exists()),
            Some(length) => assert_eq!(fs::metadata(path).unwrap().len(), length),
        }
    }
}

#[test]
fn replaced_identity_before_remaining_chunks_never_reaches_finalization() {
    let (directory, volume) = volume();
    let bytes = payload(200);
    let calls = Rc::new(Cell::new((0, 0)));
    let error = write_with(
        &volume,
        "firmware.uf2",
        &bytes,
        Uf2ImagePurpose::Application,
        &AtomicBool::new(false),
        &mut |next| {
            if next.completed > 0 {
                fs::write(
                    directory.path().join("INFO_UF2.TXT"),
                    "Board-ID: replacement\n",
                )
                .unwrap();
            }
        },
        writer(
            Fault::Sync(DISCONNECT_CODES.first().copied()),
            usize::MAX,
            &calls,
        ),
    )
    .unwrap_err();
    assert!(error.contains("identity changed"), "{error}");
    assert_eq!(calls.get(), (0, 0));
    assert_eq!(
        fs::read(directory.path().join("firmware.uf2")).unwrap(),
        bytes[..65536]
    );
}

#[test]
fn reboot_retains_current_source_and_the_same_typed_finalization_result() {
    for &code in DISCONNECT_CODES {
        for stage in [Uf2FinalizationStage::Flush, Uf2FinalizationStage::SyncAll] {
            let (directory, volume) = volume();
            let bytes = payload(200);
            fs::write(directory.path().join("CURRENT.UF2"), &bytes).unwrap();
            let calls = Rc::new(Cell::new((0, 0)));
            let fault = match stage {
                Uf2FinalizationStage::Flush => Fault::Flush(Some(code)),
                Uf2FinalizationStage::SyncAll => Fault::Sync(Some(code)),
            };
            let outcome = reboot_with(
                &volume,
                &AtomicBool::new(false),
                &mut |_| {},
                writer(fault, 29, &calls),
            )
            .unwrap();
            let CopyOutcome::ConfirmationPending(report) = outcome else {
                panic!("reboot must retain pending confirmation");
            };
            assert_eq!(report.stage, stage);
            assert_eq!(report.completed, bytes.len() as u64);
            assert_eq!(report.total, report.completed);
            assert_eq!(report.os_error, Some(code));
            assert_eq!(
                fs::read(directory.path().join("REBOOT.UF2")).unwrap(),
                bytes
            );
            assert_eq!(
                fs::read(directory.path().join("CURRENT.UF2")).unwrap(),
                bytes
            );
        }
    }
}

#[test]
fn destination_and_frozen_chip_guards_run_before_injected_copy_io() {
    let (directory, volume) = volume();
    let bytes = payload(1);
    fs::write(directory.path().join("existing.uf2"), b"existing bytes").unwrap();
    for name in [
        "../escape.uf2",
        "CURRENT.UF2",
        "INFO_UF2.TXT",
        "wrong.hex",
        "existing.uf2",
    ] {
        let error = write_with(
            &volume,
            name,
            &bytes,
            Uf2ImagePurpose::Application,
            &AtomicBool::new(false),
            &mut |_| {},
            |_: File| -> File { panic!("invalid destination reached copy IO: {name}") },
        )
        .unwrap_err();
        assert!(!error.is_empty());
    }
    assert_eq!(
        fs::read(directory.path().join("existing.uf2")).unwrap(),
        b"existing bytes"
    );
    let bootloader = updater(Nrf52Chip::Nrf52833);
    let error = write_with(
        &volume,
        "wrong-chip.uf2",
        &bootloader,
        Uf2ImagePurpose::Bootloader(Nrf52Chip::Nrf52840),
        &AtomicBool::new(false),
        &mut |_| {},
        |_: File| -> File { panic!("wrong MCU reached copy IO") },
    )
    .unwrap_err();
    assert!(!error.is_empty());
    assert!(!directory.path().join("wrong-chip.uf2").exists());
    fs::write(
        directory.path().join("INFO_UF2.TXT"),
        "Board-ID: replacement\n",
    )
    .unwrap();
    let error = write_with(
        &volume,
        "replacement.uf2",
        &bytes,
        Uf2ImagePurpose::Application,
        &AtomicBool::new(false),
        &mut |_| {},
        |_: File| -> File { panic!("replaced volume reached copy IO") },
    )
    .unwrap_err();
    assert!(error.contains("identity changed"), "{error}");
    assert!(!directory.path().join("replacement.uf2").exists());
}

#[test]
fn reboot_partial_write_and_cancellation_before_finalize_remain_failures() {
    let bytes = payload(200);
    for partial in [false, true] {
        let (directory, volume) = volume();
        fs::write(directory.path().join("CURRENT.UF2"), &bytes).unwrap();
        let calls = Rc::new(Cell::new((0, 0)));
        let cancel = AtomicBool::new(false);
        let fault = if partial {
            Fault::WriteAfter(bytes.len() - 1, DISCONNECT_CODES.first().copied())
        } else {
            Fault::Sync(DISCONNECT_CODES.first().copied())
        };
        let error = reboot_with(
            &volume,
            &cancel,
            &mut |next| {
                if !partial && next.completed == bytes.len() as u64 {
                    cancel.store(true, Ordering::Release);
                }
            },
            writer(fault, 17, &calls),
        )
        .unwrap_err();
        assert_eq!(calls.get(), (0, 0));
        if partial {
            assert!(error.contains("102399/102400 bytes"), "{error}");
        } else {
            assert!(error.contains("cancelled"), "{error}");
        }
        assert_eq!(
            fs::read(directory.path().join("REBOOT.UF2")).unwrap(),
            bytes[..bytes.len() - usize::from(partial)],
        );
        assert_eq!(
            fs::read(directory.path().join("CURRENT.UF2")).unwrap(),
            bytes
        );
    }
}
