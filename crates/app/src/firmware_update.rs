//! Execution of admitted, frozen firmware operations.
//!
//! The caller owns admission, reservations, write draining and worker lifetime.
//! Requests share the already-loaded image storage; completions contain device
//! identities and outcomes, never UI presentation or query scheduling metadata.

use crate::{
    ble_ota, discovery,
    domain::{Inventory, ReceiverEndpoint, SerialEndpoint, TrackerLink, console_port_allowed},
    esb_ota, opendfu, uf2,
    update::{OtaPhase, OtaTargetProgress, Progress, UpdateOutcome},
};
use slimenrf_firmware_image::{
    image::{self as firmware, Uf2ImagePurpose},
    legacy_dfu, secure_dfu,
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

pub(crate) struct FileImage {
    pub name: String,
    pub bytes: Arc<[u8]>,
}

pub(crate) struct EsbGroup {
    pub targets: Vec<esb_ota::Target>,
    pub image: Option<Arc<firmware::FirmwareImage>>,
}

pub(crate) enum UpdateRequest {
    Uf2(Vec<uf2::Uf2Volume>, FileImage, Uf2ImagePurpose),
    Reboot(Vec<uf2::Uf2Volume>),
    Enter(SerialEndpoint),
    OpenDfu(SerialEndpoint, FileImage, secure_dfu::Options),
    QueryGroups(Vec<(ReceiverEndpoint, Vec<TrackerLink>, bool)>),
    EsbGroups(Vec<EsbGroup>),
}

pub(crate) enum ExecutionRequest {
    Blocking(UpdateRequest),
    Ble(Vec<ble_ota::BleDevice>, FileImage),
}

pub(crate) enum ResultIdentity {
    Uf2(uf2::Uf2Volume),
    Reboot(uf2::Uf2Volume),
    Serial(String),
    Esb(esb_ota::Target),
    Ble(ble_ota::BleDevice),
}

pub(crate) struct QueryFailure {
    pub receiver_key: String,
    pub id: u8,
    pub error: String,
}

impl QueryFailure {
    fn new(receiver: &ReceiverEndpoint, id: u8, error: String) -> Self {
        Self {
            receiver_key: receiver.key.clone(),
            id,
            error,
        }
    }
}

pub(crate) struct QueryResults {
    pub targets: Vec<esb_ota::Target>,
    pub failures: Vec<QueryFailure>,
}

pub(crate) enum UpdateCompletion {
    Queries(QueryResults),
    Results(Vec<(ResultIdentity, UpdateOutcome)>),
}

pub(crate) fn execute(
    request: UpdateRequest,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(Progress),
) -> Result<UpdateCompletion, String> {
    let mut results = Vec::new();
    match request {
        UpdateRequest::Uf2(volumes, image, purpose) => {
            for volume in volumes {
                let result = uf2::write(
                    &volume,
                    &image.name,
                    &image.bytes,
                    purpose,
                    cancel,
                    progress,
                );
                results.push((ResultIdentity::Uf2(volume), uf2_outcome(result)));
            }
        }
        UpdateRequest::Reboot(volumes) => {
            for volume in volumes {
                let result = uf2::reboot(&volume, cancel, progress);
                results.push((ResultIdentity::Reboot(volume), uf2_outcome(result)));
            }
        }
        UpdateRequest::Enter(port) => {
            revalidate_serial(&port)?;
            let result = opendfu::enter_dfu(&port.port_name, cancel, progress);
            results.push((
                ResultIdentity::Serial(port.port_name),
                transport_outcome(result),
            ));
        }
        UpdateRequest::OpenDfu(port, image, options) => {
            revalidate_serial(&port)?;
            let result = opendfu::flash_with_options(
                &port,
                &image.name,
                &image.bytes,
                &options,
                cancel,
                progress,
            );
            results.push((
                ResultIdentity::Serial(port.port_name),
                transport_outcome(result),
            ));
        }
        UpdateRequest::QueryGroups(groups) => {
            let mut queries = QueryResults {
                targets: Vec::new(),
                failures: Vec::new(),
            };
            for (receiver, trackers, include_receiver) in groups {
                if cancel.load(Ordering::Acquire) {
                    break;
                }
                match esb_ota::query(&receiver, &trackers, include_receiver, cancel, progress) {
                    Ok(query) => {
                        queries.targets.extend(query.receiver);
                        queries.targets.extend(query.trackers);
                        queries.failures.extend(
                            query
                                .failures
                                .into_iter()
                                .map(|(slot, error)| QueryFailure::new(&receiver, slot, error)),
                        );
                    }
                    Err(_) if cancel.load(Ordering::Acquire) => break,
                    Err(error) => {
                        if include_receiver {
                            queries.failures.push(QueryFailure::new(
                                &receiver,
                                esb_ota::RECEIVER_ID,
                                error.clone(),
                            ));
                        }
                        queries.failures.extend(trackers.iter().map(|tracker| {
                            QueryFailure::new(&receiver, tracker.slot, error.clone())
                        }));
                    }
                }
            }
            return Ok(UpdateCompletion::Queries(queries));
        }
        UpdateRequest::EsbGroups(groups) => {
            let mut operation = queued_ota_progress(&groups);
            let mut initial = Progress::stage("");
            initial.total = operation.len() as u64;
            initial.ota = operation.clone();
            progress(initial);
            let mut plans = Vec::new();
            for group in &groups {
                match group.image.as_deref() {
                    Some(image) => plans.push(esb_ota::FlashGroup {
                        targets: &group.targets,
                        image,
                        board: group
                            .targets
                            .first()
                            .ok_or("Select a target")?
                            .info
                            .board_target
                            .as_str(),
                    }),
                    None => results.extend(group.targets.iter().map(|target| {
                        (
                            ResultIdentity::Esb(target.clone()),
                            UpdateOutcome::Failed(
                                "ESB requires UF2, HEX or MCUboot BIN firmware".to_string(),
                            ),
                        )
                    })),
                }
            }
            if !plans.is_empty() {
                match esb_ota::flash_groups(&plans, cancel, &mut |mut update| {
                    merge_ota_progress(&mut operation, std::mem::take(&mut update.ota));
                    update.ota = operation.clone();
                    update.completed = operation
                        .iter()
                        .filter(|target| target.phase == OtaPhase::Completed)
                        .count() as u64;
                    update.total = operation.len() as u64;
                    progress(update);
                }) {
                    Ok(outcomes) => results.extend(
                        outcomes
                            .into_iter()
                            .map(|outcome| (ResultIdentity::Esb(outcome.target), outcome.outcome)),
                    ),
                    Err(error) => {
                        results.extend(plans.iter().flat_map(|plan| plan.targets).map(|target| {
                            (
                                ResultIdentity::Esb(target.clone()),
                                UpdateOutcome::Failed(error.clone()),
                            )
                        }))
                    }
                }
            }
            for (target, result) in &results {
                if let ResultIdentity::Esb(target) = target {
                    finish_ota_target(
                        &mut operation,
                        target,
                        result,
                        cancel.load(Ordering::Acquire),
                    );
                }
            }
            let mut update = Progress::stage("");
            update.completed = operation
                .iter()
                .filter(|target| target.phase == OtaPhase::Completed)
                .count() as u64;
            update.total = operation.len() as u64;
            update.ota = operation;
            progress(update);
        }
    }
    Ok(UpdateCompletion::Results(results))
}

pub(crate) async fn execute_ble(
    backend: &ble_ota::Backend,
    devices: Vec<ble_ota::BleDevice>,
    image: FileImage,
    cancel: &AtomicBool,
    progress: &mut (dyn FnMut(Progress) + Send),
) -> Result<UpdateCompletion, String> {
    execute_ble_with(backend, devices, image, cancel, progress).await
}

trait BleBatchTransport {
    fn prepare(
        &mut self,
        image: FileImage,
        cancel: &AtomicBool,
    ) -> impl Future<Output = Result<legacy_dfu::Package, String>> + Send;

    fn flash(
        &mut self,
        device: &ble_ota::BleDevice,
        package: &legacy_dfu::Package,
        cancel: &AtomicBool,
        progress: &mut (dyn FnMut(Progress) + Send),
    ) -> impl Future<Output = Result<(), String>> + Send;
}

impl BleBatchTransport for &ble_ota::Backend {
    fn prepare(
        &mut self,
        image: FileImage,
        cancel: &AtomicBool,
    ) -> impl Future<Output = Result<legacy_dfu::Package, String>> + Send {
        ble_ota::prepare(image.name, image.bytes, cancel)
    }

    fn flash(
        &mut self,
        device: &ble_ota::BleDevice,
        package: &legacy_dfu::Package,
        cancel: &AtomicBool,
        progress: &mut (dyn FnMut(Progress) + Send),
    ) -> impl Future<Output = Result<(), String>> + Send {
        ble_ota::flash(self, device, package, cancel, progress)
    }
}

async fn execute_ble_with(
    mut transport: impl BleBatchTransport + Send,
    devices: Vec<ble_ota::BleDevice>,
    image: FileImage,
    cancel: &AtomicBool,
    progress: &mut (dyn FnMut(Progress) + Send),
) -> Result<UpdateCompletion, String> {
    let mut results = Vec::with_capacity(devices.len());
    if devices.is_empty() {
        return Ok(UpdateCompletion::Results(results));
    }
    let package = transport.prepare(image, cancel).await;
    for device in devices {
        let result = match &package {
            Ok(package) => transport.flash(&device, package, cancel, progress).await,
            Err(error) => Err(error.clone()),
        };
        results.push((ResultIdentity::Ble(device), transport_outcome(result)));
    }
    Ok(UpdateCompletion::Results(results))
}

fn transport_outcome(result: Result<(), String>) -> UpdateOutcome {
    match result {
        Ok(()) => UpdateOutcome::Completed,
        Err(error) => UpdateOutcome::Failed(error),
    }
}

fn uf2_outcome(result: Result<uf2::CopyOutcome, String>) -> UpdateOutcome {
    match result {
        Ok(uf2::CopyOutcome::Completed) => UpdateOutcome::Completed,
        Ok(uf2::CopyOutcome::ConfirmationPending(report)) => {
            UpdateOutcome::Uf2CopyUnconfirmed(report)
        }
        Err(error) => UpdateOutcome::Failed(error),
    }
}

fn revalidate_serial(expected: &SerialEndpoint) -> Result<(), String> {
    revalidate_serial_inventory(expected, &discovery::scan()?)
}

fn revalidate_serial_inventory(
    expected: &SerialEndpoint,
    inventory: &Inventory,
) -> Result<(), String> {
    let current = inventory
        .ports
        .iter()
        .find(|port| port.port_name == expected.port_name)
        .ok_or_else(|| {
            format!(
                "{} is no longer connected; refresh and select the bootloader again",
                expected.port_name
            )
        })?;
    if current.serial_number != expected.serial_number
        || current.vid != expected.vid
        || current.pid != expected.pid
        || current.label != expected.label
        || !current.same_usb_interface(expected)
    {
        return Err("Serial device identity changed; refresh and select the device again".into());
    }
    if !console_port_allowed(current, inventory) {
        return Err("Serial interface is not an available command console; refresh and select the device again".into());
    }
    Ok(())
}

fn queued_ota_progress(groups: &[EsbGroup]) -> Vec<OtaTargetProgress> {
    groups
        .iter()
        .flat_map(|group| &group.targets)
        .map(|target| OtaTargetProgress {
            receiver_key: target.receiver.key.clone(),
            id: target.id,
            phase: OtaPhase::Queued,
            preparation: None,
            error: None,
            completed: 0,
            total: 0,
            bytes_per_second: None,
            eta_seconds: None,
        })
        .collect()
}

fn merge_ota_progress(operation: &mut [OtaTargetProgress], incoming: Vec<OtaTargetProgress>) {
    for next in incoming {
        if let Some(current) = operation
            .iter_mut()
            .find(|current| current.receiver_key == next.receiver_key && current.id == next.id)
        {
            *current = next;
        }
    }
}

fn finish_ota_target(
    operation: &mut [OtaTargetProgress],
    target: &esb_ota::Target,
    result: &UpdateOutcome,
    cancelled: bool,
) {
    if let Some(current) = operation
        .iter_mut()
        .find(|current| current.receiver_key == target.receiver.key && current.id == target.id)
    {
        current.phase = match result {
            UpdateOutcome::Completed => OtaPhase::Completed,
            UpdateOutcome::ActivationUnconfirmed(_) => OtaPhase::ActivationUnconfirmed,
            // UF2 finalization has no corresponding OTA protocol phase.
            UpdateOutcome::Uf2CopyUnconfirmed(_) => return,
            UpdateOutcome::Failed(_) if current.phase == OtaPhase::ActivationUnconfirmed => {
                return;
            }
            UpdateOutcome::Failed(_)
                if current.phase == OtaPhase::Cancelled
                    || (cancelled && current.phase != OtaPhase::Failed) =>
            {
                OtaPhase::Cancelled
            }
            UpdateOutcome::Failed(_) => OtaPhase::Failed,
        };
        current.preparation = None;
        current.error = match result {
            UpdateOutcome::Failed(error) => Some(error.clone()),
            UpdateOutcome::Uf2CopyUnconfirmed(_) => return,
            UpdateOutcome::Completed | UpdateOutcome::ActivationUnconfirmed(_) => None,
        };
        current.bytes_per_second = None;
        current.eta_seconds = None;
    }
}

#[cfg(test)]
mod tests;
