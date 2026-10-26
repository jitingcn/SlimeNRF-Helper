use super::{
    CommandConnection, CommandFocus, CommandPin, CommandPreparation, CommandScope, PendingCommand,
    PrepareAction, PrepareTarget, PrepareTargetSource, PreparedRouteKind, PresetDraft, RemoteBatch,
    RemoteFocus, SerialGuard, SerialReview, route_parts, same_serial_endpoint,
    tracker_route_with_traffic, validate_command_pin, validate_tracker_focus,
};
use crate::ui::{
    Workbench,
    devices::{live_tracker_port, receiver_command_port},
};
use crate::{
    commands::{self, CommandContext, CommandSnapshot, ParameterSpec, RouteEvidence},
    domain::{DeviceRole, SerialEndpoint, console_port_allowed, receiver_port},
    i18n::tr,
    operations::{BatchRun, BatchSnapshot, Candidate, Excluded, TargetOutcome},
    sessions::ConnectionState,
};
use gpui_kit::component::input::InputState;
use gpui_kit::{AppContext, Context, Window};
use std::collections::BTreeSet;

impl Workbench {
    pub(in crate::ui) fn console_check_port(&self, port: &str) -> Result<(), String> {
        if self.inventory.serial_enumeration_failed
            || self.inventory_error.is_some()
            || !self
                .port_endpoint(port)
                .is_some_and(|endpoint| console_port_allowed(endpoint, &self.inventory))
        {
            return Err(tr("serial.console.error.not_allowed"));
        }
        Ok(())
    }

    pub(in crate::ui) fn target_description(&self, focus: &CommandFocus) -> String {
        match focus {
            CommandFocus::Serial(port) => self.serial_device_label(
                port,
                rust_i18n::t!("commands.target.serial", port = port).to_string(),
            ),
            CommandFocus::Receiver(key) => self
                .inventory
                .receivers
                .iter()
                .find(|receiver| &receiver.key == key)
                .map_or_else(
                    || tr("commands.target.receiver_unavailable"),
                    |receiver| self.receiver_label(receiver, receiver.label.clone()),
                ),
            CommandFocus::Tracker {
                receiver,
                slot,
                address,
            } => {
                let label = self
                    .inventory
                    .receivers
                    .iter()
                    .find(|item| &item.key == receiver)
                    .map_or_else(
                        || tr("commands.target.receiver_unavailable"),
                        |item| self.receiver_label(item, item.label.clone()),
                    );
                self.address_label(
                    address.as_deref(),
                    rust_i18n::t!(
                        "commands.target.tracker",
                        slot = slot,
                        address = address
                            .as_deref()
                            .map(std::borrow::Cow::Borrowed)
                            .unwrap_or_else(|| std::borrow::Cow::Owned(tr(
                                "commands.target.address_unknown"
                            ))),
                        label = label
                    )
                    .to_string(),
                )
            }
        }
    }

    pub(in crate::ui) fn focus_serial(&mut self, port: &str) {
        self.cancel_terminal_intents();
        self.command_focus = Some(CommandFocus::Serial(port.to_owned()));
        self.sidebar.focus_changed();
        self.command_error = None;
        if self.manager.session(port).is_some() {
            self.tabs.select(port);
        }
    }

    pub(in crate::ui) fn focus_command_target(&mut self, focus: CommandFocus) {
        self.cancel_terminal_intents();
        if let Ok(route) = self.route_evidence(&focus) {
            let (port, _, _) = route_parts(&route);
            let port = port.to_owned();
            self.tabs.select(&port);
        }
        self.command_focus = Some(focus);
        self.sidebar.focus_changed();
        self.command_error = None;
    }

    pub(in crate::ui) fn serial_role(&self, port: &str) -> DeviceRole {
        let role = self
            .manager
            .session(port)
            .filter(|session| session.state == ConnectionState::Connected)
            .map_or(DeviceRole::Unknown, |session| session.identity.role);
        if role == DeviceRole::Unknown
            && self.inventory.receivers.iter().any(|receiver| {
                receiver_port(receiver, &self.inventory)
                    .is_some_and(|endpoint| endpoint.port_name == port)
            })
        {
            DeviceRole::Receiver
        } else {
            role
        }
    }

    fn route_evidence<'a>(&'a self, focus: &'a CommandFocus) -> Result<RouteEvidence<'a>, String> {
        self.route_evidence_for(focus, true, true)
    }

    pub(super) fn route_evidence_for<'a>(
        &'a self,
        focus: &'a CommandFocus,
        prefer_direct: bool,
        require_connected: bool,
    ) -> Result<RouteEvidence<'a>, String> {
        match focus {
            CommandFocus::Serial(port) => {
                self.console_check_port(port)?;
                self.flash_check_port(port)?;
                let session = self.manager.session(port);
                let connected =
                    session.is_some_and(|session| session.state == ConnectionState::Connected);
                if require_connected && !connected {
                    return Err(tr("commands.authorization.connect_device"));
                }
                if session.is_none()
                    && !self
                        .inventory
                        .ports
                        .iter()
                        .any(|endpoint| &endpoint.port_name == port)
                {
                    return Err(tr("commands.route.error.serial_missing"));
                }
                Ok(RouteEvidence::Direct {
                    port,
                    generation: session.map_or(0, |session| session.generation),
                    connected,
                    role: self.serial_role(port),
                })
            }
            CommandFocus::Receiver(key) => {
                self.flash_check_receiver(key)?;
                let port = receiver_command_port(key, &self.inventory)?;
                self.console_check_port(&port.port_name)?;
                self.flash_check_port(&port.port_name)?;
                let session = self.manager.session(&port.port_name);
                let connected =
                    session.is_some_and(|session| session.state == ConnectionState::Connected);
                if require_connected && !connected {
                    return Err(tr("commands.route.error.connect_receiver"));
                }
                if self.serial_role(&port.port_name) != DeviceRole::Receiver {
                    return Err(tr("commands.route.error.not_receiver"));
                }
                Ok(RouteEvidence::Direct {
                    port: &port.port_name,
                    generation: session.map_or(0, |session| session.generation),
                    connected,
                    role: DeviceRole::Receiver,
                })
            }
            CommandFocus::Tracker {
                receiver,
                slot,
                address,
            } => {
                self.capture_check_receiver(receiver)?;
                self.flash_only_check_receiver(receiver)?;
                let receiver_endpoint = self
                    .inventory
                    .receivers
                    .iter()
                    .find(|item| &item.key == receiver)
                    .ok_or_else(|| tr("commands.route.error.receiver_missing"))?;
                let observation = self
                    .monitor
                    .observation(receiver)
                    .ok_or_else(|| tr("commands.route.error.waiting_for_receiver"))?;
                if observation.error.is_some() {
                    return Err(tr("commands.route.error.discovery_unavailable"));
                }
                let tracker = observation
                    .trackers
                    .iter()
                    .find(|tracker| tracker.slot == *slot)
                    .ok_or_else(|| tr("commands.route.error.tracker_not_observed"))?;
                validate_tracker_focus(focus, tracker)?;
                if let Some(port) = prefer_direct
                    .then(|| {
                        live_tracker_port(
                            tracker,
                            self.manager.sessions().map(|session| {
                                (session.port.as_str(), session.state, &session.identity)
                            }),
                        )
                    })
                    .flatten()
                {
                    self.console_check_port(port)?;
                    self.flash_check_port(port)?;
                    let session = self
                        .manager
                        .session(port)
                        .ok_or_else(|| tr("commands.route.error.tracker_disconnected"))?;
                    return tracker_route_with_traffic(
                        tracker,
                        RouteEvidence::Direct {
                            port,
                            generation: session.generation,
                            connected: true,
                            role: DeviceRole::Tracker,
                        },
                    );
                }
                self.flash_check_scoped_tracker_io(
                    receiver,
                    address
                        .as_deref()
                        .ok_or_else(|| tr("commands.route.error.address_unknown"))?,
                )?;
                let port = receiver_port(receiver_endpoint, &self.inventory)
                    .ok_or_else(|| tr("commands.route.error.receiver_port_unknown"))?;
                self.console_check_port(&port.port_name)?;
                self.capture_check_port(&port.port_name)?;
                self.flash_only_check_port(&port.port_name)?;
                let session = self.manager.session(&port.port_name);
                let connected =
                    session.is_some_and(|session| session.state == ConnectionState::Connected);
                if require_connected && !connected {
                    return Err(tr("commands.route.error.connect_receiver"));
                }
                if self.serial_role(&port.port_name) != DeviceRole::Receiver {
                    return Err(tr("commands.route.error.not_receiver"));
                }
                tracker_route_with_traffic(
                    tracker,
                    RouteEvidence::Forwarded {
                        port: &port.port_name,
                        generation: session.map_or(0, |session| session.generation),
                        connected,
                        receiver_key: receiver,
                        slot: *slot,
                        address: address.as_deref(),
                        unique_receiver_route: true,
                    },
                )
            }
        }
    }

    fn remote_evidence<'a>(&'a self, focus: &'a RemoteFocus) -> Result<RouteEvidence<'a>, String> {
        self.resolve_remote_evidence(focus, true)
    }

    pub(super) fn resolve_remote_evidence<'a>(
        &'a self,
        focus: &'a RemoteFocus,
        require_connected: bool,
    ) -> Result<RouteEvidence<'a>, String> {
        match focus {
            RemoteFocus::Tracker(focus) => self.route_evidence_for(focus, false, require_connected),
            RemoteFocus::All(key) => {
                self.flash_check_receiver(key)?;
                let receiver = self
                    .inventory
                    .receivers
                    .iter()
                    .find(|receiver| &receiver.key == key)
                    .ok_or_else(|| tr("commands.route.error.receiver_missing"))?;
                let port = receiver_port(receiver, &self.inventory)
                    .ok_or_else(|| tr("commands.route.error.receiver_port_unknown"))?;
                self.console_check_port(&port.port_name)?;
                self.flash_check_port(&port.port_name)?;
                let session = self.manager.session(&port.port_name);
                let connected =
                    session.is_some_and(|session| session.state == ConnectionState::Connected);
                if require_connected && !connected {
                    return Err(tr("commands.route.error.connect_receiver"));
                }
                if self.serial_role(&port.port_name) != DeviceRole::Receiver {
                    return Err(tr("commands.route.error.not_receiver"));
                }
                Ok(RouteEvidence::Broadcast {
                    port: &port.port_name,
                    generation: session.map_or(0, |session| session.generation),
                    connected,
                    receiver_key: key,
                    unique_receiver_route: true,
                })
            }
        }
    }

    pub(super) fn remote_selection_availability(&self) -> (usize, BTreeSet<String>) {
        let mut available = 0;
        let mut reasons = BTreeSet::new();
        for key in self
            .selected
            .iter()
            .filter(|key| key.starts_with("tracker:"))
        {
            let result = if let Some(tracker) = self.selected_tracker(key) {
                let focus = RemoteFocus::Tracker(CommandFocus::Tracker {
                    receiver: tracker.receiver_key.clone(),
                    slot: tracker.slot,
                    address: tracker.address.clone(),
                });
                self.resolve_remote_evidence(&focus, false).map(|_| ())
            } else {
                Err(tr("commands.route.error.tracker_not_listed_reselect"))
            };
            match result {
                Ok(()) => available += 1,
                Err(reason) => {
                    reasons.insert(reason);
                }
            }
        }
        (available, reasons)
    }

    fn preparation_evidence<'a>(
        &'a self,
        source: &'a PrepareTargetSource,
        require_connected: bool,
    ) -> Result<RouteEvidence<'a>, String> {
        match source {
            PrepareTargetSource::Current(focus) => {
                self.route_evidence_for(focus, true, require_connected)
            }
            PrepareTargetSource::Remote(focus) => {
                self.resolve_remote_evidence(focus, require_connected)
            }
        }
    }

    pub(in crate::ui) fn port_endpoint(&self, port: &str) -> Option<&SerialEndpoint> {
        self.inventory
            .ports
            .iter()
            .find(|endpoint| endpoint.port_name == port)
    }

    fn add_preparation_target(
        &self,
        preparation: &mut CommandPreparation,
        source: PrepareTargetSource,
        label: String,
        direct_only: bool,
    ) {
        let result = self.preparation_evidence(&source, false).and_then(|route| {
            if direct_only && matches!(route, RouteEvidence::Forwarded { .. }) {
                return Err(tr("commands.route.error.direct_required"));
            }
            if let PrepareAction::Preset { preset, .. } = &preparation.action
                && preset.receiver_wide_only
                && matches!(route, RouteEvidence::Forwarded { .. })
            {
                return Err(tr("commands.authorization.receiver_wide_required"));
            }
            let (port, _, _) = route_parts(&route);
            Ok((
                port.to_owned(),
                self.port_endpoint(port).cloned(),
                PreparedRouteKind::of(&route),
            ))
        });
        match result {
            Ok((port, endpoint, route_kind)) => {
                if !preparation
                    .connections
                    .iter()
                    .any(|connection| connection.port == port)
                {
                    preparation.connections.push(CommandConnection {
                        port: port.clone(),
                        generation: None,
                    });
                }
                preparation.targets.push(PrepareTarget {
                    source,
                    label,
                    port,
                    endpoint,
                    route_kind,
                });
            }
            Err(reason) => preparation.excluded.push(Excluded { label, reason }),
        }
    }

    fn validate_preparation_target<'a>(
        &'a self,
        target: &'a PrepareTarget,
        connected: bool,
    ) -> Result<RouteEvidence<'a>, String> {
        let route = self.preparation_evidence(&target.source, connected)?;
        target.validate_route(&route, self.port_endpoint(&target.port), &self.inventory)?;
        if let PrepareTargetSource::Current(focus) = &target.source
            && matches!(self.sidebar.scope, CommandScope::Current)
            && self.command_focus.as_ref() != Some(focus)
        {
            return Err(tr("commands.route.error.target_changed_choose"));
        }
        Ok(route)
    }

    fn start_preparation(&mut self, preparation: &mut CommandPreparation) {
        self.poll_sessions();
        preparation.error = None;
        for target in &preparation.targets {
            if let Err(error) = self.validate_preparation_target(target, false) {
                preparation.error = Some(error);
                return;
            }
        }
        for connection in &mut preparation.connections {
            if let Err(error) = connection.start(&mut self.manager) {
                preparation.error = Some(error);
                return;
            }
        }
    }

    pub(super) fn retry_preparation(&mut self, cx: &mut Context<Self>) {
        if let Some(PendingCommand::Connecting(mut preparation)) = self.sidebar.pending.take() {
            self.start_preparation(&mut preparation);
            self.sidebar.pending = Some(PendingCommand::Connecting(preparation));
        }
        cx.notify();
    }

    pub(in crate::ui) fn resume_preparation(&mut self) {
        if !matches!(self.sidebar.pending, Some(PendingCommand::Connecting(_))) {
            return;
        }
        let Some(PendingCommand::Connecting(mut preparation)) = self.sidebar.pending.take() else {
            return;
        };
        if preparation.error.is_some() {
            self.sidebar.pending = Some(PendingCommand::Connecting(preparation));
            return;
        }
        if let Err(error) = preparation
            .targets
            .iter()
            .try_for_each(|target| self.validate_preparation_target(target, false).map(|_| ()))
        {
            preparation.error = Some(error);
            self.sidebar.pending = Some(PendingCommand::Connecting(preparation));
            return;
        }
        let readiness = preparation
            .connections
            .iter()
            .try_fold(true, |ready, connection| {
                connection
                    .ready(&self.manager)
                    .map(|connected| ready && connected)
            });
        let result = match readiness {
            Ok(false) => {
                self.sidebar.pending = Some(PendingCommand::Connecting(preparation));
                return;
            }
            Err(error) => Err(error),
            Ok(true) => self.finish_preparation(&preparation),
        };
        if let Err(error) = result {
            preparation.error = Some(error);
            self.sidebar.pending = Some(PendingCommand::Connecting(preparation));
        }
    }

    fn finish_preparation(&mut self, preparation: &CommandPreparation) -> Result<(), String> {
        for target in &preparation.targets {
            self.validate_preparation_target(target, true)?;
        }
        match &preparation.action {
            PrepareAction::Serial { command } => {
                let candidates = preparation
                    .targets
                    .iter()
                    .map(|target| Candidate {
                        label: target.label.clone(),
                        port: Some(target.port.clone()),
                        exclusion: None,
                    })
                    .chain(preparation.excluded.iter().map(|excluded| Candidate {
                        label: excluded.label.clone(),
                        port: None,
                        exclusion: Some(excluded.reason.clone()),
                    }));
                let snapshot = BatchSnapshot::capture(command.clone(), candidates, &self.manager);
                let guards = preparation
                    .targets
                    .iter()
                    .map(|target| {
                        let port = self
                            .manager
                            .session(&target.port)
                            .map_or(target.port.as_str(), |session| session.port.as_str())
                            .to_owned();
                        SerialGuard {
                            port,
                            target: target.clone(),
                        }
                    })
                    .collect();
                self.sidebar.pending =
                    Some(PendingCommand::Serial(SerialReview { snapshot, guards }));
            }
            PrepareAction::Preset { preset, input } => {
                if preparation.targets.is_empty() {
                    return Err(preparation.excluded.first().map_or_else(
                        || tr("commands.target.select_device"),
                        |item| item.reason.clone(),
                    ));
                }
                let draft = if matches!(self.sidebar.scope, CommandScope::Current) {
                    let target = preparation.targets.first().ok_or_else(|| {
                        preparation
                            .excluded
                            .first()
                            .map_or(tr("commands.target.select_device"), |item| {
                                item.reason.clone()
                            })
                    })?;
                    let PrepareTargetSource::Current(focus) = &target.source else {
                        return Err(tr("commands.route.error.target_changed_select"));
                    };
                    let route = self.validate_preparation_target(target, true)?;
                    let (_, generation, role) = route_parts(&route);
                    let endpoint = target
                        .endpoint
                        .clone()
                        .ok_or_else(|| tr("commands.preparation.error.route_changed"))?;
                    PresetDraft::Single {
                        pin: CommandPin {
                            focus: focus.clone(),
                            generation,
                            role,
                            forwarded: matches!(route, RouteEvidence::Forwarded { .. }),
                            endpoint,
                        },
                        label: target.label.clone(),
                    }
                } else {
                    let mut batch = RemoteBatch {
                        targets: Vec::new(),
                        excluded: preparation.excluded.clone(),
                    };
                    for target in &preparation.targets {
                        let PrepareTargetSource::Remote(focus) = &target.source else {
                            return Err(tr("commands.route.error.target_changed_select"));
                        };
                        batch.capture_target(
                            focus.clone(),
                            target.label.clone(),
                            preset,
                            self.validate_preparation_target(target, true),
                            target.endpoint.clone(),
                        );
                    }
                    PresetDraft::Remote(batch)
                };
                if let Some(input) = input {
                    self.sidebar.pending = Some(PendingCommand::Parameter {
                        preset,
                        input: input.clone(),
                        draft,
                    });
                } else {
                    self.prepare_sidebar_preset(preset, "", draft);
                }
            }
        }
        Ok(())
    }

    pub(in crate::ui) fn change_command_scope(&mut self, scope: CommandScope) {
        self.cancel_terminal_intents();
        self.sidebar.change_scope(scope);
        self.command_error = None;
    }

    pub(in crate::ui) fn selection_changed(&mut self) {
        self.cancel_terminal_intents();
        self.sidebar
            .selection_changed(self.selected.iter().any(|key| key.starts_with("tracker:")));
        self.command_error = None;
        self.sync_flash_tracker_selection();
    }

    pub(super) fn begin_sidebar_preset(
        &mut self,
        preset: &'static commands::Preset,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.poll_sessions();
        self.monitor.poll();
        self.sidebar.pending = None;
        self.command_error = None;
        let input = if matches!(preset.parameter, ParameterSpec::None) {
            None
        } else {
            Some(cx.new(|cx| {
                let mut input =
                    InputState::new(window, cx).placeholder(tr(preset.parameter.hint()));
                input.set_value(preset.parameter.default_value(), window, cx);
                input
            }))
        };
        let mut preparation = CommandPreparation {
            action: PrepareAction::Preset { preset, input },
            targets: Vec::new(),
            excluded: Vec::new(),
            connections: Vec::new(),
            error: None,
        };
        match &self.sidebar.scope {
            CommandScope::Current => {
                if let Some(focus) = &self.command_focus {
                    self.add_preparation_target(
                        &mut preparation,
                        PrepareTargetSource::Current(focus.clone()),
                        self.target_description(focus),
                        false,
                    );
                }
            }
            CommandScope::SelectedTrackers => {
                for key in self
                    .selected
                    .iter()
                    .filter(|key| key.starts_with("tracker:"))
                {
                    if let Some(tracker) = self.selected_tracker(key) {
                        let focus = CommandFocus::Tracker {
                            receiver: tracker.receiver_key.clone(),
                            slot: tracker.slot,
                            address: tracker.address.clone(),
                        };
                        let label = self.target_description(&focus);
                        self.add_preparation_target(
                            &mut preparation,
                            PrepareTargetSource::Remote(RemoteFocus::Tracker(focus)),
                            label,
                            false,
                        );
                    } else {
                        preparation.excluded.push(Excluded {
                            label: tr("commands.target.unavailable_tracker"),
                            reason: tr("commands.route.error.tracker_not_listed"),
                        });
                    }
                }
            }
            CommandScope::AllTrackers(receiver) => {
                let label = self
                    .inventory
                    .receivers
                    .iter()
                    .find(|item| &item.key == receiver)
                    .map_or_else(
                        || tr("commands.target.receiver_unavailable"),
                        |item| self.receiver_label(item, item.label.clone()),
                    );
                self.add_preparation_target(
                    &mut preparation,
                    PrepareTargetSource::Remote(RemoteFocus::All(receiver.clone())),
                    rust_i18n::t!("commands.target.all_online", label = label).to_string(),
                    false,
                );
            }
            CommandScope::SerialBatch => return,
        }
        self.start_preparation(&mut preparation);
        self.sidebar.pending = Some(PendingCommand::Connecting(preparation));
        self.resume_preparation();
        cx.notify();
    }

    pub(super) fn continue_parameter(&mut self, cx: &mut Context<Self>) {
        let Some(PendingCommand::Parameter { preset, input, .. }) = &self.sidebar.pending else {
            return;
        };
        let parameter = input.read(cx).value().to_string();
        // Keep the frozen draft and user's value in place after invalid input.
        if let Err(error) = preset.build(&parameter) {
            self.command_error = Some(error);
            cx.notify();
            return;
        }
        if let Some(PendingCommand::Parameter { preset, draft, .. }) = self.sidebar.pending.take() {
            self.command_error = None;
            self.prepare_sidebar_preset(preset, &parameter, draft);
        }
        cx.notify();
    }

    fn prepare_sidebar_preset(
        &mut self,
        preset: &'static commands::Preset,
        parameter: &str,
        draft: PresetDraft,
    ) {
        match draft {
            PresetDraft::Single { pin, label } => {
                match self
                    .pinned_evidence(&pin)
                    .and_then(|route| CommandSnapshot::capture(preset, parameter, route))
                {
                    Ok(snapshot) if !snapshot.dangerous() => {
                        self.execute_preset(&snapshot, &pin, &label, false)
                    }
                    Ok(snapshot) => {
                        self.sidebar.pending = Some(PendingCommand::Single {
                            snapshot,
                            pin,
                            label,
                        });
                    }
                    Err(error) => self.command_error = Some(error),
                }
            }
            PresetDraft::Remote(batch) => {
                let ready = batch
                    .targets
                    .iter()
                    .try_for_each(|target| self.remote_evidence(&target.focus).map(|_| ()));
                match ready.and_then(|()| batch.with_parameter(parameter)) {
                    Ok(batch) => {
                        self.sidebar.pending = Some(PendingCommand::Remote { preset, batch })
                    }
                    Err(error) => self.command_error = Some(error),
                }
            }
        }
    }

    pub(super) fn confirm_sidebar_command(&mut self, cx: &mut Context<Self>) {
        // Taking the review makes confirmation one-shot, including fast double clicks.
        match self.sidebar.pending.take() {
            Some(PendingCommand::Single {
                snapshot,
                pin,
                label,
            }) => {
                self.execute_preset(&snapshot, &pin, &label, true);
            }
            Some(PendingCommand::Remote { preset, batch }) => {
                self.execute_remote_batch(preset, batch);
            }
            Some(PendingCommand::Serial(snapshot)) => {
                self.execute_batch(snapshot);
            }
            other => self.sidebar.pending = other,
        }
        cx.notify();
    }

    fn execute_remote_batch(&mut self, preset: &'static commands::Preset, batch: RemoteBatch) {
        if !self.reserve_batch_history() {
            return;
        }
        self.poll_sessions();
        self.monitor.poll();
        let admission = batch.authorize_all(|target| {
            if !same_serial_endpoint(
                target.endpoint.as_ref(),
                self.port_endpoint(target.snapshot.port()),
            ) {
                return Err(tr("commands.route.error.device_changed"));
            }
            self.remote_evidence(&target.focus)
                .and_then(|evidence| target.snapshot.authorize(evidence, true))
        });
        let run = batch.execute(preset.label.to_owned(), admission, |target| {
            self.remote_evidence(&target.focus)
                .and_then(|evidence| target.snapshot.authorize(evidence, true))?;
            self.manager
                .send_line(target.snapshot.port(), target.snapshot.command())
        });
        self.batch_history.push_front(run);
    }

    pub(super) fn command_context(&self) -> CommandContext {
        match self.command_focus.as_ref() {
            Some(focus @ CommandFocus::Tracker { .. }) => match self.route_evidence(focus) {
                Ok(RouteEvidence::Direct { .. }) => CommandContext::Direct(DeviceRole::Tracker),
                _ => CommandContext::ForwardedTracker,
            },
            Some(CommandFocus::Serial(port)) => CommandContext::Direct(self.serial_role(port)),
            Some(CommandFocus::Receiver(_)) => CommandContext::Direct(DeviceRole::Receiver),
            None => CommandContext::Direct(DeviceRole::Unknown),
        }
    }

    fn pinned_evidence<'a>(&'a self, pin: &'a CommandPin) -> Result<RouteEvidence<'a>, String> {
        let evidence = self.route_evidence(&pin.focus)?;
        if !same_serial_endpoint(
            Some(&pin.endpoint),
            self.port_endpoint(&pin.endpoint.port_name),
        ) {
            return Err(tr("commands.route.error.device_changed"));
        }
        validate_command_pin(pin, self.command_focus.as_ref(), &evidence)?;
        Ok(evidence)
    }

    fn execute_preset(
        &mut self,
        snapshot: &CommandSnapshot,
        pin: &CommandPin,
        label: &str,
        confirmed: bool,
    ) {
        if !self.reserve_batch_history() {
            return;
        }
        self.poll_sessions();
        self.monitor.poll();
        let result = self
            .pinned_evidence(pin)
            .and_then(|evidence| snapshot.authorize(evidence, confirmed))
            .and_then(|()| self.manager.send_line(snapshot.port(), snapshot.command()));
        let label = format!("{label} · {}", snapshot.port());
        self.batch_history.push_front(BatchRun {
            command: snapshot.command().to_owned(),
            targets: vec![TargetOutcome::submitted(label, result)],
        });
    }

    fn serial_preparation(&self, command: String) -> CommandPreparation {
        let mut preparation = CommandPreparation {
            action: PrepareAction::Serial { command },
            targets: Vec::new(),
            excluded: Vec::new(),
            connections: Vec::new(),
            error: None,
        };
        for key in &self.selected {
            if let Some(port) = key.strip_prefix("port:") {
                self.add_preparation_target(
                    &mut preparation,
                    PrepareTargetSource::Current(CommandFocus::Serial(port.to_owned())),
                    self.serial_device_label(port, port),
                    true,
                );
            } else if let Some(tracker) = self.selected_tracker(key) {
                let focus = CommandFocus::Tracker {
                    receiver: tracker.receiver_key.clone(),
                    slot: tracker.slot,
                    address: tracker.address.clone(),
                };
                let label = self.target_description(&focus);
                self.add_preparation_target(
                    &mut preparation,
                    PrepareTargetSource::Current(focus),
                    label,
                    true,
                );
            } else {
                preparation.excluded.push(Excluded {
                    label: tr("commands.target.unavailable_tracker"),
                    reason: tr("commands.route.error.device_missing"),
                });
            }
        }
        preparation
    }

    pub(super) fn review_batch(&mut self, cx: &mut Context<Self>) {
        if self
            .batch_input
            .read(cx)
            .value()
            .trim_end_matches(['\r', '\n'])
            .is_empty()
            || self.selected.is_empty()
        {
            return;
        }
        self.command_error = None;
        let command = self
            .batch_input
            .read(cx)
            .value()
            .trim_end_matches(['\r', '\n'])
            .to_owned();
        let mut preparation = self.serial_preparation(command);
        self.start_preparation(&mut preparation);
        self.sidebar.pending = Some(PendingCommand::Connecting(preparation));
        self.resume_preparation();
        cx.notify();
    }

    fn reserve_batch_history(&mut self) -> bool {
        const HISTORY_LIMIT: usize = 16;
        if self.batch_history.len() == HISTORY_LIMIT {
            if let Some(index) = self.batch_history.iter().rposition(|run| !run.pending()) {
                self.batch_history.remove(index);
            } else {
                self.error = Some(tr("commands.preparation.error.batch_busy"));
                return false;
            }
        }
        true
    }

    fn execute_batch(&mut self, review: SerialReview) {
        if !self.reserve_batch_history() {
            return;
        }
        self.poll_sessions();
        self.monitor.poll();
        let admission =
            review.authorize_all(|guard| self.validate_preparation_target(guard, true).map(|_| ()));
        let run = review.execute(admission, |review, target| {
            let current = self
                .manager
                .session(&target.port)
                .map(|session| (session.state, session.generation));
            review.authorize(target, |guard| {
                self.validate_preparation_target(guard, true).map(|_| ())
            })?;
            crate::operations::send_if_current(
                target,
                &review.snapshot.command,
                current,
                |port, command| self.manager.send_line(port, command),
            )
        });
        self.batch_history.push_front(run);
    }
}
