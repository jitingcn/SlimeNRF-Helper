use super::super::components::action_card;
use super::{
    AfterJoin, Guide, ReviewedGuide, RunCue, Workbench, can_advance, can_return_to_preparation,
    direct_description, next_axis, panel, run_cue, same_serial_endpoint, same_target,
    target_description,
};
use crate::calibration::{
    self as calibration, Application, Axis, CalibrationTask, ConsoleValues, Dispatch,
    ObservationEnd, Persistence, RunMode, Snapshot, Target,
};
use crate::i18n::tr;
use gpui_kit::component::{
    ActiveTheme, Disableable, Selectable, Sizable,
    button::{Button, ButtonVariants},
    input::Input,
};
use gpui_kit::{AnyElement, App, Context, Div, FontWeight, SharedString, div, prelude::*, px};

fn calibration_target_id(target: &Target) -> SharedString {
    match target {
        Target::Direct {
            endpoint,
            generation,
            address,
        } => {
            let port = if cfg!(windows) {
                std::borrow::Cow::Owned(
                    endpoint
                        .port_name
                        .trim_start_matches(r"\\.\")
                        .to_ascii_lowercase(),
                )
            } else {
                std::borrow::Cow::Borrowed(endpoint.port_name.as_str())
            };
            format!(
                "calibration-target:direct:{:?}",
                (
                    port,
                    endpoint.kind,
                    endpoint.vid,
                    endpoint.pid,
                    &endpoint.serial_number,
                    &endpoint.usb_interface,
                    &endpoint.product,
                    &endpoint.manufacturer,
                    generation,
                    address,
                )
            )
            .into()
        }
        Target::Wireless {
            receiver,
            slot,
            address,
        } => format!(
            "calibration-target:wireless:{:?}",
            (
                &receiver.key,
                &receiver.path,
                &receiver.serial_number,
                slot,
                address,
            )
        )
        .into(),
    }
}

impl Workbench {
    #[inline(never)]
    pub(super) fn calibration_choices(&self, cx: &mut Context<Self>) -> AnyElement {
        let visible_run = self
            .calibration
            .session
            .snapshot()
            .is_some_and(|snapshot| self.calibration.dismissed != Some(snapshot.id));
        let enabled = !visible_run
            && self.calibration.review.is_none()
            && !self.calibration.session.busy()
            && !self.shutting_down;
        let mut row = div().flex().flex_wrap().gap_3().flex_shrink_0();
        for (index, guide, title, description) in [
            (
                0,
                Guide::Stationary,
                "calibration.guide.stationary",
                "calibration.guide.stationary_hint",
            ),
            (
                1,
                Guide::Sensitivity,
                "calibration.guide.sensitivity",
                "calibration.guide.sensitivity_hint",
            ),
        ] {
            let title: SharedString = tr(title).into();
            row = row.child(
                div().flex_1().min_w(px(220.)).child(
                    action_card(format!("calibration-guide-{index}").into(), enabled, cx)
                        .selected(self.calibration.guide == guide)
                        .accessibility_label(title.clone())
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .min_w_0()
                                .w_full()
                                .gap_1()
                                .text_left()
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(title),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .whitespace_normal()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(tr(description)),
                                ),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.calibration.guide = guide;
                            this.calibration.invalidate_review();
                            cx.notify();
                        })),
                ),
            );
        }
        row.into_any_element()
    }

    #[inline(never)]
    pub(super) fn calibration_form(&self, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.calibration.session.busy() || self.shutting_down;
        let mut selections = calibration::selections(&self.inventory, &self.manager, &self.monitor);
        if let Some(selection) = &self.calibration.selection
            && let Target::Direct { endpoint, .. } = &selection.target
            && !self.inventory.serial_enumeration_failed
            && crate::domain::console_port_allowed(endpoint, &self.inventory)
            && self
                .inventory
                .ports
                .iter()
                .any(|current| same_serial_endpoint(current, endpoint))
            && !selections
                .iter()
                .any(|current| same_target(&current.target, &selection.target))
            && let Some(snapshot) = self.calibration.session.snapshot()
            && snapshot.joined
            && snapshot.identity.is_some()
            && matches!(&snapshot.target, Target::Direct { endpoint: observed, .. } if same_serial_endpoint(observed, endpoint))
        {
            let mut inspected = selection.clone();
            inspected.target = Target::Direct {
                endpoint: endpoint.clone(),
                generation: None,
                address: snapshot.identity.clone(),
            };
            selections.push(inspected);
        }
        let mut targets = div()
            .id("calibration-targets")
            .flex()
            .flex_col()
            .gap_2()
            .min_w_0()
            .max_h(px(168.))
            .flex_shrink_0()
            .overflow_y_scroll()
            .overflow_x_hidden();
        if selections.is_empty() {
            targets = targets.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(tr("calibration.target.none")),
            );
        }
        for selection in selections {
            let selected = self
                .calibration
                .selection
                .as_ref()
                .is_some_and(|current| same_target(&current.target, &selection.target));
            let caption = target_description(
                self.calibration
                    .selection
                    .as_ref()
                    .filter(|_| selected)
                    .map_or(&selection.target, |current| &current.target),
            );
            let caption = self.calibration_device_label(&selection.target, caption);
            let title: SharedString = selection.label.clone().into();
            targets = targets.child(
                action_card(calibration_target_id(&selection.target), !busy, cx)
                    .selected(selected)
                    .accessibility_label(format!("{} {}", title, caption))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .min_w_0()
                            .w_full()
                            .text_left()
                            .child(div().text_sm().whitespace_normal().child(title))
                            .child(
                                div()
                                    .text_xs()
                                    .whitespace_normal()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(caption),
                            ),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.calibration.selection = Some(selection.clone());
                        this.calibration.invalidate_review();
                        cx.notify();
                    })),
            );
        }
        let mut form = panel(cx)
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(tr("calibration.target.title")),
            )
            .child(targets);
        if self.calibration.guide == Guide::Sensitivity {
            let mut axes = div()
                .flex()
                .items_center()
                .flex_wrap()
                .gap_2()
                .child(tr("calibration.form.sensor_axis"));
            for axis in [Axis::X, Axis::Y, Axis::Z] {
                axes = axes.child(
                    Button::new(format!("calibration-axis-{}", axis.label()))
                        .small()
                        .label(axis_name(axis))
                        .selected(self.calibration.axis.unwrap_or(Axis::X) == axis)
                        .disabled(busy)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.calibration.axis = Some(axis);
                            this.calibration.invalidate_review();
                            cx.notify();
                        })),
                );
            }
            form = form
                .child(axes)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .flex_wrap()
                        .gap_2()
                        .child(tr("calibration.form.revolutions"))
                        .when_some(self.calibration.revolutions.as_ref(), |row, input| {
                            row.child(
                                div()
                                    .w(px(92.))
                                    .child(Input::new(input).small().disabled(busy)),
                            )
                        })
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(tr("calibration.form.count_hint")),
                        ),
                )
                .child(
                    div()
                        .text_xs()
                        .whitespace_normal()
                        .text_color(cx.theme().muted_foreground)
                        .child(tr("calibration.form.axis_hint")),
                );
        }
        let selected_direct = self
            .calibration
            .selection
            .as_ref()
            .is_some_and(|selection| matches!(selection.target, Target::Direct { .. }));
        form.child(
            div()
                .flex()
                .flex_wrap()
                .gap_2()
                .when(selected_direct, |row| {
                    row.child(
                        Button::new("calibration-inspect")
                            .label(tr("calibration.action.inspect"))
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| this.inspect_calibration(cx))),
                    )
                })
                .child(
                    Button::new("calibration-review")
                        .primary()
                        .label(tr("calibration.action.review"))
                        .disabled(busy || self.calibration.selection.is_none())
                        .on_click(cx.listener(|this, _, _, cx| this.review_calibration(cx))),
                ),
        )
        .when_some(self.calibration.selection.as_ref(), |form, _| {
            form.child(
                div()
                    .text_xs()
                    .whitespace_normal()
                    .text_color(cx.theme().muted_foreground)
                    .child(tr(if selected_direct {
                        "calibration.target.inspect_hint"
                    } else {
                        "calibration.target.wireless_hint"
                    })),
            )
        })
        .into_any_element()
    }

    #[inline(never)]
    pub(super) fn calibration_review_panel(
        &self,
        review: &ReviewedGuide,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        panel(cx)
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(tr("calibration.review.title")),
            )
            .child(div().whitespace_normal().child(review.label.clone()))
            .child(
                div()
                    .text_sm()
                    .whitespace_normal()
                    .child(match review.ticket.target() {
                        Target::Direct { .. } => self.address_label(
                            review.ticket.address(),
                            target_description(review.ticket.target()),
                        ),
                        target => self.calibration_device_label(target, target_description(target)),
                    }),
            )
            .child(
                div()
                    .text_sm()
                    .whitespace_normal()
                    .child(task_description(review.ticket.task())),
            )
            .child(
                div()
                    .text_sm()
                    .whitespace_normal()
                    .child(tr(match review.ticket.task() {
                        CalibrationTask::Stationary => "calibration.instruction.stationary",
                        CalibrationTask::Sensitivity { .. } => {
                            "calibration.instruction.before_rotation"
                        }
                    })),
            )
            .child(
                div()
                    .text_xs()
                    .whitespace_normal()
                    .text_color(cx.theme().muted_foreground)
                    .child(tr("calibration.review.one_operator")),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .flex_wrap()
                    .child(
                        Button::new("calibration-start")
                            .primary()
                            .label(tr("calibration.action.start"))
                            .disabled(self.calibration.session.busy() || self.shutting_down)
                            .on_click(cx.listener(|this, _, _, cx| this.start_calibration(cx))),
                    )
                    .child(
                        Button::new("calibration-back")
                            .label(tr("common.action.back"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.calibration.review = None;
                                cx.notify();
                            })),
                    ),
            )
            .into_any_element()
    }

    #[inline(never)]
    pub(super) fn calibration_run(
        &self,
        snapshot: &Snapshot,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let cue = run_cue(snapshot, self.calibration.session.stopping());
        let mut result = panel(cx)
            .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(tr(
                if snapshot.mode == RunMode::Inspect {
                    "calibration.result.inspection"
                } else {
                    "calibration.result.title"
                },
            )))
            .child(
                div()
                    .text_xs()
                    .whitespace_normal()
                    .child(match &snapshot.target {
                        Target::Direct {
                            endpoint, address, ..
                        } => {
                            let address = snapshot.identity.as_deref().or(address.as_deref());
                            self.address_label(address, direct_description(endpoint, address))
                        }
                        target => self.calibration_device_label(target, target_description(target)),
                    }),
            )
            .child(
                div()
                    .text_sm()
                    .whitespace_normal()
                    .child(task_description(snapshot.task)),
            )
            .child(
                div()
                    .text_base()
                    .font_weight(FontWeight::SEMIBOLD)
                    .whitespace_normal()
                    .child(instruction(cue)),
            );
        if snapshot.acknowledged {
            result = result.child(div().text_xs().child(tr("calibration.result.acknowledged")));
        }
        if snapshot.persistence == Persistence::StorageError {
            result = result.child(
                div()
                    .text_sm()
                    .whitespace_normal()
                    .text_color(cx.theme().danger)
                    .child(tr(if snapshot.mode == RunMode::Inspect {
                        "calibration.result.inspection_storage_error"
                    } else {
                        "calibration.result.storage_error"
                    })),
            );
        }
        let pending = self.calibration.after_join.is_some() || cue == RunCue::Closing;
        let mut actions = div().flex().flex_wrap().gap_2();
        if !snapshot.joined && snapshot.end.is_none() {
            actions = actions.child(
                Button::new("calibration-stop-observing")
                    .label(tr("calibration.action.stop_observing"))
                    .disabled(pending)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.calibration.session.stop_observing();
                        cx.notify();
                    })),
            );
        }
        if can_advance(snapshot) {
            actions = actions.child(
                Button::new("calibration-finish")
                    .primary()
                    .label(tr("calibration.action.finish"))
                    .disabled(pending)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.advance_calibration(AfterJoin::Finish, cx)
                    })),
            );
            if let Some(next) = next_axis(snapshot.task) {
                actions = actions.child(
                    Button::new("calibration-next-axis")
                        .label(
                            rust_i18n::t!("calibration.action.next_axis", axis = axis_name(next))
                                .to_string(),
                        )
                        .disabled(pending)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.advance_calibration(AfterJoin::NextAxis(next), cx);
                        })),
                );
            }
        } else if snapshot.joined && !snapshot.acknowledged && snapshot.needs_acknowledgement() {
            if snapshot.application != Application::CandidateQueued {
                result = result.child(
                    div()
                        .text_sm()
                        .whitespace_normal()
                        .text_color(cx.theme().warning)
                        .child(tr("calibration.result.unknown_detail")),
                );
            }
            actions = actions.child(
                Button::new("calibration-acknowledge")
                    .primary()
                    .label(tr(
                        if snapshot.application == Application::CandidateQueued {
                            "calibration.action.acknowledge_result"
                        } else {
                            "calibration.action.acknowledge_unknown"
                        },
                    ))
                    .on_click(cx.listener(|this, _, _, cx| {
                        match this.calibration.session.acknowledge() {
                            Ok(()) => {
                                this.calibration.dismissed = this
                                    .calibration
                                    .session
                                    .snapshot()
                                    .map(|snapshot| snapshot.id);
                                this.calibration.invalidate_review();
                            }
                            Err(error) => this.calibration.error = Some(error),
                        }
                        cx.notify();
                    })),
            );
            if matches!(snapshot.target, Target::Direct { .. }) || snapshot.association.is_some() {
                actions = actions.child(
                    Button::new("calibration-recover")
                        .label(tr("calibration.action.recover"))
                        .on_click(cx.listener(|this, _, _, cx| this.recover_calibration(cx))),
                );
            }
        }
        if can_return_to_preparation(snapshot) {
            let id = snapshot.id;
            actions = actions.child(
                Button::new("calibration-prepare")
                    .primary()
                    .label(tr("calibration.action.prepare"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.calibration.dismissed = Some(id);
                        this.calibration.invalidate_review();
                        cx.notify();
                    })),
            );
        }
        result = result.child(actions);
        result = result.child(
            Button::new("calibration-details")
                .small()
                .label(tr(if self.calibration.details {
                    "calibration.action.hide_details"
                } else {
                    "calibration.action.details"
                }))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.calibration.details = !this.calibration.details;
                    cx.notify();
                })),
        );
        if self.calibration.details {
            result = result.child(self.calibration_details(snapshot, cx));
        }
        result.into_any_element()
    }

    #[inline(never)]
    fn calibration_details(&self, snapshot: &Snapshot, cx: &App) -> AnyElement {
        let mut details = div()
            .flex()
            .flex_col()
            .gap_2()
            .text_xs()
            .whitespace_normal()
            .text_color(cx.theme().muted_foreground);
        if snapshot.mode == RunMode::Inspect {
            details = details
                .child(tr("calibration.detail.inspection"))
                .child(fact_row(
                    "calibration.result.command_support",
                    rust_i18n::t!(
                        "calibration.support.summary",
                        stationary = tr(if snapshot.stationary_supported {
                            "calibration.support.advertised"
                        } else {
                            "calibration.support.unconfirmed"
                        }),
                        sensitivity = tr(if snapshot.sensitivity_supported {
                            "calibration.support.advertised"
                        } else {
                            "calibration.support.unconfirmed"
                        }),
                    )
                    .to_string(),
                ));
        } else {
            details = details
                .child(fact_row(
                    "calibration.result.dispatch",
                    dispatch_label(snapshot.dispatch),
                ))
                .child(fact_row(
                    "calibration.result.application",
                    tr(application_key(snapshot.application)),
                ))
                .child(fact_row(
                    "calibration.result.storage",
                    tr(match snapshot.persistence {
                        Persistence::Unknown => "calibration.storage.unknown",
                        Persistence::StorageError => "calibration.storage.error",
                    }),
                ))
                .child(tr("calibration.detail.storage"))
                .child(tr("calibration.detail.observation"));
        }
        details = details
            .child(fact_row(
                "calibration.result.readback",
                readback_label(snapshot),
            ))
            .child(fact_row(
                "calibration.result.coverage",
                observation_label(snapshot),
            ));
        match &snapshot.target {
            Target::Direct { .. } => {
                if snapshot.mode != RunMode::Inspect {
                    details = details.child(tr("calibration.detail.direct"));
                    if snapshot.task == CalibrationTask::Stationary {
                        details = details.child(tr("calibration.detail.stationary"));
                    }
                }
                for (key, values) in [
                    ("calibration.readback.before", snapshot.baseline.as_ref()),
                    ("calibration.readback.after", snapshot.readback.as_ref()),
                ] {
                    if let Some(values) = values {
                        details = details.child(values_panel(key, values));
                    }
                }
                if let (Some(before), Some(after)) = (&snapshot.baseline, &snapshot.readback)
                    && matches!(snapshot.task, CalibrationTask::Sensitivity { .. })
                {
                    let comparison = calibration::compare(snapshot.task, before, after);
                    details = details.child(tr(if !comparison.complete {
                        "calibration.readback.comparison_incomplete"
                    } else if comparison.applied_bias_changed
                        || comparison
                            .non_target_scales_changed
                            .iter()
                            .any(|changed| *changed)
                    {
                        "calibration.readback.discrepancy"
                    } else {
                        "calibration.readback.consistent"
                    }));
                    details = details.child(tr("calibration.readback.precision"));
                }
            }
            Target::Wireless { .. } => {
                details = details.child(tr("calibration.detail.wireless"));
            }
        }
        if snapshot.needs_acknowledgement() {
            details = details.child(tr("calibration.detail.recovery"));
        }
        if let Some(uncertainty) = &snapshot.uncertainty {
            details = details.child(
                rust_i18n::t!("calibration.detail.diagnostic", detail = uncertainty).to_string(),
            );
        }
        if let Some(error) = &snapshot.cleanup_error {
            details = details.child(
                rust_i18n::t!("calibration.detail.cleanup_error", error = error).to_string(),
            );
        }
        details.into_any_element()
    }
}

fn fact_row(key: &str, value: String) -> Div {
    div()
        .flex()
        .flex_wrap()
        .gap_x_3()
        .gap_y_1()
        .text_xs()
        .child(
            div()
                .w(px(145.))
                .font_weight(FontWeight::MEDIUM)
                .child(tr(key)),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(160.))
                .whitespace_normal()
                .child(value),
        )
}

fn axis_name(axis: Axis) -> String {
    tr(match axis {
        Axis::X => "calibration.axis.x",
        Axis::Y => "calibration.axis.y",
        Axis::Z => "calibration.axis.z",
    })
}

fn task_description(task: CalibrationTask) -> String {
    match task {
        CalibrationTask::Stationary => tr("calibration.guide.stationary"),
        CalibrationTask::Sensitivity { axis, revolutions } => rust_i18n::t!(
            "calibration.form.task",
            axis = axis_name(axis),
            count = revolutions.get(),
        )
        .to_string(),
    }
}

fn instruction(cue: RunCue) -> String {
    let key = match cue {
        RunCue::Closing => "calibration.result.finishing",
        RunCue::Inspecting => "calibration.instruction.inspecting",
        RunCue::Inspected => "calibration.instruction.inspected",
        RunCue::InspectionStopped => "calibration.instruction.inspection_stopped",
        RunCue::InspectionFailed => "calibration.instruction.inspection_failed",
        RunCue::NotStarted => "calibration.instruction.not_started",
        RunCue::Preparing => "calibration.instruction.preparing",
        RunCue::HoldStill => "calibration.instruction.hold_still",
        RunCue::Retrimming => "calibration.instruction.retrimming",
        RunCue::Collecting => "calibration.instruction.collecting",
        RunCue::Validating => "calibration.instruction.validating",
        RunCue::ReadingBack => "calibration.instruction.reading_back",
        RunCue::Observing => "calibration.instruction.observing",
        RunCue::Result(application) => application_key(application),
        RunCue::Rotate { axis, revolutions } => {
            return rust_i18n::t!(
                "calibration.instruction.rotate",
                axis = axis_name(axis),
                count = revolutions.get()
            )
            .to_string();
        }
    };
    tr(key)
}

fn application_key(application: Application) -> &'static str {
    match application {
        Application::Unconfirmed => "calibration.application.unconfirmed",
        Application::CandidateQueued => "calibration.application.queued",
        Application::Applied => "calibration.application.applied",
        Application::Rejected => "calibration.application.rejected",
        Application::Failed => "calibration.application.failed",
        Application::Cancelled => "calibration.application.cancelled",
        Application::Conflicting => "calibration.application.conflicting",
    }
}

fn dispatch_label(dispatch: Dispatch) -> String {
    tr(match dispatch {
        Dispatch::NotAttempted => "calibration.dispatch.none",
        Dispatch::Attempted => "calibration.dispatch.attempted",
        Dispatch::Written => "calibration.dispatch.written",
        Dispatch::ConsoleAccepted => "calibration.dispatch.console_accepted",
        Dispatch::ReceiverConfirmed => "calibration.dispatch.confirmed",
        Dispatch::Rejected => "calibration.dispatch.rejected",
    })
}

fn readback_label(snapshot: &Snapshot) -> String {
    tr(if matches!(snapshot.target, Target::Wireless { .. }) {
        "calibration.readback.wireless_unavailable"
    } else if snapshot.readback_incomplete {
        "calibration.readback.incomplete"
    } else if snapshot.readback.is_some() {
        "calibration.readback.observed"
    } else {
        "calibration.readback.pending"
    })
}

fn observation_label(snapshot: &Snapshot) -> String {
    let key = if !snapshot.joined && snapshot.end.is_some() {
        "calibration.coverage.cleanup"
    } else {
        match snapshot.end {
            None => "calibration.coverage.active",
            Some(ObservationEnd::Complete) => "calibration.coverage.complete",
            Some(ObservationEnd::Stopped) => "calibration.coverage.stopped",
            Some(ObservationEnd::Deadline) => "calibration.coverage.deadline",
            Some(ObservationEnd::Disconnected) => "calibration.coverage.disconnected",
            Some(ObservationEnd::Unsupported) => "calibration.coverage.unsupported",
            Some(ObservationEnd::Ambiguous) => "calibration.coverage.ambiguous",
            Some(ObservationEnd::PreparationFailed) => "calibration.coverage.preparation_failed",
        }
    };
    rust_i18n::t!(
        "calibration.coverage.summary",
        state = tr(key),
        seconds = format!("{:.1}", snapshot.elapsed_ms as f64 / 1000.)
    )
    .to_string()
}

fn values_panel(title: &str, values: &ConsoleValues) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(div().font_weight(FontWeight::MEDIUM).child(tr(title)))
        .child(
            rust_i18n::t!(
                "calibration.readback.interval",
                open = values.native_open,
                start = values.query_start_ms,
                end = values.query_end_ms
            )
            .to_string(),
        )
        .child(fact_row("calibration.readback.bias", vector(values.bias)))
        .child(fact_row(
            "calibration.readback.scales",
            vector(values.scales),
        ))
}

fn vector(values: Option<[f64; 3]>) -> String {
    values.map_or_else(
        || tr("calibration.readback.unavailable"),
        |v| format!("{:.5} / {:.5} / {:.5}", v[0], v[1], v[2]),
    )
}

#[cfg(test)]
mod tests {
    use super::calibration_target_id;
    use crate::calibration::Target;
    use crate::domain::{ReceiverEndpoint, SerialEndpoint, SerialPortKind};

    #[test]
    fn target_identity_distinguishes_direct_generations_and_wireless_slot_reassignment() {
        let mut direct = Target::Direct {
            endpoint: SerialEndpoint {
                port_name: "COM8".into(),
                label: "Tracker".into(),
                manufacturer: None,
                product: None,
                kind: SerialPortKind::Usb,
                serial_number: Some("USB-A".into()),
                vid: Some(0x1209),
                pid: Some(0x7690),
                usb_interface: None,
            },
            generation: Some(1),
            address: Some("112233445566".into()),
        };
        let first = calibration_target_id(&direct);
        let Target::Direct { endpoint, .. } = &mut direct else {
            unreachable!();
        };
        endpoint.label = "Tracker nickname".into();
        assert_eq!(calibration_target_id(&direct), first);
        let Target::Direct { generation, .. } = &mut direct else {
            unreachable!();
        };
        *generation = Some(2);
        assert_ne!(calibration_target_id(&direct), first);
        let mut wireless = Target::Wireless {
            receiver: ReceiverEndpoint {
                key: "receiver".into(),
                path: b"receiver-hid".to_vec(),
                label: "Receiver".into(),
                serial_number: Some("USB-B".into()),
            },
            slot: 1,
            address: "112233445566".into(),
        };
        let first = calibration_target_id(&wireless);
        let Target::Wireless { receiver, .. } = &mut wireless else {
            unreachable!();
        };
        receiver.label = "Receiver nickname".into();
        assert_eq!(calibration_target_id(&wireless), first);
        let Target::Wireless { address, .. } = &mut wireless else {
            unreachable!();
        };
        *address = "223344556677".into();
        assert_ne!(calibration_target_id(&wireless), first);
    }
}
