#[path = "../build_support/build_version.rs"]
mod build_version;
#[path = "../build_support/official_release.rs"]
mod official_release;
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

#[test]
fn official_release_marker_requires_the_tagged_packaging_context() {
    const CHILD: &str = "SLIMENRF_TEST_OFFICIAL_RELEASE_CHILD";
    if std::env::var_os(CHILD).is_some() {
        println!();
        if let Err(error) = official_release::emit_official_release() {
            eprintln!("{error}");
            std::process::exit(1);
        }
        return;
    }

    fn run_marker(marker: Option<&str>, changes: &[(&str, Option<&str>)]) -> std::process::Output {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .arg("--exact")
            .arg(
                "build_resource_tests::official_release_marker_requires_the_tagged_packaging_context",
            )
            .arg("--nocapture")
            .env(CHILD, "1")
            .env("CARGO_PKG_VERSION", "0.1.1")
            .env("TARGET", "x86_64-pc-windows-msvc")
            .env("PROFILE", "release")
            .env("DEBUG", "false")
            .env("GITHUB_ACTIONS", "true")
            .env("GITHUB_EVENT_NAME", "push")
            .env("GITHUB_REPOSITORY", "jitingcn/SlimeNRF-Helper")
            .env("GITHUB_REF_TYPE", "tag")
            .env("GITHUB_REF_NAME", "v0.1.1")
            .env("GITHUB_REF", "refs/tags/v0.1.1")
            .env_remove("CARGO_CFG_DEBUG_ASSERTIONS")
            .env_remove("CARGO_FEATURE_EMBED_FIRMWARE")
            .env_remove("SLIMENRF_CONFIG_PATH")
            .env_remove("SLIMENRF_FIRMWARE_ROOT")
            .env_remove("SLIMENRF_OFFICIAL_RELEASE");
        if let Some(marker) = marker {
            command.env("SLIMENRF_OFFICIAL_RELEASE", marker);
        }
        for (name, value) in changes {
            if let Some(value) = value {
                command.env(name, value);
            } else {
                command.env_remove(name);
            }
        }
        command.output().unwrap()
    }

    let unmarked = run_marker(
        None,
        &[
            ("SLIMENRF_BUILD_OFFICIAL_TAG", Some("v9.9.9")),
            ("SLIMENRF_BUILD_OFFICIAL_PRERELEASE", Some("true")),
        ],
    );
    assert!(unmarked.status.success(), "{unmarked:?}");
    let unmarked_output = String::from_utf8_lossy(&unmarked.stdout);
    for expected in [
        "cargo:rustc-env=SLIMENRF_BUILD_OFFICIAL_TAG=",
        "cargo:rustc-env=SLIMENRF_BUILD_OFFICIAL_PRERELEASE=",
        "cargo:rerun-if-env-changed=SLIMENRF_OFFICIAL_RELEASE",
        "cargo:rerun-if-env-changed=SLIMENRF_CONFIG_PATH",
        "cargo:rerun-if-env-changed=SLIMENRF_FIRMWARE_ROOT",
        "cargo:rerun-if-env-changed=GITHUB_REF",
    ] {
        assert!(
            unmarked_output.lines().any(|line| line == expected),
            "missing {expected:?} in {unmarked_output:?}"
        );
    }

    for (channel, prerelease) in [("prerelease", "true"), ("release", "false")] {
        for embed in [false, true] {
            let firmware = if embed { Some("1") } else { None };
            let marker = format!("{channel}:v0.1.1");
            let output = run_marker(Some(&marker), &[("CARGO_FEATURE_EMBED_FIRMWARE", firmware)]);
            assert!(
                output.status.success(),
                "{marker}, embed={embed}: {output:?}"
            );
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                stdout
                    .lines()
                    .any(|line| line == "cargo:rustc-env=SLIMENRF_BUILD_OFFICIAL_TAG=v0.1.1"),
                "{marker}, embed={embed}: {stdout}"
            );
            let expected =
                format!("cargo:rustc-env=SLIMENRF_BUILD_OFFICIAL_PRERELEASE={prerelease}");
            assert!(
                stdout.lines().any(|line| line == expected),
                "{marker}, embed={embed}: {stdout}"
            );
        }
    }

    for marker in [
        "",
        "true",
        "nightly:v0.1.1",
        "prerelease:0.1.1",
        "prerelease:v0.1.2",
        "prerelease:v0.1.1\ncargo:rustc-env=INJECTED=1",
    ] {
        let output = run_marker(Some(marker), &[]);
        assert!(!output.status.success(), "{marker:?}: {output:?}");
    }

    for (name, value) in [
        ("TARGET", Some("x86_64-unknown-linux-gnu")),
        ("PROFILE", Some("debug")),
        ("DEBUG", Some("true")),
        ("CARGO_CFG_DEBUG_ASSERTIONS", Some("")),
        ("SLIMENRF_CONFIG_PATH", Some("")),
        ("SLIMENRF_CONFIG_PATH", Some("custom.yaml")),
        ("SLIMENRF_FIRMWARE_ROOT", Some("")),
        ("SLIMENRF_FIRMWARE_ROOT", Some("custom-firmware")),
        ("GITHUB_ACTIONS", None),
        ("GITHUB_EVENT_NAME", Some("workflow_dispatch")),
        ("GITHUB_REPOSITORY", Some("someone/SlimeNRF-Helper")),
        ("GITHUB_REF_TYPE", Some("branch")),
        ("GITHUB_REF_NAME", Some("v0.1.2")),
        ("GITHUB_REF", Some("refs/tags/v0.1.2")),
    ] {
        let output = run_marker(Some("prerelease:v0.1.1"), &[(name, value)]);
        assert!(!output.status.success(), "{name}={value:?}: {output:?}");
    }

    for name in ["SLIMENRF_CONFIG_PATH", "SLIMENRF_FIRMWARE_ROOT"] {
        let output = run_marker(
            Some("prerelease:v0.1.1"),
            &[
                (name, Some("custom")),
                ("CARGO_FEATURE_EMBED_FIRMWARE", Some("1")),
            ],
        );
        assert!(
            !output.status.success(),
            "full build with {name}: {output:?}"
        );
    }
}

#[cfg(target_os = "windows")]
#[test]
fn windows_packager_requires_an_explicit_release_tag() {
    for (tag, expected) in [
        (None, "<unset>|custom.yaml"),
        (Some("v0.1.1"), "prerelease:v0.1.1|custom.yaml"),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let scripts = root.join("scripts");
        fs::create_dir(&scripts).unwrap();
        fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../package.ps1"),
            root.join("package.ps1"),
        )
        .unwrap();
        fs::write(
            scripts.join("environment.ps1"),
            r#"
$script:WindowsTarget = 'x86_64-pc-windows-msvc'
function Initialize-SlimeNRFBuildEnvironment {}
function Invoke-SlimeNRFCargo {
    param([string]$Action, [switch]$NoFirmware)
    $marker = [Environment]::GetEnvironmentVariable('SLIMENRF_OFFICIAL_RELEASE', 'Process')
    if ($null -eq $marker) { $marker = '<unset>' }
    [IO.File]::WriteAllText($env:SLIMENRF_TEST_PACKAGE_CAPTURE, "$marker|$env:SLIMENRF_CONFIG_PATH")
    $script:CargoExitCode = 0
}
function Restore-SlimeNRFBuildEnvironment { param($Environment, $Directory) }
function cargo {
    $global:LASTEXITCODE = 0
    @{
        packages = @(@{ name = 'slimenrf-helper'; version = '0.1.1' })
        target_directory = $env:SLIMENRF_TEST_PACKAGE_TARGET
    } | ConvertTo-Json -Depth 4 -Compress
}
"#,
        )
        .unwrap();
        fs::write(
            scripts.join("package-upx.ps1"),
            r#"
param([string]$InputPath, [string]$OutputPath, [switch]$Force)
$global:LASTEXITCODE = 0
"#,
        )
        .unwrap();
        let capture = root.join("capture.txt");
        let mut command = Command::new("powershell.exe");
        command
            .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
            .arg(root.join("package.ps1"))
            .arg("-NoFirmware")
            .env("SLIMENRF_TEST_PACKAGE_CAPTURE", &capture)
            .env("SLIMENRF_TEST_PACKAGE_TARGET", root.join("target"))
            .env("SLIMENRF_OFFICIAL_RELEASE", "prerelease:v9.9.9")
            .env("SLIMENRF_CONFIG_PATH", "custom.yaml");
        if let Some(tag) = tag {
            command.args(["-OfficialPrereleaseTag", tag]);
        }
        let output = command.output().unwrap();
        assert!(output.status.success(), "tag={tag:?}: {output:?}");
        assert_eq!(fs::read_to_string(capture).unwrap(), expected);
    }
}
