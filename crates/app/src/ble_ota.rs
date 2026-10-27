//! Application-only Adafruit/Nordic Legacy BLE DFU on the managed async executor.
use std::{
    collections::HashSet,
    future::Future,
    io::{Cursor, Read},
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use btleplug::{
    api::{
        Central, CharPropFlags, Characteristic, Manager as _, Peripheral as _, ScanFilter,
        ValueNotification, WriteType,
    },
    platform::{Adapter, Manager, Peripheral},
};
use futures_util::{Stream, StreamExt};
use serde_json::Value;
use tokio::time::{Instant, timeout};
use uuid::Uuid;

use crate::update::Progress;

/// One native backend per application. On Linux btleplug's manager starts a
/// persistent D-Bus driver, so constructing one per operation leaks drivers until
/// runtime shutdown even after the manager itself is dropped.
#[derive(Default)]
pub(crate) struct Backend {
    manager: tokio::sync::OnceCell<Manager>,
}

impl Backend {
    async fn manager(&self, cancel: &AtomicBool) -> Result<&Manager, String> {
        bounded(cancel, IO_TIMEOUT, "Open Bluetooth manager", async {
            self.manager
                .get_or_try_init(|| async {
                    // Locked desktop backends fail before creating a driver or
                    // return success in the same poll as driver creation. OnceCell
                    // publishes that success before another cancellation tick.
                    Manager::new()
                        .await
                        .map_err(|error| format!("Open Bluetooth manager: {error}"))
                })
                .await
        })
        .await
    }
}

const SERVICE: Uuid = Uuid::from_u128(0x00001530_1212_efde_1523_785feabcd123);
const CONTROL: Uuid = Uuid::from_u128(0x00001531_1212_efde_1523_785feabcd123);
const PACKET: Uuid = Uuid::from_u128(0x00001532_1212_efde_1523_785feabcd123);
const IO_TIMEOUT: Duration = Duration::from_secs(20);
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(3);
const SCAN_TIME: Duration = Duration::from_secs(6);
const PRN: usize = 6;
const MAX_IMAGE: usize = 1024 * 1024;
const MAX_ZIP: usize = 4 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct BleDevice {
    /// Native peripheral identity (CoreBluetooth UUID on macOS, address elsewhere).
    pub id: String,
    pub adapter_id: String,
    pub name: String,
    pub address: String,
}

#[derive(Debug)]
pub(crate) struct Package {
    image: Vec<u8>,
    init: Vec<u8>,
}

#[cfg(test)]
impl Package {
    pub(crate) fn payloads(&self) -> (&[u8], &[u8]) {
        (&self.image, &self.init)
    }
}

fn safe_member(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name.contains(['\\', ':', '\0'])
        || name
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == "..")
    {
        return Err(format!("Unsafe DFU ZIP member: {name:?}"));
    }
    Ok(())
}

fn member(
    archive: &mut zip::ZipArchive<Cursor<&[u8]>>,
    name: &str,
    limit: usize,
) -> Result<Vec<u8>, String> {
    safe_member(name)?;
    let mut file = archive
        .by_name(name)
        .map_err(|e| format!("DFU ZIP {name}: {e}"))?;
    if file.size() == 0 || file.size() > limit as u64 {
        return Err(format!("DFU ZIP {name} is empty or exceeds {limit} bytes"));
    }
    let size = file.size();
    let mut bytes = Vec::with_capacity(size as usize);
    (&mut file)
        .take(size + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("DFU ZIP {name}: {e}"))?;
    if bytes.len() as u64 != size {
        return Err(format!("DFU ZIP {name} has an inconsistent size"));
    }
    Ok(bytes)
}

fn parse_package(name: &str, bytes: &[u8]) -> Result<Package, String> {
    if !name.to_ascii_lowercase().ends_with(".zip") || bytes.len() > MAX_ZIP {
        return Err("BLE OTA requires a Legacy DFU application ZIP (maximum 4 MiB)".into());
    }
    let mut archive = crate::firmware::zip_envelope::open(
        bytes,
        crate::firmware::zip_envelope::Limits {
            bytes: MAX_ZIP,
            members: 64,
            directory: MAX_ZIP,
        },
    )?;
    let mut names = HashSet::new();
    for i in 0..archive.len() {
        let file = archive
            .by_index(i)
            .map_err(|e| format!("Invalid DFU ZIP: {e}"))?;
        let name = file
            .name()
            .map_err(|e| format!("Cannot decode DFU ZIP member name: {e}"))?;
        safe_member(name.trim_end_matches('/'))?;
        if !names.insert(name.as_ref().to_owned()) {
            return Err(format!("Duplicate DFU ZIP member: {name}"));
        }
        if file
            .unix_mode()
            .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            return Err("DFU ZIP symbolic links are not supported".into());
        }
    }
    let manifest: Value = serde_json::from_slice(&member(&mut archive, "manifest.json", 65536)?)
        .map_err(|e| format!("Invalid DFU manifest: {e}"))?;
    let root = manifest
        .get("manifest")
        .and_then(Value::as_object)
        .ok_or("Not a Legacy DFU manifest; Secure DFU/MCUboot packages are not supported")?;
    if root
        .iter()
        .any(|(key, value)| key != "application" && !value.is_null())
    {
        return Err("BLE OTA accepts application-only Legacy DFU packages, not bootloader/SoftDevice updates".into());
    }
    let entry = root
        .get("application")
        .and_then(Value::as_object)
        .ok_or("Legacy DFU ZIP has no application entry")?;
    let bin = entry
        .get("bin_file")
        .and_then(Value::as_str)
        .ok_or("DFU application is missing bin_file")?;
    let dat = entry
        .get("dat_file")
        .and_then(Value::as_str)
        .ok_or("DFU application is missing dat_file")?;
    if bin == dat
        || !bin.to_ascii_lowercase().ends_with(".bin")
        || !dat.to_ascii_lowercase().ends_with(".dat")
    {
        return Err("Legacy DFU requires separate .bin and .dat members".into());
    }
    let image = member(&mut archive, bin, MAX_IMAGE)?;
    let init = member(&mut archive, dat, 1024)?;
    if image.len() % 4 != 0 {
        return Err("Legacy DFU firmware must be 4-byte aligned".into());
    }
    if image.starts_with(&[0x3d, 0xb8, 0xf3, 0x96]) {
        return Err("MCUboot images cannot be sent over Legacy BLE DFU".into());
    }
    // Secure DFU .dat is protobuf, not this fixed little-endian SDK11 header.
    if init.len() < 12 || init[..2] != [0x52, 0] {
        return Err("Not an Adafruit Legacy init packet (device_type must be 0x0052); Secure DFU is not supported".into());
    }
    let count = u16::from_le_bytes([init[8], init[9]]) as usize;
    let extension = init
        .get(10 + count * 2..)
        .ok_or("Truncated Legacy SoftDevice requirements")?;
    let valid_extension = match extension.len() {
        2 => true,                           // DFU 0.5 CRC16
        6 => extension[..4] == [0, 0, 0, 0], // DFU 0.6 CRC16
        40 | 104 => {
            let kind = if extension.len() == 40 { 1 } else { 2 };
            extension[..4] == [kind, 0, 0, 0]
                && u32::from_le_bytes(extension[4..8].try_into().unwrap()) as usize == image.len()
        }
        _ => false,
    };
    if !valid_extension {
        return Err(
            "Unsupported or malformed Legacy init packet; Secure DFU is not supported".into(),
        );
    }
    Ok(Package { image, init })
}

fn cancelled(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) {
        Err("BLE OTA cancelled".into())
    } else {
        Ok(())
    }
}

// Retain the same future across cancellation ticks; never replay a timed-out write.
async fn bounded<T>(
    cancel: &AtomicBool,
    duration: Duration,
    label: &str,
    future: impl Future<Output = Result<T, String>>,
) -> Result<T, String> {
    cancelled(cancel)?;
    let deadline = Instant::now() + duration;
    let mut future = std::pin::pin!(future);
    loop {
        cancelled(cancel)?;
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(format!("Timed out: {label}"));
        }
        if let Ok(result) = timeout(remaining.min(Duration::from_millis(100)), &mut future).await {
            return result;
        }
    }
}

async fn ble<T>(
    cancel: &AtomicBool,
    label: &str,
    future: impl Future<Output = btleplug::Result<T>>,
) -> Result<T, String> {
    bounded(cancel, IO_TIMEOUT, label, async {
        future.await.map_err(|e| format!("{label}: {e}"))
    })
    .await
}

fn report(
    progress: &mut (dyn FnMut(Progress) + Send),
    message: impl Into<String>,
    completed: usize,
    total: usize,
) {
    progress(Progress::bytes(message, completed as u64, total as u64));
}

async fn adapters(backend: &Backend, cancel: &AtomicBool) -> Result<Vec<Adapter>, String> {
    let manager = backend.manager(cancel).await?;
    let adapters = ble(cancel, "List Bluetooth adapters", manager.adapters()).await?;
    if adapters.is_empty() {
        return Err("No Bluetooth adapter found; enable Bluetooth and check OS permissions".into());
    }
    Ok(adapters)
}

async fn scan_adapter(adapter: &Adapter, cancel: &AtomicBool) -> Result<Vec<Peripheral>, String> {
    let result = async {
        ble(
            cancel,
            "Start Bluetooth scan",
            adapter.start_scan(ScanFilter::default()),
        )
        .await?;
        bounded(
            cancel,
            SCAN_TIME + Duration::from_secs(1),
            "Bluetooth scan",
            async {
                tokio::time::sleep(SCAN_TIME).await;
                Ok(())
            },
        )
        .await?;
        ble(cancel, "Read Bluetooth scan", adapter.peripherals()).await
    }
    .await;
    let cleanup = timeout(CLEANUP_TIMEOUT, adapter.stop_scan()).await;
    match (result, cleanup) {
        (Err(e), _) => Err(e),
        (Ok(_), Err(_)) => Err("Timed out stopping Bluetooth scan".into()),
        (Ok(_), Ok(Err(e))) => Err(format!("Stop Bluetooth scan: {e}")),
        (Ok(devices), Ok(Ok(()))) => Ok(devices),
    }
}

pub async fn scan(
    backend: &Backend,
    cancel: &AtomicBool,
    progress: &mut (dyn FnMut(Progress) + Send),
) -> Result<Vec<BleDevice>, String> {
    report(progress, "Scanning for Legacy BLE DFU devices", 0, 0);
    let mut devices = Vec::new();
    let mut identities = HashSet::new();
    for adapter in adapters(backend, cancel).await? {
        let adapter_id = ble(
            cancel,
            "Read Bluetooth adapter identity",
            adapter.adapter_info(),
        )
        .await?;
        if !identities.insert(adapter_id.clone()) {
            return Err(
                "Bluetooth adapters have ambiguous identities; enable only the intended adapter"
                    .into(),
            );
        }
        for peripheral in scan_adapter(&adapter, cancel).await? {
            if let Some(properties) =
                ble(cancel, "Read BLE advertisement", peripheral.properties()).await?
            {
                let name = properties
                    .local_name
                    .unwrap_or_else(|| "Legacy DFU device".into());
                if properties.services.contains(&SERVICE)
                    || name.to_ascii_lowercase().contains("adadfu")
                {
                    devices.push(BleDevice {
                        id: peripheral.id().to_string(),
                        adapter_id: adapter_id.clone(),
                        name,
                        address: peripheral.address().to_string(),
                    });
                }
            }
        }
    }
    devices.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
    Ok(devices)
}

async fn await_package(
    task: tokio::task::JoinHandle<Result<Package, String>>,
    cancel: &AtomicBool,
) -> Result<Package, String> {
    // A started blocking parser cannot be aborted. Retain it until joined, including
    // when cancellation arrives, before the caller can release its managed job.
    let result = task
        .await
        .map_err(|e| format!("DFU package parser failed: {e}"))?;
    cancelled(cancel)?;
    result
}

pub(crate) async fn prepare(
    name: String,
    bytes: Arc<[u8]>,
    cancel: &AtomicBool,
) -> Result<Package, String> {
    cancelled(cancel)?;
    await_package(
        tokio::task::spawn_blocking(move || parse_package(&name, &bytes)),
        cancel,
    )
    .await
}

pub(crate) async fn flash(
    backend: &Backend,
    device: &BleDevice,
    package: &Package,
    cancel: &AtomicBool,
    progress: &mut (dyn FnMut(Progress) + Send),
) -> Result<(), String> {
    cancelled(cancel)?;
    report(progress, format!("Finding {}", device.name), 0, 0);
    let mut selected = None;
    for adapter in adapters(backend, cancel).await? {
        if ble(
            cancel,
            "Read Bluetooth adapter identity",
            adapter.adapter_info(),
        )
        .await?
            == device.adapter_id
        {
            if selected.is_some() {
                return Err("Selected Bluetooth adapter identity is ambiguous".into());
            }
            selected = Some(adapter);
        }
    }
    let adapter =
        selected.ok_or("Selected Bluetooth adapter is no longer available; scan again")?;
    let peripheral = scan_adapter(&adapter, cancel)
        .await?
        .into_iter()
        .find(|p| p.id().to_string() == device.id)
        .ok_or("Selected BLE device is not advertising; enter its DFU mode and scan again")?;
    let mut connection = Connection {
        peripheral: &peripheral,
        control: None,
    };
    let mut started = false;
    let result = async {
        report(progress, format!("Connecting to {}", device.name), 0, 0);
        ble(cancel, "Connect BLE DFU", peripheral.connect()).await?;
        ble(cancel, "Discover Legacy DFU service", peripheral.discover_services()).await?;
        let characteristics = peripheral.characteristics();
        let cp = characteristics.iter().find(|c| c.service_uuid == SERVICE && c.uuid == CONTROL).cloned().ok_or("Selected device does not expose the Legacy DFU control point")?;
        let packet = characteristics.iter().find(|c| c.service_uuid == SERVICE && c.uuid == PACKET).cloned().ok_or("Selected device does not expose the Legacy DFU packet characteristic")?;
        if !cp.properties.contains(CharPropFlags::WRITE | CharPropFlags::NOTIFY) || !packet.properties.contains(CharPropFlags::WRITE_WITHOUT_RESPONSE) {
            return Err("Legacy DFU characteristics do not support the required write/notification properties".into());
        }
        connection.control = Some(cp.clone());
        let notifications = ble(cancel, "Open DFU notification stream", peripheral.notifications()).await?;
        ble(cancel, "Subscribe DFU notifications", peripheral.subscribe(&cp)).await?;
        let chunk = chunk_size(peripheral.mtu())?;
        report(progress, format!("Legacy DFU: {chunk}-byte packets, PRN {PRN}"), 0, package.image.len());
        let mut transport = Gatt { peripheral: &peripheral, control: &cp, packet: &packet, notifications, cancel };
        upload(&mut transport, package, chunk, cancel, progress, &mut started).await
    }.await;
    finish_connection(&mut connection, result, started).await
}

trait Cleanup: Send {
    fn abort(&mut self) -> impl Future<Output = Result<(), String>> + Send;
    fn unsubscribe(&mut self) -> impl Future<Output = Result<(), String>> + Send;
    fn disconnect(&mut self) -> impl Future<Output = Result<(), String>> + Send;
}

struct Connection<'a> {
    peripheral: &'a Peripheral,
    control: Option<Characteristic>,
}

impl Cleanup for Connection<'_> {
    async fn abort(&mut self) -> Result<(), String> {
        if let Some(control) = &self.control {
            self.peripheral
                .write(control, &[0x06], WriteType::WithResponse)
                .await
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    async fn unsubscribe(&mut self) -> Result<(), String> {
        if let Some(control) = &self.control {
            self.peripheral
                .unsubscribe(control)
                .await
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), String> {
        self.peripheral
            .disconnect()
            .await
            .map_err(|e| e.to_string())
    }
}

async fn finish_connection(
    connection: &mut impl Cleanup,
    result: Result<(), String>,
    started: bool,
) -> Result<(), String> {
    // Cleanup ignores cancellation and retains each operation through its own deadline.
    if result.is_err() && started {
        let _ = timeout(CLEANUP_TIMEOUT, connection.abort()).await;
    }
    let _ = timeout(CLEANUP_TIMEOUT, connection.unsubscribe()).await;
    let disconnected = timeout(CLEANUP_TIMEOUT, connection.disconnect()).await;
    match (result, disconnected) {
        (Err(e), Err(_)) => Err(format!("{e}; Bluetooth disconnect timed out")),
        (Err(e), Ok(Err(cleanup))) => Err(format!("{e}; Bluetooth disconnect: {cleanup}")),
        (Err(e), _) => Err(e),
        (Ok(()), _) => Ok(()), // Activate/reset normally already disconnected the device.
    }
}

fn chunk_size(mtu: u16) -> Result<usize, String> {
    // ATT write payload is MTU-3, at most the 512-byte attribute limit; SDK11 needs words.
    let payload = usize::from(mtu).saturating_sub(3).min(512);
    let size = payload / 4 * 4;
    if size < 4 {
        Err(format!("Invalid negotiated BLE MTU: {mtu}"))
    } else {
        Ok(size)
    }
}

#[derive(Debug, PartialEq)]
enum Notification {
    Response(u8),
    Receipt(usize),
}

fn notification(bytes: &[u8]) -> Result<Notification, String> {
    match bytes {
        [0x10, procedure, 1] => Ok(Notification::Response(*procedure)),
        [0x10, procedure, status] => Err(format!(
            "Legacy DFU procedure 0x{procedure:02X} failed: status 0x{status:02X}"
        )),
        [0x11, a, b, c, d] => Ok(Notification::Receipt(
            u32::from_le_bytes([*a, *b, *c, *d]) as usize
        )),
        _ => Err(format!("Malformed Legacy DFU notification: {bytes:02X?}")),
    }
}

trait Transport: Send {
    fn control(&mut self, data: &[u8]) -> impl Future<Output = Result<(), String>> + Send;
    fn packet(&mut self, data: &[u8]) -> impl Future<Output = Result<(), String>> + Send;
    fn next(&mut self) -> impl Future<Output = Result<Notification, String>> + Send;
}

struct Gatt<'a> {
    peripheral: &'a Peripheral,
    control: &'a Characteristic,
    packet: &'a Characteristic,
    notifications: Pin<Box<dyn Stream<Item = ValueNotification> + Send>>,
    cancel: &'a AtomicBool,
}
impl Transport for Gatt<'_> {
    async fn control(&mut self, data: &[u8]) -> Result<(), String> {
        ble(
            self.cancel,
            "Write DFU control point",
            self.peripheral
                .write(self.control, data, WriteType::WithResponse),
        )
        .await
    }
    async fn packet(&mut self, data: &[u8]) -> Result<(), String> {
        ble(
            self.cancel,
            "Write DFU packet",
            self.peripheral
                .write(self.packet, data, WriteType::WithoutResponse),
        )
        .await
    }
    async fn next(&mut self) -> Result<Notification, String> {
        loop {
            let event = self
                .notifications
                .next()
                .await
                .ok_or("BLE notification stream closed")?;
            if event.uuid == CONTROL {
                return notification(&event.value);
            }
        }
    }
}

async fn response(
    transport: &mut impl Transport,
    cancel: &AtomicBool,
    expected: u8,
    final_receipt: Option<usize>,
) -> Result<(), String> {
    bounded(cancel, IO_TIMEOUT, "DFU response", async {
        loop {
            match transport.next().await? {
                Notification::Response(op) if op == expected => return Ok(()),
                Notification::Receipt(count) if final_receipt == Some(count) => continue,
                other => {
                    return Err(format!(
                        "Unexpected DFU notification while waiting for 0x{expected:02X}: {other:?}"
                    ));
                }
            }
        }
    })
    .await
}

async fn receipt(
    transport: &mut impl Transport,
    cancel: &AtomicBool,
    sent: usize,
) -> Result<(), String> {
    bounded(cancel, IO_TIMEOUT, "DFU packet receipt", async {
        match transport.next().await? {
            Notification::Receipt(count) if count == sent => Ok(()),
            Notification::Receipt(count) => Err(format!(
                "DFU receipt mismatch: device received {count}, expected {sent}; transfer stopped"
            )),
            other => Err(format!(
                "Unexpected DFU notification before transfer finished: {other:?}"
            )),
        }
    })
    .await
}

async fn upload(
    transport: &mut impl Transport,
    package: &Package,
    chunk: usize,
    cancel: &AtomicBool,
    progress: &mut (dyn FnMut(Progress) + Send),
    started: &mut bool,
) -> Result<(), String> {
    cancelled(cancel)?;
    *started = true;
    transport.control(&[1, 4]).await?;
    let mut sizes = [0; 12];
    sizes[8..].copy_from_slice(&(package.image.len() as u32).to_le_bytes());
    transport.packet(&sizes).await?;
    response(transport, cancel, 1, None).await?;
    transport.control(&[2, 0]).await?;
    for bytes in package.init.chunks(chunk) {
        cancelled(cancel)?;
        transport.packet(bytes).await?;
    }
    transport.control(&[2, 1]).await?;
    response(transport, cancel, 2, None).await?;
    transport.control(&[8, PRN as u8, 0]).await?;
    transport.control(&[3]).await?;
    let mut sent = 0;
    for (index, bytes) in package.image.chunks(chunk).enumerate() {
        cancelled(cancel)?;
        transport.packet(bytes).await?;
        sent += bytes.len();
        // The final packet may produce a completion response instead of a PRN. Do not
        // consume that response here then wait for it a second time (including exact PRN multiples).
        if (index + 1) % PRN == 0 && sent < package.image.len() {
            receipt(transport, cancel, sent).await?;
            report(
                progress,
                "Uploading Legacy BLE firmware",
                sent,
                package.image.len(),
            );
        }
    }
    response(transport, cancel, 3, Some(sent)).await?;
    report(
        progress,
        "Validating Legacy BLE firmware",
        sent,
        package.image.len(),
    );
    transport.control(&[4]).await?;
    response(transport, cancel, 4, None).await?;
    cancelled(cancel)?;
    transport.control(&[5]).await.map_err(|e| format!("Firmware validated, but activation could not be confirmed (device may have restarted): {e}"))?;
    report(
        progress,
        "Firmware validated; activate/reset sent",
        sent,
        package.image.len(),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        Backend, Cleanup, Notification, Package, Transport, await_package, bounded, chunk_size,
        finish_connection, notification, parse_package, scan, upload,
    };
    use std::{
        collections::VecDeque,
        io::{Cursor, Write},
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
        time::Duration,
    };
    use tokio::sync::oneshot;

    fn runtime() -> Result<tokio::runtime::Runtime, std::io::Error> {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
    }

    fn require_send<T: Send>(future: T) -> T {
        future
    }

    #[test]
    fn cancelled_scan_does_not_initialize_native_backend() {
        let backend = Backend::default();
        let cancel = AtomicBool::new(true);
        let error = runtime()
            .unwrap()
            .block_on(require_send(scan(&backend, &cancel, &mut |_| {})))
            .unwrap_err();
        assert!(error.contains("cancelled"));
        assert!(backend.manager.get().is_none());
    }

    fn init() -> Vec<u8> {
        vec![0x52, 0, 0xff, 0xff, 1, 0, 0, 0, 1, 0, 0xfe, 0xff, 0, 0]
    }
    fn package(bin_name: &str, image: &[u8], dat: &[u8], extra: bool) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        let mut manifest = serde_json::json!({"manifest":{"application":{"bin_file":bin_name,"dat_file":"application.dat"}}});
        if extra {
            manifest["manifest"]["bootloader"] =
                serde_json::json!({"bin_file":"boot.bin","dat_file":"boot.dat"});
        }
        for (name, bytes) in [
            ("manifest.json", serde_json::to_vec(&manifest).unwrap()),
            (bin_name, image.to_vec()),
            ("application.dat", dat.to_vec()),
        ] {
            zip.start_file(name, options).unwrap();
            zip.write_all(&bytes).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    #[test]
    fn parses_legacy_application_without_changing_init() {
        let bytes = package("nested/app.bin", &[1, 2, 3, 4], &init(), false);
        let parsed = parse_package("legacy.ZIP", &bytes).unwrap();
        assert_eq!(parsed.image, [1, 2, 3, 4]);
        assert_eq!(parsed.init, init());
    }
    #[test]
    fn legacy_zip_rejects_unsafe_envelopes_and_false_member_sizes() {
        let bytes = package("app.bin", &[1, 2, 3, 4], &init(), false);
        let end = bytes.len() - 22;
        for (field, value) in [(8, 65u16), (10, 65), (4, 1), (6, 1), (10, u16::MAX)] {
            let mut malformed = bytes.clone();
            malformed[end + field..end + field + 2].copy_from_slice(&value.to_le_bytes());
            assert!(parse_package("legacy.zip", &malformed).is_err());
        }
        let central = u32::from_le_bytes(bytes[end + 16..end + 20].try_into().unwrap()) as usize;
        let local =
            u32::from_le_bytes(bytes[central + 42..central + 46].try_into().unwrap()) as usize;
        let size = u32::from_le_bytes(bytes[central + 24..central + 28].try_into().unwrap());
        for declared in [0, size - 1, size + 1] {
            let mut malformed = bytes.clone();
            malformed[local + 22..local + 26].copy_from_slice(&declared.to_le_bytes());
            malformed[central + 24..central + 28].copy_from_slice(&declared.to_le_bytes());
            assert!(parse_package("legacy.zip", &malformed).is_err());
        }
    }
    #[test]
    fn rejects_secure_mcuboot_traversal_unaligned_and_multi_image() {
        for (name, image, dat, extra) in [
            (
                "app.bin",
                vec![1, 2, 3, 4],
                vec![0x12, 0x0c, 0x0a, 0x0a, 0x08, 1],
                false,
            ),
            ("app.bin", vec![0x3d, 0xb8, 0xf3, 0x96], init(), false),
            ("../app.bin", vec![1, 2, 3, 4], init(), false),
            ("app.bin", vec![1, 2, 3], init(), false),
            ("app.bin", vec![1, 2, 3, 4], init(), true),
        ] {
            assert!(parse_package("bad.zip", &package(name, &image, &dat, extra)).is_err());
        }
    }
    #[test]
    fn negotiated_chunks_never_exceed_att_payload_and_are_words() {
        assert_eq!(chunk_size(23).unwrap(), 20);
        assert_eq!(chunk_size(247).unwrap(), 244);
        assert_eq!(chunk_size(185).unwrap(), 180);
        assert_eq!(chunk_size(517).unwrap(), 512);
        assert!(chunk_size(6).is_err());
    }
    #[test]
    fn decodes_receipts_and_preserves_protocol_failure() {
        assert_eq!(
            notification(&[0x11, 0x34, 0x12, 0, 0]).unwrap(),
            Notification::Receipt(0x1234)
        );
        assert!(notification(&[0x11, 1]).is_err());
        assert!(notification(&[0x10, 4, 5]).unwrap_err().contains("0x05"));
    }

    struct Script {
        events: VecDeque<Notification>,
        writes: Vec<(bool, Vec<u8>)>,
        fail_activation: bool,
    }
    impl Transport for Script {
        async fn control(&mut self, data: &[u8]) -> Result<(), String> {
            self.writes.push((true, data.to_vec()));
            if self.fail_activation && data == [5] {
                Err("link lost".into())
            } else {
                Ok(())
            }
        }
        async fn packet(&mut self, data: &[u8]) -> Result<(), String> {
            self.writes.push((false, data.to_vec()));
            Ok(())
        }
        async fn next(&mut self) -> Result<Notification, String> {
            self.events
                .pop_front()
                .ok_or("unexpected extra wait".into())
        }
    }
    fn script(events: Vec<Notification>) -> Script {
        Script {
            events: events.into(),
            writes: Vec::new(),
            fail_activation: false,
        }
    }

    #[test]
    fn full_transfer_waits_for_prn_validates_and_activates() {
        let mut io = script(vec![
            Notification::Response(1),
            Notification::Response(2),
            Notification::Receipt(120),
            Notification::Response(3),
            Notification::Response(4),
        ]);
        let data = Package {
            image: vec![0x55; 140],
            init: init(),
        };
        runtime()
            .unwrap()
            .block_on(upload(
                &mut io,
                &data,
                20,
                &AtomicBool::new(false),
                &mut |_| {},
                &mut false,
            ))
            .unwrap();
        assert!(io.events.is_empty());
        assert_eq!(
            io.writes
                .iter()
                .filter(|(cp, _)| *cp)
                .map(|(_, data)| data.clone())
                .collect::<Vec<_>>(),
            vec![
                vec![1, 4],
                vec![2, 0],
                vec![2, 1],
                vec![8, 6, 0],
                vec![3],
                vec![4],
                vec![5]
            ]
        );
        assert_eq!(io.writes[1].1, [0, 0, 0, 0, 0, 0, 0, 0, 140, 0, 0, 0]);
        let packets: Vec<_> = io.writes.iter().filter(|(cp, _)| !cp).collect();
        assert_eq!(packets[1].1, data.init);
        assert_eq!(
            packets[2..]
                .iter()
                .flat_map(|(_, bytes)| bytes.iter().copied())
                .collect::<Vec<_>>(),
            data.image
        );
    }
    #[test]
    fn exact_prn_multiple_does_not_consume_completion_twice() {
        for final_prn in [false, true] {
            let mut events = vec![Notification::Response(1), Notification::Response(2)];
            if final_prn {
                events.push(Notification::Receipt(120));
            }
            events.extend([Notification::Response(3), Notification::Response(4)]);
            let mut io = script(events);
            runtime()
                .unwrap()
                .block_on(upload(
                    &mut io,
                    &Package {
                        image: vec![0; 120],
                        init: init(),
                    },
                    20,
                    &AtomicBool::new(false),
                    &mut |_| {},
                    &mut false,
                ))
                .unwrap();
        }
    }
    #[test]
    fn incorrect_receipt_never_validates_or_activates() {
        let mut io = script(vec![
            Notification::Response(1),
            Notification::Response(2),
            Notification::Receipt(100),
        ]);
        let result = runtime().unwrap().block_on(upload(
            &mut io,
            &Package {
                image: vec![0; 140],
                init: init(),
            },
            20,
            &AtomicBool::new(false),
            &mut |_| {},
            &mut false,
        ));
        assert!(result.unwrap_err().contains("receipt mismatch"));
        assert!(
            !io.writes
                .iter()
                .any(|(cp, data)| *cp && (data == &[4] || data == &[5]))
        );
    }
    #[test]
    fn activation_failure_is_not_reported_as_success() {
        let mut io = script(vec![
            Notification::Response(1),
            Notification::Response(2),
            Notification::Response(3),
            Notification::Response(4),
        ]);
        io.fail_activation = true;
        let result = runtime().unwrap().block_on(upload(
            &mut io,
            &Package {
                image: vec![0; 4],
                init: init(),
            },
            20,
            &AtomicBool::new(false),
            &mut |_| {},
            &mut false,
        ));
        assert!(
            result
                .unwrap_err()
                .contains("activation could not be confirmed")
        );
    }
    #[test]
    fn cancellation_and_notification_deadlines_are_bounded() {
        runtime().unwrap().block_on(async {
            let cancelled = AtomicBool::new(true);
            assert!(
                bounded(&cancelled, Duration::from_secs(1), "test", async { Ok(()) })
                    .await
                    .unwrap_err()
                    .contains("cancelled")
            );
            let active = AtomicBool::new(false);
            assert!(
                bounded(
                    &active,
                    Duration::from_millis(1),
                    "test",
                    std::future::pending::<Result<(), String>>()
                )
                .await
                .unwrap_err()
                .contains("Timed out")
            );
        });
        let mut io = script(Vec::new());
        let mut started = false;
        assert!(
            runtime()
                .unwrap()
                .block_on(upload(
                    &mut io,
                    &Package {
                        image: vec![0; 4],
                        init: init()
                    },
                    20,
                    &AtomicBool::new(true),
                    &mut |_| {},
                    &mut started
                ))
                .is_err()
        );
        assert!(io.writes.is_empty());
        assert!(!started);
    }

    #[test]
    fn cancellation_keeps_blocking_parser_joined_before_returning() {
        runtime().unwrap().block_on(async {
            let cancel = AtomicBool::new(false);
            let completed = Arc::new(AtomicBool::new(false));
            let parsed = Arc::clone(&completed);
            let bytes = package("application.bin", &[1, 2, 3, 4], &init(), false);
            let storage: Arc<[u8]> = Arc::from(bytes);
            let parser_storage = Arc::clone(&storage);
            let (entered, ready) = oneshot::channel();
            let (release, blocked) = std::sync::mpsc::channel();
            let task = tokio::task::spawn_blocking(move || {
                entered.send(()).unwrap();
                blocked.recv().unwrap();
                let result = parse_package("legacy.zip", &parser_storage);
                parsed.store(true, Ordering::Release);
                result
            });
            ready.await.unwrap();
            let mut operation = Box::pin(require_send(await_package(task, &cancel)));
            assert!(futures_util::poll!(operation.as_mut()).is_pending());
            cancel.store(true, Ordering::Release);
            assert!(futures_util::poll!(operation.as_mut()).is_pending());
            assert!(!completed.load(Ordering::Acquire));
            assert_eq!(Arc::strong_count(&storage), 2);
            release.send(()).unwrap();
            assert!(operation.await.unwrap_err().contains("cancelled"));
            assert!(completed.load(Ordering::Acquire));
            assert_eq!(Arc::strong_count(&storage), 1);
        });
    }

    struct PendingWrite<'a> {
        script: Script,
        cancel: &'a AtomicBool,
        starts: Arc<AtomicUsize>,
        drops: Arc<AtomicUsize>,
    }

    struct WriteDrop(Arc<AtomicUsize>);

    impl Drop for WriteDrop {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    impl Transport for PendingWrite<'_> {
        async fn control(&mut self, data: &[u8]) -> Result<(), String> {
            self.script.control(data).await
        }

        async fn packet(&mut self, data: &[u8]) -> Result<(), String> {
            if data == [0x55; 20] {
                bounded(
                    self.cancel,
                    Duration::from_secs(20),
                    "pending write",
                    async {
                        self.starts.fetch_add(1, Ordering::Relaxed);
                        let _drop = WriteDrop(Arc::clone(&self.drops));
                        std::future::pending::<Result<(), String>>().await
                    },
                )
                .await
            } else {
                self.script.packet(data).await
            }
        }

        async fn next(&mut self) -> Result<Notification, String> {
            self.script.next().await
        }
    }

    struct TestConnection {
        actions: Arc<Mutex<Vec<&'static str>>>,
        gates: VecDeque<oneshot::Receiver<Result<(), String>>>,
    }

    impl TestConnection {
        async fn step(&mut self, action: &'static str) -> Result<(), String> {
            self.actions.lock().unwrap().push(action);
            match self.gates.pop_front() {
                Some(gate) => gate.await.unwrap(),
                None => Ok(()),
            }
        }
    }

    impl Cleanup for TestConnection {
        async fn abort(&mut self) -> Result<(), String> {
            self.step("abort").await
        }

        async fn unsubscribe(&mut self) -> Result<(), String> {
            self.step("unsubscribe").await
        }

        async fn disconnect(&mut self) -> Result<(), String> {
            self.step("disconnect").await
        }
    }

    #[test]
    fn cancelled_pending_write_is_not_replayed_and_cleanup_is_joined() {
        runtime().unwrap().block_on(async {
            let cancel = AtomicBool::new(false);
            let starts = Arc::new(AtomicUsize::new(0));
            let drops = Arc::new(AtomicUsize::new(0));
            let mut io = PendingWrite {
                script: script(vec![Notification::Response(1), Notification::Response(2)]),
                cancel: &cancel,
                starts: Arc::clone(&starts),
                drops: Arc::clone(&drops),
            };
            let package = Package {
                image: vec![0x55; 20],
                init: init(),
            };
            let mut started = false;
            let mut progress = |_| {};
            let mut upload = Box::pin(require_send(upload(
                &mut io,
                &package,
                20,
                &cancel,
                &mut progress,
                &mut started,
            )));
            assert!(futures_util::poll!(upload.as_mut()).is_pending());
            assert_eq!(starts.load(Ordering::Relaxed), 1);
            // Let more than one cancellation tick pass while retaining the same write.
            tokio::time::sleep(Duration::from_millis(210)).await;
            assert!(futures_util::poll!(upload.as_mut()).is_pending());
            assert_eq!(starts.load(Ordering::Relaxed), 1);
            cancel.store(true, Ordering::Release);
            let result = upload.await;
            assert!(result.as_ref().unwrap_err().contains("cancelled"));
            assert_eq!(drops.load(Ordering::Relaxed), 1);
            assert!(started);
            assert!(
                !io.script
                    .writes
                    .iter()
                    .any(|(cp, data)| { *cp && (data == &[4] || data == &[5]) })
            );

            let actions = Arc::new(Mutex::new(Vec::new()));
            let (abort, abort_gate) = oneshot::channel();
            let (unsubscribe, unsubscribe_gate) = oneshot::channel();
            let (disconnect, disconnect_gate) = oneshot::channel();
            let mut connection = TestConnection {
                actions: Arc::clone(&actions),
                gates: [abort_gate, unsubscribe_gate, disconnect_gate].into(),
            };
            let mut cleanup = Box::pin(require_send(finish_connection(
                &mut connection,
                result,
                started,
            )));
            assert!(futures_util::poll!(cleanup.as_mut()).is_pending());
            assert_eq!(*actions.lock().unwrap(), ["abort"]);
            abort.send(Err("abort failed".into())).unwrap();
            assert!(futures_util::poll!(cleanup.as_mut()).is_pending());
            assert_eq!(*actions.lock().unwrap(), ["abort", "unsubscribe"]);
            unsubscribe.send(Err("unsubscribe failed".into())).unwrap();
            assert!(futures_util::poll!(cleanup.as_mut()).is_pending());
            assert_eq!(
                *actions.lock().unwrap(),
                ["abort", "unsubscribe", "disconnect"]
            );
            disconnect.send(Err("link held".into())).unwrap();
            let error = cleanup.await.unwrap_err();
            assert!(error.contains("cancelled"));
            assert!(error.contains("Bluetooth disconnect: link held"));
        });
    }

    #[test]
    fn managed_job_cancellation_retains_transport_until_cleanup_finishes() {
        let runtime = crate::app_runtime::AppRuntime::new().unwrap();
        let starts = Arc::new(AtomicUsize::new(0));
        let drops = Arc::new(AtomicUsize::new(0));
        let actions = Arc::new(Mutex::new(Vec::new()));
        let worker_starts = Arc::clone(&starts);
        let worker_drops = Arc::clone(&drops);
        let worker_actions = Arc::clone(&actions);
        let (abort, abort_gate) = oneshot::channel();
        let (unsubscribe, unsubscribe_gate) = oneshot::channel();
        let (disconnect, disconnect_gate) = oneshot::channel();
        let (cleanup_entered, cleanup_ready) = std::sync::mpsc::channel();
        let mut job = crate::update::Job::start_async(
            &runtime.executor(),
            move |cancel, progress| async move {
                let mut io = PendingWrite {
                    script: script(vec![Notification::Response(1), Notification::Response(2)]),
                    cancel: &cancel,
                    starts: worker_starts,
                    drops: worker_drops,
                };
                let package = Package {
                    image: vec![0x55; 20],
                    init: init(),
                };
                let mut started = false;
                let result = upload(
                    &mut io,
                    &package,
                    20,
                    &cancel,
                    &mut |value| progress.emit(value),
                    &mut started,
                )
                .await;
                let mut connection = TestConnection {
                    actions: worker_actions,
                    gates: [abort_gate, unsubscribe_gate, disconnect_gate].into(),
                };
                cleanup_entered.send(()).unwrap();
                finish_connection(&mut connection, result, started).await
            },
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while starts.load(Ordering::Relaxed) == 0 {
            assert!(std::time::Instant::now() < deadline, "write never started");
            std::thread::yield_now();
        }
        job.cancel();
        cleanup_ready.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(starts.load(Ordering::Relaxed), 1);
        assert_eq!(drops.load(Ordering::Relaxed), 1);
        assert!(job.poll().is_none());
        abort.send(Ok(())).unwrap();
        unsubscribe.send(Ok(())).unwrap();
        assert!(job.poll().is_none());
        disconnect.send(Ok(())).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let result = loop {
            if let Some(result) = job.poll() {
                break result;
            }
            assert!(std::time::Instant::now() < deadline, "cleanup never joined");
            std::thread::yield_now();
        };
        assert!(result.unwrap_err().contains("cancelled"));
        assert_eq!(
            *actions.lock().unwrap(),
            ["abort", "unsubscribe", "disconnect"]
        );
        runtime.shutdown();
    }

    #[test]
    fn cleanup_deadlines_drop_pending_operations_and_still_disconnect() {
        runtime().unwrap().block_on(async {
            let actions = Arc::new(Mutex::new(Vec::new()));
            let (abort, abort_gate) = oneshot::channel();
            let (unsubscribe, unsubscribe_gate) = oneshot::channel();
            let (disconnect, disconnect_gate) = oneshot::channel();
            let mut connection = TestConnection {
                actions: Arc::clone(&actions),
                gates: [abort_gate, unsubscribe_gate, disconnect_gate].into(),
            };
            let error = require_send(finish_connection(
                &mut connection,
                Err("transfer stopped".into()),
                true,
            ))
            .await
            .unwrap_err();
            assert_eq!(
                *actions.lock().unwrap(),
                ["abort", "unsubscribe", "disconnect"]
            );
            assert!(abort.is_closed());
            assert!(unsubscribe.is_closed());
            assert!(disconnect.is_closed());
            assert!(error.contains("transfer stopped"));
            assert!(error.contains("disconnect timed out"));
        });
    }

    #[test]
    fn cleanup_skips_abort_before_start_and_after_successful_activation() {
        runtime().unwrap().block_on(async {
            for (result, started) in [(Err("connect failed".into()), false), (Ok(()), true)] {
                let expected_success = result.is_ok();
                let actions = Arc::new(Mutex::new(Vec::new()));
                let (disconnect, disconnect_gate) = oneshot::channel();
                disconnect.send(Err("already reset".into())).unwrap();
                // The first gate belongs to unsubscribe; it succeeds immediately.
                let (unsubscribe, unsubscribe_gate) = oneshot::channel();
                unsubscribe.send(Ok(())).unwrap();
                let mut connection = TestConnection {
                    actions: Arc::clone(&actions),
                    gates: [unsubscribe_gate, disconnect_gate].into(),
                };
                let result = finish_connection(&mut connection, result, started).await;
                assert_eq!(result.is_ok(), expected_success);
                assert_eq!(*actions.lock().unwrap(), ["unsubscribe", "disconnect"]);
                if !expected_success {
                    assert!(result.unwrap_err().contains("connect failed"));
                }
            }
        });
    }
}
