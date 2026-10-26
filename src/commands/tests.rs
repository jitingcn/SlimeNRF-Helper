use super::{Preset, RouteEvidence};
use crate::domain::DeviceRole;

pub(super) fn get(id: &str) -> &'static Preset {
    super::catalog::PRESETS
        .iter()
        .find(|preset| preset.id == id)
        .expect("catalog entry")
}
pub(super) fn direct(role: DeviceRole) -> RouteEvidence<'static> {
    RouteEvidence::Direct {
        port: "COM30",
        generation: 7,
        connected: true,
        role,
    }
}

pub(super) fn forwarded() -> RouteEvidence<'static> {
    RouteEvidence::Forwarded {
        port: "COM30",
        generation: 7,
        connected: true,
        receiver_key: "receiver-a",
        slot: 3,
        address: Some("AABBCCDDEEFF"),
        unique_receiver_route: true,
    }
}

pub(super) fn broadcast() -> RouteEvidence<'static> {
    RouteEvidence::Broadcast {
        port: "COM30",
        generation: 7,
        connected: true,
        receiver_key: "receiver-a",
        unique_receiver_route: true,
    }
}

mod authorization;
mod catalog;
mod parameters;
mod presentation;
