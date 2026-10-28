use crate::{bundled_resources::Resource, remote_config::RemoteConfig};
use serde::Deserialize;
use std::{collections::BTreeMap, sync::Arc};

mod filename_hints;
mod identities;
mod inventory;
mod loading;
pub use filename_hints::filename_variant;
pub(crate) use identities::bootloader_board;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum EsbOtaKind {
    Tracker,
    Receiver,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct FlashTarget {
    pub name: Option<String>,
    pub firmware: Option<Resource>,
    #[serde(default)]
    pub firmware_aliases: Vec<String>,
    #[serde(skip)]
    pub additional_firmware: Vec<Resource>,
    pub ota_firmware: Option<Resource>,
    pub esb_ota_kind: Option<EsbOtaKind>,
    pub esb_ota_firmware: Option<Resource>,
    pub esb_ota_mcuboot_firmware: Option<Resource>,
}
impl FlashTarget {
    pub fn image_for_bootloader(&self, bootloader: &str) -> Result<&Resource, String> {
        let path = match bootloader {
            "mcuboot" => self.esb_ota_mcuboot_firmware.as_ref(),
            "nrf5_opendfu" => {
                return Err(
                    "OpenDFU targets require serial HEX/Secure DFU; ESB OTA is not supported"
                        .into(),
                );
            }
            "adafruit_uf2" | "none" => self.esb_ota_firmware.as_ref().or(self.firmware.as_ref()),
            _ => return Err(format!("Unknown device bootloader: {bootloader}")),
        };
        path.ok_or_else(|| format!("No firmware configured for bootloader {bootloader}"))
    }
}

#[derive(Clone, Debug)]
pub struct FlashPreset {
    pub key: String,
    pub display_name: String,
    pub bootloader: Option<Resource>,
    pub bootloader_board: Option<String>,
    pub bootloader_mcu: Option<slimenrf_firmware_image::image::Nrf52Chip>,
    pub additional_bootloaders: Vec<Resource>,
    pub success: Option<String>,
    pub targets: Vec<FlashTarget>,
}

impl FlashPreset {
    /// Exact defaults and precomputed alternatives; iteration never performs I/O.
    pub fn firmware_candidates(&self) -> impl Iterator<Item = (usize, &Resource, &str)> {
        self.targets
            .iter()
            .enumerate()
            .flat_map(move |(index, target)| {
                target
                    .firmware
                    .iter()
                    .chain(&target.additional_firmware)
                    .map(move |resource| {
                        (
                            index,
                            resource,
                            target.name.as_deref().unwrap_or(&self.display_name),
                        )
                    })
            })
            .chain(
                self.bootloader
                    .iter()
                    .chain(&self.additional_bootloaders)
                    .map(move |resource| {
                        (self.targets.len(), resource, self.display_name.as_str())
                    }),
            )
    }
}

fn hw_default() -> u32 {
    52
}
fn app_default() -> u32 {
    1
}
fn sd_default() -> String {
    "0x00".into()
}
#[derive(Clone, Debug, Deserialize)]
pub struct OpenDfuTarget {
    pub name: String,
    pub firmware: Resource,
    #[serde(default = "hw_default")]
    pub hw_version: u32,
    #[serde(default = "app_default")]
    pub app_version: u32,
    #[serde(default = "sd_default")]
    pub sd_req: String,
    pub success: Option<String>,
}
#[derive(Clone, Debug)]
pub struct BleOtaTarget {
    pub package: Resource,
    pub display_name: String,
    pub success: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FirmwareHint {
    pub board_target: String,
    pub kind: Option<EsbOtaKind>,
    pub variant: String,
    pub mcuboot: bool,
    pub bootloader: bool,
}
#[derive(Clone, Debug)]
pub struct Catalog {
    pub poll_interval_seconds: f64,
    pub info_filename: String,
    pub board_volume_labels: Vec<String>,
    pub legacy_targets: BTreeMap<String, FlashPreset>,
    pub board_targets: BTreeMap<String, FlashPreset>,
    pub application_targets: BTreeMap<String, FlashPreset>,
    pub opendfu_targets: Vec<OpenDfuTarget>,
    pub ble_ota_targets: Vec<BleOtaTarget>,
    pub remote_firmware: Arc<RemoteConfig>,
}
impl Default for Catalog {
    fn default() -> Self {
        Self {
            poll_interval_seconds: 2.,
            info_filename: "INFO_UF2.TXT".into(),
            board_volume_labels: vec!["SLIMENRF".into()],
            legacy_targets: BTreeMap::new(),
            board_targets: BTreeMap::new(),
            application_targets: BTreeMap::new(),
            opendfu_targets: Vec::new(),
            ble_ota_targets: Vec::new(),
            remote_firmware: Arc::new(RemoteConfig::default()),
        }
    }
}
fn normalized(value: &str) -> String {
    value.trim().to_lowercase().replace('\\', "/")
}

impl Catalog {
    pub fn presets(&self) -> impl Iterator<Item = &FlashPreset> {
        self.legacy_targets
            .values()
            .chain(self.board_targets.values())
            .chain(self.application_targets.values())
    }
    pub fn preset_for_volume(&self, label: &str, board_id: Option<&str>) -> Option<&FlashPreset> {
        if let Some(board_id) = board_id {
            // Positive Board-ID metadata always outranks legacy volume labels.
            // An unrecognized ID must never select a different board by label.
            return self.board_targets.get(board_id);
        }
        self.legacy_targets.get(label)
    }
    pub fn ota_target(&self, kind: EsbOtaKind, board: &str) -> Result<&FlashTarget, String> {
        let board = board.trim();
        if board.is_empty() {
            return Err("Device did not report a board target".into());
        }
        let usable = |target: &&FlashTarget| {
            target.esb_ota_kind == Some(kind)
                && (target.firmware.is_some()
                    || target.esb_ota_firmware.is_some()
                    || !target.additional_firmware.is_empty()
                    || target.esb_ota_mcuboot_firmware.is_some())
        };
        // Exact target names always win over preset display/volume aliases.
        for preset in self.presets() {
            if let Some(target) = preset
                .targets
                .iter()
                .filter(usable)
                .find(|target| target.name.as_deref().unwrap_or(&preset.key) == board)
            {
                return Ok(target);
            }
        }
        let normalized_board = canonical_board(board);
        let mut matches = Vec::new();
        for preset in self.presets() {
            for target in preset.targets.iter().filter(usable) {
                if canonical_board(target.name.as_deref().unwrap_or(&preset.key))
                    == normalized_board
                    || normalized(&preset.key) == normalized_board
                    || normalized(&preset.display_name) == normalized_board
                {
                    matches.push(target);
                }
            }
        }
        match matches.as_slice() {
            [target] => Ok(target),
            [] => Err(format!("No {kind:?} firmware configured for board {board}")),
            _ => Err(format!(
                "Board alias {board} is ambiguous; use the device's exact board target"
            )),
        }
    }
}

/// Explicit historical configuration aliases, not a fuzzy hardware guess.
pub fn canonical_board(board: &str) -> String {
    let value = normalized(board);
    match value.as_str() {
        "holyiot-21017" => "holyiot_21017/nrf52840".into(),
        "xiao_ble/nrf52840-receiver" => "xiao_ble/nrf52840".into(),
        _ => value,
    }
}

pub fn valid_filename(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\', ':', '\0'])
}

#[cfg(test)]
mod tests;
