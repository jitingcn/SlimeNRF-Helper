use super::*;

pub(super) fn target_label(target: &esb_ota::Target) -> String {
    if target.id == esb_ota::RECEIVER_ID {
        format!("{} · {}", target.receiver.label, target.info.board_target)
    } else {
        format!(
            "{} · #{} · {} · {}",
            target.receiver.label,
            target.id,
            target.address.as_deref().unwrap_or("?"),
            target.info.board_target
        )
    }
}

pub(super) fn ota_role(target: &esb_ota::Target) -> &'static str {
    if target.id == esb_ota::RECEIVER_ID {
        "Receiver"
    } else {
        "Tracker"
    }
}

pub(super) fn validate_ota_image(image: &Image, target: &esb_ota::Target) -> Result<(), String> {
    if let Some(error) = &image.validation_error {
        return Err(error.clone());
    }
    if !firmware_page_matches(
        Mode::Esb,
        ImageCategory::Application,
        &image.name,
        &image.hints,
        std::iter::once(target),
        false,
    ) {
        return Err(tr("firmware.error.image_method_mismatch"));
    }
    if !image.local && image.hints.is_empty() {
        return Err(tr("firmware.error.unknown_remote_target"));
    }
    if image.hints.len() > 1 {
        return Err(tr("firmware.error.ambiguous_target"));
    }
    if target.id == esb_ota::RECEIVER_ID && target.info.bootloader == "nrf5_opendfu" {
        return Err(tr("firmware.error.receiver_requires_opendfu"));
    }
    if target.info.protocol_version != 1
        || !matches!(
            target.info.bootloader.as_str(),
            "none" | "adafruit_uf2" | "mcuboot"
        )
    {
        return Err(tr("firmware.error.unsupported_method"));
    }
    let parsed = image
        .parsed
        .as_ref()
        .ok_or_else(|| tr("firmware.error.esb_image_format"))?;
    if (target.info.bootloader == "mcuboot") != parsed.mcuboot {
        return Err(tr("firmware.error.mcuboot_image_format"));
    }
    if let Some(hint) = image.hints.first() {
        let kind = if target.id == esb_ota::RECEIVER_ID {
            board_catalog::EsbOtaKind::Receiver
        } else {
            board_catalog::EsbOtaKind::Tracker
        };
        if hint.bootloader
            || hint.kind.is_some_and(|value| value != kind)
            || board_catalog::canonical_board(&hint.board_target)
                != board_catalog::canonical_board(&target.info.board_target)
        {
            return Err(tr("firmware.error.board_role_mismatch"));
        }
    }
    Ok(())
}

pub(super) fn plan_ota_groups(state: &FlashState) -> Result<Vec<OtaPlanGroup>, String> {
    if state.selected_targets.is_empty() {
        return Err(tr("firmware.error.query_select_first"));
    }
    if state.unresolved_tracker_selection() {
        return Err(tr("firmware.error.tracker_info_required"));
    }
    let mut groups = std::collections::BTreeMap::<(bool, String, String), OtaPlanGroup>::new();
    for key in &state.selected_targets {
        let target = state
            .targets
            .iter()
            .find(|target| ota_target_key(target) == *key)
            .ok_or_else(|| format!("{key}: {}", tr("firmware.error.target_disappeared")))?;
        let assignment = ota_assignment_key(target);
        let image = state.assignments.get(&assignment).ok_or_else(|| {
            format!(
                "{} · {}: {}",
                target.info.board_target,
                ota_role_label(target),
                tr("firmware.error.assign_every_group")
            )
        })?;
        validate_ota_image(image, target)?;
        groups
            .entry((
                target.id == esb_ota::RECEIVER_ID,
                target.receiver.key.clone(),
                assignment,
            ))
            .or_insert_with(|| OtaPlanGroup {
                targets: Vec::new(),
                image: image.clone(),
            })
            .targets
            .push(target.clone());
    }
    // All relay tracker groups finish before any receiver can reboot itself.
    Ok(groups.into_values().collect())
}

pub(super) fn ota_supported(target: &esb_ota::Target) -> bool {
    target.info.protocol_version == 1
        && matches!(
            target.info.bootloader.as_str(),
            "none" | "adafruit_uf2" | "mcuboot"
        )
}

pub(super) fn ota_update_guidance(target: &esb_ota::Target) -> Option<OtaUpdateGuidance> {
    if ota_supported(target) {
        return None;
    }
    Some(match target.info.bootloader.as_str() {
        "nrf5_opendfu" => OtaUpdateGuidance::OpenDfu,
        "adafruit_uf2" => OtaUpdateGuidance::Uf2,
        _ => OtaUpdateGuidance::DocumentedMethod,
    })
}

pub(super) fn validate_live_ota_targets<'a, 'b>(
    targets: impl IntoIterator<Item = &'a esb_ota::Target>,
    receivers: &[ReceiverEndpoint],
    trackers: impl Iterator<Item = &'b TrackerLink> + Clone,
) -> Result<(), String> {
    for target in targets {
        if !receivers.iter().any(|receiver| {
            receiver.key == target.receiver.key
                && receiver.path == target.receiver.path
                && receiver.serial_number == target.receiver.serial_number
        }) {
            return Err(tr("firmware.error.receiver_disappeared"));
        }
        if target.id == esb_ota::RECEIVER_ID {
            continue;
        }
        let tracker = trackers
            .clone()
            .find(|tracker| {
                tracker.receiver_key == target.receiver.key && tracker.slot == target.id
            })
            .filter(|tracker| tracker.online)
            .ok_or_else(|| tr("firmware.error.wait_tracker_data"))?;
        let address = tracker
            .address
            .as_deref()
            .and_then(crate::domain::normalize_address)
            .ok_or_else(|| tr("firmware.error.wait_tracker_data"))?;
        if Some(address)
            != target
                .address
                .as_deref()
                .and_then(crate::domain::normalize_address)
        {
            return Err(tr("firmware.error.tracker_address_changed"));
        }
    }
    Ok(())
}

pub(super) fn plan_uf2_action(
    catalog: Option<&board_catalog::Catalog>,
    category: ImageCategory,
    volumes: Vec<uf2::Uf2Volume>,
    image: Image,
) -> Result<Action, String> {
    if let Some(error) = &image.validation_error {
        return Err(error.clone());
    }
    if !firmware_page_matches(
        Mode::Uf2,
        category,
        &image.name,
        &image.hints,
        std::iter::empty(),
        false,
    ) {
        return Err(tr("firmware.error.image_method_mismatch"));
    }
    if !image.name.to_ascii_lowercase().ends_with(".uf2") {
        return Err(tr("firmware.error.uf2_required"));
    }
    if volumes.is_empty() {
        return Err(tr("firmware.error.select_target"));
    }
    let hints = &image.hints;
    let purpose = match category {
        ImageCategory::Bootloader => {
            if hints.is_empty() || hints.iter().any(|hint| !hint.bootloader) {
                return Err(tr("firmware.error.bootloader_board_mismatch"));
            }
            let mut requested_chip = None;
            for volume in &volumes {
                let preset = catalog
                    .and_then(|catalog| {
                        catalog.preset_for_volume(&volume.label, volume.board_id.as_deref())
                    })
                    .ok_or_else(|| tr("firmware.error.bootloader_identity_required"))?;
                let chip = preset
                    .bootloader_mcu
                    .ok_or_else(|| tr("firmware.error.bootloader_identity_required"))?;
                if !uf2_preset_matches(preset, hints) {
                    return Err(tr("firmware.error.bootloader_board_mismatch"));
                }
                if requested_chip.is_some_and(|previous| previous != chip) {
                    return Err(tr("firmware.error.bootloader_mcu_mismatch"));
                }
                requested_chip = Some(chip);
            }
            Uf2ImagePurpose::Bootloader(requested_chip.expect("nonempty selected volumes"))
        }
        ImageCategory::Application => {
            if !image.local && hints.is_empty() {
                return Err(tr("firmware.error.unknown_remote_target"));
            }
            if hints.len() > 1 {
                return Err(tr("firmware.error.ambiguous_target"));
            }
            if !hints.is_empty() {
                for volume in &volumes {
                    let preset = catalog.and_then(|catalog| {
                        catalog.preset_for_volume(&volume.label, volume.board_id.as_deref())
                    });
                    if preset.is_some_and(|preset| !uf2_preset_matches(preset, hints)) {
                        return Err(tr("firmware.error.board_role_mismatch"));
                    }
                    if preset.is_none() && !image.local {
                        return Err(tr("firmware.error.unknown_volume_board"));
                    }
                }
            }
            Uf2ImagePurpose::Application
        }
    };
    for volume in &volumes {
        uf2::validate_for_volume(volume, &image.bytes, purpose)?;
    }
    Ok(Action::Uf2(volumes, image, purpose))
}

impl Workbench {
    pub(super) fn selected_flash_action(&self) -> Result<Action, String> {
        if self.flash_mode() == Mode::Esb {
            let groups = plan_ota_groups(&self.flash)?;
            self.validate_flash_ota_routes(&groups)?;
            return Ok(Action::EsbGroups(groups));
        }
        let image = self
            .flash
            .image
            .clone()
            .ok_or_else(|| tr("firmware.error.choose_image_first").to_string())?;
        if self.flash_mode() == Mode::Uf2 {
            let volumes = self
                .flash
                .volumes
                .iter()
                .filter(|volume| self.flash.selected_volumes.contains(&volume.identity))
                .cloned()
                .collect();
            return plan_uf2_action(
                self.flash.catalog.as_ref(),
                self.flash.category,
                volumes,
                image,
            );
        }
        if let Some(error) = &image.validation_error {
            return Err(error.clone());
        }
        self.validate_page_image(&image)?;
        let mode = self.flash_mode();
        let hints = &image.hints;
        if !image.local && hints.is_empty() {
            return Err(tr("firmware.error.unknown_remote_target").to_string());
        }
        if hints.len() > 1 {
            return Err(tr("firmware.error.ambiguous_target").to_string());
        }
        match mode {
            Mode::Uf2 => unreachable!("UF2 is planned with frozen write authorization above"),
            Mode::OpenDfu => {
                if !image.name.to_ascii_lowercase().ends_with(".hex")
                    && !image.name.to_ascii_lowercase().ends_with(".zip")
                {
                    return Err(tr("firmware.error.opendfu_image_format").to_string());
                }
                let port = self.flash_selected_port()?;
                if !port.is_open_dfu_bootloader()
                    || !crate::domain::console_port_allowed(&port, &self.inventory)
                {
                    return Err(tr("firmware.error.select_bootloader_port").to_string());
                }
                let configured = self
                    .flash
                    .catalog
                    .as_ref()
                    .and_then(|catalog| image.configured_opendfu(catalog));
                let board = configured
                    .map(|target| target.name.as_str())
                    .unwrap_or("holyiot-21017");
                if let Some(hint) = hints.first()
                    && (hint.kind == Some(board_catalog::EsbOtaKind::Tracker)
                        || board_catalog::canonical_board(&hint.board_target)
                            != board_catalog::canonical_board(board))
                {
                    return Err(tr("firmware.error.board_role_mismatch").to_string());
                }
                let options = configured_opendfu_options(configured)?;
                Ok(Action::OpenDfu(port, image, options))
            }
            Mode::Esb => plan_ota_groups(&self.flash).map(Action::EsbGroups),
            Mode::Ble => {
                if !image.local || !image.name.to_ascii_lowercase().ends_with(".zip") {
                    return Err(tr("firmware.error.ble_zip_required").to_string());
                }
                let devices: Vec<_> = self
                    .flash
                    .ble
                    .iter()
                    .filter(|device| self.flash.selected_ble.contains(&device.id))
                    .cloned()
                    .collect();
                if devices.is_empty() {
                    return Err(tr("firmware.error.select_target").to_string());
                }
                Ok(Action::Ble(devices, image))
            }
        }
    }

    pub(super) fn flash_selected_port(&self) -> Result<SerialEndpoint, String> {
        self.inventory
            .ports
            .iter()
            .find(|port| Some(&port.port_name) == self.flash.selected_port.as_ref())
            .cloned()
            .ok_or_else(|| tr("firmware.error.select_serial_port").to_string())
    }

    pub(super) fn validate_flash_ota_routes(&self, groups: &[OtaPlanGroup]) -> Result<(), String> {
        let trackers = self
            .inventory
            .receivers
            .iter()
            .filter_map(|receiver| self.monitor.observation(&receiver.key))
            .flat_map(|observation| observation.trackers.iter());
        validate_live_ota_targets(
            groups.iter().flat_map(|group| group.targets.iter()),
            &self.inventory.receivers,
            trackers,
        )
    }

    pub(in crate::ui) fn ota_assignment_groups(&self) -> Vec<OtaAssignmentView> {
        let mut groups = std::collections::BTreeMap::<String, (&esb_ota::Target, usize)>::new();
        for target in &self.flash.targets {
            if self
                .flash
                .selected_targets
                .contains(&ota_target_key(target))
            {
                let group = groups
                    .entry(ota_assignment_key(target))
                    .or_insert((target, 0));
                group.1 += 1;
            }
        }
        groups
            .into_iter()
            .map(|(key, (target, count))| OtaAssignmentView {
                label: format!(
                    "{} · {}",
                    self.flash
                        .catalog
                        .as_ref()
                        .and_then(|catalog| {
                            let kind = if target.id == esb_ota::RECEIVER_ID {
                                board_catalog::EsbOtaKind::Receiver
                            } else {
                                board_catalog::EsbOtaKind::Tracker
                            };
                            catalog
                                .presets()
                                .find(|preset| {
                                    preset.display_name != preset.key
                                        && preset.targets.iter().any(|configured| {
                                            configured.esb_ota_kind == Some(kind)
                                                && configured.name.as_deref()
                                                    == Some(target.info.board_target.as_str())
                                        })
                                })
                                .map(|preset| preset.display_name.as_str())
                        })
                        .unwrap_or(&target.info.board_target),
                    if target.id == esb_ota::RECEIVER_ID {
                        rust_i18n::t!("firmware.selection.receiver_count", count = count)
                            .to_string()
                    } else {
                        rust_i18n::t!("firmware.selection.tracker_count", count = count).to_string()
                    }
                ),
                board_target: target.info.board_target.clone(),
                bootloader: target.info.bootloader.clone(),
                image_name: self
                    .flash
                    .assignments
                    .get(&key)
                    .map(|image| image.name.clone()),
                key,
            })
            .collect()
    }
}
