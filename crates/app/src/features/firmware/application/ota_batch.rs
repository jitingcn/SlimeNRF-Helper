use super::*;
use std::{collections::HashSet, sync::atomic::Ordering};

#[cfg(test)]
#[path = "ota_batch_tests.rs"]
mod tests;

const MAX_BATCH_FILES: usize = 32;
const MAX_BATCH_FILE_BYTES: u64 = firmware::MAX_CONTAINER_SIZE as u64;
const MAX_BATCH_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, PartialEq, Eq)]
struct OtaMember {
    receiver_key: String,
    receiver_path: Vec<u8>,
    receiver_label: String,
    receiver_serial: Option<String>,
    id: u8,
    address: Option<String>,
    info: esb_ota::FirmwareInfo,
}

impl From<&esb_ota::Target> for OtaMember {
    fn from(target: &esb_ota::Target) -> Self {
        Self {
            receiver_key: target.receiver.key.clone(),
            receiver_path: target.receiver.path.clone(),
            receiver_label: target.receiver.label.clone(),
            receiver_serial: target.receiver.serial_number.clone(),
            id: target.id,
            address: target.address.clone(),
            info: target.info.clone(),
        }
    }
}

pub(super) struct OtaGroupEpoch {
    members: Vec<OtaMember>,
    pub(super) revision: u64,
}

#[derive(Clone)]
pub(super) struct OtaBatchGroup {
    key: String,
    generation: u64,
    revision: u64,
    members: Vec<OtaMember>,
    targets: Vec<esb_ota::Target>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct BatchGroupDescriptor {
    board: String,
    receiver: bool,
    bootloader: String,
}

impl BatchGroupDescriptor {
    fn label(&self) -> String {
        format!(
            "{} · {} · {}",
            self.board,
            ota_role_label_for_receiver(self.receiver),
            self.bootloader
        )
    }
}

impl OtaBatchGroup {
    fn descriptor(&self) -> BatchGroupDescriptor {
        let target = &self.targets[0];
        BatchGroupDescriptor {
            board: target.info.board_target.clone(),
            receiver: target.id == esb_ota::RECEIVER_ID,
            bootloader: target.info.bootloader.clone(),
        }
    }
}

pub(in crate::application) struct OtaBatchPrepared {
    groups: Vec<OtaBatchGroup>,
    files: Vec<PreparedFile>,
}

struct PreparedFile {
    path: PathBuf,
    image: Option<Image>,
    candidates: Vec<(usize, Result<(), String>)>,
    error: Option<String>,
    unknown: bool,
}

pub(super) struct OtaBatchResult {
    entries: Vec<BatchEntry>,
}

struct BatchEntry {
    path: PathBuf,
    outcomes: Vec<BatchOutcome>,
}

enum BatchOutcome {
    Assigned(BatchGroupDescriptor),
    Replaced(BatchGroupDescriptor),
    Conflict(BatchGroupDescriptor),
    Stale(BatchGroupDescriptor),
    Invalid(Option<BatchGroupDescriptor>, String),
    Ambiguous(Vec<BatchGroupDescriptor>),
    Unknown,
    NoMatch,
}

fn members_for_group(state: &FlashState, key: &str) -> Vec<OtaMember> {
    let mut members: Vec<_> = state
        .targets
        .iter()
        .filter(|target| {
            state.selected_targets.contains(&ota_target_key(target))
                && ota_assignment_key(target) == key
        })
        .map(OtaMember::from)
        .collect();
    members.sort_by(|a, b| (&a.receiver_key, a.id).cmp(&(&b.receiver_key, b.id)));
    members
}

impl FlashState {
    pub(super) fn reconcile_group_epochs(&mut self) {
        let keys: Vec<_> = self.group_generations.keys().cloned().collect();
        self.group_epochs
            .retain(|key, _| self.group_generations.contains_key(key));
        for key in keys {
            let members = members_for_group(self, &key);
            match self.group_epochs.get_mut(&key) {
                Some(epoch) if epoch.members != members => {
                    epoch.members = members;
                    epoch.revision += 1;
                }
                Some(_) => {}
                None => {
                    self.group_epochs.insert(
                        key,
                        OtaGroupEpoch {
                            members,
                            revision: 0,
                        },
                    );
                }
            }
        }
    }

    pub(super) fn ota_batch_scope(&self) -> Vec<OtaBatchGroup> {
        let mut groups: Vec<_> = self
            .group_generations
            .iter()
            .filter_map(|(key, generation)| {
                let epoch = self.group_epochs.get(key)?;
                let mut targets: Vec<_> = self
                    .targets
                    .iter()
                    .filter(|target| {
                        self.selected_targets.contains(&ota_target_key(target))
                            && ota_assignment_key(target) == *key
                    })
                    .cloned()
                    .collect();
                targets.sort_by(|a, b| (&a.receiver.key, a.id).cmp(&(&b.receiver.key, b.id)));
                (!targets.is_empty()).then(|| OtaBatchGroup {
                    key: key.clone(),
                    generation: *generation,
                    revision: epoch.revision,
                    members: epoch.members.clone(),
                    targets,
                })
            })
            .collect();
        groups.sort_by(|a, b| a.key.cmp(&b.key));
        groups
    }

    fn batch_group_current(&self, group: &OtaBatchGroup) -> bool {
        self.group_generations.get(&group.key) == Some(&group.generation)
            && self.group_epochs.get(&group.key).is_some_and(|epoch| {
                epoch.revision == group.revision && epoch.members == group.members
            })
            && members_for_group(self, &group.key) == group.members
    }

    pub(super) fn begin_ota_batch(&mut self, groups: &mut [OtaBatchGroup]) {
        self.source_revision += 1;
        self.review = None;
        self.acknowledged = false;
        self.risk_acknowledged = false;
        self.auto = false;
        self.auto_requested = false;
        self.ota_batch_result = None;
        for group in groups {
            if self.batch_group_current(group) {
                let destination = ImageDestination::Ota {
                    key: group.key.clone(),
                    generation: group.generation,
                };
                self.explicit_image_intent(&destination);
                if let Some(epoch) = self.group_epochs.get(&group.key) {
                    group.revision = epoch.revision;
                }
            }
        }
    }

    pub(super) fn apply_ota_batch(&mut self, prepared: OtaBatchPrepared) {
        let mut counts = vec![0usize; prepared.groups.len()];
        for file in &prepared.files {
            let mut eligible = file.candidates.iter().filter(|(_, valid)| valid.is_ok());
            if let (Some((index, _)), None) = (eligible.next(), eligible.next()) {
                counts[*index] += 1;
            }
        }
        let mut entries = Vec::with_capacity(prepared.files.len());
        for file in prepared.files {
            let mut outcomes = Vec::new();
            if let Some(error) = file.error {
                outcomes.push(BatchOutcome::Invalid(None, error));
            } else if file.unknown {
                outcomes.push(BatchOutcome::Unknown);
            } else if file.candidates.is_empty() {
                outcomes.push(BatchOutcome::NoMatch);
            } else {
                let ambiguous = file
                    .candidates
                    .iter()
                    .filter(|(_, valid)| valid.is_ok())
                    .take(2)
                    .count()
                    > 1;
                if ambiguous {
                    outcomes.push(BatchOutcome::Ambiguous(
                        file.candidates
                            .iter()
                            .filter(|(_, valid)| valid.is_ok())
                            .map(|(index, _)| prepared.groups[*index].descriptor())
                            .collect(),
                    ));
                }
                for (index, candidate) in file.candidates {
                    let group = &prepared.groups[index];
                    if candidate.is_ok() && ambiguous {
                        continue;
                    }
                    let descriptor = group.descriptor();
                    if !self.batch_group_current(group) {
                        outcomes.push(BatchOutcome::Stale(descriptor));
                    } else if let Err(error) = candidate {
                        outcomes.push(BatchOutcome::Invalid(Some(descriptor), error));
                    } else if counts[index] > 1 {
                        outcomes.push(BatchOutcome::Conflict(descriptor));
                    } else if let Some(image) = &file.image {
                        let replaced = self
                            .assignments
                            .insert(group.key.clone(), image.clone())
                            .is_some();
                        outcomes.push(if replaced {
                            BatchOutcome::Replaced(descriptor)
                        } else {
                            BatchOutcome::Assigned(descriptor)
                        });
                    }
                }
            }
            entries.push(BatchEntry {
                path: file.path,
                outcomes,
            });
        }
        self.ota_batch_result = Some(OtaBatchResult { entries });
    }
}

pub(super) fn distinct_batch_paths(
    paths: impl IntoIterator<Item = PathBuf>,
) -> Result<Vec<PathBuf>, String> {
    let mut seen = HashSet::new();
    let mut result = Vec::new();
    for path in paths {
        if seen.insert(path.as_os_str().to_os_string()) {
            result.push(path);
            if result.len() > MAX_BATCH_FILES {
                return Err(tr("firmware.batch.too_many"));
            }
        }
    }
    if result.is_empty() {
        return Err(tr("firmware.error.single_local_file"));
    }
    Ok(result)
}

pub(super) fn prepare_ota_batch(
    paths: Vec<PathBuf>,
    groups: Vec<OtaBatchGroup>,
    catalog: Option<board_catalog::Catalog>,
    cancel: &AtomicBool,
) -> Result<OtaBatchPrepared, String> {
    let mut metadata = Vec::with_capacity(paths.len());
    let mut declared = 0u64;
    for path in &paths {
        if cancel.load(Ordering::Acquire) {
            return Err("Cancelled".into());
        }
        let info = std::fs::metadata(path)
            .map_err(|error| format!("{}: {error}", path.display()))
            .and_then(|info| {
                if info.is_file() {
                    Ok(info.len())
                } else {
                    Err(tr("firmware.error.local_file_required"))
                }
            });
        if let Ok(size) = info {
            declared = declared
                .checked_add(size)
                .ok_or_else(|| tr("firmware.batch.too_large"))?;
            if declared > MAX_BATCH_BYTES {
                return Err(tr("firmware.batch.too_large"));
            }
        }
        metadata.push(info);
    }
    let mut total = 0u64;
    let mut files = Vec::with_capacity(paths.len());
    for (path, info) in paths.into_iter().zip(metadata) {
        if cancel.load(Ordering::Acquire) {
            return Err("Cancelled".into());
        }
        let mut file = PreparedFile {
            path,
            image: None,
            candidates: Vec::new(),
            error: None,
            unknown: false,
        };
        let mut exceeded_total = false;
        let read = info.and_then(|size| {
            if size > MAX_BATCH_FILE_BYTES {
                return Err(tr("firmware.batch.file_too_large"));
            }
            let resource = Resource::File(file.path.clone());
            let remaining = MAX_BATCH_BYTES - total;
            let limit = remaining.min(MAX_BATCH_FILE_BYTES);
            let content = match resource.read(limit, cancel) {
                Ok(content) => content,
                Err(error)
                    if limit < MAX_BATCH_FILE_BYTES
                        && error.ends_with(&format!("Resource exceeds {limit} byte limit")) =>
                {
                    exceeded_total = true;
                    return Err(error);
                }
                Err(error) => return Err(error),
            };
            total += content.bytes.len() as u64;
            let name = resource
                .filename()
                .ok_or("Invalid firmware filename")?
                .to_owned();
            let mut image = make_image(
                name,
                content.bytes,
                images::local_image_provenance(&file.path),
                content.digest,
                true,
            );
            image.resource = Some(Arc::new(resource));
            if let Some(error) = image.validation_error.take() {
                return Err(error);
            }
            if image.parsed.is_none() {
                return Err(tr("firmware.error.esb_image_format"));
            }
            if let Some(error) = image.uf2_application_error.as_ref() {
                return Err(error.clone());
            }
            if let Some(catalog) = &catalog {
                image.hints = catalog.match_filename(&image.name).into();
            }
            Ok(image)
        });
        if exceeded_total {
            return Err(tr("firmware.batch.too_large"));
        }
        match read {
            Err(_) if cancel.load(Ordering::Acquire) => return Err("Cancelled".into()),
            Err(error) => file.error = Some(error),
            Ok(image) => {
                if image.hints.len() != 1 || image.hints[0].kind.is_none() {
                    file.unknown = true;
                } else {
                    let hint = &image.hints[0];
                    for (index, group) in groups.iter().enumerate() {
                        let first = &group.targets[0];
                        let kind = if first.id == esb_ota::RECEIVER_ID {
                            board_catalog::EsbOtaKind::Receiver
                        } else {
                            board_catalog::EsbOtaKind::Tracker
                        };
                        if hint.kind != Some(kind)
                            || board_catalog::canonical_board(&hint.board_target)
                                != board_catalog::canonical_board(&first.info.board_target)
                        {
                            continue;
                        }
                        let result = group.targets.iter().try_for_each(|target| {
                            validate_ota_image(&image, target, ConfirmationPolicy::Standard)?;
                            if planning::ota_compatibility(&image, target) != Compatibility::Matched
                            {
                                return Err(tr("firmware.error.board_role_mismatch"));
                            }
                            planning::validate_ota_catalog_mcu(&image, target, catalog.as_ref())
                        });
                        file.candidates.push((index, result));
                    }
                }
                file.image = Some(image);
            }
        }
        files.push(file);
    }
    if cancel.load(Ordering::Acquire) {
        return Err("Cancelled".into());
    }
    Ok(OtaBatchPrepared { groups, files })
}

impl Workbench {
    pub(super) fn flash_ota_batch_result(&self, cx: &App) -> AnyElement {
        let Some(result) = &self.flash.ota_batch_result else {
            return div().into_any_element();
        };
        let mut panel = flash_panel(cx)
            .id("ota-batch-results")
            .test_support()
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(tr("firmware.batch.title")),
            );
        for entry in &result.entries {
            let path = entry.path.display().to_string();
            let mut row = div()
                .id(SharedString::from(format!(
                    "ota-batch-file-{:?}",
                    entry.path.as_os_str()
                )))
                .test_support()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_1()
                .child(div().text_sm().whitespace_normal().child(path));
            for outcome in &entry.outcomes {
                let text = match outcome {
                    BatchOutcome::Assigned(group) => {
                        rust_i18n::t!("firmware.batch.assigned", group = group.label()).to_string()
                    }
                    BatchOutcome::Replaced(group) => {
                        rust_i18n::t!("firmware.batch.replaced", group = group.label()).to_string()
                    }
                    BatchOutcome::Conflict(group) => {
                        rust_i18n::t!("firmware.batch.conflict", group = group.label()).to_string()
                    }
                    BatchOutcome::Stale(group) => {
                        rust_i18n::t!("firmware.batch.stale", group = group.label()).to_string()
                    }
                    BatchOutcome::Invalid(group, error) => {
                        let reason = group.as_ref().map_or_else(
                            || error.clone(),
                            |group| format!("{}: {error}", group.label()),
                        );
                        rust_i18n::t!("firmware.batch.invalid", reason = reason).to_string()
                    }
                    BatchOutcome::Ambiguous(groups) => {
                        let groups = groups
                            .iter()
                            .map(BatchGroupDescriptor::label)
                            .collect::<Vec<_>>()
                            .join(" / ");
                        rust_i18n::t!("firmware.batch.ambiguous_groups", groups = groups)
                            .to_string()
                    }
                    BatchOutcome::Unknown => tr("firmware.batch.unknown"),
                    BatchOutcome::NoMatch => tr("firmware.batch.no_match"),
                };
                row = row.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .whitespace_normal()
                        .child(text),
                );
            }
            panel = panel.child(row);
        }
        panel.into_any_element()
    }
}
