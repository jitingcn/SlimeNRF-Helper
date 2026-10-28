use std::env;
use std::ffi::{CString, OsStr};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde::de::{self, Deserialize, Deserializer, IgnoredAny, MapAccess, Visitor};
use serde_json::{Value, json};
use tempfile::TempDir;

const APP_ID: &str = "io.github.jitingcn.SlimeNRFHelper";
const RUNTIME_REPO: &str = "https://dl.flathub.org/repo/flathub.flatpakrepo";
const RUST_DIST: &str = "https://static.rust-lang.org/dist";
const RUST_PREFIX: &str = "/app/lib/slimenrf-build-rust";
const MANIFEST_LIMIT: u64 = 4 * 1024 * 1024;
const PIN_LIMIT: usize = 64 * 1024;

// An explicit inventory keeps caches, private configuration, and user data out of the SDK.
const SOURCE_FILES: &[&str] = &[
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
    "LICENSE-MIT",
    "LICENSE-APACHE",
    "scripts/notices-inputs.json",
    "crates/app/Cargo.toml",
    "crates/app/build.rs",
    "crates/firmware-image/Cargo.toml",
    "crates/recording/Cargo.toml",
    "crates/recording/build.rs",
    "crates/terminal-core/Cargo.toml",
    "crates/tools/Cargo.toml",
];
const SOURCE_TREES: &[&str] = &[
    "crates/app/src",
    "crates/app/build_support",
    "locales",
    "crates/firmware-image/src",
    "crates/recording/src",
    "crates/recording/vendor/vqf",
    "crates/terminal-core/src",
    "crates/tools/src",
];
const EXCLUDED: &[&str] = &[
    "target",
    "dist",
    ".git",
    ".flatpak-builder",
    "__pycache__",
    ".cache",
];

fn read_regular(path: &Path) -> Result<Vec<u8>, String> {
    let mut file = super::open_regular(path)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|error| format!("Cannot read {}: {error}", path.display()))?;
    Ok(bytes)
}

fn manifest_template(root: &Path) -> Result<Value, String> {
    let path = root.join("scripts/flatpak").join(format!("{APP_ID}.json"));
    let file = super::open_regular(&path)?;
    serde_json::from_reader(file)
        .map_err(|error| format!("Cannot read Flatpak manifest {}: {error}", path.display()))
}

fn executable_on_path(name: &str) -> bool {
    let search_path = env::var_os("PATH").unwrap_or_else(|| "/bin:/usr/bin".into());
    if search_path.is_empty() {
        return false;
    }
    env::split_paths(&search_path).any(|directory| {
        let candidate = directory.join(name);
        let Ok(metadata) = fs::metadata(&candidate) else {
            return false;
        };
        if !metadata.is_file() {
            return false;
        }
        let Ok(path) = CString::new(candidate.as_os_str().as_bytes()) else {
            return false;
        };
        // SAFETY: path is NUL-terminated and remains alive throughout the access call.
        unsafe { libc::access(path.as_ptr(), libc::X_OK) == 0 }
    })
}

pub(super) fn check_tools(root: &Path, _no_firmware: bool) -> Result<String, String> {
    let missing: Vec<_> = ["cargo", "flatpak", "flatpak-builder"]
        .into_iter()
        .filter(|tool| !executable_on_path(tool))
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "Missing Flatpak build prerequisites: {}. Install flatpak and flatpak-builder using \
             your distribution package manager (Arch: sudo pacman -S --needed flatpak flatpak-builder; \
             Debian/Ubuntu: sudo apt install flatpak flatpak-builder). Cargo must be on PATH.",
            missing.join(", ")
        ));
    }
    let manifest = manifest_template(root)?;
    let output = Command::new("flatpak")
        .arg("--default-arch")
        .output()
        .map_err(|error| format!("Cannot run flatpak --default-arch: {error}"))?;
    if !output.status.success() {
        return Err(format!("flatpak --default-arch failed: {}", output.status));
    }
    let architecture = String::from_utf8(output.stdout)
        .map_err(|error| format!("Invalid Flatpak architecture output: {error}"))?
        .trim()
        .to_owned();
    if !matches!(architecture.as_str(), "x86_64" | "aarch64") {
        return Err(format!(
            "Freedesktop SDK Flatpak packaging supports x86_64/aarch64, not {architecture:?}."
        ));
    }
    let branch = manifest
        .get("runtime-version")
        .and_then(Value::as_str)
        .ok_or("Flatpak manifest has no runtime-version")?;
    let mut refs = Vec::new();
    for key in ["runtime", "sdk"] {
        let name = manifest
            .get(key)
            .and_then(Value::as_str)
            .ok_or_else(|| format!("Flatpak manifest has no {key}"))?;
        refs.push(format!("{name}/{architecture}/{branch}"));
    }
    let mut absent = Vec::new();
    for reference in &refs {
        let status = Command::new("flatpak")
            .args(["info", "--show-ref", &format!("runtime/{reference}")])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|error| format!("Cannot inspect Flatpak runtime {reference}: {error}"))?;
        if !status.success() {
            absent.push(reference.as_str());
        }
    }
    if !absent.is_empty() {
        return Err(format!(
            "Install the Freedesktop runtime and SDK first.\n  Missing: {}\n  \
             flatpak remote-add --user --if-not-exists flathub {RUNTIME_REPO}\n  \
             flatpak install --user flathub {}\n\
             Packaging never installs runtimes or changes system permissions.",
            absent.join(", "),
            refs.join(" ")
        ));
    }
    Ok(architecture)
}

fn stable_numeric_version(version: &str) -> bool {
    let mut parts = version.split('.');
    let Some(major) = parts.next() else {
        return false;
    };
    let Some(minor) = parts.next() else {
        return false;
    };
    let Some(patch) = parts.next() else {
        return false;
    };
    let digits = |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
    digits(major)
        && !major.starts_with('0')
        && digits(minor)
        && digits(patch)
        && parts.next().is_none()
}

fn parse_rust_pin(bytes: &[u8]) -> Result<String, String> {
    if bytes.len() > PIN_LIMIT {
        return Err("rust-toolchain.toml exceeds 64 KiB.".into());
    }
    let text = std::str::from_utf8(bytes)
        .map_err(|error| format!("Cannot read the Flatpak Rust pin: {error}"))?;
    let pin: toml::Value = toml::from_str(text)
        .map_err(|error| format!("Cannot read the Flatpak Rust pin: {error}"))?;
    let version = pin
        .get("toolchain")
        .and_then(|table| table.get("channel"))
        .and_then(toml::Value::as_str)
        .ok_or("Cannot read the Flatpak Rust pin: missing toolchain.channel")?;
    if !stable_numeric_version(version) {
        return Err(
            "Flatpak needs an explicit numeric stable toolchain.channel in rust-toolchain.toml."
                .into(),
        );
    }
    Ok(version.to_owned())
}

fn rust_target(architecture: &str) -> Result<&'static str, String> {
    match architecture {
        "x86_64" => Ok("x86_64-unknown-linux-gnu"),
        "aarch64" => Ok("aarch64-unknown-linux-gnu"),
        _ => Err(format!(
            "No official native Flatpak Rust target for {architecture:?}."
        )),
    }
}

fn build_toolchain_module(
    version: &str,
    architecture: &str,
    metadata: &str,
) -> Result<Value, String> {
    let target = rust_target(architecture)?;
    let release: toml::Value = toml::from_str(metadata)
        .map_err(|error| format!("Invalid Rust release manifest: {error}"))?;
    if release
        .get("manifest-version")
        .and_then(toml::Value::as_str)
        != Some("2")
    {
        return Err("Unsupported Rust release manifest format.".into());
    }
    let date = release
        .get("date")
        .and_then(toml::Value::as_str)
        .ok_or("Invalid Rust release date.")?;
    let date_bytes = date.as_bytes();
    if date_bytes.len() != 10
        || date_bytes[4] != b'-'
        || date_bytes[7] != b'-'
        || date_bytes
            .iter()
            .enumerate()
            .any(|(index, byte)| index != 4 && index != 7 && !byte.is_ascii_digit())
    {
        return Err("Invalid Rust release date.".into());
    }
    let mut sources = Vec::new();
    let mut commands = Vec::new();
    for component in ["rustc", "cargo", "rust-std"] {
        let archive = release
            .get("pkg")
            .and_then(|value| value.get(component))
            .and_then(|value| value.get("target"))
            .and_then(|value| value.get(target))
            .ok_or_else(|| format!("Missing official {component} archive for {target}."))?;
        if archive.get("available").and_then(toml::Value::as_bool) != Some(true) {
            return Err(format!("{component} is unavailable for {target}."));
        }
        let expected_url = format!("{RUST_DIST}/{date}/{component}-{version}-{target}.tar.xz");
        if archive.get("xz_url").and_then(toml::Value::as_str) != Some(expected_url.as_str()) {
            return Err(format!("Unexpected official {component} archive URL."));
        }
        let checksum = archive
            .get("xz_hash")
            .and_then(toml::Value::as_str)
            .ok_or_else(|| format!("Invalid SHA256 for {component}/{target}."))?;
        if checksum.len() != 64
            || !checksum
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(format!("Invalid SHA256 for {component}/{target}."));
        }
        sources.push(json!({
            "type": "archive", "url": expected_url, "sha256": checksum, "dest": component
        }));
        commands.push(format!(
            "./{component}/install.sh --prefix={RUST_PREFIX} --disable-ldconfig"
        ));
    }
    for executable in ["rustc", "cargo"] {
        commands.push(format!(
            "test \"$({RUST_PREFIX}/bin/{executable} --version | cut -d \" \" -f 2)\" = \"{version}\""
        ));
        commands.push(format!("{RUST_PREFIX}/bin/{executable} --version"));
    }
    Ok(json!({
        "name": "rust-toolchain",
        "buildsystem": "simple",
        "build-options": {"build-args": ["--unshare=network"], "no-debuginfo": true},
        "build-commands": commands,
        "sources": sources,
        "cleanup": ["/lib/slimenrf-build-rust"]
    }))
}

fn read_release_manifest(reader: impl Read) -> Result<String, String> {
    let mut bytes = Vec::new();
    reader
        .take(MANIFEST_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MANIFEST_LIMIT {
        return Err("Rust release manifest exceeds 4 MiB.".into());
    }
    String::from_utf8(bytes).map_err(|error| error.to_string())
}

fn rust_toolchain_module(source: &Path, architecture: &str) -> Result<Value, String> {
    let pin_path = source.join("rust-toolchain.toml");
    let mut pin_bytes = Vec::new();
    super::open_regular(&pin_path)?
        .take(PIN_LIMIT as u64 + 1)
        .read_to_end(&mut pin_bytes)
        .map_err(|error| format!("Cannot read the Flatpak Rust pin: {error}"))?;
    let version = parse_rust_pin(&pin_bytes)?;
    let target = rust_target(architecture)?;
    let url = format!("{RUST_DIST}/channel-rust-{version}.toml");
    let fetch = || -> Result<Value, String> {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| error.to_string())?;
        let response = client.get(&url).send().map_err(|error| error.to_string())?;
        if response.url().as_str() != url || !response.status().is_success() {
            return Err(format!(
                "Unexpected Rust release response: {} {}",
                response.status(),
                response.url()
            ));
        }
        let metadata = read_release_manifest(response)?;
        build_toolchain_module(&version, architecture, &metadata)
    };
    fetch().map_err(|error| {
        format!("Cannot resolve official Rust {version} for {target} from {url}: {error}")
    })
}

struct FirmwareField(Option<String>);

impl<'de> Deserialize<'de> for FirmwareField {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct MappingVisitor;
        impl<'de> Visitor<'de> for MappingVisitor {
            type Value = FirmwareField;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a configuration mapping")
            }

            fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
            where
                M: MapAccess<'de>,
            {
                let mut field = None;
                while let Some(key) = map.next_key::<serde_yaml::Value>()? {
                    let mut scalar = &key;
                    while let serde_yaml::Value::Tagged(tagged) = scalar {
                        scalar = &tagged.value;
                    }
                    if scalar.as_str() == Some("firmware_root") {
                        // Requesting a string preserves the source scalar spelling (e.g. 0123).
                        let value = map.next_value::<String>()?;
                        if field.replace(value).is_some() {
                            return Err(de::Error::custom("duplicate firmware_root"));
                        }
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                Ok(FirmwareField(field))
            }
        }
        deserializer.deserialize_map(MappingVisitor)
    }
}

fn firmware_root(
    root: &Path,
    selected_config: &Path,
    config_bytes: &[u8],
    override_path: Option<&Path>,
) -> Result<PathBuf, String> {
    if let Some(path) = override_path {
        return Ok(root.join(path));
    }
    let field: FirmwareField = serde_yaml::from_slice(config_bytes).map_err(|error| {
        format!(
            "Cannot read firmware_root in {}: {error}",
            selected_config.display()
        )
    })?;
    let value = field
        .0
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            format!(
                "Configuration needs one nonempty scalar firmware_root: {}",
                selected_config.display()
            )
        })?;
    let parent = selected_config
        .parent()
        .ok_or_else(|| format!("Configuration has no parent: {}", selected_config.display()))?;
    Ok(parent.join(value))
}

fn copy_file(source: &Path, destination: &Path) -> Result<(), String> {
    let mut input = super::open_regular(source)?;
    let mode = input
        .metadata()
        .map_err(|error| format!("Cannot inspect {}: {error}", source.display()))?
        .permissions()
        .mode()
        & 0o777;
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("Cannot create {}: {error}", parent.display()))?;
    }
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .map_err(|error| format!("Cannot create {}: {error}", destination.display()))?;
    std::io::copy(&mut input, &mut output).map_err(|error| {
        format!(
            "Cannot copy {} to {}: {error}",
            source.display(),
            destination.display()
        )
    })?;
    output
        .set_permissions(fs::Permissions::from_mode(mode))
        .map_err(|error| {
            format!(
                "Cannot set permissions on {}: {error}",
                destination.display()
            )
        })
}

fn sorted_entries(path: &Path) -> Result<Vec<fs::DirEntry>, String> {
    let mut entries = fs::read_dir(path)
        .map_err(|error| format!("Cannot read directory {}: {error}", path.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("Cannot read directory {}: {error}", path.display()))?;
    entries.sort_by_key(|entry| entry.file_name());
    Ok(entries)
}

fn copy_tree(source: &Path, destination: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(source)
        .map_err(|error| format!("Cannot inspect source tree {}: {error}", source.display()))?;
    if !metadata.is_dir() {
        return Err(format!(
            "Source tree must be a real directory: {}",
            source.display()
        ));
    }
    fs::create_dir_all(destination)
        .map_err(|error| format!("Cannot create {}: {error}", destination.display()))?;
    for entry in sorted_entries(source)? {
        let name = entry.file_name();
        if EXCLUDED.iter().any(|excluded| name == OsStr::new(excluded)) {
            continue;
        }
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| format!("Cannot inspect {}: {error}", path.display()))?;
        if file_type.is_dir() {
            copy_tree(&path, &destination.join(&name))?;
        } else if file_type.is_file() {
            copy_file(&path, &destination.join(&name))?;
        } else {
            return Err(format!(
                "Unsupported source entry (symlinks are not staged): {}",
                path.display()
            ));
        }
    }
    Ok(())
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| format!("Cannot create {}: {error}", path.display()))?;
    file.write_all(bytes)
        .map_err(|error| format!("Cannot write {}: {error}", path.display()))
}

fn stage_sources(
    root: &Path,
    stage: &Path,
    no_firmware: bool,
    selected_config: &Path,
    firmware_override: Option<&Path>,
) -> Result<PathBuf, String> {
    let source = stage.join("source");
    for relative in SOURCE_FILES {
        copy_file(&root.join(relative), &source.join(relative))?;
    }
    for relative in SOURCE_TREES {
        copy_tree(&root.join(relative), &source.join(relative))?;
    }
    // The selected config may itself be a symlink, but its original parent owns relative paths.
    let resolved_config = fs::canonicalize(selected_config).map_err(|error| {
        format!(
            "Cannot resolve configuration {}: {error}",
            selected_config.display()
        )
    })?;
    let config_bytes = read_regular(&resolved_config)?;
    write_new(&source.join("slimenrf-helper.yaml"), &config_bytes)?;
    if !no_firmware {
        let selected_root = firmware_root(root, selected_config, &config_bytes, firmware_override)?;
        if !selected_root.is_dir() {
            return Err(format!(
                "Firmware directory is missing: {}; restore inputs or set \
                 SLIMENRF_FIRMWARE_ROOT (relative to the application directory).",
                selected_root.display()
            ));
        }
        let destination = source.join("flatpak-firmwares");
        fs::create_dir(&destination)
            .map_err(|error| format!("Cannot create {}: {error}", destination.display()))?;
        let mut count = 0;
        for entry in sorted_entries(&selected_root)? {
            let path = entry.path();
            let file_type = entry
                .file_type()
                .map_err(|error| format!("Cannot inspect {}: {error}", path.display()))?;
            let allowed = path
                .extension()
                .and_then(OsStr::to_str)
                .is_some_and(|extension| {
                    ["uf2", "hex", "bin", "zip"]
                        .iter()
                        .any(|allowed| extension.eq_ignore_ascii_case(allowed))
                });
            if file_type.is_file() && allowed {
                copy_file(&path, &destination.join(entry.file_name()))?;
                count += 1;
            }
        }
        if count == 0 {
            return Err(format!(
                "{} contains no top-level UF2/HEX/BIN/ZIP firmware files.",
                selected_root.display()
            ));
        }
        println!(
            "Flatpak staging: {count} firmware inputs from {}",
            selected_root.display()
        );
    }
    copy_file(
        &root
            .join("scripts/flatpak")
            .join(format!("{APP_ID}.desktop")),
        &stage.join(format!("{APP_ID}.desktop")),
    )?;
    copy_file(
        &root.join("scripts/appimage/slimenrf-helper.svg"),
        &stage.join(format!("{APP_ID}.svg")),
    )?;
    Ok(source)
}

fn vendor_sources(source: &Path) -> Result<(), String> {
    let output = Command::new("cargo")
        .args(["vendor", "--locked", "--versioned-dirs", "vendor"])
        .current_dir(source)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|error| format!("Cannot run cargo vendor: {error}"))?;
    if !output.status.success() {
        return Err(format!("cargo vendor failed: {}", output.status));
    }
    std::str::from_utf8(&output.stdout)
        .map_err(|error| format!("Invalid cargo vendor configuration: {error}"))?;
    let config_dir = source.join(".cargo");
    fs::create_dir(&config_dir)
        .map_err(|error| format!("Cannot create {}: {error}", config_dir.display()))?;
    write_new(&config_dir.join("config.toml"), &output.stdout)
}

fn cache_path(root: &Path) -> Result<PathBuf, String> {
    let override_path = env::var_os("SLIMENRF_FLATPAK_CACHE").filter(|value| !value.is_empty());
    let cache = if let Some(path) = override_path {
        root.join(path)
    } else {
        let base = match env::var_os("XDG_CACHE_HOME").filter(|value| !value.is_empty()) {
            Some(value) => PathBuf::from(value),
            None => super::appimage::home_directory()?.join(".cache"),
        };
        if !base.is_absolute() {
            return Err("XDG_CACHE_HOME must be absolute for Flatpak caching.".into());
        }
        base.join("slimenrf-helper/flatpak")
    };
    fs::create_dir_all(&cache)
        .map_err(|error| format!("Cannot create Flatpak cache {}: {error}", cache.display()))?;
    fs::canonicalize(&cache)
        .map_err(|error| format!("Cannot resolve Flatpak cache {}: {error}", cache.display()))
}

fn lock_cache(path: &Path) -> Result<File, String> {
    let lock = OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|error| format!("Cannot open Flatpak build lock {}: {error}", path.display()))?;
    if !lock
        .metadata()
        .map_err(|error| format!("Cannot inspect Flatpak build lock: {error}"))?
        .is_file()
    {
        return Err(format!(
            "Flatpak build lock is not a regular file: {}",
            path.display()
        ));
    }
    loop {
        // SAFETY: lock owns a live file descriptor; flock does not retain the pointer or descriptor.
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } == 0 {
            break;
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(format!(
                "Cannot lock Flatpak cache {}: {error}",
                path.display()
            ));
        }
    }
    Ok(lock)
}

fn configure_manifest(
    mut manifest: Value,
    toolchain: Value,
    target: &Path,
    jobs: &str,
    no_firmware: bool,
) -> Result<Value, String> {
    let target = target
        .to_str()
        .ok_or("Flatpak SDK target path is not valid UTF-8.")?;
    let modules = manifest
        .get_mut("modules")
        .and_then(Value::as_array_mut)
        .ok_or("Flatpak manifest has no modules")?;
    let module = modules
        .first_mut()
        .ok_or("Flatpak manifest has no application module")?;
    let options = module
        .get_mut("build-options")
        .and_then(Value::as_object_mut)
        .ok_or("Flatpak application has no build-options")?;
    let vars = options
        .get_mut("env")
        .and_then(Value::as_object_mut)
        .ok_or("Flatpak application has no build environment")?;
    vars.insert("CARGO_TARGET_DIR".into(), json!(target));
    vars.insert("CARGO_BUILD_JOBS".into(), json!(jobs));
    options.insert(
        "build-args".into(),
        json!([format!("--filesystem={target}"), "--unshare=network"]),
    );
    if no_firmware {
        let commands = module
            .get_mut("build-commands")
            .and_then(Value::as_array_mut)
            .ok_or("Flatpak application has no build-commands")?;
        let Some(Value::String(command)) = commands.first_mut() else {
            return Err("Flatpak application has no Cargo build command".into());
        };
        command.push_str(" --no-default-features");
    }
    modules.insert(0, toolchain);
    Ok(manifest)
}

fn run_command(command: &mut Command) -> Result<(), String> {
    let description = format!("{command:?}");
    let status = command
        .status()
        .map_err(|error| format!("Cannot run {description}: {error}"))?;
    if !status.success() {
        return Err(format!("{description} failed: {status}"));
    }
    Ok(())
}

pub(super) fn build(
    root: &Path,
    no_firmware: bool,
    version: &str,
) -> Result<(TempDir, PathBuf, String), String> {
    let architecture = check_tools(root, no_firmware)?;
    let cache = cache_path(root)?.join(&architecture);
    fs::create_dir_all(&cache).map_err(|error| {
        format!(
            "Cannot create Flatpak architecture cache {}: {error}",
            cache.display()
        )
    })?;
    let target = cache.join("sdk-target");
    fs::create_dir_all(&target).map_err(|error| {
        format!(
            "Cannot create Flatpak SDK target {}: {error}",
            target.display()
        )
    })?;
    let jobs = env::var_os("SLIMENRF_FLATPAK_JOBS")
        .or_else(|| env::var_os("CARGO_BUILD_JOBS"))
        .unwrap_or_else(|| "2".into());
    let jobs = jobs
        .to_str()
        .ok_or("SLIMENRF_FLATPAK_JOBS/CARGO_BUILD_JOBS must be a positive integer.")?;
    if jobs.is_empty()
        || !jobs.bytes().all(|byte| byte.is_ascii_digit())
        || !jobs.bytes().any(|byte| byte != b'0')
    {
        return Err("SLIMENRF_FLATPAK_JOBS/CARGO_BUILD_JOBS must be a positive integer.".into());
    }
    let variant = if no_firmware { "no-firmware" } else { "full" };
    // The lock covers both builder/OSTree and SDK Cargo caches, across variants.
    let _lock = lock_cache(&cache.join("build.lock"))?;
    let temporary = tempfile::Builder::new()
        .prefix("stage-")
        .tempdir_in(&cache)
        .map_err(|error| format!("Cannot create Flatpak staging directory: {error}"))?;
    let stage = temporary.path();
    let selected_config = env::var_os("SLIMENRF_CONFIG_PATH")
        .map_or_else(|| root.join("slimenrf-helper.yaml"), |path| root.join(path));
    let firmware_override = env::var_os("SLIMENRF_FIRMWARE_ROOT").map(PathBuf::from);
    let source = stage_sources(
        root,
        stage,
        no_firmware,
        &selected_config,
        firmware_override.as_deref(),
    )?;
    vendor_sources(&source)?;
    let toolchain = rust_toolchain_module(&source, &architecture)?;
    let manifest = configure_manifest(
        manifest_template(root)?,
        toolchain,
        &target,
        jobs,
        no_firmware,
    )?;
    let manifest_path = stage.join(format!("{APP_ID}.json"));
    let mut manifest_bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|error| format!("Cannot serialize Flatpak manifest: {error}"))?;
    manifest_bytes.push(b'\n');
    write_new(&manifest_path, &manifest_bytes)?;
    let state = cache.join("builder-state");
    let output = cache.join(format!("build-{variant}"));
    let repository = cache.join(format!("repo-{variant}"));
    println!(
        "Flatpak SDK build: {architecture}, {variant}; isolated cache: {}",
        cache.display()
    );
    run_command(
        Command::new("flatpak-builder")
            .args([
                "--user",
                &format!("--arch={architecture}"),
                "--download-only",
            ])
            .arg(format!("--state-dir={}", state.display()))
            .arg(&output)
            .arg(&manifest_path),
    )?;
    run_command(
        Command::new("flatpak-builder")
            .args([
                "--user",
                &format!("--arch={architecture}"),
                "--force-clean",
                "--disable-download",
            ])
            .arg(format!("--state-dir={}", state.display()))
            .arg(format!("--repo={}", repository.display()))
            .arg(&output)
            .arg(&manifest_path),
    )?;
    let bundle = stage.join(format!("{APP_ID}-{version}.flatpak"));
    run_command(
        Command::new("flatpak")
            .args([
                "build-bundle",
                &format!("--arch={architecture}"),
                &format!("--runtime-repo={RUNTIME_REPO}"),
            ])
            .arg(&repository)
            .arg(&bundle)
            .args([APP_ID, "master"]),
    )?;
    Ok((temporary, bundle, architecture))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use tempfile::TempDir;

    const INPUTS: &[&str] = &[
        "Cargo.toml",
        "Cargo.lock",
        "rust-toolchain.toml",
        "LICENSE-MIT",
        "LICENSE-APACHE",
        "scripts/notices-inputs.json",
        "crates/app/Cargo.toml",
        "crates/app/build.rs",
        "crates/app/src/main.rs",
        "crates/app/build_support/notices.rs",
        "locales/en.yml",
        "crates/recording/Cargo.toml",
        "crates/recording/build.rs",
        "crates/recording/src/lib.rs",
        "crates/recording/vendor/vqf/LICENSE",
        "crates/firmware-image/Cargo.toml",
        "crates/firmware-image/src/lib.rs",
        "crates/firmware-image/src/parsers/nested.rs",
        "crates/terminal-core/Cargo.toml",
        "crates/terminal-core/src/lib.rs",
        "crates/terminal-core/src/model/nested.rs",
        "crates/tools/Cargo.toml",
        "crates/tools/src/lib.rs",
        "crates/tools/src/packaging/flatpak.rs",
    ];

    fn put(root: &Path, relative: &str, bytes: &[u8]) -> PathBuf {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, bytes).unwrap();
        path
    }

    fn fixture(root: &Path) {
        for relative in INPUTS.iter().copied().chain([
            "scripts/appimage/slimenrf-helper.svg",
            "scripts/flatpak/io.github.jitingcn.SlimeNRFHelper.desktop",
        ]) {
            put(
                root,
                relative,
                format!("fixture for {relative}\n").as_bytes(),
            );
        }
        for relative in [
            "crates/app/src/.cache/private.txt",
            "crates/firmware-image/src/target/private.txt",
            "crates/terminal-core/src/.cache/private.txt",
            "crates/tools/src/dist/private.txt",
        ] {
            put(root, relative, b"excluded\n");
        }
    }

    #[test]
    fn flatpak_stages_real_workspace_inputs_without_empty_examples() {
        let temporary = TempDir::new().unwrap();
        let root = temporary.path().join("workspace");
        fixture(&root);
        let config = put(&root, "config/selected.yaml", b"firmware_root: ignored\n");
        put(&root, "firmware/test.UF2", b"firmware\n");
        put(&root, "firmware/test.HeX", b"hex\n");
        put(&root, "firmware/nested/not-staged.uf2", b"nested\n");
        put(&root, "firmware/private.txt", b"not firmware\n");
        symlink(
            root.join("firmware/test.UF2"),
            root.join("firmware/link.bin"),
        )
        .unwrap();
        fs::set_permissions(
            root.join("crates/app/src/main.rs"),
            fs::Permissions::from_mode(0o751),
        )
        .unwrap();

        for no_firmware in [true, false] {
            let stage = temporary.path().join(if no_firmware {
                "no-firmware-stage"
            } else {
                "full-stage"
            });
            fs::create_dir(&stage).unwrap();
            let source = stage_sources(
                &root,
                &stage,
                no_firmware,
                &config,
                Some(Path::new("firmware")),
            )
            .unwrap();
            for relative in INPUTS {
                assert_eq!(
                    fs::read(source.join(relative)).unwrap(),
                    fs::read(root.join(relative)).unwrap(),
                    "{relative}"
                );
            }
            assert_eq!(
                fs::read(source.join("slimenrf-helper.yaml")).unwrap(),
                b"firmware_root: ignored\n"
            );
            assert_eq!(
                fs::metadata(source.join("crates/app/src/main.rs"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o751
            );
            for absent in [
                "examples",
                "src",
                "build_support",
                "crates/app/src/.cache",
                "crates/firmware-image/src/target",
                "crates/terminal-core/src/.cache",
                "crates/tools/src/dist",
            ] {
                assert!(!source.join(absent).exists(), "unexpected staged {absent}");
            }
            for (staged, original) in [
                (
                    format!("{APP_ID}.desktop"),
                    "scripts/flatpak/io.github.jitingcn.SlimeNRFHelper.desktop",
                ),
                (
                    format!("{APP_ID}.svg"),
                    "scripts/appimage/slimenrf-helper.svg",
                ),
            ] {
                assert_eq!(
                    fs::read(stage.join(staged)).unwrap(),
                    fs::read(root.join(original)).unwrap()
                );
            }
            let firmware = source.join("flatpak-firmwares");
            assert_eq!(firmware.exists(), !no_firmware);
            if !no_firmware {
                let mut names = fs::read_dir(&firmware)
                    .unwrap()
                    .map(|entry| entry.unwrap().file_name().into_string().unwrap())
                    .collect::<Vec<_>>();
                names.sort();
                assert_eq!(names, vec!["test.HeX".to_owned(), "test.UF2".to_owned()]);
                assert_eq!(fs::read(firmware.join("test.UF2")).unwrap(), b"firmware\n");
            }
        }
    }

    #[test]
    fn flatpak_yaml_firmware_root_is_relative_to_selected_configuration() {
        let temporary = TempDir::new().unwrap();
        let root = temporary.path();
        let selected = put(root, "config/selected.yaml", b"firmware_root: payloads\n");
        assert_eq!(
            firmware_root(root, &selected, &fs::read(&selected).unwrap(), None).unwrap(),
            root.join("config/payloads")
        );
        assert_eq!(
            firmware_root(root, &selected, b"not: valid", Some(Path::new("override"))).unwrap(),
            root.join("override")
        );
        for text in ["yes", "on", "0123", "true", "null", "0x10"] {
            let yaml = format!("firmware_root: {text}\n");
            assert_eq!(
                firmware_root(root, &selected, yaml.as_bytes(), None).unwrap(),
                root.join("config").join(text)
            );
        }
    }

    #[test]
    fn selected_config_symlink_retains_its_original_parent_for_firmware() {
        let temporary = TempDir::new().unwrap();
        let root = temporary.path().join("workspace");
        fixture(&root);
        let actual = put(&root, "real/config.yaml", b"firmware_root: payloads\n");
        put(&root, "config/payloads/device.bin", b"chosen\n");
        put(&root, "real/payloads/device.bin", b"wrong\n");
        let selected = root.join("config/link.yaml");
        symlink(actual, &selected).unwrap();
        let stage = temporary.path().join("stage");
        fs::create_dir(&stage).unwrap();
        let source = stage_sources(&root, &stage, false, &selected, None).unwrap();
        assert_eq!(
            fs::read(source.join("flatpak-firmwares/device.bin")).unwrap(),
            b"chosen\n"
        );
    }

    #[test]
    fn rejects_bad_yaml_and_empty_firmware_but_no_firmware_never_parses_yaml() {
        let temporary = TempDir::new().unwrap();
        let root = temporary.path().join("workspace");
        fixture(&root);
        let config = put(&root, "config/selected.yaml", b"not: [valid\n");
        let stage = temporary.path().join("no-fw");
        fs::create_dir(&stage).unwrap();
        stage_sources(&root, &stage, true, &config, None).unwrap();
        for bad in [
            "[]",
            "other: value",
            "firmware_root: ''",
            "firmware_root: []",
            "firmware_root: a\nfirmware_root: b\n",
        ] {
            assert!(
                firmware_root(&root, &config, bad.as_bytes(), None).is_err(),
                "{bad}"
            );
        }
        put(&root, "firmware/nested/only.uf2", b"nested");
        let stage = temporary.path().join("full");
        fs::create_dir(&stage).unwrap();
        assert!(
            stage_sources(&root, &stage, false, &config, Some(Path::new("firmware")))
                .unwrap_err()
                .contains("no top-level")
        );
    }

    #[test]
    fn rejects_symlinked_source_files_and_directories_without_reading_target() {
        for directory in [false, true] {
            let temporary = TempDir::new().unwrap();
            let root = temporary.path().join("workspace");
            fixture(&root);
            let config = put(&root, "config.yaml", b"firmware_root: ignored\n");
            let outside = put(temporary.path(), "outside/secret", b"private");
            let source = if directory {
                root.join("crates/app/src/foreign")
            } else {
                root.join("crates/app/src/foreign.rs")
            };
            symlink(
                if directory {
                    outside.parent().unwrap()
                } else {
                    &outside
                },
                &source,
            )
            .unwrap();
            let stage = temporary.path().join("stage");
            fs::create_dir(&stage).unwrap();
            assert!(stage_sources(&root, &stage, true, &config, None).is_err());
            assert!(!stage.join("source/crates/app/src/foreign/secret").exists());
            assert!(!stage.join("source/crates/app/src/foreign.rs").exists());
            assert_eq!(fs::read(outside).unwrap(), b"private");
        }
    }

    #[test]
    fn refuses_symlinked_explicit_source_file_and_tree() {
        for source_name in ["LICENSE-MIT", "crates/app/build_support"] {
            let temporary = TempDir::new().unwrap();
            let root = temporary.path().join("workspace");
            fixture(&root);
            let config = put(&root, "config.yaml", b"firmware_root: ignored\n");
            let source = root.join(source_name);
            let outside = if source.is_dir() {
                fs::remove_dir_all(&source).unwrap();
                let path = temporary.path().join("outside");
                fs::create_dir(&path).unwrap();
                put(temporary.path(), "outside/secret", b"private");
                path
            } else {
                fs::remove_file(&source).unwrap();
                put(temporary.path(), "outside", b"private")
            };
            symlink(&outside, &source).unwrap();
            let stage = temporary.path().join("stage");
            fs::create_dir(&stage).unwrap();
            assert!(
                stage_sources(&root, &stage, true, &config, None).is_err(),
                "{source_name}"
            );
            assert!(
                !stage.join("source").join(source_name).exists(),
                "{source_name}"
            );
        }
    }

    #[test]
    fn refuses_fifo_source_without_waiting_for_a_writer() {
        let temporary = TempDir::new().unwrap();
        let root = temporary.path().join("workspace");
        fixture(&root);
        let config = put(&root, "config.yaml", b"firmware_root: ignored\n");
        let source = root.join("Cargo.lock");
        fs::remove_file(&source).unwrap();
        let name = CString::new(source.as_os_str().as_bytes()).unwrap();
        // SAFETY: name is a live, NUL-terminated pathname in this isolated temporary fixture.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let stage = temporary.path().join("stage");
        fs::create_dir(&stage).unwrap();
        assert!(stage_sources(&root, &stage, true, &config, None).is_err());
        assert!(!stage.join("source/Cargo.lock").exists());
    }

    #[test]
    fn build_cache_lock_rejects_symlink_without_modifying_target() {
        let temporary = TempDir::new().unwrap();
        let protected = put(temporary.path(), "protected", b"do not modify");
        let lock_path = temporary.path().join("build.lock");
        symlink(&protected, &lock_path).unwrap();
        assert!(lock_cache(&lock_path).is_err());
        assert_eq!(fs::read(&protected).unwrap(), b"do not modify");
        fs::remove_file(&lock_path).unwrap();
        let lock = lock_cache(&lock_path).unwrap();
        assert!(lock.metadata().unwrap().is_file());
    }

    #[test]
    fn release_metadata_read_is_bounded_and_requires_utf8() {
        let small = b"valid metadata";
        assert_eq!(read_release_manifest(&small[..]).unwrap(), "valid metadata");
        assert!(read_release_manifest(&b"\xff"[..]).is_err());
        let oversized = vec![b'x'; MANIFEST_LIMIT as usize + 1];
        assert!(
            read_release_manifest(&oversized[..])
                .unwrap_err()
                .contains("4 MiB")
        );
    }

    fn release_manifest(target: &str) -> String {
        let mut document = String::from("manifest-version = '2'\ndate = '2026-01-02'\n");
        for component in ["rustc", "cargo", "rust-std"] {
            document.push_str(&format!("[pkg.{component}.target.{target}]\navailable = true\nxz_url = 'https://static.rust-lang.org/dist/2026-01-02/{component}-1.99.1-{target}.tar.xz'\nxz_hash = '{}'\n", "a".repeat(64)));
        }
        document
    }

    #[test]
    fn compiler_pin_and_release_metadata_are_strictly_validated() {
        let pin = parse_rust_pin(b"[toolchain]\nchannel = '1.99.1'\n").unwrap();
        assert_eq!(pin, "1.99.1");
        for invalid in ["stable", "nightly", "01.2.3", "1.2", "1.2.3-beta"] {
            let pin = format!("[toolchain]\nchannel = '{invalid}'\n");
            assert!(parse_rust_pin(pin.as_bytes()).is_err(), "{invalid}");
        }
        let oversized = vec![b' '; 65_537];
        assert!(parse_rust_pin(&oversized).is_err());
        let release = release_manifest("x86_64-unknown-linux-gnu");
        let module = build_toolchain_module(&pin, "x86_64", &release).unwrap();
        assert_eq!(module["sources"].as_array().unwrap().len(), 3);
        assert_eq!(module["build-options"]["no-debuginfo"], true);
        assert_eq!(module["cleanup"][0], "/lib/slimenrf-build-rust");
        assert_eq!(
            module["build-commands"][3],
            "test \"$(/app/lib/slimenrf-build-rust/bin/rustc --version | cut -d \" \" -f 2)\" = \"1.99.1\""
        );
        let arm_release = release_manifest("aarch64-unknown-linux-gnu");
        let arm = build_toolchain_module(&pin, "aarch64", &arm_release).unwrap();
        assert_eq!(
            arm["sources"][0]["url"],
            "https://static.rust-lang.org/dist/2026-01-02/rustc-1.99.1-aarch64-unknown-linux-gnu.tar.xz"
        );
        for bad in [
            release.replace("manifest-version = '2'", "manifest-version = '1'"),
            release.replace("2026-01-02", "2026-1-2"),
            release.replace("available = true", "available = false"),
            release.replacen("https://static.rust-lang.org", "https://example.invalid", 1),
            release.replacen(&"a".repeat(64), &"z".repeat(64), 1),
        ] {
            assert!(build_toolchain_module(&pin, "x86_64", &bad).is_err());
        }
        assert!(build_toolchain_module(&pin, "arm", &release).is_err());
    }

    #[test]
    fn manifest_preserves_offline_sdk_build_and_variant_selection() {
        let template: serde_json::Value = serde_json::json!({"modules": [{
            "name": "slimenrf-helper", "build-options": {"env": {"CARGO_NET_OFFLINE": "true"}},
            "build-commands": ["cargo build --offline --locked --release --package slimenrf-helper"]
        }]});
        let toolchain =
            serde_json::json!({"name": "rust-toolchain", "cleanup": ["/lib/slimenrf-build-rust"]});
        for no_firmware in [false, true] {
            let manifest = configure_manifest(
                template.clone(),
                toolchain.clone(),
                Path::new("/cache/sdk-target"),
                "2",
                no_firmware,
            )
            .unwrap();
            assert_eq!(manifest["modules"][0], toolchain);
            let app = &manifest["modules"][1];
            assert_eq!(app["build-options"]["env"]["CARGO_NET_OFFLINE"], "true");
            assert_eq!(
                app["build-options"]["env"]["CARGO_TARGET_DIR"],
                "/cache/sdk-target"
            );
            assert_eq!(app["build-options"]["env"]["CARGO_BUILD_JOBS"], "2");
            assert_eq!(
                app["build-options"]["build-args"],
                serde_json::json!(["--filesystem=/cache/sdk-target", "--unshare=network"])
            );
            assert_eq!(
                app["build-commands"][0]
                    .as_str()
                    .unwrap()
                    .ends_with(" --no-default-features"),
                no_firmware
            );
        }
    }
}
