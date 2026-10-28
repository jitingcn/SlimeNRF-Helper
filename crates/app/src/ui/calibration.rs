use super::Workbench;
use crate::calibration::{
    Application, Axis, CalibrationSession, CalibrationTask, CalibrationWorker, Dispatch,
    GuideSelection, ObservationEnd, Persistence, Phase, ReviewTicket, Revolutions, RunMode,
    Snapshot, Target,
};
use crate::{i18n::tr, serial_port::same_port_name};
use gpui_kit::component::{
    ActiveTheme,
    input::{InputEvent, InputState},
};
use gpui_kit::{
    AnyElement, App, Context, Div, Entity, FontWeight, Subscription, Window, div, prelude::*,
};

mod render;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Guide {
    #[default]
    Stationary,
    Sensitivity,
}

struct ReviewedGuide {
    ticket: ReviewTicket,
    label: String,
}

#[derive(Clone, Copy)]
enum AfterJoin {
    Finish,
    NextAxis(Axis),
}

struct PendingAdvance {
    id: u64,
    action: AfterJoin,
    persistence: Persistence,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RunCue {
    Closing,
    Inspecting,
    Inspected,
    InspectionStopped,
    InspectionFailed,
    NotStarted,
    Preparing,
    HoldStill,
    Retrimming,
    Collecting,
    Rotate {
        axis: Axis,
        revolutions: Revolutions,
    },
    Validating,
    ReadingBack,
    Observing,
    Result(Application),
}

#[derive(Default)]
pub(super) struct CalibrationUiState {
    pub(super) session: CalibrationSession,
    guide: Guide,
    axis: Option<Axis>,
    revolutions: Option<Entity<InputState>>,
    count_subscription: Option<Subscription>,
    selection: Option<GuideSelection>,
    review: Option<ReviewedGuide>,
    after_join: Option<PendingAdvance>,
    dismissed: Option<u64>,
    details: bool,
    error: Option<String>,
}

impl CalibrationUiState {
    fn invalidate_review(&mut self) {
        self.review = None;
        self.error = None;
    }

    fn accept_joined_selection(&mut self, snapshot: &Snapshot) -> bool {
        if !snapshot.joined {
            return false;
        }
        let Target::Direct {
            endpoint: observed, ..
        } = &snapshot.target
        else {
            return false;
        };
        let Some(selection) = &mut self.selection else {
            return false;
        };
        let Target::Direct {
            endpoint,
            generation,
            address,
        } = &mut selection.target
        else {
            return false;
        };
        if !same_serial_endpoint(observed, endpoint) {
            return false;
        }
        let mut changed = generation.take().is_some();
        if let Some(identity) = &snapshot.identity
            && address.as_ref() != Some(identity)
        {
            *address = Some(identity.clone());
            changed = true;
        }
        changed
    }

    fn take_joined_action(&mut self, snapshot: &Snapshot) -> Option<Result<AfterJoin, String>> {
        if snapshot.joined
            && self
                .after_join
                .as_ref()
                .is_some_and(|pending| pending.id == snapshot.id)
        {
            self.after_join.take().map(|pending| {
                if can_advance(snapshot) && snapshot.persistence == pending.persistence {
                    Ok(pending.action)
                } else {
                    Err(tr("calibration.error.evidence_changed"))
                }
            })
        } else {
            None
        }
    }
}

impl Workbench {
    pub(super) fn calibration_check_port(&self, port: &str) -> Result<(), String> {
        self.calibration
            .session
            .observe_aliases(&self.inventory, &self.manager, &self.monitor);
        if self.calibration.session.blocks_port(
            port,
            self.manager.session(port).map(|session| &session.identity),
            self.inventory
                .ports
                .iter()
                .find(|endpoint| same_port_name(&endpoint.port_name, port)),
        ) {
            Err(tr("calibration.error.busy"))
        } else {
            Ok(())
        }
    }

    pub(super) fn calibration_check_receiver(&self, key: &str) -> Result<(), String> {
        self.calibration
            .session
            .observe_aliases(&self.inventory, &self.manager, &self.monitor);
        if self.calibration.session.blocks_receiver(key) {
            Err(tr("calibration.error.busy"))
        } else {
            Ok(())
        }
    }

    pub(super) fn calibration_check_receiver_io(&self, key: &str) -> Result<(), String> {
        self.calibration
            .session
            .observe_aliases(&self.inventory, &self.manager, &self.monitor);
        if self.calibration.session.blocks_receiver_io(key) {
            Err(tr("calibration.error.busy"))
        } else {
            Ok(())
        }
    }

    pub(super) fn calibration_check_scoped_tracker_io(
        &self,
        receiver: &str,
        address: &str,
    ) -> Result<(), String> {
        self.calibration
            .session
            .observe_aliases(&self.inventory, &self.manager, &self.monitor);
        check_scoped_tracker_io(&self.calibration.session, receiver, address)
    }

    pub(super) fn poll_calibration(&mut self, _cx: &mut Context<Self>) -> bool {
        if self.shutting_down {
            return false;
        }
        let mut changed =
            self.calibration
                .session
                .poll(&self.inventory, &mut self.manager, &mut self.monitor);
        if let Some(snapshot) = self.calibration.session.snapshot() {
            changed |= self.calibration.accept_joined_selection(&snapshot);
        }
        if let Some(snapshot) = self.calibration.session.snapshot()
            && let Some(action) = self.calibration.take_joined_action(&snapshot)
        {
            let id = snapshot.id;
            match action.and_then(|action| self.calibration.session.acknowledge().map(|()| action))
            {
                Ok(action) => {
                    self.calibration.dismissed = Some(id);
                    self.calibration.invalidate_review();
                    if let AfterJoin::NextAxis(axis) = action {
                        self.calibration.axis = Some(axis);
                        self.calibration.guide = Guide::Sensitivity;
                    }
                }
                Err(error) => self.calibration.error = Some(error),
            }
            changed = true;
        }
        changed
    }

    pub(super) fn shutdown_calibration(&mut self) -> Option<CalibrationWorker> {
        self.calibration.review = None;
        self.calibration.after_join = None;
        self.calibration.session.take_shutdown_worker()
    }

    fn calibration_task(&self, cx: &App) -> Result<CalibrationTask, String> {
        match self.calibration.guide {
            Guide::Stationary => Ok(CalibrationTask::Stationary),
            Guide::Sensitivity => {
                let count = self
                    .calibration
                    .revolutions
                    .as_ref()
                    .map(|input| input.read(cx).value().to_string())
                    .unwrap_or_else(|| "5".into());
                sensitivity_task(self.calibration.axis.unwrap_or(Axis::X), &count)
            }
        }
    }

    fn calibration_review_ticket(&self, task: CalibrationTask) -> Result<ReviewTicket, String> {
        let mut selection = self
            .calibration
            .selection
            .clone()
            .ok_or_else(|| tr("calibration.error.select_target"))?;
        if let Target::Direct { endpoint, .. } = &selection.target {
            let snapshot = self
                .calibration
                .session
                .snapshot()
                .filter(|snapshot| inspection_ready(snapshot, endpoint, task))
                .ok_or_else(|| tr("calibration.error.inspect_first"))?;
            selection.target = Target::Direct {
                endpoint: endpoint.clone(),
                generation: None,
                address: snapshot.identity.clone(),
            };
        }
        self.calibration.session.review(
            selection,
            task,
            &self.inventory,
            &self.manager,
            &self.monitor,
        )
    }

    fn calibration_guard_ticket(
        &self,
        ticket: &ReviewTicket,
        recovery: bool,
    ) -> Result<(), String> {
        if self.shutting_down {
            return Err(tr("app.status.closing"));
        }
        if !calibration_target_console_allowed(ticket.target(), &self.inventory)
            || (matches!(ticket.target(), Target::Direct { .. }) && self.inventory_error.is_some())
        {
            return Err(tr("serial.console.error.not_allowed"));
        }
        if let Some(address) = ticket.address() {
            self.capture_check_address(address)?;
            self.flash_only_check_address(address)?;
        }
        if let Target::Wireless { receiver, .. } = ticket.target() {
            if recovery {
                self.capture_check_receiver(&receiver.key)?;
                self.flash_only_check_receiver(&receiver.key)?;
            } else {
                self.flash_check_receiver(&receiver.key)?;
            }
        }
        let dumps = self.manager.active_thermal_dumps();
        for port in &ticket.refresh_ports(&self.inventory, &self.manager, &self.monitor) {
            if recovery {
                self.capture_check_port(port)?;
                self.flash_only_check_port(port)?;
            } else {
                self.flash_check_port(port)?;
            }
            if dumps
                .iter()
                .any(|request| same_port_name(&request.port, port))
            {
                return Err(tr("calibration.error.thermal_busy"));
            }
            if self.manager.thermal_dump_needs_reconnect(port) {
                return Err(tr("calibration.error.thermal_incomplete"));
            }
        }
        Ok(())
    }

    fn inspect_calibration(&mut self, cx: &mut Context<Self>) {
        if self.calibration.session.busy() || self.shutting_down {
            return;
        }
        self.calibration.invalidate_review();
        let result = (|| {
            let selection = self
                .calibration
                .selection
                .clone()
                .ok_or_else(|| tr("calibration.error.select_target"))?;
            let task = self.calibration_task(cx)?;
            let ticket = self.calibration.session.review(
                selection,
                task,
                &self.inventory,
                &self.manager,
                &self.monitor,
            )?;
            self.calibration_guard_ticket(&ticket, false)?;
            self.calibration.session.start(
                ticket,
                RunMode::Inspect,
                &mut self.manager,
                &mut self.monitor,
            )
        })();
        self.calibration.error = result.err();
        self.calibration.dismissed = None;
        cx.notify();
    }

    fn review_calibration(&mut self, cx: &mut Context<Self>) {
        if self.calibration.session.busy() || self.shutting_down {
            return;
        }
        self.calibration.invalidate_review();
        let result = (|| {
            let task = self.calibration_task(cx)?;
            let ticket = self.calibration_review_ticket(task)?;
            self.calibration_guard_ticket(&ticket, false)?;
            Ok(ReviewedGuide {
                label: self
                    .calibration
                    .selection
                    .as_ref()
                    .map(|selection| selection.label.clone())
                    .unwrap_or_default(),
                ticket,
            })
        })();
        match result {
            Ok(review) => self.calibration.review = Some(review),
            Err(error) => self.calibration.error = Some(error),
        }
        cx.notify();
    }

    fn start_calibration(&mut self, cx: &mut Context<Self>) {
        if self.shutting_down {
            return;
        }
        let Some(review) = self.calibration.review.take() else {
            return;
        };
        let result = self
            .calibration_guard_ticket(&review.ticket, false)
            .and_then(|()| {
                self.calibration.session.start(
                    review.ticket,
                    RunMode::Execute,
                    &mut self.manager,
                    &mut self.monitor,
                )
            });
        self.calibration.error = result.err();
        self.calibration.dismissed = None;
        self.calibration.details = false;
        cx.notify();
    }

    fn recover_calibration(&mut self, cx: &mut Context<Self>) {
        if self.shutting_down {
            return;
        }
        let result = (|| {
            let snapshot = self
                .calibration
                .session
                .snapshot()
                .filter(|snapshot| snapshot.joined)
                .ok_or_else(|| tr("calibration.error.wait_cleanup"))?;
            let ticket = self.calibration.session.review(
                GuideSelection {
                    target: snapshot.target.clone(),
                    label: target_description(&snapshot.target),
                },
                snapshot.task,
                &self.inventory,
                &self.manager,
                &self.monitor,
            )?;
            self.calibration_guard_ticket(&ticket, true)?;
            self.calibration
                .session
                .recover(&mut self.manager, &mut self.monitor)
        })();
        self.calibration.error = result.err();
        cx.notify();
    }

    fn advance_calibration(&mut self, action: AfterJoin, cx: &mut Context<Self>) {
        if self.calibration.session.stopping() || self.calibration.after_join.is_some() {
            return;
        }
        if let Some(snapshot) = self.calibration.session.snapshot()
            && can_advance(&snapshot)
        {
            self.calibration.after_join = Some(PendingAdvance {
                id: snapshot.id,
                action,
                persistence: snapshot.persistence,
            });
            self.calibration.session.stop_observing();
            self.poll_calibration(cx);
            cx.notify();
        }
    }

    #[inline(never)]
    pub(super) fn render_calibration(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if self.calibration.revolutions.is_none() {
            let input = cx.new(|cx| InputState::new(window, cx).default_value("5"));
            self.calibration.count_subscription =
                Some(cx.subscribe_in(&input, window, |this, _, event, _, cx| {
                    if matches!(event, InputEvent::Change) {
                        this.calibration.invalidate_review();
                        cx.notify();
                    }
                }));
            self.calibration.revolutions = Some(input);
        }
        let mut page = div()
            .id("calibration-page")
            .track_focus(&self.calibration_page_focus)
            .flex_1()
            .h_full()
            .min_w_0()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(tr("calibration.title")),
            )
            .child(self.calibration_choices(cx));
        if let Some(snapshot) = self.calibration.session.snapshot()
            && self.calibration.dismissed != Some(snapshot.id)
        {
            page = page.child(self.calibration_run(&snapshot, cx));
        } else if let Some(review) = &self.calibration.review {
            page = page.child(self.calibration_review_panel(review, cx));
        } else {
            page = page.child(self.calibration_form(cx));
        }
        if let Some(error) = &self.calibration.error {
            page = page.child(
                panel(cx)
                    .text_color(cx.theme().danger)
                    .child(rust_i18n::t!("calibration.error.detail", error = error).to_string()),
            );
        }
        page.into_any_element()
    }
}

fn calibration_target_console_allowed(
    target: &Target,
    inventory: &crate::domain::Inventory,
) -> bool {
    match target {
        Target::Direct { endpoint, .. } => crate::domain::console_port_allowed(endpoint, inventory),
        Target::Wireless { .. } => true,
    }
}

fn sensitivity_task(axis: Axis, count: &str) -> Result<CalibrationTask, String> {
    let count = count
        .trim()
        .parse::<u16>()
        .ok()
        .and_then(|count| Revolutions::new(count).ok())
        .ok_or_else(|| tr("calibration.error.count"))?;
    Ok(CalibrationTask::Sensitivity {
        axis,
        revolutions: count,
    })
}

fn same_target(a: &Target, b: &Target) -> bool {
    match (a, b) {
        (Target::Direct { endpoint: a, .. }, Target::Direct { endpoint: b, .. }) => {
            same_serial_endpoint(a, b)
        }
        (
            Target::Wireless {
                receiver: a,
                slot: sa,
                address: aa,
            },
            Target::Wireless {
                receiver: b,
                slot: sb,
                address: ab,
            },
        ) => a.key == b.key && sa == sb && aa == ab,
        _ => false,
    }
}

fn same_serial_endpoint(
    a: &crate::domain::SerialEndpoint,
    b: &crate::domain::SerialEndpoint,
) -> bool {
    same_port_name(&a.port_name, &b.port_name)
        && a.serial_number == b.serial_number
        && a.vid == b.vid
        && a.pid == b.pid
        && a.kind == b.kind
        && a.product == b.product
        && a.manufacturer == b.manufacturer
        && a.same_usb_interface(b)
}

fn inspection_ready(
    snapshot: &Snapshot,
    endpoint: &crate::domain::SerialEndpoint,
    task: CalibrationTask,
) -> bool {
    snapshot.joined
        && (snapshot.end == Some(ObservationEnd::Complete)
            || snapshot.application == Application::Applied)
        && snapshot.identity.is_some()
        && snapshot.uncertainty.is_none()
        && matches!(&snapshot.target, Target::Direct { endpoint: observed, .. } if same_serial_endpoint(observed, endpoint))
        && match task {
            CalibrationTask::Stationary => snapshot.stationary_supported,
            CalibrationTask::Sensitivity { .. } => snapshot.sensitivity_supported,
        }
}

fn can_advance(snapshot: &Snapshot) -> bool {
    snapshot.mode != RunMode::Inspect
        && snapshot.application == Application::Applied
        && !snapshot.acknowledged
}

fn can_return_to_preparation(snapshot: &Snapshot) -> bool {
    snapshot.joined
        && !can_advance(snapshot)
        && (snapshot.acknowledged || !snapshot.needs_acknowledgement())
}

fn next_axis(task: CalibrationTask) -> Option<Axis> {
    match task {
        CalibrationTask::Sensitivity { axis: Axis::X, .. } => Some(Axis::Y),
        CalibrationTask::Sensitivity { axis: Axis::Y, .. } => Some(Axis::Z),
        _ => None,
    }
}

fn panel(cx: &App) -> Div {
    div()
        .flex()
        .flex_col()
        .flex_shrink_0()
        .min_w_0()
        .w_full()
        .gap_2()
        .p_3()
        .rounded_lg()
        .border_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().group_box)
}

pub(super) fn check_scoped_tracker_io(
    session: &CalibrationSession,
    receiver: &str,
    address: &str,
) -> Result<(), String> {
    if session.blocks_scoped_tracker_io(receiver, address) {
        Err(tr("calibration.error.busy"))
    } else {
        Ok(())
    }
}

fn target_description(target: &Target) -> String {
    match target {
        Target::Direct {
            endpoint, address, ..
        } => direct_description(endpoint, address.as_deref()),
        Target::Wireless {
            receiver,
            slot,
            address,
        } => rust_i18n::t!(
            "calibration.target.wireless",
            receiver = receiver.label,
            usb = receiver
                .serial_number
                .as_deref()
                .unwrap_or(receiver.key.as_str()),
            slot = slot,
            address = address,
        )
        .to_string(),
    }
}

fn direct_description(endpoint: &crate::domain::SerialEndpoint, address: Option<&str>) -> String {
    rust_i18n::t!(
        "calibration.target.direct",
        port = endpoint.port_name,
        usb = endpoint
            .serial_number
            .as_deref()
            .unwrap_or(endpoint.label.as_str()),
        address = address.map_or_else(|| tr("calibration.target.inspect_identity"), str::to_owned),
    )
    .to_string()
}

fn run_cue(snapshot: &Snapshot, stopping: bool) -> RunCue {
    if !snapshot.joined && (stopping || snapshot.end.is_some()) {
        return RunCue::Closing;
    }
    if snapshot.mode == RunMode::Inspect {
        return if !snapshot.joined {
            RunCue::Inspecting
        } else if snapshot.end == Some(ObservationEnd::Complete)
            && snapshot.identity.is_some()
            && snapshot.uncertainty.is_none()
        {
            RunCue::Inspected
        } else if snapshot.end == Some(ObservationEnd::Stopped) {
            RunCue::InspectionStopped
        } else {
            RunCue::InspectionFailed
        };
    }
    if snapshot.end.is_some()
        && snapshot.dispatch == Dispatch::NotAttempted
        && snapshot.application == Application::Unconfirmed
    {
        return RunCue::NotStarted;
    }
    if snapshot.end.is_some()
        || matches!(
            snapshot.application,
            Application::Applied
                | Application::Rejected
                | Application::Failed
                | Application::Cancelled
                | Application::Conflicting
        )
    {
        return RunCue::Result(snapshot.application);
    }
    if matches!(snapshot.phase, Phase::WaitingRotation | Phase::Recording)
        && let CalibrationTask::Sensitivity { axis, revolutions } = snapshot.task
    {
        return RunCue::Rotate { axis, revolutions };
    }
    match snapshot.phase {
        Phase::Preparing => RunCue::Preparing,
        Phase::WaitingStill => RunCue::HoldStill,
        Phase::Retrimming => RunCue::Retrimming,
        Phase::Collecting => RunCue::Collecting,
        Phase::Validating => RunCue::Validating,
        Phase::ReadingBack => RunCue::ReadingBack,
        Phase::CandidateQueued => RunCue::Result(Application::CandidateQueued),
        Phase::Finished | Phase::WaitingRotation | Phase::Recording => RunCue::Observing,
    }
}

#[cfg(test)]
mod tests;
