#[cfg(test)]
mod tests;

use super::{
    Workbench,
    command_panel::{CommandConnection, same_serial_endpoint},
};
use crate::{
    commands::{CustomCommandSnapshot, RouteEvidence},
    domain::{DeviceRole, ReceiverEndpoint, SerialEndpoint, receiver_port},
    i18n::tr,
    operations::{
        CommandOrigin, CommandRun, CommandStepResult, CommandTargetPlan, Excluded, RunId,
    },
    sessions::{ConnectionState, WriteReceipt},
    shortcuts::{CommandLine, SavedCommandCard, SavedCommandId, TargetMode, parse_editor_lines},
};
use gpui_kit::Context;
use std::collections::{BTreeSet, VecDeque};

const TARGET_LIMIT: usize = 64;
const RUN_LIMIT: usize = 16;

#[derive(Default)]
pub(super) struct CustomCommandState {
    pending: Option<PendingCustomCommand>,
    runs: VecDeque<CommandRun>,
    next_run_id: u64,
    error: Option<String>,
    pub(super) terminal_clear: Option<(String, String)>,
    pub(super) pending_history: Vec<(String, String)>,
}

enum PendingCustomCommand {
    Preparing(CommandPreparation),
    Review(FrozenCommandReview),
}

struct Invocation {
    source_id: SavedCommandId,
    title: String,
    mode: TargetMode,
    lines: Vec<CommandLine>,
}

struct PreparationTarget {
    label: String,
    endpoint: SerialEndpoint,
    receiver: Option<ReceiverEndpoint>,
    connection: CommandConnection,
    failure: Option<String>,
}

struct CommandPreparation {
    invocation: Invocation,
    targets: Vec<PreparationTarget>,
    excluded: Vec<Excluded>,
}

pub(in crate::ui) struct CommandTargetChoice {
    pub(in crate::ui) key: String,
    pub(in crate::ui) label: String,
    pub(in crate::ui) port: Option<String>,
    pub(in crate::ui) connected: bool,
    pub(in crate::ui) exclusion: Option<String>,
}

pub(in crate::ui) struct ReviewTargetSummary {
    pub(in crate::ui) label: String,
    pub(in crate::ui) port: String,
    pub(in crate::ui) lines: Vec<String>,
}

pub(in crate::ui) struct FrozenCommandReview {
    invocation: Invocation,
    targets: Vec<CommandTargetPlan>,
    summaries: Vec<ReviewTargetSummary>,
    excluded: Vec<Excluded>,
}

struct TerminalBroadcast {
    source_port: String,
    text: String,
    targets: Vec<CommandTargetPlan>,
    excluded: Vec<Excluded>,
}

impl FrozenCommandReview {
    pub(in crate::ui) fn source_id(&self) -> SavedCommandId {
        self.invocation.source_id
    }
    pub(in crate::ui) fn title(&self) -> &str {
        &self.invocation.title
    }
    pub(in crate::ui) fn target_mode(&self) -> TargetMode {
        self.invocation.mode
    }
    pub(in crate::ui) fn targets(&self) -> &[ReviewTargetSummary] {
        &self.summaries
    }
    pub(in crate::ui) fn excluded(&self) -> &[Excluded] {
        &self.excluded
    }
    pub(in crate::ui) fn has_known_destructive_steps(&self) -> bool {
        self.targets
            .iter()
            .flat_map(|target| &target.steps)
            .any(CustomCommandSnapshot::known_destructive)
    }
    pub(in crate::ui) fn has_unknown_steps(&self) -> bool {
        self.targets
            .iter()
            .flat_map(|target| &target.steps)
            .any(CustomCommandSnapshot::unknown_effect)
    }
}

impl CustomCommandState {
    fn submit_terminal_broadcast(
        &mut self,
        broadcast: TerminalBroadcast,
        submit: impl FnMut(&CommandTargetPlan, &CustomCommandSnapshot) -> Result<WriteReceipt, String>,
    ) -> Result<RunId, String> {
        let (id, accepted) = self.submit_run(
            CommandOrigin::Terminal {
                source_port: broadcast.source_port.clone(),
            },
            tr("serial.terminal.action.send_all"),
            broadcast.targets,
            broadcast.excluded,
            submit,
        )?;
        if accepted {
            self.terminal_clear = Some((broadcast.source_port, broadcast.text));
        }
        Ok(id)
    }

    fn submit_run(
        &mut self,
        origin: CommandOrigin,
        title: String,
        targets: Vec<CommandTargetPlan>,
        excluded: Vec<Excluded>,
        submit: impl FnMut(&CommandTargetPlan, &CustomCommandSnapshot) -> Result<WriteReceipt, String>,
    ) -> Result<(RunId, bool), String> {
        if targets.is_empty() && excluded.is_empty() {
            return Err(tr("shortcuts.error.no_targets"));
        }
        if self.runs.len() == RUN_LIMIT {
            if let Some(index) = self
                .runs
                .iter()
                .rposition(|run| !run.pending() && run.summary().failed == 0)
            {
                self.runs.remove(index);
            } else {
                return Err(tr("shortcuts.error.run_busy"));
            }
        }
        let next = self
            .next_run_id
            .checked_add(1)
            .ok_or_else(|| tr("shortcuts.error.id_exhausted"))?;
        let id = RunId::new(next).ok_or_else(|| tr("shortcuts.error.id_exhausted"))?;
        self.next_run_id = next;
        let mut run = CommandRun::new(id, origin, title, targets).with_excluded(excluded);
        let (_, accepted) = advance_custom_run(&mut run, &mut self.pending_history, submit);
        self.runs.push_front(run);
        Ok((id, accepted))
    }

    fn poll_runs(
        &mut self,
        mut submit: impl FnMut(
            bool,
            &CommandTargetPlan,
            &CustomCommandSnapshot,
        ) -> Result<WriteReceipt, String>,
    ) -> bool {
        let mut changed = false;
        for run in &mut self.runs {
            let saved_reviewed = matches!(run.origin(), CommandOrigin::SavedCommand);
            changed |= advance_custom_run(run, &mut self.pending_history, |target, step| {
                submit(saved_reviewed, target, step)
            })
            .0;
        }
        changed
    }

    fn saved_runs(&self) -> impl Iterator<Item = &CommandRun> {
        self.runs
            .iter()
            .filter(|run| matches!(run.origin(), CommandOrigin::SavedCommand))
    }

    fn terminal_runs(&self) -> impl Iterator<Item = &CommandRun> {
        self.runs
            .iter()
            .filter(|run| matches!(run.origin(), CommandOrigin::Terminal { .. }))
    }

    fn dismiss_run(&mut self, id: RunId) -> bool {
        let Some(index) = self
            .runs
            .iter()
            .position(|run| run.id() == id && !run.pending())
        else {
            return false;
        };
        self.runs.remove(index);
        true
    }
}

impl Workbench {
    fn custom_endpoint_check(
        &self,
        endpoint: &SerialEndpoint,
        receiver: Option<&ReceiverEndpoint>,
    ) -> Result<(), String> {
        validate_target_identity(endpoint, receiver, &self.inventory)?;
        self.console_check_port(&endpoint.port_name)?;
        self.flash_check_port(&endpoint.port_name)?;
        if self.manager.command_port_reserved(&endpoint.port_name) {
            return Err(tr("firmware.error.port_updating"));
        }
        if let Some(receiver) = receiver {
            self.flash_check_receiver(&receiver.key)?;
        }
        Ok(())
    }

    fn custom_evidence<'a>(
        &'a self,
        endpoint: &'a SerialEndpoint,
        receiver: Option<&'a ReceiverEndpoint>,
        mode: TargetMode,
        generation: u64,
    ) -> Result<RouteEvidence<'a>, String> {
        self.custom_endpoint_check(endpoint, receiver)?;
        let session = self
            .manager
            .session(&endpoint.port_name)
            .ok_or_else(|| tr("serial.batch.error.disconnected"))?;
        if session.generation != generation {
            return Err(tr("serial.batch.error.connection_changed"));
        }
        if session.state != ConnectionState::Connected {
            return Err(tr("serial.batch.error.disconnected"));
        }
        let role = self.serial_role(&session.port);
        if mode != TargetMode::ConnectedTerminals && role != DeviceRole::Receiver {
            return Err(tr("commands.route.error.not_receiver"));
        }
        if mode == TargetMode::ReceiverTrackers {
            let receiver = receiver.ok_or_else(|| tr("commands.route.error.receiver_missing"))?;
            Ok(RouteEvidence::Broadcast {
                port: &session.port,
                generation,
                connected: true,
                receiver_key: &receiver.key,
                unique_receiver_route: true,
            })
        } else {
            Ok(RouteEvidence::Direct {
                port: &session.port,
                generation,
                connected: true,
                role,
            })
        }
    }

    pub(in crate::ui) fn saved_command_targets(
        &self,
        mode: TargetMode,
    ) -> Vec<CommandTargetChoice> {
        if mode == TargetMode::ConnectedTerminals {
            let mut choices =
                connected_command_targets(&self.manager, &self.inventory, |endpoint, receiver| {
                    self.custom_endpoint_check(endpoint, receiver)
                });
            for choice in &mut choices {
                if let Some(port) = &choice.port {
                    choice.label =
                        self.serial_session_label(port, std::mem::take(&mut choice.label));
                }
            }
            choices
        } else {
            self.inventory
                .receivers
                .iter()
                .map(|receiver| {
                    let endpoint = receiver_port(receiver, &self.inventory);
                    let exclusion = endpoint
                        .ok_or_else(|| tr("commands.route.error.receiver_port_unknown"))
                        .and_then(|endpoint| self.custom_endpoint_check(endpoint, Some(receiver)))
                        .err();
                    CommandTargetChoice {
                        key: receiver.key.clone(),
                        label: self.receiver_label(receiver, receiver.label.clone()),
                        port: endpoint.map(|endpoint| endpoint.port_name.clone()),
                        connected: endpoint
                            .and_then(|endpoint| self.manager.session(&endpoint.port_name))
                            .is_some_and(|session| session.state == ConnectionState::Connected),
                        exclusion,
                    }
                })
                .collect()
        }
    }

    pub(in crate::ui) fn prepare_saved_command(
        &mut self,
        card: SavedCommandCard,
        receiver_keys: BTreeSet<String>,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let invocation = Invocation {
            source_id: card.id,
            title: card.title,
            mode: card.target_mode,
            lines: card.lines,
        };
        let result = self.prepare_custom_invocation(invocation, receiver_keys);
        self.custom_commands.error = result.as_ref().err().cloned();
        cx.notify();
        result
    }

    pub(super) fn submit_terminal_send_all(
        &mut self,
        port: &str,
        text: String,
        cx: &mut Context<Self>,
    ) -> Result<RunId, String> {
        if self.shutting_down {
            return Err(tr("firmware.status.closing"));
        }
        self.poll_sessions();
        self.monitor.poll();
        let broadcast = capture_terminal_broadcast(
            port,
            text,
            &self.manager,
            &self.inventory,
            |endpoint, receiver| self.custom_endpoint_check(endpoint, receiver),
            |port| self.serial_role(port),
            |port, label| self.serial_session_label(port, label),
        )?;
        let mut commands = std::mem::take(&mut self.custom_commands);
        let result = commands.submit_terminal_broadcast(broadcast, |target, step| {
            self.submit_custom_step(target, step, false)
        });
        self.custom_commands = commands;
        cx.notify();
        result
    }

    fn prepare_custom_invocation(
        &mut self,
        invocation: Invocation,
        receiver_keys: BTreeSet<String>,
    ) -> Result<(), String> {
        self.custom_commands.pending = None;
        if self.shutting_down {
            return Err(tr("firmware.status.closing"));
        }
        self.poll_sessions();
        self.monitor.poll();
        if invocation.mode != TargetMode::ConnectedTerminals && receiver_keys.len() > TARGET_LIMIT {
            return Err(
                rust_i18n::t!("shortcuts.error.too_many_targets", max = TARGET_LIMIT).to_string(),
            );
        }
        let choices = self.saved_command_targets(invocation.mode);
        let mut preparation = CommandPreparation {
            invocation,
            targets: Vec::new(),
            excluded: Vec::new(),
        };
        for choice in choices {
            if preparation.invocation.mode != TargetMode::ConnectedTerminals
                && !receiver_keys.contains(&choice.key)
            {
                continue;
            }
            if let Some(reason) = choice.exclusion {
                preparation.excluded.push(Excluded {
                    label: choice.label,
                    reason,
                });
                continue;
            }
            let Some(port) = choice.port else {
                continue;
            };
            let Some(endpoint) = self.port_endpoint(&port).cloned() else {
                continue;
            };
            let receiver = if preparation.invocation.mode == TargetMode::ConnectedTerminals {
                self.inventory
                    .receivers
                    .iter()
                    .find(|receiver| {
                        receiver_port(receiver, &self.inventory).is_some_and(|candidate| {
                            same_serial_endpoint(Some(&endpoint), Some(candidate))
                        })
                    })
                    .cloned()
            } else {
                self.inventory
                    .receivers
                    .iter()
                    .find(|receiver| receiver.key == choice.key)
                    .cloned()
            };
            if preparation
                .targets
                .iter()
                .any(|target| target.endpoint.port_name == endpoint.port_name)
            {
                continue;
            }
            let generation = self
                .manager
                .session(&port)
                .map(|session| session.generation);
            preparation.targets.push(PreparationTarget {
                label: choice.label,
                endpoint,
                receiver,
                connection: CommandConnection { port, generation },
                failure: None,
            });
        }
        if preparation.invocation.mode != TargetMode::ConnectedTerminals {
            for key in receiver_keys {
                if !self
                    .inventory
                    .receivers
                    .iter()
                    .any(|receiver| receiver.key == key)
                {
                    preparation.excluded.push(Excluded {
                        label: key,
                        reason: tr("commands.route.error.receiver_missing"),
                    });
                }
            }
        }
        if preparation.targets.len() > TARGET_LIMIT {
            return Err(
                rust_i18n::t!("shortcuts.error.too_many_targets", max = TARGET_LIMIT).to_string(),
            );
        }
        if preparation.targets.is_empty() && preparation.excluded.is_empty() {
            return Err(tr("shortcuts.error.no_targets"));
        }
        if preparation.invocation.mode != TargetMode::ConnectedTerminals {
            for target in &mut preparation.targets {
                target.failure = self
                    .custom_endpoint_check(&target.endpoint, target.receiver.as_ref())
                    .and_then(|()| target.connection.start(&mut self.manager))
                    .err();
            }
        }
        self.custom_commands.pending = Some(PendingCustomCommand::Preparing(preparation));
        self.resume_custom_preparation();
        Ok(())
    }

    fn resume_custom_preparation(&mut self) -> bool {
        let Some(pending) = self.custom_commands.pending.take() else {
            return false;
        };
        let PendingCustomCommand::Preparing(mut preparation) = pending else {
            self.custom_commands.pending = Some(pending);
            return false;
        };
        let mut waiting = false;
        for target in &mut preparation.targets {
            if target.failure.is_some() {
                continue;
            }
            let readiness = self
                .custom_endpoint_check(&target.endpoint, target.receiver.as_ref())
                .and_then(|()| target.connection.ready(&self.manager));
            match readiness {
                Ok(false) => waiting = true,
                Ok(true) => {}
                Err(error) => target.failure = Some(error),
            }
        }
        if waiting {
            self.custom_commands.pending = Some(PendingCustomCommand::Preparing(preparation));
            return false;
        }
        let mut plans = Vec::new();
        let mut summaries = Vec::new();
        for target in preparation.targets {
            let result = if let Some(error) = target.failure {
                Err(error)
            } else {
                let generation = target.connection.generation.unwrap_or(0);
                self.custom_evidence(
                    &target.endpoint,
                    target.receiver.as_ref(),
                    preparation.invocation.mode,
                    generation,
                )
                .and_then(|evidence| capture_custom_steps(&preparation.invocation.lines, evidence))
                .map(|steps| (generation, steps))
            };
            match result {
                Ok((generation, steps)) => {
                    summaries.push(ReviewTargetSummary {
                        label: target.label.clone(),
                        port: target.endpoint.port_name.clone(),
                        lines: steps.iter().map(|step| step.command().to_owned()).collect(),
                    });
                    plans.push(CommandTargetPlan {
                        label: target.label,
                        endpoint: target.endpoint,
                        receiver: target.receiver,
                        mode: preparation.invocation.mode,
                        generation,
                        steps,
                    });
                }
                Err(reason) => preparation.excluded.push(Excluded {
                    label: target.label,
                    reason,
                }),
            }
        }
        self.custom_commands.pending = Some(PendingCustomCommand::Review(FrozenCommandReview {
            invocation: preparation.invocation,
            targets: plans,
            summaries,
            excluded: preparation.excluded,
        }));
        true
    }

    pub(in crate::ui) fn custom_command_review(&self) -> Option<&FrozenCommandReview> {
        match &self.custom_commands.pending {
            Some(PendingCustomCommand::Review(review)) => Some(review),
            _ => None,
        }
    }
    pub(in crate::ui) fn custom_command_preparing(&self) -> bool {
        matches!(
            self.custom_commands.pending,
            Some(PendingCustomCommand::Preparing(_))
        )
    }
    pub(in crate::ui) fn custom_command_error(&self) -> Option<&str> {
        self.custom_commands.error.as_deref()
    }
    pub(in crate::ui) fn saved_command_runs(&self) -> impl Iterator<Item = &CommandRun> {
        self.custom_commands.saved_runs()
    }
    pub(super) fn terminal_command_runs(&self) -> impl Iterator<Item = &CommandRun> {
        self.custom_commands.terminal_runs()
    }
    pub(in crate::ui) fn dismiss_custom_run(&mut self, id: RunId) -> bool {
        self.custom_commands.dismiss_run(id)
    }
    pub(in crate::ui) fn cancel_custom_review(&mut self) {
        self.custom_commands.pending = None;
        self.custom_commands.error = None;
    }
    pub(in crate::ui) fn invalidate_saved_command_review(&mut self, id: SavedCommandId) {
        let source_id = match &self.custom_commands.pending {
            Some(PendingCustomCommand::Preparing(preparation)) => {
                Some(preparation.invocation.source_id)
            }
            Some(PendingCustomCommand::Review(review)) => Some(review.source_id()),
            None => None,
        };
        if source_id == Some(id) {
            self.cancel_custom_review();
        }
    }

    pub(in crate::ui) fn confirm_custom_command(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Result<RunId, String> {
        let result = self.submit_custom_review();
        self.custom_commands.error = result.as_ref().err().cloned();
        cx.notify();
        result
    }

    fn submit_custom_review(&mut self) -> Result<RunId, String> {
        let Some(pending) = self.custom_commands.pending.take() else {
            return Err(tr("shortcuts.error.review_changed"));
        };
        let PendingCustomCommand::Review(review) = pending else {
            self.custom_commands.pending = Some(pending);
            return Err(tr("shortcuts.error.review_changed"));
        };
        if self.shutting_down {
            return Err(tr("firmware.status.closing"));
        }
        if review.targets.is_empty() {
            return Err(tr("shortcuts.error.no_targets"));
        }
        let mut commands = std::mem::take(&mut self.custom_commands);
        let result = commands.submit_run(
            CommandOrigin::SavedCommand,
            review.invocation.title,
            review.targets,
            review.excluded,
            |target, step| self.submit_custom_step(target, step, true),
        );
        self.custom_commands = commands;
        result.map(|(id, _)| id)
    }

    fn submit_custom_step(
        &mut self,
        target: &CommandTargetPlan,
        step: &CustomCommandSnapshot,
        saved_reviewed: bool,
    ) -> Result<WriteReceipt, String> {
        self.poll_sessions();
        let evidence = self.custom_evidence(
            &target.endpoint,
            target.receiver.as_ref(),
            target.mode,
            target.generation,
        )?;
        if saved_reviewed {
            step.authorize(evidence, true)?;
        } else {
            step.authorize_terminal_send(evidence)?;
        }
        self.flash_send_line(step.port(), step.command())
    }

    pub(in crate::ui) fn cancel_custom_run(&mut self, id: RunId) {
        if let Some(run) = self
            .custom_commands
            .runs
            .iter_mut()
            .find(|run| run.id() == id)
        {
            run.cancel();
        }
    }

    pub(super) fn poll_custom_commands(&mut self) -> bool {
        let mut changed = self.resume_custom_preparation();
        let mut commands = std::mem::take(&mut self.custom_commands);
        changed |= commands.poll_runs(|saved_reviewed, target, step| {
            self.submit_custom_step(target, step, saved_reviewed)
        });
        self.custom_commands = commands;
        changed
    }

    pub(super) fn stop_custom_commands(&mut self) {
        self.cancel_custom_review();
        for run in &mut self.custom_commands.runs {
            run.cancel();
        }
    }
}

fn capture_custom_steps(
    lines: &[CommandLine],
    evidence: RouteEvidence<'_>,
) -> Result<Vec<CustomCommandSnapshot>, String> {
    lines
        .iter()
        .map(|line| CustomCommandSnapshot::capture(line.as_str(), evidence))
        .collect()
}

fn capture_terminal_broadcast(
    source_port: &str,
    text: String,
    manager: &crate::sessions::SessionManager,
    inventory: &crate::domain::Inventory,
    admission: impl FnMut(&SerialEndpoint, Option<&ReceiverEndpoint>) -> Result<(), String>,
    mut role: impl FnMut(&str) -> DeviceRole,
    mut label: impl FnMut(&str, &str) -> String,
) -> Result<TerminalBroadcast, String> {
    let lines = parse_editor_lines(&text, TargetMode::ConnectedTerminals)
        .map_err(|error| error.to_string())?;
    let mut targets = Vec::new();
    let mut excluded = Vec::new();
    for mut choice in connected_command_targets(manager, inventory, admission)
        .into_iter()
        .filter(|choice| choice.connected)
    {
        if let Some(port) = &choice.port {
            choice.label = label(port, &choice.label);
        }
        if let Some(reason) = choice.exclusion {
            excluded.push(Excluded {
                label: choice.label,
                reason,
            });
            continue;
        }
        let Some(port) = choice.port else {
            continue;
        };
        let Some(endpoint) = inventory
            .ports
            .iter()
            .find(|endpoint| endpoint.port_name == port)
        else {
            continue;
        };
        let Some(session) = manager.session(&port) else {
            continue;
        };
        let receiver = inventory.receivers.iter().find(|receiver| {
            receiver_port(receiver, inventory)
                .is_some_and(|candidate| same_serial_endpoint(Some(endpoint), Some(candidate)))
        });
        let evidence = RouteEvidence::Direct {
            port: &session.port,
            generation: session.generation,
            connected: session.state == ConnectionState::Connected,
            role: role(&session.port),
        };
        match capture_custom_steps(&lines, evidence) {
            Ok(steps) => targets.push(CommandTargetPlan {
                label: choice.label,
                endpoint: endpoint.clone(),
                receiver: receiver.cloned(),
                mode: TargetMode::ConnectedTerminals,
                generation: session.generation,
                steps,
            }),
            Err(reason) => excluded.push(Excluded {
                label: choice.label,
                reason,
            }),
        }
    }
    if targets.len() > TARGET_LIMIT {
        return Err(
            rust_i18n::t!("shortcuts.error.too_many_targets", max = TARGET_LIMIT).to_string(),
        );
    }
    if targets.is_empty() && excluded.is_empty() {
        return Err(tr("shortcuts.error.no_targets"));
    }
    Ok(TerminalBroadcast {
        source_port: source_port.to_owned(),
        text,
        targets,
        excluded,
    })
}

fn advance_custom_run(
    run: &mut CommandRun,
    history: &mut Vec<(String, String)>,
    mut submit: impl FnMut(&CommandTargetPlan, &CustomCommandSnapshot) -> Result<WriteReceipt, String>,
) -> (bool, bool) {
    let mut accepted = false;
    let changed = run.advance(|target, step| {
        let result = submit(target, step);
        if result.is_ok() {
            accepted = true;
            history.push((step.port().to_owned(), step.command().to_owned()));
        }
        result
    });
    (changed, accepted)
}

fn same_receiver_identity(expected: &ReceiverEndpoint, current: &ReceiverEndpoint) -> bool {
    expected.key == current.key
        && expected.path == current.path
        && expected.serial_number == current.serial_number
}

fn validate_target_identity(
    endpoint: &SerialEndpoint,
    receiver: Option<&ReceiverEndpoint>,
    inventory: &crate::domain::Inventory,
) -> Result<(), String> {
    if !crate::domain::console_port_allowed(endpoint, inventory) {
        return Err(tr("serial.console.error.not_allowed"));
    }
    if let Some(receiver) = receiver {
        let current = inventory
            .receivers
            .iter()
            .find(|current| current.key == receiver.key)
            .ok_or_else(|| tr("commands.route.error.receiver_missing"))?;
        if !same_receiver_identity(receiver, current)
            || !receiver_port(current, inventory)
                .is_some_and(|port| same_serial_endpoint(Some(endpoint), Some(port)))
        {
            return Err(tr("commands.preparation.error.route_changed"));
        }
    }
    Ok(())
}

fn connected_command_targets(
    manager: &crate::sessions::SessionManager,
    inventory: &crate::domain::Inventory,
    mut admission: impl FnMut(&SerialEndpoint, Option<&ReceiverEndpoint>) -> Result<(), String>,
) -> Vec<CommandTargetChoice> {
    manager
        .sessions()
        .map(|session| {
            let endpoint = inventory
                .ports
                .iter()
                .find(|endpoint| endpoint.port_name == session.port);
            let receiver = endpoint.and_then(|endpoint| {
                inventory.receivers.iter().find(|receiver| {
                    receiver_port(receiver, inventory)
                        .is_some_and(|port| same_serial_endpoint(Some(endpoint), Some(port)))
                })
            });
            let exclusion = if session.state != ConnectionState::Connected {
                Some(tr("serial.batch.error.not_connected"))
            } else {
                endpoint
                    .ok_or_else(|| tr("commands.preparation.error.target_unavailable"))
                    .and_then(|endpoint| {
                        validate_target_identity(endpoint, receiver, inventory)?;
                        if manager.command_port_reserved(&session.port) {
                            return Err(tr("firmware.error.port_updating"));
                        }
                        admission(endpoint, receiver)
                    })
                    .err()
            };
            CommandTargetChoice {
                key: session.port.clone(),
                label: endpoint
                    .map_or_else(|| session.port.clone(), |endpoint| endpoint.label.clone()),
                port: Some(session.port.clone()),
                connected: session.state == ConnectionState::Connected,
                exclusion,
            }
        })
        .collect()
}

pub(super) fn target_summary(steps: &[CommandStepResult]) -> String {
    let written = steps
        .iter()
        .filter(|step| matches!(step, CommandStepResult::Written))
        .count();
    for (index, step) in steps.iter().enumerate() {
        if let CommandStepResult::Rejected(reason) | CommandStepResult::WriteFailed(reason) = step {
            return format!(
                "{} · {reason}",
                rust_i18n::t!("shortcuts.result.failed_step", step = index + 1)
            );
        }
    }
    if written == steps.len() {
        return tr("shortcuts.result.written");
    }
    if let Some(reason) = steps.iter().find_map(|step| match step {
        CommandStepResult::Skipped(reason) => Some(reason),
        _ => None,
    }) {
        return format!(
            "{} · {written}/{} · {reason}",
            tr("shortcuts.result.skipped"),
            steps.len()
        );
    }
    rust_i18n::t!(
        "shortcuts.result.progress",
        written = written,
        total = steps.len()
    )
    .to_string()
}

pub(super) fn custom_step_summary(step: &CommandStepResult) -> String {
    match step {
        CommandStepResult::NotSubmitted => tr("shortcuts.result.waiting"),
        CommandStepResult::Queued => tr("shortcuts.result.queued"),
        CommandStepResult::Written => tr("shortcuts.result.written"),
        CommandStepResult::Rejected(error) => {
            format!("{} · {error}", tr("shortcuts.result.rejected"))
        }
        CommandStepResult::WriteFailed(error) => {
            format!("{} · {error}", tr("shortcuts.result.partial_write"))
        }
        CommandStepResult::Skipped(reason) => {
            format!("{} · {reason}", tr("shortcuts.result.skipped"))
        }
    }
}
