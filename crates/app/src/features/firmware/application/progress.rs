use super::*;
use crate::update::{Uf2CopyReport, Uf2FinalizationStage};
use gpui_kit::component::progress::Progress as ProgressBar;

pub(super) fn wired_transfer_percent(progress: &Progress) -> Option<f32> {
    if progress.total == 0 {
        return None;
    }
    if progress.completed >= progress.total {
        return Some(100.);
    }
    let percent = (progress.completed as f64 / progress.total as f64 * 100.) as f32;
    // Converting to f32 must not round an incomplete transfer up to 100%.
    Some(percent.min(100.0_f32.next_down()))
}

pub(super) fn ota_terminal(phase: OtaPhase) -> bool {
    matches!(
        phase,
        OtaPhase::Completed
            | OtaPhase::ActivationUnconfirmed
            | OtaPhase::Failed
            | OtaPhase::Cancelled
    )
}

pub(super) fn progress_receiver_groups(
    progress: &[OtaTargetProgress],
) -> std::collections::BTreeMap<&str, Vec<&OtaTargetProgress>> {
    let mut groups = std::collections::BTreeMap::<_, Vec<_>>::new();
    for target in progress {
        if target.phase != OtaPhase::Queued {
            groups
                .entry(target.receiver_key.as_str())
                .or_default()
                .push(target);
        }
    }
    groups
}

type ReceiverResultGroups<'a> =
    std::collections::BTreeMap<&'a str, Vec<(&'a ResultTarget, &'a UpdateOutcome)>>;

pub(super) fn result_receiver_groups(
    results: &[(ResultTarget, UpdateOutcome)],
) -> ReceiverResultGroups<'_> {
    let mut groups = std::collections::BTreeMap::<_, Vec<_>>::new();
    for (target, result) in results {
        if let Some((key, _)) = &target.receiver {
            groups
                .entry(key.as_str())
                .or_default()
                .push((target, result));
        }
    }
    groups
}

fn ota_phase_label(phase: OtaPhase) -> &'static str {
    match phase {
        OtaPhase::Queued => "firmware.phase.queued",
        OtaPhase::Preparing => "firmware.phase.preparing",
        OtaPhase::Transferring => "firmware.phase.transferring",
        OtaPhase::Verifying => "firmware.phase.verifying",
        OtaPhase::Activating => "firmware.phase.activating",
        OtaPhase::Completed => "firmware.phase.completed",
        OtaPhase::ActivationUnconfirmed => "firmware.phase.activation_unconfirmed",
        OtaPhase::Failed => "firmware.phase.failed",
        OtaPhase::Cancelled => "firmware.phase.cancelled",
    }
}

fn ota_progress_label(target: &OtaTargetProgress) -> &'static str {
    if target.phase == OtaPhase::Preparing {
        match target.preparation {
            Some(OtaPreparation::ReceiverRecovery) => "firmware.phase.wait_receiver",
            Some(OtaPreparation::QueryingDevice) => "firmware.phase.check_device",
            Some(OtaPreparation::StartingUpdate) => "firmware.phase.start_update",
            None => ota_phase_label(target.phase),
        }
    } else {
        ota_phase_label(target.phase)
    }
}

pub(super) fn ota_terminal_tooltip(target: &OtaTargetProgress, mut identity: String) -> String {
    if matches!(target.phase, OtaPhase::Failed | OtaPhase::Cancelled)
        && let Some(error) = &target.error
    {
        identity.push('\n');
        identity.push_str(error);
    }
    identity
}

pub(super) fn activation_report_label(report: &ReceiverActivationReport) -> String {
    let submission = match &report.submission {
        ActivationSubmission::Sent => "firmware.result.activation.sent",
        ActivationSubmission::DeliveryUncertain(_) => {
            "firmware.result.activation.delivery_uncertain"
        }
        ActivationSubmission::Accepted => "firmware.result.activation.accepted",
    };
    let availability = match &report.availability {
        ReceiverAvailability::Responding => "firmware.result.activation.responding",
        ReceiverAvailability::Detected { .. } => "firmware.result.activation.detected",
        ReceiverAvailability::Unconfirmed(_) => "firmware.result.activation.availability_pending",
    };
    format!("{} · {}", tr(submission), tr(availability))
}

pub(super) fn activation_report_tooltip(
    report: &ReceiverActivationReport,
    mut identity: String,
) -> String {
    if let ActivationSubmission::DeliveryUncertain(error) = &report.submission {
        identity.push('\n');
        identity.push_str(error);
    }
    let diagnostic = match &report.availability {
        ReceiverAvailability::Unconfirmed(error) => Some(error),
        ReceiverAvailability::Detected { prior_diagnostic } => Some(prior_diagnostic),
        ReceiverAvailability::Responding => None,
    };
    if let Some(diagnostic) = diagnostic {
        identity.push('\n');
        identity.push_str(&rust_i18n::t!(
            "firmware.result.activation.observation",
            diagnostic = diagnostic.as_str()
        ));
    }
    identity.push('\n');
    identity.push_str(&tr("firmware.result.activation.unverified"));
    identity
}

pub(super) fn uf2_report_details(report: &Uf2CopyReport) -> String {
    let stage = tr(match report.stage {
        Uf2FinalizationStage::Flush => "firmware.result.uf2.flush",
        Uf2FinalizationStage::SyncAll => "firmware.result.uf2.sync",
    });
    rust_i18n::t!(
        "firmware.result.uf2.details",
        completed = report.completed,
        total = report.total,
        stage = stage,
        diagnostic = report.diagnostic.as_str()
    )
    .to_string()
}

impl From<firmware_update::ResultIdentity> for ResultTarget {
    fn from(identity: firmware_update::ResultIdentity) -> Self {
        use firmware_update::ResultIdentity;
        match identity {
            ResultIdentity::Uf2(volume) => Self::generic(
                volume.identity,
                format!("{} · {}", volume.label, volume.mount.display()),
            ),
            ResultIdentity::Reboot(volume) => {
                Self::generic(volume.identity, volume.mount.display().to_string())
            }
            ResultIdentity::Serial(port) => Self::generic(port.clone(), port),
            ResultIdentity::Esb(target) => Self::ota(&target),
            ResultIdentity::Ble(device) => {
                let label = format!("{} · {}", device.name, device.address);
                Self::generic(device.address, label)
            }
        }
    }
}

impl ResultTarget {
    pub(super) fn generic(key: String, label: String) -> Self {
        Self {
            key,
            identity: label.clone(),
            label,
            receiver: None,
            id: None,
            registry_key: None,
            receiver_registry_key: None,
        }
    }

    pub(super) fn ota(target: &esb_ota::Target) -> Self {
        Self {
            key: ota_target_key(target),
            label: if target.id == esb_ota::RECEIVER_ID {
                tr("common.device_role.receiver")
            } else {
                format!("{} {}", tr("common.device_role.tracker"), target.id)
            },
            identity: format!(
                "{}\n{}\n{}",
                target_label(target),
                target.receiver.key,
                target.receiver.serial_number.as_deref().unwrap_or("")
            ),
            receiver: Some((target.receiver.key.clone(), target.receiver.label.clone())),
            id: Some(target.id),
            registry_key: if target.id == esb_ota::RECEIVER_ID {
                target
                    .receiver
                    .serial_number
                    .as_deref()
                    .and_then(crate::features::device_registry::model::DeviceKey::receiver_serial)
            } else {
                target
                    .address
                    .as_deref()
                    .and_then(crate::features::device_registry::model::DeviceKey::tracker_address)
            },
            receiver_registry_key: target
                .receiver
                .serial_number
                .as_deref()
                .and_then(crate::features::device_registry::model::DeviceKey::receiver_serial),
        }
    }
}

impl OtaCounts {
    pub(super) fn from_progress(targets: &[OtaTargetProgress]) -> Self {
        let mut counts = Self::default();
        for target in targets {
            match target.phase {
                OtaPhase::Completed => counts.completed += 1,
                OtaPhase::ActivationUnconfirmed => counts.activation_unconfirmed += 1,
                OtaPhase::Failed => counts.failed += 1,
                OtaPhase::Cancelled => counts.cancelled += 1,
                OtaPhase::Queued => counts.queued += 1,
                _ => counts.active += 1,
            }
        }
        counts
    }

    fn summary(&self, total: usize) -> String {
        let status = if self.active + self.queued > 0 {
            "firmware.status.updating"
        } else if self.cancelled > 0 {
            "firmware.status.cancelled"
        } else if self.failed > 0 && self.completed + self.activation_unconfirmed > 0 {
            "firmware.status.partial"
        } else if self.failed > 0 {
            "firmware.status.failed"
        } else if self.activation_unconfirmed > 0 {
            "firmware.status.finished"
        } else {
            "firmware.status.completed"
        };
        let mut text = if self.active + self.queued == 0 && self.activation_unconfirmed > 0 {
            format!(
                "{} · {} {}",
                tr(status),
                tr("firmware.phase.completed"),
                self.completed
            )
        } else {
            format!(
                "{} · {} {}/{}",
                tr(status),
                tr("firmware.phase.completed"),
                self.completed,
                total
            )
        };
        for (label, count) in [
            ("firmware.phase.active", self.active),
            (
                "firmware.phase.activation_unconfirmed",
                self.activation_unconfirmed,
            ),
            ("firmware.phase.queued", self.queued),
            ("firmware.phase.failed", self.failed),
            ("firmware.phase.cancelled", self.cancelled),
        ] {
            if count > 0 {
                text.push_str(&format!(" · {} {}", tr(label), count));
            }
        }
        text
    }
}

impl FlashState {
    pub(super) fn progress_identity(&self, target: &OtaTargetProgress) -> Option<&ResultTarget> {
        self.operation_targets.iter().find(|item| {
            item.id == Some(target.id)
                && item
                    .receiver
                    .as_ref()
                    .is_some_and(|(key, _)| key == &target.receiver_key)
        })
    }

    pub(super) fn shows_results(&self, mode: Mode) -> bool {
        self.operation_origin == mode
            && !self.operation_results_dismissed
            && !self.results.is_empty()
    }

    pub(super) fn dismiss_results(&mut self, mode: Mode) {
        if self.operation_origin == mode {
            self.operation_results_dismissed = true;
            self.receiver_result_joined_at = None;
        }
    }

    pub(super) fn apply_joined_operation_results(
        &mut self,
        results: Vec<(ResultTarget, UpdateOutcome)>,
        joined_at: Instant,
    ) {
        // Per-volume terminal outcomes supersede a batch's last Writing message.
        // Retain ESB's structured terminal snapshots and active job progress.
        if self.operation_origin == Mode::Uf2 && !results.is_empty() {
            self.last_progress = None;
        }
        self.results = results;
        self.receiver_result_joined_at = self
            .results
            .iter()
            .any(|(target, outcome)| {
                target.id == Some(esb_ota::RECEIVER_ID)
                    && target.receiver_registry_key.is_some()
                    && matches!(
                        outcome,
                        UpdateOutcome::ActivationUnconfirmed(ReceiverActivationReport {
                            availability: ReceiverAvailability::Unconfirmed(_),
                            ..
                        })
                    )
            })
            .then_some(joined_at);
        self.operation_results_dismissed = false;
    }

    pub(in crate::application) fn reconcile_receiver_result_presence(
        &mut self,
        inventory: &crate::domain::Inventory,
        scan_started_at: Instant,
    ) -> bool {
        use crate::features::device_registry::model::DeviceKey;

        let Some(joined_at) = self.receiver_result_joined_at else {
            return false;
        };
        if scan_started_at <= joined_at {
            return false;
        }
        let mut changed = false;
        let mut pending = false;
        for (target, outcome) in &mut self.results {
            if target.id != Some(esb_ota::RECEIVER_ID) {
                continue;
            }
            let Some(key) = target.receiver_registry_key else {
                continue;
            };
            let UpdateOutcome::ActivationUnconfirmed(report) = outcome else {
                continue;
            };
            let ReceiverAvailability::Unconfirmed(diagnostic) = &mut report.availability else {
                continue;
            };
            let current = inventory.receivers.iter().find(|receiver| {
                receiver
                    .serial_number
                    .as_deref()
                    .and_then(DeviceKey::receiver_serial)
                    == Some(key)
            });
            if current.and_then(|receiver| DeviceKey::receiver(receiver, inventory)) == Some(key) {
                report.availability = ReceiverAvailability::Detected {
                    prior_diagnostic: std::mem::take(diagnostic),
                };
                changed = true;
            } else {
                pending = true;
            }
        }
        if !pending {
            self.receiver_result_joined_at = None;
        }
        changed
    }

    pub(super) fn floating_activity(&self, mode: Mode) -> bool {
        self.activity_origin == mode
            && (self.pending.is_some()
                || (self.job.is_some() && self.activity_kind != ActivityKind::Operation))
    }

    pub(super) fn running_operation(&self, mode: Mode) -> bool {
        self.activity_origin == mode
            && self.job.is_some()
            && self.activity_kind == ActivityKind::Operation
    }

    pub(super) fn shows_activity(&self, mode: Mode) -> bool {
        self.running_operation(mode)
            || (self.error_origin == mode && self.error.is_some())
            || (self.operation_origin == mode
                && self
                    .last_progress
                    .as_ref()
                    .is_some_and(|progress| progress.ota.is_empty()))
            || (mode == Mode::Esb
                && (self.query_error.is_some() || (self.query_cancelled && !self.querying())))
    }

    pub(super) fn prepare_action_results(&mut self, action: &Action) {
        if let Action::QueryGroups(plan) = action {
            self.preserve_query_card_layout = !plan.automatic;
            self.query_error = None;
            self.query_cancelled = false;
            // Keep a manual scoped retry's error card stable during lease handoff.
            // A successful result removes its own failure in apply_queries.
            if plan.incremental && !plan.automatic {
                return;
            }
            if plan.incremental {
                for (receiver, trackers, include_receiver) in &plan.groups {
                    if *include_receiver {
                        self.query_failures.remove(&format!(
                            "{}:{}",
                            receiver.key,
                            esb_ota::RECEIVER_ID
                        ));
                    }
                    for tracker in trackers {
                        self.query_failures
                            .remove(&format!("{}:{}", receiver.key, tracker.slot));
                    }
                }
            } else {
                self.query_failures.clear();
            }
            return;
        }
        self.operation_origin = action.mode();
        self.results.clear();
        self.receiver_result_joined_at = None;
        if let Action::EsbGroups(groups) = action {
            self.ota_presentation = OtaPresentation::Running;
            self.query_cancelled = false;
            for target in groups.iter().flat_map(|group| &group.targets) {
                self.esb_auto_query
                    .invalidate(&target.receiver.key, target.id);
            }
        }
        self.last_progress = None;
        self.operation_targets = match action {
            Action::EsbGroups(groups) => groups
                .iter()
                .flat_map(|group| &group.targets)
                .map(ResultTarget::ota)
                .collect(),
            _ => Vec::new(),
        };
        self.result_success = match action {
            Action::Uf2(..) => "firmware.result.copy_completed",
            Action::Enter(_) => "firmware.result.dfu_requested",
            Action::Reboot(_) => "firmware.result.exit_dfu_requested",
            _ => "firmware.result.operation_completed",
        };
        self.result_notes.clear();
    }

    pub(super) fn finish_job_progress(&mut self, progress: Option<Progress>) {
        if self.activity_kind == ActivityKind::Operation && self.activity_origin == Mode::Esb {
            self.finish_ota_operation();
        }
        if self.activity_kind == ActivityKind::Query {
            self.preserve_query_card_layout = false;
        }
        // Read-only work owns only its live progress, never a previous update's
        // retained outcome (including when a download fails or is cancelled).
        if self.activity_kind == ActivityKind::Operation && progress.is_some() {
            self.last_progress = progress;
        }
        self.activity_kind = ActivityKind::Preparation;
    }
}

impl Workbench {
    #[inline(never)]
    pub(super) fn flash_activity_toast(&self, cx: &mut Context<Self>) -> AnyElement {
        let status = div()
            .id("firmware-activity-message")
            .min_w_0()
            .max_h(px(120.))
            .overflow_y_scroll()
            .overflow_x_hidden()
            .whitespace_normal();
        #[cfg(test)]
        let status = status.test_support();
        let mut status = status;
        if self.flash.pending.is_some() {
            status = status.child(tr("firmware.status.wait_lease"));
        } else if let Some(progress) = self
            .flash
            .job
            .as_ref()
            .and_then(|job| job.progress.as_ref())
        {
            status = status.child(self.flash_progress(progress, cx));
        } else {
            status = status.child(tr("firmware.status.working"));
        }
        div()
            .id("firmware-activity-toast")
            .absolute()
            .right(px(12.))
            .bottom(px(
                if self.flash_mode() == Mode::Esb && self.flash.ota_shows_next() {
                    96.
                } else {
                    12.
                },
            ))
            .w(px(320.))
            .max_w(relative(0.9))
            .occlude()
            .p_3()
            .rounded_md()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().group_box)
            .text_color(cx.theme().foreground)
            .text_sm()
            .flex()
            .flex_col()
            .gap_2()
            .child(status)
            .child(
                div()
                    .flex()
                    .justify_end()
                    .child(self.flash_cancel_button(cx)),
            )
            .into_any_element()
    }

    fn flash_cancel_button(&self, cx: &mut Context<Self>) -> Button {
        Button::new("flash-cancel")
            .small()
            .label(tr("common.action.cancel"))
            .on_click(cx.listener(|this, _, _, cx| {
                this.flash.cancel_activity();
                if this.flash.pending.take().is_some() {
                    this.release_flash_lease();
                    this.flash.finish_job_progress(None);
                }
                cx.notify();
            }))
    }

    #[inline(never)]
    pub(super) fn flash_activity(&self, cx: &mut Context<Self>) -> AnyElement {
        let mode = self.flash_mode();
        let busy = self.flash.running_operation(mode);
        let mut page = div().flex().flex_col().gap_3();
        if let Some(job) = self.flash.job.as_ref().filter(|_| busy) {
            if let Some(progress) = &job.progress {
                page = page.child(self.flash_progress(progress, cx));
            } else {
                page = page.child(div().text_sm().child(tr("firmware.status.working")));
            }
        }
        if busy {
            page = page.child(self.flash_cancel_button(cx));
        }
        if let Some(error) = self
            .flash
            .error
            .as_ref()
            .filter(|_| self.flash.error_origin == mode)
        {
            page = page.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().danger)
                    .child(error.clone()),
            );
        }
        if mode == Mode::Esb {
            if let Some(error) = &self.flash.query_error {
                page = page.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(error.clone()),
                );
            }
            if self.flash.query_cancelled && !self.flash.querying() {
                page = page.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(tr("firmware.query.cancelled")),
                );
            }
        }
        if !busy
            && let Some(progress) = self
                .flash
                .last_progress
                .as_ref()
                .filter(|_| self.flash.operation_origin == mode)
            && progress.ota.is_empty()
        {
            page = page.child(div().text_sm().child(progress.message.clone()));
        }
        page.into_any_element()
    }

    #[inline(never)]
    fn flash_progress(&self, progress: &Progress, cx: &mut Context<Self>) -> AnyElement {
        if progress.ota.is_empty() {
            if self.flash.activity_kind == ActivityKind::Operation
                && matches!(self.flash.activity_origin, Mode::Uf2 | Mode::OpenDfu)
            {
                let percent = wired_transfer_percent(progress);
                let mut text = if progress.total > 0 {
                    let whole_percent = (u128::from(progress.completed) * 100
                        / u128::from(progress.total))
                    .min(100);
                    format!(
                        "{} · {} / {} B ({}%)",
                        progress.message, progress.completed, progress.total, whole_percent
                    )
                } else if progress.completed > 0 {
                    format!("{} · {} B", progress.message, progress.completed)
                } else {
                    progress.message.clone()
                };
                if progress.total > 0 && progress.completed >= progress.total {
                    text.push_str(" · ");
                    text.push_str(&tr("firmware.progress.waiting_result"));
                }
                let message = div()
                    .id("flash-progress-message")
                    .role(Role::Status)
                    .aria_label(text.clone())
                    .min_w_0()
                    .text_sm()
                    .whitespace_normal()
                    .child(text);
                #[cfg(test)]
                let message = message.test_support();
                let bar = ProgressBar::new("flash-progress-bar")
                    .small()
                    .loading(percent.is_none())
                    .value(percent.unwrap_or_default())
                    .accessibility_label(progress.message.clone());
                return div()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(message)
                    .child(bar)
                    .into_any_element();
            }
            let text = if progress.total > 0 {
                format!(
                    "{} · {} / {} ({:.0}%)",
                    progress.message,
                    progress.completed,
                    progress.total,
                    (progress.completed as f64 / progress.total as f64 * 100.).min(100.)
                )
            } else {
                progress.message.clone()
            };
            let message = div()
                .id("flash-progress-message")
                .role(Role::Status)
                .aria_label(text.clone())
                .text_sm()
                .child(text);
            #[cfg(test)]
            let message = message.test_support();
            return message.into_any_element();
        }
        let counts = OtaCounts::from_progress(&progress.ota);
        let mut panel = div()
            .flex()
            .flex_col()
            .gap_3()
            .child(div().text_sm().child(counts.summary(progress.ota.len())));
        for (key, targets) in progress_receiver_groups(&progress.ota) {
            let metadata = self.flash.operation_targets.iter().find(|target| {
                target
                    .receiver
                    .as_ref()
                    .is_some_and(|(receiver, _)| receiver == key)
            });
            let label = metadata
                .and_then(|target| target.receiver.as_ref())
                .map(|(_, label)| label.as_str())
                .unwrap_or(key);
            let label = self.registry_label(
                metadata.and_then(|target| target.receiver_registry_key.as_ref()),
                label,
            );
            let mut group = div()
                .flex()
                .flex_col()
                .gap_2()
                .child(div().text_sm().child(label));
            let mut outcomes = div().flex().flex_wrap().gap_2();
            let mut has_outcomes = false;
            for target in targets {
                if ota_terminal(target.phase) {
                    has_outcomes = true;
                    outcomes = outcomes.child(self.flash_ota_terminal_chip(target, cx));
                } else {
                    group = group.child(self.flash_ota_progress_card(target, cx));
                }
            }
            if has_outcomes {
                group = group.child(outcomes);
            }
            panel = panel.child(group);
        }
        panel.into_any_element()
    }

    fn flash_ota_terminal_chip(
        &self,
        target: &OtaTargetProgress,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let metadata = self.flash.progress_identity(target);
        let label = metadata
            .map(|item| self.registry_label(item.registry_key.as_ref(), item.label.clone()))
            .unwrap_or_else(|| {
                if target.id == esb_ota::RECEIVER_ID {
                    tr("common.device_role.receiver")
                } else {
                    format!("{} {}", tr("common.device_role.tracker"), target.id)
                }
            });
        let identity = metadata
            .map(|item| item.identity.clone())
            .unwrap_or_else(|| format!("{} · {label}", target.receiver_key));
        let tooltip = ota_terminal_tooltip(target, identity);
        let color = match target.phase {
            OtaPhase::Completed => cx.theme().success,
            OtaPhase::Failed => cx.theme().danger,
            _ => cx.theme().muted_foreground,
        };
        div()
            .id(SharedString::from(format!(
                "ota-live-result-{}-{}",
                target.receiver_key, target.id
            )))
            .px_2()
            .py_1()
            .rounded_md()
            .border_1()
            .border_color(color.opacity(0.45))
            .bg(color.opacity(0.10))
            .text_xs()
            .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
            .child(format!("{label} · {}", tr(ota_phase_label(target.phase))))
            .into_any_element()
    }

    #[inline(never)]
    fn flash_ota_progress_card(
        &self,
        target: &OtaTargetProgress,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let metadata = self.flash.progress_identity(target);
        let label = metadata
            .map(|item| self.registry_label(item.registry_key.as_ref(), item.label.clone()))
            .unwrap_or_else(|| {
                if target.id == esb_ota::RECEIVER_ID {
                    tr("common.device_role.receiver")
                } else {
                    format!("{} {}", tr("common.device_role.tracker"), target.id)
                }
            });
        let identity = metadata
            .map(|item| item.identity.clone())
            .unwrap_or_else(|| format!("{} · {}", target.receiver_key, label));
        let mut details = tr(ota_progress_label(target));
        if target.phase == OtaPhase::Transferring {
            if target.total > 0 {
                details.push_str(&format!(
                    " · {:.0}% · {:.1} / {:.1} KiB",
                    (target.completed as f64 / target.total as f64 * 100.).min(100.),
                    target.completed as f64 / 1024.,
                    target.total as f64 / 1024.
                ));
            }
            if let Some(rate) = target.bytes_per_second {
                details.push_str(&format!(" · {:.1} KiB/s", rate as f64 / 1024.));
            }
            if let Some(seconds) = target.eta_seconds {
                details.push_str(&format!(
                    " · {} {}:{:02}",
                    tr("firmware.progress.eta"),
                    seconds / 60,
                    seconds % 60
                ));
            }
            if target.bytes_per_second.is_none() {
                details.push_str(&format!(" · {}", tr("firmware.progress.waiting")));
            }
        }
        let mut card = div()
            .id(SharedString::from(format!(
                "ota-progress-{}-{}",
                target.receiver_key, target.id
            )))
            .min_w_0()
            .p_3()
            .rounded_md()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().group_box)
            .flex()
            .flex_col()
            .gap_2()
            .tooltip(move |window, cx| Tooltip::new(identity.clone()).build(window, cx))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .text_sm()
                    .child(label)
                    .child(div().text_color(cx.theme().muted_foreground).child(details)),
            );
        if target.phase == OtaPhase::Transferring && target.total > 0 {
            let fraction = (target.completed as f64 / target.total as f64).clamp(0., 1.) as f32;
            card = card.child(
                div()
                    .relative()
                    .h(px(7.))
                    .w_full()
                    .rounded_md()
                    .overflow_hidden()
                    .bg(cx.theme().border)
                    .child(
                        div()
                            .absolute()
                            .left_0()
                            .top_0()
                            .h_full()
                            .w(relative(fraction))
                            .bg(linear_gradient(
                                90.,
                                linear_color_stop(cx.theme().primary, 0.),
                                linear_color_stop(cx.theme().success, 1.),
                            )),
                    ),
            );
        }
        card.into_any_element()
    }

    #[inline(never)]
    fn flash_result_row(
        &self,
        target: &ResultTarget,
        result: &UpdateOutcome,
        cx: &Context<Self>,
    ) -> AnyElement {
        let mut row = div().flex().flex_col().gap_2();
        let title = div().id(SharedString::from(format!(
            "firmware-result-{}",
            target.key
        )));
        #[cfg(test)]
        let title = title.test_support();
        row = row.child(
            title
                .text_sm()
                .whitespace_normal()
                .when(
                    matches!(result, UpdateOutcome::Uf2CopyUnconfirmed(_)),
                    |row| row.text_color(cx.theme().warning),
                )
                .map(|row| match result {
                    UpdateOutcome::ActivationUnconfirmed(report) => {
                        let tooltip = activation_report_tooltip(report, target.identity.clone());
                        row.tooltip(move |window, cx| {
                            Tooltip::new(tooltip.clone()).build(window, cx)
                        })
                    }
                    _ => row,
                })
                .child(format!(
                    "{}: {}",
                    self.registry_label(target.registry_key.as_ref(), target.label.clone()),
                    match result {
                        UpdateOutcome::Completed => tr(self.flash.result_success),
                        UpdateOutcome::ActivationUnconfirmed(_) =>
                            tr("firmware.result.activation.unconfirmed"),
                        UpdateOutcome::Uf2CopyUnconfirmed(_) => tr("firmware.result.uf2.pending"),
                        UpdateOutcome::Failed(error) => error.clone(),
                    }
                )),
        );
        if let UpdateOutcome::ActivationUnconfirmed(report) = result {
            row = row.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .whitespace_normal()
                    .child(activation_report_label(report)),
            );
        }
        if let UpdateOutcome::Uf2CopyUnconfirmed(report) = result {
            let details = div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .whitespace_normal()
                .child(uf2_report_details(report));
            #[cfg(test)]
            let details = details
                .id(SharedString::from(format!(
                    "firmware-result-details-{}",
                    target.key
                )))
                .test_support();
            row = row
                .child(
                    div()
                        .text_sm()
                        .whitespace_normal()
                        .child(tr("firmware.result.uf2.check_version")),
                )
                .child(details);
        }
        if matches!(result, UpdateOutcome::Completed)
            && let Some(note) = self.flash.result_notes.get(&target.key)
        {
            row = row.child(div().text_sm().child(note.clone()));
        }
        row.into_any_element()
    }

    #[inline(never)]
    pub(super) fn flash_results(&self, cx: &mut Context<Self>) -> AnyElement {
        let mode = self.flash_mode();
        let panel = flash_panel(cx);
        #[cfg(test)]
        let panel = panel.id("flash-results-panel").test_support();
        let mut results = panel.child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(div().text_sm().child(tr("firmware.result.title")))
                .child(
                    Button::new("flash-results-dismiss")
                        .small()
                        .ghost()
                        .icon(IconName::Close)
                        .tooltip(tr("firmware.result.dismiss"))
                        .accessibility_label(tr("firmware.result.dismiss"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.flash.dismiss_results(mode);
                            cx.notify();
                        })),
                ),
        );
        let operation_results = if self.flash.shows_results(mode) {
            self.flash.results.as_slice()
        } else {
            &[]
        };
        if let Some(progress) = self
            .flash
            .last_progress
            .as_ref()
            .filter(|progress| self.flash.shows_results(mode) && !progress.ota.is_empty())
        {
            results = results.child(
                div()
                    .text_sm()
                    .child(OtaCounts::from_progress(&progress.ota).summary(progress.ota.len())),
            );
        }
        for (receiver_key, targets) in result_receiver_groups(operation_results) {
            let receiver_label = &targets[0].0.receiver.as_ref().unwrap().1;
            let mut group =
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(div().text_sm().child(self.registry_label(
                        targets[0].0.receiver_registry_key.as_ref(),
                        receiver_label.clone(),
                    )));
            let mut successes = div().flex().flex_wrap().gap_2();
            for (target, result) in &targets {
                if matches!(result, UpdateOutcome::Completed) {
                    let identity = target.identity.clone();
                    successes = successes.child(
                        div()
                            .id(SharedString::from(format!("ota-success-{}", target.key)))
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .border_1()
                            .border_color(cx.theme().success.opacity(0.45))
                            .bg(cx.theme().success.opacity(0.10))
                            .text_color(cx.theme().foreground)
                            .text_xs()
                            .tooltip(move |window, cx| {
                                Tooltip::new(identity.clone()).build(window, cx)
                            })
                            .child(self.registry_label(
                                target.registry_key.as_ref(),
                                target.label.clone(),
                            )),
                    );
                }
            }
            if targets
                .iter()
                .any(|(_, result)| matches!(result, UpdateOutcome::Completed))
            {
                group = group
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(tr("firmware.phase.completed")),
                    )
                    .child(successes);
            }
            for (target, result) in targets {
                match result {
                    UpdateOutcome::Completed => {}
                    UpdateOutcome::ActivationUnconfirmed(report) => {
                        let tooltip = activation_report_tooltip(report, target.identity.clone());
                        group = group.child(
                            div()
                                .id(SharedString::from(format!(
                                    "ota-unconfirmed-{}",
                                    target.key
                                )))
                                .flex()
                                .flex_col()
                                .gap_1()
                                .text_sm()
                                .whitespace_normal()
                                .tooltip(move |window, cx| {
                                    Tooltip::new(tooltip.clone()).build(window, cx)
                                })
                                .child(format!(
                                    "{} · {}",
                                    self.registry_label(
                                        target.registry_key.as_ref(),
                                        target.label.clone()
                                    ),
                                    tr("firmware.result.activation.unconfirmed")
                                ))
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(activation_report_label(report)),
                                ),
                        );
                    }
                    UpdateOutcome::Uf2CopyUnconfirmed(_) => {
                        group = group.child(self.flash_result_row(target, result, cx));
                    }
                    UpdateOutcome::Failed(error) => {
                        let identity = target.identity.clone();
                        let message = format!(
                            "{} · {}",
                            self.registry_label(target.registry_key.as_ref(), target.label.clone()),
                            error
                        );
                        let failure = div()
                            .id(SharedString::from(format!(
                                "ota-failure-{}-{}",
                                receiver_key, target.key
                            )))
                            .text_sm()
                            .text_color(cx.theme().danger)
                            .whitespace_normal()
                            .tooltip(move |window, cx| {
                                Tooltip::new(identity.clone()).build(window, cx)
                            });
                        #[cfg(test)]
                        let failure = failure
                            .role(Role::Label)
                            .aria_label(message.clone())
                            .test_support();
                        group = group.child(failure.child(message));
                    }
                }
            }
            results = results.child(group);
        }
        for (target, result) in operation_results {
            if target.receiver.is_none() {
                results = results.child(self.flash_result_row(target, result, cx));
            }
        }
        results.into_any_element()
    }
}
