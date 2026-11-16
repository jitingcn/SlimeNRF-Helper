use super::identities::{
    copy_stems, matches_application_stem, matches_bootloader, matches_firmware_alias,
};
use super::{Catalog, EsbOtaKind, FirmwareHint, FlashPreset, canonical_board};
use std::collections::BTreeSet;

impl Catalog {
    pub fn match_filename(&self, name: &str) -> Vec<FirmwareHint> {
        let filename = name.rsplit(['/', '\\']).next().unwrap_or(name);
        let variant = filename_variant(filename);
        let mut hints = Vec::new();
        for preset in self.presets() {
            if matches_bootloader(preset, filename)
                || preset.bootloader.as_ref().is_some_and(|resource| {
                    resource
                        .filename()
                        .is_some_and(|pin| matches_exact_pin(pin, filename))
                })
            {
                append_bootloader_hints(preset, &mut hints);
                // Separately configured applications can be ambiguous with a custom pin.
            }
            for target in &preset.targets {
                let board_target = canonical_board(target.name.as_deref().unwrap_or(&preset.key));
                for (path, mcuboot) in [
                    (target.firmware.as_ref(), false),
                    (target.esb_ota_firmware.as_ref(), false),
                    (target.ota_firmware.as_ref(), false),
                    (target.esb_ota_mcuboot_firmware.as_ref(), true),
                ] {
                    if path.is_some_and(|resource| {
                        resource
                            .filename()
                            .is_some_and(|pin| matches_resource_pin(pin, filename, variant))
                    }) {
                        let bootloader = variant == "Bootloader";
                        let hint = FirmwareHint {
                            board_target: board_target.clone(),
                            kind: if bootloader {
                                None
                            } else {
                                target.esb_ota_kind
                            },
                            variant: variant.into(),
                            mcuboot: mcuboot && !bootloader,
                            bootloader,
                        };
                        if !hints.contains(&hint) {
                            hints.push(hint);
                        }
                    }
                }
                if target
                    .firmware_aliases
                    .iter()
                    .any(|alias| matches_firmware_alias(alias, filename))
                {
                    let hint = FirmwareHint {
                        board_target,
                        kind: target.esb_ota_kind,
                        variant: variant.into(),
                        mcuboot: filename.to_ascii_lowercase().ends_with(".bin"),
                        bootloader: false,
                    };
                    if !hints.contains(&hint) {
                        hints.push(hint);
                    }
                }
            }
        }
        for target in &self.opendfu_targets {
            if matches_resource_pin(target.firmware.filename().unwrap_or(""), filename, variant) {
                hints.push(FirmwareHint {
                    board_target: canonical_board(&target.name),
                    kind: if variant == "Bootloader" {
                        None
                    } else {
                        Some(EsbOtaKind::Receiver)
                    },
                    variant: variant.into(),
                    mcuboot: false,
                    bootloader: variant == "Bootloader",
                });
            }
        }
        let mut upstream = upstream_hints(filename, variant);
        upstream.retain(|hint| {
            let mut declared = false;
            for target in self
                .presets()
                .flat_map(|preset| preset.targets.iter().map(move |target| (preset, target)))
            {
                let (preset, target) = target;
                if !target.firmware_aliases.is_empty()
                    && canonical_board(target.name.as_deref().unwrap_or(&preset.key))
                        == hint.board_target
                    && (target.esb_ota_kind.is_none() || target.esb_ota_kind == hint.kind)
                {
                    declared = true;
                    if target
                        .firmware_aliases
                        .iter()
                        .any(|alias| matches_firmware_alias(alias, filename))
                    {
                        return true;
                    }
                }
            }
            // Explicit aliases own version/container compatibility for this
            // board and role. Upstream fallback cannot broaden that declaration.
            !declared
        });
        if hints.is_empty() {
            return upstream;
        }
        for hint in &mut hints {
            if hint.kind.is_none() && !hint.bootloader {
                let matches: Vec<_> = upstream
                    .iter()
                    .filter(|known| known.board_target == hint.board_target)
                    .collect();
                if let [known] = matches.as_slice() {
                    hint.kind = known.kind;
                }
            }
        }
        hints
    }

    /// Matches a CI container's exact configured board token, not an image filename.
    pub(crate) fn match_bootloader_board(&self, board: &str) -> Vec<FirmwareHint> {
        let mut hints = Vec::new();
        for preset in self.presets() {
            if preset.bootloader_board.as_deref() == Some(board) {
                append_bootloader_hints(preset, &mut hints);
            }
        }
        hints
    }
}

fn append_bootloader_hints(preset: &FlashPreset, hints: &mut Vec<FirmwareHint>) {
    let boards = preset
        .targets
        .iter()
        .map(|target| target.name.as_deref().unwrap_or(&preset.key))
        .chain(preset.targets.is_empty().then_some(preset.key.as_str()));
    for board_target in boards {
        let hint = FirmwareHint {
            board_target: canonical_board(board_target),
            kind: None,
            variant: "Bootloader".into(),
            mcuboot: false,
            bootloader: true,
        };
        if !hints.contains(&hint) {
            hints.push(hint);
        }
    }
}

// Pinned host-side filename hints from slimenrf-ota-web@51bf6e01/js/boardmap.js.
// These are not trusted device metadata; the caller must compare queried role,
// bootloader and application base before any OTA operation.
// Entries are now complete stems, optionally prefixed with the known SlimeNRF_
// product prefix. No unconsumed prefix/suffix or longest-substring guessing.
const TRACKER_PATTERNS: &[(&str, &[&str])] = &[
    (
        "styria_mini_uf2/nrf52840/spi",
        &["styria_mini_tracker", "styria_mini_zephyr_spi"],
    ),
    (
        "promicro_uf2/nrf52840/spi",
        &[
            "stackedsmol_tracker_spi",
            "promicro_stackedsmol_tracker_spi",
            "tracker_mag_spi_stackedsmol_jitingcat",
            "tracker_mag_spi_stackedsmol",
            "mag_spi_stackedsmol_jitingcat",
            "mag_spi_stackedsmol",
            "tracker_spi_stackedsmol",
        ],
    ),
    (
        "promicro_uf2/nrf52840/i2c",
        &[
            "stackedsmol_tracker_i2c",
            "promicro_stackedsmol_tracker_i2c",
            "tracker_mag_i2c_stackedsmol_jitingcat",
            "tracker_mag_i2c_stackedsmol",
            "mag_i2c_stackedsmol_jitingcat",
            "mag_i2c_stackedsmol",
            "tracker_i2c_stackedsmol",
        ],
    ),
    (
        "promicro_uf2/nrf52840/default_spi",
        &[
            "promicro_tracker_spi",
            "tracker_mag_spi_promicro_jitingcat",
            "tracker_mag_spi_promicro",
            "mag_spi_promicro_jitingcat",
            "mag_spi_promicro",
            "tracker_defaultspi",
        ],
    ),
    (
        "promicro_uf2/nrf52840/default_i2c",
        &[
            "promicro_tracker_i2c",
            "tracker_mag_i2c_promicro_jitingcat",
            "tracker_mag_i2c_promicro",
            "mag_i2c_promicro_jitingcat",
            "mag_i2c_promicro",
            "tracker_defaulti2c",
        ],
    ),
    (
        "promicro_uf2/nrf52840/chrysalis",
        &[
            "chrysalis_tracker",
            "promicro_chrysalis_tracker",
            "tracker_mag_chrysalis_promicro_jitingcat",
            "mag_chrysalis_promicro_jitingcat",
            "chrysalis",
        ],
    ),
    (
        "promicro_uf2/nrf52840/lwkj_icm45v1",
        &["lwkj_icm45v1", "promicro_stackedsmol_tracker_lwkj_icm45v1"],
    ),
    (
        "promicro_uf2/nrf52840/bao",
        &["tracker_bao_jitingcat", "tracker_bao"],
    ),
    (
        "promicro_uf2/nrf52840/smspi",
        &["smspi", "promicro_stackedsmol_tracker_smspi"],
    ),
    (
        "foxsnacklite_uf2/nrf52840",
        &[
            "foxsnacklite_tracker",
            "foxsnacklite",
            "foxlite_tracker",
            "foxapplication_foxsnacklite_tracker",
        ],
    ),
    (
        "mochi_uf2/nrf52833",
        &["mochi_tracker", "tracker_mag_mochi_jitingcat"],
    ),
    ("aero_tracker_uf2/nrf52840", &["aero_tracker"]),
    ("aero_pro_uf2/nrf52840", &["aero_tracker_pro"]),
    (
        "xiao_nrf54lm20a/nrf54lm20a/cpuapp/sense",
        &["slimenrf_xiao_nrf54lm20a_sense_tracker"],
    ),
    (
        "xiao_nrf54lm20a/nrf54lm20a/cpuapp",
        &["slimenrf_xiao_nrf54lm20a_tracker"],
    ),
    ("xiao_ble/nrf52840/sense", &["xiao_sense_tracker"]),
    ("xiao_ble/nrf52840", &["xiao_tracker"]),
    ("dawn_tracker_mini_uf2/nrf52833", &["dawn_tracker_mini"]),
    (
        "kawazu_uf2/nrf52833",
        &["kawazu_tracker", "tracker_kawazu_jitingcat"],
    ),
    (
        "nini_slimevr_mag_uf2/nrf52833",
        &["nini_slimevr_mag", "nini_slimevr_mag_tracker"],
    ),
    ("nekonya_tracker_uf2/nrf52833", &["nekonya_tracker_uf2"]),
    ("estmini_uf2/nrf52840", &["estmini_tracker"]),
    ("paper_smol_v1_uf2/nrf52833", &["paper_smol_v1_uf2"]),
    ("slimenrf_r3/nrf52840/uf2", &["r3_tracker"]),
    (
        "slimevrmini_p1_uf2/nrf52833",
        &["slimevrmini_tracker", "slimevrmini "],
    ),
    (
        "slimevrmini_p2_uf2/nrf52833",
        &["slimevrmini2_tracker", "slimevrmini2"],
    ),
    ("slimevrmini_p3r6_uf2/nrf52833", &["slimevrmini3_r6"]),
    ("slimevrmini_p3r7_uf2/nrf52833", &["slimevrmini3_r7"]),
    (
        "slimevrmini_p4_uf2/nrf52833",
        &["slimevrmini4_tracker", "slimevrmini4 "],
    ),
    ("slimevrmini_p4r9_uf2/nrf52833", &["slimevrmini4r9"]),
    ("slimevrmini_p4r11_uf2/nrf52833", &["slimevrmini4r11"]),
];
const RECEIVER_PATTERNS: &[(&str, &[&str])] = &[
    (
        "styria_r1_uf2/nrf52840",
        &["styria_r1_receiver", "styria_receiver_r1"],
    ),
    (
        "aerorx_r1_uf2/nrf52840",
        &["aerorx_r1_receiver", "aerorx_receiver_r1"],
    ),
    (
        "promicro_uf2/nrf52840",
        &["promicro_receiver", "promicro-receiver"],
    ),
    (
        "holyiot_21017/nrf52840",
        &["holyiot_dongle_receiver", "holyiot_receiver"],
    ),
    (
        "nrf52840dongle/nrf52840",
        &["nordic_ebyte_dongle_receiver", "dongle_receiver"],
    ),
    ("xiao_ble/nrf52840", &["xiao_receiver"]),
    ("etee_dongle_uf2/nrf52840", &["etee_receiver"]),
    ("nrf52840dk/nrf52840", &["nrf52840dk_receiver"]),
];
fn upstream_hints(filename: &str, variant: &str) -> Vec<FirmwareHint> {
    if variant == "Bootloader" {
        return Vec::new();
    }
    let filename = filename.to_ascii_lowercase();
    let Some((stem, extension)) = filename.rsplit_once('.') else {
        return Vec::new();
    };
    if !matches!(extension, "uf2" | "hex" | "bin") {
        return Vec::new();
    }
    let categories: &[&str] = match variant {
        "Standard" => &[""],
        "DataCollect" => &["_datacollect", "-datacollect"],
        "DataCollect CDC" => &[
            "_datacollect_cdc",
            "-datacollect-cdc",
            "_datacollect-cdc",
            "-datacollect_cdc",
        ],
        "DataCollect HID" => &[
            "_datacollect_hid",
            "-datacollect-hid",
            "_datacollect-hid",
            "-datacollect_hid",
        ],
        _ => return Vec::new(),
    };
    let formats: &[&str] = if extension == "bin" {
        &["_update", "_mcuboot"]
    } else {
        &[""]
    };
    let mut hints = Vec::new();
    for (kind, table) in [
        (EsbOtaKind::Tracker, TRACKER_PATTERNS),
        (EsbOtaKind::Receiver, RECEIVER_PATTERNS),
    ] {
        let mut boards = BTreeSet::new();
        for (board, patterns) in table {
            // These nRF54 products are published only as HEX, not UF2.
            if board.contains("/nrf54") && extension != "hex" {
                continue;
            }
            for pattern in *patterns {
                if categories.iter().any(|category| {
                    let prefixes: &[&str] = if pattern.starts_with("slimenrf_") {
                        &[""]
                    } else {
                        &["", "slimenrf_"]
                    };
                    prefixes.iter().any(|prefix| {
                        formats.iter().any(|format| {
                            matches_application_stem(&[prefix, pattern, category, format], stem)
                        })
                    })
                }) {
                    boards.insert(*board);
                }
            }
        }
        for board in boards {
            hints.push(FirmwareHint {
                board_target: board.into(),
                kind: Some(kind),
                variant: variant.into(),
                mcuboot: extension == "bin",
                bootloader: false,
            });
        }
    }
    hints
}
/// Filename-only category hint; this does not establish board compatibility.
/// MCUboot application format remains separate from the Standard application variant.
pub fn filename_variant(name: &str) -> &'static str {
    let name = name.rsplit(['/', '\\']).next().unwrap_or(name);
    normalized_filename_variant(&name.to_ascii_lowercase())
}

fn normalized_filename_variant(name: &str) -> &'static str {
    if name.contains("bootloader") || name.starts_with("update-") {
        "Bootloader"
    } else if name.contains("datacollect_cdc") || name.contains("datacollect-cdc") {
        "DataCollect CDC"
    } else if name.contains("datacollect_hid") || name.contains("datacollect-hid") {
        "DataCollect HID"
    } else if name.contains("datacollect") {
        "DataCollect"
    } else {
        "Standard"
    }
}

#[derive(Clone, Copy)]
enum FilenameExtension<'a> {
    Flash,
    Exact(Option<&'a str>),
}

fn split_filename(name: &str) -> (&str, Option<&str>) {
    name.rsplit_once('.')
        .map_or((name, None), |(stem, extension)| (stem, Some(extension)))
}

fn supported_extension(extension: Option<&str>) -> bool {
    extension.is_some_and(|extension| {
        ["uf2", "hex", "bin", "zip"]
            .iter()
            .any(|allowed| extension.eq_ignore_ascii_case(allowed))
    })
}

fn matches_exact_pin(pin: &str, filename: &str) -> bool {
    if pin.eq_ignore_ascii_case(filename) {
        return true;
    }
    let (pin_stem, pin_extension) = split_filename(pin);
    let (stem, extension) = split_filename(filename);
    supported_extension(extension)
        && pin_extension
            .zip(extension)
            .is_some_and(|(expected, actual)| expected.eq_ignore_ascii_case(actual))
        && copy_stems(stem).any(|candidate| candidate.eq_ignore_ascii_case(pin_stem))
}

fn matches_resource_pin(pin: &str, filename: &str, variant: &str) -> bool {
    let (pin_stem, pin_extension) = split_filename(pin);
    let pin_key = filename_key(pin_stem, pin_extension, filename_variant(pin));
    let (stem, extension) = split_filename(filename);
    let max_candidates = if supported_extension(extension) {
        usize::MAX
    } else {
        1
    };
    copy_stems(stem).take(max_candidates).any(|candidate| {
        let key = filename_key(candidate, extension, variant);
        pin_key.0.eq_ignore_ascii_case(key.0)
            && match (pin_key.1, key.1) {
                (FilenameExtension::Flash, FilenameExtension::Flash) => true,
                (FilenameExtension::Exact(Some(a)), FilenameExtension::Exact(Some(b))) => {
                    a.eq_ignore_ascii_case(b)
                }
                (FilenameExtension::Exact(None), FilenameExtension::Exact(None)) => true,
                _ => false,
            }
    })
}

fn filename_key<'a>(
    mut stem: &'a str,
    extension: Option<&'a str>,
    variant: &str,
) -> (&'a str, FilenameExtension<'a>) {
    // Bootloader pins retain their complete board, profile and container.
    // Version compatibility belongs exclusively to the strict updater parser.
    if variant == "Bootloader" {
        return (stem, FilenameExtension::Exact(extension));
    }
    if extension.is_some() {
        for suffix in [
            "_datacollect_cdc",
            "_datacollect_hid",
            "-datacollect-cdc",
            "-datacollect-hid",
            "_datacollect",
            "-datacollect",
        ] {
            let Some(index) = stem.len().checked_sub(suffix.len()) else {
                continue;
            };
            if stem
                .get(index..)
                .is_some_and(|tail| tail.eq_ignore_ascii_case(suffix))
            {
                stem = &stem[..index];
                break;
            }
        }
    }
    // Only these two containers are alternative encodings of the same full firmware.
    let extension = if extension
        .is_some_and(|ext| ext.eq_ignore_ascii_case("uf2") || ext.eq_ignore_ascii_case("hex"))
    {
        FilenameExtension::Flash
    } else {
        FilenameExtension::Exact(extension)
    };
    (stem, extension)
}
