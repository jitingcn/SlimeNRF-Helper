use super::*;
use gpui_kit::component::input::Input;

fn artifact_firmware_hints(
    catalog: &board_catalog::Catalog,
    artifact: &source::Artifact,
) -> Vec<board_catalog::FirmwareHint> {
    if artifact.version.channel == source::SourceChannel::BootloaderCi {
        return catalog.match_bootloader_board(&artifact.name);
    }
    let mut hints = catalog.match_filename(&artifact.name);
    if artifact.version.channel.is_release()
        || !hints.is_empty()
        || artifact
            .name
            .rsplit_once('.')
            .is_some_and(|(_, extension)| {
                ["uf2", "hex", "bin", "zip"]
                    .iter()
                    .any(|known| extension.eq_ignore_ascii_case(known))
            })
    {
        return hints;
    }
    // CI labels retain the complete firmware stem, including version/profile suffixes.
    // Extracted members are independently matched by their actual filenames.
    let mut filename = String::with_capacity(artifact.name.len() + 4);
    filename.push_str(&artifact.name);
    for extension in [".uf2", ".hex"] {
        filename.truncate(artifact.name.len());
        filename.push_str(extension);
        for hint in catalog.match_filename(&filename) {
            if !hints.contains(&hint) {
                hints.push(hint);
            }
        }
    }
    hints
}

impl FlashState {
    pub(super) fn show_remote(&mut self, remote: bool) {
        // A source tab is presentation only: preserve the frozen update and all caches.
        self.remote_view = remote;
    }

    pub(super) fn apply_latest(
        // The caller's request generation is checked before this cache update.
        &mut self,
        mode: Mode,
        destination: ImageDestination,
        version: source::Version,
    ) {
        if self.remote_current(mode)
            && self.remote_destination == destination
            && self.channel == version.channel
        {
            self.latest_version = Some((version.channel, version.id));
            self.versions = vec![version];
            self.artifacts.clear();
            self.selected_version = None;
        }
    }

    pub(super) fn is_latest(&self, mode: Mode, version: &source::Version) -> bool {
        self.remote_current(mode)
            && self.channel == version.channel
            && self.latest_version == Some((version.channel, version.id))
    }

    fn remote_sources(&self) -> Option<Arc<crate::remote_config::RemoteConfig>> {
        if !self.catalog_loaded || self.catalog_error.is_some() {
            return None;
        }
        self.catalog
            .as_ref()
            .map(|catalog| Arc::clone(&catalog.remote_firmware))
    }

    fn remote_category_for(&self, mode: Mode) -> ImageCategory {
        if mode == Mode::Uf2 {
            self.category
        } else {
            ImageCategory::Application
        }
    }

    fn remote_current(&self, mode: Mode) -> bool {
        self.destination(mode).as_ref() == Some(&self.remote_destination)
            && self.remote_category == self.remote_category_for(mode)
            && self.channel.is_bootloader() == (self.remote_category == ImageCategory::Bootloader)
    }

    fn source_channel(&self, mode: Mode) -> source::SourceChannel {
        if self.remote_current(mode) {
            self.channel
        } else if self.remote_category_for(mode) == ImageCategory::Bootloader {
            source::SourceChannel::BootloaderRelease
        } else {
            self.application_channel
        }
    }

    fn clear_remote(&mut self) {
        self.remote_generation += 1;
        self.versions.clear();
        self.artifacts.clear();
        self.selected_version = None;
        self.latest_version = None;
    }

    pub(super) fn select_category(&mut self, category: ImageCategory) {
        if self.category == category {
            return;
        }
        self.category = category;
        self.review = None;
        self.acknowledged = false;
        self.auto = false;
        self.auto_requested = false;
        self.images.clear();
        self.variant_filter = None;
        self.clear_remote();
        self.remote_category = category;
        self.channel = if category == ImageCategory::Bootloader {
            source::SourceChannel::BootloaderRelease
        } else {
            self.application_channel
        };
    }

    pub(super) fn prepare_remote(&mut self, mode: Mode, destination: &ImageDestination) {
        self.explicit_image_intent(destination);
        let category = self.remote_category_for(mode);
        if self.remote_destination != *destination || self.remote_category != category {
            self.clear_remote();
            self.remote_category = category;
            self.channel = if category == ImageCategory::Bootloader {
                source::SourceChannel::BootloaderRelease
            } else {
                self.application_channel
            };
            self.remote_destination = destination.clone();
        }
    }

    pub(super) fn select_channel(&mut self, channel: source::SourceChannel) {
        if channel.is_bootloader() != (self.remote_category == ImageCategory::Bootloader) {
            return;
        }
        self.channel = channel;
        if !channel.is_bootloader() {
            self.application_channel = channel;
        }
        self.clear_remote();
        self.variant_filter = None;
    }

    fn remote_request(&self, mode: Mode) -> RemoteRequest {
        RemoteRequest {
            mode,
            destination: self.remote_destination.clone(),
            category: self.remote_category,
            channel: self.channel,
            generation: self.remote_generation,
            version: self.selected_version,
        }
    }

    fn accepts_remote(&self, mode: Mode, request: &RemoteRequest) -> bool {
        request.mode == mode
            && self.remote_current(mode)
            && self.destination_exists(&request.destination)
            && request.destination == self.remote_destination
            && request.category == self.remote_category
            && request.channel == self.channel
            && request.generation == self.remote_generation
            && request.version == self.selected_version
    }

    pub(super) fn apply_remote(&mut self, mode: Mode, payload: Payload) {
        match payload {
            Payload::Versions(request, versions)
                if self.accepts_remote(mode, &request)
                    && versions
                        .iter()
                        .all(|version| version.channel == request.channel) =>
            {
                self.versions = versions;
                self.artifacts.clear();
                self.selected_version = None;
            }
            Payload::Latest(request, version)
                if self.accepts_remote(mode, &request) && version.channel == request.channel =>
            {
                self.apply_latest(mode, request.destination, version);
            }
            Payload::Artifacts(request, artifacts)
                if self.accepts_remote(mode, &request)
                    && artifacts.iter().all(|artifact| {
                        artifact.version.channel == request.channel
                            && Some(artifact.version.id) == request.version
                    }) =>
            {
                self.artifacts = artifacts;
            }
            Payload::RemoteImages(request, images) if self.accepts_remote(mode, &request) => {
                self.apply_images(request.destination, images);
            }
            _ => {}
        }
    }

    pub(super) fn select_version(&mut self, id: u64) {
        self.remote_generation += 1;
        // A failed or cancelled request must never expose the previous version's files.
        self.artifacts.clear();
        self.selected_version = Some(id);
    }
}

impl Workbench {
    #[inline(never)]
    pub(super) fn flash_remote_picker(&self, cx: &mut Context<Self>) -> AnyElement {
        let current = self.flash.remote_current(self.flash_mode());
        let mut panel = flash_panel(cx).child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .whitespace_normal()
                .child(tr("firmware.remote.help")),
        );
        panel = panel.child(self.flash_remote_channels(cx));
        panel = panel.child(self.flash_remote_load_controls(cx));
        if current && !self.flash.versions.is_empty() {
            panel = panel.child(self.flash_remote_versions(cx));
        }
        panel = panel.child(self.flash_firmware_filters(cx));
        if current && !self.flash.artifacts.is_empty() {
            panel = panel.child(self.flash_remote_artifacts(cx));
        }
        panel.into_any_element()
    }

    #[inline(never)]
    fn flash_remote_channel(
        &self,
        index: usize,
        channel: source::SourceChannel,
        label: &'static str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let busy = self.flash.busy();
        context_button(
            Button::new(("firmware-channel", index))
                .label(tr(label))
                .disabled(busy),
            self.flash.source_channel(self.flash_mode()) == channel,
            cx,
        )
        .on_click(cx.listener(move |this, _, _, cx| {
            let Some(destination) = this.flash.destination(this.flash_mode()) else {
                return;
            };
            this.flash.prepare_remote(this.flash_mode(), &destination);
            this.flash.select_channel(channel);
            cx.notify();
        }))
        .into_any_element()
    }

    #[inline(never)]
    fn flash_remote_channels(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.flash.remote_category_for(self.flash_mode()) == ImageCategory::Bootloader {
            return div()
                .flex()
                .flex_wrap()
                .gap_2()
                .child(self.flash_remote_channel(
                    0,
                    source::SourceChannel::BootloaderRelease,
                    "firmware.remote.channel.bootloader_release",
                    cx,
                ))
                .child(self.flash_remote_channel(
                    1,
                    source::SourceChannel::BootloaderCi,
                    "firmware.remote.channel.bootloader_ci",
                    cx,
                ))
                .into_any_element();
        }
        let mut channels = div().flex().flex_wrap().gap_2();
        for (index, (channel, label)) in [
            (
                source::SourceChannel::Release,
                "firmware.remote.channel.release",
            ),
            (
                source::SourceChannel::TrackerCi,
                "firmware.remote.channel.tracker_ci",
            ),
            (
                source::SourceChannel::ReceiverCi,
                "firmware.remote.channel.receiver_ci",
            ),
        ]
        .into_iter()
        .enumerate()
        {
            channels = channels.child(self.flash_remote_channel(index, channel, label, cx));
        }
        channels.into_any_element()
    }

    #[inline(never)]
    fn flash_remote_load_controls(&self, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.flash.busy() || self.flash.remote_sources().is_none();
        div()
            .flex()
            .flex_wrap()
            .gap_2()
            .child(
                Button::new("firmware-versions")
                    .small()
                    .icon(IconName::RefreshCw)
                    .label(tr("firmware.remote.load_versions"))
                    .disabled(busy)
                    .on_click(cx.listener(|this, _, _, cx| {
                        let channel = this.flash.source_channel(this.flash_mode());
                        let Some(destination) = this.flash.destination(this.flash_mode()) else {
                            return;
                        };
                        this.flash.prepare_remote(this.flash_mode(), &destination);
                        this.flash.clear_remote();
                        let request = this.flash.remote_request(this.flash_mode());
                        let Some(sources) = this.flash.remote_sources() else {
                            return;
                        };
                        let client = this.async_io.http_client();
                        this.start_flash_async(move |cancel, reporter| async move {
                            source::list_versions(
                                &client,
                                sources.as_ref(),
                                channel,
                                &cancel,
                                &mut |value| reporter.emit(value),
                            )
                            .await
                            .map(|versions| Payload::Versions(request, versions))
                        });
                        cx.notify();
                    })),
            )
            .child(
                Button::new("firmware-latest")
                    .small()
                    .icon(IconName::RefreshCw)
                    .label(tr("firmware.remote.latest_available"))
                    .disabled(busy)
                    .on_click(cx.listener(|this, _, _, cx| {
                        let channel = this.flash.source_channel(this.flash_mode());
                        let Some(destination) = this.flash.destination(this.flash_mode()) else {
                            return;
                        };
                        this.flash.prepare_remote(this.flash_mode(), &destination);
                        this.flash.clear_remote();
                        let request = this.flash.remote_request(this.flash_mode());
                        let Some(sources) = this.flash.remote_sources() else {
                            return;
                        };
                        let client = this.async_io.http_client();
                        this.start_flash_async(move |cancel, reporter| async move {
                            source::latest(
                                &client,
                                sources.as_ref(),
                                channel,
                                &cancel,
                                &mut |value| reporter.emit(value),
                            )
                            .await
                            .map(|version| Payload::Latest(request, version))
                        });
                        cx.notify();
                    })),
            )
            .into_any_element()
    }

    #[inline(never)]
    fn flash_remote_version(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.flash.busy() || self.flash.remote_sources().is_none();
        let version = &self.flash.versions[index];
        let selected = self.flash.selected_version == Some(version.id);
        let date = crate::ui::date_label::display_date(&version.published_at)
            .unwrap_or_else(|| version.published_at.clone());
        let latest = self.flash.is_latest(self.flash_mode(), version);
        context_button(
            Button::new(SharedString::from(format!(
                "firmware-version-{:?}-{}",
                version.channel, version.id
            )))
            .small()
            .w_full()
            .min_w_0()
            .flex_shrink_0()
            .accessibility_label(format!(
                "Choose firmware version {} · {}",
                version.name, version.published_at
            ))
            .tooltip(if latest {
                format!(
                    "{} · {}\n{}\n{}",
                    version.name,
                    version.published_at,
                    version.url,
                    tr("firmware.remote.latest_help")
                )
            } else {
                format!(
                    "{} · {}\n{}",
                    version.name, version.published_at, version.url
                )
            })
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_left()
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .child(version.name.clone()),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(date),
                    )
                    .when(latest, |row| {
                        row.child(
                            div()
                                .flex_shrink_0()
                                .text_xs()
                                .text_color(cx.theme().success)
                                .child(tr("firmware.remote.latest_channel")),
                        )
                    }),
            )
            .disabled(busy),
            selected,
            cx,
        )
        .on_click(cx.listener(move |this, _, _, cx| {
            if let Some(version) = this.flash.versions.get(index).cloned() {
                if !this.flash.remote_current(this.flash_mode()) {
                    return;
                }
                let Some(sources) = this.flash.remote_sources() else {
                    return;
                };
                this.flash.select_version(version.id);
                let request = this.flash.remote_request(this.flash_mode());
                let client = this.async_io.http_client();
                this.start_flash_async(move |cancel, reporter| async move {
                    source::list_artifacts(
                        &client,
                        sources.as_ref(),
                        &version,
                        &cancel,
                        &mut |value| reporter.emit(value),
                    )
                    .await
                    .map(|artifacts| Payload::Artifacts(request, artifacts))
                });
            }
            cx.notify();
        }))
        .into_any_element()
    }

    #[inline(never)]
    fn flash_remote_versions(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut versions = div()
            .id("firmware-version-list")
            .min_w_0()
            .max_h(px(112.))
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap_1();
        for index in 0..self.flash.versions.len() {
            versions = versions.child(self.flash_remote_version(index, cx));
        }
        versions.overflow_y_scroll().into_any_element()
    }

    #[inline(never)]
    fn flash_remote_artifact(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.flash.busy();
        let artifact = &self.flash.artifacts[index];
        Button::new(SharedString::from(format!(
            "firmware-artifact-{:?}-{}-{}",
            artifact.version.channel, artifact.version.id, artifact.id
        )))
        .small()
        .h_auto()
        .py_2()
        .w_full()
        .min_w_0()
        .flex_shrink_0()
        .accessibility_label(format!(
            "Load firmware {} · {} · {} · {} B",
            artifact.version.name,
            artifact.name,
            firmware_variant_label(&artifact.variant),
            artifact.size
        ))
        .tooltip(format!(
            "{} · {} · {} B\n{}\n{}",
            artifact.name,
            firmware_variant_label(&artifact.variant),
            artifact.size,
            artifact.url,
            if self.flash_mode() != Mode::Esb || self.preferences.advanced_features {
                artifact.sha256.as_deref().unwrap_or("")
            } else {
                ""
            }
        ))
        .child(
            div()
                .w_full()
                .min_w_0()
                .flex()
                .items_center()
                .gap_2()
                .text_left()
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .child(div().truncate().child(artifact.name.clone()))
                        .child(
                            div()
                                .truncate()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!(
                                    "{} · {} B",
                                    firmware_variant_label(&artifact.variant),
                                    artifact.size
                                )),
                        ),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(if busy {
                            cx.theme().muted_foreground
                        } else {
                            cx.theme().primary
                        })
                        .child(tr("firmware.remote.load_image")),
                ),
        )
        .disabled(busy)
        .on_click(cx.listener(move |this, _, _, cx| {
            if let Some(artifact) = this.flash.artifacts.get(index).cloned() {
                if !this.flash.remote_current(this.flash_mode()) {
                    return;
                }
                let Some(sources) = this.flash.remote_sources() else {
                    return;
                };
                let request = this.flash.remote_request(this.flash_mode());
                let client = this.async_io.http_client();
                this.start_flash_async(move |cancel, reporter| async move {
                    let files = source::download(
                        &client,
                        sources.as_ref(),
                        &artifact,
                        &cancel,
                        &mut |value| reporter.emit(value),
                    )
                    .await?;
                    // Firmware decoding and image construction must not stall the
                    // shared network/BLE driver; retain its join through cancel.
                    let images = tokio::task::spawn_blocking(move || {
                        files
                            .into_iter()
                            .map(|file| {
                                make_image(
                                    file.name,
                                    file.bytes,
                                    file.provenance,
                                    file.sha256,
                                    false,
                                )
                            })
                            .collect()
                    })
                    .await
                    .map_err(|error| format!("Firmware image worker failed: {error}"))?;
                    if cancel.load(std::sync::atomic::Ordering::Acquire) {
                        return Err("Firmware acquisition cancelled".into());
                    }
                    Ok(Payload::RemoteImages(request, images))
                });
            }
            cx.notify();
        }))
        .into_any_element()
    }

    #[inline(never)]
    fn flash_remote_artifacts(&self, cx: &mut Context<Self>) -> AnyElement {
        let targets = self.flash_filter_targets();
        let query = self.firmware_search.read(cx).value().trim().to_lowercase();
        let searching = !query.is_empty();
        let mut eligible_count = 0;
        let mut count = 0;
        let mut artifacts = div()
            .id("firmware-artifact-list")
            .min_w_0()
            .max_h(px(156.))
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap_1();
        for (index, artifact) in self.flash.artifacts.iter().enumerate() {
            let hints = self
                .flash
                .catalog
                .as_ref()
                .map(|catalog| artifact_firmware_hints(catalog, artifact))
                .unwrap_or_default();
            if self.candidate_allowed(&artifact.name, &hints, true)
                && firmware_filter_matches(
                    &hints,
                    &artifact.variant,
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
                eligible_count += 1;
                if searching
                    && !artifact.name.to_lowercase().contains(&query)
                    && !firmware_variant_label(&artifact.variant)
                        .to_lowercase()
                        .contains(&query)
                {
                    continue;
                }
                count += 1;
                artifacts = artifacts.child(self.flash_remote_artifact(index, cx));
            }
        }
        artifacts = artifacts.child(
            div()
                .text_xs()
                .child(format!("{count} / {}", self.flash.artifacts.len())),
        );
        if eligible_count == 0 {
            let message = tr("firmware.image.no_compatible");
            artifacts = artifacts.child(
                div()
                    .id("firmware-artifact-no-compatible")
                    .role(Role::Status)
                    .aria_label(message.clone())
                    .test_support()
                    .text_sm()
                    .whitespace_normal()
                    .child(message),
            );
        } else if count == 0 {
            let message = tr("firmware.remote.search.empty");
            artifacts = artifacts.child(
                div()
                    .flex()
                    .flex_col()
                    .items_start()
                    .gap_2()
                    .child(
                        div()
                            .id("firmware-artifact-search-empty")
                            .role(Role::Status)
                            .aria_label(message.clone())
                            .test_support()
                            .text_sm()
                            .whitespace_normal()
                            .child(message),
                    )
                    .child(
                        Button::new("firmware-artifact-clear-search")
                            .label(tr("firmware.remote.search.clear"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.firmware_search
                                    .update(cx, |input, cx| input.set_value("", window, cx));
                                this.firmware_search.focus_handle(cx).focus(window, cx);
                            })),
                    ),
            );
        }
        div()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                Input::new(&self.firmware_search)
                    .id("firmware-artifact-search")
                    .aria_label(tr("firmware.remote.search.placeholder"))
                    .prefix(IconName::Search)
                    .cleanable(true),
            )
            .child(artifacts.overflow_y_scroll())
            .into_any_element()
    }
}

#[cfg(test)]
#[path = "remote_regression_tests.rs"]
mod remote_regression_tests;
