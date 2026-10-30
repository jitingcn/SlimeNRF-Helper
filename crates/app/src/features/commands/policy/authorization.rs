use super::parameters::receiver_wide_payload;
use super::risk::{custom_known_destructive, payload_requires_confirmation};
use super::{CommandContext, Preset};
use crate::domain::DeviceRole;
use crate::i18n::tr;

/// Current evidence must be reconstructed from live sessions and associations,
/// never copied from the pending confirmation. Ports are canonical session names;
/// addresses are the domain model's normalized twelve hexadecimal digits.
#[derive(Clone, Copy, Debug)]
pub enum RouteEvidence<'a> {
    Direct {
        port: &'a str,
        generation: u64,
        connected: bool,
        role: DeviceRole,
    },
    Broadcast {
        port: &'a str,
        generation: u64,
        connected: bool,
        receiver_key: &'a str,
        unique_receiver_route: bool,
    },
    Forwarded {
        port: &'a str,
        generation: u64,
        connected: bool,
        receiver_key: &'a str,
        slot: u8,
        address: Option<&'a str>,
        /// Exactly one connected, role-confirmed receiver serial association.
        unique_receiver_route: bool,
    },
}

impl<'a> RouteEvidence<'a> {
    fn transport(self) -> (&'a str, u64, bool) {
        match self {
            Self::Direct {
                port,
                generation,
                connected,
                ..
            }
            | Self::Broadcast {
                port,
                generation,
                connected,
                ..
            }
            | Self::Forwarded {
                port,
                generation,
                connected,
                ..
            } => (port, generation, connected),
        }
    }

    fn context(self) -> CommandContext {
        match self {
            Self::Direct { role, .. } => CommandContext::Direct(role),
            Self::Forwarded { .. } | Self::Broadcast { .. } => CommandContext::ForwardedTracker,
        }
    }

    pub fn check(self) -> Result<(), String> {
        let (port, _, connected) = self.transport();
        if port.is_empty() || !connected {
            return Err(tr("commands.authorization.connect_device"));
        }
        if let Self::Broadcast {
            receiver_key,
            unique_receiver_route,
            ..
        } = self
            && (!unique_receiver_route || receiver_key.is_empty())
        {
            return Err(tr("commands.authorization.receiver_port_unknown"));
        }
        if let Self::Forwarded {
            receiver_key,
            slot,
            address,
            unique_receiver_route,
            ..
        } = self
        {
            if !unique_receiver_route || receiver_key.is_empty() {
                return Err(tr("commands.authorization.receiver_port_unknown"));
            }
            if slot > 15 {
                return Err(tr("commands.authorization.slot_range"));
            }
            if !address.is_some_and(|address| {
                address.len() == 12 && address.bytes().all(|byte| byte.is_ascii_hexdigit())
            }) {
                return Err(tr("commands.authorization.address_required"));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
enum FrozenRoute {
    Direct(DeviceRole),
    Broadcast {
        receiver_key: String,
    },
    Forwarded {
        receiver_key: String,
        slot: u8,
        address: String,
    },
}

#[derive(Clone, Debug)]
struct FrozenRoutePin {
    port: String,
    generation: u64,
    route: FrozenRoute,
}

impl FrozenRoutePin {
    fn capture_checked(evidence: RouteEvidence<'_>) -> Result<Self, String> {
        let (port, generation, _) = evidence.transport();
        let route = match evidence {
            RouteEvidence::Direct { role, .. } => FrozenRoute::Direct(role),
            RouteEvidence::Broadcast { receiver_key, .. } => FrozenRoute::Broadcast {
                receiver_key: receiver_key.to_owned(),
            },
            RouteEvidence::Forwarded {
                receiver_key,
                slot,
                address,
                ..
            } => FrozenRoute::Forwarded {
                receiver_key: receiver_key.to_owned(),
                slot,
                address: address
                    .ok_or_else(|| tr("commands.authorization.address_unknown"))?
                    .to_owned(),
            },
        };
        Ok(Self {
            port: port.to_owned(),
            generation,
            route,
        })
    }

    fn validate(&self, current: RouteEvidence<'_>, receiver_wide: bool) -> Result<(), String> {
        current.check()?;
        if matches!(current, RouteEvidence::Forwarded { .. }) && receiver_wide {
            return Err(tr("commands.authorization.receiver_wide_required"));
        }
        let (port, generation, _) = current.transport();
        if self.port != port || self.generation != generation {
            return Err(tr("commands.authorization.connection_changed"));
        }
        let same_target = match (&self.route, current) {
            (FrozenRoute::Direct(expected), RouteEvidence::Direct { role, .. }) => {
                *expected == role
            }
            (
                FrozenRoute::Broadcast { receiver_key },
                RouteEvidence::Broadcast {
                    receiver_key: current_receiver,
                    ..
                },
            ) => receiver_key == current_receiver,
            (
                FrozenRoute::Forwarded {
                    receiver_key,
                    slot,
                    address,
                },
                RouteEvidence::Forwarded {
                    receiver_key: current_receiver,
                    slot: current_slot,
                    address: Some(current_address),
                    ..
                },
            ) => {
                receiver_key == current_receiver
                    && *slot == current_slot
                    && address.eq_ignore_ascii_case(current_address)
            }
            _ => false,
        };
        if !same_target {
            return Err(tr("commands.authorization.target_changed"));
        }
        Ok(())
    }
}

/// Immutable command and destination shown in a confirmation. It cannot become
/// a different command, session, receiver, or tracker when selection changes.
#[derive(Clone, Debug)]
pub struct CommandSnapshot {
    command: String,
    dangerous: bool,
    preset: &'static Preset,
    route: FrozenRoutePin,
}

impl CommandSnapshot {
    pub fn capture(
        preset: &'static Preset,
        parameter: &str,
        evidence: RouteEvidence<'_>,
    ) -> Result<Self, String> {
        evidence.check()?;
        if !preset.supports(evidence.context()) {
            return Err(tr("commands.authorization.shortcut_unavailable"));
        }
        let command = match evidence {
            RouteEvidence::Direct { .. } => preset.build(parameter)?,
            RouteEvidence::Broadcast { .. } => format!("send all {}", preset.build(parameter)?),
            RouteEvidence::Forwarded { slot, .. } => {
                preset.build_with_slot(parameter, Some(slot))?
            }
        };
        let dangerous =
            preset.dangerous || payload_requires_confirmation(&command, evidence.context());
        Ok(Self {
            command,
            route: FrozenRoutePin::capture_checked(evidence)?,
            dangerous,
            preset,
        })
    }

    /// Apply shared parameters without recapturing or changing the frozen route.
    pub fn with_parameter(mut self, parameter: &str) -> Result<Self, String> {
        let context = match &self.route.route {
            FrozenRoute::Direct(role) => {
                self.command = self.preset.build(parameter)?;
                CommandContext::Direct(*role)
            }
            FrozenRoute::Forwarded { slot, .. } => {
                self.command = self.preset.build_with_slot(parameter, Some(*slot))?;
                CommandContext::ForwardedTracker
            }
            FrozenRoute::Broadcast { .. } => {
                self.command = format!("send all {}", self.preset.build(parameter)?);
                CommandContext::ForwardedTracker
            }
        };
        self.dangerous =
            self.preset.dangerous || payload_requires_confirmation(&self.command, context);
        Ok(self)
    }

    pub fn command(&self) -> &str {
        &self.command
    }
    pub fn port(&self) -> &str {
        &self.route.port
    }
    pub fn dangerous(&self) -> bool {
        self.dangerous
    }

    pub fn validate(&self, current: RouteEvidence<'_>) -> Result<(), String> {
        self.route.validate(
            current,
            self.preset.receiver_wide_only || receiver_wide_payload(&self.command),
        )
    }

    /// Call immediately before queueing the frozen command on its frozen port.
    /// `confirmed` is only true for explicit acceptance of this snapshot's exact
    /// command and target, never a global approval or an earlier selection.
    pub fn authorize(&self, current: RouteEvidence<'_>, confirmed: bool) -> Result<(), String> {
        self.validate(current)?;
        if (self.dangerous || matches!(self.route.route, FrozenRoute::Broadcast { .. }))
            && !confirmed
        {
            return Err(tr("commands.authorization.confirmation_required"));
        }
        Ok(())
    }
}

const MAX_CUSTOM_PAYLOAD_BYTES: usize = 16_383;
const CUSTOM_BROADCAST_PREFIX: &str = "send all ";

/// Literal custom text and its frozen route, independent of the preset catalog.
#[derive(Clone, Debug)]
pub(crate) struct CustomCommandSnapshot {
    command: String,
    route: FrozenRoutePin,
    known_destructive: bool,
}

impl CustomCommandSnapshot {
    pub(crate) fn capture(line: &str, evidence: RouteEvidence<'_>) -> Result<Self, String> {
        evidence.check()?;
        if line
            .chars()
            .any(|character| character.is_control() || matches!(character, '\u{2028}' | '\u{2029}'))
        {
            return Err(tr("commands.authorization.custom_single_line"));
        }
        if line.trim().is_empty() {
            return Err(tr("commands.authorization.custom_required"));
        }
        let prefix = match evidence {
            RouteEvidence::Direct { .. } => "",
            RouteEvidence::Broadcast { .. } => {
                if line
                    .split_whitespace()
                    .next()
                    .is_some_and(|token| token.eq_ignore_ascii_case("send"))
                {
                    return Err(tr("commands.authorization.custom_nested_send"));
                }
                CUSTOM_BROADCAST_PREFIX
            }
            RouteEvidence::Forwarded { .. } => {
                return Err(tr("commands.authorization.shortcut_unavailable"));
            }
        };
        if line.len() > MAX_CUSTOM_PAYLOAD_BYTES - prefix.len() {
            return Err(rust_i18n::t!(
                "serial.session.error.command_too_long",
                bytes = MAX_CUSTOM_PAYLOAD_BYTES
            )
            .to_string());
        }
        let mut command = String::with_capacity(prefix.len() + line.len());
        command.push_str(prefix);
        command.push_str(line);
        Ok(Self {
            known_destructive: custom_known_destructive(&command),
            command,
            route: FrozenRoutePin::capture_checked(evidence)?,
        })
    }

    pub(crate) fn command(&self) -> &str {
        &self.command
    }

    pub(crate) fn port(&self) -> &str {
        &self.route.port
    }

    /// Call immediately before submission after explicit review of this snapshot.
    pub(crate) fn authorize(
        &self,
        current: RouteEvidence<'_>,
        confirmed: bool,
    ) -> Result<(), String> {
        self.route
            .validate(current, receiver_wide_payload(&self.command))?;
        if !confirmed {
            return Err(tr("commands.authorization.confirmation_required"));
        }
        Ok(())
    }

    pub(crate) fn authorize_terminal_send(&self, current: RouteEvidence<'_>) -> Result<(), String> {
        if !matches!(current, RouteEvidence::Direct { .. }) {
            return Err(tr("commands.authorization.shortcut_unavailable"));
        }
        self.route
            .validate(current, receiver_wide_payload(&self.command))
    }

    pub(crate) fn known_destructive(&self) -> bool {
        self.known_destructive
    }

    pub(crate) fn unknown_effect(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod custom_tests {
    use crate::domain::DeviceRole;
    use crate::features::commands::policy::tests::{broadcast, direct, forwarded};
    use crate::features::commands::policy::{CustomCommandSnapshot, RouteEvidence};

    #[test]
    fn direct_custom_snapshot_preserves_literal_bytes_and_clones_the_review() {
        let route = direct(DeviceRole::Receiver);
        let snapshot = CustomCommandSnapshot::capture("  meow  中文 café  ", route).unwrap();
        let cloned = snapshot.clone();
        assert_eq!(
            cloned.command().as_bytes(),
            "  meow  中文 café  ".as_bytes()
        );
        assert_eq!(cloned.port(), "COM30");
        assert!(!cloned.known_destructive());
        assert!(cloned.unknown_effect());
        assert_eq!(cloned.authorize(route, true), Ok(()));

        let literal = CustomCommandSnapshot::capture("send 3 channel 100", route).unwrap();
        assert_eq!(literal.command(), "send 3 channel 100");
        assert_eq!(literal.authorize(route, true), Ok(()));
    }

    #[test]
    fn broadcast_custom_snapshot_adds_one_prefix_without_rewriting_literal_spacing() {
        let snapshot = CustomCommandSnapshot::capture("  meow 中文  ", broadcast()).unwrap();
        assert_eq!(snapshot.command(), "send all   meow 中文  ");
        assert_eq!(snapshot.port(), "COM30");
        assert!(snapshot.unknown_effect());
        assert_eq!(snapshot.authorize(broadcast(), true), Ok(()));
    }

    #[test]
    fn saved_custom_commands_require_confirmation_even_with_normal_visual_tone() {
        for route in [
            direct(DeviceRole::Tracker),
            direct(DeviceRole::Receiver),
            direct(DeviceRole::Unknown),
            broadcast(),
        ] {
            let snapshot = CustomCommandSnapshot::capture("info", route).unwrap();
            assert!(!snapshot.known_destructive());
            assert!(snapshot.unknown_effect());
            assert!(snapshot.authorize(route, false).is_err());
            assert_eq!(snapshot.authorize(route, true), Ok(()));
        }
    }

    #[test]
    fn terminal_send_authorizes_literal_direct_text_without_saved_review() {
        let direct = direct(DeviceRole::Receiver);
        let snapshot = CustomCommandSnapshot::capture("reset", direct).unwrap();
        assert_eq!(snapshot.command(), "reset");
        assert_eq!(snapshot.authorize_terminal_send(direct), Ok(()));
        assert!(snapshot.authorize(direct, false).is_err());
        assert!(snapshot.authorize_terminal_send(broadcast()).is_err());
        assert!(snapshot.authorize_terminal_send(forwarded()).is_err());
        let broadcast = CustomCommandSnapshot::capture("reset", broadcast()).unwrap();
        assert!(broadcast.authorize_terminal_send(direct).is_err());
    }

    #[test]
    fn custom_direct_authorization_rejects_connection_role_and_route_mutations() {
        let snapshot = CustomCommandSnapshot::capture("info", direct(DeviceRole::Tracker)).unwrap();
        for mutation in 0..5 {
            let mut current = direct(DeviceRole::Tracker);
            if let RouteEvidence::Direct {
                port,
                generation,
                connected,
                role,
            } = &mut current
            {
                match mutation {
                    0 => *generation = 8,
                    1 => *port = "COM31",
                    2 => *connected = false,
                    3 => *role = DeviceRole::Receiver,
                    _ => *role = DeviceRole::Unknown,
                }
            }
            let error = match mutation {
                0 | 1 => "commands.authorization.connection_changed",
                2 => "commands.authorization.connect_device",
                _ => "commands.authorization.target_changed",
            };
            assert!(snapshot.authorize(current, true).is_err(), "{error}");
        }
        for current in [broadcast(), forwarded()] {
            assert!(snapshot.authorize(current, true).is_err());
        }
    }

    #[test]
    fn custom_broadcast_authorization_pins_receiver_and_unique_connected_route() {
        let snapshot = CustomCommandSnapshot::capture("ping", broadcast()).unwrap();
        for mutation in 0..7 {
            let mut current = broadcast();
            if let RouteEvidence::Broadcast {
                port,
                generation,
                connected,
                receiver_key,
                unique_receiver_route,
            } = &mut current
            {
                match mutation {
                    0 => *port = "COM31",
                    1 => *generation = 8,
                    2 => *connected = false,
                    3 => *receiver_key = "receiver-b",
                    4 => *unique_receiver_route = false,
                    5 => *receiver_key = "",
                    _ => *port = "",
                }
            }
            let error = match mutation {
                0 | 1 => "commands.authorization.connection_changed",
                2 | 6 => "commands.authorization.connect_device",
                3 => "commands.authorization.target_changed",
                _ => "commands.authorization.receiver_port_unknown",
            };
            assert!(snapshot.authorize(current, true).is_err(), "{error}");
            if matches!(mutation, 2 | 4 | 5 | 6) {
                assert!(
                    CustomCommandSnapshot::capture("ping", current).is_err(),
                    "{error}"
                );
            }
        }
        for current in [direct(DeviceRole::Receiver), forwarded()] {
            assert!(snapshot.authorize(current, true).is_err());
        }
    }

    #[test]
    fn receiver_wide_custom_commands_never_acquire_selected_slot_authority() {
        for line in ["channel 100", "clearchannel"] {
            let snapshot = CustomCommandSnapshot::capture(line, broadcast()).unwrap();
            assert_eq!(snapshot.command(), format!("send all {line}"));
            assert_eq!(snapshot.authorize(broadcast(), true), Ok(()));
            assert!(snapshot.authorize(forwarded(), true).is_err());
            assert!(CustomCommandSnapshot::capture(line, forwarded()).is_err());
        }
        assert!(CustomCommandSnapshot::capture("ping", forwarded()).is_err());
    }

    #[test]
    fn custom_capture_rejects_empty_and_control_framing_at_its_boundary() {
        for route in [direct(DeviceRole::Receiver), broadcast()] {
            for line in ["", "   ", "\u{00a0}"] {
                assert!(
                    CustomCommandSnapshot::capture(line, route).is_err(),
                    "{line:?}"
                );
            }
            for line in [
                "info\n",
                "\rinfo",
                "info\r\nreboot",
                "info\0",
                "info\tall",
                "info\u{007f}",
                "info\u{0085}",
                "info\u{2028}reboot",
                "info\u{2029}reboot",
            ] {
                assert!(
                    CustomCommandSnapshot::capture(line, route).is_err(),
                    "{line:?}"
                );
            }
        }
    }

    #[test]
    fn custom_capture_bounds_effective_utf8_bytes_including_the_broadcast_prefix() {
        let direct_limit = format!("{}x", "é".repeat(8_191));
        let snapshot =
            CustomCommandSnapshot::capture(&direct_limit, direct(DeviceRole::Tracker)).unwrap();
        assert_eq!(snapshot.command(), direct_limit);
        assert_eq!(snapshot.command().len(), 16_383);

        let broadcast_limit = "é".repeat(8_187);
        let snapshot = CustomCommandSnapshot::capture(&broadcast_limit, broadcast()).unwrap();
        assert_eq!(snapshot.command(), format!("send all {broadcast_limit}"));
        assert_eq!(snapshot.command().len(), 16_383);

        for (line, route) in [
            ("é".repeat(8_192), direct(DeviceRole::Tracker)),
            (format!("{broadcast_limit}x"), broadcast()),
        ] {
            assert!(CustomCommandSnapshot::capture(&line, route).is_err());
        }
    }

    #[test]
    fn broadcast_custom_capture_rejects_only_a_complete_first_send_token() {
        for line in ["send all ping", "SEND 3 ping", "  sEnD 3 ping  ", "send"] {
            assert!(
                CustomCommandSnapshot::capture(line, broadcast()).is_err(),
                "{line:?}"
            );
            let literal =
                CustomCommandSnapshot::capture(line, direct(DeviceRole::Receiver)).unwrap();
            assert_eq!(literal.command(), line);
            assert_eq!(
                literal.authorize(direct(DeviceRole::Receiver), true),
                Ok(())
            );
        }
        for line in ["sender", "sendall ping", "meow send all ping"] {
            let snapshot = CustomCommandSnapshot::capture(line, broadcast()).unwrap();
            assert_eq!(snapshot.command(), format!("send all {line}"));
        }
    }

    #[test]
    fn custom_risk_markers_keep_known_destructive_and_unknown_effect_independent() {
        for (line, known_destructive) in [
            ("clear", true),
            ("  SeNs  ReSeT extra", true),
            ("mag on", true),
            ("reset fusion", true),
            ("reboot", true),
            ("info", false),
            ("meow", false),
            ("clearance", false),
            ("mag onyx", false),
        ] {
            for route in [direct(DeviceRole::Unknown), broadcast()] {
                let snapshot = CustomCommandSnapshot::capture(line, route).unwrap();
                assert_eq!(snapshot.known_destructive(), known_destructive, "{line:?}");
                assert!(snapshot.unknown_effect(), "{line:?}");
            }
        }
        let literal =
            CustomCommandSnapshot::capture("SEND 3 MAG ON", direct(DeviceRole::Receiver)).unwrap();
        assert!(literal.known_destructive());
        assert!(literal.unknown_effect());
    }
}
