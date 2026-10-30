//! Positively associated capture endpoints; discovery never opens a capture session.
use crate::domain::{
    Inventory, ReceiverEndpoint, SerialEndpoint, SerialRole, receiver_serial_ports,
};
use hidapi::HidApi;

const VID: u16 = 0x1209;
const PID: u16 = 0x7690;
#[derive(Clone, Debug)]
pub enum CaptureTransport {
    RawHid {
        path: Vec<u8>,
    },
    Cdc {
        endpoint: SerialEndpoint,
        console: Box<SerialEndpoint>,
    },
}

#[derive(Clone, Debug)]
pub struct CaptureSource {
    pub receiver: ReceiverEndpoint,
    pub transport: CaptureTransport,
}
impl CaptureSource {
    pub fn label(&self) -> String {
        match &self.transport {
            CaptureTransport::RawHid { .. } => format!("{} · Raw HID", self.receiver.label),
            CaptureTransport::Cdc { endpoint, .. } => {
                format!(
                    "{} · CDC binary data · {}",
                    self.receiver.label, endpoint.port_name
                )
            }
        }
    }
    pub fn serial_port(&self) -> Option<&str> {
        match &self.transport {
            CaptureTransport::Cdc { endpoint, .. } => Some(&endpoint.port_name),
            _ => None,
        }
    }
    pub fn console(&self) -> Option<&SerialEndpoint> {
        match &self.transport {
            CaptureTransport::Cdc { console, .. } => Some(console.as_ref()),
            CaptureTransport::RawHid { .. } => None,
        }
    }

    pub fn matches_inventory(&self, inventory: &Inventory) -> bool {
        match &self.transport {
            CaptureTransport::RawHid { .. } => true,
            CaptureTransport::Cdc { endpoint, console } => {
                receiver_serial_ports(&self.receiver, inventory).is_some_and(|ports| {
                    let (Some(raw), Some(current_console)) = (ports.raw_data, ports.console) else {
                        return false;
                    };
                    !crate::serial_port::same_port_name(&raw.port_name, &current_console.port_name)
                        && same_endpoint(endpoint, raw, SerialRole::ReceiverRawData)
                        && same_endpoint(console, current_console, SerialRole::Unclassified)
                })
            }
        }
    }
    pub fn matches(&self, other: &Self) -> bool {
        self.receiver.key == other.receiver.key
            && self.receiver.path == other.receiver.path
            && self.receiver.serial_number == other.receiver.serial_number
            && match (&self.transport, &other.transport) {
                (CaptureTransport::RawHid { path: a }, CaptureTransport::RawHid { path: b }) => {
                    a == b
                }
                (
                    CaptureTransport::Cdc {
                        endpoint: a,
                        console: ac,
                    },
                    CaptureTransport::Cdc {
                        endpoint: b,
                        console: bc,
                    },
                ) => {
                    same_endpoint(a, b, SerialRole::ReceiverRawData)
                        && same_endpoint(ac, bc, SerialRole::Unclassified)
                }
                _ => false,
            }
    }
}

fn same_endpoint(a: &SerialEndpoint, b: &SerialEndpoint, role: SerialRole) -> bool {
    crate::serial_port::same_port_name(&a.port_name, &b.port_name)
        && a.kind == b.kind
        && a.vid == b.vid
        && a.pid == b.pid
        && a.serial_number == b.serial_number
        && a.same_usb_interface(b)
        && a.role() == role
        && b.role() == role
}

/// Read-only enumeration. A unique USB serial is required; labels/order never establish identity.
pub fn discover_sources() -> Result<Vec<CaptureSource>, String> {
    let inventory = crate::discovery::scan()?;
    let api = HidApi::new().map_err(|e| e.to_string())?;
    let mut sources = Vec::new();
    for receiver in &inventory.receivers {
        let Some(serial) = receiver
            .serial_number
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        if inventory
            .receivers
            .iter()
            .filter(|r| r.serial_number.as_deref().map(str::trim) == Some(serial))
            .count()
            != 1
        {
            continue;
        }
        let mut raw: Vec<_> = api
            .device_list()
            .filter(|d| {
                d.vendor_id() == VID
                    && d.product_id() == PID
                    && d.usage_page() == 0xff00
                    && d.serial_number().map(str::trim) == Some(serial)
            })
            .map(|d| d.path().to_bytes().to_vec())
            .collect();
        raw.sort();
        raw.dedup();
        if raw.len() == 1 {
            sources.push(CaptureSource {
                receiver: receiver.clone(),
                transport: CaptureTransport::RawHid {
                    path: raw.remove(0),
                },
            });
        }
        if let Some(source) = cdc_source(receiver, &inventory) {
            sources.push(source);
        }
    }
    Ok(sources)
}

fn cdc_source(receiver: &ReceiverEndpoint, inventory: &Inventory) -> Option<CaptureSource> {
    let ports = receiver_serial_ports(receiver, inventory)?;
    let endpoint = ports.raw_data?;
    let console = ports.console?;
    if crate::serial_port::same_port_name(&endpoint.port_name, &console.port_name) {
        return None;
    }
    Some(CaptureSource {
        receiver: receiver.clone(),
        transport: CaptureTransport::Cdc {
            endpoint: endpoint.clone(),
            console: Box::new(console.clone()),
        },
    })
}

#[cfg(test)]
mod tests;
