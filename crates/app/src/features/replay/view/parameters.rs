use super::preset_io::PresetEdit;
use super::*;
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};

fn replay_field_placeholder_key(index: usize) -> &'static str {
    if index == 1 {
        "replay.parameter.placeholder.raw_tx"
    } else {
        "replay.parameter.placeholder.metadata"
    }
}

impl ReplayFeature {
    pub(crate) fn locale_changed(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(input) = &self.state.preset_name {
            input.update(cx, |input, cx| {
                input.set_placeholder(tr("replay.preset.name"), window, cx);
            });
        }
        for (index, field) in self.state.fields.iter().enumerate() {
            field.update(cx, |input, cx| {
                input.set_placeholder(tr(replay_field_placeholder_key(index)), window, cx);
            });
        }
        cx.notify();
    }

    pub(super) fn initialize_view(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.state.focus.is_none() {
            self.state.focus = Some(cx.focus_handle().tab_stop(true));
            self.state.layout = Some(cx.new(|_| ResizableState::default()));
            self.state.preset_name = Some(
                cx.new(|cx| InputState::new(window, cx).placeholder(tr("replay.preset.name"))),
            );
        }
        if self.state.model.is_none() {
            self.state.model = Some(cx.new(model::ModelView::new));
            self.state.model_dirty = true;
        }
        if self.state.model_dirty {
            self.sync_replay_model(cx);
        }
        if self.state._scrub_subscriptions.is_empty() {
            self.state._scrub_subscriptions = vec![
                cx.observe_window_activation(window, |this, window, cx| {
                    if !window.is_window_active() {
                        this.cancel_replay_scrub(window, cx);
                    }
                }),
                cx.observe_window_bounds(window, |this, window, cx| {
                    this.cancel_replay_scrub(window, cx);
                }),
            ];
        }
        if !self.state.fields.is_empty() {
            return;
        }
        let mut values = vec![
            self.state.options.raw_rate_hz,
            self.state.options.gyro_rate_hz,
            self.state.options.accel_rate_hz,
            self.state.options.mag_rate_hz,
        ];
        values.extend(parameter_values(&self.state.options.params).map(Some));
        self.state.fields = values
            .into_iter()
            .enumerate()
            .map(|(index, value)| {
                cx.new(|cx| {
                    let mut input = InputState::new(window, cx)
                        .placeholder(tr(replay_field_placeholder_key(index)));
                    if let Some(value) = value {
                        input.set_value(value.to_string(), window, cx);
                    }
                    input
                })
            })
            .collect();
        self.state.field_subscriptions = self
            .state
            .fields
            .iter()
            .map(|field| {
                cx.subscribe_in(field, window, |this, _, event, _, cx| {
                    if matches!(event, InputEvent::Change) {
                        this.replay_settings_changed(cx);
                    }
                    if matches!(event, InputEvent::PressEnter { .. }) {
                        this.state.edit_deadline = Some(Instant::now());
                    }
                })
            })
            .collect();
    }

    pub(super) fn replay_settings_changed(&mut self, cx: &mut Context<Self>) {
        if self.stopped {
            return;
        }
        self.state.invalidate();
        self.state.settings_dirty = true;
        self.state.edit_deadline = Some(Instant::now() + Duration::from_millis(450));
        cx.notify();
    }

    pub(super) fn read_replay_options(&self, cx: &App) -> Result<ReplayOptions, String> {
        if self.state.fields.is_empty() {
            return Ok(self.state.options.clone());
        }
        let mut values = Vec::with_capacity(self.state.fields.len());
        for (index, field) in self.state.fields.iter().enumerate() {
            let text = field.read(cx).value();
            if text.trim().is_empty() && index < 4 {
                values.push(None);
                continue;
            }
            match text.trim().parse::<f32>() {
                Ok(value)
                    if value.is_finite() && (index >= 4 || (0.1..=32000.).contains(&value)) =>
                {
                    values.push(Some(value))
                }
                _ => {
                    return Err(format!(
                        "{}: {}",
                        tr(FIELD_LABELS[index]),
                        tr("replay.parameter.error.finite_number")
                    ));
                }
            }
        }
        if values.len() != FIELD_LABELS.len() {
            return Err(tr("replay.parameter.error.incomplete"));
        }
        let mut options = self.state.options.clone();
        options.raw_rate_hz = values[0];
        options.gyro_rate_hz = values[1];
        options.accel_rate_hz = values[2];
        options.mag_rate_hz = values[3];
        set_parameter_values(&mut options.params, &values[4..]);
        options.params.validate()?;
        Ok(options)
    }

    fn apply_replay_params(
        &mut self,
        params: replay::ReplayParams,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        for (field, value) in self
            .state
            .fields
            .iter()
            .skip(4)
            .zip(parameter_values(&params))
        {
            field.update(cx, |input, cx| {
                input.set_value(value.to_string(), window, cx)
            });
        }
        self.state.options.params = params;
        self.replay_settings_changed(cx);
    }

    #[inline(never)]
    fn replay_presets(&self, cx: &mut Context<Self>) -> AnyElement {
        let label = tr("replay.preset.load");
        let owner = cx.weak_entity();
        let selector = Button::new("replay-presets")
            .small()
            .label(label)
            .dropdown_menu(move |mut menu, _, cx| {
                let Ok(list) = owner.read_with(cx, |this, _| {
                    if this.stopped {
                        return None;
                    }
                    let mut list = presets::builtin_presets();
                    let builtin_count = list.len();
                    list.extend_from_slice(this.state.presets.snapshot());
                    Some((list, builtin_count))
                }) else {
                    return menu;
                };
                let Some((list, builtin_count)) = list else {
                    return menu;
                };
                for (index, preset) in list.into_iter().enumerate() {
                    let params = preset.params;
                    let name = preset.name;
                    let owner = owner.clone();
                    let label = replay_preset_label(&name, index < builtin_count);
                    menu = menu.item(PopupMenuItem::new(label).on_click(move |_, window, cx| {
                        let _ = owner.update(cx, |this, cx| {
                            if this.stopped {
                                return;
                            }
                            let selection = (index >= builtin_count).then(|| name.clone());
                            this.state.presets.select(selection);
                            if let Some(input) = &this.state.preset_name {
                                input.update(cx, |input, cx| {
                                    input.set_value(name.clone(), window, cx)
                                });
                            }
                            this.apply_replay_params(params.clone(), window, cx);
                        });
                    }));
                }
                menu
            });
        let mut row =
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_1()
                .child(selector)
                .child(div().w(px(150.)).child(
                    Input::new(self.state.preset_name.as_ref().expect("preset name")).small(),
                ));
        for (index, name) in [
            "replay.preset.save",
            "replay.preset.rename",
            "replay.preset.delete",
        ]
        .into_iter()
        .enumerate()
        {
            row = row.child(
                Button::new(("replay-preset-edit", index))
                    .small()
                    .label(tr(name))
                    .disabled(
                        self.stopped
                            || self.state.chooser.is_some()
                            || !self.state.presets.can_mutate()
                            || (index > 0 && self.state.presets.selected_custom_name().is_none()),
                    )
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.edit_replay_preset(index, cx)),
                    ),
            );
        }
        for (index, name) in ["replay.preset.import_yaml", "replay.preset.export_yaml"]
            .into_iter()
            .enumerate()
        {
            row = row.child(
                Button::new(("replay-yaml", index))
                    .small()
                    .label(tr(name))
                    .disabled(
                        self.stopped || self.state.chooser.is_some() || self.state.presets.busy(),
                    )
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.replay_yaml_dialog(index == 1, cx)),
                    ),
            );
        }
        row.into_any_element()
    }

    fn edit_replay_preset(&mut self, operation: usize, cx: &mut Context<Self>) {
        if self.stopped || self.state.chooser.is_some() || !self.state.presets.can_mutate() {
            return;
        }
        let name = self
            .state
            .preset_name
            .as_ref()
            .map(|input| input.read(cx).value().to_string())
            .unwrap_or_default();
        let selected = self.state.presets.selected_custom_name().map(str::to_owned);
        let edit = match operation {
            0 => {
                let params = match self.read_replay_options(cx) {
                    Ok(options) => options.params,
                    Err(error) => {
                        self.state.error = Some(error);
                        cx.notify();
                        return;
                    }
                };
                PresetEdit::Save { name, params }
            }
            1 => {
                let Some(old) = selected else {
                    return;
                };
                PresetEdit::Rename { old, new: name }
            }
            _ => {
                let Some(name) = selected else {
                    return;
                };
                PresetEdit::Delete { name }
            }
        };
        if let Some(join) = self.state.presets.edit(edit, cx.background_executor()) {
            self.watch_replay_preset_io(join, cx);
        }
    }

    fn replay_yaml_dialog(&mut self, export: bool, cx: &mut Context<Self>) {
        if self.state.chooser.is_some() || self.state.presets.busy() || self.stopped {
            return;
        }
        let params = match self.read_replay_options(cx) {
            Ok(options) => options.params,
            Err(error) => {
                self.state.error = Some(error);
                cx.notify();
                return;
            }
        };
        let generation = self.state.generation;
        if export {
            let dialog = cx.prompt_for_new_path(&PathBuf::from("."), Some("vqf.yaml"));
            self.state.chooser = Some(cx.spawn(async move |this, cx| {
                let chosen = dialog.await;
                let _ = this.update(cx, |this, cx| {
                    this.state.chooser = None;
                    if this.stopped || generation != this.state.generation {
                        return;
                    }
                    match chosen {
                        Ok(Ok(Some(path))) => {
                            if let Some(join) =
                                this.state
                                    .presets
                                    .export(path, params, cx.background_executor())
                            {
                                this.watch_replay_preset_io(join, cx);
                            }
                        }
                        Ok(Err(error)) => this.state.error = Some(error.to_string()),
                        Err(error) => this.state.error = Some(error.to_string()),
                        _ => {}
                    }
                    cx.notify();
                });
            }));
        } else {
            let dialog = cx.prompt_for_paths(PathPromptOptions {
                files: true,
                directories: false,
                multiple: false,
                prompt: Some(tr("replay.preset.import_yaml").into()),
            });
            self.state.chooser = Some(cx.spawn(async move |this, cx| {
                let chosen = dialog.await;
                let _ = this.update(cx, |this, cx| {
                    this.state.chooser = None;
                    if this.stopped || generation != this.state.generation {
                        return;
                    }
                    match chosen {
                        Ok(Ok(Some(paths))) => {
                            if let Some(path) = paths.into_iter().next()
                                && let Some(join) = this.state.presets.import(
                                    path,
                                    params,
                                    generation,
                                    cx.background_executor(),
                                )
                            {
                                this.watch_replay_preset_io(join, cx);
                            }
                        }
                        Ok(Err(error)) => this.state.error = Some(error.to_string()),
                        Err(error) => this.state.error = Some(error.to_string()),
                        _ => {}
                    }
                    cx.notify();
                });
            }));
        }
    }

    #[inline(never)]
    pub(super) fn replay_parameters(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut groups = div().flex().flex_wrap().gap_1();
        for (index, name) in [
            "replay.parameter.group.rates",
            "replay.parameter.group.fusion",
            "replay.parameter.group.bias",
            "replay.parameter.group.rest",
            "replay.parameter.group.magnetic",
        ]
        .into_iter()
        .enumerate()
        {
            groups = groups.child(
                Button::new(("replay-param-group", index))
                    .small()
                    .label(tr(name))
                    .selected(self.state.parameter_group == index)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.state.parameter_group = index;
                        cx.notify();
                    })),
            );
        }
        let range = match self.state.parameter_group {
            0 => 0..4,
            1 => 4..6,
            2 => 6..11,
            3 => 11..15,
            _ => 16..26,
        };
        let mut fields = div().flex().flex_wrap().gap_2();
        for index in range.chain((self.state.parameter_group == 2).then_some(15)) {
            fields = fields.child(
                div()
                    .w_40()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .min_h_10()
                            .flex_1()
                            .flex_shrink_0()
                            .whitespace_normal()
                            .text_xs()
                            .child(tr(FIELD_LABELS[index])),
                    )
                    .child(Input::new(&self.state.fields[index]).small()),
            );
        }
        let p = &self.state.options.params;
        let toggles = div()
            .flex()
            .flex_wrap()
            .gap_2()
            .child(
                Checkbox::new("replay-motion-bias")
                    .label(tr("replay.parameter.motion_bias"))
                    .checked(p.motion_bias_est_enabled)
                    .on_click(cx.listener(|this, v: &bool, _, cx| {
                        this.state.options.params.motion_bias_est_enabled = *v;
                        this.replay_settings_changed(cx);
                    })),
            )
            .child(
                Checkbox::new("replay-rest-bias")
                    .label(tr("replay.parameter.rest_bias"))
                    .checked(p.rest_bias_est_enabled)
                    .on_click(cx.listener(|this, v: &bool, _, cx| {
                        this.state.options.params.rest_bias_est_enabled = *v;
                        this.replay_settings_changed(cx);
                    })),
            )
            .child(
                Checkbox::new("replay-mag-rejection")
                    .label(tr("replay.parameter.magnetic_rejection"))
                    .checked(p.mag_dist_rejection_enabled)
                    .on_click(cx.listener(|this, v: &bool, _, cx| {
                        this.state.options.params.mag_dist_rejection_enabled = *v;
                        this.replay_settings_changed(cx);
                    })),
            );
        div()
            .p_2()
            .flex()
            .flex_col()
            .gap_2()
            .child(self.replay_presets(cx))
            .when_some(self.state.presets.load_error.as_ref(), |panel, error| {
                panel.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(error.clone()),
                )
            })
            .child(groups)
            .child(fields)
            .child(toggles)
            .into_any_element()
    }
}

const FIELD_LABELS: [&str; 26] = [
    "replay.parameter.raw_tx_rate",
    "replay.parameter.gyro_rate",
    "replay.parameter.acceleration_rate",
    "replay.parameter.magnetic_rate",
    "replay.parameter.acceleration_tau",
    "replay.parameter.magnetic_tau",
    "replay.parameter.bias_clip",
    "replay.parameter.bias_initial_sigma",
    "replay.parameter.bias_forgetting_time",
    "replay.parameter.bias_motion_sigma",
    "replay.parameter.bias_rest_sigma",
    "replay.parameter.rest_minimum_time",
    "replay.parameter.rest_filter_tau",
    "replay.parameter.rest_gyro_threshold",
    "replay.parameter.rest_acceleration_threshold",
    "replay.parameter.bias_vertical_forgetting",
    "replay.parameter.magnetic_current_tau",
    "replay.parameter.magnetic_norm_threshold",
    "replay.parameter.magnetic_dip_threshold",
    "replay.parameter.magnetic_reference_tau",
    "replay.parameter.magnetic_new_time",
    "replay.parameter.magnetic_first_time",
    "replay.parameter.magnetic_new_minimum_gyro",
    "replay.parameter.magnetic_minimum_undisturbed",
    "replay.parameter.magnetic_maximum_rejection",
    "replay.parameter.magnetic_rejection_factor",
];

pub(super) fn parameter_values(p: &replay::ReplayParams) -> [f32; 22] {
    [
        p.tau_acc,
        p.tau_mag,
        p.bias_clip,
        p.bias_sigma_init,
        p.bias_forgetting_time,
        p.bias_sigma_motion,
        p.bias_sigma_rest,
        p.rest_min_t,
        p.rest_filter_tau,
        p.rest_th_gyr,
        p.rest_th_acc,
        p.bias_vertical_forgetting_factor,
        p.mag_current_tau,
        p.mag_norm_th,
        p.mag_dip_th,
        p.mag_ref_tau,
        p.mag_new_time,
        p.mag_new_first_time,
        p.mag_new_min_gyr,
        p.mag_min_undisturbed_time,
        p.mag_max_rejection_time,
        p.mag_rejection_factor,
    ]
}

pub(super) fn set_parameter_values(p: &mut replay::ReplayParams, values: &[Option<f32>]) {
    let targets = [
        &mut p.tau_acc,
        &mut p.tau_mag,
        &mut p.bias_clip,
        &mut p.bias_sigma_init,
        &mut p.bias_forgetting_time,
        &mut p.bias_sigma_motion,
        &mut p.bias_sigma_rest,
        &mut p.rest_min_t,
        &mut p.rest_filter_tau,
        &mut p.rest_th_gyr,
        &mut p.rest_th_acc,
        &mut p.bias_vertical_forgetting_factor,
        &mut p.mag_current_tau,
        &mut p.mag_norm_th,
        &mut p.mag_dip_th,
        &mut p.mag_ref_tau,
        &mut p.mag_new_time,
        &mut p.mag_new_first_time,
        &mut p.mag_new_min_gyr,
        &mut p.mag_min_undisturbed_time,
        &mut p.mag_max_rejection_time,
        &mut p.mag_rejection_factor,
    ];
    for (target, value) in targets.into_iter().zip(values) {
        *target = value.expect("validated numeric parameter");
    }
}

fn builtin_preset_label_key(name: &str) -> Option<&'static str> {
    match name {
        "Native firmware baseline · 8aa61a6" => Some("replay.preset.builtin.native_baseline"),
        "VQF-C defaults" => Some("replay.preset.builtin.vqf_c_defaults"),
        "Web development profile" => Some("replay.preset.builtin.web_development"),
        "Web firmware tag 260824" => Some("replay.preset.builtin.web_260824"),
        "Web firmware tag 260708" => Some("replay.preset.builtin.web_260708"),
        "Web firmware tag 260513" => Some("replay.preset.builtin.web_260513"),
        "Web firmware tag 260402" => Some("replay.preset.builtin.web_260402"),
        "Web firmware tag 260301" => Some("replay.preset.builtin.web_260301"),
        _ => None,
    }
}

fn replay_preset_label(name: &str, builtin: bool) -> String {
    if builtin && let Some(key) = builtin_preset_label_key(name) {
        tr(key)
    } else {
        name.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::replay_preset_label;

    #[test]
    fn custom_preset_names_are_never_translation_keys() {
        for name in [
            "replay.preset.builtin.vqf_c_defaults",
            "VQF-C defaults",
            "Preset name",
            "common.action.cancel",
        ] {
            assert_eq!(replay_preset_label(name, false), name);
        }
    }
}
