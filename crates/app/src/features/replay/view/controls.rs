use super::playback::ReferencePoint;
use super::*;
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};

impl ReplayFeature {
    #[inline(never)]
    pub(super) fn replay_toolbar(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex()
            .items_center()
            .flex_wrap()
            .gap_2()
            .child(
                Button::new("replay-import")
                    .label(tr("replay.action.import"))
                    .small()
                    .disabled(self.state.chooser.is_some() || self.state.presets.importing())
                    .on_click(cx.listener(|this, _, _, cx| this.choose_replay(cx))),
            )
            .child(
                Button::new("replay-export")
                    .label(tr("replay.action.export_zip"))
                    .small()
                    .disabled(
                        self.state.recording.is_none()
                            || self.state.busy()
                            || self.state.chooser.is_some()
                            || self.state.presets.importing(),
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.export_replay(cx))),
            )
            .child(
                Button::new("replay-cancel")
                    .label(tr("common.action.cancel"))
                    .small()
                    .disabled(!self.state.busy())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.state.invalidate();
                        this.state.notice = Some(tr("replay.notice.cancelled"));
                        cx.notify();
                    })),
            )
            .child(
                Button::new("replay-settings")
                    .label(tr("replay.action.parameters"))
                    .small()
                    .selected(self.state.settings_open)
                    .disabled(self.state.recording.is_none())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.state.settings_open = !this.state.settings_open;
                        cx.notify();
                    })),
            )
            .child(
                Button::new("replay-details")
                    .label(tr("replay.action.details"))
                    .small()
                    .selected(self.state.details_open)
                    .disabled(self.state.recording.is_none())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.state.details_open = !this.state.details_open;
                        cx.notify();
                    })),
            )
            .child(
                Button::new("replay-recompute")
                    .small()
                    .label(tr("replay.action.recompute"))
                    .disabled(self.state.recording.is_none())
                    .on_click(cx.listener(|this, _, _, cx| this.compute_replay(cx))),
            )
            .when_some(self.state.path.as_ref(), |row, path| {
                let full = path.display().to_string();
                let filename = path
                    .file_name()
                    .map_or_else(|| full.clone(), |name| name.to_string_lossy().into_owned());
                row.child(
                    div()
                        .id("replay-source")
                        .max_w(px(160.))
                        .truncate()
                        .text_sm()
                        .child(filename)
                        .tooltip(move |window, cx| Tooltip::new(full.clone()).build(window, cx)),
                )
            })
            .into_any_element()
    }

    #[inline(never)]
    pub(super) fn replay_calibration(&self, cx: &mut Context<Self>) -> AnyElement {
        let owner = cx.weak_entity();
        let recording = self.state.recording.clone();
        let label = recording
            .as_ref()
            .and_then(|r| r.tracks.get(self.state.track))
            .map_or_else(
                || tr("common.device_role.tracker"),
                |t| {
                    format!(
                        "{} {} · #{} · {} {}",
                        tr("replay.track"),
                        self.state.track + 1,
                        t.identity.slot,
                        t.samples.len(),
                        tr("replay.samples")
                    )
                },
            );
        let track_button = Button::new("replay-track-select")
            .small()
            .label(label)
            .dropdown_menu(move |mut menu, _, _| {
                let Some(recording) = &recording else {
                    return menu;
                };
                for (index, track) in recording.tracks.iter().enumerate() {
                    let owner = owner.clone();
                    let name = format!(
                        "{} {} · {} #{} · {} {}",
                        tr("replay.track"),
                        index + 1,
                        tr("common.device_role.tracker"),
                        track.identity.slot,
                        track.samples.len(),
                        tr("replay.samples")
                    );
                    menu = menu.item(PopupMenuItem::new(name).on_click(move |_, _, cx| {
                        let _ = owner.update(cx, |this, cx| {
                            if this.state.track == index {
                                return;
                            }
                            this.state.invalidate();
                            this.state.track = index;
                            this.state.processed = None;
                            this.state.cursor = 0;
                            this.state.playback_time = 0.;
                            this.state.reference_a = None;
                            this.state.reference_b = None;
                            this.state.auto_attempted = false;
                            this.state.requested_range = None;
                            this.state.hover_time = None;
                            this.state.hover_chart = None;
                            this.compute_replay(cx);
                        });
                    }));
                }
                menu
            });
        let mut row = div()
            .flex()
            .items_center()
            .flex_wrap()
            .gap_2()
            .text_xs()
            .child(track_button);
        let available = self
            .state
            .processed
            .as_ref()
            .map(|data| data.result.calibration_available)
            .or_else(|| {
                self.state
                    .recording
                    .as_ref()?
                    .tracks
                    .get(self.state.track)
                    .map(|t| replay::calibration_availability(&t.calibration))
            })
            .unwrap_or_default();
        row = row.child(
            Checkbox::new("replay-accel-cal")
                .label(tr("replay.calibration.acceleration"))
                .checked(self.state.options.calibrate_accel && available.accel)
                .disabled(!available.accel)
                .on_click(cx.listener(|this, v: &bool, _, cx| {
                    this.state.options.calibrate_accel = *v;
                    this.replay_settings_changed(cx);
                })),
        );
        row = row.child(
            Checkbox::new("replay-mag-cal")
                .label(tr("replay.calibration.magnetic"))
                .checked(self.state.options.calibrate_mag && available.mag)
                .disabled(!available.mag)
                .on_click(cx.listener(|this, v: &bool, _, cx| {
                    this.state.options.calibrate_mag = *v;
                    this.replay_settings_changed(cx);
                })),
        );
        row = row.child(
            Checkbox::new("replay-use-mag")
                .label(tr("replay.calibration.use_magnetometer"))
                .checked(self.state.options.use_magnetometer)
                .on_click(cx.listener(|this, v: &bool, _, cx| {
                    this.state.options.use_magnetometer = *v;
                    this.replay_settings_changed(cx);
                })),
        );
        let gyro_mode = self
            .state
            .processed
            .as_ref()
            .filter(|_| !self.state.settings_dirty)
            .map_or(self.state.options.gyro_calibration, |data| {
                data.result.options.gyro_calibration
            });
        let label = match gyro_mode {
            GyroCalibration::None => "replay.calibration.gyro_none",
            GyroCalibration::Bias => "replay.calibration.gyro_bias",
            GyroCalibration::Thermal => "replay.calibration.gyro_thermal",
        };
        row = row.child(
            Button::new("replay-gyro-cal")
                .small()
                .label(tr(label))
                .disabled(
                    !available.gyro_bias && !available.gyro_sensitivity && !available.gyro_thermal,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.state.options.gyro_calibration = match gyro_mode {
                        GyroCalibration::None
                            if available.gyro_bias || available.gyro_sensitivity =>
                        {
                            GyroCalibration::Bias
                        }
                        GyroCalibration::None | GyroCalibration::Bias if available.gyro_thermal => {
                            GyroCalibration::Thermal
                        }
                        _ => GyroCalibration::None,
                    };
                    this.replay_settings_changed(cx);
                })),
        );
        if self.state.settings_dirty {
            row = row.child(tr("replay.settings.pending"));
        }
        row.into_any_element()
    }

    #[inline(never)]
    pub(super) fn replay_viewport_controls(&self, cx: &mut Context<Self>) -> AnyElement {
        let disabled = self.stopped
            || self.state.processed.is_none()
            || self.state.settings_dirty
            || self.state.busy();
        let mut row = div()
            .flex()
            .items_center()
            .flex_wrap()
            .gap_2()
            .p_2()
            .flex_shrink_0()
            .on_key_down(|event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "space" {
                    // Button activates on keyup; do not also invoke the page's play shortcut.
                    cx.stop_propagation();
                }
            });
        for (id, label, zoom, pan) in [
            ("replay-zoom-in", "replay.plot.zoom_in", 0.8, 0.),
            ("replay-zoom-out", "replay.plot.zoom_out", 1.25, 0.),
            ("replay-pan-earlier", "replay.plot.pan_earlier", 1., -0.5),
            ("replay-pan-later", "replay.plot.pan_later", 1., 0.5),
        ] {
            row = row.child(
                Button::new(id)
                    .small()
                    .label(tr(label))
                    .disabled(disabled)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.change_replay_viewport(zoom, pan, cx);
                    })),
            );
        }
        row.child(
            Button::new("replay-fit")
                .small()
                .label(tr("replay.plot.fit"))
                .disabled(disabled)
                .on_click(cx.listener(|this, _, _, cx| this.reset_replay_plot(cx))),
        )
        .into_any_element()
    }

    #[inline(never)]
    pub(super) fn replay_timeline(&self, duration: f64, cx: &mut Context<Self>) -> AnyElement {
        let disabled =
            self.state.processed.is_none() || self.state.settings_dirty || self.state.busy();
        let mut row = div()
            .w(px(516.))
            .flex_shrink_0()
            .flex()
            .items_center()
            .flex_wrap()
            .gap_1();
        row = row.child(
            Button::new("replay-home")
                .small()
                .w(px(32.))
                .label("|<")
                .tooltip(tr("replay.playback.first_sample"))
                .disabled(disabled)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.state.pause();
                    this.state.seek(0.);
                    this.sync_replay_model(cx);
                    cx.notify();
                })),
        );
        row = row.child(
            Button::new("replay-prev")
                .small()
                .w(px(32.))
                .label("<")
                .tooltip(tr("replay.playback.previous_sample"))
                .disabled(disabled)
                .on_click(cx.listener(|this, _, _, cx| this.step_replay(false, cx))),
        );
        row = row.child(
            Button::new("replay-play")
                .small()
                .w(px(68.))
                .label(tr(if self.state.playing {
                    "replay.playback.pause"
                } else {
                    "replay.playback.play"
                }))
                .disabled(disabled)
                .on_click(cx.listener(|this, _, window, cx| this.toggle_replay_play(window, cx))),
        );
        row = row.child(
            Button::new("replay-next")
                .small()
                .w(px(32.))
                .label(">")
                .tooltip(tr("replay.playback.next_sample"))
                .disabled(disabled)
                .on_click(cx.listener(|this, _, _, cx| this.step_replay(true, cx))),
        );
        let owner = cx.weak_entity();
        row = row.child(
            Button::new("replay-speed")
                .small()
                .w(px(52.))
                .label(format!("{}×", self.state.speed))
                .dropdown_menu(move |mut menu, _, _| {
                    for speed in [0.25, 0.5, 1., 2., 4., 8.] {
                        let owner = owner.clone();
                        menu = menu.item(PopupMenuItem::new(format!("{speed}×")).on_click(
                            move |_, _, cx| {
                                let _ = owner.update(cx, |this, cx| this.set_speed(speed, cx));
                            },
                        ));
                    }
                    menu
                }),
        );
        row = row.child(
            Button::new("replay-loop")
                .small()
                .w(px(56.))
                .label(tr("replay.playback.loop"))
                .selected(self.state.looping)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.state.looping = !this.state.looping;
                    cx.notify();
                })),
        );
        row = row.child(
            Button::new("replay-orientation")
                .small()
                .w(px(36.))
                .label(if self.state.nine { "9D" } else { "6D" })
                .disabled(
                    self.state
                        .processed
                        .as_ref()
                        .is_none_or(|data| !data.summary.has_9d),
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.state.nine = !this.state.nine;
                    this.sync_replay_model(cx);
                    cx.notify();
                })),
        );
        row = row.child(
            div()
                .w(px(180.))
                .flex_shrink_0()
                .font_family(cx.theme().mono_font_family.clone())
                .whitespace_nowrap()
                .text_xs()
                .child(format!("{:.2} / {duration:.2}s", self.state.playback_time)),
        );
        let transport = row.into_any_element();
        let mut row = div()
            .w(px(360.))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_end()
            .flex_wrap()
            .gap_1();
        row = row.child(
            Button::new("replay-auto-ab")
                .small()
                .w(px(84.))
                .label(tr("replay.reference.auto"))
                .disabled(disabled)
                .on_click(cx.listener(|this, _, _, cx| this.auto_replay_references(cx))),
        );
        for (index, label) in [
            "replay.reference.a",
            "replay.reference.b",
            "replay.reference.clear",
        ]
        .into_iter()
        .enumerate()
        {
            row = row.child(
                Button::new(("replay-reference", index))
                    .small()
                    .w(px(if index < 2 { 28. } else { 56. }))
                    .label(tr(label))
                    .when(index < 2, |button| {
                        button
                            .text_color(presentation::reference_color(index, cx.theme().is_dark()))
                    })
                    .disabled(disabled)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        match index {
                            0 => this.state.set_reference(ReferencePoint::A),
                            1 => this.state.set_reference(ReferencePoint::B),
                            _ => this.state.clear_references(),
                        }
                        cx.notify();
                    })),
            );
        }
        row = row.child(
            Button::new("replay-y-scale")
                .small()
                .w(px(84.))
                .label(tr(if self.state.full_scale {
                    "replay.plot.full_scale"
                } else {
                    "replay.plot.robust_scale"
                }))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.state.full_scale = !this.state.full_scale;
                    cx.notify();
                })),
        );
        div()
            .w_full()
            .min_w_0()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap_1()
            .border_t_1()
            .border_color(cx.theme().border)
            .child(presentation::timeline_canvas(
                (0., duration),
                self.replay_overlay(),
                cx.weak_entity(),
                cx.theme().border,
                cx.theme().is_dark(),
            ))
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .justify_between()
                    .flex_wrap()
                    .gap_2()
                    .child(transport)
                    .child(row),
            )
            .into_any_element()
    }
}
