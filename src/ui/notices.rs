use super::Workbench;
use crate::i18n::tr;
use gpui_kit::component::{
    ActiveTheme, Sizable,
    button::Button,
    scroll::{Scrollbar, ScrollbarAxis, ScrollbarMode},
};
use gpui_kit::{
    self as gpui, AnyWindowHandle, Bounds, ClipboardItem, Context, Entity, FontWeight,
    ListHorizontalSizingBehavior, Render, SharedString, Subscription, TitlebarOptions,
    UniformListScrollHandle, Window, WindowBounds, WindowOptions, div, prelude::*, px, size,
    uniform_list,
};
use unicode_width::UnicodeWidthStr;

const NOTICES: &str = include_str!(concat!(env!("OUT_DIR"), "/THIRD_PARTY_NOTICES.txt"));
const FONT_SIZE: f32 = 13.;
const LINE_HEIGHT: f32 = 20.;
const TAB_SPACES: &str = "    ";

#[derive(Default)]
pub(super) struct NoticesWindowState {
    open: Option<OpenNoticesWindow>,
}

struct OpenNoticesWindow {
    handle: AnyWindowHandle,
    content: Entity<NoticesWindow>,
    _close_observer: Subscription,
}

struct NoticesWindow {
    lines: Vec<&'static str>,
    widest_line: usize,
    scroll: UniformListScrollHandle,
}

impl NoticesWindow {
    fn new() -> Self {
        let mut lines = Vec::new();
        let mut widest_line = 0;
        let mut widest_columns = 0;
        for line in NOTICES.lines() {
            let columns = line
                .split('\t')
                .enumerate()
                .map(|(index, segment)| {
                    segment.width() + if index == 0 { 0 } else { TAB_SPACES.len() }
                })
                .sum::<usize>();
            if columns > widest_columns {
                widest_line = lines.len();
                widest_columns = columns;
            }
            lines.push(line);
        }
        Self {
            lines,
            widest_line,
            scroll: UniformListScrollHandle::new(),
        }
    }
}

impl Workbench {
    pub(super) fn open_notices_window(&mut self, cx: &mut Context<Self>) {
        if self.shutting_down {
            return;
        }
        if let Some(open) = &self.notices.open {
            if open
                .handle
                .update(cx, |_, window, _| window.activate_window())
                .is_ok()
            {
                return;
            }
            self.notices.open = None;
        }
        let bounds = Bounds::centered(None, size(px(960.), px(720.)), cx);
        match gpui::open_window(
            WindowOptions {
                titlebar: Some(TitlebarOptions {
                    title: Some(tr("notices.window.title").into()),
                    ..Default::default()
                }),
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(480.), px(320.))),
                ..Default::default()
            },
            cx,
            |_, cx| cx.new(|_| NoticesWindow::new()),
        ) {
            Ok((handle, content)) => {
                let parent = cx.weak_entity();
                let close_observer = cx.on_window_closed(move |cx, closed_id| {
                    let parent = parent.clone();
                    cx.defer(move |cx| {
                        let _ = parent.update(cx, |this, _| {
                            if this
                                .notices
                                .open
                                .as_ref()
                                .is_some_and(|open| open.handle.window_id() == closed_id)
                            {
                                this.notices.open = None;
                            }
                        });
                    });
                });
                self.notices.open = Some(OpenNoticesWindow {
                    handle,
                    content,
                    _close_observer: close_observer,
                });
                let _ = handle.update(cx, |_, window, _| window.activate_window());
            }
            Err(error) => {
                self.error = Some(format!("{}: {error}", tr("notices.error.open")));
                cx.notify();
            }
        }
    }

    pub(super) fn refresh_notices_locale(&self, cx: &mut Context<Self>) {
        if let Some(open) = &self.notices.open {
            let _ = open.handle.update(cx, |_, window, _| {
                window.set_window_title(&tr("notices.window.title"));
            });
            open.content.update(cx, |_, cx| cx.notify());
        }
    }

    pub(super) fn shutdown_notices(&mut self, cx: &mut Context<Self>) {
        if let Some(open) = self.notices.open.take() {
            let _ = open
                .handle
                .update(cx, |_, window, _| window.remove_window());
        }
    }
}

impl Render for NoticesWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let owner = cx.weak_entity();
        let list = uniform_list("notices-lines", self.lines.len(), move |range, _, cx| {
            owner
                .update(cx, |this, cx| {
                    range
                        .map(|index| {
                            let line = this.lines[index];
                            let text = if line.contains('\t') {
                                SharedString::from(line.replace('\t', TAB_SPACES))
                            } else {
                                SharedString::from(line)
                            };
                            div()
                                .h(px(LINE_HEIGHT))
                                .px_2()
                                .text_size(px(FONT_SIZE))
                                .line_height(px(LINE_HEIGHT))
                                .font_family(cx.theme().mono_font_family.clone())
                                .whitespace_nowrap()
                                .child(text)
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        })
        .with_width_from_item(Some(self.widest_line))
        .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
        .track_scroll(&self.scroll)
        .size_full()
        .pb(px(LINE_HEIGHT))
        .pr(px(LINE_HEIGHT));
        div()
            .size_full()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .flex_shrink_0()
                    .items_center()
                    .gap_3()
                    .p_3()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(tr("notices.window.title")),
                    )
                    .child(
                        Button::new("notices-copy-all")
                            .small()
                            .label(tr("notices.action.copy_all"))
                            .on_click(cx.listener(|_, _, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(
                                    NOTICES.to_owned(),
                                ));
                            })),
                    ),
            )
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .overflow_hidden()
                    .child(list)
                    .child(
                        div().absolute().inset_0().child(
                            Scrollbar::new(&self.scroll)
                                .axis(ScrollbarAxis::Both)
                                .mode(ScrollbarMode::Always)
                                .viewport_from_layout(),
                        ),
                    ),
            )
    }
}
