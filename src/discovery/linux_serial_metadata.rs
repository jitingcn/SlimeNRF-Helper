use std::fs::File;
use std::io::Read;
use std::path::Path;

use crate::domain::{InterfaceDescription, UsbInterface};

const MAX_ANCESTORS: usize = 8;
const MAX_ATTRIBUTE_BYTES: usize = 1024;

pub(super) fn usb_interface(port: &str, usb: &serialport::UsbPortInfo) -> UsbInterface {
    read_interface(Path::new("/sys/class/tty"), port, usb)
}

fn read_interface(tty_root: &Path, port: &str, usb: &serialport::UsbPortInfo) -> UsbInterface {
    let number = usb.interface;
    let unavailable = UsbInterface {
        number,
        description: InterfaceDescription::Unavailable,
    };
    let Some(tty) = port
        .strip_prefix("/dev/")
        .filter(|tty| !tty.is_empty() && !tty.contains('/') && *tty != "." && *tty != "..")
    else {
        return unavailable;
    };
    let Ok(device) = tty_root.join(tty).join("device").canonicalize() else {
        return unavailable;
    };
    for ancestor in device.ancestors().take(MAX_ANCESTORS) {
        let native_number = match attribute(&ancestor.join("bInterfaceNumber"), interface_number) {
            Attribute::Missing => {
                if ancestor.join("idVendor").exists() {
                    break;
                }
                continue;
            }
            Attribute::Value(Some(number)) => number,
            Attribute::Value(None) | Attribute::Invalid => return ambiguous(number),
        };
        if number.is_some_and(|number| number != native_number) {
            return ambiguous(number);
        }
        let number = Some(native_number);
        if usb.vid != super::RECEIVER_VID || usb.pid != super::RECEIVER_PID {
            return UsbInterface {
                number,
                ..unavailable
            };
        }
        match attribute(&ancestor.join("bInterfaceClass"), |value| {
            value.trim() == "02"
        }) {
            Attribute::Missing => {
                return UsbInterface {
                    number,
                    ..unavailable
                };
            }
            Attribute::Value(true) => {}
            Attribute::Value(false) | Attribute::Invalid => return ambiguous(number),
        }
        let description = match attribute(&ancestor.join("interface"), |value| {
            if value.trim().is_empty() {
                InterfaceDescription::Unavailable
            } else {
                InterfaceDescription::Value(value.trim().into())
            }
        }) {
            Attribute::Missing => InterfaceDescription::Unavailable,
            Attribute::Invalid => InterfaceDescription::Ambiguous,
            Attribute::Value(description) => description,
        };
        return UsbInterface {
            number,
            description,
        };
    }
    unavailable
}

fn ambiguous(number: Option<u8>) -> UsbInterface {
    UsbInterface {
        number,
        description: InterfaceDescription::Ambiguous,
    }
}

fn interface_number(value: &str) -> Option<u8> {
    let value = value.trim();
    if value.len() != 2 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    u8::from_str_radix(value, 16).ok()
}

enum Attribute<T> {
    Missing,
    Invalid,
    Value(T),
}

fn attribute<T>(path: &Path, parse: impl FnOnce(&str) -> T) -> Attribute<T> {
    let Ok(mut file) = File::open(path) else {
        return Attribute::Missing;
    };
    let mut bytes = [0; MAX_ATTRIBUTE_BYTES + 1];
    let mut length = 0;
    while length < bytes.len() {
        match file.read(&mut bytes[length..]) {
            Ok(0) => break,
            Ok(count) => length += count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return Attribute::Invalid,
        }
    }
    if length > MAX_ATTRIBUTE_BYTES {
        return Attribute::Invalid;
    }
    match std::str::from_utf8(&bytes[..length]) {
        Ok(value) if !value.contains('\0') => Attribute::Value(parse(value)),
        _ => Attribute::Invalid,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::path::PathBuf;

    use super::{MAX_ANCESTORS, read_interface};
    use crate::domain::{
        InterfaceDescription, Inventory, SerialEndpoint, SerialPortKind, SerialRole, UsbInterface,
        console_port_allowed,
    };

    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let tty = root.path().join("class/tty/ttyACM7");
        let interface = root.path().join("devices/usb/device/usb-function");
        let child = interface.join("tty/ttyACM7");
        fs::create_dir_all(&tty).unwrap();
        fs::create_dir_all(&child).unwrap();
        symlink(&child, tty.join("device")).unwrap();
        fs::write(interface.join("bInterfaceNumber"), "07\n").unwrap();
        fs::write(interface.join("bInterfaceClass"), "02\n").unwrap();
        let tty_root = root.path().join("class/tty");
        (root, tty_root, interface)
    }

    fn receiver(number: Option<u8>) -> serialport::UsbPortInfo {
        serialport::UsbPortInfo {
            vid: 0x1209,
            pid: 0x7690,
            serial_number: Some("receiver-7".into()),
            manufacturer: None,
            product: None,
            interface: number,
        }
    }

    fn endpoint(usb: &serialport::UsbPortInfo, interface: UsbInterface) -> SerialEndpoint {
        SerialEndpoint {
            port_name: "/dev/ttyACM7".into(),
            label: "SlimeNRF Raw Data CDC".into(),
            manufacturer: None,
            product: Some("SlimeNRF Raw Data CDC".into()),
            kind: SerialPortKind::Usb,
            serial_number: usb.serial_number.clone(),
            vid: Some(usb.vid),
            pid: Some(usb.pid),
            usb_interface: Some(interface),
        }
    }

    #[test]
    fn reads_communication_interface_without_product_or_port_order_inference() {
        let (_root, tty, interface) = fixture();
        fs::write(interface.join("interface"), "SlimeNRF Raw Data CDC\n").unwrap();
        fs::write(
            interface.parent().unwrap().join("product"),
            "Receiver model\n",
        )
        .unwrap();
        assert_eq!(
            read_interface(&tty, "/dev/ttyACM7", &receiver(Some(7))),
            UsbInterface {
                number: Some(7),
                description: InterfaceDescription::Value("SlimeNRF Raw Data CDC".into()),
            }
        );
        assert_eq!(
            read_interface(&tty, "/dev/ttyACM7", &receiver(None)).number,
            Some(7)
        );
        fs::remove_file(interface.join("bInterfaceClass")).unwrap();
        assert_eq!(
            read_interface(&tty, "/dev/ttyACM7", &receiver(Some(7))),
            UsbInterface {
                number: Some(7),
                description: InterfaceDescription::Unavailable,
            }
        );
    }

    #[test]
    fn vendor_class_uarts_keep_manual_admission_without_raw_capability() {
        let (_root, tty, interface) = fixture();
        fs::write(interface.join("bInterfaceClass"), "ff\n").unwrap();
        fs::write(interface.join("interface"), "SlimeNRF Raw Data CDC\n").unwrap();
        fs::write(
            interface.parent().unwrap().join("product"),
            "SlimeNRF Raw Data CDC\n",
        )
        .unwrap();
        for (vid, pid) in [(0x0403, 0x6001), (0x10c4, 0xea60), (0x1a86, 0x7523)] {
            let usb = serialport::UsbPortInfo {
                vid,
                pid,
                ..receiver(Some(7))
            };
            let metadata = read_interface(&tty, "/dev/ttyACM7", &usb);
            assert_eq!(
                metadata,
                UsbInterface {
                    number: Some(7),
                    description: InterfaceDescription::Unavailable,
                }
            );
            let port = endpoint(&usb, metadata);
            assert_eq!(port.role(), SerialRole::Unclassified);
            let inventory = Inventory {
                ports: vec![port.clone()],
                ..Inventory::default()
            };
            assert!(console_port_allowed(&port, &inventory));

            let conflicting_usb = serialport::UsbPortInfo {
                interface: Some(2),
                ..usb
            };
            let port = endpoint(
                &conflicting_usb,
                read_interface(&tty, "/dev/ttyACM7", &conflicting_usb),
            );
            assert_eq!(port.role(), SerialRole::Ambiguous);
            let inventory = Inventory {
                ports: vec![port.clone()],
                ..Inventory::default()
            };
            assert!(!console_port_allowed(&port, &inventory));
        }
    }

    #[test]
    fn receiver_wrong_class_rejects_admission_and_cdc_keeps_raw_role() {
        let (_root, tty, interface) = fixture();
        let usb = receiver(Some(7));
        fs::write(interface.join("interface"), "SlimeNRF Raw Data CDC\n").unwrap();
        for class in ["ff\n", "0a\n", "invalid\n"] {
            fs::write(interface.join("bInterfaceClass"), class).unwrap();
            let port = endpoint(&usb, read_interface(&tty, "/dev/ttyACM7", &usb));
            assert_eq!(port.role(), SerialRole::Ambiguous);
            let inventory = Inventory {
                ports: vec![port.clone()],
                ..Inventory::default()
            };
            assert!(!console_port_allowed(&port, &inventory));
        }
        fs::write(interface.join("bInterfaceClass"), "02\n").unwrap();
        let port = endpoint(&usb, read_interface(&tty, "/dev/ttyACM7", &usb));
        assert_eq!(port.role(), SerialRole::ReceiverRawData);
        let inventory = Inventory {
            ports: vec![port.clone()],
            ..Inventory::default()
        };
        assert!(!console_port_allowed(&port, &inventory));
    }

    #[test]
    fn missing_description_never_inherits_usb_product_or_parent_interface() {
        let (_root, tty, interface) = fixture();
        let parent = interface.parent().unwrap();
        fs::write(parent.join("product"), "SlimeNRF Raw Data CDC\n").unwrap();
        fs::write(parent.join("interface"), "SlimeNRF Raw Data CDC\n").unwrap();
        assert_eq!(
            read_interface(&tty, "/dev/ttyACM7", &receiver(Some(7))).description,
            InterfaceDescription::Unavailable
        );
        fs::remove_file(interface.join("bInterfaceNumber")).unwrap();
        fs::write(parent.join("idVendor"), "1209\n").unwrap();
        assert_eq!(
            read_interface(&tty, "/dev/ttyACM7", &receiver(Some(7))).description,
            InterfaceDescription::Unavailable
        );
    }

    #[test]
    fn contradictory_or_malformed_native_interface_facts_are_ambiguous() {
        let (_root, tty, interface) = fixture();
        fs::write(interface.join("interface"), "SlimeNRF Raw Data CDC\n").unwrap();
        assert_eq!(
            read_interface(&tty, "/dev/ttyACM7", &receiver(Some(2))).description,
            InterfaceDescription::Ambiguous
        );
        for value in ["", "7", "gg", "100"] {
            fs::write(interface.join("bInterfaceNumber"), value).unwrap();
            assert_eq!(
                read_interface(&tty, "/dev/ttyACM7", &receiver(Some(7))).description,
                InterfaceDescription::Ambiguous
            );
        }
        fs::write(interface.join("bInterfaceNumber"), "07\n").unwrap();
        fs::write(interface.join("bInterfaceClass"), "0a\n").unwrap();
        assert_eq!(
            read_interface(&tty, "/dev/ttyACM7", &receiver(Some(7))).description,
            InterfaceDescription::Ambiguous
        );
    }

    #[test]
    fn bounds_native_attribute_reads_and_rejects_invalid_strings() {
        let (_root, tty, interface) = fixture();
        for value in [
            vec![b'x'; 1025],
            vec![0xff],
            b"SlimeNRF Raw Data CDC\0".to_vec(),
        ] {
            fs::write(interface.join("interface"), value).unwrap();
            assert_eq!(
                read_interface(&tty, "/dev/ttyACM7", &receiver(Some(7))).description,
                InterfaceDescription::Ambiguous
            );
        }
    }

    #[test]
    fn missing_devices_invalid_tty_names_and_excessive_ancestry_stay_unavailable() {
        let (root, tty, interface) = fixture();
        for port in ["/dev/ttyACM8", "/dev/../ttyACM7", "ttyACM7", "/dev/."] {
            assert_eq!(
                read_interface(&tty, port, &receiver(Some(7))).description,
                InterfaceDescription::Unavailable
            );
        }
        let mut deep = interface.clone();
        for _ in 0..MAX_ANCESTORS {
            deep.push("child");
        }
        fs::create_dir_all(&deep).unwrap();
        let device = root.path().join("class/tty/ttyACM7/device");
        fs::remove_file(&device).unwrap();
        symlink(&deep, &device).unwrap();
        assert_eq!(
            read_interface(&tty, "/dev/ttyACM7", &receiver(Some(7))).description,
            InterfaceDescription::Unavailable
        );
    }
}
