use super::*;
use gpui_kit::base::h_resizable;

impl Workbench {
    pub(in crate::ui) fn hid_log_page(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.hid_fields(cx);
        if self.hid_log.dirty {
            self.hid_log.rebuild();
        }
        let list_focus = self
            .hid_log
            .list_focus
            .as_ref()
            .expect("HID log focus")
            .clone();
        let owner = cx.weak_entity();
        let list = uniform_list(
            "hid-log-list",
            self.hid_log.visible.len(),
            move |range, window, cx| {
                owner
                    .update(cx, |this, cx| {
                        let focused = this
                            .hid_log
                            .list_focus
                            .as_ref()
                            .is_some_and(|focus| focus.contains_focused(window, cx));
                        let active_index = this.hid_log.selected_visible_index().unwrap_or(0);
                        range
                            .map(|index| this.hid_log_row(index, active_index, focused, cx))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default()
            },
        )
        .track_scroll(&self.hid_log.scroll)
        .size_full()
        .on_scroll(cx.listener(|this, _, _, cx| {
            if this.hid_log.follow {
                this.hid_log.follow = false;
                cx.notify();
            }
        }))
        .test_support();
        let list_pane = div()
            .size_full()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .rounded_md()
            .border_1()
            .border_color(cx.theme().border)
            .overflow_hidden()
            .bg(cx.theme().background)
            .child(
                div()
                    .px_2()
                    .py_1()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        rust_i18n::t!(
                            "hid.history.count",
                            rows = self.hid_log.visible.len(),
                            lost = self.hid_log.dropped,
                            evicted = self.hid_log.evicted
                        )
                        .to_string(),
                    ),
            )
            .child(if self.hid_log.visible.is_empty() {
                div()
                    .flex_1()
                    .p_3()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(tr("hid.history.no_matches"))
                    .into_any_element()
            } else {
                div()
                    .id("hid-log-navigation")
                    .role(Role::List)
                    .aria_label(tr("hid.page.title"))
                    .aria_description(
                        "Use Up, Down, Home, End, Page Up, and Page Down to inspect reports.",
                    )
                    .test_support()
                    .track_focus(&list_focus)
                    .flex_1()
                    .min_h_0()
                    .border_1()
                    .border_color(cx.theme().background)
                    .focus(|style| style.border_color(cx.theme().ring))
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                        let scroll = this.hid_log.scroll.0.borrow();
                        let row_count = this.hid_log.visible.len();
                        let page_rows = scroll.last_item_size.map_or(1, |size| {
                            if row_count == 0 || size.contents.height <= Pixels::ZERO {
                                return 1;
                            }
                            let row_height = size.contents.height / row_count as f32;
                            (size.item.height / row_height).floor().max(1.) as usize
                        });
                        drop(scroll);
                        if this
                            .hid_log
                            .navigate_rows(event.keystroke.key.as_str(), page_rows)
                        {
                            cx.stop_propagation();
                            cx.notify();
                        }
                    }))
                    .child(list)
                    .into_any_element()
            });
        let detail = self.hid_log_detail(cx);
        let layout = self
            .hid_log
            .layout
            .as_ref()
            .expect("HID log layout")
            .clone();
        let mut page = div()
            .id("hid-log-page")
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .gap_2()
            .child(self.hid_log_toolbar(cx))
            .child(self.hid_log_filters(cx));
        if let Some(error) = &self.hid_log.error {
            page = page.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().danger)
                    .whitespace_normal()
                    .child(error.clone()),
            );
        }
        if let Some(notice) = &self.hid_log.notice {
            page = page.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .whitespace_normal()
                    .child(notice.clone()),
            );
        }
        if let Some(path) = &self.hid_log.recording_path {
            page = page.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!(
                        "{} · {}",
                        tr(if self.hid_log.stopping {
                            "hid.recording.saving"
                        } else {
                            "hid.recording.active"
                        }),
                        path.display()
                    )),
            );
        } else if self.hid_log.file_worker.is_some() || self.hid_log.chooser.is_some() {
            page = page.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(tr("hid.recording.saving")),
            );
        }
        page.child(
            div().flex_1().min_h_0().min_w_0().child(
                h_resizable("hid-log-split")
                    .with_state(&layout)
                    .with_handle_appearance(self.resize_handle_appearance.clone())
                    .child(
                        resizable_panel().size_range(px(230.)..Pixels::MAX).child(
                            div()
                                .size_full()
                                .min_w_0()
                                .min_h_0()
                                .pr_1()
                                .child(list_pane),
                        ),
                    )
                    .child(
                        resizable_panel()
                            .size(px(340.))
                            .flex_none()
                            .size_range(px(240.)..px(640.))
                            .child(div().size_full().min_w_0().min_h_0().pl_1().child(detail)),
                    ),
            ),
        )
        .into_any_element()
    }

    fn hid_log_toolbar(&self, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.hid_log.chooser.is_some() || self.hid_log.file_worker.is_some();
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(tr("hid.page.title")),
            )
            .child(
                Button::new("hid-log-save")
                    .small()
                    .label(tr("hid.history.save"))
                    .tooltip(tr("hid.history.save_hint"))
                    .disabled(busy || self.hid_log.reports.is_empty())
                    .on_click(cx.listener(|this, _, _, cx| this.choose_hid_file(false, cx))),
            )
            .child(
                Button::new("hid-log-record")
                    .small()
                    .label(tr("hid.recording.start"))
                    .tooltip(tr("hid.recording.start_hint"))
                    .disabled(!self.hid_log.can_start_recording())
                    .on_click(cx.listener(|this, _, _, cx| this.choose_hid_file(true, cx))),
            )
            .child(
                Button::new("hid-log-record-stop")
                    .small()
                    .label(tr("hid.recording.stop"))
                    .tooltip(tr("hid.recording.stop_hint"))
                    .disabled(!self.hid_log.can_stop_recording())
                    .on_click(cx.listener(|this, _, _, cx| this.hid_stop_recording(cx))),
            )
            .child(
                Checkbox::new("hid-log-follow")
                    .label(tr("hid.history.follow"))
                    .checked(self.hid_log.follow)
                    .on_click(cx.listener(|this, checked, _, cx| {
                        this.hid_log.follow = *checked;
                        if *checked && !this.hid_log.visible.is_empty() {
                            this.hid_log.scroll.scroll_to_item(
                                this.hid_log.visible.len() - 1,
                                ScrollStrategy::Bottom,
                            );
                        }
                        cx.notify();
                    })),
            )
            .child(
                Checkbox::new("hid-log-pause")
                    .label(tr("hid.history.pause"))
                    .checked(self.hid_log.paused)
                    .on_click(cx.listener(|this, checked, _, cx| {
                        this.hid_log.paused = *checked;
                        this.hid_log.rebuild();
                        cx.notify();
                    })),
            )
            .child(
                Button::new("hid-log-clear")
                    .small()
                    .label(tr("hid.history.clear"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.hid_log.sensors.clear();
                        this.hid_log.others.clear();
                        this.hid_log.reports.clear();
                        this.hid_log.events.clear();
                        this.hid_log.visible.clear();
                        this.hid_log.selected = None;
                        this.hid_log.dropped = 0;
                        this.hid_log.evicted = 0;
                        cx.notify();
                    })),
            )
            .into_any_element()
    }

    fn hid_log_row(
        &self,
        index: usize,
        active_index: usize,
        list_focused: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let row = self.hid_log.visible[index].clone();
        let selected = self.hid_log.selected.as_ref().is_some_and(|selected| {
            selected.record.sequence == row.record.sequence
                && selected.decoded.byte_offset == row.decoded.byte_offset
        });
        let active = active_index == index;
        let tracker = row
            .decoded
            .tracker_id
            .map(|id| format!(" · {} {id}", tr("hid.field.tracker_id")))
            .unwrap_or_default();
        let heading = format!(
            "{:.3}s · {}{} · {}",
            row.record.elapsed_us as f64 / 1_000_000.,
            self.hid_source_label(&row.record.source, row.record.source.label.clone()),
            tracker,
            row.decoded.category.label()
        );
        let summary = row.decoded.summary();
        let accessible_label = SharedString::from(format!("{heading} · {summary}"));
        let row_id = SharedString::from(format!(
            "hid-log-row:{}:{}",
            row.record.sequence, row.decoded.byte_offset
        ));
        let select_id = SharedString::from(format!(
            "hid-log-select:{}:{}",
            row.record.sequence, row.decoded.byte_offset
        ));
        let focus = self
            .hid_log
            .list_focus
            .as_ref()
            .expect("HID log focus")
            .clone();
        let background = if selected {
            cx.theme().secondary
        } else {
            cx.theme().background
        };
        div()
            .id(row_id)
            .role(Role::ListItem)
            .aria_label(accessible_label.clone())
            .aria_selected(selected)
            .when(active, |this| this.aria_active_descendant())
            .relative()
            .h_12()
            .w_full()
            .min_w_0()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                Button::new(select_id)
                    .ghost()
                    .tab_stop(false)
                    .selected(selected)
                    .accessibility_label(accessible_label)
                    .h_full()
                    .w_full()
                    .min_w_0()
                    .px_2()
                    .py_1()
                    .rounded_none()
                    .bg(background)
                    .text_color(cx.theme().foreground)
                    .child(
                        div()
                            .w_full()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .items_start()
                            .overflow_hidden()
                            .child(
                                div()
                                    .w_full()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .truncate()
                                    .child(heading),
                            )
                            .child(div().w_full().text_sm().truncate().child(summary)),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.hid_log.selected = Some(row.clone());
                        focus.focus(window, cx);
                        cx.notify();
                    })),
            )
            .when(list_focused && active, |this| {
                this.child(
                    div()
                        .absolute()
                        .inset_0()
                        .border_1()
                        .border_color(cx.theme().ring),
                )
            })
            .into_any_element()
    }

    fn hid_log_detail(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut detail = div()
            .id("hid-log-detail")
            .size_full()
            .min_w_0()
            .min_h_0()
            .rounded_md()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().group_box)
            .overflow_y_scroll()
            .child(
                div()
                    .p_2()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(tr("hid.detail.heading")),
            );
        let Some(row) = &self.hid_log.selected else {
            return detail
                .child(
                    div()
                        .p_2()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(tr("hid.detail.select_hint")),
                )
                .into_any_element();
        };
        let record = &row.record;
        let source = &record.source;
        let source_fields = [
            (tr("hid.field.sequence"), record.sequence.to_string()),
            (
                tr("hid.source.connection"),
                source.connection_id.to_string(),
            ),
            (
                tr("hid.source.serial_number"),
                source.serial_number.clone().unwrap_or_else(|| "—".into()),
            ),
            (
                tr("hid.source.usb_vid_pid"),
                format!("{:04X}:{:04X}", source.vendor_id, source.product_id),
            ),
            (
                tr("hid.source.usage_page"),
                format!("0x{:04X}", source.usage_page),
            ),
            (tr("hid.source.path"), hex(&source.path)),
            (
                tr("hid.source.byte_range"),
                format!(
                    "{}..{}",
                    row.decoded.byte_offset,
                    row.decoded.byte_offset + row.decoded.byte_len
                ),
            ),
        ];
        detail = detail.child(
            div()
                .px_2()
                .pb_1()
                .text_xs()
                .whitespace_normal()
                .text_color(cx.theme().muted_foreground)
                .child(format!(
                    "{} · {:?} · {:.3}s",
                    self.hid_source_label(source, source.label.clone()),
                    source.interface,
                    record.elapsed_us as f64 / 1_000_000.
                )),
        );
        let mut decoded_fields = row.decoded.details();
        if row.decoded.category == Category::Event {
            let priority = [
                "hid.field.action",
                "hid.field.outcome",
                "hid.field.phase",
                "hid.field.detail",
                "hid.field.event_kind",
                "hid.field.tracker_id",
                "hid.field.event_source",
            ]
            .map(tr);
            decoded_fields.sort_by_key(|(label, _)| {
                priority
                    .iter()
                    .position(|key| key == label)
                    .unwrap_or(priority.len())
            });
        }
        detail = detail.child(
            div()
                .px_2()
                .pb_2()
                .text_sm()
                .whitespace_normal()
                .child(row.decoded.summary()),
        );
        for (label, value) in decoded_fields {
            detail = detail.child(
                div()
                    .px_2()
                    .py_1()
                    .text_xs()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(div().text_color(cx.theme().muted_foreground).child(label))
                    .child(div().whitespace_normal().child(value)),
            );
        }
        if let RecordData::Report {
            bytes,
            direction,
            transferred,
            error,
        } = &record.data
        {
            let raw = hex(bytes.as_slice());
            let copy = raw.clone();
            detail = detail.child(
                div()
                    .px_2()
                    .py_2()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(tr("hid.detail.original_report"))
                            .child(
                                Button::new("hid-log-copy-raw")
                                    .small()
                                    .label(tr("hid.detail.copy_hex"))
                                    .on_click(cx.listener(move |_, _, _, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            copy.clone(),
                                        ))
                                    })),
                            ),
                    )
                    .child(div().text_xs().child(format!(
                        "{:?} · {} {} · {}: {}",
                        direction,
                        bytes.as_slice().len(),
                        tr("hid.detail.bytes"),
                        tr("hid.detail.transferred"),
                        transferred.map_or_else(|| "—".into(), |count| count.to_string())
                    )))
                    .when_some(error.clone(), |detail, error| {
                        detail.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().danger)
                                .whitespace_normal()
                                .child(error),
                        )
                    })
                    .child(
                        div()
                            .mt_2()
                            .text_xs()
                            .font_family(cx.theme().mono_font_family.clone())
                            .whitespace_normal()
                            .child(raw),
                    ),
            );
        }
        detail = detail.child(
            div()
                .px_2()
                .py_2()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .border_t_1()
                .border_color(cx.theme().border)
                .child(tr("hid.source.heading")),
        );
        for (label, value) in source_fields {
            detail = detail.child(
                div()
                    .px_2()
                    .py_1()
                    .text_xs()
                    .child(div().text_color(cx.theme().muted_foreground).child(label))
                    .child(div().whitespace_normal().child(value)),
            );
        }
        detail.into_any_element()
    }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut text = String::with_capacity(bytes.len() * 3);
    for (index, byte) in bytes.iter().enumerate() {
        if index != 0 {
            text.push(if index % 16 == 0 { '\n' } else { ' ' });
        }
        let _ = write!(&mut text, "{byte:02X}");
    }
    text
}
