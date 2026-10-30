use super::*;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum OtaStep {
    #[default]
    Devices,
    Firmware,
    Mapping,
    Confirm,
}

impl OtaStep {
    fn title(self) -> &'static str {
        match self {
            Self::Devices => "firmware.workflow.step.devices",
            Self::Firmware => "firmware.workflow.step.firmware",
            Self::Mapping => "firmware.workflow.step.mapping",
            Self::Confirm => "firmware.workflow.step.confirm",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum OtaPresentation {
    #[default]
    Configuring,
    Running,
    Finished,
}

impl FlashState {
    pub(super) fn ota_shows_next(&self) -> bool {
        self.ota_presentation == OtaPresentation::Configuring && self.ota_step != OtaStep::Confirm
    }

    pub(super) fn finish_ota_operation(&mut self) {
        if self.ota_presentation == OtaPresentation::Running {
            self.ota_presentation = OtaPresentation::Finished;
        }
    }

    pub(super) fn ota_new_update(&mut self) {
        if self.busy() || self.ota_presentation != OtaPresentation::Finished {
            return;
        }
        self.ota_presentation = OtaPresentation::Configuring;
        self.receiver_result_joined_at = None;
        self.ota_step = OtaStep::Devices;
        self.review = None;
        self.acknowledged = false;
        self.auto_requested = false;
    }

    pub(super) fn ota_selection_ready(&self) -> Result<(), String> {
        if self.unresolved_tracker_selection() {
            return Err(tr("firmware.error.tracker_info_required"));
        }
        if self.selected_targets.is_empty() {
            return Err(tr("firmware.error.query_select_first"));
        }
        for key in &self.selected_targets {
            let target = self
                .targets
                .iter()
                .find(|target| ota_target_key(target) == *key)
                .ok_or_else(|| tr("firmware.error.target_disappeared"))?;
            if !ota_supported(target) {
                return Err(tr("firmware.error.unsupported_method"));
            }
        }
        Ok(())
    }

    pub(super) fn ota_images_ready(&self) -> Result<(), String> {
        self.ota_selection_ready()?;
        for target in self
            .targets
            .iter()
            .filter(|target| self.selected_targets.contains(&ota_target_key(target)))
        {
            let image = self
                .assignments
                .get(&ota_assignment_key(target))
                .ok_or_else(|| tr("firmware.error.assign_every_group"))?;
            validate_ota_image(image, target)?;
        }
        Ok(())
    }

    // Only retreat automatically. Queries, defaults and downloads never advance
    // the user's step, and the existing frozen Action remains the sole authority.
    pub(super) fn reconcile_ota_step(&mut self, mode: Mode) -> bool {
        if mode != Mode::Esb || self.ota_presentation != OtaPresentation::Configuring {
            return false;
        }
        let step = if self.ota_step > OtaStep::Devices && self.ota_selection_ready().is_err() {
            OtaStep::Devices
        } else if self.ota_step > OtaStep::Firmware && self.ota_images_ready().is_err() {
            OtaStep::Firmware
        } else if self.ota_step == OtaStep::Confirm
            && !matches!(self.review, Some(Action::EsbGroups(_)))
        {
            OtaStep::Mapping
        } else {
            self.ota_step
        };
        if step != self.ota_step {
            self.ota_step = step;
            self.review = None;
            self.acknowledged = false;
            self.auto_requested = false;
            return true;
        }
        false
    }

    pub(super) fn ota_back(&mut self) {
        if self.ota_presentation != OtaPresentation::Configuring {
            return;
        }
        self.ota_step = match self.ota_step {
            OtaStep::Devices | OtaStep::Firmware => OtaStep::Devices,
            OtaStep::Mapping => OtaStep::Firmware,
            OtaStep::Confirm => OtaStep::Mapping,
        };
        self.review = None;
        self.acknowledged = false;
        self.auto_requested = false;
    }

    pub(super) fn ota_advance(&mut self) -> Result<(), String> {
        if self.ota_presentation != OtaPresentation::Configuring {
            return Ok(());
        }
        match self.ota_step {
            OtaStep::Devices => {
                self.ota_selection_ready()?;
                self.ota_step = OtaStep::Firmware;
            }
            OtaStep::Firmware => {
                self.ota_images_ready()?;
                self.ota_step = OtaStep::Mapping;
            }
            // Mapping requires the Workbench's current route checks first.
            OtaStep::Mapping | OtaStep::Confirm => return Ok(()),
        }
        self.review = None;
        self.acknowledged = false;
        self.auto_requested = false;
        Ok(())
    }

    pub(super) fn tracker_info(
        &self,
        tracker: &TrackerLink,
        receiver: &ReceiverEndpoint,
    ) -> Option<&esb_ota::Target> {
        let address = tracker
            .address
            .as_deref()
            .and_then(crate::domain::normalize_address)?;
        self.targets.iter().find(|target| {
            target.receiver.key == tracker.receiver_key
                && target.receiver.key == receiver.key
                && target.receiver.path == receiver.path
                && target.receiver.serial_number == receiver.serial_number
                && target.id == tracker.slot
                && target
                    .address
                    .as_deref()
                    .and_then(crate::domain::normalize_address)
                    .as_ref()
                    == Some(&address)
        })
    }
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum TrackerSection<'a> {
    Reported(&'a str, &'a str),
    Reading,
    Unknown,
    Error,
    Unsupported,
    Offline,
}

impl TrackerSection<'_> {
    pub(super) fn id(&self) -> String {
        match self {
            Self::Reported(version, build) => format!("reported-{version}-{build}"),
            Self::Reading => "reading".into(),
            Self::Unknown => "unknown".into(),
            Self::Error => "error".into(),
            Self::Unsupported => "unsupported".into(),
            Self::Offline => "offline".into(),
        }
    }

    pub(super) fn label(&self) -> String {
        match self {
            Self::Reported(version, build) => format!(
                "{} {version} · {}",
                tr("firmware.metadata.reported_firmware"),
                super::super::date_label::display_date(build)
                    .unwrap_or_else(|| (*build).to_owned())
            ),
            Self::Reading => tr("firmware.query.reading"),
            Self::Unknown => tr("firmware.query.not_queried"),
            Self::Error => tr("firmware.query.unavailable"),
            Self::Unsupported => tr("firmware.query.unsupported_method"),
            Self::Offline => tr("firmware.selection.offline"),
        }
    }

    pub(super) fn tooltip(&self) -> String {
        match self {
            Self::Reported(version, build) => format!(
                "{}: {version}\n{}: {build}",
                tr("firmware.metadata.version"),
                tr("firmware.metadata.build_date")
            ),
            Self::Offline => tr("firmware.selection.offline_help"),
            _ => self.label(),
        }
    }
}

pub(super) fn tracker_section<'a>(
    tracker: &TrackerLink,
    target: Option<&'a esb_ota::Target>,
    querying: bool,
    failed: bool,
) -> TrackerSection<'a> {
    // HID lease handoff empties the monitor, not the physical receiver roster.
    if querying && failed {
        return TrackerSection::Error;
    }
    if !tracker.online {
        return if querying {
            TrackerSection::Reading
        } else {
            TrackerSection::Offline
        };
    }
    if querying && target.is_none() {
        return TrackerSection::Reading;
    }
    if failed && !querying {
        return TrackerSection::Error;
    }
    match target {
        Some(target) if !ota_supported(target) => TrackerSection::Unsupported,
        Some(target) => TrackerSection::Reported(&target.info.version, &target.info.build_date),
        None => TrackerSection::Unknown,
    }
}

impl Workbench {
    #[inline(never)]
    pub(super) fn flash_esb_workflow(&self, cx: &mut Context<Self>) -> AnyElement {
        let step = self.flash.ota_step;
        let mut content = div()
            .id("ota-step-content")
            .flex_1()
            .min_h_0()
            .min_w_0()
            .overflow_y_scroll()
            .overflow_x_hidden()
            .pb(px(100.))
            .flex()
            .flex_col()
            .gap_3();
        // Retained outcomes and physical updates stay inline across steps.
        // Read-only work and reservation waits float at the firmware-page root.
        if self.flash.shows_activity(Mode::Esb) {
            content = content.child(
                div()
                    .min_w_0()
                    .flex_shrink_0()
                    .child(self.flash_activity(cx)),
            );
        }
        if self.flash.shows_results(Mode::Esb) {
            content = content.child(self.flash_results(cx));
        }
        content = if self.flash.ota_presentation != OtaPresentation::Configuring {
            content
        } else {
            match step {
                OtaStep::Devices => content.child(self.flash_esb_targets(cx)),
                OtaStep::Firmware => {
                    if let Some(error) = &self.flash.catalog_error {
                        content = content.child(div().text_sm().child(format!(
                            "{}: {error}",
                            tr("firmware.error.configuration_unavailable")
                        )));
                    }
                    content.child(self.flash_ota_firmware_step(cx))
                }
                OtaStep::Mapping => content.child(self.flash_ota_mapping(cx)),
                OtaStep::Confirm => {
                    if let Some(action) = &self.flash.review {
                        content.child(self.flash_confirmation(action, cx))
                    } else {
                        content
                    }
                }
            }
        };
        div()
            .relative()
            .flex_1()
            .h_full()
            .min_h_0()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_3()
            .child(self.flash_ota_navigation(cx))
            .child(content)
            .when(self.flash.ota_shows_next(), |page| {
                page.child(self.flash_ota_next(cx))
            })
            .into_any_element()
    }

    #[inline(never)]
    fn flash_ota_firmware_step(&self, cx: &mut Context<Self>) -> AnyElement {
        let selector = div()
            .id("ota-firmware-groups")
            .flex_basis(px(280.))
            .flex_grow_0()
            .flex_shrink_1()
            .min_w_0()
            .max_w_full()
            .child(self.flash_ota_assignments(cx));
        let chooser = div()
            .id("ota-firmware-chooser")
            .flex_basis(px(420.))
            .flex_grow_1()
            .flex_shrink_1()
            .min_w_0()
            .max_w_full()
            .flex()
            .flex_col()
            .gap_3()
            .child(self.flash_source_picker(cx))
            .child(self.flash_image_panel(cx));
        div()
            .min_w_0()
            .w_full()
            .flex_shrink_0()
            .flex()
            .flex_wrap()
            .items_start()
            .gap_3()
            .child(selector)
            .child(chooser)
            .into_any_element()
    }

    #[inline(never)]
    fn flash_ota_navigation(&self, cx: &mut Context<Self>) -> AnyElement {
        let step = self.flash.ota_step;
        let busy = self.flash.busy();
        let selected = self.flash.tracker_selection.len()
            + self
                .flash
                .targets
                .iter()
                .filter(|target| {
                    target.id == esb_ota::RECEIVER_ID
                        && self
                            .flash
                            .selected_targets
                            .contains(&ota_target_key(target))
                })
                .count();
        let mut header = div()
            .flex_shrink_0()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(tr(
                        match self.flash.ota_presentation {
                            OtaPresentation::Configuring => step.title(),
                            OtaPresentation::Running => "firmware.status.updating",
                            OtaPresentation::Finished => "firmware.workflow.finished",
                        },
                    ))),
            );
        let mut actions = div()
            .flex()
            .flex_wrap()
            .flex_shrink_0()
            .justify_end()
            .ml_auto()
            .gap_2();
        if self.flash.ota_presentation == OtaPresentation::Finished {
            actions = actions.child(
                Button::new("ota-new-update")
                    .h(px(36.))
                    .label(tr("firmware.workflow.new_update"))
                    .disabled(busy)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.flash.ota_new_update();
                        cx.notify();
                    })),
            );
        } else if self.flash.ota_presentation == OtaPresentation::Configuring
            && step != OtaStep::Devices
        {
            actions = actions.child(
                Button::new("ota-step-back")
                    .h(px(36.))
                    .label(tr("common.action.back"))
                    .disabled(busy)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.flash.ota_back();
                        cx.notify();
                    })),
            );
        }
        header = header.child(
            div()
                .w_full()
                .min_w_0()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .min_w_0()
                        .text_xs()
                        .whitespace_normal()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!(
                            "{}: {selected} · {}: {}",
                            tr("firmware.selection.selected"),
                            tr("firmware.workflow.groups"),
                            self.flash.group_generations.len()
                        )),
                )
                .child(actions),
        );
        header.into_any_element()
    }

    #[inline(never)]
    fn flash_ota_next(&self, cx: &mut Context<Self>) -> AnyElement {
        let ready = match self.flash.ota_step {
            OtaStep::Devices => self.flash.ota_selection_ready(),
            _ => self.flash.ota_images_ready(),
        };
        div()
            .id("ota-step-next-floating")
            .absolute()
            .right(px(16.))
            .bottom(px(16.))
            .h(px(48.))
            .flex()
            .occlude()
            .rounded_md()
            .bg(cx.theme().group_box)
            .shadow_md()
            .child(
                Button::new("ota-step-next")
                    .h(px(48.))
                    .min_w(px(160.))
                    .text_base()
                    .font_weight(FontWeight::SEMIBOLD)
                    .icon(IconName::ArrowRight)
                    .primary()
                    .when(!cx.theme().is_dark(), |button| button.outline())
                    .label(tr("firmware.workflow.next"))
                    .disabled(self.flash.busy() || ready.is_err())
                    .when_some(ready.as_ref().err(), |button, error| {
                        button.tooltip(error.clone())
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        if this.flash.busy()
                            || this.flash_mode() != Mode::Esb
                            || !this.flash.ota_shows_next()
                        {
                            return;
                        }
                        let result = if this.flash.ota_step == OtaStep::Mapping {
                            this.selected_flash_action().map(|action| {
                                this.review_flash(action);
                                this.flash.ota_step = OtaStep::Confirm;
                            })
                        } else {
                            this.flash.ota_advance()
                        };
                        if let Err(error) = result {
                            this.flash.set_error(Mode::Esb, error);
                        }
                        cx.notify();
                    })),
            )
            .into_any_element()
    }

    #[inline(never)]
    fn flash_ota_mapping(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut groups = BTreeMap::<(String, String), Vec<&esb_ota::Target>>::new();
        for target in self.flash.targets.iter().filter(|target| {
            self.flash
                .selected_targets
                .contains(&ota_target_key(target))
        }) {
            groups
                .entry((target.receiver.key.clone(), ota_assignment_key(target)))
                .or_default()
                .push(target);
        }
        let mut panel = div()
            .min_w_0()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .whitespace_normal()
                    .child(tr("firmware.workflow.version_comparison_help")),
            );
        for ((receiver, assignment), targets) in groups {
            let first = targets[0];
            let mut group = flash_panel(cx).child(
                div()
                    .text_sm()
                    .whitespace_normal()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(format!(
                        "{} · {} · {} ({})",
                        self.frozen_receiver_label(&first.receiver, first.receiver.label.clone()),
                        first.info.board_target,
                        ota_role_label(first),
                        targets.len()
                    )),
            );
            let mut reports = BTreeMap::<(&str, &str), Vec<&esb_ota::Target>>::new();
            for target in targets {
                reports
                    .entry((&target.info.version, &target.info.build_date))
                    .or_default()
                    .push(target);
            }
            let mut installed = div()
                .min_w_0()
                .max_w_full()
                .flex_basis(px(300.))
                .flex_grow_1()
                .flex_shrink_1()
                .flex()
                .flex_col()
                .gap_2();
            for ((version, build), mut members) in reports {
                members.sort_by_key(|target| target.id);
                installed = installed.child(div().text_sm().whitespace_normal().child(format!(
                    "{}: {version} · {}: {build} ({})",
                    tr("firmware.metadata.reported_firmware"),
                    tr("firmware.metadata.build_date"),
                    members.len()
                )));
                for target in members {
                    installed = installed.child(div().text_xs().whitespace_normal().child(
                        if target.id == esb_ota::RECEIVER_ID {
                            self.frozen_receiver_label(
                                &target.receiver,
                                tr("common.device_role.receiver"),
                            )
                        } else {
                            self.address_label(
                                target.address.as_deref(),
                                format!(
                                    "{} #{} · {}",
                                    tr("common.device_role.tracker"),
                                    target.id,
                                    target.address.as_deref().unwrap_or("?")
                                ),
                            )
                        },
                    ));
                }
            }
            let mut chosen = div()
                .min_w_0()
                .max_w_full()
                .flex_basis(px(360.))
                .flex_grow_1()
                .flex_shrink_1()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .text_xs()
                        .child(format!("→ {}", tr("firmware.workflow.selected_image"))),
                );
            if let Some(image) = self.flash.assignments.get(&assignment) {
                chosen = chosen.child(self.flash_image_identity(
                    image,
                    &format!("mapping-{receiver}-{assignment}"),
                    cx,
                ));
            }
            group = group.child(
                div()
                    .min_w_0()
                    .w_full()
                    .flex()
                    .flex_wrap()
                    .items_start()
                    .gap_3()
                    .child(installed)
                    .child(chosen),
            );
            group = group.child(
                div().min_w_0().flex().justify_end().child(
                    Button::new(SharedString::from(format!(
                        "ota-edit-{receiver}-{assignment}"
                    )))
                    .small()
                    .label(tr("firmware.workflow.edit_image"))
                    .disabled(self.flash.busy())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.flash.reconcile_groups(Some(assignment.clone()));
                        this.flash.ota_step = OtaStep::Firmware;
                        this.flash.review = None;
                        this.flash.acknowledged = false;
                        this.flash.auto_requested = false;
                        cx.notify();
                    })),
                ),
            );
            panel = panel.child(group);
        }
        panel.into_any_element()
    }
}
