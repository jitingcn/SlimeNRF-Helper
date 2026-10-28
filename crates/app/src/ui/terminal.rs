#[cfg(test)]
mod tests;

use super::{
    FeaturePage, Workbench,
    command_panel::{CommandConnection, CommandFocus, CommandScope, same_serial_endpoint},
    components::{
        CONNECTION_ACTION_WIDTH, clipped, command_button_tone, connection_action,
        connection_status, state_label,
    },
};
use crate::{
    commands,
    domain::{Inventory, SerialEndpoint, console_port_allowed},
    i18n::tr,
    sessions::{ConnectionState, WriteReceipt},
    terminal_view::{TerminalTranscript, TranscriptState, TranscriptViewport},
};
use futures_util::{FutureExt as _, future::Shared};
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, IconName, Sizable,
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    input::{Input, InputEvent, InputState},
    tab::{Tab, TabBar},
};
use gpui_kit::{
    AnyElement, App, BackgroundExecutor, ClipboardItem, Context, Entity, FocusHandle, FontWeight,
    KeyDownEvent, MouseButton, ScrollHandle, SharedString, Subscription, Task, Window, div,
    prelude::*, px, rems,
};
use gpui_kit::{
    Focusable as _,
    base::{Scrollbar, ScrollbarAxis, TestSupportExt as _, TextSelection, TextSelectionHandle},
};
use slimenrf_terminal_core::CommandHistory;
use std::{cell::RefCell, path::PathBuf, rc::Rc, sync::Arc};

pub(super) struct TerminalSession {
    pub(super) input: Entity<InputState>,
    scroll: ScrollHandle,
    follow: bool,
    soft_wrap: bool,
    revision: Option<(u64, u64)>,
    selection_identity: Option<(u64, u64)>,
    transcript: Rc<RefCell<TranscriptState>>,
    selection: TextSelectionHandle,
    transcript_focus: FocusHandle,
    _selection_subscription: Subscription,
    pub(super) status: Option<String>,
    _subscription: Subscription,
    pub(super) pending: Option<TerminalConnect>,
    history: CommandHistory,
    export: TerminalExport,
    export_status: Option<Result<std::path::PathBuf, String>>,
}

#[derive(Default)]
struct TerminalExport {
    task: Option<Task<()>>,
    io_join: Option<Shared<Task<()>>>,
}

type TerminalExportOutput = Arc<parking_lot::Mutex<Option<Result<PathBuf, String>>>>;

impl TerminalExport {
    fn busy(&self) -> bool {
        self.task.is_some() || self.io_join.is_some()
    }

    fn write(
        &mut self,
        path: PathBuf,
        snapshot: String,
        executor: &BackgroundExecutor,
    ) -> (Shared<Task<()>>, TerminalExportOutput) {
        let output = Arc::new(parking_lot::Mutex::new(None));
        let worker_output = output.clone();
        let join = executor
            .spawn(async move {
                let result = match crate::file_export::write(&path, snapshot.as_bytes()) {
                    Ok(()) => Ok(path),
                    Err(error) => Err(rust_i18n::t!(
                        "serial.terminal.export.error.write",
                        path = path.display().to_string(),
                        error = error
                    )
                    .to_string()),
                };
                *worker_output.lock() = Some(result);
            })
            .shared();
        self.io_join = Some(join.clone());
        (join, output)
    }

    fn shutdown(&mut self) -> Option<Shared<Task<()>>> {
        let join = self.io_join.take();
        self.task = None;
        join
    }
}

#[derive(Default)]
pub(super) struct TabSelection {
    pub(super) active: Option<String>,
}

impl TabSelection {
    fn sync<'a>(&mut self, ports: impl IntoIterator<Item = &'a str>) {
        let mut first = None;
        let mut retained = false;
        for port in ports {
            if first.is_none() {
                first = Some(port);
            }
            retained |= self.active.as_deref() == Some(port);
        }
        if !retained {
            self.active = first.map(str::to_owned);
        }
    }

    pub(super) fn select(&mut self, port: &str) {
        self.active = Some(port.to_owned());
    }
}

pub(super) struct PendingWrite {
    port: String,
    generation: u64,
    receipt: WriteReceipt,
}

pub(super) struct TerminalConnect {
    connection: CommandConnection,
    endpoint: Option<SerialEndpoint>,
    command: String,
    pub(super) error: Option<String>,
}

impl TerminalConnect {
    fn validate_endpoint(
        &self,
        current: Option<&SerialEndpoint>,
        inventory: &Inventory,
    ) -> Result<(), String> {
        if inventory.serial_enumeration_failed
            || !same_serial_endpoint(self.endpoint.as_ref(), current)
            || !current.is_some_and(|endpoint| console_port_allowed(endpoint, inventory))
        {
            return Err(tr("serial.terminal.error.device_changed"));
        }
        Ok(())
    }
}

fn cancel_terminal_command(pending: &mut Option<TerminalConnect>, status: &mut Option<String>) {
    if pending.take().is_some() {
        *status = None;
    }
}

fn consume_terminal_input_event(
    event: &InputEvent,
    pending: &mut Option<TerminalConnect>,
    status: &mut Option<String>,
) {
    if matches!(event, InputEvent::Change) {
        cancel_terminal_command(pending, status);
    }
}

fn take_terminal_command(
    pending: &mut Option<TerminalConnect>,
    status: &mut Option<String>,
    input: &str,
) -> Option<TerminalConnect> {
    let command = pending.take()?;
    if input != command.command {
        *status = None;
        return None;
    }
    Some(command)
}

impl Workbench {
    pub(super) fn sync_tabs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for session in self.manager.sessions() {
            if self.terminals.contains_key(&session.port) {
                continue;
            }
            let port = session.port.clone();
            let input = cx.new(|cx| {
                InputState::new(window, cx).placeholder(crate::i18n::tr(
                    "serial.terminal.command_placeholder.single",
                ))
            });
            let subscription =
                cx.subscribe_in(&input, window, move |this, _, event, window, cx| {
                    if let Some(terminal) = this.terminals.get_mut(&port) {
                        consume_terminal_input_event(
                            event,
                            &mut terminal.pending,
                            &mut terminal.status,
                        );
                    }
                    if matches!(event, InputEvent::Change) {
                        cx.notify();
                    }
                    if matches!(event, InputEvent::PressEnter { .. }) {
                        this.send(&port, window, cx);
                    }
                });
            let selection = TextSelectionHandle::new(String::new(), cx);
            let transcript = Rc::new(RefCell::new(TranscriptState::default()));
            let selection_state = transcript.clone();
            let selection_handle = selection.clone();
            let selection_window = window.window_handle();
            let selection_subscription = selection.subscribe(
                move |_, cx| {
                    selection_state.borrow_mut().project(&selection_handle, cx);
                    let released = selection_handle
                        .snapshot(cx)
                        .filter(|snapshot| !snapshot.is_selecting());
                    let handle = selection_handle.clone();
                    let _ = selection_window.update(cx, move |_, window, cx| {
                        if let Some(snapshot) = released {
                            // Keep the byte selection, not Root's geometric projection:
                            // its copy cache would duplicate selected text every redraw.
                            window.defer(cx, move |window, cx| {
                                if handle.snapshot(cx) == Some(snapshot) {
                                    TextSelection::clear(window, cx);
                                }
                            });
                        }
                        window.refresh();
                    });
                },
                cx,
            );
            let transcript_focus = cx.focus_handle().tab_stop(true);
            let focus = transcript_focus.clone();
            let view = cx.weak_entity();
            let selection_port = session.port.clone();
            selection.focus_with(
                move |window, cx| {
                    focus.focus(window, cx);
                    let _ = view.update(cx, |this, cx| {
                        if let Some(terminal) = this.terminals.get_mut(&selection_port) {
                            terminal.follow = false;
                        }
                        cx.notify();
                    });
                },
                cx,
            );
            self.terminals.insert(
                session.port.clone(),
                TerminalSession {
                    input,
                    scroll: ScrollHandle::default(),
                    follow: true,
                    soft_wrap: true,
                    revision: None,
                    selection_identity: None,
                    transcript,
                    selection,
                    transcript_focus,
                    _selection_subscription: selection_subscription,
                    status: None,
                    _subscription: subscription,
                    pending: None,
                    history: CommandHistory::default(),
                    export: TerminalExport::default(),
                    export_status: None,
                },
            );
        }
        let dismissed: Vec<_> = self
            .manager
            .sessions()
            .filter(|session| {
                self.manager.is_close_requested(&session.port)
                    && self.can_dismiss_terminal(&session.port)
            })
            .map(|session| session.port.clone())
            .collect();
        for port in dismissed {
            self.dismiss_terminal(&port, window, cx);
        }
        self.tabs
            .sync(self.manager.sessions().map(|session| session.port.as_str()));
        if self.command_focus.is_none()
            && self.sidebar.pending.is_none()
            && self.sidebar.scope == CommandScope::Current
            && let Some(port) = self.tabs.active.clone()
        {
            self.focus_serial(&port);
        }
        for (port, terminal) in &self.terminals {
            if self.tabs.active.as_deref() != Some(port.as_str())
                || self.active_page == FeaturePage::Settings
            {
                terminal.transcript.borrow_mut().clear_selection();
            }
        }
    }

    fn terminal_owners_idle(&self, port: &str) -> bool {
        !self.flash.serial_dfu_owns_preparation(port)
            && self.sidebar.pending.is_none()
            && !self.custom_command_preparing()
            && self.custom_command_review().is_none()
            && !self.saved_command_runs().any(|run| run.pending())
            && !self.terminal_command_runs().any(|run| {
                run.pending()
                    && run
                        .targets()
                        .iter()
                        .any(|target| crate::serial_port::same_port_name(&target.port, port))
            })
            && !self.batch_history.iter().any(|run| run.pending())
            && !self
                .custom_commands
                .pending_history
                .iter()
                .any(|(pending, _)| crate::serial_port::same_port_name(pending, port))
            && !self
                .custom_commands
                .terminal_clear
                .as_ref()
                .is_some_and(|(pending, _)| crate::serial_port::same_port_name(pending, port))
            && self
                .terminals
                .get(port)
                .is_some_and(|terminal| terminal.pending.is_none() && !terminal.export.busy())
            && !self
                .pending_writes
                .iter()
                .any(|pending| crate::serial_port::same_port_name(&pending.port, port))
            && self.flash_check_port(port).is_ok()
    }

    pub(in crate::ui) fn can_request_terminal_close(&self, port: &str) -> bool {
        !self.manager.is_close_requested(port)
            && self.manager.can_request_close(port)
            && self.terminal_owners_idle(port)
    }

    pub(in crate::ui) fn request_terminal_close(
        &mut self,
        port: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.can_request_terminal_close(port) {
            return;
        }
        if self.can_dismiss_terminal(port) {
            self.dismiss_terminal(port, window, cx);
            return;
        }
        if let Err(error) = self.manager.request_close(port) {
            self.error = Some(format!("{port}: {error}"));
        }
        cx.notify();
    }

    pub(in crate::ui) fn can_dismiss_terminal(&self, port: &str) -> bool {
        self.manager.can_forget(port) && self.terminal_owners_idle(port)
    }

    pub(in crate::ui) fn dismiss_terminal(
        &mut self,
        port: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.can_dismiss_terminal(port) || !self.manager.forget(port) {
            return;
        }
        let terminal = self
            .terminals
            .remove(port)
            .expect("dismissed terminal exists");
        if self.tabs.active.as_deref() == Some(port) && terminal.selection.snapshot(cx).is_some() {
            TextSelection::clear(window, cx);
        }
        if terminal.transcript_focus.contains_focused(window, cx)
            || terminal
                .input
                .read(cx)
                .focus_handle(cx)
                .contains_focused(window, cx)
        {
            window.blur(cx);
        }
        self.tabs
            .sync(self.manager.sessions().map(|session| session.port.as_str()));
        if matches!(
            &self.command_focus,
            Some(CommandFocus::Serial(focus)) if crate::serial_port::same_port_name(focus, port)
        ) {
            self.command_focus = self.tabs.active.clone().map(CommandFocus::Serial);
            self.command_error = None;
        }
        cx.notify();
    }

    pub(super) fn toggle_connection(&mut self, port: &str, cx: &mut Context<Self>) {
        self.poll_sessions();
        if self.manager.is_close_requested(port) {
            return;
        }
        if let Err(error) = self.flash_check_port(port) {
            self.error = Some(format!("{port}: {error}"));
            cx.notify();
            return;
        }
        let state = self.manager.session(port).map(|session| session.state);
        let reconnecting = self.manager.is_reconnecting(port);
        let connecting = !reconnecting
            && !matches!(
                state,
                Some(
                    ConnectionState::Connected
                        | ConnectionState::Connecting
                        | ConnectionState::Closing
                )
            );
        let result = if reconnecting {
            self.manager.disconnect(port)
        } else {
            match state {
                Some(ConnectionState::Connected | ConnectionState::Connecting) => {
                    self.manager.disconnect(port)
                }
                Some(ConnectionState::Closing) => return,
                _ => self
                    .console_check_port(port)
                    .and_then(|()| self.manager.connect(port)),
            }
        };
        self.error = result.err().map(|error| format!("{port}: {error}"));
        if connecting && self.error.is_none() {
            self.tabs.select(port);
            self.focus_serial(port);
        }
        cx.notify();
    }

    pub(super) fn complete_custom_terminal_clear(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let pending = std::mem::take(&mut self.custom_commands.pending_history);
        for (port, command) in pending {
            if let Some(terminal) = self.terminals.get_mut(&port) {
                terminal
                    .history
                    .record_submission(&command, &Ok::<(), ()>(()));
            } else {
                self.custom_commands.pending_history.push((port, command));
            }
        }
        let Some((port, original)) = self.custom_commands.terminal_clear.take() else {
            return;
        };
        if let Some(terminal) = self.terminals.get_mut(&port)
            && terminal.input.read(cx).value().as_ref() == original
        {
            terminal
                .input
                .update(cx, |input, cx| input.set_value("", window, cx));
        }
    }

    fn send_terminal_all(&mut self, port: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.manager.is_close_requested(port) {
            return;
        }
        let Some(terminal) = self.terminals.get(port) else {
            return;
        };
        let text = terminal.input.read(cx).value().to_string();
        self.cancel_terminal_intents();
        let result = self.submit_terminal_send_all(port, text, cx);
        if let Some(terminal) = self.terminals.get_mut(port) {
            terminal.status = result.err();
        }
        self.complete_custom_terminal_clear(window, cx);
        cx.notify();
    }

    fn queue_write(&mut self, port: &str, command: &str) -> bool {
        if self.manager.is_close_requested(port) {
            return false;
        }
        if self.pending_writes.len() >= 128 {
            if let Some(terminal) = self.terminals.get_mut(port) {
                terminal.status = Some(crate::i18n::tr("serial.terminal.status.sending"));
            }
            return false;
        }
        let generation = self
            .manager
            .session(port)
            .map_or(0, |session| session.generation);
        let submission = self.flash_send_line(port, command);
        if let Some(terminal) = self.terminals.get_mut(port) {
            terminal.history.record_submission(command, &submission);
        }
        match submission {
            Ok(receipt) => {
                self.pending_writes.push(PendingWrite {
                    port: port.to_owned(),
                    generation,
                    receipt,
                });
                if let Some(terminal) = self.terminals.get_mut(port) {
                    terminal.status = None;
                }
                true
            }
            Err(error) => {
                if let Some(terminal) = self.terminals.get_mut(port) {
                    terminal.status = Some(
                        rust_i18n::t!("serial.terminal.error.failed", error = error).to_string(),
                    );
                }
                false
            }
        }
    }

    fn send(&mut self, port: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.poll_sessions();
        if self.manager.is_close_requested(port) {
            return;
        }
        let Some(terminal) = self.terminals.get_mut(port) else {
            return;
        };
        let command = terminal.input.read(cx).value().to_string();
        if command.is_empty() {
            return;
        }
        let previous = terminal.pending.take();
        let mut pending = match previous {
            Some(pending) if pending.command == command => {
                if pending.error.is_none() {
                    terminal.pending = Some(pending);
                    return;
                }
                pending
            }
            _ => TerminalConnect {
                connection: CommandConnection {
                    port: port.to_owned(),
                    generation: None,
                },
                endpoint: self.port_endpoint(port).cloned(),
                command,
                error: None,
            },
        };
        pending.error = pending
            .validate_endpoint(self.port_endpoint(port), &self.inventory)
            .and_then(|()| self.console_check_port(port))
            .and_then(|()| self.flash_check_port(port))
            .and_then(|()| pending.connection.start(&mut self.manager))
            .err();
        let terminal = self.terminals.get_mut(port).expect("terminal retained");
        terminal.status = pending.error.clone().or_else(|| {
            Some(rust_i18n::t!("serial.terminal.status.connecting", port = port).to_string())
        });
        terminal.pending = Some(pending);
        self.resume_terminal_command(window, cx);
        cx.notify();
    }

    fn recall_terminal_command(
        &mut self,
        port: &str,
        older: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if window.modifiers().modified() {
            return;
        }
        let Some(terminal) = self.terminals.get_mut(port) else {
            return;
        };
        cx.stop_propagation();
        if terminal
            .pending
            .as_ref()
            .is_some_and(|pending| pending.error.is_none())
        {
            return;
        }
        let current = terminal.input.read(cx).value();
        let Some(command) = terminal.history.navigate(older, current.as_ref()) else {
            return;
        };
        // Cancel synchronously too: a later poll must not revive an old intent.
        cancel_terminal_command(&mut terminal.pending, &mut terminal.status);
        terminal.input.update(cx, |input, cx| {
            input.set_value(command, window, cx);
            // GPUIInput::set_value suppresses Change; replay must follow the
            // same cancellation path as user edits.
            cx.emit(InputEvent::Change);
        });
        cx.notify();
    }

    fn export_terminal_log(&mut self, port: &str, cx: &mut Context<Self>) {
        if self.shutting_down {
            return;
        }
        let Some(terminal) = self.terminals.get_mut(port) else {
            return;
        };
        if terminal.export.busy() {
            return;
        }
        let Some(session) = self.manager.session(port) else {
            return;
        };
        let generation = session.generation;
        let snapshot = session.terminal.text();
        let port = port.to_owned();
        let filename_port: String = port
            .chars()
            .map(|ch| {
                if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                    ch
                } else {
                    '_'
                }
            })
            .collect();
        let filename = format!("{filename_port}-terminal.txt");
        let directory = directories::BaseDirs::new()
            .map(|dirs| dirs.home_dir().to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        let dialog = cx.prompt_for_new_path(&directory, Some(&filename));
        let executor = cx.background_executor().clone();
        terminal.export.task = Some(cx.spawn(async move |this, cx| {
            let selected = match dialog.await {
                Ok(Ok(path)) => Ok(path),
                Ok(Err(error)) => Err(rust_i18n::t!(
                    "serial.terminal.export.error.dialog",
                    error = error
                )
                .to_string()),
                Err(error) => Err(rust_i18n::t!(
                    "serial.terminal.export.error.dialog",
                    error = error
                )
                .to_string()),
            };
            let write = this.update(cx, |this, cx| {
                let current = this
                    .manager
                    .session(&port)
                    .map(|session| session.generation);
                let terminal = this.terminals.get_mut(&port)?;
                if this.shutting_down || current != Some(generation) {
                    terminal.export.task = None;
                    cx.notify();
                    return None;
                }
                match selected {
                    Ok(Some(path)) => {
                        let write = terminal.export.write(path, snapshot, &executor);
                        cx.notify();
                        Some(write)
                    }
                    result => {
                        terminal.export.task = None;
                        if let Err(error) = result {
                            terminal.export_status = Some(Err(error));
                        }
                        cx.notify();
                        None
                    }
                }
            });
            let Ok(Some((join, output))) = write else {
                return;
            };
            join.await;
            let result = output.lock().take();
            let _ = this.update(cx, |this, cx| {
                let current = this
                    .manager
                    .session(&port)
                    .map(|session| session.generation);
                if let Some(terminal) = this.terminals.get_mut(&port) {
                    terminal.export = TerminalExport::default();
                    if current == Some(generation) && !this.shutting_down {
                        terminal.export_status = result;
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    pub(super) fn shutdown_terminal_exports(&mut self) -> Vec<Shared<Task<()>>> {
        self.terminals
            .values_mut()
            .filter_map(|terminal| terminal.export.shutdown())
            .collect()
    }

    pub(super) fn cancel_terminal_intents(&mut self) {
        for terminal in self.terminals.values_mut() {
            cancel_terminal_command(&mut terminal.pending, &mut terminal.status);
        }
    }

    pub(super) fn resume_terminal_command(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(port) = self
            .tabs
            .active
            .as_ref()
            .filter(|port| {
                self.terminals
                    .get(*port)
                    .is_some_and(|terminal| terminal.pending.is_some())
            })
            .cloned()
        else {
            return;
        };
        self.poll_sessions();
        if self.manager.is_close_requested(&port) {
            return;
        }
        let terminal = self.terminals.get_mut(&port).expect("active terminal");
        let input = terminal.input.read(cx).value();
        let Some(mut pending) =
            take_terminal_command(&mut terminal.pending, &mut terminal.status, input.as_ref())
        else {
            return;
        };
        if pending.error.is_some() {
            terminal.pending = Some(pending);
            return;
        }
        let ready = pending
            .validate_endpoint(self.port_endpoint(&port), &self.inventory)
            .and_then(|()| self.console_check_port(&port))
            .and_then(|()| self.flash_check_port(&port))
            .and_then(|()| pending.connection.ready(&self.manager));
        match ready {
            Ok(true) if self.queue_write(&port, &pending.command) => {
                let terminal = self.terminals.get_mut(&port).expect("active terminal");
                terminal.status = None;
                terminal
                    .input
                    .update(cx, |input, cx| input.set_value("", window, cx));
                return;
            }
            Ok(true) => {
                pending.error = self.terminals[&port].status.clone();
            }
            Ok(false) => {}
            Err(error) => pending.error = Some(error),
        }
        let terminal = self.terminals.get_mut(&port).expect("active terminal");
        terminal.status = pending.error.clone().or_else(|| {
            Some(rust_i18n::t!("serial.terminal.status.connecting", port = port).to_string())
        });
        terminal.pending = Some(pending);
    }

    pub(super) fn poll_writes(&mut self) -> bool {
        let mut changed = false;
        self.pending_writes.retain_mut(|pending| {
            let Some(result) = pending.receipt.poll() else {
                return true;
            };
            changed = true;
            if self
                .manager
                .session(&pending.port)
                .is_some_and(|session| session.generation == pending.generation)
                && let Some(terminal) = self.terminals.get_mut(&pending.port)
            {
                terminal.status = result.err().map(|error| {
                    rust_i18n::t!("serial.terminal.error.failed", error = error).to_string()
                });
            }
            false
        });
        for run in &mut self.batch_history {
            changed |= run.poll();
        }
        changed
    }

    // Synchronize changed rows only for the visible transcript. Scrolling reuses
    // the retained rows instead of joining the entire terminal history.
    fn refresh_transcript(&mut self, port: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = self.manager.session(port) else {
            return;
        };
        let terminal = self.terminals.get_mut(port).expect("terminal tab exists");
        let revision = (session.generation, session.terminal.revision());
        if terminal.revision == Some(revision) {
            return;
        }
        let identity = (session.generation, session.terminal.selection_epoch());
        if terminal.selection_identity != Some(identity) {
            terminal.transcript.borrow_mut().clear_selection();
            if terminal.selection.snapshot(cx).is_some() {
                TextSelection::clear(window, cx);
            }
        }
        terminal.selection_identity = Some(identity);
        terminal
            .transcript
            .borrow_mut()
            .sync_buffer(&session.terminal, session.generation);
        terminal.revision = Some(revision);
        if terminal.follow {
            terminal.scroll.scroll_to_bottom();
        }
    }

    fn copy_terminal_selection(&self, port: &str, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(terminal) = self.terminals.get(port) else {
            return;
        };
        let Some(session) = self.manager.session(port) else {
            return;
        };
        if self.tabs.active.as_deref() != Some(port)
            || terminal.selection_identity
                != Some((session.generation, session.terminal.selection_epoch()))
        {
            return;
        }
        if let Some(text) = terminal.transcript.borrow().selected_text() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    pub(super) fn workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let mut workspace = div()
            .flex()
            .flex_col()
            .size_full()
            .min_w_0()
            .min_h_0()
            .bg(cx.theme().group_box)
            .border_1()
            .border_color(cx.theme().border)
            .rounded_md()
            .overflow_hidden();
        let mut tabs = TabBar::new("terminal-tabs").menu(true).max_width(px(260.));
        let mut tab_ports = Vec::new();
        for (index, session) in self.manager.sessions().enumerate() {
            tab_ports.push(session.port.clone());
            let label = terminal_session_label(
                &session.port,
                Some(self.serial_name(&session.port).as_str()),
                &self.inventory,
            );
            if self.tabs.active.as_ref() == Some(&session.port) {
                tabs = tabs.selected_index(index);
            }
            let closing = self.manager.is_close_requested(&session.port);
            let can_close = self.can_request_terminal_close(&session.port);
            let close_port = session.port.clone();
            let close_group = SharedString::from(format!("terminal-close:{close_port}"));
            tabs = tabs.child(
                Tab::new()
                    .group(close_group.clone())
                    .aria_label(format!("{label} · {}", state_label(session.state)))
                    .label(label)
                    .suffix(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(div().size(px(5.)).flex_shrink_0().rounded_full().bg(
                                match session.state {
                                    ConnectionState::Connected => cx.theme().success,
                                    ConnectionState::Connecting => cx.theme().warning,
                                    ConnectionState::Faulted => cx.theme().danger,
                                    ConnectionState::Closing | ConnectionState::Disconnected => {
                                        cx.theme().muted_foreground
                                    }
                                },
                            ))
                            .child(
                                Button::new(close_group.clone())
                                    .small()
                                    .ghost()
                                    .opacity(0.)
                                    .group_hover(close_group, |style| style.opacity(1.))
                                    .focus(|style| style.opacity(1.))
                                    .icon(IconName::Close)
                                    .accessibility_label(tr("serial.terminal.action.close_tab"))
                                    .tooltip(tr(if closing {
                                        "serial.terminal.close.closing"
                                    } else if can_close {
                                        "serial.terminal.action.close_tab"
                                    } else {
                                        "serial.terminal.close.busy"
                                    }))
                                    .disabled(!can_close)
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.request_terminal_close(&close_port, window, cx);
                                    })),
                            ),
                    ),
            );
        }
        tabs = tabs.on_click(cx.listener(move |this, index: &usize, _, cx| {
            if let Some(port) = tab_ports.get(*index) {
                this.focus_serial(port);
                cx.notify();
            }
        }));
        if let Some(port) = self.tabs.active.clone() {
            workspace = workspace
                .child(tabs)
                .child(self.terminal(&port, window, cx));
        } else {
            workspace = workspace
                .child(
                    div()
                        .px_3()
                        .py_2()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(tr("serial.terminal.title")),
                )
                .child(div().flex_1().min_w_0().child(terminal_empty_state(
                    "serial.terminal.empty.no_connection",
                    "serial.terminal.empty.connect_hint",
                    cx,
                )));
        }
        workspace.into_any_element()
    }

    fn terminal(&mut self, port: &str, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        self.refresh_transcript(port, window, cx);
        let terminal = &self.terminals[port];
        let session = self
            .manager
            .session(port)
            .expect("selected tab has a session");
        let reconnecting = self.manager.is_reconnecting(port);
        let closing = self.manager.is_close_requested(port);
        let transcript_empty = terminal.transcript.borrow().is_empty();
        let waiting = terminal
            .pending
            .as_ref()
            .is_some_and(|pending| pending.error.is_none());
        let transcript_port = port.to_owned();
        let gesture_port = port.to_owned();
        let keyboard_port = port.to_owned();
        let reflow_port = port.to_owned();
        let reflow_view = cx.weak_entity();
        let reflow_state = Rc::downgrade(&terminal.transcript);
        let on_reflow = Rc::new(move |_: &mut Window, cx: &mut App| {
            let Some(state) = reflow_state.upgrade() else {
                return;
            };
            let _ = reflow_view.update(cx, |this, cx| {
                let Some(terminal) = this.terminals.get(&reflow_port) else {
                    return;
                };
                let visible = !this.shutting_down
                    && this.active_page == FeaturePage::DeviceSetup
                    && this.tabs.active.as_deref() == Some(reflow_port.as_str())
                    && Rc::ptr_eq(&state, &terminal.transcript)
                    && this.manager.session(&reflow_port).is_some_and(|session| {
                        terminal.revision == Some((session.generation, session.terminal.revision()))
                    });
                if state.borrow_mut().settle_frame(
                    &terminal.scroll,
                    visible,
                    terminal.soft_wrap,
                    terminal.follow,
                ) {
                    cx.notify();
                }
            });
        });
        let transcript = div()
            .id(SharedString::from(format!("transcript-scroll:{port}")))
            .role(gpui_kit::Role::Document)
            .aria_label("Terminal transcript")
            .size_full()
            .when(terminal.soft_wrap, |view| {
                view.overflow_y_scroll().overflow_x_hidden()
            })
            .when(!terminal.soft_wrap, |view| view.overflow_scroll())
            .px_2()
            .text_xs()
            .line_height(rems(1.25))
            .font_family(cx.theme().mono_font_family.clone())
            .whitespace_nowrap()
            .track_scroll(&terminal.scroll)
            .test_support()
            .track_focus(&terminal.transcript_focus)
            .key_context("TerminalTranscript")
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                if let Some(terminal) = this.terminals.get_mut(&keyboard_port)
                    && terminal.transcript_focus.is_focused(window)
                    && crate::terminal_view::handle_key_down(
                        &terminal.transcript,
                        event,
                        &terminal.scroll,
                        window,
                        cx,
                    )
                {
                    terminal.follow = false;
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    if let Some(terminal) = this.terminals.get_mut(&gesture_port) {
                        terminal.transcript.borrow_mut().clear_selection();
                        terminal.follow = false;
                        terminal.transcript_focus.focus(window, cx);
                    }
                    cx.notify();
                }),
            )
            .capture_action(cx.listener(
                move |this, _: &gpui_kit::component::input::Copy, window, cx| {
                    this.copy_terminal_selection(&transcript_port, window, cx);
                    cx.stop_propagation();
                },
            ))
            .child(TerminalTranscript::new(
                SharedString::from(format!("transcript:{port}")),
                terminal.selection.clone(),
                terminal.transcript.clone(),
                gpui_kit::base::Theme::global(cx).tokens.colors.selection,
                cx.theme().background,
                TranscriptViewport {
                    soft_wrap: terminal.soft_wrap,
                    follow: terminal.follow,
                    scroll: terminal.scroll.clone(),
                    focus: terminal.transcript_focus.clone(),
                    on_reflow,
                },
            ));
        let connect_port = port.to_owned();
        let send_port = port.to_owned();
        let send_all_port = port.to_owned();
        let cancel_port = port.to_owned();
        let clear_port = port.to_owned();
        let copy_port = port.to_owned();
        let copy_selection_port = port.to_owned();
        let follow_port = port.to_owned();
        let wrap_port = port.to_owned();
        let older_history_port = port.to_owned();
        let newer_history_port = port.to_owned();
        let export_port = port.to_owned();
        let title = self.serial_name(port);
        let mut panel = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .gap_2()
            .p_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .min_w_0()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(clipped("terminal-title".into(), title)),
                            )
                            .when_some(session.identity.address.as_ref(), |column, address| {
                                column.child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(clipped("terminal-address".into(), address.clone())),
                                )
                            }),
                    )
                    .child(div().text_xs().flex_shrink_0().child(connection_status(
                        Some(session.state),
                        reconnecting,
                        cx,
                    )))
                    .child(
                        Button::new("terminal-connection")
                            .small()
                            .w(px(CONNECTION_ACTION_WIDTH))
                            .label(connection_action(Some(session.state), reconnecting))
                            .when(
                                !reconnecting
                                    && matches!(
                                        session.state,
                                        ConnectionState::Disconnected | ConnectionState::Faulted
                                    ),
                                |button| {
                                    button
                                        .primary()
                                        .when(!cx.theme().is_dark(), |button| button.outline())
                                },
                            )
                            .disabled(
                                closing
                                    || (!reconnecting && session.state == ConnectionState::Closing),
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.toggle_connection(&connect_port, cx)
                            })),
                    ),
            )
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .rounded_md()
                    .border_1()
                    .border_color(if terminal.transcript_focus.is_focused(window) {
                        cx.theme().ring
                    } else {
                        cx.theme().border
                    })
                    .bg(cx.theme().background)
                    .child(if transcript_empty {
                        terminal_empty_state(
                            "serial.terminal.empty.no_output",
                            if session.state == ConnectionState::Connected {
                                "serial.terminal.empty.send_hint"
                            } else {
                                "serial.terminal.empty.reconnect_hint"
                            },
                            cx,
                        )
                    } else {
                        transcript.into_any_element()
                    })
                    .child(
                        div().absolute().inset_0().child(
                            Scrollbar::new(&terminal.scroll)
                                .axis(if terminal.soft_wrap {
                                    ScrollbarAxis::Vertical
                                } else {
                                    ScrollbarAxis::Both
                                })
                                .viewport_from_layout(),
                        ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .min_w_0()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            // Input keybindings dispatch actions before raw key events.
                            // Capture the existing movement actions before Input handles them.
                            .capture_action(cx.listener(
                                move |this, _: &gpui_kit::component::input::MoveUp, window, cx| {
                                    this.recall_terminal_command(
                                        &older_history_port,
                                        true,
                                        window,
                                        cx,
                                    );
                                },
                            ))
                            .capture_action(cx.listener(
                                move |this,
                                      _: &gpui_kit::component::input::MoveDown,
                                      window,
                                      cx| {
                                    this.recall_terminal_command(
                                        &newer_history_port,
                                        false,
                                        window,
                                        cx,
                                    );
                                },
                            ))
                            .child(Input::new(&terminal.input)),
                    )
                    .child(
                        Button::new("terminal-send")
                            .small()
                            .w(px(88.))
                            .flex_shrink_0()
                            .primary()
                            .when(!cx.theme().is_dark(), |button| button.outline())
                            .map(|button| {
                                command_button_tone(
                                    button,
                                    commands::command_tone(
                                        terminal.input.read(cx).value().as_ref(),
                                    ),
                                    cx,
                                )
                            })
                            .label(
                                if terminal
                                    .pending
                                    .as_ref()
                                    .is_some_and(|pending| pending.error.is_some())
                                {
                                    crate::i18n::tr("serial.terminal.action.retry")
                                } else {
                                    crate::i18n::tr("serial.terminal.action.send")
                                },
                            )
                            .disabled(closing || waiting)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.send(&send_port, window, cx)
                            })),
                    )
                    .child(
                        Button::new("terminal-send-all")
                            .small()
                            .w(px(88.))
                            .flex_shrink_0()
                            .label(tr("serial.terminal.action.send_all"))
                            .disabled(
                                closing
                                    || waiting
                                    || terminal.input.read(cx).value().trim().is_empty(),
                            )
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.send_terminal_all(&send_all_port, window, cx);
                            })),
                    )
                    .when(terminal.pending.is_some(), |row| {
                        row.child(
                            Button::new("terminal-cancel")
                                .small()
                                .label(tr("common.action.cancel"))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if let Some(terminal) = this.terminals.get_mut(&cancel_port) {
                                        cancel_terminal_command(
                                            &mut terminal.pending,
                                            &mut terminal.status,
                                        );
                                    }
                                    cx.notify();
                                })),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .child(
                        Button::new("terminal-clear")
                            .small()
                            .ghost()
                            .label(crate::i18n::tr("serial.terminal.action.clear"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                TextSelection::clear(window, cx);
                                this.manager.clear(&clear_port);
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("terminal-copy-selection")
                            .small()
                            .ghost()
                            .label(tr("serial.terminal.action.copy_selection"))
                            .disabled(!terminal.transcript.borrow().has_selection())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.copy_terminal_selection(&copy_selection_port, window, cx);
                            })),
                    )
                    .child(
                        Button::new("terminal-copy")
                            .small()
                            .ghost()
                            .label(crate::i18n::tr("serial.terminal.action.copy_all"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(session) = this.manager.session(&copy_port) {
                                    cx.write_to_clipboard(ClipboardItem::new_string(
                                        session.terminal.text(),
                                    ));
                                }
                            })),
                    )
                    .child(
                        Button::new("terminal-export")
                            .small()
                            .ghost()
                            .label(crate::i18n::tr("serial.terminal.action.export_log"))
                            .tooltip(crate::i18n::tr("serial.terminal.export.hint"))
                            .disabled(terminal.export.busy())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.export_terminal_log(&export_port, cx);
                            })),
                    )
                    .child(
                        Checkbox::new("terminal-follow")
                            .label(crate::i18n::tr("serial.terminal.action.follow"))
                            .checked(terminal.follow)
                            .on_click(cx.listener(move |this, checked, _, cx| {
                                if let Some(terminal) = this.terminals.get_mut(&follow_port) {
                                    terminal.follow = *checked;
                                    if *checked {
                                        terminal.scroll.scroll_to_bottom();
                                    }
                                }
                                cx.notify();
                            })),
                    )
                    .child(
                        Checkbox::new("terminal-wrap")
                            .small()
                            .label(tr("serial.terminal.action.wrap_lines"))
                            .checked(terminal.soft_wrap)
                            .on_click(cx.listener(move |this, checked, _, cx| {
                                if let Some(terminal) = this.terminals.get_mut(&wrap_port) {
                                    terminal.soft_wrap = *checked;
                                    let mut offset = terminal.scroll.offset();
                                    offset.x = px(0.);
                                    terminal.scroll.set_offset(offset);
                                }
                                cx.notify();
                            })),
                    ),
            );
        if let Some(status) = session.last_error.as_ref().or(terminal.status.as_ref()) {
            panel = panel.child(
                div()
                    .text_xs()
                    .text_color(
                        if session.last_error.is_some()
                            || terminal
                                .pending
                                .as_ref()
                                .is_some_and(|pending| pending.error.is_some())
                        {
                            cx.theme().danger
                        } else {
                            cx.theme().muted_foreground
                        },
                    )
                    .child(clipped("terminal-status".into(), status.clone())),
            );
        }
        if let Some(result) = terminal.export_status.as_ref() {
            let message = match result {
                Ok(path) => rust_i18n::t!(
                    "serial.terminal.export.success",
                    path = path.display().to_string()
                )
                .to_string(),
                Err(error) => error.clone(),
            };
            panel = panel.child(
                div()
                    .text_xs()
                    .text_color(if result.is_err() {
                        cx.theme().danger
                    } else {
                        cx.theme().muted_foreground
                    })
                    .child(clipped("terminal-export-status".into(), message)),
            );
        }
        panel.into_any_element()
    }
}

// Port first: tab/header ellipsis may hide a long description, never the distinguishing port.
// TabBar retains the complete label in its overflow menu; the header has a full tooltip.
fn terminal_session_label(
    port: &str,
    reported_name: Option<&str>,
    inventory: &Inventory,
) -> String {
    if let Some(name) = reported_name
        .map(str::trim)
        .filter(|name| !name.is_empty() && *name != port)
    {
        return format!("{port} — {name}");
    }
    // Inventory labels already contain the port and USB product description.
    let label = inventory
        .ports
        .iter()
        .find(|endpoint| endpoint.port_name == port)
        .map(|endpoint| endpoint.label.trim())
        .filter(|label| !label.is_empty())
        .unwrap_or(port);
    label.to_owned()
}

pub(super) fn serial_display_name<'a>(port: &str, label: &'a str) -> &'a str {
    let label = label.trim();
    if label == port {
        return "";
    }
    let label = label
        .strip_prefix(port)
        .and_then(|rest| rest.strip_prefix(" — "))
        .unwrap_or(label);
    label
        .strip_suffix(')')
        .and_then(|rest| rest.strip_suffix(port))
        .and_then(|rest| rest.strip_suffix('('))
        .map(str::trim_end)
        .filter(|description| !description.is_empty())
        .unwrap_or(label)
}

fn terminal_empty_state(title: &str, instruction: &str, cx: &App) -> AnyElement {
    div()
        .size_full()
        .min_w_0()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_2()
        .p_4()
        .child(Icon::new(IconName::SquareTerminal).text_color(cx.theme().muted_foreground))
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .child(tr(title)),
        )
        .child(
            div()
                .text_sm()
                .text_center()
                .whitespace_normal()
                .text_color(cx.theme().muted_foreground)
                .child(tr(instruction)),
        )
        .into_any_element()
}

#[cfg(test)]
mod terminal_label_tests {
    use super::{serial_display_name, terminal_session_label};
    use crate::domain::{Inventory, SerialEndpoint};

    fn inventory() -> Inventory {
        Inventory {
            ports: vec![SerialEndpoint {
                port_name: "COM30".into(),
                label: "COM30 — USB receiver".into(),
                manufacturer: None,
                product: None,
                kind: crate::domain::SerialPortKind::Usb,
                serial_number: Some("not-a-device-name".into()),
                vid: None,
                pid: None,
                usb_interface: None,
            }],
            ..Inventory::default()
        }
    }

    #[test]
    fn roster_name_omits_matching_trailing_port_without_losing_description() {
        for label in [
            "USB serial device (COM30)",
            "USB串行设备(COM30)",
            "COM30 — USB串行设备(COM30)",
        ] {
            let expected = if label.starts_with("USB serial") {
                "USB serial device"
            } else {
                "USB串行设备"
            };
            assert_eq!(serial_display_name("COM30", label), expected);
        }
        for label in [
            "USB serial device (COM3)",
            "USB serial device (COM300)",
            "USB (COM30) serial device",
            "USB serial device (COM30 extra)",
            "(COM30)",
            " (COM30)",
        ] {
            assert_eq!(serial_display_name("COM30", label), label.trim());
        }
    }

    #[test]
    fn roster_name_omits_only_its_own_port_prefix() {
        assert_eq!(
            serial_display_name("COM30", "COM30 — USB receiver"),
            "USB receiver"
        );
        assert_eq!(
            serial_display_name("COM30", " 受信機 Receiver "),
            "受信機 Receiver"
        );
        assert_eq!(serial_display_name("COM30", "COM30"), "");
        assert_eq!(
            serial_display_name("COM3", "COM30 — USB receiver"),
            "COM30 — USB receiver"
        );
        assert_eq!(
            serial_display_name("COM30", "COM30 Custom board"),
            "COM30 Custom board"
        );
    }

    #[test]
    fn reported_name_wins_over_inventory_description() {
        let inventory = inventory();
        let label = terminal_session_label("COM30", Some("受信機 Receiver"), &inventory);
        assert_eq!(label, "COM30 — 受信機 Receiver");
    }

    #[test]
    fn missing_name_uses_only_matching_inventory_description() {
        let inventory = inventory();
        for name in [None, Some(" \t ")] {
            let label = terminal_session_label("COM30", name, &inventory);
            assert!(label.contains(&inventory.ports[0].label));
            let unmatched = terminal_session_label("COM5", name, &inventory);
            assert!(unmatched.starts_with("COM5"));
            assert!(!unmatched.contains("USB receiver"));
            assert!(!unmatched.contains("not-a-device-name"));
        }
    }

    #[test]
    fn identical_long_names_keep_distinct_ports_at_start() {
        let name = "受信機".repeat(100);
        let inventory = Inventory::default();
        let first = terminal_session_label("COM30", Some(&name), &inventory);
        let second = terminal_session_label("COM5", Some(&name), &inventory);
        assert!(first.starts_with("COM30"));
        assert!(second.starts_with("COM5"));
        assert!(first.contains(&name));
        assert!(second.contains(&name));
        assert_ne!(first, second);
    }
}
