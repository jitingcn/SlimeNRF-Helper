use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    error::Error,
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
};

use serde_yaml::Value;
use sha2::{Digest, Sha256};
use zip::{CompressionMethod, DateTime, ZipWriter, write::SimpleFileOptions};

type BuildResult<T> = Result<T, Box<dyn Error>>;

pub fn build_resource_bundle() -> BuildResult<()> {
    println!("cargo:rerun-if-env-changed=SLIMENRF_CONFIG_PATH");
    let embed_firmware = env::var_os("CARGO_FEATURE_EMBED_FIRMWARE").is_some();
    let root =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").ok_or("missing CARGO_MANIFEST_DIR")?)
            .join("../..");
    let config_path = env::var_os("SLIMENRF_CONFIG_PATH")
        .map(|path| root.join(path))
        .unwrap_or_else(|| root.join("slimenrf-helper.yaml"));
    println!("cargo:rerun-if-changed={}", config_path.display());
    let mut config: Value = serde_yaml::from_slice(&fs::read(&config_path)?)?;
    if config["schema_version"].as_u64() != Some(1) {
        return Err("schema_version must be 1".into());
    }
    let configured_root = config["firmware_root"]
        .as_str()
        .filter(|root| !root.trim().is_empty())
        .ok_or("firmware_root must be nonempty")?;
    let firmware_dir = if embed_firmware {
        println!("cargo:rerun-if-env-changed=SLIMENRF_FIRMWARE_ROOT");
        Some(match env::var_os("SLIMENRF_FIRMWARE_ROOT") {
            Some(firmware_root) => root.join(firmware_root),
            None => config_path
                .parent()
                .ok_or("configuration has no parent")?
                .join(configured_root),
        })
    } else {
        None
    };
    let mut configured = BTreeSet::new();
    for section in [
        "legacy_targets",
        "board_targets",
        "application_targets",
        "opendfu_targets",
    ] {
        if let Some(value) = config.get_mut(section) {
            collect_configured_members(value, &mut configured)?;
        }
    }
    let config_bytes = serde_yaml::to_string(&config)?.into_bytes();

    let mut files = BTreeMap::new();
    let mut missing = BTreeMap::new();
    if let Some(firmware_dir) = firmware_dir {
        println!("cargo:rerun-if-changed={}", firmware_dir.display());
        if !firmware_dir.is_dir() {
            return Err(format!(
                "{} is not a firmware directory; restore it or set SLIMENRF_FIRMWARE_ROOT",
                firmware_dir.display()
            )
            .into());
        }
        for entry in fs::read_dir(&firmware_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let path = entry.path();
            let Some(extension) = path.extension().and_then(|extension| extension.to_str()) else {
                continue;
            };
            if !["uf2", "hex", "bin", "zip"]
                .iter()
                .any(|allowed| extension.eq_ignore_ascii_case(allowed))
            {
                continue;
            }
            let filename = entry
                .file_name()
                .into_string()
                .map_err(|_| "firmware filename is not valid UTF-8")?;
            let name = format!("firmwares/{filename}");
            normalize_member_name(&name)?;
            println!("cargo:rerun-if-changed={}", path.display());
            files.insert(name, path);
        }
        if files.is_empty() {
            return Err(format!(
                "{} contains no top-level UF2/HEX/BIN/ZIP firmware files; restore those inputs or set \
                 SLIMENRF_FIRMWARE_ROOT to a populated firmware directory",
                firmware_dir.display()
            )
            .into());
        }

        let mut actual_names = BTreeMap::new();
        actual_names.insert("slimenrf-helper.yaml".to_owned(), "slimenrf-helper.yaml");
        for name in files.keys() {
            if let Some(previous) = actual_names.insert(name.to_ascii_lowercase(), name.as_str()) {
                return Err(format!(
                    "case-insensitive bundle name collision: {previous:?} and {name:?}"
                )
                .into());
            }
        }
        for name in configured {
            let folded = name.to_ascii_lowercase();
            if !actual_names.contains_key(&folded) {
                missing.entry(folded).or_insert(name);
            }
        }
        if !missing.is_empty() {
            println!(
                "cargo:warning={} configured firmware resources are unavailable in the embedded bundle:",
                missing.len()
            );
            for name in missing.values() {
                println!("cargo:warning=Missing configured resource: {name}");
            }
        }
    }

    let output = PathBuf::from(env::var_os("OUT_DIR").ok_or("missing OUT_DIR")?);
    let archive_path = output.join("firmware_bundle.zip");
    let mut archive = ZipWriter::new(File::create(&archive_path)?);
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .compression_level(Some(6))
        .last_modified_time(DateTime::default())
        .unix_permissions(0o644);
    let mut index = File::create(output.join("firmware_bundle_index.rs"))?;
    writeln!(index, "pub static MEMBERS: &[Member] = &[")?;
    let mut raw_bytes = append_member(
        &mut archive,
        &mut index,
        options,
        "slimenrf-helper.yaml",
        config_bytes.as_slice(),
    )?;
    for (name, path) in &files {
        raw_bytes += append_member(&mut archive, &mut index, options, name, File::open(path)?)?;
    }
    writeln!(index, "];")?;
    writeln!(index, "pub static MISSING_CONFIGURED_MEMBERS: &[&str] = &[")?;
    for name in missing.values() {
        writeln!(index, "    {name:?},")?;
    }
    writeln!(index, "];")?;
    let archive = archive.finish()?;
    println!(
        "cargo:warning=Embedded firmware bundle: {} compressed ZIP bytes, {raw_bytes} raw bytes, {} firmware files plus slimenrf-helper.yaml",
        archive.metadata()?.len(),
        files.len()
    );
    Ok(())
}

fn append_member(
    archive: &mut ZipWriter<File>,
    index: &mut File,
    options: SimpleFileOptions,
    name: &str,
    mut input: impl Read,
) -> BuildResult<u64> {
    archive.start_file(name, options)?;
    let mut hash = Sha256::new();
    let mut len = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        archive.write_all(&buffer[..count])?;
        hash.update(&buffer[..count]);
        len += count as u64;
    }
    let sha256: [u8; 32] = hash.finalize().into();
    writeln!(
        index,
        "    Member {{ name: {name:?}, len: {len}, sha256: {sha256:?} }},"
    )?;
    Ok(len)
}

fn collect_configured_members(
    value: &mut Value,
    members: &mut BTreeSet<String>,
) -> BuildResult<()> {
    match value {
        Value::Mapping(mapping) => {
            for (key, value) in mapping {
                if matches!(
                    key.as_str(),
                    Some(
                        "firmware"
                            | "ota_firmware"
                            | "esb_ota_firmware"
                            | "esb_ota_mcuboot_firmware"
                            | "bootloader"
                    )
                ) {
                    match value {
                        Value::Null => {}
                        Value::String(path) if path.is_empty() => {}
                        Value::String(path) => {
                            if path.is_empty()
                                || matches!(path.as_str(), "." | "..")
                                || path.contains(['/', '\\', ':'])
                                || path.chars().any(char::is_control)
                            {
                                return Err(format!("configured resource {path:?} must be a root-relative single filename").into());
                            }
                            *path = normalize_member_name(&format!("firmwares/{path}"))?;
                            members.insert(path.clone());
                        }
                        _ => {
                            return Err(format!(
                                "configured resource {key:?} must be a string or null"
                            )
                            .into());
                        }
                    }
                }
                collect_configured_members(value, members)?;
            }
        }
        Value::Sequence(values) => {
            for value in values {
                collect_configured_members(value, members)?;
            }
        }
        Value::Tagged(tagged) => collect_configured_members(&mut tagged.value, members)?,
        _ => {}
    }
    Ok(())
}

fn normalize_member_name(path: &str) -> BuildResult<String> {
    let slashed = path.replace('\\', "/");
    let name = slashed.strip_prefix("./").unwrap_or(&slashed);
    let firmware_filename = name.split_once('/').and_then(|(directory, filename)| {
        (directory.eq_ignore_ascii_case("firmwares")
            && !matches!(filename, "" | "." | "..")
            && !filename.contains('/'))
        .then_some(filename)
    });
    if name.chars().any(|character| character.is_control())
        || name.contains(':')
        || !(name.eq_ignore_ascii_case("slimenrf-helper.yaml") || firmware_filename.is_some())
        || Path::new(name).is_absolute()
    {
        return Err(format!(
            "invalid bundled resource path {path:?}; expected slimenrf-helper.yaml or firmwares/<single filename>"
        ).into());
    }
    if let Some(filename) = firmware_filename {
        Ok(format!("firmwares/{filename}"))
    } else {
        Ok("slimenrf-helper.yaml".to_owned())
    }
}
