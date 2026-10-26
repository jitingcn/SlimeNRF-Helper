use super::identities::{valid_board_token, valid_firmware_alias};
use super::{BleOtaTarget, Catalog, FlashPreset, FlashTarget, OpenDfuTarget, valid_filename};
use crate::{bundled_resources::Resource, remote_config::RemoteConfig};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::{Arc, atomic::AtomicBool},
};

mod bounded_yaml;

#[derive(Deserialize)]
#[serde(untagged)]
enum Labels {
    One(String),
    Many(Vec<String>),
}
#[derive(Deserialize, Default)]
struct RawCatalog {
    schema_version: u32,
    firmware_root: String,
    #[serde(default)]
    remote_firmware: RemoteConfig,
    poll_interval_seconds: Option<f64>,
    info_filename: Option<String>,
    board_volume_labels: Option<Labels>,
    #[serde(default)]
    legacy_targets: BTreeMap<String, RawPreset>,
    #[serde(default)]
    board_targets: BTreeMap<String, RawPreset>,
    #[serde(default)]
    application_targets: BTreeMap<String, RawPreset>,
    #[serde(default)]
    opendfu_targets: Vec<OpenDfuTarget>,
}
#[derive(Deserialize)]
struct RawPreset {
    #[serde(flatten)]
    target: FlashTarget,
    bootloader: Option<Resource>,
    bootloader_board: Option<String>,
    bootloader_mcu: Option<crate::firmware::Nrf52Chip>,
    success: Option<String>,
    targets: Option<Vec<FlashTarget>>,
}
fn resolve_resource(resource: &mut Option<Resource>, base: Option<&Path>) -> Result<(), String> {
    if resource.as_ref().is_some_and(Resource::is_empty) {
        *resource = None;
    }
    if let Some(resource) = resource {
        resource.resolve(base)?;
    }
    Ok(())
}
fn preset(key: String, raw: RawPreset, base: Option<&Path>) -> Result<FlashPreset, String> {
    let display_name = raw
        .target
        .name
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| key.clone());
    let bootloader_board = raw.bootloader_board;
    if bootloader_board
        .as_deref()
        .is_some_and(|board| !valid_board_token(board))
    {
        return Err(format!(
            "Preset {key} has an invalid bootloader_board token"
        ));
    }
    if bootloader_board.is_some() && raw.bootloader_mcu.is_none() {
        return Err(format!(
            "Preset {key} requires bootloader_mcu with bootloader_board"
        ));
    }
    let bootloader_only = (bootloader_board.is_some() || raw.bootloader.is_some())
        && raw.target.firmware.is_none()
        && raw.target.ota_firmware.is_none()
        && raw.target.esb_ota_firmware.is_none()
        && raw.target.esb_ota_mcuboot_firmware.is_none()
        && raw.target.firmware_aliases.is_empty()
        && raw.target.esb_ota_kind.is_none();
    let mut targets = raw.targets.unwrap_or_else(|| {
        if bootloader_only {
            Vec::new()
        } else {
            vec![raw.target]
        }
    });
    let mut bootloader = raw.bootloader;
    resolve_resource(&mut bootloader, base)?;
    if targets.is_empty() && bootloader.is_none() && bootloader_board.is_none() {
        return Err(format!(
            "Preset {key} has no targets or bootloader identity"
        ));
    }
    let multiple = targets.len() > 1;
    let mut names = BTreeSet::new();
    for target in &mut targets {
        target.name = target
            .name
            .take()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty());
        if multiple && (target.name.is_none() || !names.insert(target.name.clone())) {
            return Err(format!(
                "Preset {key} requires unique nonempty names for all variants"
            ));
        }
        if target
            .firmware_aliases
            .iter()
            .any(|alias| !valid_firmware_alias(alias))
        {
            return Err(format!(
                "Preset {key} contains an invalid firmware_aliases entry"
            ));
        }
        target.firmware_aliases.sort();
        target.firmware_aliases.dedup();
        resolve_resource(&mut target.firmware, base)?;
        resolve_resource(&mut target.ota_firmware, base)?;
        resolve_resource(&mut target.esb_ota_firmware, base)?;
        resolve_resource(&mut target.esb_ota_mcuboot_firmware, base)?;
    }
    Ok(FlashPreset {
        key,
        display_name,
        bootloader,
        bootloader_board,
        bootloader_mcu: raw.bootloader_mcu,
        additional_bootloaders: Vec::new(),
        success: raw.success,
        targets,
    })
}

impl Catalog {
    pub fn load(path: &Path) -> Result<Self, String> {
        let content = Resource::File(path.to_owned())
            .read(
                bounded_yaml::MAX_INPUT_BYTES as u64,
                &AtomicBool::new(false),
            )
            .map_err(|e| format!("Cannot read configuration {}: {e}", path.display()))?;
        let base = path.parent().unwrap_or_else(|| Path::new("."));
        Self::from_bytes(&content.bytes, base)
    }
    pub(super) fn from_bytes(bytes: &[u8], base: &Path) -> Result<Self, String> {
        Self::from_resource_bytes(bytes, Some(base))
    }
    pub fn load_embedded() -> Result<Self, String> {
        let content = Resource::bundled("slimenrf-helper.yaml")?.read(
            bounded_yaml::MAX_INPUT_BYTES as u64,
            &AtomicBool::new(false),
        )?;
        Self::from_resource_bytes(&content.bytes, None)
    }
    pub(super) fn from_resource_bytes(bytes: &[u8], base: Option<&Path>) -> Result<Self, String> {
        // The project configuration accepts YAML and its JSON subset.
        let raw: RawCatalog = bounded_yaml::from_slice(bytes)
            .map_err(|e| format!("Invalid board configuration: {e}"))?;
        if raw.schema_version != 1 {
            return Err("schema_version must be 1".into());
        }
        if raw.firmware_root.trim().is_empty() {
            return Err("firmware_root must be nonempty".into());
        }
        raw.remote_firmware.validate()?;
        let firmware_root = base.map(|base| base.join(&raw.firmware_root));
        let base = firmware_root.as_deref();
        let mut catalog = Self {
            remote_firmware: Arc::new(raw.remote_firmware),
            ..Self::default()
        };
        if let Some(interval) = raw.poll_interval_seconds {
            catalog.poll_interval_seconds = interval;
        }
        if !catalog.poll_interval_seconds.is_finite() || catalog.poll_interval_seconds <= 0. {
            return Err("poll_interval_seconds must be positive and finite".into());
        }
        if let Some(filename) = raw.info_filename {
            catalog.info_filename = filename;
        }
        if !valid_filename(&catalog.info_filename) {
            return Err("info_filename must be a single filename".into());
        }
        if let Some(labels) = raw.board_volume_labels {
            catalog.board_volume_labels = match labels {
                Labels::One(label) => vec![label],
                Labels::Many(labels) => labels,
            };
        }
        if catalog.board_volume_labels.is_empty()
            || catalog
                .board_volume_labels
                .iter()
                .any(|label| label.is_empty())
        {
            return Err("board_volume_labels must contain nonempty labels".into());
        }
        for (key, raw) in raw.legacy_targets {
            catalog
                .legacy_targets
                .insert(key.clone(), preset(key, raw, base)?);
        }
        for (key, raw) in raw.board_targets {
            catalog
                .board_targets
                .insert(key.clone(), preset(key, raw, base)?);
        }
        for (key, raw) in raw.application_targets {
            catalog
                .application_targets
                .insert(key.clone(), preset(key, raw, base)?);
        }
        let mut bootloader_chips = BTreeMap::new();
        for preset in catalog.presets() {
            if let (Some(board), Some(chip)) = (&preset.bootloader_board, preset.bootloader_mcu)
                && bootloader_chips
                    .insert(board.to_ascii_lowercase(), chip)
                    .is_some_and(|previous| previous != chip)
            {
                return Err(format!(
                    "Conflicting bootloader_mcu for bootloader_board {board}"
                ));
            }
        }
        catalog.opendfu_targets = raw.opendfu_targets;
        for target in &mut catalog.opendfu_targets {
            if target.name.trim().is_empty() || target.firmware.is_empty() {
                return Err("OpenDFU requires a target name and firmware path".into());
            }
            target.firmware.resolve(base)?;
        }
        let mut seen = BTreeSet::new();
        let mut ble = Vec::new();
        for preset in catalog.presets() {
            for target in &preset.targets {
                if let Some(package) = &target.ota_firmware {
                    let name = target
                        .name
                        .clone()
                        .unwrap_or_else(|| preset.display_name.clone());
                    if seen.insert(name.clone()) {
                        ble.push(BleOtaTarget {
                            package: package.clone(),
                            display_name: name,
                            success: preset.success.clone(),
                        });
                    }
                }
            }
        }
        catalog.ble_ota_targets = ble;
        catalog.cache_firmware_candidates(base)?;
        Ok(catalog)
    }
    pub fn load_default() -> Result<Self, String> {
        Self::load_default_in(
            std::env::current_exe().ok().as_deref(),
            std::env::current_dir().ok().as_deref(),
        )
    }
    pub(super) fn load_default_in(exe: Option<&Path>, cwd: Option<&Path>) -> Result<Self, String> {
        let mut paths = Vec::new();
        if let Some(parent) = exe.and_then(Path::parent) {
            paths.push(parent.join("slimenrf-helper.yaml"));
        }
        if let Some(cwd) = cwd {
            paths.push(cwd.join("slimenrf-helper.yaml"));
        }
        for path in paths {
            if path.is_file() {
                return Self::load(&path);
            }
        }
        Self::load_embedded()
    }
}
