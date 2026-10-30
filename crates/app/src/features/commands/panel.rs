#[path = "panel/actions.rs"]
mod actions;
#[path = "panel/cards.rs"]
mod cards;
#[cfg(test)]
#[path = "panel/connection_tests.rs"]
mod connection_tests;
#[cfg(test)]
#[path = "panel/consumer_tests.rs"]
mod consumer_tests;
#[cfg(test)]
#[path = "panel/presentation_tests.rs"]
mod presentation_tests;
#[cfg(test)]
#[path = "panel/remote_batch_tests.rs"]
mod remote_batch_tests;
#[cfg(test)]
#[path = "panel/routing_tests.rs"]
mod routing_tests;
#[cfg(test)]
#[path = "panel/sidebar_transition_tests.rs"]
mod sidebar_transition_tests;

use super::{
    Workbench,
    components::{command_button_tone, context_button},
    shortcuts::{custom_step_summary, target_summary},
};
use crate::{
    domain::{DeviceRole, Inventory, SerialEndpoint, TrackerLink, console_port_allowed},
    features::commands::{
        batch::{BatchRun, BatchSnapshot, CommandOrigin, Excluded, Outcome, RunId, TargetOutcome},
        policy::{self as commands, CommandContext, CommandSnapshot, RouteEvidence},
    },
    i18n::tr,
    sessions::{ConnectionState, SessionManager, WriteReceipt},
};
use cards::command_card;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::{
    AnyElement, Context, Div, Entity, Focusable as _, FontWeight, SharedString, div, prelude::*, px,
};
use gpui_kit::{
    App,
    component::{
        ActiveTheme, Disableable, IconName, Sizable,
        button::{Button, ButtonVariants},
        input::{Input, InputState},
        scroll::ScrollableElement,
    },
};
use std::{collections::BTreeSet, fmt::Write as _};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum CommandFocus {
    Serial(String),
    Receiver(String),
    Tracker {
        receiver: String,
        slot: u8,
        address: Option<String>,
    },
}

#[derive(Clone)]
pub(super) struct CommandPin {
    pub(super) focus: CommandFocus,
    generation: u64,
    role: DeviceRole,
    forwarded: bool,
    endpoint: SerialEndpoint,
}

pub(super) struct CommandConnection {
    pub(super) port: String,
    pub(super) generation: Option<u64>,
}

impl CommandConnection {
    pub(super) fn start(&mut self, manager: &mut SessionManager) -> Result<(), String> {
        if let Some(expected) = self.generation
            && manager
                .session(&self.port)
                .is_none_or(|session| session.generation != expected)
        {
            return Err(rust_i18n::t!(
                "commands.preparation.error.connection_changed",
                port = self.port
            )
            .to_string());
        }
        match manager.session(&self.port).map(|session| session.state) {
            Some(ConnectionState::Connected | ConnectionState::Connecting) => {}
            Some(ConnectionState::Closing) => {
                self.generation = manager
                    .session(&self.port)
                    .map(|session| session.generation);
                return Err(rust_i18n::t!(
                    "commands.preparation.error.connection_closing",
                    port = self.port
                )
                .to_string());
            }
            _ => {
                let result = manager.connect(&self.port);
                self.generation = manager
                    .session(&self.port)
                    .map(|session| session.generation);
                result?;
            }
        }
        self.generation = manager
            .session(&self.port)
            .map(|session| session.generation);
        Ok(())
    }

    pub(super) fn ready(&self, manager: &SessionManager) -> Result<bool, String> {
        let session = manager
            .session(&self.port)
            .ok_or_else(|| tr("commands.preparation.error.connection_unavailable"))?;
        if Some(session.generation) != self.generation {
            return Err(rust_i18n::t!(
                "commands.preparation.error.connection_changed",
                port = self.port
            )
            .to_string());
        }
        match session.state {
            ConnectionState::Connected => Ok(true),
            ConnectionState::Connecting => Ok(false),
            _ => Err(session.last_error.clone().unwrap_or_else(|| {
                rust_i18n::t!("commands.preparation.error.not_connected", port = self.port)
                    .to_string()
            })),
        }
    }
}

#[derive(Clone)]
pub(super) enum PrepareTargetSource {
    Current(CommandFocus),
    Remote(RemoteFocus),
}

#[derive(Clone)]
pub(super) struct PrepareTarget {
    pub(super) source: PrepareTargetSource,
    pub(super) label: String,
    port: String,
    endpoint: Option<SerialEndpoint>,
    route_kind: PreparedRouteKind,
}

impl PrepareTarget {
    fn validate_route(
        &self,
        route: &RouteEvidence<'_>,
        endpoint: Option<&SerialEndpoint>,
        inventory: &Inventory,
    ) -> Result<(), String> {
        if route_parts(route).0 != self.port
            || PreparedRouteKind::of(route) != self.route_kind
            || !same_serial_endpoint(self.endpoint.as_ref(), endpoint)
            || !endpoint.is_some_and(|endpoint| console_port_allowed(endpoint, inventory))
        {
            return Err(tr("commands.preparation.error.route_changed"));
        }
        Ok(())
    }
}

struct SerialGuard {
    port: String,
    target: PrepareTarget,
}

pub(super) struct SerialReview {
    snapshot: BatchSnapshot,
    guards: Vec<SerialGuard>,
}

impl SerialReview {
    fn target_label<'a>(&'a self, target: &'a crate::features::commands::batch::Target) -> &'a str {
        self.guards
            .iter()
            .find(|guard| guard.port == target.port)
            .map_or(target.port.as_str(), |guard| guard.target.label.as_str())
    }

    fn authorize(
        &self,
        target: &crate::features::commands::batch::Target,
        mut validate: impl FnMut(&PrepareTarget) -> Result<(), String>,
    ) -> Result<(), String> {
        let mut matched = false;
        for guard in self.guards.iter().filter(|guard| guard.port == target.port) {
            matched = true;
            validate(&guard.target)?;
        }
        if !matched {
            return Err(tr("commands.preparation.error.target_unavailable"));
        }
        Ok(())
    }

    fn authorize_all(
        &self,
        mut validate: impl FnMut(&PrepareTarget) -> Result<(), String>,
    ) -> Result<(), String> {
        self.snapshot
            .targets
            .iter()
            .try_for_each(|target| self.authorize(target, &mut validate))
    }

    fn execute(
        self,
        admission: Result<(), String>,
        mut submit: impl FnMut(
            &Self,
            &crate::features::commands::batch::Target,
        ) -> Result<WriteReceipt, String>,
    ) -> BatchRun {
        let targets = self
            .snapshot
            .targets
            .iter()
            .map(|target| {
                let result = admission.clone().and_then(|()| submit(&self, target));
                TargetOutcome::submitted(self.target_label(target).to_owned(), result)
            })
            .collect();
        BatchRun {
            command: self.snapshot.command,
            targets,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PreparedRouteKind {
    Direct,
    Forwarded,
    Broadcast,
}

impl PreparedRouteKind {
    fn of(route: &RouteEvidence<'_>) -> Self {
        match route {
            RouteEvidence::Direct { .. } => Self::Direct,
            RouteEvidence::Forwarded { .. } => Self::Forwarded,
            RouteEvidence::Broadcast { .. } => Self::Broadcast,
        }
    }
}

pub(super) enum PrepareAction {
    Preset {
        preset: &'static commands::Preset,
        input: Option<Entity<InputState>>,
    },
    Serial {
        command: String,
    },
}

pub(super) struct CommandPreparation {
    pub(super) action: PrepareAction,
    pub(super) targets: Vec<PrepareTarget>,
    excluded: Vec<Excluded>,
    connections: Vec<CommandConnection>,
    error: Option<String>,
}

#[derive(Clone)]
pub(super) enum RemoteFocus {
    Tracker(CommandFocus),
    All(String),
}

pub(super) struct RemoteTarget {
    pub(super) focus: RemoteFocus,
    pub(super) label: String,
    snapshot: CommandSnapshot,
    endpoint: Option<SerialEndpoint>,
}

#[derive(Default)]
pub(super) struct RemoteBatch {
    pub(super) targets: Vec<RemoteTarget>,
    excluded: Vec<Excluded>,
}

impl RemoteBatch {
    fn capture_target(
        &mut self,
        focus: RemoteFocus,
        label: String,
        preset: &'static commands::Preset,
        evidence: Result<RouteEvidence<'_>, String>,
        endpoint: Option<SerialEndpoint>,
    ) {
        match evidence.and_then(|route| {
            CommandSnapshot::capture(preset, preset.parameter.default_value(), route)
        }) {
            Ok(snapshot) => self.targets.push(RemoteTarget {
                focus,
                label,
                snapshot,
                endpoint,
            }),
            Err(reason) => self.excluded.push(Excluded { label, reason }),
        }
    }

    fn with_parameter(mut self, parameter: &str) -> Result<Self, String> {
        self.targets = self
            .targets
            .into_iter()
            .map(|target| {
                Ok(RemoteTarget {
                    snapshot: target.snapshot.with_parameter(parameter)?,
                    ..target
                })
            })
            .collect::<Result<_, String>>()?;
        Ok(self)
    }

    fn authorize_all(
        &self,
        mut validate: impl FnMut(&RemoteTarget) -> Result<(), String>,
    ) -> Result<(), String> {
        self.targets.iter().try_for_each(&mut validate)
    }

    fn execute(
        self,
        command: String,
        admission: Result<(), String>,
        mut submit: impl FnMut(&RemoteTarget) -> Result<WriteReceipt, String>,
    ) -> BatchRun {
        let mut targets = Vec::with_capacity(self.targets.len() + self.excluded.len());
        for target in self.targets {
            let result = admission.clone().and_then(|()| submit(&target));
            targets.push(TargetOutcome::submitted(
                format!(
                    "{} · {} · {}",
                    target.label,
                    target.snapshot.port(),
                    target.snapshot.command()
                ),
                result,
            ));
        }
        for excluded in self.excluded {
            targets.push(TargetOutcome::submitted(
                excluded.label,
                Err(
                    rust_i18n::t!("commands.review.excluded", reason = excluded.reason).to_string(),
                ),
            ));
        }
        BatchRun { command, targets }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) enum CommandScope {
    #[default]
    Current,
    SelectedTrackers,
    AllTrackers(String),
    SerialBatch,
}

pub(super) enum PresetDraft {
    Single { pin: CommandPin, label: String },
    Remote(RemoteBatch),
}

pub(super) enum PendingCommand {
    Parameter {
        preset: &'static commands::Preset,
        input: Entity<InputState>,
        draft: PresetDraft,
    },
    Single {
        snapshot: CommandSnapshot,
        pin: CommandPin,
        label: String,
    },
    Remote {
        preset: &'static commands::Preset,
        batch: RemoteBatch,
    },
    Serial(SerialReview),
    Connecting(CommandPreparation),
}

#[derive(Default)]
pub(super) struct CommandSidebar {
    pub(super) scope: CommandScope,
    history: bool,
    pub(super) pending: Option<PendingCommand>,
    expanded_terminal_runs: BTreeSet<RunId>,
    collapsed_groups: BTreeSet<&'static str>,
}

impl CommandSidebar {
    fn toggle_group(&mut self, group: &'static str) {
        if !self.collapsed_groups.remove(group) {
            self.collapsed_groups.insert(group);
        }
    }

    pub(super) fn shows_current_device(&self) -> bool {
        !self.history && self.scope == CommandScope::Current
    }

    fn change_scope(&mut self, scope: CommandScope) {
        if self.scope != scope {
            self.pending = None;
            self.scope = scope;
        }
        self.history = false;
    }

    fn select_tab(&mut self, history: bool) {
        self.history = history;
    }

    fn focus_changed(&mut self) {
        self.pending = None;
        self.change_scope(CommandScope::Current);
    }

    fn selection_changed(&mut self, has_trackers: bool) {
        self.pending = None;
        self.change_scope(if has_trackers {
            CommandScope::SelectedTrackers
        } else {
            CommandScope::SerialBatch
        });
    }
}

impl Workbench {
    fn sidebar_scope_summary(&self) -> (String, bool) {
        match &self.sidebar.scope {
            CommandScope::Current => {
                let Some(focus) = self.command_focus.as_ref() else {
                    return (tr("commands.target.select_card"), false);
                };
                let label = self.target_description(focus);
                match self.route_evidence_for(focus, true, false) {
                    Ok(_) if matches!(focus, CommandFocus::Serial(_)) => (label, true),
                    Ok(route) => {
                        let (port, _, _) = route_parts(&route);
                        (format!("{label} · {port}"), true)
                    }
                    Err(error) => (format!("{label} · {error}"), false),
                }
            }
            CommandScope::SelectedTrackers => {
                let count = self
                    .selected
                    .iter()
                    .filter(|key| key.starts_with("tracker:"))
                    .count();
                let (available, reasons) = self.remote_selection_availability();
                let mut label = rust_i18n::t!(
                    "commands.target.selected_trackers_summary",
                    count = count,
                    available = available,
                    excluded = count - available
                )
                .to_string();
                for reason in reasons {
                    let _ = write!(label, " · {reason}");
                }
                (label, available > 0)
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
                let focus = RemoteFocus::All(receiver.clone());
                match self.resolve_remote_evidence(&focus, false) {
                    Ok(route) => {
                        let (port, _, _) = route_parts(&route);
                        (
                            rust_i18n::t!(
                                "commands.target.all_online_summary",
                                label = label,
                                port = port
                            )
                            .to_string(),
                            true,
                        )
                    }
                    Err(error) => (
                        rust_i18n::t!(
                            "commands.target.all_online_error",
                            label = label,
                            error = error
                        )
                        .to_string(),
                        false,
                    ),
                }
            }
            CommandScope::SerialBatch => (
                rust_i18n::t!(
                    "commands.target.selected_devices_summary",
                    count = self.selected.len()
                )
                .to_string(),
                !self.selected.is_empty(),
            ),
        }
    }

    fn sidebar_catalog(&self, cx: &mut Context<Self>) -> AnyElement {
        let (summary, enabled) = self.sidebar_scope_summary();
        let mut content = div().flex().flex_col().gap_3().min_w_0();
        if !enabled && self.sidebar.scope != CommandScope::SerialBatch {
            return content
                .child(
                    div()
                        .pt_3()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(tr("commands.target.choose")),
                )
                .when(self.sidebar.scope != CommandScope::Current, |content| {
                    content.child(sidebar_target(&summary).text_color(cx.theme().muted_foreground))
                })
                .into_any_element();
        }
        if self.sidebar.scope != CommandScope::Current {
            content =
                content.child(sidebar_target(&summary).text_color(cx.theme().muted_foreground));
        }
        if self.sidebar.scope == CommandScope::SerialBatch {
            return content
                .child(Input::new(&self.batch_input))
                .child(
                    Button::new("review-batch")
                        .label(tr("commands.review.selected"))
                        .primary()
                        .when(!cx.theme().is_dark(), |button| button.outline())
                        .disabled(!enabled)
                        .on_click(cx.listener(|this, _, _, cx| this.review_batch(cx))),
                )
                .into_any_element();
        }
        let context = if self.sidebar.scope == CommandScope::Current {
            self.command_context()
        } else {
            CommandContext::ForwardedTracker
        };
        let query = self.command_search.read(cx).value().trim().to_lowercase();
        let searching = !query.is_empty();
        let mut has_matches = false;
        for group in commands::preset_groups(context) {
            let mut matches = commands::presets(context)
                .filter(|preset| {
                    preset.group == group
                        && (!searching
                            || preset.command.to_lowercase().contains(&query)
                            || tr(preset.label).to_lowercase().contains(&query)
                            || tr(preset.description).to_lowercase().contains(&query))
                })
                .peekable();
            if matches.peek().is_none() {
                continue;
            }
            has_matches = true;
            let collapsed = !searching && self.sidebar.collapsed_groups.contains(group);
            let label = SharedString::from(tr(group));
            let mut choices = div()
                .flex()
                .flex_col()
                .gap_2()
                .min_w_0()
                .pr_1()
                .pb_3()
                .border_b_1()
                .border_color(cx.theme().border.opacity(0.5))
                .child(
                    Button::new(group)
                        .small()
                        .ghost()
                        .w_full()
                        .icon(if collapsed {
                            IconName::ChevronRight
                        } else {
                            IconName::ChevronDown
                        })
                        .disabled(searching)
                        .accessibility_label(label.clone())
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_left()
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_ellipsis()
                                .child(label),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.sidebar.toggle_group(group);
                            cx.notify();
                        })),
                );
            if !collapsed {
                for preset in matches {
                    choices = choices.child(
                        command_card(
                            SharedString::from(format!("sidebar-preset:{}", preset.id)),
                            preset,
                            enabled,
                            cx,
                        )
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.begin_sidebar_preset(preset, window, cx);
                            },
                        )),
                    );
                }
            }
            content = content.child(choices);
        }
        if searching && !has_matches {
            let message = tr("commands.search.empty");
            content = content.child(
                div()
                    .flex()
                    .flex_col()
                    .items_start()
                    .gap_2()
                    .text_sm()
                    .whitespace_normal()
                    .child(
                        div()
                            .id("command-search-empty")
                            .role(gpui_kit::Role::Status)
                            .aria_label(message.clone())
                            .test_support()
                            .child(message),
                    )
                    .child(
                        Button::new("command-clear-search")
                            .label(tr("commands.search.clear"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.command_search
                                    .update(cx, |input, cx| input.set_value("", window, cx));
                                this.command_search.focus_handle(cx).focus(window, cx);
                            })),
                    ),
            );
        }
        content.into_any_element()
    }

    fn sidebar_pending(&self, pending: &PendingCommand, cx: &mut Context<Self>) -> AnyElement {
        let mut content = div().flex().flex_col().gap_3().min_w_0();
        let can_send;
        let is_parameter = matches!(pending, PendingCommand::Parameter { .. });
        let is_connecting = matches!(pending, PendingCommand::Connecting(_));
        let tone = match pending {
            PendingCommand::Single { snapshot, .. } => commands::command_tone(snapshot.command()),
            PendingCommand::Remote { batch, .. } => batch
                .targets
                .iter()
                .map(|target| commands::command_tone(target.snapshot.command()))
                .max()
                .unwrap_or(commands::CommandTone::Normal),
            PendingCommand::Serial(review) => commands::command_tone(&review.snapshot.command),
            _ => commands::CommandTone::Normal,
        };
        match pending {
            PendingCommand::Connecting(preparation) => {
                content = content.child(match &preparation.action {
                    PrepareAction::Preset { preset, .. } => div().text_sm().child(tr(preset.label)),
                    PrepareAction::Serial { command } => sidebar_command(command, cx),
                });
                for target in &preparation.targets {
                    content =
                        content
                            .child(sidebar_target(&target.label))
                            .child(div().text_xs().child(if preparation.error.is_some() {
                                target.port.clone()
                            } else {
                                rust_i18n::t!("commands.preparation.connecting", port = target.port)
                                    .to_string()
                            }));
                }
                for excluded in &preparation.excluded {
                    content = content.child(sidebar_target(&excluded.label)).child(
                        div().text_xs().child(
                            rust_i18n::t!("commands.review.excluded", reason = excluded.reason)
                                .to_string(),
                        ),
                    );
                }
                if let Some(error) = &preparation.error {
                    content = content.child(
                        div()
                            .text_xs()
                            .whitespace_normal()
                            .text_color(cx.theme().danger)
                            .child(error.clone()),
                    );
                }
                can_send = preparation.error.is_some();
            }
            PendingCommand::Parameter {
                preset,
                input,
                draft,
            } => {
                content = content.child(div().text_lg().child(tr(preset.label)));
                match draft {
                    PresetDraft::Single { label, pin } => {
                        content = content
                            .child(sidebar_target(label))
                            .child(div().text_sm().child(pin.endpoint.port_name.clone()));
                    }
                    PresetDraft::Remote(batch) => {
                        content = content.child(remote_preview(batch, preset, false, cx));
                    }
                }
                content = content
                    .child(div().text_xs().child(tr(preset.parameter.hint())))
                    .child(parameter_editor(preset, input));
                can_send = true;
            }
            PendingCommand::Single {
                snapshot, label, ..
            } => {
                content = content
                    .child(div().text_lg().child(tr("commands.review.command")))
                    .child(sidebar_target(label))
                    .child(div().text_sm().child(snapshot.port().to_owned()))
                    .child(sidebar_command(snapshot.command(), cx));
                can_send = true;
            }
            PendingCommand::Remote { preset, batch } => {
                content = content
                    .child(div().text_lg().child(tr(preset.label)))
                    .child(remote_preview(batch, preset, true, cx));
                can_send = !batch.targets.is_empty();
            }
            PendingCommand::Serial(review) => {
                let snapshot = &review.snapshot;
                content = content
                    .child(div().text_lg().child(tr("commands.review.serial_batch")))
                    .child(sidebar_command(&snapshot.command, cx));
                for target in &snapshot.targets {
                    content = content.child(
                        div()
                            .text_sm()
                            .child(review.target_label(target).to_owned()),
                    );
                }
                for excluded in &snapshot.excluded {
                    content = content.child(sidebar_target(&excluded.label)).child(
                        div().text_xs().child(
                            rust_i18n::t!("commands.review.excluded", reason = excluded.reason)
                                .to_string(),
                        ),
                    );
                }
                can_send = !snapshot.targets.is_empty();
            }
        }
        content
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .child(
                        Button::new("cancel-sidebar-command")
                            .label(tr("common.action.cancel"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.sidebar.pending = None;
                                this.command_error = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("continue-sidebar-command")
                            .primary()
                            .when(!cx.theme().is_dark(), |button| button.outline())
                            .map(|button| command_button_tone(button, tone, cx))
                            .label(tr(if is_connecting {
                                "commands.action.retry"
                            } else if is_parameter {
                                "commands.action.continue"
                            } else {
                                "commands.action.confirm_send"
                            }))
                            .disabled(!can_send)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if is_connecting {
                                    this.retry_preparation(cx);
                                } else if is_parameter {
                                    this.continue_parameter(cx);
                                } else {
                                    this.confirm_sidebar_command(cx);
                                }
                            })),
                    ),
            )
            .into_any_element()
    }

    pub(super) fn command_sidebar(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut tabs = div()
            .id("command-sidebar-tabs")
            .flex()
            .flex_shrink_0()
            .gap_1()
            .p_2()
            .border_b_1()
            .border_color(cx.theme().border);
        for (id, label, history) in [
            ("command-tab-commands", tr("commands.tabs.commands"), false),
            (
                "command-tab-history",
                rust_i18n::t!(
                    "commands.tabs.history",
                    count = self.batch_history.len() + self.terminal_command_runs().count()
                )
                .to_string(),
                true,
            ),
        ] {
            tabs = tabs.child(
                context_button(
                    Button::new(id).small().ghost().flex_1().label(label),
                    self.sidebar.history == history,
                    cx,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.sidebar.select_tab(history);
                    cx.notify();
                })),
            );
        }
        let mut sidebar = div()
            .size_full()
            .flex()
            .flex_col()
            .min_w_0()
            .min_h_0()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().sidebar)
            .rounded_md()
            .overflow_hidden()
            .child(tabs);
        if self.sidebar.history {
            return sidebar
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .min_w_0()
                        .p_3()
                        .child(self.history_view(cx))
                        .overflow_y_scrollbar()
                        .id("sidebar-history-scroll"),
                )
                .into_any_element();
        }
        let mut scopes = div()
            .flex()
            .flex_wrap()
            .gap_1()
            .p_2()
            .flex_shrink_0()
            .bg(cx.theme().muted)
            .border_b_1()
            .border_color(cx.theme().border);
        for (id, label, scope) in [
            ("current", "commands.scope.current", CommandScope::Current),
            (
                "trackers",
                "commands.scope.selected_trackers",
                CommandScope::SelectedTrackers,
            ),
            (
                "serial",
                "commands.scope.serial_batch",
                CommandScope::SerialBatch,
            ),
        ] {
            let button = Button::new(SharedString::from(format!("scope:{id}")))
                .small()
                .ghost()
                .label(tr(label));
            let button = context_button(button, self.sidebar.scope == scope, cx);
            scopes = scopes.child(button.on_click(cx.listener(move |this, _, _, cx| {
                this.change_command_scope(scope.clone());
                cx.notify();
            })));
        }
        if matches!(self.sidebar.scope, CommandScope::AllTrackers(_)) {
            scopes = scopes.child(
                div()
                    .text_xs()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(cx.theme().accent)
                    .text_color(cx.theme().accent_foreground)
                    .child(tr("commands.scope.all_online")),
            );
        }
        sidebar = sidebar.child(scopes);
        if self.sidebar.pending.is_none() && self.sidebar.scope != CommandScope::SerialBatch {
            sidebar = sidebar.child(
                div().flex_shrink_0().min_w_0().px_3().pt_3().child(
                    Input::new(&self.command_search)
                        .id("command-search")
                        .aria_label(tr("commands.search.placeholder"))
                        .prefix(IconName::Search)
                        .cleanable(true),
                ),
            );
        }
        let mut body = div().flex().flex_col().gap_3().min_w_0();
        if self.sidebar.shows_current_device() {
            let (summary, _) = self.sidebar_scope_summary();
            body = body.child(self.current_device_nickname_header(&summary, cx));
        }
        if let Some(error) = &self.command_error {
            body = body.child(
                div()
                    .text_xs()
                    .whitespace_normal()
                    .text_color(cx.theme().danger)
                    .child(error.clone()),
            );
        }
        body = body.child(match &self.sidebar.pending {
            Some(pending) => self.sidebar_pending(pending, cx),
            None => self.sidebar_catalog(cx),
        });
        sidebar
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .p_3()
                    .child(body)
                    .overflow_y_scrollbar()
                    .id("sidebar-command-scroll"),
            )
            .into_any_element()
    }

    fn history_view(&self, cx: &Context<Self>) -> AnyElement {
        let mut rows = div().flex().flex_col().gap_3().min_w_0();
        let mut terminal_runs = self.terminal_command_runs().peekable();
        if self.batch_history.is_empty() && terminal_runs.peek().is_none() {
            rows = rows.child(div().text_sm().child(tr("commands.history.empty")));
        }
        for run in terminal_runs {
            let CommandOrigin::Terminal { source_port } = run.origin() else {
                continue;
            };
            let id = run.id();
            let summary = run.summary();
            let expanded = self.sidebar.expanded_terminal_runs.contains(&id);
            let mut group = div()
                .flex()
                .flex_col()
                .gap_2()
                .min_w_0()
                .pb_3()
                .border_b_1()
                .border_color(cx.theme().border)
                .child(
                    div().text_xs().whitespace_normal().child(
                        rust_i18n::t!("commands.history.terminal_source", port = source_port)
                            .to_string(),
                    ),
                )
                .child(
                    div()
                        .text_xs()
                        .whitespace_normal()
                        .text_color(if summary.failed > 0 {
                            cx.theme().danger
                        } else {
                            cx.theme().muted_foreground
                        })
                        .child(
                            rust_i18n::t!(
                                "commands.history.terminal_summary",
                                written = summary.written,
                                pending = summary.pending,
                                failed = summary.failed
                            )
                            .to_string(),
                        ),
                );
            let mut actions = div().flex().flex_wrap().items_center().gap_2().child(
                Button::new(SharedString::from(format!(
                    "history-terminal-details:{}",
                    id.get()
                )))
                .small()
                .ghost()
                .label(tr(if expanded {
                    "shortcuts.result.hide_details"
                } else {
                    "shortcuts.result.details"
                }))
                .tooltip(tr("shortcuts.result.transport_only"))
                .on_click(cx.listener(move |this, _, _, cx| {
                    let retained: BTreeSet<_> =
                        this.terminal_command_runs().map(|run| run.id()).collect();
                    this.sidebar
                        .expanded_terminal_runs
                        .retain(|id| retained.contains(id));
                    if !this.sidebar.expanded_terminal_runs.remove(&id) {
                        this.sidebar.expanded_terminal_runs.insert(id);
                    }
                    cx.notify();
                })),
            );
            if run.pending() {
                actions = actions.child(
                    Button::new(SharedString::from(format!(
                        "history-terminal-stop:{}",
                        id.get()
                    )))
                    .small()
                    .ghost()
                    .label(tr("shortcuts.action.stop"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.cancel_custom_run(id);
                        cx.notify();
                    })),
                );
            } else {
                actions = actions.child(
                    Button::new(SharedString::from(format!(
                        "history-terminal-dismiss:{}",
                        id.get()
                    )))
                    .small()
                    .ghost()
                    .icon(IconName::Close)
                    .tooltip(tr("shortcuts.result.dismiss"))
                    .accessibility_label(tr("shortcuts.result.dismiss"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if this.dismiss_custom_run(id) {
                            this.sidebar.expanded_terminal_runs.remove(&id);
                        }
                        cx.notify();
                    })),
                );
            }
            group = group.child(actions);
            if expanded {
                for target in run.targets() {
                    let mut details = div().text_xs().whitespace_normal().child(format!(
                        "{} · {}",
                        target.label,
                        target_summary(&target.steps)
                    ));
                    for (index, step) in target.steps.iter().enumerate() {
                        details = details.child(div().child(format!(
                            "{}. {}",
                            index + 1,
                            custom_step_summary(step)
                        )));
                    }
                    group = group.child(details);
                }
            }
            rows = rows.child(group);
        }
        for (index, run) in self.batch_history.iter().enumerate() {
            let mut group = div()
                .flex()
                .flex_col()
                .gap_2()
                .min_w_0()
                .pb_3()
                .border_b_1()
                .border_color(cx.theme().border)
                .child(
                    div().text_xs().child(
                        rust_i18n::t!("commands.history.command_number", number = index + 1)
                            .to_string(),
                    ),
                )
                .child(sidebar_command(&run.command, cx));
            for target in &run.targets {
                let outcome = match &target.outcome {
                    Outcome::Queued => tr("commands.history.queued"),
                    Outcome::Written => tr("commands.history.sent"),
                    Outcome::Failed(error) => {
                        rust_i18n::t!("commands.history.failed", error = error).to_string()
                    }
                };
                group = group.child(
                    sidebar_target(&target.label).child(
                        div()
                            .text_xs()
                            .whitespace_normal()
                            .text_color(if matches!(&target.outcome, Outcome::Failed(_)) {
                                cx.theme().danger
                            } else {
                                cx.theme().muted_foreground
                            })
                            .child(outcome),
                    ),
                );
            }
            rows = rows.child(group);
        }
        rows.into_any_element()
    }
}

fn sidebar_target(label: &str) -> Div {
    let mut lines = div().flex().flex_col().gap_1().min_w_0().w_full();
    for part in label.split(" · ") {
        lines = lines.child(
            div()
                .text_xs()
                .w_full()
                .whitespace_normal()
                .child(part.to_owned()),
        );
    }
    lines
}

fn sidebar_command(command: &str, cx: &App) -> Div {
    div()
        .text_sm()
        .font_family(cx.theme().mono_font_family.clone())
        .w_full()
        .whitespace_normal()
        .child(command.to_owned())
}

fn remote_preview(batch: &RemoteBatch, preset: &commands::Preset, commands: bool, cx: &App) -> Div {
    let broadcast = batch
        .targets
        .iter()
        .any(|target| matches!(target.focus, RemoteFocus::All(_)));
    let mut content = div().flex().flex_col().gap_3().min_w_0().w_full().child(
        div().text_sm().whitespace_normal().child(tr(if broadcast {
            if preset.receiver_wide_only {
                "commands.review.receiver_wide_description"
            } else {
                "commands.review.all_online_description"
            }
        } else {
            "commands.review.selected_description"
        })),
    );
    if !commands && !broadcast {
        return content.child(
            div().text_xs().child(
                rust_i18n::t!(
                    "commands.review.selected_trackers_summary",
                    count = batch.targets.len(),
                    excluded = batch.excluded.len()
                )
                .to_string(),
            ),
        );
    }
    for target in &batch.targets {
        let mut row = sidebar_target(&target.label)
            .child(div().text_sm().child(target.snapshot.port().to_owned()));
        if commands {
            row = row.child(sidebar_command(target.snapshot.command(), cx));
        }
        content = content.child(row);
    }
    for excluded in &batch.excluded {
        content = content.child(sidebar_target(&excluded.label).child(
            div().text_xs().whitespace_normal().child(
                rust_i18n::t!("commands.review.excluded", reason = excluded.reason).to_string(),
            ),
        ));
    }
    content
}

fn parameter_editor(
    preset: &'static commands::Preset,
    parameter: &Entity<InputState>,
) -> AnyElement {
    let mut editor = div().flex().flex_col().gap_2();
    let choices = preset.parameter_presets();
    if !choices.is_empty() {
        // Keep wrapping content unconstrained; the separate viewport measures
        // its full height so later rows remain scrollable instead of clipped.
        let mut presets = div().w_full().flex().flex_wrap().flex_shrink_0().gap_1();
        for choice in choices {
            let input = parameter.clone();
            presets = presets.child(
                Button::new(SharedString::from(format!(
                    "parameter-choice:{}",
                    choice.value
                )))
                .small()
                .label(choice.label.map_or_else(|| choice.value.to_owned(), tr))
                .on_click(move |_, window, cx| {
                    input.update(cx, |input, cx| input.set_value(choice.value, window, cx));
                }),
            );
        }
        editor = editor
            .child(
                div()
                    .text_xs()
                    .child(tr("commands.parameter.preset_values")),
            )
            .child(
                div()
                    // Kit clears height on its tracked content, but retains max-height.
                    // Bound only the viewport so all preset rows contribute to scrolling.
                    .h(px(96.))
                    .flex_shrink_0()
                    .child(presets)
                    .overflow_y_scrollbar()
                    .id("parameter-presets"),
            );
    }
    editor
        .child(div().text_xs().child(tr("commands.parameter.custom_value")))
        .child(Input::new(parameter))
        .into_any_element()
}

pub(super) fn same_serial_endpoint(
    left: Option<&SerialEndpoint>,
    right: Option<&SerialEndpoint>,
) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => {
            left.port_name == right.port_name
                && left.serial_number == right.serial_number
                && left.vid == right.vid
                && left.pid == right.pid
                && left.same_usb_interface(right)
        }
        (None, None) => true,
        _ => false,
    }
}

fn validate_tracker_focus(focus: &CommandFocus, tracker: &TrackerLink) -> Result<(), String> {
    match focus {
        CommandFocus::Tracker {
            receiver,
            slot,
            address: Some(address),
        } if !address.is_empty()
            && receiver == &tracker.receiver_key
            && *slot == tracker.slot
            && tracker.address.as_deref() == Some(address.as_str()) =>
        {
            Ok(())
        }
        CommandFocus::Tracker { address: None, .. } => {
            Err(tr("commands.route.error.address_unknown"))
        }
        _ => Err(tr("commands.route.error.tracker_changed")),
    }
}

// Passive HID silence only gates explicit wireless targets, never live direct serial or broadcast.
fn tracker_route_with_traffic<'a>(
    tracker: &TrackerLink,
    route: RouteEvidence<'a>,
) -> Result<RouteEvidence<'a>, String> {
    if matches!(route, RouteEvidence::Forwarded { .. }) && !tracker.online {
        return Err(tr("commands.route.error.no_recent_data"));
    }
    Ok(route)
}

fn route_parts<'a>(evidence: &'a RouteEvidence<'_>) -> (&'a str, u64, DeviceRole) {
    match evidence {
        RouteEvidence::Direct {
            port,
            generation,
            role,
            ..
        } => (port, *generation, *role),
        RouteEvidence::Forwarded {
            port, generation, ..
        }
        | RouteEvidence::Broadcast {
            port, generation, ..
        } => (port, *generation, DeviceRole::Tracker),
    }
}

fn validate_command_pin(
    pin: &CommandPin,
    current_focus: Option<&CommandFocus>,
    evidence: &RouteEvidence<'_>,
) -> Result<(), String> {
    if current_focus != Some(&pin.focus) {
        return Err(tr("commands.route.error.shortcut_target_changed"));
    }
    let (port, generation, role) = route_parts(evidence);
    if port != pin.endpoint.port_name
        || generation != pin.generation
        || role != pin.role
        || matches!(evidence, RouteEvidence::Forwarded { .. }) != pin.forwarded
    {
        return Err(tr("commands.route.error.shortcut_connection_changed"));
    }
    Ok(())
}
