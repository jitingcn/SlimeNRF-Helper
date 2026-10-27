#[path = "../build_support/resources.rs"]
mod resources;

use std::{
    fs::{self, File},
    io::Read as _,
    path::{Path, PathBuf},
    process::Command,
};
use zip::ZipArchive;

struct Case {
    name: &'static str,
    config: Option<PathBuf>,
    firmware: Option<PathBuf>,
    embed: bool,
    marker: &'static str,
    image: Option<&'static [u8]>,
}

fn write_file(root: &Path, path: &str, bytes: &[u8]) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

fn member(archive: &mut ZipArchive<File>, name: &str) -> Vec<u8> {
    let mut file = archive.by_name(name).unwrap();
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).unwrap();
    bytes
}

#[test]
fn resource_paths_follow_repository_and_config_roots() {
    const CHILD: &str = "SLIMENRF_TEST_RESOURCE_BUILD_CHILD";
    if std::env::var_os(CHILD).is_some() {
        resources::build_resource_bundle().expect("build resource bundle in isolated process");
        return;
    }

    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let app = root.join("crates/app");
    let cwd = root.join("unrelated working directory");
    fs::create_dir_all(&app).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    write_file(
        root,
        "slimenrf-helper.yaml",
        b"schema_version: 1\nfirmware_root: default-firmware\nmarker: default\n",
    );
    write_file(
        root,
        "config/nested/selected.yaml",
        b"schema_version: 1\nfirmware_root: ../yaml-firmware\nmarker: selected\n",
    );
    write_file(root, "default-firmware/image.uf2", b"default image");
    write_file(root, "config/yaml-firmware/image.uf2", b"yaml-parent image");
    write_file(root, "override-firmware/image.uf2", b"override image");

    let cases = [
        Case {
            name: "default",
            config: None,
            firmware: None,
            embed: true,
            marker: "default",
            image: Some(b"default image"),
        },
        Case {
            name: "relative-config",
            config: Some("config/nested/selected.yaml".into()),
            firmware: None,
            embed: true,
            marker: "selected",
            image: Some(b"yaml-parent image"),
        },
        Case {
            name: "relative-overrides",
            config: Some("config/nested/selected.yaml".into()),
            firmware: Some("override-firmware".into()),
            embed: true,
            marker: "selected",
            image: Some(b"override image"),
        },
        Case {
            name: "absolute-overrides",
            config: Some(root.join("config/nested/selected.yaml")),
            firmware: Some(root.join("override-firmware")),
            embed: true,
            marker: "selected",
            image: Some(b"override image"),
        },
        Case {
            name: "no-firmware",
            config: Some("config/nested/selected.yaml".into()),
            firmware: Some("missing-firmware".into()),
            embed: false,
            marker: "selected",
            image: None,
        },
    ];

    for case in cases {
        let output_dir = root.join(case.name);
        fs::create_dir_all(&output_dir).unwrap();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .arg("--exact")
            .arg("build_resource_tests::resource_paths_follow_repository_and_config_roots")
            .arg("--nocapture")
            .current_dir(&cwd)
            .env(CHILD, "1")
            .env("CARGO_MANIFEST_DIR", &app)
            .env("OUT_DIR", &output_dir)
            .env_remove("SLIMENRF_CONFIG_PATH")
            .env_remove("SLIMENRF_FIRMWARE_ROOT")
            .env_remove("CARGO_FEATURE_EMBED_FIRMWARE");
        if let Some(config) = &case.config {
            command.env("SLIMENRF_CONFIG_PATH", config);
        }
        if let Some(firmware) = &case.firmware {
            command.env("SLIMENRF_FIRMWARE_ROOT", firmware);
        }
        if case.embed {
            command.env("CARGO_FEATURE_EMBED_FIRMWARE", "1");
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}: stdout={} stderr={}",
            case.name,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        let mut archive =
            ZipArchive::new(File::open(output_dir.join("firmware_bundle.zip")).unwrap()).unwrap();
        let config: serde_yaml::Value =
            serde_yaml::from_slice(&member(&mut archive, "slimenrf-helper.yaml")).unwrap();
        assert_eq!(
            config["marker"].as_str(),
            Some(case.marker),
            "{}",
            case.name
        );
        assert_eq!(
            archive.len(),
            if case.embed { 2 } else { 1 },
            "{}",
            case.name
        );
        if let Some(expected) = case.image {
            assert_eq!(
                member(&mut archive, "firmwares/image.uf2").as_slice(),
                expected,
                "{}",
                case.name
            );
        }
    }
}
