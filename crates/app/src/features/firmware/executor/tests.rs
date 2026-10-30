use super::*;
use crate::features::firmware::esb_ota::{FirmwareInfo, RECEIVER_ID, Target};
use crate::update::{Job, OtaPreparation};
use std::collections::VecDeque;
use std::io::{Cursor, Write};

fn target(receiver: &str, id: u8) -> Target {
    Target {
        receiver: ReceiverEndpoint {
            key: receiver.into(),
            path: receiver.as_bytes().to_vec(),
            label: receiver.into(),
            serial_number: None,
        },
        id,
        address: (id != RECEIVER_ID).then(|| format!("1122334455{id:02X}")),
        info: FirmwareInfo {
            version: "1".into(),
            build_date: "today".into(),
            firmware_size: 4,
            bootloader: "none".into(),
            protocol_version: 1,
            board_target: "board".into(),
            flash_base: 0x1000,
        },
    }
}

fn groups(devices: &[Target]) -> Vec<EsbGroup> {
    let image = Arc::new(
        firmware::parse_image("firmware.hex", b":0410000001020304E2\n:00000001FF\n").unwrap(),
    );
    devices
        .iter()
        .map(|target| EsbGroup {
            targets: vec![target.clone()],
            image: Some(image.clone()),
        })
        .collect()
}

#[test]
fn cancelled_groups_report_every_target_and_complete_terminal_snapshots() {
    let mut snapshots = Vec::new();
    let completion = execute(
        UpdateRequest::EsbGroups(groups(&[target("a", 0), target("b", 0)])),
        &AtomicBool::new(true),
        &mut |progress| snapshots.push(progress),
    )
    .unwrap();
    let UpdateCompletion::Results(results) = completion else {
        panic!("expected per-target outcomes");
    };
    assert_eq!(results.len(), 2);
    assert!(
        results
            .iter()
            .all(|(_, result)| matches!(result, UpdateOutcome::Failed(_)))
    );
    for ((identity, _), expected) in results.iter().zip(["a", "b"]) {
        let ResultIdentity::Esb(target) = identity else {
            panic!("expected frozen ESB identity");
        };
        assert_eq!(target.receiver.key, expected);
    }
    assert!(snapshots.iter().all(|progress| progress.ota.len() == 2));
    let terminal = snapshots.last().unwrap();
    assert_eq!(terminal.total, 2);
    assert_eq!(terminal.completed, 0);
    assert!(
        terminal
            .ota
            .iter()
            .all(|target| target.phase == OtaPhase::Cancelled)
    );
}

#[test]
fn full_operation_snapshots_preserve_finished_groups_and_mixed_cancellation() {
    let devices = [target("a", 0), target("a", 1), target("b", 0)];
    let mut operation = queued_ota_progress(&groups(&devices));
    assert!(
        operation
            .iter()
            .all(|item| item.total == 0 && item.phase == OtaPhase::Queued)
    );
    let mut completed = operation[0].clone();
    completed.phase = OtaPhase::Completed;
    completed.completed = 100;
    completed.total = 100;
    merge_ota_progress(&mut operation, vec![completed]);
    let mut failed = operation[1].clone();
    failed.phase = OtaPhase::Failed;
    failed.error = Some("VERIFY rejected".into());
    merge_ota_progress(&mut operation, vec![failed]);
    finish_ota_target(
        &mut operation,
        &devices[1],
        &UpdateOutcome::Failed("VERIFY rejected; OTA abort failed: disconnected".into()),
        true,
    );
    let mut next_group = operation[2].clone();
    next_group.phase = OtaPhase::Preparing;
    next_group.preparation = Some(OtaPreparation::ReceiverRecovery);
    merge_ota_progress(&mut operation, vec![next_group]);
    assert_eq!(
        operation[1].error.as_deref(),
        Some("VERIFY rejected; OTA abort failed: disconnected")
    );
    assert_eq!(
        operation[2].preparation,
        Some(OtaPreparation::ReceiverRecovery)
    );
    finish_ota_target(
        &mut operation,
        &devices[2],
        &UpdateOutcome::Failed("cancelled".into()),
        true,
    );
    assert_eq!(operation[0].phase, OtaPhase::Completed);
    assert_eq!(operation[0].completed, 100);
    assert_eq!(operation[1].phase, OtaPhase::Failed);
    assert_eq!(operation[2].phase, OtaPhase::Cancelled);
    assert_eq!(operation[2].error.as_deref(), Some("cancelled"));
    assert_eq!(operation[2].preparation, None);
}

#[test]
fn progress_replaces_only_matching_receiver_and_clears_recovered_errors() {
    let devices = [target("a", 0), target("b", 0)];
    let mut operation = queued_ota_progress(&groups(&devices));
    operation[1].phase = OtaPhase::Cancelled;
    operation[1].error = Some("cancelled".into());
    let mut cleanup_failure = operation[0].clone();
    cleanup_failure.phase = OtaPhase::Failed;
    cleanup_failure.error = Some("CRC mismatch; OTA abort failed: disconnected".into());
    merge_ota_progress(&mut operation, vec![cleanup_failure]);
    assert_eq!(operation[0].phase, OtaPhase::Failed);
    assert_eq!(
        operation[0].error.as_deref(),
        Some("CRC mismatch; OTA abort failed: disconnected")
    );
    assert_eq!(operation[1].phase, OtaPhase::Cancelled);
    assert_eq!(operation[1].error.as_deref(), Some("cancelled"));
    finish_ota_target(
        &mut operation,
        &devices[0],
        &UpdateOutcome::Completed,
        false,
    );
    assert_eq!(operation[0].phase, OtaPhase::Completed);
    assert_eq!(operation[0].error, None);
    assert_eq!(operation[1].phase, OtaPhase::Cancelled);
}

#[test]
fn running_query_cancellation_joins_without_synthesizing_device_failures() {
    let ended = Arc::new(AtomicBool::new(false));
    let worker_ended = ended.clone();
    let receiver = target("a", 1).receiver;
    let tracker = TrackerLink {
        receiver_key: "a".into(),
        slot: 1,
        address: Some("AABBCCDDEE01".into()),
        online: true,
    };
    let mut job = Job::start(move |cancel, progress| {
        while !cancel.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
        let result = execute(
            UpdateRequest::QueryGroups(vec![(receiver, vec![tracker], true)]),
            cancel,
            progress,
        );
        worker_ended.store(true, Ordering::Release);
        result
    })
    .unwrap();
    job.cancel();
    let result = loop {
        if let Some(result) = job.poll() {
            break result;
        }
        std::thread::yield_now();
    };
    let UpdateCompletion::Queries(query) = result.unwrap() else {
        panic!("expected query completion");
    };
    assert!(ended.load(Ordering::Acquire));
    assert!(query.targets.is_empty());
    assert!(query.failures.is_empty());
}

#[test]
fn cancelled_async_ble_sequence_reports_each_frozen_device_without_hardware() {
    let devices = ["one", "two"]
        .map(|id| ble_ota::BleDevice {
            id: id.into(),
            adapter_id: "adapter".into(),
            name: format!("DFU {id}"),
            address: format!("address-{id}"),
        })
        .to_vec();
    let bytes: Arc<[u8]> = Arc::from([1, 2, 3, 4]);
    let image = FileImage {
        name: "legacy.zip".into(),
        bytes: Arc::clone(&bytes),
    };
    let cancel = AtomicBool::new(true);
    let mut progress = |_| panic!("cancelled operation must not reach hardware discovery");
    let backend = ble_ota::Backend::default();
    let operation = execute_ble(&backend, devices, image, &cancel, &mut progress);
    fn require_send<T: Send>(future: T) -> T {
        future
    }
    let completion = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(require_send(operation))
        .unwrap();
    let UpdateCompletion::Results(results) = completion else {
        panic!("expected BLE outcomes");
    };
    assert_eq!(results.len(), 2);
    for ((identity, result), expected) in results.into_iter().zip(["one", "two"]) {
        let ResultIdentity::Ble(device) = identity else {
            panic!("expected frozen BLE identity");
        };
        assert_eq!(device.id, expected);
        assert_eq!(device.adapter_id, "adapter");
        let UpdateOutcome::Failed(error) = result else {
            panic!("expected cancelled BLE failure");
        };
        assert!(error.contains("cancelled"));
    }
    assert_eq!(Arc::strong_count(&bytes), 1);
}

fn ble_devices() -> Vec<ble_ota::BleDevice> {
    ["one", "two", "three"]
        .map(|id| ble_ota::BleDevice {
            id: id.into(),
            adapter_id: "adapter".into(),
            name: format!("DFU {id}"),
            address: format!("address-{id}"),
        })
        .to_vec()
}

fn legacy_ble_image() -> FileImage {
    let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default();
    for (name, bytes) in [
        (
            "manifest.json",
            br#"{"manifest":{"application":{"bin_file":"application.bin","dat_file":"application.dat"}}}"#.as_slice(),
        ),
        ("application.bin", &[1, 2, 3, 4]),
        (
            "application.dat",
            &[0x52, 0, 0xff, 0xff, 1, 0, 0, 0, 1, 0, 0xfe, 0xff, 0, 0],
        ),
    ] {
        archive.start_file(name, options).unwrap();
        archive.write_all(bytes).unwrap();
    }
    FileImage {
        name: "legacy.zip".into(),
        bytes: Arc::from(archive.finish().unwrap().into_inner()),
    }
}

struct BleTransfer {
    device: String,
    package: usize,
    image_storage: usize,
    init_storage: usize,
    image: Vec<u8>,
    init: Vec<u8>,
}

#[derive(Default)]
struct BleBatchScript {
    preparations: usize,
    preparation_error: Option<String>,
    events: Vec<String>,
    transfers: Vec<BleTransfer>,
    outcomes: VecDeque<Result<(), String>>,
}

impl BleBatchTransport for &mut BleBatchScript {
    async fn prepare(
        &mut self,
        image: FileImage,
        cancel: &AtomicBool,
    ) -> Result<legacy_dfu::Package, String> {
        self.preparations += 1;
        let result = ble_ota::prepare(image.name, image.bytes, cancel).await;
        self.preparation_error = result.as_ref().err().cloned();
        result
    }

    async fn flash(
        &mut self,
        device: &ble_ota::BleDevice,
        package: &legacy_dfu::Package,
        _: &AtomicBool,
        _: &mut (dyn FnMut(Progress) + Send),
    ) -> Result<(), String> {
        self.events.push(format!("start {}", device.id));
        tokio::task::yield_now().await;
        let (image, init) = package.payloads();
        self.transfers.push(BleTransfer {
            device: device.id.clone(),
            package: std::ptr::from_ref(package) as usize,
            image_storage: image.as_ptr() as usize,
            init_storage: init.as_ptr() as usize,
            image: image.to_vec(),
            init: init.to_vec(),
        });
        let outcome = self.outcomes.pop_front().expect("unexpected BLE target");
        self.events.push(format!("finish {}", device.id));
        outcome
    }
}

#[test]
fn ble_batch_parses_once_and_borrows_one_package_for_independent_sequential_targets() {
    let mut script = BleBatchScript {
        outcomes: [Ok(()), Err("scripted transfer rejected".into()), Ok(())].into(),
        ..Default::default()
    };
    let completion = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(execute_ble_with(
            &mut script,
            ble_devices(),
            legacy_ble_image(),
            &AtomicBool::new(false),
            &mut |_| {},
        ))
        .unwrap();
    let UpdateCompletion::Results(results) = completion else {
        panic!("expected per-target BLE outcomes");
    };
    let results = results
        .into_iter()
        .map(|(identity, outcome)| {
            let ResultIdentity::Ble(device) = identity else {
                panic!("expected frozen BLE identity");
            };
            (device.id, outcome)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        results,
        vec![
            ("one".into(), UpdateOutcome::Completed),
            (
                "two".into(),
                UpdateOutcome::Failed("scripted transfer rejected".into())
            ),
            ("three".into(), UpdateOutcome::Completed),
        ]
    );
    assert_eq!(script.preparations, 1);
    assert!(script.preparation_error.is_none());
    assert_eq!(
        script.events,
        [
            "start one",
            "finish one",
            "start two",
            "finish two",
            "start three",
            "finish three",
        ]
    );
    assert_eq!(script.transfers.len(), 3);
    let first = &script.transfers[0];
    for (transfer, expected) in script.transfers.iter().zip(["one", "two", "three"]) {
        assert_eq!(transfer.device, expected);
        assert_eq!(transfer.image, [1, 2, 3, 4]);
        assert_eq!(
            transfer.init,
            [0x52, 0, 0xff, 0xff, 1, 0, 0, 0, 1, 0, 0xfe, 0xff, 0, 0]
        );
        assert_eq!(transfer.package, first.package);
        assert_eq!(transfer.image_storage, first.image_storage);
        assert_eq!(transfer.init_storage, first.init_storage);
    }
}

#[test]
fn ble_package_failure_preserves_every_frozen_target_without_starting_transfers() {
    let mut script = BleBatchScript::default();
    let completion = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(execute_ble_with(
            &mut script,
            ble_devices(),
            FileImage {
                name: "broken.zip".into(),
                bytes: Arc::from([1, 2, 3, 4]),
            },
            &AtomicBool::new(false),
            &mut |_| panic!("invalid package must not reach device discovery"),
        ))
        .unwrap();
    let UpdateCompletion::Results(results) = completion else {
        panic!("expected per-target BLE outcomes");
    };
    assert_eq!(script.preparations, 1);
    let error = script.preparation_error.unwrap();
    assert_eq!(results.len(), 3);
    for ((identity, outcome), expected) in results.into_iter().zip(["one", "two", "three"]) {
        let ResultIdentity::Ble(device) = identity else {
            panic!("expected frozen BLE identity");
        };
        assert_eq!(device.id, expected);
        assert_eq!(device.adapter_id, "adapter");
        assert_eq!(outcome, UpdateOutcome::Failed(error.clone()));
    }
    assert!(script.transfers.is_empty());
    assert!(script.events.is_empty());
}

#[test]
fn empty_ble_batch_does_not_parse_a_package_or_start_a_transfer() {
    let mut script = BleBatchScript::default();
    let completion = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(execute_ble_with(
            &mut script,
            Vec::new(),
            legacy_ble_image(),
            &AtomicBool::new(false),
            &mut |_| panic!("empty batch must not reach device discovery"),
        ))
        .unwrap();
    let UpdateCompletion::Results(results) = completion else {
        panic!("expected per-target BLE outcomes");
    };
    assert!(results.is_empty());
    assert_eq!(script.preparations, 0);
    assert!(script.transfers.is_empty());
    assert!(script.events.is_empty());
}

#[test]
fn receiver_activation_finalization_preserves_uncertainty_after_cancellation() {
    use crate::update::{ActivationSubmission, ReceiverActivationReport, ReceiverAvailability};
    let devices = [target("receiver", RECEIVER_ID), target("receiver", 1)];
    let mut operation = queued_ota_progress(&groups(&devices));
    operation[0].phase = OtaPhase::Activating;
    operation[0].preparation = Some(OtaPreparation::StartingUpdate);
    operation[0].bytes_per_second = Some(4000);
    operation[0].eta_seconds = Some(1);
    let outcome = UpdateOutcome::ActivationUnconfirmed(ReceiverActivationReport {
        submission: ActivationSubmission::Accepted,
        availability: ReceiverAvailability::Responding,
    });
    finish_ota_target(&mut operation, &devices[0], &outcome, true);
    assert_eq!(operation[0].phase, OtaPhase::ActivationUnconfirmed);
    assert_eq!(operation[0].preparation, None);
    assert_eq!(operation[0].error, None);
    assert_eq!(operation[0].bytes_per_second, None);
    assert_eq!(operation[0].eta_seconds, None);
    finish_ota_target(
        &mut operation,
        &devices[0],
        &UpdateOutcome::Failed("cancelled".into()),
        true,
    );
    assert_eq!(operation[0].phase, OtaPhase::ActivationUnconfirmed);
    assert_eq!(operation[0].error, None);
    finish_ota_target(&mut operation, &devices[1], &UpdateOutcome::Completed, true);
    assert_eq!(operation[1].phase, OtaPhase::Completed);
    finish_ota_target(
        &mut operation,
        &devices[0],
        &UpdateOutcome::Completed,
        false,
    );
    assert_eq!(operation[0].phase, OtaPhase::Completed);
}

#[test]
fn transport_result_boundary_keeps_completion_and_exact_failure() {
    assert_eq!(transport_outcome(Ok(())), UpdateOutcome::Completed);
    assert_eq!(
        transport_outcome(Err("transfer rejected".into())),
        UpdateOutcome::Failed("transfer rejected".into())
    );
}

#[test]
fn uf2_result_boundary_preserves_pending_evidence_without_an_ota_phase() {
    use crate::update::{Uf2CopyReport, Uf2FinalizationStage};
    let report = Uf2CopyReport {
        completed: 102400,
        total: 102400,
        stage: Uf2FinalizationStage::SyncAll,
        os_error: Some(433),
        diagnostic: "device disappeared (os error 433)".into(),
    };
    assert_eq!(
        uf2_outcome(Ok(uf2::CopyOutcome::Completed)),
        UpdateOutcome::Completed
    );
    let pending = uf2_outcome(Ok(uf2::CopyOutcome::ConfirmationPending(report.clone())));
    assert_eq!(pending, UpdateOutcome::Uf2CopyUnconfirmed(report));
    assert_eq!(
        uf2_outcome(Err(
            "UF2 copy failed after 31/512 bytes: disconnected".into()
        )),
        UpdateOutcome::Failed("UF2 copy failed after 31/512 bytes: disconnected".into()),
    );
    let device = target("receiver", 0);
    let mut operation = queued_ota_progress(&groups(std::slice::from_ref(&device)));
    operation[0].phase = OtaPhase::Transferring;
    operation[0].completed = 23;
    operation[0].total = 100;
    operation[0].bytes_per_second = Some(12);
    finish_ota_target(&mut operation, &device, &pending, false);
    assert_eq!(operation[0].phase, OtaPhase::Transferring);
    assert_eq!(operation[0].completed, 23);
    assert_eq!(operation[0].bytes_per_second, Some(12));
}

fn serial_endpoint() -> SerialEndpoint {
    SerialEndpoint {
        port_name: "serial-fixture".into(),
        label: "Synthetic console".into(),
        manufacturer: None,
        product: None,
        kind: crate::domain::SerialPortKind::Usb,
        serial_number: Some("SYNTHETIC-RECEIVER".into()),
        vid: Some(0x1209),
        pid: Some(0x7690),
        usb_interface: Some(crate::domain::UsbInterface {
            number: Some(0),
            description: crate::domain::InterfaceDescription::Unavailable,
        }),
    }
}

#[test]
fn serial_revalidation_keeps_frozen_display_usb_and_interface_identity() {
    let expected = serial_endpoint();
    let mut inventory = Inventory {
        ports: vec![expected.clone()],
        ..Inventory::default()
    };
    revalidate_serial_inventory(&expected, &inventory).unwrap();
    inventory.ports[0].usb_interface.as_mut().unwrap().number = Some(2);
    assert!(revalidate_serial_inventory(&expected, &inventory).is_err());
    inventory.ports[0] = expected.clone();
    inventory.ports[0].label = "Changed display".into();
    assert!(revalidate_serial_inventory(&expected, &inventory).is_err());
    inventory.ports[0] = expected.clone();
    inventory.ports[0].serial_number = Some("REPLACEMENT".into());
    assert!(revalidate_serial_inventory(&expected, &inventory).is_err());
    inventory.ports[0] = expected.clone();
    inventory.ports[0].pid = Some(1);
    assert!(revalidate_serial_inventory(&expected, &inventory).is_err());
    inventory.ports.clear();
    assert!(revalidate_serial_inventory(&expected, &inventory).is_err());
}

#[test]
fn serial_revalidation_rejects_raw_and_new_contextual_console_ambiguity() {
    use crate::domain::{InterfaceDescription, UsbInterface};
    let expected = serial_endpoint();
    let mut receiver = target("receiver", RECEIVER_ID).receiver;
    receiver.serial_number = expected.serial_number.clone();
    let mut raw = expected.clone();
    raw.port_name = "raw-fixture".into();
    raw.usb_interface = Some(UsbInterface {
        number: Some(2),
        description: InterfaceDescription::Value("SlimeNRF Raw Data CDC".into()),
    });
    let mut inventory = Inventory {
        ports: vec![expected.clone(), raw.clone()],
        receivers: vec![receiver],
        ..Inventory::default()
    };
    revalidate_serial_inventory(&expected, &inventory).unwrap();
    assert!(revalidate_serial_inventory(&raw, &inventory).is_err());
    let mut unknown = expected.clone();
    unknown.port_name = "unknown-fixture".into();
    unknown.usb_interface.as_mut().unwrap().number = Some(4);
    inventory.ports.push(unknown);
    assert!(revalidate_serial_inventory(&expected, &inventory).is_err());
    inventory.ports.pop();
    inventory.serial_enumeration_failed = true;
    assert!(revalidate_serial_inventory(&expected, &inventory).is_err());
}
