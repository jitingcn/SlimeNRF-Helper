use super::*;
use crate::features::firmware::uf2::{tests::payload, write};
use slimenrf_firmware_image::image::Uf2ImagePurpose;
use std::{io::Write, sync::atomic::AtomicBool};

const MARKER_NAME: &str = "INFO_UF2.TXT";
const MARKER: &str = "UF2 Bootloader test\nBoard-ID: nRF52840-test\n";

fn mount_record(kind: &[u8], path: &[u8], flags: libc::c_int) -> libc::statfs {
    // SAFETY: statfs contains only integer fields and fixed arrays of integers.
    let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
    stat.f_flags = flags as u32;
    for (field, value) in stat.f_fstypename.iter_mut().zip(kind) {
        *field = *value as libc::c_char;
    }
    for (field, value) in stat.f_mntonname.iter_mut().zip(path) {
        *field = *value as libc::c_char;
    }
    stat
}

#[test]
fn mount_table_filters_flags_and_filesystem_not_directory_names() {
    let point = b"/private/tmp/UF2 device";
    let local = libc::MNT_LOCAL;
    for kind in [b"msdos".as_slice(), b"lifs", b"exfat", b"msdos_fskit"] {
        assert_eq!(
            mounted_path(&mount_record(kind, point, local)),
            Some(Path::new("/private/tmp/UF2 device")),
        );
    }
    for (kind, flags) in [
        (b"msdos".as_slice(), local | libc::MNT_RDONLY),
        (b"lifs".as_slice(), 0),
        (b"smbfs".as_slice(), local),
        (b"apfs".as_slice(), local),
    ] {
        assert!(mounted_path(&mount_record(kind, point, flags)).is_none());
    }
    assert!(mounted_path(&mount_record(b"msdos", b"not-absolute", local)).is_none());
}

#[test]
fn marker_directory_is_not_a_native_mount() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().canonicalize().unwrap().join("SLIMENRF");
    fs::create_dir(&path).unwrap();
    fs::write(path.join(MARKER_NAME), MARKER).unwrap();
    let root = Root::open(&path).unwrap();
    assert_eq!(
        root.marker(c"INFO_UF2.TXT").unwrap().board_id.as_deref(),
        Some("nRF52840-test")
    );
    assert!(!root.is_mounted_at(&path));
}

#[test]
fn replaced_path_revokes_write_and_relative_open_cannot_redirect() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("selected");
    let detached = directory.path().join("original");
    fs::create_dir(&path).unwrap();
    fs::write(path.join(MARKER_NAME), MARKER).unwrap();
    let volume = inspect_fixture(&path, MARKER_NAME).unwrap();
    revalidate(&volume).unwrap();

    fs::rename(&path, &detached).unwrap();
    fs::create_dir(&path).unwrap();
    fs::write(path.join(MARKER_NAME), MARKER).unwrap();
    let replacement = inspect_fixture(&path, MARKER_NAME).unwrap();
    assert_ne!(volume.identity, replacement.identity);
    let error = write(
        &volume,
        "firmware.uf2",
        &payload(1),
        Uf2ImagePurpose::Application,
        &AtomicBool::new(false),
        &mut |_| {},
    )
    .unwrap_err();
    assert!(error.contains("identity changed"), "{error}");
    assert!(!path.join("firmware.uf2").exists());
    assert!(!detached.join("firmware.uf2").exists());

    // Model replacement after validation but before the OS opens the destination.
    create_destination(&volume, "relative.uf2")
        .unwrap()
        .write_all(b"held directory")
        .unwrap();
    assert!(!path.join("relative.uf2").exists());
    assert_eq!(
        fs::read(detached.join("relative.uf2")).unwrap(),
        b"held directory"
    );
}
