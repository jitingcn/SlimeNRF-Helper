use super::*;
use serde_json::json;
use std::fs;
use std::io::{Seek, SeekFrom};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn write_file(root: &Path, path: &str, bytes: &[u8]) -> PathBuf {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, bytes).unwrap();
    path
}

fn executable(root: &Path) -> PathBuf {
    let mut bytes = vec![0; 120];
    bytes[..8].copy_from_slice(b"\x7fELF\x02\x01\x01\x00");
    bytes[16..18].copy_from_slice(&2_u16.to_le_bytes());
    bytes[18..20].copy_from_slice(&62_u16.to_le_bytes());
    bytes[32..40].copy_from_slice(&64_u64.to_le_bytes());
    bytes[54..56].copy_from_slice(&56_u16.to_le_bytes());
    bytes[56..58].copy_from_slice(&1_u16.to_le_bytes());
    bytes[64..68].copy_from_slice(&1_u32.to_le_bytes());
    let path = write_file(root, "target/release/slimenrf-helper", &bytes);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn package_fixture(root: &Path, manifest: &str, members: &[&str], decoy: bool) -> Vec<OsString> {
    let app = executable(root);
    let mut packages = vec![
        json!({"name": APP, "version": "1.2.3", "id": "app-id", "manifest_path": root.join(manifest)}),
    ];
    if decoy {
        packages.push(json!({"name": APP, "version": "9.9.9", "id": "other-id", "manifest_path": root.join("Cargo.toml")}));
    }
    let metadata = json!({"packages": packages, "workspace_members": members});
    let metadata_path = write_file(root, "metadata.json", metadata.to_string().as_bytes());
    let artifact = json!({
        "reason": "compiler-artifact", "package_id": "app-id",
        "target": {"name": APP, "kind": ["bin"]},
        "profile": {"test": false}, "executable": app
    });
    let decoy_artifact = json!({
        "reason": "compiler-artifact", "package_id": "firmware-image-id",
        "target": {"name": APP, "kind": ["bin"]},
        "profile": {"test": false}, "executable": root.join("target/release/not-the-app")
    });
    let artifacts_path = write_file(
        root,
        "artifacts.jsonl",
        format!("{decoy_artifact}\n{artifact}\n").as_bytes(),
    );
    vec![
        root.as_os_str().into(),
        metadata_path.into_os_string(),
        artifacts_path.into_os_string(),
        "1".into(),
        "0".into(),
        "elf".into(),
    ]
}

#[test]
fn package_selects_relocated_workspace_app_and_keeps_artifact_name() {
    let root = TempDir::new().unwrap();
    let args = package_fixture(
        root.path(),
        "crates/app/Cargo.toml",
        &[
            "app-id",
            "recording-id",
            "firmware-image-id",
            "terminal-core-id",
        ],
        true,
    );
    run(&args).unwrap();
    let artifact = root
        .path()
        .join("dist/slimenrf-helper-1.2.3-linux-x64-no-firmware");
    assert_eq!(
        fs::read(&artifact).unwrap(),
        fs::read(root.path().join("target/release/slimenrf-helper")).unwrap()
    );
    assert_ne!(
        fs::metadata(artifact).unwrap().permissions().mode() & 0o111,
        0
    );
}

#[test]
fn package_rejects_old_location_and_nonmember() {
    for (manifest, members) in [
        ("Cargo.toml", vec!["app-id", "terminal-core-id"]),
        (
            "crates/app/Cargo.toml",
            vec!["recording-id", "firmware-image-id"],
        ),
    ] {
        let root = TempDir::new().unwrap();
        let args = package_fixture(root.path(), manifest, &members, false);
        assert!(
            run(&args)
                .unwrap_err()
                .contains("workspace slimenrf-helper application")
        );
        assert!(!root.path().join("dist").exists());
    }
}

#[test]
fn rejects_ambiguous_and_unsafe_cargo_artifacts_before_publication() {
    let root = TempDir::new().unwrap();
    let args = package_fixture(root.path(), "crates/app/Cargo.toml", &["app-id"], false);
    let artifacts = PathBuf::from(&args[2]);
    let mut lines = fs::read_to_string(&artifacts).unwrap();
    let actual = lines.lines().last().unwrap().to_owned();
    let mut additional: serde_json::Value = serde_json::from_str(&actual).unwrap();
    additional["executable"] = json!(root.path().join("other"));
    lines.push_str(&format!("{additional}\n"));
    fs::write(&artifacts, lines).unwrap();
    assert!(
        run(&args)
            .unwrap_err()
            .contains("exactly one application executable")
    );
    assert!(!root.path().join("dist").exists());

    let metadata = PathBuf::from(&args[1]);
    let mut value: serde_json::Value =
        serde_json::from_slice(&fs::read(&metadata).unwrap()).unwrap();
    value["packages"][0]["version"] = json!("1.2.3/escape");
    fs::write(metadata, value.to_string()).unwrap();
    assert!(run(&args).unwrap_err().contains("Unsafe package version"));
    assert!(!root.path().join("dist").exists());
}

#[test]
fn elf_identity_validates_full_table_architecture_and_dynamic_state() {
    let root = TempDir::new().unwrap();
    let path = executable(root.path());
    let mut f = open_regular(&path).unwrap();
    assert_eq!(elf_identity(&mut f).unwrap(), ("x64", false));
    let mut bytes = fs::read(&path).unwrap();
    bytes[64..68].copy_from_slice(&3_u32.to_le_bytes());
    fs::write(&path, &bytes).unwrap();
    f.seek(SeekFrom::Start(0)).unwrap();
    assert_eq!(elf_identity(&mut f).unwrap(), ("x64", true));
    bytes[32..40].copy_from_slice(&u64::MAX.to_le_bytes());
    fs::write(&path, bytes).unwrap();
    f.seek(SeekFrom::Start(0)).unwrap();
    assert!(elf_identity(&mut f).unwrap_err().contains("program-header"));
}

#[test]
fn refuses_symlinked_source_destination_and_dist() {
    let root = TempDir::new().unwrap();
    let source_path = executable(root.path());
    let source_link = root.path().join("source-link");
    symlink(&source_path, &source_link).unwrap();
    assert!(open_regular(&source_link).is_err());
    let external = write_file(root.path(), "external", b"external protected");
    fs::create_dir(root.path().join("dist")).unwrap();
    symlink(&external, root.path().join("dist/package")).unwrap();
    let mut source = open_regular(&source_path).unwrap();
    assert!(publish_package(root.path(), &mut source, "package", true, true).is_err());
    assert_eq!(fs::read(&external).unwrap(), b"external protected");
    fs::remove_file(root.path().join("dist/package")).unwrap();
    fs::remove_dir(root.path().join("dist")).unwrap();
    let outside = root.path().join("outside");
    fs::create_dir(&outside).unwrap();
    symlink(&outside, root.path().join("dist")).unwrap();
    assert!(publish_package(root.path(), &mut source, "package", true, true).is_err());
    assert_eq!(fs::read_dir(outside).unwrap().count(), 0);
}

#[test]
fn forced_replacement_is_atomic_and_does_not_modify_hardlinks() {
    let root = TempDir::new().unwrap();
    let source_path = executable(root.path());
    let dist = root.path().join("dist");
    fs::create_dir(&dist).unwrap();
    let destination = write_file(root.path(), "dist/package", b"old protected bytes");
    let retained = root.path().join("retained");
    fs::hard_link(&destination, &retained).unwrap();
    let mut source = open_regular(&source_path).unwrap();
    assert!(publish_package(root.path(), &mut source, "package", false, true).is_err());
    assert_eq!(fs::read(&destination).unwrap(), b"old protected bytes");
    publish_package(root.path(), &mut source, "package", true, true).unwrap();
    assert_eq!(fs::read(&retained).unwrap(), b"old protected bytes");
    assert_eq!(
        fs::read(&destination).unwrap(),
        fs::read(&source_path).unwrap()
    );
    assert_ne!(
        fs::metadata(&destination).unwrap().permissions().mode() & 0o111,
        0
    );
    assert_eq!(
        fs::read_dir(dist).unwrap().count(),
        1,
        "temporary output must be removed"
    );
}

#[test]
fn publication_detects_destination_replacement_and_removes_temporary() {
    let root = TempDir::new().unwrap();
    let source_path = executable(root.path());
    let dist = root.path().join("dist");
    fs::create_dir(&dist).unwrap();
    let destination = write_file(root.path(), "dist/package", b"old");
    let mut source = open_regular(&source_path).unwrap();
    let directory = open_directory(&dist).unwrap();
    let result = publish_impl(&mut source, &directory, "package", true, true, || {
        let attacker = dist.join("attacker");
        fs::write(&attacker, b"replacement").unwrap();
        fs::rename(attacker, &destination).unwrap();
        Ok(())
    });
    assert!(result.unwrap_err().contains("destination changed"));
    assert_eq!(fs::read(&destination).unwrap(), b"replacement");
    assert_eq!(
        fs::read_dir(dist).unwrap().count(),
        1,
        "temporary output must be removed"
    );
}

#[test]
fn existing_identical_nonexecutable_package_is_not_silently_chmodded() {
    let root = TempDir::new().unwrap();
    let source_path = executable(root.path());
    let dist = root.path().join("dist");
    fs::create_dir(&dist).unwrap();
    let destination = dist.join("package");
    fs::copy(&source_path, &destination).unwrap();
    fs::set_permissions(&destination, fs::Permissions::from_mode(0o644)).unwrap();
    let mut source = open_regular(&source_path).unwrap();
    assert!(
        publish_package(root.path(), &mut source, "package", true, true)
            .unwrap_err()
            .contains("not executable")
    );
    assert_eq!(
        fs::metadata(destination).unwrap().permissions().mode() & 0o111,
        0
    );
}

#[test]
fn refuses_fifo_source_and_destination_without_waiting() {
    use std::os::unix::ffi::OsStrExt;

    let root = TempDir::new().unwrap();
    let source_path = executable(root.path());
    let fifo = root.path().join("fifo");
    let name = CString::new(fifo.as_os_str().as_bytes()).unwrap();
    // SAFETY: name is a valid, terminated path in the isolated test directory.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    assert!(open_regular(&fifo).is_err());
    let dist = root.path().join("dist");
    fs::create_dir(&dist).unwrap();
    fs::rename(&fifo, dist.join("package")).unwrap();
    let mut source = open_regular(&source_path).unwrap();
    assert!(publish_package(root.path(), &mut source, "package", true, true).is_err());
}

#[test]
fn temporary_name_collision_never_removes_existing_file() {
    let root = TempDir::new().unwrap();
    let dist = root.path().join("dist");
    fs::create_dir(&dist).unwrap();
    let existing = dist.join(".package.fixed.tmp");
    fs::write(&existing, b"keep these bytes").unwrap();
    let directory = open_directory(&dist).unwrap();
    assert!(create_temporary(&directory, entry_name(".package.fixed.tmp").unwrap()).is_err());
    assert_eq!(fs::read(existing).unwrap(), b"keep these bytes");
}

#[test]
fn publication_without_previous_file_never_overwrites_a_new_collision() {
    let root = TempDir::new().unwrap();
    let source_path = executable(root.path());
    let dist = root.path().join("dist");
    fs::create_dir(&dist).unwrap();
    let destination = dist.join("package");
    let directory = open_directory(&dist).unwrap();
    let mut source = open_regular(&source_path).unwrap();
    let result = publish_impl(&mut source, &directory, "package", true, true, || {
        fs::write(&destination, b"simultaneous package").unwrap();
        Ok(())
    });
    assert!(result.is_err());
    assert_eq!(fs::read(destination).unwrap(), b"simultaneous package");
    assert_eq!(
        fs::read_dir(dist).unwrap().count(),
        1,
        "temporary output must be removed"
    );
}

#[test]
fn elf_identity_obeys_endianness_class_and_architecture() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("elf");
    let cases = [
        (1_u8, 1_u8, 3_u16, "x86"),
        (2, 1, 183, "arm64"),
        (2, 1, 21, "ppc64le"),
        (2, 2, 21, "ppc64"),
        (1, 2, 40, "arm"),
        (2, 1, 243, "riscv64"),
    ];
    for (class, encoding, machine, expected) in cases {
        let mut bytes = [0_u8; 120];
        bytes[..4].copy_from_slice(b"\x7fELF");
        bytes[4] = class;
        bytes[5] = encoding;
        let write_u16 = |value: u16| {
            if encoding == 1 {
                value.to_le_bytes()
            } else {
                value.to_be_bytes()
            }
        };
        bytes[16..18].copy_from_slice(&write_u16(2));
        bytes[18..20].copy_from_slice(&write_u16(machine));
        if class == 2 {
            let offset = if encoding == 1 {
                64_u64.to_le_bytes()
            } else {
                64_u64.to_be_bytes()
            };
            bytes[32..40].copy_from_slice(&offset);
            bytes[54..56].copy_from_slice(&write_u16(56));
            bytes[56..58].copy_from_slice(&write_u16(1));
        } else {
            let offset = if encoding == 1 {
                64_u32.to_le_bytes()
            } else {
                64_u32.to_be_bytes()
            };
            bytes[28..32].copy_from_slice(&offset);
            bytes[42..44].copy_from_slice(&write_u16(32));
            bytes[44..46].copy_from_slice(&write_u16(1));
        }
        fs::write(&path, bytes).unwrap();
        let mut file = open_regular(&path).unwrap();
        assert_eq!(
            elf_identity(&mut file).unwrap(),
            (expected, false),
            "{class} {encoding} {machine}"
        );
    }
    fs::write(&path, b"\x7fELF").unwrap();
    assert!(elf_identity(&mut open_regular(&path).unwrap()).is_err());
}

#[test]
fn flatpak_bundle_publication_removes_executable_bits() {
    let root = TempDir::new().unwrap();
    let source_path = executable(root.path());
    let mut source = open_regular(&source_path).unwrap();
    publish_package(root.path(), &mut source, "package.flatpak", false, false).unwrap();
    let output = root.path().join("dist/package.flatpak");
    assert_eq!(fs::read(&output).unwrap(), fs::read(source_path).unwrap());
    assert_eq!(
        fs::metadata(output).unwrap().permissions().mode() & 0o111,
        0
    );
}

#[test]
fn publication_failure_preserves_prior_destination_and_cleans_staging() {
    let root = TempDir::new().unwrap();
    let source_path = executable(root.path());
    let dist = root.path().join("dist");
    fs::create_dir(&dist).unwrap();
    let destination = dist.join("package");
    fs::write(&destination, b"prior package").unwrap();
    let directory = open_directory(&dist).unwrap();
    let mut source = open_regular(&source_path).unwrap();
    let result = publish_impl(&mut source, &directory, "package", true, true, || {
        Err("injected commit failure".to_owned())
    });
    assert!(result.unwrap_err().contains("injected commit failure"));
    assert_eq!(fs::read(destination).unwrap(), b"prior package");
    assert_eq!(
        fs::read_dir(dist).unwrap().count(),
        1,
        "temporary output must be removed"
    );
}

#[test]
fn cargo_metadata_follows_real_manifest_alias_before_identity_comparison() {
    let root = TempDir::new().unwrap();
    let args = package_fixture(root.path(), "crates/app/Cargo.toml", &["app-id"], false);
    let manifest = write_file(root.path(), "crates/app/Cargo.toml", b"[package]\n");
    let alias = root.path().join("alias");
    symlink(manifest.parent().unwrap(), &alias).unwrap();
    let metadata = PathBuf::from(&args[1]);
    let mut value: serde_json::Value =
        serde_json::from_slice(&fs::read(&metadata).unwrap()).unwrap();
    value["packages"][0]["manifest_path"] = json!(alias.join("Cargo.toml"));
    fs::write(metadata, value.to_string()).unwrap();
    run(&args).unwrap();
    assert!(
        root.path()
            .join("dist/slimenrf-helper-1.2.3-linux-x64-no-firmware")
            .is_file()
    );
}
