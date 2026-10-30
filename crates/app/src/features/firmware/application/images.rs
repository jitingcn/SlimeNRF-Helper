use super::*;

pub(super) fn configured_opendfu_options(
    configured: Option<&board_catalog::OpenDfuTarget>,
) -> Result<secure_dfu::Options, String> {
    match configured {
        Some(target) => Ok(secure_dfu::Options {
            hw_version: target.hw_version,
            app_version: target.app_version,
            sd_req: parse_sd_req(&target.sd_req)?,
            ..secure_dfu::Options::default()
        }),
        None => Ok(secure_dfu::Options::default()),
    }
}

pub(super) fn local_image_provenance(path: &std::path::Path) -> String {
    use std::path::Component;

    #[cfg(windows)]
    let native = {
        use std::path::Prefix;
        // Verbatim and device paths have special parsing rules; their spelling is
        // meaningful, including forward slashes and dot components.
        if matches!(
            path.components().next(),
            Some(Component::Prefix(prefix))
                if prefix.kind().is_verbatim() || matches!(prefix.kind(), Prefix::DeviceNS(_))
        ) {
            return path.display().to_string();
        }
        PathBuf::from(path.to_string_lossy().replace('/', "\\"))
    };
    #[cfg(windows)]
    let path = native.as_path();

    // Do not collapse "..": it can traverse a symlink. This label is never used
    // for file access or configured-target identity.
    let normalized: PathBuf = path
        .components()
        .filter(|component| !matches!(component, Component::CurDir))
        .collect();
    if normalized.as_os_str().is_empty() && !path.as_os_str().is_empty() {
        ".".into()
    } else {
        normalized.display().to_string()
    }
}

pub(super) fn load_image(resource: Resource, cancel: &AtomicBool) -> Result<Image, String> {
    let name = resource
        .filename()
        .ok_or("Invalid firmware filename")?
        .to_owned();
    let content = resource.read(64 * 1024 * 1024, cancel)?;
    let provenance = match &resource {
        Resource::File(path) => local_image_provenance(path),
        Resource::Bundled(member) => format!("Bundled · {member}"),
    };
    let mut image = make_image(name, content.bytes, provenance, content.digest, true);
    image.resource = Some(Arc::new(resource));
    if let Some(error) = &image.validation_error {
        return Err(error.clone());
    }
    Ok(image)
}

pub(super) fn make_image(
    name: String,
    bytes: Vec<u8>,
    provenance: String,
    digest: String,
    local: bool,
) -> Image {
    let extension = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    let validation = match extension.as_str() {
        "uf2" | "hex" | "bin" => firmware::parse_image(&name, &bytes)
            .map(|parsed| Some(Arc::new(parsed)))
            .map_err(String::from),
        "zip" => Ok(None),
        _ => Err("Choose UF2, Intel HEX, MCUboot BIN or DFU ZIP firmware".into()),
    };
    let (parsed, validation_error) = match validation {
        Ok(parsed) => (parsed, None),
        Err(error) => (None, Some(error)),
    };
    Image {
        name,
        bytes: bytes.into(),
        provenance,
        resource: None,
        digest,
        local,
        parsed,
        hints: Arc::from([]),
        validation_error,
    }
}

fn parse_sd_req(value: &str) -> Result<Vec<u32>, String> {
    if value.trim().is_empty() || value.trim().eq_ignore_ascii_case("none") {
        return Ok(Vec::new());
    }
    value
        .split(',')
        .map(|part| {
            let part = part.trim();
            let result =
                if let Some(hex) = part.strip_prefix("0x").or_else(|| part.strip_prefix("0X")) {
                    u32::from_str_radix(hex, 16)
                } else {
                    part.parse()
                };
            result.map_err(|_| format!("Invalid configured SoftDevice requirement: {part}"))
        })
        .collect()
}

pub(super) fn firmware_filter_matches(
    hints: &[board_catalog::FirmwareHint],
    variant: &str,
    targets: &[(String, Option<board_catalog::EsbOtaKind>)],
    matched_only: bool,
    variant_filter: Option<&str>,
) -> bool {
    variant_filter.is_none_or(|filter| filter == variant)
        && (!matched_only
            || targets.is_empty()
            || hints.iter().any(|hint| {
                targets.iter().any(|(board, kind)| {
                    *board == board_catalog::canonical_board(&hint.board_target)
                        && (hint.bootloader || kind.is_none() || *kind == hint.kind)
                })
            }))
}

pub(super) fn image_category(name: &str, hints: &[board_catalog::FirmwareHint]) -> ImageCategory {
    if board_catalog::filename_variant(name) == "Bootloader"
        || hints.iter().any(|hint| hint.bootloader)
    {
        ImageCategory::Bootloader
    } else {
        ImageCategory::Application
    }
}

pub(super) fn firmware_page_matches<'a>(
    mode: Mode,
    category: ImageCategory,
    name: &str,
    hints: &[board_catalog::FirmwareHint],
    targets: impl Iterator<Item = &'a esb_ota::Target> + Clone,
    archive: bool,
) -> bool {
    let actual_category = image_category(name, hints);
    if actual_category
        != if mode == Mode::Uf2 {
            category
        } else {
            ImageCategory::Application
        }
    {
        return false;
    }
    let variant = board_catalog::filename_variant(name);
    if actual_category == ImageCategory::Application {
        if !matches!(variant, "Standard" | "DataCollect CDC")
            || hints
                .iter()
                .any(|hint| !matches!(hint.variant.as_str(), "Standard" | "DataCollect CDC"))
        {
            return false;
        }
        if (variant == "DataCollect CDC"
            || hints.iter().any(|hint| hint.variant == "DataCollect CDC"))
            && !hints
                .iter()
                .any(|hint| hint.kind == Some(board_catalog::EsbOtaKind::Receiver))
            && !(mode == Mode::Esb
                && targets
                    .clone()
                    .any(|target| target.id == esb_ota::RECEIVER_ID))
        {
            return false;
        }
    }
    let lower = name.to_ascii_lowercase();
    if actual_category == ImageCategory::Bootloader && lower.contains("datacollect") {
        return false;
    }
    let file = if archive {
        lower.strip_suffix(".zip").unwrap_or(&lower)
    } else {
        &lower
    };
    let mcuboot = actual_category == ImageCategory::Application
        && (file.ends_with(".bin")
            || file.ends_with("_update")
            || file.contains("mcuboot")
            || hints.iter().any(|hint| hint.mcuboot));
    let container =
        archive && !file.ends_with(".uf2") && !file.ends_with(".hex") && !file.ends_with(".bin");
    match mode {
        Mode::Uf2 => !mcuboot && (file.ends_with(".uf2") || container),
        Mode::OpenDfu => {
            !mcuboot && (file.ends_with(".hex") || file.ends_with(".zip") || container)
        }
        Mode::Ble => !mcuboot && file.ends_with(".zip") && variant == "Standard",
        Mode::Esb => {
            if mcuboot && !(file.ends_with(".bin") || container) {
                return false;
            }
            if !(mcuboot || file.ends_with(".uf2") || file.ends_with(".hex") || container) {
                return false;
            }
            let mut known = false;
            let compatible = targets.clone().any(|target| {
                known = true;
                ota_supported(target)
                    && (target.info.bootloader == "mcuboot") == mcuboot
                    && (variant != "DataCollect CDC" || target.id == esb_ota::RECEIVER_ID)
                    && (hints.is_empty()
                        || hints.iter().any(|hint| {
                            board_catalog::canonical_board(&hint.board_target)
                                == board_catalog::canonical_board(&target.info.board_target)
                                && hint.kind.is_none_or(|kind| {
                                    (kind == board_catalog::EsbOtaKind::Receiver)
                                        == (target.id == esb_ota::RECEIVER_ID)
                                })
                        }))
            });
            compatible || (!known && !mcuboot)
        }
    }
}

pub(super) fn uf2_preset_matches(
    preset: &board_catalog::FlashPreset,
    hints: &[board_catalog::FirmwareHint],
) -> bool {
    hints.iter().any(|hint| {
        let board = board_catalog::canonical_board(&hint.board_target);
        (hint.bootloader && board == board_catalog::canonical_board(&preset.key))
            || preset.targets.iter().any(|target| {
                board
                    == board_catalog::canonical_board(target.name.as_deref().unwrap_or(&preset.key))
                    && (hint.bootloader
                        || hint.kind.is_none()
                        || target.esb_ota_kind.is_none()
                        || hint.kind == target.esb_ota_kind)
            })
    })
}

impl Image {
    pub(super) fn configured_opendfu<'a>(
        &self,
        catalog: &'a board_catalog::Catalog,
    ) -> Option<&'a board_catalog::OpenDfuTarget> {
        catalog
            .opendfu_targets
            .iter()
            .find(|target| self.resource.as_deref() == Some(&target.firmware))
    }
}

struct LocalFirmwareRequest {
    mode: Mode,
    destination: ImageDestination,
}

pub(super) fn single_firmware_path(
    mut paths: impl ExactSizeIterator<Item = PathBuf>,
) -> Result<PathBuf, String> {
    if paths.len() != 1 {
        return Err(tr("firmware.error.single_local_file"));
    }
    Ok(paths.next().unwrap())
}

pub(super) fn load_local_image(path: PathBuf, cancel: &AtomicBool) -> Result<Image, String> {
    let metadata =
        std::fs::metadata(&path).map_err(|error| format!("{}: {error}", path.display()))?;
    if !metadata.is_file() {
        return Err(tr("firmware.error.local_file_required"));
    }
    load_image(Resource::File(path), cancel)
}

impl Workbench {
    pub(super) fn choose_firmware(&mut self, cx: &mut Context<Self>) {
        if self.shutting_down || self.flash.busy() {
            return;
        }
        let mode = self.flash_mode();
        let Some(destination) = self.flash.destination(mode) else {
            return;
        };
        self.flash.explicit_image_intent(&destination);
        let request = LocalFirmwareRequest { mode, destination };
        self.flash.activity_origin = mode;
        self.flash.ble_activity = mode == Mode::Ble;
        let dialog = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(tr("firmware.image.choose_title").into()),
        });
        self.flash.chooser = Some(cx.spawn(async move |this, cx| {
            let chosen = dialog.await;
            let _ = this.update(cx, |this, cx| {
                this.flash.chooser = None;
                if this.shutting_down {
                    cx.notify();
                    return;
                }
                match chosen {
                    Ok(Ok(Some(paths))) => this.load_local_firmware(request, paths.into_iter()),
                    Ok(Err(error)) => this.flash.set_error(mode, error.to_string()),
                    Err(error) => this.flash.set_error(mode, error.to_string()),
                    _ => {}
                }
                cx.notify();
            });
        }));
    }

    pub(in crate::application) fn drop_firmware(
        &mut self,
        paths: &ExternalPaths,
        cx: &mut Context<Self>,
    ) {
        if self.shutting_down
            || self.flash.busy()
            || !matches!(self.active_page, FeaturePage::Uf2 | FeaturePage::Ota)
        {
            return;
        }
        let mode = self.flash_mode();
        let Some(destination) = self.flash.destination(mode) else {
            self.flash
                .set_error(mode, tr("firmware.error.query_select_first"));
            cx.notify();
            return;
        };
        self.load_local_firmware(
            LocalFirmwareRequest { mode, destination },
            paths.paths().iter().cloned(),
        );
        cx.notify();
    }

    fn load_local_firmware(
        &mut self,
        request: LocalFirmwareRequest,
        paths: impl ExactSizeIterator<Item = PathBuf>,
    ) {
        if self.shutting_down
            || self.flash.busy()
            || !self.flash.destination_exists(&request.destination)
        {
            return;
        }
        let path = match single_firmware_path(paths) {
            Ok(path) => path,
            Err(error) => {
                self.flash.set_error(request.mode, error);
                return;
            }
        };
        self.flash.explicit_image_intent(&request.destination);
        // Local selection only loads: revoke an old confirmation/auto-copy even
        // if this file later fails validation.
        self.flash.review = None;
        self.flash.acknowledged = false;
        self.flash.auto = false;
        self.flash.auto_requested = false;
        self.start_flash_job_for(request.mode, ActivityKind::Preparation, move |cancel, _| {
            load_local_image(path, cancel)
                .map(|image| Payload::Images(request.destination, vec![image]))
        });
    }

    pub(super) fn load_configured_image(&mut self, path: Resource) {
        if self.shutting_down || self.flash.busy() {
            return;
        }
        let Some(destination) = self.flash.destination(self.flash_mode()) else {
            return;
        };
        self.flash.explicit_image_intent(&destination);
        self.start_flash_job(move |cancel, _| {
            load_image(path, cancel).map(|image| Payload::Images(destination, vec![image]))
        });
    }

    pub(super) fn candidate_allowed(
        &self,
        name: &str,
        hints: &[board_catalog::FirmwareHint],
        archive: bool,
    ) -> bool {
        let targets = self.flash.targets.iter().filter(|target| {
            self.flash
                .selected_targets
                .contains(&ota_target_key(target))
                && self.flash.active_group.as_deref() == Some(ota_assignment_key(target).as_str())
        });
        firmware_page_matches(
            self.flash_mode(),
            self.flash.category,
            name,
            hints,
            targets,
            archive,
        )
    }

    pub(super) fn validate_page_image(&self, image: &Image) -> Result<(), String> {
        if self.candidate_allowed(&image.name, &image.hints, false) {
            Ok(())
        } else {
            Err(tr("firmware.error.image_method_mismatch"))
        }
    }

    pub(super) fn flash_filter_targets(&self) -> Vec<(String, Option<board_catalog::EsbOtaKind>)> {
        let mut targets = Vec::new();
        match self.flash_mode() {
            Mode::Esb => {
                for target in &self.flash.targets {
                    if self
                        .flash
                        .selected_targets
                        .contains(&ota_target_key(target))
                        && self.flash.active_group.as_deref()
                            == Some(ota_assignment_key(target).as_str())
                    {
                        targets.push((
                            board_catalog::canonical_board(&target.info.board_target),
                            Some(if target.id == esb_ota::RECEIVER_ID {
                                board_catalog::EsbOtaKind::Receiver
                            } else {
                                board_catalog::EsbOtaKind::Tracker
                            }),
                        ));
                    }
                }
            }
            Mode::Uf2 => {
                if let Some(catalog) = &self.flash.catalog {
                    for volume in &self.flash.volumes {
                        if !self.flash.selected_volumes.contains(&volume.identity) {
                            continue;
                        }
                        if let Some(preset) =
                            catalog.preset_for_volume(&volume.label, volume.board_id.as_deref())
                        {
                            if preset.targets.is_empty()
                                && self.flash.category == ImageCategory::Bootloader
                            {
                                targets.push((board_catalog::canonical_board(&preset.key), None));
                            }
                            targets.extend(preset.targets.iter().map(|target| {
                                (
                                    board_catalog::canonical_board(
                                        target.name.as_deref().unwrap_or(&preset.key),
                                    ),
                                    target.esb_ota_kind,
                                )
                            }));
                        }
                    }
                }
            }
            _ => {}
        }
        targets
    }
}
