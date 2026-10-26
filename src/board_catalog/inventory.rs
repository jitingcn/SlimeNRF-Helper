use super::Catalog;
use super::identities::{matches_bootloader, matches_firmware_alias};
use crate::bundled_resources::{self, Resource};
use std::{io::ErrorKind, path::Path};

fn resources(root: Option<&Path>) -> Result<Vec<Resource>, String> {
    let mut resources = match root {
        None => bundled_resources::firmware_resources().collect(),
        Some(root) => {
            let entries = match std::fs::read_dir(root) {
                Ok(entries) => entries,
                Err(error)
                    if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) =>
                {
                    return Ok(Vec::new());
                }
                Err(error) => {
                    return Err(format!(
                        "Cannot enumerate firmware directory {}: {error}",
                        root.display()
                    ));
                }
            };
            let mut resources = Vec::new();
            for entry in entries {
                let entry = entry.map_err(|error| {
                    format!(
                        "Cannot enumerate firmware directory {}: {error}",
                        root.display()
                    )
                })?;
                let kind = entry.file_type().map_err(|error| {
                    format!(
                        "Cannot inspect firmware file {}: {error}",
                        entry.path().display()
                    )
                })?;
                // No recursion, directory fixtures, or implicit symlink traversal.
                if kind.is_file() && entry.file_name().to_str().is_some() {
                    resources.push(Resource::File(entry.path()));
                }
            }
            resources
        }
    };
    resources.retain(|resource| {
        resource
            .filename()
            .and_then(|name| name.rsplit_once('.'))
            .is_some_and(|(_, extension)| {
                ["uf2", "hex", "bin", "zip"]
                    .iter()
                    .any(|allowed| extension.eq_ignore_ascii_case(allowed))
            })
    });
    resources.sort_by(|a, b| a.filename().cmp(&b.filename()));
    resources.dedup();
    Ok(resources)
}

impl Catalog {
    pub(super) fn cache_firmware_candidates(&mut self, root: Option<&Path>) -> Result<(), String> {
        let resources = resources(root)?;
        for preset in self
            .legacy_targets
            .values_mut()
            .chain(self.board_targets.values_mut())
            .chain(self.application_targets.values_mut())
        {
            preset.additional_bootloaders = resources
                .iter()
                .filter(|resource| {
                    preset.bootloader.as_ref() != Some(*resource)
                        && resource
                            .filename()
                            .is_some_and(|name| matches_bootloader(preset, name))
                })
                .cloned()
                .collect();
            for target in &mut preset.targets {
                target.additional_firmware = resources
                    .iter()
                    .filter(|resource| {
                        target.firmware.as_ref() != Some(*resource)
                            && resource.filename().is_some_and(|name| {
                                target
                                    .firmware_aliases
                                    .iter()
                                    .any(|alias| matches_firmware_alias(alias, name))
                            })
                    })
                    .cloned()
                    .collect();
            }
        }
        Ok(())
    }
}
