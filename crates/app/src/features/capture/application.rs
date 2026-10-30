use super::components::clipped;
use super::tracker_cards::{TrackerCardGrid, tracker_activity_outline, tracker_card_frame};
use super::*;
use crate::discovery::ReceiverObservation;
use crate::domain::{ReceiverEndpoint, TrackerLink};
use crate::features::capture::device::{
    self as device, CaptureMode, CaptureOutcome, CapturePhase, CaptureSnapshot, CaptureSource,
    CaptureWorker,
};
use crate::features::capture::session::{CapturePreparation, CaptureSession};
use crate::serial_port::same_port_name;
use gpui_kit::component::{
    Selectable as _,
    button::{ButtonCustomVariant, ButtonVariants as _},
    checkbox::Checkbox,
    input::Input,
};
use slimenrf_recording::capture::{Gyro, Sample};
use std::{path::PathBuf, sync::Arc};

#[cfg(test)]
#[path = "application/interaction_tests.rs"]
mod interaction_tests;

#[derive(Default)]
pub(super) struct CaptureUiState {
    sources: Vec<CaptureSource>,
    source: Option<usize>,
    discovering: bool,
    discovery_task: Option<Task<()>>,
    chooser: Option<Task<()>>,
    destination: Option<PathBuf>,
    batch: bool,
    rate: Option<Entity<InputState>>,
    target: Option<(u8, String)>,
    acknowledged: bool,
    session: CaptureSession,
    snapshot: Option<Arc<CaptureSnapshot>>,
    last_preview_poll: Option<Instant>,
    preview_slot: Option<u8>,
    details: bool,
    completed: Option<CaptureOutcome>,
    error: Option<SharedString>,
}

impl CaptureUiState {
    pub(super) fn busy(&self) -> bool {
        self.session.busy()
    }

    fn replace_sources(&mut self, sources: Vec<CaptureSource>) {
        self.source = self
            .source
            .and_then(|i| self.sources.get(i))
            .and_then(|previous| sources.iter().position(|source| previous.matches(source)));
        self.sources = sources;
        self.target = None;
        self.acknowledged = false;
        self.error = None;
    }

    pub(super) fn receiver_key(&self) -> Option<&str> {
        self.session
            .receiver()
            .map(|receiver| receiver.key.as_str())
    }

    pub(super) fn revoke_receiver_bindings(&mut self, receiver: &str) {
        if !self.busy()
            && self
                .source
                .and_then(|index| self.sources.get(index))
                .is_some_and(|source| source.receiver.key == receiver)
        {
            self.target = None;
            self.acknowledged = false;
        }
    }

    fn preview_due(&mut self, visible: bool, now: Instant) -> bool {
        if !visible {
            self.last_preview_poll = None;
            return false;
        }
        if self
            .last_preview_poll
            .is_some_and(|last| now.saturating_duration_since(last) < Duration::from_millis(100))
        {
            return false;
        }
        self.last_preview_poll = Some(now);
        true
    }

    fn accept_preview_snapshot(&mut self, snapshot: Arc<CaptureSnapshot>) -> bool {
        if self
            .snapshot
            .as_ref()
            .is_some_and(|previous| Arc::ptr_eq(previous, &snapshot))
        {
            return false;
        }
        self.snapshot = Some(snapshot);
        true
    }
}

fn capture_port_name(name: &str) -> std::borrow::Cow<'_, str> {
    if cfg!(windows) {
        std::borrow::Cow::Owned(name.trim_start_matches(r"\\.\").to_ascii_lowercase())
    } else {
        std::borrow::Cow::Borrowed(name)
    }
}

fn capture_source_id(source: &CaptureSource) -> SharedString {
    let receiver = (
        &source.receiver.key,
        &source.receiver.path,
        &source.receiver.serial_number,
    );
    match &source.transport {
        device::CaptureTransport::RawHid { path } => {
            format!("capture-source:{receiver:?}:hid:{path:?}").into()
        }
        device::CaptureTransport::Cdc { endpoint, console } => format!(
            "capture-source:{receiver:?}:cdc:{:?}:{:?}",
            (
                capture_port_name(&endpoint.port_name),
                endpoint.kind,
                endpoint.vid,
                endpoint.pid,
                &endpoint.serial_number,
                &endpoint.usb_interface,
            ),
            (
                capture_port_name(&console.port_name),
                console.kind,
                console.vid,
                console.pid,
                &console.serial_number,
                &console.usb_interface,
            )
        )
        .into(),
    }
}

struct CaptureTrackerCard<'a> {
    tracker: &'a TrackerLink,
    address: String,
    tps: u32,
}

fn capture_tracker_cards<'a>(
    receiver: &'a ReceiverEndpoint,
    observation: Option<&'a ReceiverObservation>,
) -> impl Iterator<Item = CaptureTrackerCard<'a>> {
    observation.into_iter().flat_map(move |observation| {
        observation.trackers.iter().filter_map(move |tracker| {
            if tracker.receiver_key != receiver.key {
                return None;
            }
            Some(CaptureTrackerCard {
                tracker,
                address: tracker
                    .address
                    .as_deref()
                    .and_then(crate::domain::normalize_address)?,
                tps: observation.tps(tracker.slot),
            })
        })
    })
}

impl Workbench {
    pub(super) fn capture_check_address(&self, address: &str) -> Result<(), String> {
        if self.capture.session.blocks_address(address) {
            Err(tr("capture.error.receiver_busy"))
        } else {
            Ok(())
        }
    }

    pub(super) fn capture_check_receiver(&self, key: &str) -> Result<(), String> {
        if self.capture.receiver_key() == Some(key) {
            Err(tr("capture.error.receiver_busy"))
        } else {
            Ok(())
        }
    }

    pub(super) fn capture_check_port(&self, port: &str) -> Result<(), String> {
        if self.capture.session.blocks_port(
            port,
            self.manager.session(port).map(|s| &s.identity),
            self.inventory
                .ports
                .iter()
                .find(|p| same_port_name(&p.port_name, port)),
        ) {
            Err(tr("capture.error.receiver_busy"))
        } else {
            Ok(())
        }
    }

    pub(super) fn refresh_capture_sources(&mut self, cx: &mut Context<Self>) {
        if self.shutting_down || self.capture.busy() || self.capture.discovering {
            return;
        }
        self.capture.discovering = true;
        let scan = cx
            .background_executor()
            .spawn(async { device::discover_sources() });
        self.capture.discovery_task = Some(cx.spawn(async move |this, cx| {
            let result = scan.await;
            let _ = this.update(cx, |this, cx| {
                this.capture.discovering = false;
                if this.shutting_down || this.capture.busy() {
                    return;
                }
                match result {
                    Ok(sources) => {
                        this.capture.replace_sources(sources);
                    }
                    Err(error) => this.capture.error = Some(error.into()),
                }
                cx.notify();
            });
        }));
    }

    fn choose_capture_path(&mut self, cx: &mut Context<Self>) {
        if self.shutting_down || self.capture.busy() || self.capture.chooser.is_some() {
            return;
        }
        let directory = directories::BaseDirs::new()
            .map(|dirs| dirs.home_dir().to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        let filename = format!(
            "capture-{}.slimecap",
            chrono::Local::now().format("%Y%m%d-%H%M%S")
        );
        let dialog = cx.prompt_for_new_path(&directory, Some(&filename));
        self.capture.chooser = Some(cx.spawn(async move |this, cx| {
            let result = dialog.await;
            let _ = this.update(cx, |this, cx| {
                this.capture.chooser = None;
                if this.shutting_down || this.capture.busy() {
                    return;
                }
                match result {
                    Ok(Ok(Some(path))) => this.capture.destination = Some(path),
                    Ok(Ok(None)) => {}
                    Ok(Err(error)) => this.capture.error = Some(error.to_string().into()),
                    Err(error) => this.capture.error = Some(error.to_string().into()),
                }
                cx.notify();
            });
        }));
    }

    fn start_capture(&mut self, cx: &mut Context<Self>) {
        if self.shutting_down || self.capture.busy() {
            return;
        }
        self.capture.error = None;
        if let Err(error) = self.prepare_capture(cx) {
            self.capture.error = Some(error.into());
        }
        cx.notify();
    }

    fn prepare_capture(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
        let source = self
            .capture
            .source
            .and_then(|i| self.capture.sources.get(i))
            .cloned()
            .ok_or_else(|| tr("capture.error.source_required"))?;
        self.flash_check_receiver(&source.receiver.key)?;
        let destination = self
            .capture
            .destination
            .clone()
            .ok_or_else(|| tr("capture.file.choose"))?;
        let mode = if self.capture.batch {
            let rate = self
                .capture
                .rate
                .as_ref()
                .map(|input| input.read(cx).value().to_string())
                .unwrap_or_else(|| "100".into());
            let rate_hz = rate
                .trim()
                .parse::<u8>()
                .map_err(|_| tr("capture.error.invalid_rate"))?;
            CaptureMode::AllPaired { rate_hz }
        } else {
            if !self.capture.acknowledged {
                return Err(tr("capture.error.sleep_ack_required"));
            }
            let (slot, address) = self
                .capture
                .target
                .clone()
                .ok_or_else(|| tr("capture.error.target_required"))?;
            CaptureMode::Single {
                slot,
                rf_address: address,
            }
        };
        let preparation = CapturePreparation::freeze(
            source,
            mode,
            destination,
            &self.inventory,
            &self.manager,
            &self.monitor,
        )?;
        for port in preparation.ports() {
            self.flash_check_port(port)?;
        }
        self.capture
            .session
            .prepare(preparation, &mut self.monitor)?;
        self.capture.completed = None;
        self.capture.snapshot = None;
        self.capture.last_preview_poll = None;
        Ok(())
    }

    fn stop_capture(&mut self) {
        self.capture
            .session
            .stop(&mut self.manager, &mut self.monitor);
        if let Some(error) = self.capture.session.take_error() {
            self.capture.error = Some(error.into());
        }
    }

    pub(super) fn poll_capture(&mut self, _cx: &mut Context<Self>) -> bool {
        if self.shutting_down {
            return false;
        }
        let visible = self.active_page == FeaturePage::RawCapture;
        let mut changed =
            self.capture
                .session
                .poll(&self.inventory, &mut self.manager, &mut self.monitor);
        if self.capture.session.running()
            && self.capture.preview_due(visible, Instant::now())
            && let Some(snapshot) = self.capture.session.snapshot()
        {
            changed |= self.capture.accept_preview_snapshot(snapshot);
        }
        if let Some(result) = self.capture.session.take_completion() {
            // Completion always publishes the final snapshot, even on a hidden page.
            if let Some(snapshot) = self.capture.session.snapshot() {
                self.capture.accept_preview_snapshot(snapshot);
            }
            match result {
                Ok(outcome) => {
                    self.capture.error = outcome.error.as_ref().map(|error| error.as_str().into());
                    self.capture.completed = Some(outcome);
                }
                Err(error) => self.capture.error = Some(error.into()),
            }
        }
        // Release errors take precedence over worker errors as before.
        if let Some(error) = self.capture.session.take_error() {
            self.capture.error = Some(error.into());
        }
        visible && changed
    }

    pub(super) fn shutdown_capture(&mut self) -> Option<CaptureWorker> {
        self.capture.session.take_shutdown_worker()
    }

    #[inline(never)]
    pub(super) fn capture_page(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if self.capture.rate.is_none() {
            self.capture.rate = Some(cx.new(|cx| InputState::new(window, cx).default_value("100")));
        }
        div()
            .id("capture-page")
            .flex_1()
            .h_full()
            .min_w_0()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_3()
            .p_3()
            .child(div().text_lg().child(tr("capture.title")))
            .child(self.capture_source_controls(cx))
            .child(self.capture_mode_controls(cx))
            .child(self.capture_record_controls(cx))
            .child(self.capture_live_status(cx))
            .child(self.capture_preview(cx))
            .into_any_element()
    }

    #[inline(never)]
    fn capture_source_controls(&self, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.capture.busy();
        let mut sources = div().flex().flex_col().gap_2().child(
            div()
                .flex()
                .gap_2()
                .items_center()
                .child(tr("capture.source.title"))
                .child(
                    Button::new("capture-refresh")
                        .small()
                        .label(tr("common.action.refresh"))
                        .loading(self.capture.discovering)
                        .disabled(busy || self.capture.discovering)
                        .on_click(cx.listener(|this, _, _, cx| this.refresh_capture_sources(cx))),
                ),
        );
        for (index, source) in self.capture.sources.iter().enumerate() {
            sources = sources.child(
                Button::new(capture_source_id(source))
                    .small()
                    .label(self.receiver_label(&source.receiver, source.label()))
                    .selected(self.capture.source == Some(index))
                    .disabled(busy)
                    .on_click({
                        let source = source.clone();
                        cx.listener(move |this, _, _, cx| {
                            if this.capture.busy() {
                                return;
                            }
                            let Some(index) = this
                                .capture
                                .sources
                                .iter()
                                .position(|current| current.matches(&source))
                            else {
                                return;
                            };
                            this.capture.source = Some(index);
                            this.capture.target = None;
                            this.capture.acknowledged = false;
                            cx.notify();
                        })
                    }),
            );
        }
        if self.capture.discovering || self.capture.sources.is_empty() {
            sources = sources.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(tr(if self.capture.discovering {
                        "capture.source.searching"
                    } else {
                        "capture.source.none_found"
                    })),
            );
        }
        sources.into_any_element()
    }

    #[inline(never)]
    fn capture_mode_controls(&self, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.capture.busy();
        let mut section = div().flex().flex_col().gap_2().child(
            div()
                .flex()
                .gap_2()
                .child(
                    Button::new("capture-single")
                        .small()
                        .label(tr("capture.mode.single"))
                        .selected(!self.capture.batch)
                        .disabled(busy)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.capture.batch = false;
                            this.capture.acknowledged = false;
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("capture-batch")
                        .small()
                        .label(tr("capture.mode.batch"))
                        .selected(self.capture.batch)
                        .disabled(busy)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.capture.batch = true;
                            this.capture.acknowledged = false;
                            cx.notify();
                        })),
                ),
        );
        if self.capture.batch {
            if let Some(rate) = &self.capture.rate {
                section = section.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(tr("capture.mode.rate"))
                        .child(
                            div()
                                .w(px(100.))
                                .child(Input::new(rate).small().disabled(busy)),
                        ),
                );
            }
        } else {
            let mut targets = div().w_full().min_w_0();
            if let Some(receiver) = self.capture.session.receiver() {
                if let Some((slot, address)) = &self.capture.target {
                    targets = targets.child(self.address_label(
                        Some(address),
                        format!(
                            "{} · #{} · {}",
                            self.frozen_receiver_label(receiver, receiver.label.clone()),
                            slot,
                            address
                        ),
                    ));
                }
            } else if let Some(source) = self
                .capture
                .source
                .and_then(|i| self.capture.sources.get(i))
            {
                let cards = capture_tracker_cards(
                    &source.receiver,
                    self.monitor.observation_for(&source.receiver),
                )
                .map(|card| self.capture_tracker_card(card, cx))
                .collect();
                targets = targets.child(
                    TrackerCardGrid::new(
                        SharedString::from(format!("capture-tracker-grid:{}", source.receiver.key)),
                        cards,
                    )
                    .horizontal_padding(0.),
                );
            }
            section = section.child(targets).child(
                Checkbox::new("capture-sleep-ack")
                    .label(tr("capture.mode.sleep_ack"))
                    .checked(self.capture.acknowledged)
                    .disabled(busy)
                    .on_click(cx.listener(|this, checked, _, cx| {
                        this.capture.acknowledged = *checked;
                        cx.notify();
                    })),
            );
        }
        section.into_any_element()
    }

    #[inline(never)]
    fn capture_tracker_card(
        &self,
        data: CaptureTrackerCard<'_>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let CaptureTrackerCard {
            tracker,
            address,
            tps,
        } = data;
        let target = (tracker.slot, address.clone());
        let selected = self.capture.target.as_ref() == Some(&target);
        let busy = self.capture.busy();
        let key = format!("{:?}", (&tracker.receiver_key, tracker.slot, &address));
        let (activity, outline_width) =
            tracker_activity_outline(tps, cx.theme().is_dark(), self.preferences.accent);
        let label: SharedString = self
            .tracker_label(
                tracker,
                rust_i18n::t!("devices.tracker.slot_label", slot = tracker.slot).to_string(),
            )
            .into();
        let contents =
            div()
                .w_full()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_1()
                .child(div().min_w_0().text_sm().truncate().child(label.clone()))
                .child(
                    div()
                        .min_w_0()
                        .text_xs()
                        .font_family(cx.theme().mono_font_family.clone())
                        .text_color(cx.theme().muted_foreground)
                        .child(clipped(format!("capture-address:{key}"), address)),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(tr(if tracker.online {
                            "devices.tracker.state.online"
                        } else {
                            "devices.tracker.state.offline"
                        }))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1()
                                .child(div().size(px(6.)).flex_shrink_0().rounded_md().bg(
                                    if tps > 0 {
                                        activity
                                    } else {
                                        cx.theme().muted_foreground
                                    },
                                ))
                                .child(
                                    rust_i18n::t!("devices.tracker.traffic_rate", tps = tps)
                                        .to_string(),
                                ),
                        ),
                );
        let card = Button::new(SharedString::from(format!("capture-card:{key}")))
            .custom(
                ButtonCustomVariant::new(cx)
                    .color(if tracker.online {
                        cx.theme().group_box
                    } else {
                        cx.theme().sidebar
                    })
                    .foreground(cx.theme().foreground)
                    .hover(cx.theme().accent)
                    .active(cx.theme().accent),
            )
            .w_full()
            .min_w_0()
            .h_auto()
            .p_2()
            .text_left()
            .whitespace_normal()
            .accessibility_label(label)
            .selected(selected)
            .toggled(selected)
            .disabled(busy)
            .rounded_md()
            .border_1()
            .border_color(if selected {
                cx.theme().primary
            } else {
                cx.theme().border.opacity(0.18)
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                if !this.capture.busy() {
                    this.capture.target = Some(target.clone());
                    this.capture.acknowledged = false;
                    cx.notify();
                }
            }))
            .child(contents);
        tracker_card_frame(card, activity, outline_width).into_any_element()
    }

    #[inline(never)]
    fn capture_record_controls(&self, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.capture.busy();
        let mut row = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div().text_sm().child(
                    self.capture
                        .destination
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| tr("capture.file.choose")),
                ),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .flex_wrap()
                    .child(
                        Button::new("capture-path")
                            .small()
                            .label(tr("capture.file.choose"))
                            .disabled(busy || self.capture.chooser.is_some())
                            .on_click(cx.listener(|this, _, _, cx| this.choose_capture_path(cx))),
                    )
                    .child(
                        Button::new("capture-start")
                            .small()
                            .label(tr("capture.action.start"))
                            .disabled(
                                busy || self.capture.source.is_none()
                                    || self.capture.destination.is_none(),
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.start_capture(cx))),
                    )
                    .child(
                        Button::new("capture-stop")
                            .small()
                            .label(tr("capture.action.stop_save"))
                            .disabled(!busy)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.stop_capture();
                                cx.notify();
                            })),
                    ),
            );
        if let Some(outcome) = &self.capture.completed {
            let path = outcome.path.clone();
            row = row
                .child(div().text_sm().child(format!(
                    "{} · {} {}",
                    path.display(),
                    outcome.samples,
                    tr("capture.samples")
                )))
                .child(
                    Button::new("capture-open-replay")
                        .small()
                        .label(tr("capture.action.open_replay"))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            if this.set_feature_page(FeaturePage::VqfReplay, window, cx) {
                                this.replay
                                    .update(cx, |replay, cx| replay.open_path(path.clone(), cx));
                                cx.notify();
                            }
                        })),
                );
        }
        if let Some(error) = &self.capture.error {
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
    fn capture_live_status(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut status = div().flex().flex_col().gap_1().text_sm();
        if let Some(snapshot) = &self.capture.snapshot {
            let phase = match snapshot.phase {
                CapturePhase::Opening | CapturePhase::Starting => "capture.phase.starting",
                CapturePhase::WarmingUp => "capture.phase.warming_up",
                CapturePhase::WaitingForSamples => "capture.phase.waiting_for_samples",
                CapturePhase::Receiving => "capture.phase.receiving",
                CapturePhase::Stopping => "capture.phase.saving",
                CapturePhase::Finished => "capture.phase.finished",
                CapturePhase::Failed => "capture.phase.failed",
            };
            status = status
                .child(tr(phase))
                .child(
                    rust_i18n::t!(
                        "capture.status.summary",
                        samples = snapshot.counters.samples,
                        rate = format!("{:.1}", snapshot.rate_hz),
                        seconds = format!("{:.1}", snapshot.elapsed.as_secs_f64())
                    )
                    .to_string(),
                )
                .child(
                    Button::new("capture-details")
                        .small()
                        .label(tr("capture.status.details"))
                        .selected(self.capture.details)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.capture.details = !this.capture.details;
                            cx.notify();
                        })),
                );
            if self.capture.details {
                status = status.child(
                    rust_i18n::t!(
                        "capture.status.issues",
                        gaps = snapshot.counters.sequence_gaps,
                        bad = snapshot.counters.malformed
                    )
                    .to_string(),
                );
                for warning in &snapshot.warnings {
                    status = status.child(
                        div()
                            .text_color(cx.theme().muted_foreground)
                            .child(warning.clone()),
                    );
                }
            }
        } else if self.capture.session.pending() {
            status = status.child(tr("capture.phase.starting"));
        }
        status.into_any_element()
    }

    #[inline(never)]
    fn capture_preview(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(snapshot) = &self.capture.snapshot else {
            return div().into_any_element();
        };
        let mut panel = div().flex().flex_col().gap_2();
        let mut tabs = div().flex().flex_wrap().gap_2();
        let slot = self
            .capture
            .preview_slot
            .filter(|slot| snapshot.tracks.iter().any(|t| t.identity.slot == *slot))
            .or_else(|| snapshot.tracks.first().map(|t| t.identity.slot));
        for track in &snapshot.tracks {
            let track_slot = track.identity.slot;
            tabs = tabs.child(
                Button::new(("capture-preview-track", track_slot as usize))
                    .small()
                    .label(self.address_label(
                        track.identity.rf_address.as_deref(),
                        format!("#{} · {}", track_slot, track.samples),
                    ))
                    .selected(slot == Some(track_slot))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.capture.preview_slot = Some(track_slot);
                        cx.notify();
                    })),
            );
        }
        panel = panel.child(tabs);
        if let Some(index) = snapshot
            .tracks
            .iter()
            .position(|t| Some(t.identity.slot) == slot)
        {
            let track = &snapshot.tracks[index];
            let quaternion = track
                .latest
                .back()
                .is_some_and(|s| matches!(s.gyro, Gyro::IntegratedQuat(_)));
            if self.capture.details {
                panel = panel.child(
                    div().text_sm().child(
                        rust_i18n::t!(
                            "capture.status.sensor_rates",
                            raw = optional_rate(track.metadata.raw_tx_hz),
                            chip = optional_rate(track.metadata.gyro_chip_hz),
                            fusion = optional_rate(track.metadata.gyro_fusion_hz)
                        )
                        .to_string(),
                    ),
                );
            }
            panel = panel.child(live_curve(
                snapshot.clone(),
                index,
                0,
                if quaternion {
                    "capture.plot.gyro_quaternion"
                } else {
                    "capture.plot.gyro_rate"
                },
                cx,
            ));
            panel = panel.child(live_curve(
                snapshot.clone(),
                index,
                1,
                "capture.plot.acceleration",
                cx,
            ));
            panel = panel.child(live_curve(
                snapshot.clone(),
                index,
                2,
                "capture.plot.magnetic",
                cx,
            ));
        }
        panel.into_any_element()
    }
}

fn optional_rate(rate: Option<f32>) -> String {
    rate.map(|v| format!("{v:.1}"))
        .unwrap_or_else(|| "?".into())
}

fn signal(sample: &Sample, kind: u8, axis: usize) -> Option<f32> {
    let value = match kind {
        0 => match sample.gyro {
            Gyro::RatesDps(v) => *v.get(axis)?,
            Gyro::IntegratedQuat(v) => *v.get(axis)?,
        },
        1 if sample.acceleration_fresh => *sample.acceleration_g.get(axis)?,
        2 if sample.new_mag => *sample.magnetic_gauss.get(axis)?,
        _ => return None,
    };
    value.is_finite().then_some(value)
}

#[inline(never)]
fn live_curve(
    snapshot: Arc<CaptureSnapshot>,
    index: usize,
    kind: u8,
    title: &'static str,
    cx: &Context<Workbench>,
) -> AnyElement {
    let track = &snapshot.tracks[index];
    let dimensions = if kind == 0
        && track
            .latest
            .back()
            .is_some_and(|s| matches!(s.gyro, Gyro::IntegratedQuat(_)))
    {
        4
    } else {
        3
    };
    let mut extent = 0.001_f32;
    for sample in &track.latest {
        for axis in 0..dimensions {
            if let Some(v) = signal(sample, kind, axis) {
                extent = extent.max(v.abs());
            }
        }
    }
    let colors = [rgb(0xd85a64), rgb(0x3aaa7c), rgb(0x5688dd), rgb(0xb37cdb)];
    let labels: &[&str] = if dimensions == 4 {
        &[
            "replay.axis.w",
            "replay.axis.x",
            "replay.axis.y",
            "replay.axis.z",
        ]
    } else {
        &["replay.axis.x", "replay.axis.y", "replay.axis.z"]
    };
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_sm()
                .child(format!("{} · ±{:.3}", tr(title), extent)),
        )
        .child(
            div().flex().flex_wrap().gap_2().text_xs().children(
                colors
                    .iter()
                    .zip(labels)
                    .map(|(color, label)| div().text_color(*color).child(tr(label))),
            ),
        )
        .child(
            div()
                .h(px(110.))
                .w_full()
                .border_1()
                .border_color(cx.theme().border)
                .rounded_md()
                .overflow_hidden()
                .child(
                    canvas(
                        move |_, _, _| (),
                        move |bounds, (), window, _| {
                            let data = &snapshot.tracks[index].latest;
                            if data.len() < 2 {
                                return;
                            }
                            for (axis, &color) in colors.iter().enumerate().take(dimensions) {
                                let mut path = PathBuilder::stroke(px(1.4));
                                let mut previous = None;
                                for (i, sample) in data.iter().enumerate() {
                                    let Some(value) = signal(sample, kind, axis) else {
                                        previous = None;
                                        continue;
                                    };
                                    let p = point(
                                        bounds.origin.x
                                            + bounds.size.width
                                                * (i as f32 / (data.len() - 1) as f32),
                                        bounds.origin.y
                                            + bounds.size.height * (0.5 - 0.45 * value / extent),
                                    );
                                    let contiguous = previous.is_some_and(|seq: u16| {
                                        sample.sequence.wrapping_sub(seq) == 1
                                    }) && !sample.discontinuity;
                                    if contiguous {
                                        path.line_to(p);
                                    } else {
                                        // Fresh accel/mag may be sparse. Draw each observation without
                                        // inventing held values or joining across absent measurements.
                                        path.move_to(point(p.x - px(1.), p.y));
                                        path.line_to(p);
                                    }
                                    previous = Some(sample.sequence);
                                }
                                if let Ok(path) = path.build() {
                                    window.paint_path(path, color);
                                }
                            }
                        },
                    )
                    .size_full(),
                ),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::{CaptureUiState, capture_source_id, capture_tracker_cards};
    use crate::discovery::ReceiverMonitor;
    use crate::domain::{
        InterfaceDescription, Inventory, ReceiverEndpoint, SerialEndpoint, SerialPortKind,
        TrackerLink, UsbInterface,
    };
    use crate::features::calibration::engine::{
        CalibrationSession, CalibrationTask, GuideSelection, Target,
    };
    use crate::features::capture::device::{
        CaptureMode, CaptureSnapshot, CaptureSource, CaptureTransport,
    };
    use crate::features::capture::session::CapturePreparation;
    use crate::sessions::test_support;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    #[gpui_kit::test]
    fn shutdown_rejects_capture_start_discovery_and_picker(cx: &mut gpui_kit::TestAppContext) {
        use gpui_kit::VisualContext as _;

        let _ui_session = crate::test_support::UiTestSession::new(cx);
        let runtime = crate::app_runtime::AppRuntime::new().unwrap();
        let executor = runtime.executor();
        let cx = cx.add_empty_window();
        let workbench = cx.new_window_entity(move |window, cx| {
            crate::application::test_support::offline_workbench(
                window,
                cx,
                crate::sessions::SessionManager::new(),
                executor,
            )
        });
        workbench.update(cx, |this, cx| {
            this.shutting_down = true;
            this.refresh_capture_sources(cx);
            this.choose_capture_path(cx);
            this.start_capture(cx);
            assert!(!this.capture.busy());
            assert!(!this.capture.discovering);
            assert!(this.capture.chooser.is_none());
            assert!(this.capture.error.is_none());
            this.shutting_down = false;
            this.start_capture(cx);
            assert!(this.capture.error.is_some(), "normal admission resumes");
            this.choose_capture_path(cx);
            assert!(this.capture.chooser.is_some());
        });
        cx.simulate_new_path_selection(|_| None);
        cx.run_until_parked();
    }

    fn cdc_source() -> CaptureSource {
        CaptureSource {
            receiver: ReceiverEndpoint {
                key: "receiver".into(),
                path: b"receiver-hid".to_vec(),
                label: "Receiver".into(),
                serial_number: Some("USB-A".into()),
            },
            transport: CaptureTransport::Cdc {
                endpoint: SerialEndpoint {
                    port_name: "COM8".into(),
                    label: "Raw data".into(),
                    manufacturer: None,
                    product: None,
                    kind: SerialPortKind::Usb,
                    serial_number: Some("USB-A".into()),
                    vid: Some(0x1209),
                    pid: Some(0x7690),
                    usb_interface: Some(UsbInterface {
                        number: Some(2),
                        description: InterfaceDescription::Value("SlimeNRF Raw Data CDC".into()),
                    }),
                },
                console: Box::new(SerialEndpoint {
                    port_name: "COM7".into(),
                    label: "Console".into(),
                    manufacturer: None,
                    product: None,
                    kind: SerialPortKind::Usb,
                    serial_number: Some("USB-A".into()),
                    vid: Some(0x1209),
                    pid: Some(0x7690),
                    usb_interface: Some(UsbInterface {
                        number: Some(0),
                        description: InterfaceDescription::Unavailable,
                    }),
                }),
            },
        }
    }

    #[test]
    fn source_identity_survives_label_and_inventory_order_changes() {
        let source = cdc_source();
        let id = capture_source_id(&source);
        let mut renamed = source.clone();
        renamed.receiver.label = "Receiver nickname".into();
        let CaptureTransport::Cdc { endpoint, console } = &mut renamed.transport else {
            unreachable!();
        };
        endpoint.label = "New display name".into();
        console.label = "New console display name".into();
        assert_eq!(capture_source_id(&renamed), id);
        let mut second = source.clone();
        second.receiver.key = "another-receiver".into();
        let mut state = CaptureUiState {
            sources: vec![source.clone(), second.clone()],
            source: Some(0),
            ..Default::default()
        };
        state.replace_sources(vec![second, renamed]);
        assert_eq!(state.source, Some(1));
        assert_eq!(capture_source_id(&state.sources[1]), id);
        let CaptureTransport::Cdc { endpoint, .. } = &mut state.sources[1].transport else {
            unreachable!();
        };
        endpoint.usb_interface.as_mut().unwrap().number = Some(3);
        assert_ne!(capture_source_id(&state.sources[1]), id);
        let mut raw = source;
        raw.transport = CaptureTransport::RawHid {
            path: b"raw-interface".to_vec(),
        };
        assert_ne!(capture_source_id(&raw), id);
    }

    #[test]
    fn capture_cards_show_current_receiver_scoped_activity_without_gating_offline_targets() {
        let source = cdc_source();
        let receiver = &source.receiver;
        let mut monitor = ReceiverMonitor::fixture(receiver.clone(), 1, "112233445566", true);
        monitor.fixture_tps(75);
        let mut other = receiver.clone();
        other.key = "other-receiver".into();
        other.path = b"other-hid".to_vec();
        other.serial_number = Some("USB-B".into());
        monitor.fixture_extend(other.clone(), 1, "445566778899", true);
        monitor.fixture_observation_mut(receiver).trackers.extend([
            TrackerLink {
                receiver_key: receiver.key.clone(),
                slot: 2,
                address: Some("223344556677".into()),
                online: true,
            },
            TrackerLink {
                receiver_key: receiver.key.clone(),
                slot: 3,
                address: Some("334455667788".into()),
                online: false,
            },
            TrackerLink {
                receiver_key: other.key.clone(),
                slot: 1,
                address: Some("445566778899".into()),
                online: true,
            },
            TrackerLink {
                receiver_key: receiver.key.clone(),
                slot: 4,
                address: None,
                online: true,
            },
        ]);
        let cards: Vec<_> = capture_tracker_cards(receiver, monitor.observation_for(receiver))
            .map(|card| {
                (
                    card.tracker.slot,
                    card.address,
                    card.tracker.online,
                    card.tps,
                )
            })
            .collect();
        assert_eq!(
            cards,
            [
                (1, "112233445566".into(), true, 75),
                (2, "223344556677".into(), true, 0),
                (3, "334455667788".into(), false, 0),
            ]
        );
        let other_cards: Vec<_> = capture_tracker_cards(&other, monitor.observation_for(&other))
            .map(|card| (card.tracker.slot, card.address, card.tps))
            .collect();
        assert_eq!(other_cards, [(1, "445566778899".into(), 0)]);

        monitor.fixture_tps(0);
        let current = capture_tracker_cards(receiver, monitor.observation_for(receiver))
            .next()
            .unwrap();
        assert!(current.tracker.online);
        assert_eq!(current.tps, 0);
        let inventory = Inventory {
            receivers: vec![receiver.clone()],
            ..Default::default()
        };
        assert!(
            CapturePreparation::freeze(
                source,
                CaptureMode::Single {
                    slot: 3,
                    rf_address: "334455667788".into(),
                },
                "synthetic-offline-capture.snr".into(),
                &inventory,
                &test_support::display_manager(),
                &monitor,
            )
            .is_ok()
        );
    }

    #[test]
    fn missing_or_replaced_monitor_observation_is_not_an_offline_card() {
        let receiver = cdc_source().receiver;
        let monitor = ReceiverMonitor::fixture(receiver.clone(), 1, "112233445566", true);
        assert_eq!(capture_tracker_cards(&receiver, None).count(), 0);
        for replacement in [
            ReceiverEndpoint {
                path: b"replacement-hid".to_vec(),
                ..receiver.clone()
            },
            ReceiverEndpoint {
                serial_number: Some("USB-REPLACED".into()),
                ..receiver.clone()
            },
        ] {
            assert_eq!(
                capture_tracker_cards(&replacement, monitor.observation_for(&replacement)).count(),
                0
            );
        }
    }

    #[test]
    fn pairing_change_clears_only_the_affected_idle_capture_selection() {
        let mut state = CaptureUiState {
            sources: vec![cdc_source()],
            source: Some(0),
            target: Some((3, "334455667788".into())),
            acknowledged: true,
            ..Default::default()
        };
        state.revoke_receiver_bindings("another-receiver");
        assert_eq!(state.target, Some((3, "334455667788".into())));
        assert!(state.acknowledged);
        state.revoke_receiver_bindings("receiver");
        assert_eq!(state.target, None);
        assert!(!state.acknowledged);
        assert_eq!(state.source, Some(0));
    }

    #[test]
    fn capture_selection_retains_source_identity_not_display_labels_or_list_position() {
        let original = cdc_source();
        let mut state = CaptureUiState {
            sources: vec![original.clone()],
            source: Some(0),
            ..Default::default()
        };
        let mut renamed = original.clone();
        renamed.receiver.label = "Renamed receiver".into();
        let CaptureTransport::Cdc { endpoint, .. } = &mut renamed.transport else {
            unreachable!();
        };
        endpoint.label = "Updated native display name".into();
        let mut other = original;
        other.receiver.key = "different-receiver".into();
        state.replace_sources(vec![other, renamed]);
        assert_eq!(state.source, Some(1));
    }

    #[test]
    fn capture_selection_is_revoked_by_receiver_interface_or_role_changes() {
        for scenario in 0..8 {
            let original = cdc_source();
            let mut changed = original.clone();
            match scenario {
                0 => changed.receiver.path = b"replacement-hid".to_vec(),
                1 => changed.receiver.serial_number = Some("USB-B".into()),
                _ => {
                    let CaptureTransport::Cdc { endpoint, console } = &mut changed.transport else {
                        unreachable!();
                    };
                    match scenario {
                        2 => endpoint.port_name = "COM9".into(),
                        3 => endpoint.usb_interface.as_mut().unwrap().number = Some(4),
                        4 => {
                            endpoint.usb_interface.as_mut().unwrap().description =
                                InterfaceDescription::Unavailable
                        }
                        5 => console.port_name = "COM10".into(),
                        6 => console.usb_interface.as_mut().unwrap().number = Some(5),
                        7 => console.serial_number = Some("USB-B".into()),
                        _ => unreachable!(),
                    }
                }
            }
            let mut state = CaptureUiState {
                sources: vec![original],
                source: Some(0),
                ..Default::default()
            };
            state.replace_sources(vec![changed]);
            assert_eq!(state.source, None, "scenario {scenario}");
        }
    }

    #[test]
    fn frozen_capture_blocks_inspected_address_after_passive_roster_clears() {
        let receiver = ReceiverEndpoint {
            key: "capture-fixture".into(),
            path: b"synthetic-receiver".to_vec(),
            label: "Synthetic receiver".into(),
            serial_number: Some("RECEIVER-ONLY".into()),
        };
        let endpoint = SerialEndpoint {
            port_name: "inspected-tracker".into(),
            label: "Synthetic tracker".into(),
            manufacturer: Some("SlimeVR".into()),
            product: Some("SlimeVR Tracker".into()),
            kind: SerialPortKind::Usb,
            serial_number: Some("TRACKER-ONLY".into()),
            vid: Some(0x1209),
            pid: Some(1),
            usb_interface: None,
        };
        let inventory = Inventory {
            receivers: vec![receiver.clone()],
            ports: vec![endpoint.clone()],
            ..Inventory::default()
        };
        let mut manager = test_support::display_manager();
        let mut monitor = ReceiverMonitor::fixture(receiver.clone(), 2, "112233445566", true);
        let calibration = CalibrationSession::default();
        let ticket = calibration
            .review(
                GuideSelection {
                    target: Target::Direct {
                        endpoint: endpoint.clone(),
                        generation: None,
                        address: Some("112233445566".into()),
                    },
                    label: "Inspected tracker".into(),
                },
                CalibrationTask::Stationary,
                &inventory,
                &manager,
                &monitor,
            )
            .unwrap();
        let unrelated = calibration
            .review(
                GuideSelection {
                    target: Target::Direct {
                        endpoint: endpoint.clone(),
                        generation: None,
                        address: Some("AABBCCDDEEFF".into()),
                    },
                    label: "Unrelated tracker".into(),
                },
                CalibrationTask::Stationary,
                &inventory,
                &manager,
                &monitor,
            )
            .unwrap();
        let source = CaptureSource {
            receiver: receiver.clone(),
            transport: CaptureTransport::RawHid {
                path: b"synthetic-raw".to_vec(),
            },
        };
        let preparation = CapturePreparation::freeze(
            source.clone(),
            CaptureMode::AllPaired { rate_hz: 100 },
            "unused.slimecap".into(),
            &inventory,
            &manager,
            &monitor,
        )
        .unwrap();
        let mut state = CaptureUiState {
            sources: vec![source],
            source: Some(0),
            target: Some((2, "112233445566".into())),
            acknowledged: true,
            ..Default::default()
        };
        assert!(!state.session.blocks_address(ticket.address().unwrap()));
        state.session.prepare(preparation, &mut monitor).unwrap();
        state.revoke_receiver_bindings(&receiver.key);
        assert_eq!(state.target, Some((2, "112233445566".into())));
        assert!(state.acknowledged);
        assert!(manager.session(&endpoint.port_name).is_none());
        assert!(
            monitor
                .observation(&receiver.key)
                .unwrap()
                .trackers
                .is_empty()
        );
        assert!(
            !state
                .session
                .blocks_port(&endpoint.port_name, None, Some(&endpoint))
        );
        assert!(state.session.blocks_address(ticket.address().unwrap()));
        assert!(!state.session.blocks_address(unrelated.address().unwrap()));
        state.session.stop(&mut manager, &mut monitor);
        assert!(!state.session.blocks_address(ticket.address().unwrap()));
    }

    #[test]
    fn preview_polls_only_visible_at_ten_hertz_and_reenters_immediately() {
        let mut state = CaptureUiState::default();
        let now = Instant::now();
        assert!(!state.preview_due(false, now));
        assert!(state.preview_due(true, now));
        assert!(!state.preview_due(true, now + Duration::from_millis(50)));
        assert!(state.preview_due(true, now + Duration::from_millis(100)));
        assert!(!state.preview_due(false, now + Duration::from_millis(125)));
        assert!(state.preview_due(true, now + Duration::from_millis(150)));
    }

    #[test]
    fn unchanged_worker_publication_does_not_request_redraw() {
        let mut state = CaptureUiState::default();
        let first = Arc::new(CaptureSnapshot::default());
        assert!(state.accept_preview_snapshot(first.clone()));
        assert!(!state.accept_preview_snapshot(first));
        assert!(state.accept_preview_snapshot(Arc::new(CaptureSnapshot::default())));
    }
}
