use super::*;
use crate::{
    domain::{DeviceRole, Inventory, SerialPortKind, console_port_allowed, receiver_port},
    serial_port::same_port_name,
    sessions::{ConnectionState, SessionManager, SessionView, WriteReceipt},
    ui::command_panel::{CommandConnection, same_serial_endpoint},
};

#[cfg(test)]
mod tests;
mod view;

#[derive(Default)]
pub(super) struct SerialDfuState {
    selected_port: Option<String>,
    show_other: bool,
    pub(super) warning: Option<SerialDfuIntent>,
    pending: Option<SerialDfuPending>,
    outcome: Option<SerialDfuOutcome>,
}

pub(super) struct SerialDfuIntent {
    port: String,
    endpoint: Option<SerialEndpoint>,
    generation: Option<u64>,
    owned: bool,
    inventory_generation: Option<u64>,
    confirmed_unknown: bool,
    evidence: Option<SerialDfuEvidence>,
}

enum SerialDfuEvidence {
    Reported(DeviceRole),
    Receiver { key: String, path: Vec<u8> },
}

struct SerialDfuPending {
    intent: SerialDfuIntent,
    phase: SerialDfuPhase,
}

enum SerialDfuPhase {
    Inventory(Option<u64>),
    Connecting(CommandConnection),
    Submitting,
    Writing(WriteReceipt),
}

enum SerialDfuOutcome {
    Sent,
    Failed(String),
}

struct SerialDfuCandidate<'a> {
    port: &'a str,
    endpoint: Option<&'a SerialEndpoint>,
    session: Option<&'a SessionView>,
}

impl SerialDfuCandidate<'_> {
    fn label(&self) -> &str {
        self.endpoint
            .map_or(self.port, |endpoint| endpoint.label.as_str())
    }
}

fn live_owner(session: &SessionView) -> bool {
    matches!(
        session.state,
        ConnectionState::Connected | ConnectionState::Connecting
    )
}

fn reported_slime(session: &SessionView) -> bool {
    session.state == ConnectionState::Connected
        && session.identity.role != DeviceRole::Unknown
        && [
            session.identity.name.as_deref(),
            session.identity.target.as_deref(),
            session.identity.board.as_deref(),
        ]
        .into_iter()
        .flatten()
        .any(|text| {
            text.split(|c: char| !c.is_ascii_alphanumeric())
                .any(|token| {
                    token.eq_ignore_ascii_case("SlimeVR") || token.eq_ignore_ascii_case("SlimeNRF")
                })
        })
}

fn recognition(
    port: &str,
    manager: &SessionManager,
    inventory: &Inventory,
    current: bool,
) -> Option<SerialDfuEvidence> {
    if !current
        || !inventory
            .ports
            .iter()
            .find(|endpoint| same_port_name(&endpoint.port_name, port))
            .is_some_and(|endpoint| console_port_allowed(endpoint, inventory))
    {
        return None;
    }
    if let Some(session) = manager
        .session(port)
        .filter(|session| reported_slime(session))
    {
        return Some(SerialDfuEvidence::Reported(session.identity.role));
    }
    inventory
        .receivers
        .iter()
        .find(|receiver| {
            receiver_port(receiver, inventory)
                .is_some_and(|endpoint| same_port_name(&endpoint.port_name, port))
        })
        .map(|receiver| SerialDfuEvidence::Receiver {
            key: receiver.key.clone(),
            path: receiver.path.clone(),
        })
}

fn evidence_valid(
    intent: &SerialDfuIntent,
    manager: &SessionManager,
    inventory: &Inventory,
) -> bool {
    if !inventory
        .ports
        .iter()
        .find(|endpoint| same_port_name(&endpoint.port_name, &intent.port))
        .is_some_and(|endpoint| console_port_allowed(endpoint, inventory))
    {
        return false;
    }
    match &intent.evidence {
        Some(SerialDfuEvidence::Reported(role)) => manager
            .session(&intent.port)
            .is_some_and(|session| session.identity.role == *role && reported_slime(session)),
        Some(SerialDfuEvidence::Receiver { key, path }) => {
            inventory.receivers.iter().any(|receiver| {
                receiver.key == *key
                    && receiver.path == *path
                    && receiver_port(receiver, inventory)
                        .is_some_and(|endpoint| same_port_name(&endpoint.port_name, &intent.port))
            })
        }
        None => false,
    }
}

#[cfg(test)]
fn recognized_device(
    port: &str,
    manager: &SessionManager,
    inventory: &Inventory,
    current: bool,
) -> bool {
    recognition(port, manager, inventory, current).is_some()
}

fn changed_target() -> String {
    tr("firmware.serial_dfu.error.target_changed")
}

fn inventory_unavailable() -> String {
    tr("firmware.serial_dfu.error.inventory_unavailable")
}

impl SerialDfuState {
    pub(super) fn busy(&self) -> bool {
        self.pending.is_some()
    }

    fn status(&self) -> Option<&'static str> {
        self.pending.as_ref().map(|pending| match &pending.phase {
            SerialDfuPhase::Inventory(_) => "firmware.serial_dfu.phase.refreshing",
            SerialDfuPhase::Connecting(_) => "firmware.serial_dfu.phase.connecting",
            SerialDfuPhase::Submitting | SerialDfuPhase::Writing(_) => {
                "firmware.serial_dfu.phase.writing"
            }
        })
    }

    fn fail(&mut self, error: String) {
        self.warning = None;
        self.pending = None;
        self.outcome = Some(SerialDfuOutcome::Failed(error));
    }

    fn select(&mut self, port: String) {
        if self.busy() {
            return;
        }
        if self.selected_port.as_deref() != Some(port.as_str()) {
            self.selected_port = Some(port);
            self.warning = None;
            self.outcome = None;
        }
    }

    fn request(
        &mut self,
        manager: &SessionManager,
        inventory: &Inventory,
        current: bool,
        gate: Result<(), String>,
    ) {
        if self.busy() {
            return;
        }
        self.warning = None;
        self.outcome = None;
        if let Err(error) = gate {
            self.fail(error);
            return;
        }
        let Some(port) = self.selected_port.as_ref() else {
            self.fail(changed_target());
            return;
        };
        let endpoint = inventory
            .ports
            .iter()
            .find(|endpoint| same_port_name(&endpoint.port_name, port));
        let session = manager.session(port);
        let owned = session.is_some_and(live_owner);
        if !current || !endpoint.is_some_and(|endpoint| console_port_allowed(endpoint, inventory)) {
            self.fail(changed_target());
            return;
        }
        let evidence = recognition(port, manager, inventory, current);
        let intent = SerialDfuIntent {
            port: port.clone(),
            endpoint: endpoint.cloned(),
            generation: session.map(|session| session.generation),
            owned,
            inventory_generation: None,
            confirmed_unknown: false,
            evidence,
        };
        if intent.evidence.is_some() {
            self.accept(intent);
        } else {
            self.warning = Some(intent);
        }
    }

    fn valid_intent(
        &self,
        intent: &SerialDfuIntent,
        manager: &SessionManager,
        inventory: &Inventory,
    ) -> bool {
        if self.selected_port.as_deref() != Some(intent.port.as_str())
            || manager
                .session(&intent.port)
                .map(|session| session.generation)
                != intent.generation
        {
            return false;
        }
        let current = inventory
            .ports
            .iter()
            .find(|endpoint| same_port_name(&endpoint.port_name, &intent.port));
        if !current.is_some_and(|endpoint| console_port_allowed(endpoint, inventory)) {
            return false;
        }
        same_serial_endpoint(intent.endpoint.as_ref(), current)
    }

    fn confirm(
        &mut self,
        manager: &SessionManager,
        inventory: &Inventory,
        gate: Result<(), String>,
    ) {
        let Some(mut intent) = self.warning.take() else {
            return;
        };
        if let Err(error) = gate {
            self.fail(error);
            return;
        }
        if !self.valid_intent(&intent, manager, inventory) {
            self.fail(changed_target());
            return;
        }
        intent.confirmed_unknown = true;
        self.accept(intent);
    }

    fn accept(&mut self, intent: SerialDfuIntent) {
        let phase = if intent.owned {
            SerialDfuPhase::Connecting(CommandConnection {
                port: intent.port.clone(),
                generation: intent.generation,
            })
        } else {
            SerialDfuPhase::Inventory(None)
        };
        self.pending = Some(SerialDfuPending { intent, phase });
    }

    // Return a one-shot submission only after the real owner is ready. The caller
    // must use flash_send_line, then retain that receipt via submitted().
    fn advance(
        &mut self,
        manager: &mut SessionManager,
        inventory: &Inventory,
        inventory_generation: u64,
        scanning: bool,
        current: bool,
        gate: Result<(), String>,
    ) -> Option<String> {
        let pending = self.pending.as_mut()?;
        if let SerialDfuPhase::Writing(receipt) = &mut pending.phase {
            if let Some(result) = receipt.poll() {
                self.pending = None;
                self.outcome = Some(match result {
                    Ok(()) => SerialDfuOutcome::Sent,
                    Err(error) => SerialDfuOutcome::Failed(error),
                });
            }
            return None;
        }
        // A receipt already accepted by the transport is independent of later
        // inventory/session errors, including the expected post-write EOF.
        if let Err(error) = gate {
            self.fail(error);
            return None;
        }
        let pending = self.pending.as_ref()?;
        if !self.valid_intent(&pending.intent, manager, inventory) {
            self.fail(changed_target());
            return None;
        }
        let waiting_inventory = matches!(pending.phase, SerialDfuPhase::Inventory(_)) && scanning;
        if !current {
            if scanning {
                return None;
            }
            self.fail(inventory_unavailable());
            return None;
        }
        if let Some(expected) = pending.intent.inventory_generation
            && (!current || expected != inventory_generation)
        {
            self.fail(inventory_unavailable());
            return None;
        }
        if !waiting_inventory
            && !pending.intent.confirmed_unknown
            && !evidence_valid(&pending.intent, manager, inventory)
        {
            self.fail(changed_target());
            return None;
        }
        let pending = self.pending.as_mut()?;
        let result: Result<(), String> = match &mut pending.phase {
            SerialDfuPhase::Inventory(Some(expected)) => {
                if *expected != inventory_generation {
                    Err(changed_target())
                } else if scanning {
                    return None;
                } else if !current {
                    Err(inventory_unavailable())
                } else {
                    let mut connection = CommandConnection {
                        port: pending.intent.port.clone(),
                        generation: pending.intent.generation,
                    };
                    match connection.start(manager) {
                        Ok(()) => {
                            pending.intent.generation = connection.generation;
                            pending.intent.inventory_generation = Some(inventory_generation);
                            pending.phase = SerialDfuPhase::Connecting(connection);
                            return None;
                        }
                        Err(error) => Err(error),
                    }
                }
            }
            SerialDfuPhase::Connecting(connection) => match connection.ready(manager) {
                Ok(false) => return None,
                Ok(true) => {
                    let port = pending.intent.port.clone();
                    pending.phase = SerialDfuPhase::Submitting;
                    return Some(port);
                }
                Err(error) => Err(error),
            },
            SerialDfuPhase::Inventory(None) | SerialDfuPhase::Submitting => return None,
            SerialDfuPhase::Writing(_) => unreachable!(),
        };
        if let Err(error) = result {
            self.fail(error);
        }
        None
    }

    fn needs_scan(&self) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|pending| matches!(pending.phase, SerialDfuPhase::Inventory(None)))
    }

    fn scanned(&mut self, generation: u64) {
        if let Some(pending) = self.pending.as_mut()
            && matches!(pending.phase, SerialDfuPhase::Inventory(None))
        {
            pending.phase = SerialDfuPhase::Inventory(Some(generation));
        }
    }

    fn submitted(&mut self, result: Result<WriteReceipt, String>) {
        match result {
            Ok(receipt) => {
                if let Some(pending) = self.pending.as_mut()
                    && matches!(pending.phase, SerialDfuPhase::Submitting)
                {
                    pending.phase = SerialDfuPhase::Writing(receipt);
                }
            }
            Err(error) => self.fail(error),
        }
    }
}

impl FlashState {
    pub(in crate::ui) fn serial_dfu_owns_preparation(&self, port: &str) -> bool {
        self.serial_dfu.pending.as_ref().is_some_and(|pending| {
            same_port_name(&pending.intent.port, port)
                && !matches!(pending.phase, SerialDfuPhase::Writing(_))
        })
    }

    fn serial_dfu_blocked(&self) -> bool {
        self.job.is_some()
            || self.pending.is_some()
            || self.chooser.is_some()
            || self.review.is_some()
            || self.auto
            || self.auto_requested
    }
}

impl Workbench {
    fn serial_inventory_current(&self) -> bool {
        !self.scanning_inventory
            && self.inventory_error.is_none()
            && !self.inventory.serial_enumeration_failed
    }

    fn flash_serial_dfu_candidates(&self) -> impl Iterator<Item = SerialDfuCandidate<'_>> {
        self.inventory
            .ports
            .iter()
            .filter(move |endpoint| console_port_allowed(endpoint, &self.inventory))
            .filter(move |endpoint| {
                self.flash.serial_dfu.show_other
                    || endpoint.kind == SerialPortKind::Usb
                    || self
                        .manager
                        .session(&endpoint.port_name)
                        .is_some_and(live_owner)
                    || [
                        Some(endpoint.label.as_str()),
                        endpoint.product.as_deref(),
                        endpoint.manufacturer.as_deref(),
                    ]
                    .into_iter()
                    .flatten()
                    .any(|text| {
                        text.as_bytes()
                            .windows(5)
                            .any(|word| word.eq_ignore_ascii_case(b"slime"))
                    })
            })
            .map(move |endpoint| SerialDfuCandidate {
                port: &endpoint.port_name,
                endpoint: Some(endpoint),
                session: self
                    .manager
                    .session(&endpoint.port_name)
                    .filter(|session| live_owner(session)),
            })
    }

    fn flash_serial_dfu_blocked(&self) -> bool {
        self.shutting_down || self.flash.serial_dfu_blocked()
    }

    fn serial_dfu_gate(&self, port: &str) -> Result<(), String> {
        if self.flash_serial_dfu_blocked() {
            return Err(tr("firmware.serial_dfu.error.firmware_busy"));
        }
        self.console_check_port(port)?;
        self.flash_check_port(port)
    }

    fn flash_select_serial_dfu(&mut self, port: String) {
        self.flash.serial_dfu.select(port);
    }

    fn flash_show_other_serial_dfu(&mut self, show: bool) {
        if !self.flash.serial_dfu.busy() {
            self.flash.serial_dfu.show_other = show;
            self.flash.serial_dfu.warning = None;
        }
    }

    fn flash_cancel_serial_dfu(&mut self) {
        self.flash.serial_dfu.warning = None;
    }

    fn flash_start_serial_dfu(&mut self, cx: &mut Context<Self>) {
        self.poll_sessions();
        let gate = self
            .flash
            .serial_dfu
            .selected_port
            .as_deref()
            .map_or_else(|| Err(changed_target()), |port| self.serial_dfu_gate(port));
        let current = self.serial_inventory_current();
        self.flash
            .serial_dfu
            .request(&self.manager, &self.inventory, current, gate);
        self.poll_serial_dfu(cx);
    }

    fn flash_confirm_serial_dfu(&mut self, cx: &mut Context<Self>) {
        self.poll_sessions();
        let gate = self.flash.serial_dfu.warning.as_ref().map_or_else(
            || Err(changed_target()),
            |intent| self.serial_dfu_gate(&intent.port),
        );
        self.flash
            .serial_dfu
            .confirm(&self.manager, &self.inventory, gate);
        self.poll_serial_dfu(cx);
    }

    pub(in crate::ui) fn poll_serial_dfu(&mut self, cx: &mut Context<Self>) -> bool {
        let was_busy = self.flash.serial_dfu.busy();
        if !was_busy {
            return false;
        }
        self.poll_sessions();
        let previous_status = self.flash.serial_dfu.status();
        if self.flash.serial_dfu.needs_scan() && !self.scanning_inventory && !self.shutting_down {
            self.scan_inventory(cx);
            self.flash.serial_dfu.scanned(self.inventory_generation);
        }
        let gate = self
            .flash
            .serial_dfu
            .pending
            .as_ref()
            .map_or(Ok(()), |pending| self.serial_dfu_gate(&pending.intent.port));
        let current = self.serial_inventory_current();
        if let Some(port) = self.flash.serial_dfu.advance(
            &mut self.manager,
            &self.inventory,
            self.inventory_generation,
            self.scanning_inventory,
            current,
            gate,
        ) {
            let result = self.flash_send_line(&port, "dfu");
            self.flash.serial_dfu.submitted(result);
        }
        // No flash activity, terminal focus/draft, selection, or volume authorization
        // is touched. Polling is root-owned, so navigation cannot discard a write.
        previous_status != self.flash.serial_dfu.status()
    }
}
