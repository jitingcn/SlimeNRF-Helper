use std::borrow::Cow;

#[derive(Clone, Debug)]
pub struct SerialEndpoint {
    pub port_name: String,
    pub label: String,
    pub manufacturer: Option<Box<str>>,
    pub product: Option<Box<str>>,
    pub kind: SerialPortKind,
    pub serial_number: Option<String>,
    pub vid: Option<u16>,
    pub pid: Option<u16>,
    pub usb_interface: Option<UsbInterface>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SerialPortKind {
    Usb,
    Bluetooth,
    Pci,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsbInterface {
    pub number: Option<u8>,
    pub description: InterfaceDescription,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InterfaceDescription {
    Unavailable,
    Value(Box<str>),
    Ambiguous,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SerialRole {
    Unclassified,
    ReceiverRawData,
    Ambiguous,
}

impl SerialEndpoint {
    pub fn role(&self) -> SerialRole {
        match self
            .usb_interface
            .as_ref()
            .map(|interface| &interface.description)
        {
            Some(InterfaceDescription::Ambiguous) => SerialRole::Ambiguous,
            Some(InterfaceDescription::Value(description))
                if self.kind == SerialPortKind::Usb
                    && self.vid == Some(0x1209)
                    && self.pid == Some(0x7690)
                    && description.trim() == "SlimeNRF Raw Data CDC" =>
            {
                SerialRole::ReceiverRawData
            }
            _ => SerialRole::Unclassified,
        }
    }

    /// Identify bootloader metadata without treating display labels as evidence.
    pub fn is_open_dfu_bootloader(&self) -> bool {
        if self.kind != SerialPortKind::Usb {
            return false;
        }
        let description = match self
            .usb_interface
            .as_ref()
            .map(|interface| &interface.description)
        {
            Some(InterfaceDescription::Ambiguous) => return false,
            Some(InterfaceDescription::Value(description)) => Some(description.as_ref()),
            Some(InterfaceDescription::Unavailable) | None => self.product.as_deref(),
        };
        const NAME: &[u8] = b"Open DFU Bootloader";
        description.is_some_and(|description| {
            description
                .as_bytes()
                .windows(NAME.len())
                .any(|part| part.eq_ignore_ascii_case(NAME))
        })
    }

    pub fn same_usb_interface(&self, other: &Self) -> bool {
        self.usb_interface == other.usb_interface && self.role() == other.role()
    }
}

#[derive(Clone, Debug)]
pub struct ReceiverEndpoint {
    /// Lossless control-interface path identity, never a USB serial or RF slot.
    pub key: String,
    /// Native HID path bytes without the trailing NUL.
    pub path: Vec<u8>,
    pub label: String,
    pub serial_number: Option<String>,
}

#[derive(Clone, Debug)]
pub struct TrackerLink {
    /// A tracker slot is meaningful only together with this receiver key.
    pub receiver_key: String,
    pub slot: u8,
    pub address: Option<String>,
    /// Sensor traffic observed recently (the passive monitor expires it after 3s).
    /// False means no recent sensor traffic, not proof of being offline.
    pub online: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Inventory {
    pub ports: Vec<SerialEndpoint>,
    pub receivers: Vec<ReceiverEndpoint>,
    /// A failed serial enumeration is unknown presence, not an empty roster.
    pub serial_enumeration_failed: bool,
    /// A backend failed while the other backend still produced usable inventory.
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct DeviceIdentity {
    pub name: Option<String>,
    pub role: DeviceRole,
    pub address: Option<String>,
    pub tracker_id: Option<u8>,
    pub target: Option<String>,
    pub board: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DeviceRole {
    Receiver,
    Tracker,
    #[default]
    Unknown,
}

impl DeviceIdentity {
    pub fn ingest_line(&mut self, line: &str) {
        let cleaned = clean_info_line(line);
        let line = cleaned.trim();
        // Log lines are not info responses, even if they mention an info label.
        if line.is_empty() || line.starts_with(['[', '<', '>']) {
            return;
        }
        let line = line.split('[').next().unwrap_or(line).trim();
        if let Some((label, value)) = line.split_once(':') {
            let label = label.trim();
            let value = value.trim();
            if value.is_empty() {
                return;
            }
            if label.eq_ignore_ascii_case("Device name") {
                self.name = Some(value.trim_matches('*').trim().to_owned());
            } else if label.eq_ignore_ascii_case("Board") {
                self.board = Some(value.to_owned());
            } else if label.eq_ignore_ascii_case("Target") {
                self.target = Some(value.to_owned());
            } else if label.eq_ignore_ascii_case("Tracker ID") {
                if let Ok(id) = value.parse::<u8>() {
                    self.tracker_id = Some(id);
                }
            } else if label.eq_ignore_ascii_case("Device address")
                && let Some(address) = normalize_address(value)
            {
                self.address = Some(address);
            }
        } else {
            let banner = line.trim_matches('*').trim();
            let branded = banner.split_whitespace().next().is_some_and(|word| {
                word.eq_ignore_ascii_case("SlimeVR") || word.eq_ignore_ascii_case("SlimeNRF")
            });
            if (line.starts_with('*') || branded)
                && banner.len() <= 64
                && role_marker(banner) != DeviceRole::Unknown
            {
                self.name = Some(banner.to_owned());
            }
        }
        self.role = DeviceRole::Unknown;
        for value in [
            self.name.as_deref(),
            self.target.as_deref(),
            self.board.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            match role_marker(value) {
                DeviceRole::Receiver => {
                    self.role = DeviceRole::Receiver;
                    break;
                }
                DeviceRole::Tracker => self.role = DeviceRole::Tracker,
                DeviceRole::Unknown => {}
            }
        }
    }
}

fn role_marker(value: &str) -> DeviceRole {
    let mut role = DeviceRole::Unknown;
    for token in value.split(|character: char| !character.is_ascii_alphanumeric()) {
        if token.eq_ignore_ascii_case("receiver") || token.eq_ignore_ascii_case("dongle") {
            return DeviceRole::Receiver;
        }
        if token.eq_ignore_ascii_case("tracker") {
            role = DeviceRole::Tracker;
        }
    }
    role
}

// Serial sessions may pass raw lines, so remove ANSI CSI sequences without an
// allocation for the usual plain-text info response.
fn clean_info_line(line: &str) -> Cow<'_, str> {
    if !line.chars().any(|c| c.is_control() && c != '\t') {
        return Cow::Borrowed(line);
    }
    let mut clean = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(character) = chars.next() {
        if character == '\u{1b}' {
            if let Some('[') = chars.next() {
                for escaped in chars.by_ref() {
                    if ('@'..='~').contains(&escaped) {
                        break;
                    }
                }
            }
        } else if !character.is_control() || character == '\t' {
            clean.push(character);
        }
    }
    Cow::Owned(clean)
}

/// Accept exactly six hex bytes; colon, hyphen and ASCII whitespace separators
/// are allowed. Other punctuation and text are rejected rather than stripped.
pub fn normalize_address(value: &str) -> Option<String> {
    String::from_utf8(address_digits(value)?.to_vec()).ok()
}

pub(crate) fn address_digits(value: &str) -> Option<[u8; 12]> {
    let mut normalized = [0_u8; 12];
    let mut length = 0;
    for byte in value.bytes() {
        if byte.is_ascii_hexdigit() {
            if length == normalized.len() {
                return None;
            }
            normalized[length] = byte.to_ascii_uppercase();
            length += 1;
        } else if byte != b':' && byte != b'-' && !byte.is_ascii_whitespace() {
            return None;
        }
    }
    if length != normalized.len() {
        return None;
    }
    Some(normalized)
}

#[derive(Clone, Copy, Debug)]
pub struct ReceiverSerialPorts<'a> {
    pub console: Option<&'a SerialEndpoint>,
    pub raw_data: Option<&'a SerialEndpoint>,
}

pub fn receiver_serial_ports<'a>(
    receiver: &ReceiverEndpoint,
    inventory: &'a Inventory,
) -> Option<ReceiverSerialPorts<'a>> {
    let serial = normalized_serial(receiver.serial_number.as_deref())?;
    if inventory.serial_enumeration_failed
        || inventory
            .receivers
            .iter()
            .filter(|candidate| serial_matches(candidate.serial_number.as_deref(), serial))
            .count()
            != 1
        || !inventory.receivers.iter().any(|candidate| {
            candidate.key == receiver.key
                && candidate.path == receiver.path
                && serial_matches(candidate.serial_number.as_deref(), serial)
        })
        || inventory.receivers.iter().any(|candidate| {
            (candidate.key == receiver.key || candidate.path == receiver.path)
                && !serial_matches(candidate.serial_number.as_deref(), serial)
        })
    {
        return None;
    }

    let matches_receiver = |port: &SerialEndpoint| {
        receiver_usb(port) && serial_matches(port.serial_number.as_deref(), serial)
    };
    let mut raw_data = None;
    let mut console = None;
    let mut console_count = 0;
    for (index, port) in inventory.ports.iter().enumerate() {
        if !matches_receiver(port) {
            continue;
        }
        let number = port
            .usb_interface
            .as_ref()
            .and_then(|interface| interface.number);
        if inventory.ports[..index]
            .iter()
            .filter(|port| matches_receiver(port))
            .any(|previous| {
                previous.port_name == port.port_name
                    || number.is_some_and(|number| {
                        previous
                            .usb_interface
                            .as_ref()
                            .and_then(|interface| interface.number)
                            == Some(number)
                    })
            })
        {
            return None;
        }
        match port.role() {
            SerialRole::Ambiguous => return None,
            SerialRole::ReceiverRawData => {
                if raw_data.replace(port).is_some() {
                    return None;
                }
            }
            SerialRole::Unclassified => {
                console_count += 1;
                console = Some(port);
            }
        }
    }
    Some(ReceiverSerialPorts {
        console: console.filter(|_| console_count == 1),
        raw_data,
    })
}

pub fn receiver_port<'a>(
    receiver: &ReceiverEndpoint,
    inventory: &'a Inventory,
) -> Option<&'a SerialEndpoint> {
    receiver_serial_ports(receiver, inventory)?.console
}

pub fn console_port_allowed(endpoint: &SerialEndpoint, inventory: &Inventory) -> bool {
    if inventory.serial_enumeration_failed || endpoint.role() != SerialRole::Unclassified {
        return false;
    }
    let mut current = inventory
        .ports
        .iter()
        .filter(|port| port.port_name == endpoint.port_name);
    let Some(current_port) = current.next() else {
        return false;
    };
    if current.next().is_some()
        || current_port.kind != endpoint.kind
        || current_port.vid != endpoint.vid
        || current_port.pid != endpoint.pid
        || current_port.serial_number != endpoint.serial_number
        || !current_port.same_usb_interface(endpoint)
    {
        return false;
    }
    if receiver_usb(current_port)
        && let Some(serial) = normalized_serial(current_port.serial_number.as_deref())
        && let Some(receiver) = inventory
            .receivers
            .iter()
            .find(|receiver| serial_matches(receiver.serial_number.as_deref(), serial))
    {
        return receiver_port(receiver, inventory)
            .is_some_and(|console| std::ptr::eq(console, current_port));
    }
    true
}

fn receiver_usb(port: &SerialEndpoint) -> bool {
    port.kind == SerialPortKind::Usb && port.vid == Some(0x1209) && port.pid == Some(0x7690)
}

fn normalized_serial(serial: Option<&str>) -> Option<&str> {
    serial.map(str::trim).filter(|serial| !serial.is_empty())
}

fn serial_matches(candidate: Option<&str>, serial: &str) -> bool {
    normalized_serial(candidate).is_some_and(|candidate| candidate.eq_ignore_ascii_case(serial))
}

/// Match only observed exact addresses, never local tracker IDs, names, RF
/// channels, BLE address offsets, or guessed USB serial transformations.
pub fn tracker_port<'a>(
    tracker: &TrackerLink,
    identities: impl IntoIterator<Item = (&'a str, &'a DeviceIdentity)>,
) -> Option<&'a str> {
    let address = address_digits(tracker.address.as_deref()?)?;
    let mut matched = None;
    for (port, identity) in identities {
        if identity.address.as_deref().and_then(address_digits) == Some(address) {
            if matched.is_some() {
                return None;
            }
            matched = Some(port);
        }
    }
    matched
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receiver(serial: Option<&str>) -> ReceiverEndpoint {
        ReceiverEndpoint {
            key: "hid:one".into(),
            path: b"one".to_vec(),
            label: "Receiver".into(),
            serial_number: serial.map(str::to_owned),
        }
    }

    fn port(name: &str, serial: Option<&str>) -> SerialEndpoint {
        SerialEndpoint {
            port_name: name.into(),
            label: "Receiver".into(),
            manufacturer: None,
            product: None,
            kind: SerialPortKind::Usb,
            serial_number: serial.map(str::to_owned),
            vid: Some(0x1209),
            pid: Some(0x7690),
            usb_interface: None,
        }
    }

    fn inventory(ports: Vec<SerialEndpoint>) -> Inventory {
        Inventory {
            ports,
            receivers: vec![receiver(Some("ABCD"))],
            ..Inventory::default()
        }
    }

    fn interface_port(name: &str, number: Option<u8>, description: &str) -> SerialEndpoint {
        SerialEndpoint {
            usb_interface: Some(UsbInterface {
                number,
                description: InterfaceDescription::Value(description.into()),
            }),
            ..port(name, Some("ABCD"))
        }
    }

    #[test]
    fn opendfu_uses_native_usb_description_without_replacing_raw_metadata() {
        let endpoint = SerialEndpoint {
            port_name: "COM10".into(),
            label: "COM10 — Open DFU Bootloader".into(),
            manufacturer: Some("Microsoft".into()),
            product: Some("USB串行设备 (COM10)".into()),
            vid: Some(0x1915),
            pid: Some(0x521f),
            usb_interface: Some(UsbInterface {
                number: Some(0),
                description: InterfaceDescription::Value("Open DFU Bootloader".into()),
            }),
            ..port("COM10", Some("SYNTHETIC-OPENDFU"))
        };
        assert!(endpoint.is_open_dfu_bootloader());
        assert_eq!(endpoint.product.as_deref(), Some("USB串行设备 (COM10)"));
        assert_eq!(endpoint.manufacturer.as_deref(), Some("Microsoft"));

        let mut changed = endpoint.clone();
        changed.label = "Generic display label".into();
        assert!(
            changed.is_open_dfu_bootloader(),
            "labels are presentation only"
        );
        changed.usb_interface.as_mut().unwrap().description = InterfaceDescription::Unavailable;
        assert!(
            !changed.is_open_dfu_bootloader(),
            "USB IDs alone are not proof"
        );
        changed.product = Some("OPEN DFU BOOTLOADER".into());
        assert!(
            changed.is_open_dfu_bootloader(),
            "raw product is the fallback"
        );
        changed.usb_interface = None;
        assert!(changed.is_open_dfu_bootloader());
    }

    #[test]
    fn opendfu_rejects_labels_manufacturers_missing_names_and_conflicting_interfaces() {
        let mut endpoint = port("COM10", Some("SYNTHETIC-OPENDFU"));
        endpoint.vid = Some(0x1915);
        endpoint.pid = Some(0x521f);
        endpoint.label = "COM10 — Open DFU Bootloader".into();
        endpoint.manufacturer = Some("Open DFU Bootloader".into());
        assert!(!endpoint.is_open_dfu_bootloader());
        endpoint.manufacturer = None;
        assert!(
            !endpoint.is_open_dfu_bootloader(),
            "nameless USB metadata must fail"
        );
        endpoint.product = Some("Open DFU Bootloader".into());
        endpoint.usb_interface = Some(UsbInterface {
            number: Some(0),
            description: InterfaceDescription::Ambiguous,
        });
        assert!(!endpoint.is_open_dfu_bootloader());
        for description in ["SlimeNRF Raw Data CDC", "Other USB interface"] {
            endpoint.usb_interface.as_mut().unwrap().description =
                InterfaceDescription::Value(description.into());
            assert!(!endpoint.is_open_dfu_bootloader(), "{description}");
        }
        endpoint.usb_interface = None;
        for kind in [
            SerialPortKind::Bluetooth,
            SerialPortKind::Pci,
            SerialPortKind::Unknown,
        ] {
            endpoint.kind = kind;
            assert!(!endpoint.is_open_dfu_bootloader(), "{kind:?}");
        }
    }

    #[test]
    fn address_normalization_rejects_malformed_values() {
        for value in [
            "aa:bb:cc:dd:ee:ff",
            "aa-bb-cc-dd-ee-ff",
            "AABBCCDDEEFF",
            "aa bb cc dd ee ff",
        ] {
            assert_eq!(normalize_address(value).as_deref(), Some("AABBCCDDEEFF"));
        }
        for value in [
            "",
            "AA:BB",
            "AABBCCDDEEFF00",
            "AABBCCDDEEFG",
            "AA/BB/CC/DD/EE/FF",
            "0xAABBCCDDEEFF",
            "AABBCCDDEEFF garbage",
        ] {
            assert_eq!(normalize_address(value), None, "{value}");
        }
    }

    #[test]
    fn parses_real_info_banner_labels_and_inline_log() {
        let mut identity = DeviceIdentity::default();
        for line in [
            "[00:11:48.241,790] <inf> sensor: Device address: 112233445566",
            "\x1b[32m* SlimeVR Tracker *\x1b[0m\r",
            "Board: promicro",
            "Target: promicro_uf2/nrf52840/spi[00:11:48.241,790] trailing log",
            "Tracker ID: 7",
            "Device address: aa:bb:cc:dd:ee:ff",
        ] {
            identity.ingest_line(line);
        }
        assert_eq!(identity.name.as_deref(), Some("SlimeVR Tracker"));
        assert_eq!(identity.role, DeviceRole::Tracker);
        assert_eq!(identity.board.as_deref(), Some("promicro"));
        assert_eq!(
            identity.target.as_deref(),
            Some("promicro_uf2/nrf52840/spi")
        );
        assert_eq!(identity.tracker_id, Some(7));
        assert_eq!(identity.address.as_deref(), Some("AABBCCDDEEFF"));
        identity.ingest_line("Tracker ID: 300");
        identity.ingest_line("Device address: invalid");
        assert_eq!(identity.tracker_id, Some(7));
        assert_eq!(identity.address.as_deref(), Some("AABBCCDDEEFF"));
    }

    #[test]
    fn role_requires_explicit_marker_and_receiver_takes_precedence() {
        let mut identity = DeviceIdentity::default();
        identity.ingest_line("Device name: SlimeNRF");
        identity.ingest_line("Tracker ID: 4");
        identity.ingest_line("Board: nrf52840");
        assert_eq!(identity.role, DeviceRole::Unknown);
        identity.ingest_line("Device name: Promicro Receiver");
        identity.ingest_line("Target: promicro_uf2/nrf52840/tracker-compatible");
        assert_eq!(identity.role, DeviceRole::Receiver);
    }

    #[test]
    fn receiver_serial_must_be_nonempty_unique_and_exact() {
        let inventory = inventory(vec![
            port("COM1", Some("ABCD")),
            port("COM2", Some("ABCD-extra")),
        ]);
        assert_eq!(
            receiver_port(&receiver(Some(" abcd ")), &inventory)
                .unwrap()
                .port_name,
            "COM1"
        );
        assert!(receiver_serial_ports(&receiver(None), &inventory).is_none());
        assert!(receiver_serial_ports(&receiver(Some(" ")), &inventory).is_none());
        assert!(receiver_serial_ports(&receiver(Some("abc")), &inventory).is_none());
        assert!(receiver_serial_ports(&receiver(Some("ABCD-extra")), &inventory).is_none());
    }

    #[test]
    fn composite_console_and_raw_pair_in_either_inventory_order() {
        let console = interface_port("COM30", Some(7), "Receiver model");
        let raw = interface_port("COM2", Some(1), " SlimeNRF Raw Data CDC \n");
        for ports in [
            vec![console.clone(), raw.clone()],
            vec![raw.clone(), console.clone()],
        ] {
            let inventory = inventory(ports);
            let associated = receiver_serial_ports(&inventory.receivers[0], &inventory).unwrap();
            assert_eq!(associated.console.unwrap().port_name, "COM30");
            assert_eq!(associated.raw_data.unwrap().port_name, "COM2");
            assert!(console_port_allowed(&console, &inventory));
            assert!(!console_port_allowed(&raw, &inventory));
        }
    }

    #[test]
    fn sole_raw_and_legitimate_absence_never_become_console() {
        let raw = interface_port("COM2", None, "SlimeNRF Raw Data CDC");
        let current = inventory(vec![raw.clone()]);
        let associated = receiver_serial_ports(&current.receivers[0], &current).unwrap();
        assert!(associated.console.is_none());
        assert_eq!(associated.raw_data.unwrap().port_name, "COM2");
        assert!(!console_port_allowed(&raw, &current));
        let empty = inventory(Vec::new());
        let associated = receiver_serial_ports(&empty.receivers[0], &empty).unwrap();
        assert!(associated.console.is_none());
        assert!(associated.raw_data.is_none());
    }

    #[test]
    fn unlabeled_console_remains_usable_without_granting_raw_capability() {
        let mut console = port("/dev/ttyACM9", Some("ABCD"));
        console.label = "SlimeNRF Raw Data CDC".into();
        console.product = Some("SlimeNRF Raw Data CDC".into());
        let inventory = inventory(vec![console.clone()]);
        let associated = receiver_serial_ports(&inventory.receivers[0], &inventory).unwrap();
        assert_eq!(associated.console.unwrap().port_name, "/dev/ttyACM9");
        assert!(associated.raw_data.is_none());
        assert_eq!(console.role(), SerialRole::Unclassified);
        assert!(console_port_allowed(&console, &inventory));
    }

    #[test]
    fn multiple_unknowns_revoke_console_admission_without_hiding_explicit_raw() {
        let console = port("COM1", Some("ABCD"));
        let raw = interface_port("COM2", Some(5), "SlimeNRF Raw Data CDC");
        let mut inventory = inventory(vec![console.clone(), raw]);
        assert!(console_port_allowed(&console, &inventory));
        inventory.ports.push(port("COM3", Some("ABCD")));
        let associated = receiver_serial_ports(&inventory.receivers[0], &inventory).unwrap();
        assert!(associated.console.is_none());
        assert_eq!(associated.raw_data.unwrap().port_name, "COM2");
        assert!(!console_port_allowed(&console, &inventory));
    }

    #[test]
    fn ambiguous_descriptions_duplicate_interfaces_and_duplicate_raw_reject_association() {
        let console = interface_port("COM1", Some(0), "Receiver");
        let raw = interface_port("COM2", Some(2), "SlimeNRF Raw Data CDC");
        let mut ambiguous = console.clone();
        ambiguous.usb_interface.as_mut().unwrap().description = InterfaceDescription::Ambiguous;
        let duplicate_number = interface_port("COM2", Some(0), "SlimeNRF Raw Data CDC");
        let duplicate_raw = interface_port("COM3", Some(4), "SlimeNRF Raw Data CDC");
        let duplicate_port = interface_port("COM1", Some(2), "SlimeNRF Raw Data CDC");
        for ports in [
            vec![ambiguous.clone(), raw.clone()],
            vec![console.clone(), duplicate_number],
            vec![console.clone(), raw.clone(), duplicate_raw],
            vec![console.clone(), duplicate_port],
        ] {
            let inventory = inventory(ports);
            assert!(receiver_serial_ports(&inventory.receivers[0], &inventory).is_none());
            assert!(!console_port_allowed(&console, &inventory));
            assert!(!console_port_allowed(&raw, &inventory));
        }
        assert_eq!(ambiguous.role(), SerialRole::Ambiguous);
    }

    #[test]
    fn duplicate_missing_or_replaced_hid_identity_rejects_association() {
        let console = port("COM1", Some("ABCD"));
        let frozen = receiver(Some("ABCD"));
        let mut inventory = inventory(vec![console.clone()]);
        inventory.receivers.push(ReceiverEndpoint {
            key: "hid:two".into(),
            path: b"two".to_vec(),
            serial_number: Some(" abcd ".into()),
            ..frozen.clone()
        });
        assert!(receiver_serial_ports(&frozen, &inventory).is_none());
        assert!(!console_port_allowed(&console, &inventory));
        inventory.receivers.pop();
        inventory.receivers[0].path = b"replacement".to_vec();
        assert!(receiver_serial_ports(&frozen, &inventory).is_none());
        inventory.receivers[0] = frozen.clone();
        inventory.receivers[0].serial_number = None;
        assert!(receiver_serial_ports(&frozen, &inventory).is_none());
        inventory.receivers.clear();
        assert!(receiver_serial_ports(&frozen, &inventory).is_none());
    }

    #[test]
    fn serial_candidates_require_usb_receiver_vid_pid_and_serial() {
        for change in 0..4 {
            let mut unrelated = port("COM1", Some("ABCD"));
            match change {
                0 => unrelated.kind = SerialPortKind::Unknown,
                1 => unrelated.vid = Some(0x0001),
                2 => unrelated.pid = Some(0x0001),
                _ => unrelated.serial_number = None,
            }
            let inventory = inventory(vec![unrelated]);
            let associated = receiver_serial_ports(&inventory.receivers[0], &inventory).unwrap();
            assert!(associated.console.is_none());
            assert!(associated.raw_data.is_none());
        }
    }

    #[test]
    fn native_raw_description_is_exact_and_receiver_specific() {
        let mut endpoint = interface_port("COM1", None, "SlimeNRF Raw Data CDC");
        assert_eq!(endpoint.role(), SerialRole::ReceiverRawData);
        endpoint.vid = Some(0x0001);
        assert_eq!(endpoint.role(), SerialRole::Unclassified);
        endpoint.vid = Some(0x1209);
        endpoint.kind = SerialPortKind::Bluetooth;
        assert_eq!(endpoint.role(), SerialRole::Unclassified);
        endpoint.kind = SerialPortKind::Usb;
        endpoint.usb_interface.as_mut().unwrap().description =
            InterfaceDescription::Value("slimenrf raw data cdc".into());
        assert_eq!(endpoint.role(), SerialRole::Unclassified);
    }

    #[test]
    fn interface_comparison_ignores_display_but_retains_number_and_evidence() {
        let endpoint = interface_port("COM1", Some(0), "Receiver");
        let mut changed = endpoint.clone();
        changed.label = "Different translated label".into();
        changed.product = Some("Different product".into());
        assert!(endpoint.same_usb_interface(&changed));
        changed.usb_interface.as_mut().unwrap().number = Some(2);
        assert!(!endpoint.same_usb_interface(&changed));
        changed = endpoint.clone();
        changed.usb_interface.as_mut().unwrap().description = InterfaceDescription::Unavailable;
        assert!(!endpoint.same_usb_interface(&changed));
        changed = endpoint.clone();
        changed.usb_interface.as_mut().unwrap().description = InterfaceDescription::Ambiguous;
        assert!(!endpoint.same_usb_interface(&changed));
    }

    #[test]
    fn manual_policy_allows_ordinary_console_without_hid_but_not_raw_or_ambiguous() {
        let console = port("COM1", Some("ABCD"));
        let raw = interface_port("COM2", Some(2), "SlimeNRF Raw Data CDC");
        let mut ambiguous = port("COM3", None);
        ambiguous.usb_interface = Some(UsbInterface {
            number: None,
            description: InterfaceDescription::Ambiguous,
        });
        let inventory = Inventory {
            ports: vec![console.clone(), raw.clone(), ambiguous.clone()],
            ..Inventory::default()
        };
        assert!(console_port_allowed(&console, &inventory));
        assert!(!console_port_allowed(&raw, &inventory));
        assert!(!console_port_allowed(&ambiguous, &inventory));
        let mut changed = console.clone();
        changed.serial_number = Some("OTHER".into());
        assert!(!console_port_allowed(&changed, &inventory));
        changed = console.clone();
        changed.usb_interface = Some(UsbInterface {
            number: Some(0),
            description: InterfaceDescription::Unavailable,
        });
        assert!(!console_port_allowed(&changed, &inventory));
        let mut failed = inventory.clone();
        failed.serial_enumeration_failed = true;
        assert!(!console_port_allowed(&console, &failed));
    }

    #[test]
    fn tracker_addresses_must_match_uniquely_without_slot_or_address_guesses() {
        let tracker = TrackerLink {
            receiver_key: "receiver-one".into(),
            slot: 7,
            address: Some("AABBCCDDEEFF".into()),
            online: false,
        };
        let identity = DeviceIdentity {
            address: Some("aa:bb:cc:dd:ee:ff".into()),
            tracker_id: Some(2),
            ..DeviceIdentity::default()
        };
        assert_eq!(tracker_port(&tracker, [("COM1", &identity)]), Some("COM1"));
        assert_eq!(
            tracker_port(&tracker, [("COM1", &identity), ("COM2", &identity)]),
            None
        );
        let different = DeviceIdentity {
            tracker_id: Some(7),
            address: Some("AABBCCDDEEFE".into()),
            ..DeviceIdentity::default()
        };
        assert_eq!(tracker_port(&tracker, [("COM1", &different)]), None);
        let no_address = TrackerLink {
            address: None,
            ..tracker
        };
        assert_eq!(tracker_port(&no_address, [("COM1", &identity)]), None);
    }
}
