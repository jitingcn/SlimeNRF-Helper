use super::*;

#[cfg(test)]
mod tests;

impl Workbench {
    #[inline(never)]
    pub(super) fn flash_columns(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.flash_mode() == Mode::Esb {
            return self.flash_esb_workflow(cx);
        }
        let left = div()
            .id("firmware-target-column")
            .flex_1()
            .min_w_0()
            .min_h_0()
            .h_full()
            .overflow_y_scroll()
            .overflow_x_hidden()
            .child(self.flash_targets(cx));
        div()
            .flex()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .gap_3()
            .child(left)
            .child(self.flash_firmware_content(cx))
            .into_any_element()
    }

    #[inline(never)]
    fn flash_firmware_content(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut page = div()
            .id("firmware-image-column")
            .flex_1()
            .h_full()
            .min_h_0()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_3()
            .overflow_y_scroll()
            .overflow_x_hidden();
        if let Some(error) = &self.flash.catalog_error {
            page = page.child(div().text_sm().child(format!(
                "{}: {error}",
                tr("firmware.error.configuration_unavailable")
            )));
        }
        if self.flash_mode() == Mode::Uf2 {
            page = page.child(self.flash_image_categories(cx));
        }
        page = page
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(tr("firmware.step.choose_firmware")),
            )
            .child(self.flash_source_picker(cx))
            .child(self.flash_image_panel(cx));
        if self.flash_mode() == Mode::Uf2 {
            page = page.child(self.flash_auto_copy(cx));
        }
        page = page.child(self.flash_review_controls(cx));
        if let Some(action) = self.flash.review.as_ref() {
            page = page.child(self.flash_confirmation(action, cx));
        }
        if self.flash.shows_activity(self.flash_mode()) {
            page = page.child(self.flash_activity(cx));
        }
        if self.flash.shows_results(self.flash_mode()) {
            page = page.child(self.flash_results(cx));
        }
        page.into_any_element()
    }

    #[inline(never)]
    pub(super) fn flash_mode_tabs(&self, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.flash.busy();
        let mode = self.flash_mode();
        let modes = if self.active_page == FeaturePage::Uf2 {
            [
                (Mode::Uf2, "firmware.mode.uf2"),
                (Mode::OpenDfu, "firmware.mode.opendfu"),
            ]
        } else {
            [
                (Mode::Esb, "firmware.mode.esb"),
                (Mode::Ble, "firmware.mode.ble"),
            ]
        };
        let mut tabs = div().flex().gap_2();
        for (index, (value, label)) in modes.into_iter().enumerate() {
            if value == Mode::Ble && !self.preferences.advanced_features {
                continue;
            }
            tabs = tabs.child(
                context_button(
                    Button::new(("flash-mode", index))
                        .label(tr(label))
                        .disabled(busy),
                    mode == value,
                    cx,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    if this.flash.busy()
                        || (value == Mode::Ble && !this.preferences.advanced_features)
                    {
                        return;
                    }
                    this.flash.transition_mode(value);
                    this.sync_esb_page();
                    cx.notify();
                })),
            );
        }
        let explanation = match mode {
            Mode::Uf2 => "firmware.mode.uf2_help",
            Mode::OpenDfu => "firmware.mode.opendfu_help",
            Mode::Esb => "firmware.mode.esb_help",
            Mode::Ble => "firmware.mode.ble_help",
        };
        div()
            .flex()
            .flex_col()
            .gap_2()
            .flex_shrink_0()
            .child(tabs)
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .whitespace_normal()
                    .child(tr(explanation)),
            )
            .into_any_element()
    }

    #[inline(never)]
    fn flash_image_categories(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut panel = flash_panel(cx).child(div().text_sm().child(tr("firmware.category.title")));
        let mut tabs = div().min_w_0().flex().flex_wrap().gap_2();
        for (index, (category, label)) in [
            (ImageCategory::Application, "firmware.category.application"),
            (ImageCategory::Bootloader, "firmware.category.bootloader"),
        ]
        .into_iter()
        .enumerate()
        {
            tabs = tabs.child(
                context_button(
                    Button::new(("firmware-category", index))
                        .label(tr(label))
                        .disabled(self.flash.busy()),
                    self.flash.category == category,
                    cx,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.flash.select_category(category);
                    cx.notify();
                })),
            );
        }
        panel = panel.child(tabs);
        if self.flash.category == ImageCategory::Bootloader {
            panel = panel.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().warning)
                    .whitespace_normal()
                    .child(tr("firmware.confirmation.bootloader_risk")),
            );
        }
        panel.into_any_element()
    }

    #[inline(never)]
    fn flash_image_details(&self, image: &Image, cx: &App) -> AnyElement {
        let mut details = div().flex().flex_col().gap_2();
        details = details.child(self.flash_image_identity(image, "chosen", cx));
        if let Err(error) = self.validate_page_image(image) {
            details = details.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().warning)
                    .whitespace_normal()
                    .child(error),
            );
        }
        let hints = &image.hints;
        if hints.is_empty() && image.local {
            details = details.child(
                div()
                    .text_sm()
                    .child(tr("firmware.image.unknown_local_target")),
            );
        }
        for hint in hints.iter() {
            details = details.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .whitespace_normal()
                    .child(format!(
                        "{} · {}",
                        hint.board_target,
                        firmware_variant_label(&hint.variant)
                    )),
            );
        }
        if image.name.to_ascii_lowercase().contains("datacollect") {
            details = details.child(
                div()
                    .text_sm()
                    .child(tr("firmware.image.datacollect_interfaces")),
            );
        }
        details.into_any_element()
    }

    #[inline(never)]
    pub(super) fn flash_image_identity(
        &self,
        image: &Image,
        context: &str,
        cx: &App,
    ) -> AnyElement {
        let mut panel = div()
            .flex()
            .flex_col()
            .gap_1()
            .min_w_0()
            .w_full()
            .child(
                div()
                    .id(SharedString::from(format!("flash-image-name-{context}")))
                    .min_w_0()
                    .w_full()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .truncate()
                    .tooltip({
                        let identity = format!(
                            "{}\n{}\nSHA-256 {}",
                            image.name, image.provenance, image.digest
                        );
                        move |window, cx| Tooltip::new(identity.clone()).build(window, cx)
                    })
                    .child(
                        image
                            .name
                            .rsplit(['/', '\\'])
                            .next()
                            .unwrap_or(&image.name)
                            .to_owned(),
                    ),
            )
            .child(
                div()
                    .min_w_0()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(clipped(
                        format!("flash-image-source-{context}"),
                        image.provenance.clone(),
                    )),
            );
        if self.flash_mode() != Mode::Esb || self.preferences.advanced_features {
            panel = panel.child(
                div()
                    .min_w_0()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(clipped(
                        format!("flash-image-sha-{context}"),
                        format!("SHA-256 {}", image.digest),
                    )),
            );
        }
        panel.into_any_element()
    }

    #[inline(never)]
    fn flash_image_variant(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.flash.busy();
        let image = &self.flash.images[index];
        let mut row = div().min_w_0().flex_shrink_0().flex().flex_col().gap_2();
        let destination = self.flash.images_destination.clone();
        let chosen = image.clone();
        let variant_id = match &destination {
            ImageDestination::Global => {
                format!("image-variant-global-{:?}-{}", image.name, image.digest)
            }
            ImageDestination::Ota { key, generation } => format!(
                "image-variant-ota-{key:?}-{generation}-{:?}-{}",
                image.name, image.digest
            ),
        };
        let selected = self
            .flash
            .displayed_image(self.flash_mode())
            .is_some_and(|value| value.digest == image.digest && value.name == image.name);
        row = row.child(
            context_button(
                Button::new(SharedString::from(variant_id))
                    .small()
                    .w_full()
                    .min_w_0()
                    .accessibility_label(format!("Choose firmware {}", image.name))
                    .tooltip(image.name.clone())
                    .child(
                        div()
                            .w_full()
                            .min_w_0()
                            .truncate()
                            .text_left()
                            .child(image.name.clone()),
                    )
                    .disabled(busy || image.validation_error.is_some()),
                selected,
                cx,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                if this.flash.destination(this.flash_mode()).as_ref() == Some(&destination)
                    && let Err(error) = this.flash.choose_image(&destination, chosen.clone())
                {
                    this.flash.set_error(this.flash_mode(), error);
                }
                cx.notify();
            })),
        );
        if let Some(error) = &image.validation_error {
            row = row.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().danger)
                    .child(error.clone()),
            );
        }
        row.into_any_element()
    }

    #[inline(never)]
    pub(super) fn flash_source_picker(&self, cx: &mut Context<Self>) -> AnyElement {
        let remote = self.flash.remote_view && self.flash_mode() != Mode::Ble;
        let mut panel = div().min_w_0().flex().flex_col().gap_2();
        if self.flash_mode() != Mode::Ble {
            panel = panel.child(
                TabBar::new("firmware-source-tabs")
                    .selected_index(usize::from(remote))
                    .child(Tab::new().label(tr("firmware.source.local")))
                    .child(Tab::new().label(tr("firmware.source.remote")))
                    .on_click(cx.listener(|this, index: &usize, _, cx| {
                        this.flash.show_remote(*index == 1);
                        cx.notify();
                    })),
            );
        }
        panel
            .child(if remote {
                self.flash_remote_picker(cx)
            } else {
                self.flash_local_picker(cx)
            })
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .whitespace_normal()
                    .child(tr("firmware.image.drop_hint")),
            )
            .into_any_element()
    }

    #[inline(never)]
    fn flash_local_picker(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut panel = flash_panel(cx)
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .whitespace_normal()
                    .child(tr(if cfg!(feature = "embed-firmware") {
                        "firmware.source.bundled_help"
                    } else {
                        "firmware.source.configured_help"
                    })),
            )
            .child(
                Button::new("flash-local")
                    .label(tr("firmware.action.choose_local"))
                    .disabled(self.flash.busy())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.choose_firmware(cx);
                        cx.notify();
                    })),
            );
        match self.flash_mode() {
            Mode::Esb => panel = panel.child(self.flash_ota_preset(cx)),
            Mode::Uf2 => {
                if let Some(catalog) = &self.flash.catalog {
                    for (index, volume) in self.flash.volumes.iter().enumerate() {
                        if let Some(preset) =
                            catalog.preset_for_volume(&volume.label, volume.board_id.as_deref())
                        {
                            panel = panel.child(self.flash_uf2_presets(index, preset, cx));
                        }
                    }
                }
            }
            Mode::OpenDfu => {
                if let Some(catalog) = &self.flash.catalog {
                    for (index, target) in catalog.opendfu_targets.iter().enumerate() {
                        panel = panel.child(
                            self.flash_configured_button(
                                ("opendfu-preset", index).into(),
                                format!("{} · {}", tr("firmware.source.configured"), target.name)
                                    .into(),
                                &target.firmware,
                                cx,
                            ),
                        );
                    }
                }
            }
            Mode::Ble => {
                if let Some(catalog) = &self.flash.catalog {
                    for (index, target) in catalog.ble_ota_targets.iter().enumerate() {
                        panel = panel.child(
                            self.flash_configured_button(
                                ("ble-preset", index).into(),
                                format!(
                                    "{} · {}",
                                    tr("firmware.source.configured"),
                                    target.display_name
                                )
                                .into(),
                                &target.package,
                                cx,
                            ),
                        );
                    }
                }
            }
        }
        panel.into_any_element()
    }

    #[inline(never)]
    pub(super) fn flash_image_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        let mode = self.flash_mode();
        let mut image_panel = flash_panel(cx).child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .child(tr("firmware.image.selected")),
        );
        if let Some(image) = self.flash.displayed_image(mode) {
            image_panel = image_panel.child(self.flash_image_details(image, cx));
        } else {
            image_panel = image_panel.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .whitespace_normal()
                    .child(tr("firmware.image.choose_help")),
            );
        }
        if self.flash.destination(mode).as_ref() == Some(&self.flash.images_destination)
            && (self.flash.images.len() > 1
                || self
                    .flash
                    .images
                    .iter()
                    .any(|image| image.validation_error.is_some()))
        {
            image_panel = image_panel.child(self.flash_image_choices(cx));
        }
        if mode == Mode::Ble {
            image_panel =
                image_panel.child(div().text_sm().child(tr("firmware.error.ble_zip_required")));
        }
        image_panel.into_any_element()
    }

    #[inline(never)]
    fn flash_image_choices(&self, cx: &mut Context<Self>) -> AnyElement {
        let targets = self.flash_filter_targets();
        let mut list = div()
            .id("firmware-image-choices")
            .min_w_0()
            .max_h(px(144.))
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap_1();
        let mut count = 0;
        for (index, image) in self.flash.images.iter().enumerate() {
            if self.candidate_allowed(&image.name, &image.hints, false)
                && firmware_filter_matches(
                    &image.hints,
                    &source::variant(&image.name),
                    &targets,
                    self.flash.matched_only,
                    if self.flash_mode() == Mode::Uf2
                        && self.flash.category == ImageCategory::Bootloader
                    {
                        None
                    } else {
                        self.flash.variant_filter.as_deref()
                    },
                )
            {
                count += 1;
                list = list.child(self.flash_image_variant(index, cx));
            }
        }
        if count == 0 {
            list = list.child(div().text_sm().child(tr("firmware.image.no_compatible")));
        }
        list.overflow_y_scroll().into_any_element()
    }

    #[inline(never)]
    fn flash_auto_copy(&self, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.flash.busy();
        Checkbox::new("uf2-auto")
            .label(tr("firmware.uf2.auto_copy"))
            .checked(self.flash.auto || self.flash.auto_requested)
            .disabled(busy)
            .on_click(cx.listener(|this, checked: &bool, _, cx| {
                if !*checked {
                    this.flash.auto = false;
                    this.flash.auto_requested = false;
                } else {
                    match this.selected_flash_action() {
                        Ok(action) => {
                            this.review_flash(action);
                            this.flash.auto_requested = true;
                        }
                        Err(error) => this.flash.set_error(this.flash_mode(), error),
                    }
                }
                cx.notify();
            }))
            .into_any_element()
    }

    #[inline(never)]
    pub(super) fn flash_ota_assignments(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut panel = flash_panel(cx).child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .child(tr("firmware.assignment.title")),
        );
        let groups = self.ota_assignment_groups();
        if groups.is_empty() {
            panel = panel.child(
                div()
                    .text_sm()
                    .whitespace_normal()
                    .child(tr("firmware.assignment.select_devices")),
            );
        }
        for group in groups {
            panel = panel.child(self.flash_ota_assignment_row(&group, cx));
        }
        if self.flash.unresolved_tracker_selection() {
            panel = panel.child(
                div()
                    .text_sm()
                    .whitespace_normal()
                    .text_color(cx.theme().muted_foreground)
                    .child(tr(if self.flash.querying() {
                        "firmware.query.reading"
                    } else {
                        "firmware.error.tracker_info_required"
                    })),
            );
        }
        panel.into_any_element()
    }

    #[inline(never)]
    fn flash_ota_assignment_row(
        &self,
        group: &OtaAssignmentView,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = group.key.clone();
        let selected = self.flash.active_group.as_ref() == Some(&key);
        let status = if group.image_name.is_some() {
            "firmware.assignment.selected"
        } else {
            "firmware.assignment.empty"
        };
        let tooltip = format!(
            "{}\n{}",
            group.board_target,
            group.image_name.clone().unwrap_or_else(|| tr(status))
        );
        context_button(
            Button::new(SharedString::from(format!("ota-group-{key}")))
                .small()
                .h_auto()
                .w_full()
                .min_w_0()
                .p_2()
                .accessibility_label(format!("Choose firmware for {} · {tooltip}", group.label))
                .tooltip(tooltip)
                .child(
                    div()
                        .w_full()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .text_left()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .whitespace_normal()
                                .child(group.label.clone()),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .whitespace_normal()
                                .child(if self.preferences.advanced_features {
                                    format!(
                                        "{} · {}: {}",
                                        tr(status),
                                        tr("firmware.metadata.bootloader"),
                                        group.bootloader
                                    )
                                } else {
                                    tr(status)
                                }),
                        ),
                ),
            selected,
            cx,
        )
        .on_click(cx.listener(move |this, _, _, cx| {
            this.flash.reconcile_groups(Some(key.clone()));
            cx.notify();
        }))
        .into_any_element()
    }

    #[inline(never)]
    fn flash_ota_preset(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(key) = self.flash.active_group.as_deref() else {
            return div().into_any_element();
        };
        let mut panel = div().flex().flex_col().gap_2().min_w_0();
        let path = self.flash.internal_image_resource(key);
        if path
            .as_ref()
            .is_ok_and(|path| path.is_intentionally_omitted())
        {
            panel = panel.child(
                div()
                    .text_sm()
                    .whitespace_normal()
                    .child(tr("firmware.source.not_embedded")),
            );
        }
        let configured = self.flash.internal_image_target(key).ok();
        let target_name = configured
            .and_then(|target| target.name.as_deref())
            .unwrap_or(key);
        if let Ok(path) = &path {
            panel = panel.child(self.flash_configured_button(
                "ota-internal-firmware".into(),
                format!("{} · {target_name}", tr("firmware.source.internal")).into(),
                path,
                cx,
            ));
        }
        if let Some(target) = configured {
            for (index, resource) in target.additional_firmware.iter().enumerate() {
                if path.as_ref().is_ok_and(|default| *default == resource) {
                    continue;
                }
                panel = panel.child(self.flash_configured_button(
                    ("ota-configured-firmware", index).into(),
                    format!("{} · {target_name}", tr("firmware.source.configured")).into(),
                    resource,
                    cx,
                ));
            }
        }
        if path
            .as_ref()
            .is_ok_and(|path| matches!(path, Resource::Bundled(_)))
        {
            panel = panel.child(
                div()
                    .id("ota-internal-caption")
                    .text_xs()
                    .whitespace_normal()
                    .text_color(cx.theme().muted_foreground)
                    .child(tr("firmware.source.bundled_version_unknown"))
                    .tooltip(|window, cx| {
                        Tooltip::new(tr("firmware.source.bundled_version_help")).build(window, cx)
                    }),
            );
        }
        let error = self
            .flash
            .ota_defaults
            .get(key)
            .and_then(|state| state.error.as_deref())
            .or_else(|| path.as_ref().err().map(String::as_str));
        if let Some(error) = error {
            panel = panel.child(div().text_sm().whitespace_normal().child(format!(
                "{}: {error}",
                tr("firmware.source.internal_unavailable"),
            )));
        }
        panel.into_any_element()
    }

    #[inline(never)]
    pub(super) fn flash_firmware_filters(&self, cx: &mut Context<Self>) -> AnyElement {
        let known = !self.flash_filter_targets().is_empty();
        let mut panel = div().min_w_0().flex().flex_col().gap_2().child(
            Checkbox::new("firmware-matched")
                .label(tr("firmware.filter.matching_boards"))
                .checked(self.flash.matched_only && known)
                .disabled(self.flash.busy() || !known)
                .on_click(cx.listener(|this, checked: &bool, _, cx| {
                    this.flash.matched_only = *checked;
                    cx.notify();
                })),
        );
        if !known {
            panel = panel.child(
                div()
                    .text_xs()
                    .child(tr("firmware.filter.known_target_required")),
            );
        }
        panel = panel.child(
            div()
                .min_w_0()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                .child(self.flash_variant_filters(cx))
                .child(
                    Button::new("firmware-clear-filters")
                        .small()
                        .ghost()
                        .label(tr("firmware.filter.clear"))
                        .disabled(
                            self.flash.busy()
                                || (!self.flash.matched_only
                                    && self.flash.variant_filter.is_none()),
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.flash.matched_only = false;
                            this.flash.variant_filter = None;
                            cx.notify();
                        })),
                ),
        );
        panel.into_any_element()
    }

    #[inline(never)]
    fn flash_variant_filters(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.flash_mode() == Mode::Uf2 && self.flash.category == ImageCategory::Bootloader {
            return div().into_any_element();
        }
        let mut row = div().min_w_0().flex().flex_wrap().gap_1();
        let targets = self.flash_filter_targets();
        let receivers_allowed = targets.is_empty()
            || targets
                .iter()
                .any(|(_, kind)| *kind != Some(board_catalog::EsbOtaKind::Tracker));
        for (index, variant) in ["Standard", "DataCollect CDC"].into_iter().enumerate() {
            if variant == "DataCollect CDC" && !receivers_allowed {
                continue;
            }
            row = row.child(
                context_button(
                    Button::new(("firmware-variant-filter", index))
                        .small()
                        .label(firmware_variant_label(variant).into_owned())
                        .disabled(self.flash.busy()),
                    self.flash.variant_filter.as_deref() == Some(variant),
                    cx,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.flash.variant_filter = Some(variant.to_string());
                    cx.notify();
                })),
            );
        }
        row.into_any_element()
    }
}
