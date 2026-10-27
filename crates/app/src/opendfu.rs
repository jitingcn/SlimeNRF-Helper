//! Nordic SDK15+ Secure DFU over SLIP; callers retain the exclusive serial lease.
use crate::domain::SerialEndpoint;
use crate::serial_port::same_port_name;
use crate::update::Progress;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::io::{self, Cursor, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const MAX_IMAGE: usize = 2 * 1024 * 1024;
const MAX_INIT: usize = 4096;
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(10);
const READ_TIMEOUT: Duration = Duration::from_millis(100);
const WRITE_TIMEOUT: Duration = Duration::from_millis(500);

#[derive(Clone, Debug)]
pub struct Options {
    pub hw_version: u32,
    pub app_version: u32,
    pub sd_req: Vec<u32>,
    /// Known bootloader application start. Secure DFU carries no HEX addresses.
    pub app_base: u32,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            hw_version: 52,
            app_version: 1,
            sd_req: vec![0],
            app_base: 0x1000,
        }
    }
}

#[derive(Debug)]
enum Error {
    Io(io::Error),
    Protocol(String),
    Rejected(u8, String),
    Timeout,
    Cancelled,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "Serial DFU I/O failed: {e}"),
            Self::Protocol(s) => f.write_str(s),
            Self::Rejected(code, s) => write!(f, "Bootloader rejected DFU (0x{code:02x}): {s}"),
            Self::Timeout => f.write_str("Timed out waiting for the DFU bootloader"),
            Self::Cancelled => f.write_str("DFU cancelled; the bootloader retains resumable data. No activation was requested after cancellation."),
        }
    }
}
impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}
type Result<T, E = Error> = std::result::Result<T, E>;
fn check_cancel(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Acquire) {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}
fn report(
    progress: &mut dyn FnMut(Progress),
    message: impl Into<String>,
    completed: usize,
    total: usize,
) {
    progress(Progress::bytes(message, completed as u64, total as u64));
}

trait Transport: Read + Write {
    fn reading(&mut self) -> io::Result<()> {
        Ok(())
    }
    fn writing(&mut self) -> io::Result<()> {
        Ok(())
    }
}
struct Serial(Box<dyn serialport::SerialPort>);
impl Read for Serial {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        self.0.read(b)
    }
}
impl Write for Serial {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.0.write(b)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}
impl Transport for Serial {
    // serialport sets both native timeouts on Windows. Set the appropriate budget
    // before each direction; never replay a timed-out write (a prefix may be sent).
    fn reading(&mut self) -> io::Result<()> {
        self.0.set_timeout(READ_TIMEOUT).map_err(io::Error::from)
    }
    fn writing(&mut self) -> io::Result<()> {
        self.0.set_timeout(WRITE_TIMEOUT).map_err(io::Error::from)
    }
}
fn open(port: &str) -> std::result::Result<Serial, String> {
    let mut serial = serialport::new(port, 115_200)
        .timeout(READ_TIMEOUT)
        .open()
        .map_err(|e| {
            format!("Cannot open {port}: {e}. Close other applications using this port.")
        })?;
    serial
        .write_data_terminal_ready(true)
        .map_err(|e| format!("Cannot assert DTR on {port}: {e}"))?;
    serial
        .write_request_to_send(true)
        .map_err(|e| format!("Cannot assert RTS on {port}: {e}"))?;
    serial
        .clear(serialport::ClearBuffer::Input)
        .map_err(|e| format!("Cannot clear {port}: {e}"))?;
    Ok(Serial(serial))
}
fn bootloader_identity(
    port: &str,
    ports: std::result::Result<Vec<SerialEndpoint>, String>,
) -> std::result::Result<SerialEndpoint, String> {
    let mut matches = ports?
        .into_iter()
        .filter(|entry| same_port_name(&entry.port_name, port));
    let entry = matches.next().ok_or_else(|| {
        format!("{port} is no longer present; select the reenumerated bootloader explicitly")
    })?;
    if matches.next().is_some() {
        return Err(format!(
            "{port} has ambiguous serial identity; explicitly select the bootloader again"
        ));
    }
    if !entry.is_open_dfu_bootloader() {
        return Err(format!(
            "{port} is not identified as Open DFU Bootloader. Enter DFU, then explicitly select its bootloader port."
        ));
    }
    Ok(entry)
}

fn revalidate_bootloader_identity(
    expected: &SerialEndpoint,
    current: &SerialEndpoint,
) -> std::result::Result<(), String> {
    if !expected.is_open_dfu_bootloader()
        || !current.is_open_dfu_bootloader()
        || !same_port_name(&current.port_name, &expected.port_name)
        || current.kind != expected.kind
        || current.vid != expected.vid
        || current.pid != expected.pid
        || current.serial_number != expected.serial_number
        || current.product != expected.product
        || !current.same_usb_interface(expected)
    {
        return Err("Bootloader identity changed; explicitly select it again".into());
    }
    Ok(())
}

struct Image {
    name: String,
    firmware: Vec<u8>,
    init: Vec<u8>,
}

pub fn flash_with_options(
    port: &SerialEndpoint,
    name: &str,
    bytes: &[u8],
    options: &Options,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(Progress),
) -> std::result::Result<(), String> {
    check_cancel(cancel).map_err(|e| e.to_string())?;
    let images = parse_images(name, bytes, options)?;
    let total: usize = images.iter().map(|i| i.firmware.len()).sum();
    let mut completed = 0;
    for (index, image) in images.iter().enumerate() {
        check_cancel(cancel).map_err(|e| e.to_string())?;
        report(
            progress,
            format!("Connecting to OpenDFU: {}", image.name),
            completed,
            total,
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        let (mut serial, mtu) = loop {
            check_cancel(cancel).map_err(|e| e.to_string())?;
            // Revalidate immediately before every open. System-image activation
            // may reset USB: reconnect only this exact port/identity, never guess
            // a changed COM name. Only the harmless connection handshake retries.
            let attempt =
                match bootloader_identity(&port.port_name, crate::discovery::serial_endpoints()) {
                    Ok(current) => {
                        revalidate_bootloader_identity(port, &current)?;
                        open(&port.port_name).and_then(|mut serial| {
                            let mtu = {
                                let mut session = Session::new(&mut serial, cancel);
                                session.connect().map_err(|e| e.to_string())?;
                                session.mtu
                            };
                            Ok((serial, mtu))
                        })
                    }
                    Err(error) => Err(error),
                };
            match attempt {
                Ok(connection) => break connection,
                Err(error) if index == 0 || Instant::now() >= deadline => {
                    return Err(format!(
                        "{error}. If USB reenumerated, explicitly select the bootloader port and resume."
                    ));
                }
                Err(_) => {
                    check_cancel(cancel).map_err(|e| e.to_string())?;
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        };
        let mut session = Session::new(&mut serial, cancel);
        session.mtu = mtu;
        session.init(&image.init).map_err(|e| e.to_string())?;
        let disconnected = session
            .firmware(&image.firmware, &mut |offset| {
                report(
                    progress,
                    format!("Writing {}", image.name),
                    completed + offset,
                    total,
                );
            })
            .map_err(|e| e.to_string())?;
        completed += image.firmware.len();
        report(
            progress,
            if disconnected {
                "Image CRC verified; bootloader disconnected during activation. Reconnect to verify the installed version."
            } else {
                "Bootloader accepted the image and activation."
            },
            completed,
            total,
        );
    }
    Ok(())
}

fn varint(mut value: u32, out: &mut Vec<u8>) {
    while value >= 128 {
        out.push((value as u8 & 127) | 128);
        value >>= 7;
    }
    out.push(value as u8);
}
fn number(field: u32, value: u32, out: &mut Vec<u8>) {
    varint(field << 3, out);
    varint(value, out);
}
fn blob(field: u32, bytes: &[u8], out: &mut Vec<u8>) {
    varint((field << 3) | 2, out);
    varint(bytes.len() as u32, out);
    out.extend_from_slice(bytes);
}
fn generate_init(data: &[u8], options: &Options) -> Vec<u8> {
    let mut hash = Vec::with_capacity(36);
    number(1, 3, &mut hash);
    let mut digest = Sha256::digest(data);
    digest.reverse();
    blob(2, &digest, &mut hash);
    let mut init = Vec::with_capacity(80);
    number(1, options.app_version, &mut init);
    number(2, options.hw_version, &mut init);
    if !options.sd_req.is_empty() {
        let mut packed = Vec::with_capacity(options.sd_req.len() * 3);
        for &sd in &options.sd_req {
            varint(sd, &mut packed);
        }
        blob(3, &packed, &mut init);
    }
    number(4, 0, &mut init);
    number(7, data.len() as u32, &mut init);
    blob(8, &hash, &mut init);
    let mut command = Vec::with_capacity(init.len() + 4);
    number(1, 1, &mut command);
    blob(2, &init, &mut command);
    let mut packet = Vec::with_capacity(command.len() + 2);
    blob(1, &command, &mut packet);
    packet
}

// Read only the Secure DFU protobuf fields needed to reject Legacy packets,
// mismatched package members and malformed sizes before opening any serial port.
#[derive(Clone, Copy)]
enum Field<'a> {
    Number(u64),
    Bytes(&'a [u8]),
}
fn take_varint(bytes: &[u8], pos: &mut usize) -> std::result::Result<u64, String> {
    let mut result = 0;
    for shift in (0..70).step_by(7) {
        let b = *bytes
            .get(*pos)
            .ok_or("Truncated Secure DFU init protobuf")?;
        *pos += 1;
        if shift == 63 && b > 1 {
            return Err("Overflow in Secure DFU init protobuf".into());
        }
        result |= u64::from(b & 127) << shift;
        if b & 128 == 0 {
            return Ok(result);
        }
    }
    Err("Invalid Secure DFU init protobuf varint".into())
}
fn fields(bytes: &[u8]) -> std::result::Result<Vec<(u64, Field<'_>)>, String> {
    let mut pos = 0;
    let mut result = Vec::new();
    while pos < bytes.len() {
        let tag = take_varint(bytes, &mut pos)?;
        if tag >> 3 == 0 {
            return Err(
                "Not a Secure DFU init packet (Legacy BLE ZIP is not supported here)".into(),
            );
        }
        let value = match tag & 7 {
            0 => Field::Number(take_varint(bytes, &mut pos)?),
            2 => {
                let len = usize::try_from(take_varint(bytes, &mut pos)?)
                    .map_err(|_| "Invalid protobuf length")?;
                let end = pos
                    .checked_add(len)
                    .filter(|&e| e <= bytes.len())
                    .ok_or("Truncated Secure DFU init protobuf field")?;
                let data = &bytes[pos..end];
                pos = end;
                Field::Bytes(data)
            }
            _ => return Err("Unsupported Secure DFU protobuf wire type".into()),
        };
        result.push((tag >> 3, value));
    }
    Ok(result)
}
fn field<'a>(
    fields: &[(u64, Field<'a>)],
    key: u64,
) -> std::result::Result<Option<Field<'a>>, String> {
    let mut matches = fields.iter().filter(|(k, _)| *k == key);
    let value = matches.next().map(|(_, v)| *v);
    if matches.next().is_some() {
        return Err(format!("Duplicate Secure DFU init field {key}"));
    }
    Ok(value)
}
fn bytes_field<'a>(f: &[(u64, Field<'a>)], key: u64) -> std::result::Result<&'a [u8], String> {
    match field(f, key)? {
        Some(Field::Bytes(v)) => Ok(v),
        _ => Err(format!("Missing Secure DFU init message {key}")),
    }
}
fn num_field(f: &[(u64, Field<'_>)], key: u64, default: u64) -> std::result::Result<u64, String> {
    match field(f, key)? {
        Some(Field::Number(v)) => Ok(v),
        None => Ok(default),
        _ => Err(format!("Invalid Secure DFU init integer {key}")),
    }
}
fn validate_init(init: &[u8], firmware: &[u8], kind: &str) -> std::result::Result<(), String> {
    let root = fields(init)?;
    let command = match (field(&root, 1)?, field(&root, 2)?) {
        (Some(Field::Bytes(c)), None) => c,
        (None, Some(Field::Bytes(s))) => {
            let signed = fields(s)?;
            bytes_field(&signed, 1)?
        }
        _ => return Err("ZIP requires a Nordic Secure DFU .dat packet, not Legacy BLE DFU".into()),
    };
    let command = fields(command)?;
    if num_field(&command, 1, 0)? != 1 {
        return Err("Secure DFU packet is not an INIT command".into());
    }
    let init = fields(bytes_field(&command, 2)?)?;
    let expected_type = match kind {
        "application" => 0,
        "softdevice" => 1,
        "bootloader" => 2,
        "softdevice_bootloader" => 3,
        _ => unreachable!(),
    };
    if num_field(&init, 4, 0)? != expected_type {
        return Err(format!("Secure DFU init type does not match {kind}"));
    }
    let size = num_field(&init, 5, 0)?
        .checked_add(num_field(&init, 6, 0)?)
        .and_then(|v| v.checked_add(num_field(&init, 7, 0).ok()?))
        .ok_or("Secure DFU image size overflow")?;
    if size != firmware.len() as u64 {
        return Err("Secure DFU .dat image size does not match .bin".into());
    }
    let hash = fields(bytes_field(&init, 8)?)?;
    let actual = bytes_field(&hash, 2)?;
    let valid = match num_field(&hash, 1, 0)? {
        1 => actual == crc32fast::hash(firmware).to_le_bytes(),
        3 => {
            let mut expected = Sha256::digest(firmware);
            expected.reverse();
            actual == expected.as_slice()
        }
        _ => return Err("Secure DFU package must contain a CRC32 or SHA256 image hash".into()),
    };
    if !valid {
        return Err("Secure DFU .dat hash does not match .bin".into());
    }
    Ok(())
}
fn safe_member(name: &str) -> bool {
    !name.is_empty()
        && !name.contains('\\')
        && !name.contains(':')
        && !name.starts_with('/')
        && !name.split('/').any(|p| p == ".." || p == ".")
}
fn zip_member(
    archive: &mut zip::ZipArchive<Cursor<&[u8]>>,
    name: &str,
    limit: usize,
) -> std::result::Result<Vec<u8>, String> {
    if !safe_member(name) {
        return Err("Unsafe DFU ZIP member path".into());
    }
    let mut member = archive
        .by_name(name)
        .map_err(|e| format!("Missing ZIP member {name}: {e}"))?;
    if member.is_dir() || member.size() == 0 || member.size() > limit as u64 {
        return Err(format!("DFU ZIP member {name} has invalid size"));
    }
    let size = member.size();
    let mut bytes = Vec::with_capacity(size as usize);
    (&mut member)
        .take(size + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("Cannot read DFU ZIP member {name}: {e}"))?;
    if bytes.len() as u64 != size {
        return Err(format!("DFU ZIP member {name} has an inconsistent size"));
    }
    Ok(bytes)
}
fn parse_images(
    name: &str,
    bytes: &[u8],
    options: &Options,
) -> std::result::Result<Vec<Image>, String> {
    let extension = std::path::Path::new(name)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    if extension.eq_ignore_ascii_case("hex") {
        let image = crate::firmware::parse_image(name, bytes)?;
        if image.data.is_empty() || image.data.len() > MAX_IMAGE {
            return Err("Invalid OpenDFU HEX image size".into());
        }
        if image.base_address != options.app_base {
            return Err(format!(
                "HEX application begins at {:#x}, but this bootloader is configured for {:#x}; refusing to relocate an addressless Secure DFU image",
                image.base_address, options.app_base
            ));
        }
        return Ok(vec![Image {
            name: name.into(),
            init: generate_init(&image.data, options),
            firmware: image.data,
        }]);
    }
    if !extension.eq_ignore_ascii_case("zip") {
        return Err("OpenDFU requires Intel HEX or a Nordic Secure DFU ZIP".into());
    }
    if bytes.len() > 16 * 1024 * 1024 {
        return Err("DFU ZIP exceeds 16 MiB limit".into());
    }
    let mut archive = crate::firmware::zip_envelope::open(
        bytes,
        crate::firmware::zip_envelope::Limits {
            bytes: 16 * 1024 * 1024,
            members: 128,
            directory: 4 * 1024 * 1024,
        },
    )?;
    let mut seen = HashSet::new();
    for i in 0..archive.len() {
        let member = archive
            .by_index(i)
            .map_err(|e| format!("Invalid DFU ZIP member: {e}"))?;
        let name = member
            .name()
            .map_err(|e| format!("Cannot decode DFU ZIP member name: {e}"))?;
        if !safe_member(&name) || !seen.insert(name.into_owned()) {
            return Err("Unsafe or duplicate DFU ZIP member".into());
        }
    }
    let manifest = zip_member(&mut archive, "manifest.json", 64 * 1024)?;
    let manifest: serde_json::Value =
        serde_json::from_slice(&manifest).map_err(|e| format!("Invalid DFU manifest: {e}"))?;
    let entries = manifest
        .get("manifest")
        .unwrap_or(&manifest)
        .as_object()
        .ok_or("DFU manifest must be an object")?;
    if entries.contains_key("softdevice_bootloader")
        && (entries.contains_key("softdevice") || entries.contains_key("bootloader"))
    {
        return Err("DFU ZIP contains conflicting combined and separate system images".into());
    }
    let mut images = Vec::new();
    // System updates must precede an application which depends on the new SD/BL.
    for kind in [
        "softdevice_bootloader",
        "softdevice",
        "bootloader",
        "application",
    ] {
        if let Some(entry) = entries.get(kind) {
            let bin = entry
                .get("bin_file")
                .and_then(|v| v.as_str())
                .ok_or("DFU manifest is missing bin_file")?;
            let dat = entry
                .get("dat_file")
                .and_then(|v| v.as_str())
                .ok_or("DFU manifest is missing dat_file")?;
            let firmware = zip_member(&mut archive, bin, MAX_IMAGE)?;
            let init = zip_member(&mut archive, dat, MAX_INIT)?;
            validate_init(&init, &firmware, kind)?;
            images.push(Image {
                name: bin.into(),
                firmware,
                init,
            });
        }
    }
    if images.is_empty() {
        return Err("No Secure DFU firmware in ZIP".into());
    }
    Ok(images)
}

struct Session<'a, T> {
    io: &'a mut T,
    cancel: &'a AtomicBool,
    mtu: usize,
    tx: Vec<u8>,
    rx: Vec<u8>,
    escaping: bool,
    input: [u8; 256],
    input_pos: usize,
    input_len: usize,
}
impl<'a, T: Transport> Session<'a, T> {
    fn new(io: &'a mut T, cancel: &'a AtomicBool) -> Self {
        Self {
            io,
            cancel,
            mtu: 20,
            tx: Vec::with_capacity(4096),
            rx: Vec::with_capacity(64),
            escaping: false,
            input: [0; 256],
            input_pos: 0,
            input_len: 0,
        }
    }
    fn send(&mut self, opcode: u8, data: &[u8]) -> Result<()> {
        check_cancel(self.cancel)?;
        self.tx.clear();
        for b in std::iter::once(&opcode).chain(data.iter()) {
            match b {
                0xc0 => self.tx.extend_from_slice(&[0xdb, 0xdc]),
                0xdb => self.tx.extend_from_slice(&[0xdb, 0xdd]),
                _ => self.tx.push(*b),
            }
        }
        self.tx.push(0xc0);
        self.io.writing()?;
        // No flush: native flush may wait indefinitely for a rebooted device.
        self.io.write_all(&self.tx)?;
        Ok(())
    }
    fn response(&mut self, opcode: u8, timeout: Duration) -> Result<Vec<u8>> {
        self.io.reading()?;
        let deadline = Instant::now() + timeout;
        loop {
            check_cancel(self.cancel)?;
            if Instant::now() >= deadline {
                return Err(Error::Timeout);
            }
            if self.input_pos == self.input_len {
                self.input_len = match self.io.read(&mut self.input) {
                    Ok(0) => {
                        return Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "bootloader disconnected",
                        )
                        .into());
                    }
                    Ok(n) => n,
                    Err(e)
                        if matches!(
                            e.kind(),
                            io::ErrorKind::TimedOut
                                | io::ErrorKind::WouldBlock
                                | io::ErrorKind::Interrupted
                        ) =>
                    {
                        continue;
                    }
                    Err(e) => return Err(e.into()),
                };
                self.input_pos = 0;
            }
            let b = self.input[self.input_pos];
            self.input_pos += 1;
            if b == 0xc0 {
                if self.escaping {
                    self.rx.clear();
                    self.escaping = false;
                    return Err(Error::Protocol("Malformed SLIP escape".into()));
                }
                if self.rx.is_empty() {
                    continue;
                }
                if self.rx.len() < 3 || self.rx[0] != 0x60 {
                    self.rx.clear();
                    return Err(Error::Protocol("Malformed DFU response".into()));
                }
                if self.rx[1] != opcode {
                    // Ping retries may leave delayed replies; no other response
                    // can be silently discarded in the object protocol.
                    if self.rx[1] == 9 {
                        self.rx.clear();
                        continue;
                    }
                    let got = self.rx[1];
                    self.rx.clear();
                    return Err(Error::Protocol(format!(
                        "Unexpected DFU response opcode 0x{got:02x}; expected 0x{opcode:02x}"
                    )));
                }
                let code = self.rx[2];
                if code != 1 {
                    let detail = if code == 11 {
                        extended_error(self.rx.get(3).copied().unwrap_or(255)).to_owned()
                    } else {
                        result_error(code).to_owned()
                    };
                    self.rx.clear();
                    return Err(Error::Rejected(code, detail));
                }
                let data = self.rx[3..].to_vec();
                self.rx.clear();
                return Ok(data);
            }
            if self.escaping {
                self.escaping = false;
                match b {
                    0xdc => self.rx.push(0xc0),
                    0xdd => self.rx.push(0xdb),
                    _ => {
                        self.rx.clear();
                        return Err(Error::Protocol("Invalid SLIP escape".into()));
                    }
                }
            } else if b == 0xdb {
                self.escaping = true;
            } else {
                self.rx.push(b);
            }
            if self.rx.len() > 4096 {
                self.rx.clear();
                return Err(Error::Protocol("Oversized SLIP response".into()));
            }
        }
    }
    fn request(&mut self, op: u8, payload: &[u8]) -> Result<Vec<u8>> {
        self.send(op, payload)?;
        self.response(op, RESPONSE_TIMEOUT)
    }
    fn connect(&mut self) -> Result<()> {
        let deadline = Instant::now() + RESPONSE_TIMEOUT;
        let mut id = 0u8;
        loop {
            id = id.wrapping_add(1);
            self.send(9, &[id])?;
            match self.response(9, Duration::from_secs(1)) {
                Ok(data) if data == [id] => break,
                Ok(_) | Err(Error::Timeout) if Instant::now() < deadline => continue,
                Ok(_) => return Err(Error::Protocol("Bootloader ping ID mismatch".into())),
                Err(e) => return Err(e),
            }
        }
        self.request(2, &[0, 0])?;
        let mtu = self.request(7, &[])?;
        if mtu.len() != 2 {
            return Err(Error::Protocol("Malformed serial MTU response".into()));
        }
        self.mtu = u16::from_le_bytes([mtu[0], mtu[1]]) as usize;
        if self.mtu < 5 || self.mtu > 4096 {
            return Err(Error::Protocol(format!(
                "Invalid bootloader serial MTU {}",
                self.mtu
            )));
        }
        Ok(())
    }
    fn select(&mut self, kind: u8) -> Result<(usize, usize, u32)> {
        let r = self.request(6, &[kind])?;
        if r.len() != 12 {
            return Err(Error::Protocol(
                "Malformed object selection response".into(),
            ));
        }
        let size = le32(&r[0..4]) as usize;
        if size == 0 || size > MAX_IMAGE {
            return Err(Error::Protocol("Invalid bootloader object size".into()));
        }
        Ok((size, le32(&r[4..8]) as usize, le32(&r[8..12])))
    }
    fn create(&mut self, kind: u8, size: usize) -> Result<()> {
        let mut payload = [0u8; 5];
        payload[0] = kind;
        payload[1..].copy_from_slice(&(size as u32).to_le_bytes());
        self.request(1, &payload)?;
        Ok(())
    }
    fn checksum(&mut self, offset: usize, crc: u32) -> Result<()> {
        let r = self.request(3, &[])?;
        if r.len() != 8 {
            return Err(Error::Protocol("Malformed checksum response".into()));
        }
        if le32(&r[..4]) as usize != offset || le32(&r[4..]) != crc {
            return Err(Error::Protocol(format!(
                "DFU checksum/offset mismatch at byte {offset}; transfer stopped without activation"
            )));
        }
        Ok(())
    }
    fn stream(&mut self, bytes: &[u8], offset: usize, crc: &mut crc32fast::Hasher) -> Result<()> {
        let chunk_size = (self.mtu - 1) / 2 - 1;
        for chunk in bytes.chunks(chunk_size) {
            self.send(8, chunk)?;
            crc.update(chunk);
        }
        self.checksum(offset + bytes.len(), crc.clone().finalize())
    }
    fn execute(&mut self, final_object: bool, resumed_boundary: bool) -> Result<bool> {
        self.send(4, &[])?;
        match self.response(4, RESPONSE_TIMEOUT) {
            Ok(_) => Ok(false),
            // A validated boundary may already have been executed in a prior
            // connection. Operation-not-permitted then means nothing to execute.
            Err(Error::Rejected(8, _)) if resumed_boundary => Ok(false),
            Err(Error::Io(e)) if final_object && disconnected(&e) => Ok(true),
            Err(e) => Err(e),
        }
    }
    fn init(&mut self, init: &[u8]) -> Result<()> {
        let (max, mut offset, crc) = self.select(1)?;
        if init.len() > max {
            return Err(Error::Protocol(
                "Init packet exceeds bootloader command-object capacity".into(),
            ));
        }
        if offset > init.len() || crc32fast::hash(&init[..offset.min(init.len())]) != crc {
            offset = 0;
        }
        let resumed = offset == init.len();
        if offset == 0 {
            self.create(1, init.len())?;
        }
        let mut hasher = crc32fast::Hasher::new();
        hasher.update(&init[..offset]);
        self.stream(&init[offset..], offset, &mut hasher)?;
        self.execute(false, resumed)?;
        Ok(())
    }
    fn firmware(&mut self, bytes: &[u8], progress: &mut dyn FnMut(usize)) -> Result<bool> {
        let (max, mut offset, crc) = self.select(2)?;
        if offset > bytes.len() {
            return Err(Error::Protocol(
                "Bootloader resume offset exceeds selected image".into(),
            ));
        }
        let valid = crc32fast::hash(&bytes[..offset]) == crc;
        if !valid && offset % max == 0 {
            return Err(Error::Protocol(
                "Committed bootloader data does not match this image; refusing unsafe resume"
                    .into(),
            ));
        }
        let mut hasher = crc32fast::Hasher::new();
        if !valid {
            // CREATE discards only the current unexecuted object. Check the
            // committed prefix before writing; do not restart over committed data.
            offset -= offset % max;
            self.create(2, (bytes.len() - offset).min(max))?;
            hasher.update(&bytes[..offset]);
            self.checksum(offset, hasher.clone().finalize())?;
            let end = (offset + max).min(bytes.len());
            self.stream(&bytes[offset..end], offset, &mut hasher)?;
            offset = end;
            let rebooted = self.execute(offset == bytes.len(), false)?;
            progress(offset);
            if offset == bytes.len() {
                return Ok(rebooted);
            }
        } else {
            hasher.update(&bytes[..offset]);
            if offset > 0 {
                let boundary = offset % max == 0 || offset == bytes.len();
                if !boundary {
                    let end = ((offset / max + 1) * max).min(bytes.len());
                    self.stream(&bytes[offset..end], offset, &mut hasher)?;
                    offset = end;
                } else {
                    self.checksum(offset, hasher.clone().finalize())?;
                }
                let rebooted = self.execute(offset == bytes.len(), boundary)?;
                progress(offset);
                if offset == bytes.len() {
                    return Ok(rebooted);
                }
            }
        }
        while offset < bytes.len() {
            let end = (offset + max).min(bytes.len());
            self.create(2, end - offset)?;
            self.stream(&bytes[offset..end], offset, &mut hasher)?;
            offset = end;
            let rebooted = self.execute(offset == bytes.len(), false)?;
            progress(offset);
            if offset == bytes.len() {
                return Ok(rebooted);
            }
        }
        Ok(false)
    }
}
fn le32(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}
fn disconnected(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::NotConnected
            | io::ErrorKind::UnexpectedEof
    ) || if cfg!(windows) {
        matches!(error.raw_os_error(), Some(6 | 1167))
    } else {
        matches!(error.raw_os_error(), Some(5 | 19))
    }
}
fn result_error(code: u8) -> &'static str {
    match code {
        2 => "opcode unsupported",
        3 => "invalid parameter",
        4 => "insufficient resources",
        5 => "invalid object",
        7 => "unsupported type",
        8 => "operation not permitted",
        10 => "operation failed",
        _ => "unknown DFU result",
    }
}
fn extended_error(code: u8) -> &'static str {
    match code {
        0 => "no extended error",
        1 => "invalid error code",
        2 => "wrong command format",
        3 => "unknown command",
        4 => "invalid init command",
        5 => "firmware version too low",
        6 => "hardware version mismatch",
        7 => "SoftDevice version mismatch",
        8 => "signature missing",
        9 => "wrong hash type",
        10 => "hash failed",
        11 => "wrong signature type",
        12 => "verification failed",
        13 => "insufficient space",
        _ => "unknown extended error",
    }
}

/// Bounded application-console handshake from ota-web console-dfu.js. This only
/// requests entry; bootloader identification and selection are separate UI steps.
pub fn enter_dfu(
    port: &str,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(Progress),
) -> std::result::Result<(), String> {
    check_cancel(cancel).map_err(|e| e.to_string())?;
    let mut serial = open(port)?;
    let result = console_handshake(&mut serial, cancel, progress);
    result.map_err(|e| e.to_string())
}
fn console_handshake<T: Transport>(
    io: &mut T,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(Progress),
) -> Result<()> {
    let mut line = Vec::with_capacity(512);
    let ready_deadline = Instant::now() + Duration::from_secs(8);
    let mut next_probe = Instant::now();
    let mut ready = false;
    report(progress, "Waiting for the receiver console", 0, 0);
    while !ready {
        check_cancel(cancel)?;
        if Instant::now() >= ready_deadline {
            return Err(Error::Protocol(
                "Console did not answer uptime within 8 seconds; DFU was not sent".into(),
            ));
        }
        if Instant::now() >= next_probe {
            io.writing()?;
            io.write_all(b"\r\nuptime\r\n")?;
            next_probe = Instant::now() + Duration::from_millis(500);
        }
        console_lines(io, &mut line, &mut |text| {
            ready |= text.strip_prefix("Uptime:").is_some_and(|v| {
                v.trim_start()
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_digit)
            });
        })?;
    }
    check_cancel(cancel)?;
    line.clear();
    report(progress, "Console ready; requesting DFU", 0, 0);
    io.writing()?;
    let write_error = io.write_all(b"dfu\r\n").err();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        check_cancel(cancel)?;
        let mut entered = false;
        let mut rejected = false;
        let read = console_lines(io, &mut line, &mut |text| {
            entered |= text
                .strip_prefix("Entering DFU bootloader")
                .is_some_and(|s| s.len() <= 3 && s.bytes().all(|b| b == b'.'));
            rejected |= text == "DFU not available on this build"
                || text == "Unknown command"
                || text.starts_with("Unknown dfu argument:");
        });
        if rejected {
            return Err(Error::Protocol(
                "Receiver rejected DFU entry; this build may not support it".into(),
            ));
        }
        if entered || matches!(&read, Err(Error::Io(e)) if disconnected(e)) {
            report(
                progress,
                "DFU entry acknowledged or USB disconnected. Select the reenumerated Open DFU Bootloader port.",
                0,
                0,
            );
            return Ok(());
        }
        read?;
        if Instant::now() >= deadline {
            return Err(write_error.map(Error::Io).unwrap_or_else(|| Error::Protocol("No DFU acknowledgement or disconnect within 5 seconds; do not assume entry succeeded".into())));
        }
    }
}
fn console_lines<T: Transport>(
    io: &mut T,
    line: &mut Vec<u8>,
    on_line: &mut dyn FnMut(&str),
) -> Result<()> {
    io.reading()?;
    let mut buf = [0u8; 256];
    let count = match io.read(&mut buf) {
        Ok(0) => {
            return Err(
                io::Error::new(io::ErrorKind::UnexpectedEof, "console disconnected").into(),
            );
        }
        Ok(n) => n,
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) =>
        {
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    };
    for &b in &buf[..count] {
        if b == b'\r' || b == b'\n' {
            let text = strip_ansi(line);
            on_line(text.trim());
            line.clear();
        } else {
            if line.len() == 512 {
                line.remove(0);
            }
            line.push(b);
        }
    }
    Ok(())
}
fn strip_ansi(line: &[u8]) -> String {
    let mut out = Vec::with_capacity(line.len());
    let mut i = 0;
    while i < line.len() {
        if line[i] == 0x1b && line.get(i + 1) == Some(&b'[') {
            let start = i;
            i += 2;
            while i < line.len() && (line[i].is_ascii_digit() || line[i] == b';') {
                i += 1;
            }
            if line.get(i) == Some(&b'm') {
                i += 1;
                continue;
            }
            i = start;
        }
        out.push(line[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::{
        Error, Options, Session, Transport, bootloader_identity, console_handshake, generate_init,
        parse_images, revalidate_bootloader_identity, validate_init,
    };
    use crate::domain::{InterfaceDescription, SerialEndpoint, SerialPortKind, UsbInterface};
    use std::collections::VecDeque;
    use std::io::{self, Cursor, Read, Write};
    use std::sync::atomic::{AtomicBool, Ordering};

    fn bootloader_endpoint() -> SerialEndpoint {
        SerialEndpoint {
            port_name: "COM10".into(),
            label: "COM10 — Open DFU Bootloader".into(),
            manufacturer: Some("Microsoft".into()),
            product: Some("USB串行设备 (COM10)".into()),
            kind: SerialPortKind::Usb,
            serial_number: Some("SYNTHETIC-OPENDFU".into()),
            vid: Some(0x1915),
            pid: Some(0x521f),
            usb_interface: Some(UsbInterface {
                number: Some(0),
                description: InterfaceDescription::Value("Open DFU Bootloader".into()),
            }),
        }
    }

    #[test]
    fn native_bootloader_description_passes_the_protocol_identity_boundary() {
        let expected = bootloader_endpoint();
        let current = bootloader_identity(&expected.port_name, Ok(vec![expected.clone()])).unwrap();
        revalidate_bootloader_identity(&expected, &current).unwrap();
        assert_eq!(current.product, expected.product);
        assert_eq!(current.manufacturer, expected.manufacturer);
        assert_eq!(current.usb_interface, expected.usb_interface);

        let mut fallback = expected.clone();
        fallback.product = Some("Open DFU Bootloader".into());
        fallback.usb_interface.as_mut().unwrap().description = InterfaceDescription::Unavailable;
        let current = bootloader_identity(&fallback.port_name, Ok(vec![fallback.clone()])).unwrap();
        revalidate_bootloader_identity(&fallback, &current).unwrap();
    }

    #[test]
    fn protocol_identity_rejects_missing_failed_duplicate_and_changed_port_enumeration() {
        let expected = bootloader_endpoint();
        assert!(bootloader_identity(&expected.port_name, Ok(Vec::new())).is_err());
        assert!(
            bootloader_identity(
                &expected.port_name,
                Err("synthetic enumeration failure".into())
            )
            .is_err()
        );
        assert!(
            bootloader_identity(
                &expected.port_name,
                Ok(vec![expected.clone(), expected.clone()])
            )
            .is_err()
        );
        let mut moved = expected.clone();
        moved.port_name = "COM11".into();
        assert!(bootloader_identity(&expected.port_name, Ok(vec![moved])).is_err());
        let alias = SerialEndpoint {
            port_name: r"\\.\com10".into(),
            ..expected.clone()
        };
        let alias_result = bootloader_identity(&expected.port_name, Ok(vec![alias]));
        assert_eq!(alias_result.is_ok(), cfg!(windows));
        if let Ok(alias) = alias_result {
            revalidate_bootloader_identity(&expected, &alias).unwrap();
            assert!(
                bootloader_identity(&expected.port_name, Ok(vec![expected.clone(), alias]))
                    .is_err()
            );
        }
    }

    #[test]
    fn protocol_identity_rejects_unproven_or_ambiguous_bootloader_metadata() {
        let expected = bootloader_endpoint();
        let mut generic = expected.clone();
        generic.usb_interface.as_mut().unwrap().description = InterfaceDescription::Unavailable;
        assert!(bootloader_identity(&expected.port_name, Ok(vec![generic.clone()])).is_err());
        assert!(revalidate_bootloader_identity(&generic, &expected).is_err());
        generic.product = None;
        generic.manufacturer = Some("Open DFU Bootloader".into());
        assert!(bootloader_identity(&expected.port_name, Ok(vec![generic.clone()])).is_err());
        generic.manufacturer = None;
        assert!(bootloader_identity(&expected.port_name, Ok(vec![generic])).is_err());
        for description in [
            InterfaceDescription::Ambiguous,
            InterfaceDescription::Value("SlimeNRF Raw Data CDC".into()),
        ] {
            let mut conflict = expected.clone();
            conflict.product = Some("Open DFU Bootloader".into());
            conflict.usb_interface.as_mut().unwrap().description = description;
            assert!(bootloader_identity(&expected.port_name, Ok(vec![conflict])).is_err());
        }
    }

    #[test]
    fn repeated_protocol_identity_checks_reject_replacement_at_the_same_port() {
        let expected = bootloader_endpoint();
        let current = bootloader_identity(&expected.port_name, Ok(vec![expected.clone()])).unwrap();
        revalidate_bootloader_identity(&expected, &current).unwrap();

        let mut replacements = Vec::new();
        let mut replacement = expected.clone();
        replacement.serial_number = Some("SYNTHETIC-REPLACEMENT".into());
        replacements.push(replacement);
        let mut replacement = expected.clone();
        replacement.vid = Some(0x1209);
        replacements.push(replacement);
        let mut replacement = expected.clone();
        replacement.pid = Some(0x7690);
        replacements.push(replacement);
        let mut replacement = expected.clone();
        replacement.product = Some("Changed raw product".into());
        replacements.push(replacement);
        let mut replacement = expected.clone();
        replacement.usb_interface.as_mut().unwrap().number = Some(2);
        replacements.push(replacement);
        let mut replacement = expected.clone();
        replacement.usb_interface.as_mut().unwrap().description =
            InterfaceDescription::Value("Open DFU Bootloader replacement".into());
        replacements.push(replacement);
        for replacement in replacements {
            let current =
                bootloader_identity(&expected.port_name, Ok(vec![replacement.clone()])).unwrap();
            assert!(
                revalidate_bootloader_identity(&expected, &current).is_err(),
                "{replacement:?}"
            );
        }
    }

    /// Stateful SDK-style object emulator. Both directions use actual SLIP;
    /// reads fragment escapes and writes may contain every possible byte.
    #[derive(Default)]
    struct Bootloader {
        replies: VecDeque<u8>,
        command: Vec<u8>,
        data: Vec<u8>,
        committed: usize,
        kind: u8,
        created_size: usize,
        packets: Vec<Vec<u8>>,
        reject: Option<(u8, u8)>,
        bad_checksum: bool,
        disconnect_final: bool,
        fail_execute_write: bool,
        image_size: usize,
        fragment: usize,
        max: usize,
        executed: usize,
    }
    impl Bootloader {
        fn new(image_size: usize) -> Self {
            Self {
                image_size,
                fragment: 3,
                max: 32,
                ..Self::default()
            }
        }
        fn reply(&mut self, op: u8, result: u8, bytes: &[u8]) {
            for b in [0x60, op, result].iter().chain(bytes) {
                match b {
                    0xc0 => self.replies.extend([0xdb, 0xdc]),
                    0xdb => self.replies.extend([0xdb, 0xdd]),
                    _ => self.replies.push_back(*b),
                }
            }
            self.replies.push_back(0xc0);
        }
        fn selected(&self) -> &[u8] {
            if self.kind == 1 {
                &self.command
            } else {
                &self.data
            }
        }
        fn packet(&mut self, p: Vec<u8>) -> io::Result<()> {
            let op = p[0];
            self.packets.push(p.clone());
            if self.reject.is_some_and(|(code, _)| code == op) {
                let (_, result) = self.reject.take().unwrap();
                self.reply(op, result, &[6]);
                return Ok(());
            }
            match op {
                9 => self.reply(op, 1, &p[1..]),
                2 => {
                    assert_eq!(&p[1..], &[0, 0]);
                    self.reply(op, 1, &[]);
                }
                7 => self.reply(op, 1, &23u16.to_le_bytes()),
                6 => {
                    self.kind = p[1];
                    let max = if self.kind == 1 { 4096 } else { self.max };
                    let selected = self.selected();
                    let mut r = (max as u32).to_le_bytes().to_vec();
                    r.extend_from_slice(&(selected.len() as u32).to_le_bytes());
                    r.extend_from_slice(&crc32fast::hash(selected).to_le_bytes());
                    self.reply(op, 1, &r);
                }
                1 => {
                    self.kind = p[1];
                    self.created_size = u32::from_le_bytes(p[2..6].try_into().unwrap()) as usize;
                    if self.kind == 1 {
                        self.command.clear();
                    } else {
                        self.data.truncate(self.committed);
                    }
                    self.reply(op, 1, &[]);
                }
                8 => {
                    if self.kind == 1 {
                        self.command.extend_from_slice(&p[1..]);
                    } else {
                        self.data.extend_from_slice(&p[1..]);
                    }
                }
                3 => {
                    let selected = self.selected();
                    let mut r = (selected.len() as u32).to_le_bytes().to_vec();
                    let crc = crc32fast::hash(selected) ^ u32::from(self.bad_checksum);
                    r.extend_from_slice(&crc.to_le_bytes());
                    self.reply(op, 1, &r);
                }
                4 => {
                    if self.fail_execute_write {
                        return Err(io::Error::new(
                            io::ErrorKind::BrokenPipe,
                            "execute write failed",
                        ));
                    }
                    self.executed += 1;
                    if self.kind == 2 {
                        self.committed = self.data.len();
                    }
                    if !(self.kind == 2
                        && self.data.len() == self.image_size
                        && self.disconnect_final)
                    {
                        self.reply(op, 1, &[]);
                    }
                }
                _ => panic!("unexpected opcode {op}"),
            }
            Ok(())
        }
    }
    impl Read for Bootloader {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            if self.replies.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "emulated disconnect",
                ));
            }
            let count = out.len().min(self.fragment).min(self.replies.len());
            for b in &mut out[..count] {
                *b = self.replies.pop_front().unwrap();
            }
            Ok(count)
        }
    }
    impl Write for Bootloader {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            assert_eq!(data.last(), Some(&0xc0));
            let mut packet = Vec::new();
            let mut escaped = false;
            for &b in &data[..data.len() - 1] {
                if escaped {
                    packet.push(match b {
                        0xdc => 0xc0,
                        0xdd => 0xdb,
                        _ => panic!("bad outgoing SLIP"),
                    });
                    escaped = false;
                } else if b == 0xdb {
                    escaped = true;
                } else {
                    assert_ne!(b, 0xc0);
                    packet.push(b);
                }
            }
            assert!(!escaped);
            self.packet(packet)?;
            Ok(data.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            panic!("DFU must not use potentially unbounded flush")
        }
    }
    impl Transport for Bootloader {}

    #[test]
    fn complete_transfer_uses_slip_crc_objects_and_activation() {
        let bytes: Vec<u8> = (0..=255).collect();
        let init = generate_init(&bytes, &Options::default());
        validate_init(&init, &bytes, "application").unwrap();
        let cancel = AtomicBool::new(false);
        let mut boot = Bootloader::new(bytes.len());
        let mut progress = Vec::new();
        {
            let mut session = Session::new(&mut boot, &cancel);
            session.connect().unwrap();
            session.init(&init).unwrap();
            assert!(!session.firmware(&bytes, &mut |n| progress.push(n)).unwrap());
        }
        assert_eq!(boot.command, init);
        assert_eq!(boot.data, bytes);
        assert_eq!(boot.executed, 9);
        assert_eq!(progress.last(), Some(&256));
        assert!(
            boot.packets
                .iter()
                .filter(|p| p[0] == 8)
                .all(|p| p.len() <= 11)
        );
    }

    #[test]
    fn resumes_init_and_partial_data_without_replaying_committed_prefix() {
        let bytes: Vec<u8> = (0..100).collect();
        let init = generate_init(&bytes, &Options::default());
        let mut boot = Bootloader::new(bytes.len());
        boot.command.extend_from_slice(&init[..13]);
        boot.data.extend_from_slice(&bytes[..45]);
        boot.committed = 32;
        let cancel = AtomicBool::new(false);
        {
            let mut session = Session::new(&mut boot, &cancel);
            session.connect().unwrap();
            session.init(&init).unwrap();
            session.firmware(&bytes, &mut |_| {}).unwrap();
        }
        assert_eq!(boot.command, init);
        assert_eq!(boot.data, bytes);
        let create: Vec<_> = boot.packets.iter().filter(|p| p[0] == 1).collect();
        assert_eq!(create.len(), 2);
        assert!(create.iter().all(|p| p[1] == 2));
    }

    #[test]
    fn corrupt_unexecuted_object_rolls_back_only_to_verified_boundary() {
        let bytes: Vec<u8> = (0..100).collect();
        let mut boot = Bootloader::new(bytes.len());
        boot.data.extend_from_slice(&bytes[..45]);
        boot.data[44] ^= 1;
        boot.committed = 32;
        let cancel = AtomicBool::new(false);
        Session::new(&mut boot, &cancel)
            .firmware(&bytes, &mut |_| {})
            .unwrap();
        assert_eq!(boot.data, bytes);
        assert_eq!(boot.executed, 3);
    }

    #[test]
    fn corrupt_committed_prefix_and_out_of_range_resume_are_rejected() {
        let bytes = vec![7; 64];
        let cancel = AtomicBool::new(false);
        for offset in [32, 65] {
            let mut boot = Bootloader::new(bytes.len());
            boot.data = vec![9; offset];
            boot.committed = offset;
            assert!(
                Session::new(&mut boot, &cancel)
                    .firmware(&bytes, &mut |_| {})
                    .is_err()
            );
            assert_eq!(boot.executed, 0);
            assert!(!boot.packets.iter().any(|p| p[0] == 8));
        }
    }

    #[test]
    fn verified_final_reboot_is_distinct_from_failed_execute_write() {
        let bytes = vec![0xc0; 12];
        let cancel = AtomicBool::new(false);
        let mut boot = Bootloader::new(bytes.len());
        boot.disconnect_final = true;
        assert!(
            Session::new(&mut boot, &cancel)
                .firmware(&bytes, &mut |_| {})
                .unwrap()
        );
        let mut boot = Bootloader::new(bytes.len());
        boot.fail_execute_write = true;
        assert!(
            Session::new(&mut boot, &cancel)
                .firmware(&bytes, &mut |_| {})
                .is_err()
        );
        assert_eq!(boot.executed, 0);
    }

    #[test]
    fn checksum_and_extended_error_stop_without_execute() {
        let bytes = vec![1; 32];
        let cancel = AtomicBool::new(false);
        let mut boot = Bootloader::new(bytes.len());
        boot.bad_checksum = true;
        assert!(
            Session::new(&mut boot, &cancel)
                .firmware(&bytes, &mut |_| {})
                .is_err()
        );
        assert_eq!(boot.executed, 0);
        let mut boot = Bootloader::new(bytes.len());
        boot.reject = Some((1, 11));
        let error = Session::new(&mut boot, &cancel)
            .firmware(&bytes, &mut |_| {})
            .unwrap_err();
        assert!(error.to_string().contains("hardware version mismatch"));
        assert_eq!(boot.executed, 0);
    }

    #[test]
    fn cancellation_between_objects_does_not_activate_or_replay() {
        let bytes = vec![2; 64];
        let cancel = AtomicBool::new(false);
        let mut boot = Bootloader::new(bytes.len());
        let result = Session::new(&mut boot, &cancel).firmware(&bytes, &mut |_| {
            cancel.store(true, Ordering::Release);
        });
        assert!(matches!(result, Err(Error::Cancelled)));
        assert_eq!(boot.data.len(), 32);
        assert_eq!(boot.executed, 1);
    }

    #[test]
    fn already_complete_resume_only_checks_and_executes() {
        let bytes = vec![3; 45];
        let cancel = AtomicBool::new(false);
        let mut boot = Bootloader::new(bytes.len());
        boot.data = bytes.clone();
        boot.committed = bytes.len();
        Session::new(&mut boot, &cancel)
            .firmware(&bytes, &mut |_| {})
            .unwrap();
        assert!(!boot.packets.iter().any(|p| p[0] == 8 || p[0] == 1));
        assert_eq!(boot.executed, 1);
    }

    #[test]
    fn secure_init_rejects_legacy_wrong_hash_and_wrong_manifest_kind() {
        let bytes = vec![4; 100];
        let init = generate_init(&bytes, &Options::default());
        assert!(validate_init(&[52, 0, 0, 0, 1, 0], &bytes, "application").is_err());
        assert!(validate_init(&init, &[5; 100], "application").is_err());
        assert!(validate_init(&init, &bytes, "bootloader").is_err());
        assert!(validate_init(&init, &[4; 99], "application").is_err());
    }

    fn hex_record(address: u16, bytes: &[u8]) -> String {
        let mut record = vec![bytes.len() as u8, (address >> 8) as u8, address as u8, 0];
        record.extend_from_slice(bytes);
        let checksum = 0u8.wrapping_sub(record.iter().fold(0u8, |sum, &b| sum.wrapping_add(b)));
        record.push(checksum);
        let encoded: String = record.iter().map(|b| format!("{b:02X}")).collect();
        format!(":{encoded}\n")
    }

    #[test]
    fn hex_drops_mbr_and_never_relocates_an_unknown_application_base() {
        let app = [0x00, 0x10, 0x00, 0x20, 0x09, 0x10, 0x00, 0x00];
        let mixed = format!(
            "{}{}:00000001FF\n",
            hex_record(0, &[1; 16]),
            hex_record(0x1000, &app)
        );
        let images = parse_images("holyiot.hex", mixed.as_bytes(), &Options::default()).unwrap();
        assert_eq!(images[0].firmware, app);
        let crossing = format!(
            "{}:00000001FF\n",
            hex_record(0x0ffc, &[1, 2, 3, 4, 5, 6, 7, 8])
        );
        assert_eq!(
            parse_images("crossing.hex", crossing.as_bytes(), &Options::default()).unwrap()[0]
                .firmware,
            [5, 6, 7, 8]
        );
        let higher = format!("{}:00000001FF\n", hex_record(0x2000, &app));
        assert!(parse_images("other-base.hex", higher.as_bytes(), &Options::default()).is_err());
        let known_other_bootloader = Options {
            app_base: 0x2000,
            ..Options::default()
        };
        assert_eq!(
            parse_images("known-base.hex", higher.as_bytes(), &known_other_bootloader).unwrap()[0]
                .firmware,
            app
        );
    }

    #[test]
    fn init_packet_matches_nordic_protobuf_sha256_vector() {
        // SHA256("abc"), reversed per Nordic dfu-cc.proto; hw52/fw1/sd0.
        let expected = [
            0x0a, 0x35, 0x08, 0x01, 0x12, 0x31, 0x08, 0x01, 0x10, 0x34, 0x1a, 0x01, 0x00, 0x20,
            0x00, 0x38, 0x03, 0x42, 0x24, 0x08, 0x03, 0x12, 0x20, 0xad, 0x15, 0x00, 0xf2, 0x61,
            0xff, 0x10, 0xb4, 0x9c, 0x7a, 0x17, 0x96, 0xa3, 0x61, 0x03, 0xb0, 0x23, 0x22, 0xae,
            0x5d, 0xde, 0x40, 0x41, 0x41, 0xea, 0xcf, 0x01, 0x8f, 0xbf, 0x16, 0x78, 0xba,
        ];
        assert_eq!(generate_init(b"abc", &Options::default()), expected);
    }

    fn package(bin: &[u8], dat: &[u8], bin_name: &str) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        writer.start_file("manifest.json", options).unwrap();
        let manifest = serde_json::json!({"manifest": {"application": {"bin_file": bin_name, "dat_file": "app.dat"}}});
        writer.write_all(manifest.to_string().as_bytes()).unwrap();
        writer.start_file(bin_name, options).unwrap();
        writer.write_all(bin).unwrap();
        writer.start_file("app.dat", options).unwrap();
        writer.write_all(dat).unwrap();
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn secure_zip_extracts_real_members_and_rejects_traversal_or_legacy_init() {
        let bin = vec![0xdb; 128];
        let options = Options::default();
        let init = generate_init(&bin, &options);
        let bytes = package(&bin, &init, "app.bin");
        let parsed = parse_images("firmware.zip", &bytes, &options).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].firmware, bin);
        assert_eq!(parsed[0].init, init);
        assert!(
            parse_images(
                "legacy.zip",
                &package(&bin, &[52, 0, 0, 0, 1, 0], "app.bin"),
                &options
            )
            .is_err()
        );
        assert!(parse_images("unsafe.zip", &package(&bin, &init, "../app.bin"), &options).is_err());
    }

    #[test]
    fn secure_zip_rejects_unsafe_envelopes_and_false_member_sizes() {
        let options = Options::default();
        let bin = [1, 2, 3, 4];
        let bytes = package(&bin, &generate_init(&bin, &options), "app.bin");
        let end = bytes.len() - 22;
        for (field, value) in [(8, 129u16), (10, 129), (4, 1), (6, 1), (10, u16::MAX)] {
            let mut malformed = bytes.clone();
            malformed[end + field..end + field + 2].copy_from_slice(&value.to_le_bytes());
            assert!(parse_images("secure.zip", &malformed, &options).is_err());
        }
        let central = u32::from_le_bytes(bytes[end + 16..end + 20].try_into().unwrap()) as usize;
        let local =
            u32::from_le_bytes(bytes[central + 42..central + 46].try_into().unwrap()) as usize;
        let size = u32::from_le_bytes(bytes[central + 24..central + 28].try_into().unwrap());
        for declared in [0, size - 1, size + 1] {
            let mut malformed = bytes.clone();
            malformed[local + 22..local + 26].copy_from_slice(&declared.to_le_bytes());
            malformed[central + 24..central + 28].copy_from_slice(&declared.to_le_bytes());
            assert!(parse_images("secure.zip", &malformed, &options).is_err());
        }
    }

    #[test]
    fn malformed_slip_mtu_and_response_deadline_are_errors() {
        let cancel = AtomicBool::new(false);
        let mut boot = Bootloader::new(12);
        boot.replies.extend([0x60, 9, 1, 0xdb, 0xff, 0xc0]);
        assert!(matches!(
            Session::new(&mut boot, &cancel).response(9, std::time::Duration::from_secs(1)),
            Err(Error::Protocol(_))
        ));
        let mut boot = Bootloader::new(12);
        assert!(matches!(
            Session::new(&mut boot, &cancel).response(9, std::time::Duration::ZERO),
            Err(Error::Timeout)
        ));
        let mut boot = Bootloader::new(12);
        boot.max = 0;
        assert!(
            Session::new(&mut boot, &cancel)
                .firmware(&[1; 12], &mut |_| {})
                .is_err()
        );
    }

    struct Console {
        rx: VecDeque<u8>,
        writes: Vec<Vec<u8>>,
        reject: bool,
    }
    impl Read for Console {
        fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
            if self.rx.is_empty() {
                return Err(io::Error::new(io::ErrorKind::BrokenPipe, "console reset"));
            }
            // Exercise fragmented ANSI and lines.
            let n = b.len().min(3).min(self.rx.len());
            for byte in &mut b[..n] {
                *byte = self.rx.pop_front().unwrap();
            }
            Ok(n)
        }
    }
    impl Write for Console {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.writes.push(b.to_vec());
            if b == b"\r\nuptime\r\n" {
                self.rx
                    .extend(b"uptime\r\nSlimeNRF Receiver\r\n\x1b[32mUptime: 10 s\x1b[0m\r\n");
            } else {
                assert_eq!(b, b"dfu\r\n");
                if self.reject {
                    self.rx.extend(b"DFU not available on this build\r\n");
                } else {
                    self.rx.extend(b"Entering DFU bootloader...\r\n");
                }
            }
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            panic!("no flush")
        }
    }
    impl Transport for Console {}

    #[test]
    fn console_probes_uptime_before_dfu_and_preserves_rejection() {
        let cancel = AtomicBool::new(false);
        for reject in [false, true] {
            let mut console = Console {
                rx: VecDeque::new(),
                writes: Vec::new(),
                reject,
            };
            let result = console_handshake(&mut console, &cancel, &mut |_| {});
            assert_eq!(result.is_err(), reject);
            assert_eq!(
                console.writes,
                [b"\r\nuptime\r\n".to_vec(), b"dfu\r\n".to_vec()]
            );
        }
    }
}
