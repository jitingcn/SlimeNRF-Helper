use std::{cell::RefCell, rc::Rc};

use gpui_kit::assets::IconName;
use gpui_kit::base::{
    Scrollbar, ScrollbarAxis, TestSupportExt as _, TextSelection, TextSelectionHandle,
};
use gpui_kit::component::{
    ActiveTheme, Icon, Selectable, Sizable, Theme, TitleBar,
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
};
use gpui_kit::{
    AnyElement, App, Bounds, ClipboardItem, Context, Div, FocusHandle, FontWeight, IntoElement,
    KeyBinding, KeyDownEvent, MouseButton, Pixels, Render, ScrollHandle, SharedString,
    Subscription, TitlebarOptions, Window, WindowBounds, WindowDecorations, WindowOptions, div,
    point, prelude::*, px, rems, size,
};
use slimenrf_terminal_core::TerminalBuffer;

use super::{
    AppAssets, Quit,
    components::{action_card, context_button},
    page_scroll::{page_scroll, reveal_on_focus},
    theme,
    tracker_cards::TrackerCardGrid,
};
use crate::{
    components::terminal_transcript::{TerminalTranscript, TranscriptState, TranscriptViewport},
    features::settings::store::{Appearance, Language, Preferences},
    i18n::tr,
};

// Logical pixels define the native window boundary; interior spacing uses rems.
const DEFAULT_WIDTH: f32 = 1100.;
const DEFAULT_HEIGHT: f32 = 760.;
const MIN_WIDTH: f32 = 760.;
const MIN_HEIGHT: f32 = 600.;
const LARGE_ZOOM: f32 = 1.25;

pub(crate) fn run() {
    gpui_kit::application().with_assets(AppAssets).run(|cx| {
        gpui_kit::init(cx);
        cx.bind_keys([
            #[cfg(target_os = "macos")]
            KeyBinding::new("cmd-q", Quit, None),
            #[cfg(target_os = "macos")]
            KeyBinding::new(
                "cmd-c",
                gpui_kit::component::input::Copy,
                Some("TerminalTranscript"),
            ),
            #[cfg(not(target_os = "macos"))]
            KeyBinding::new(
                "ctrl-c",
                gpui_kit::component::input::Copy,
                Some("TerminalTranscript"),
            ),
        ]);
        cx.on_action(|_: &Quit, cx| cx.quit());
        let bounds = Bounds::centered(None, size(px(DEFAULT_WIDTH), px(DEFAULT_HEIGHT)), cx);
        if let Err(error) = gpui_kit::open_window(
            WindowOptions {
                titlebar: Some(TitlebarOptions {
                    title: Some("SlimeNRF Helper — Development gallery".into()),
                    ..TitleBar::title_bar_options()
                }),
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(MIN_WIDTH), px(MIN_HEIGHT))),
                window_decorations: Some(WindowDecorations::Client),
                ..TitleBar::window_options()
            },
            cx,
            |window, cx| {
                let window_id = window.window_handle().window_id();
                cx.on_window_closed(move |cx, closed_id| {
                    if closed_id == window_id {
                        cx.quit();
                    }
                })
                .detach();
                cx.new(|cx| Gallery::new(window, cx))
            },
        ) {
            eprintln!("{}", rust_i18n::t!("app.error.open_window", error = error));
            cx.quit();
        }
        cx.activate(true);
    });
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scene {
    Controls,
    Terminal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sample {
    Alpha,
    Beta,
}

impl Sample {
    fn label(self) -> String {
        tr(match self {
            Self::Alpha => "gallery.card_alpha",
            Self::Beta => "gallery.card_beta",
        })
    }
}

struct Gallery {
    preferences: Preferences,
    base_font_size: Pixels,
    large_zoom: bool,
    scene: Scene,
    long_text: bool,
    detail: u8,
    selected: Sample,
    root_focus: FocusHandle,
    page_focus: FocusHandle,
    page_scroll: ScrollHandle,
    terminal: GalleryTerminal,
}

struct GalleryTerminal {
    buffer: TerminalBuffer,
    state: Rc<RefCell<TranscriptState>>,
    scroll: ScrollHandle,
    selection: TextSelectionHandle,
    focus: FocusHandle,
    _selection_subscription: Subscription,
    selection_epoch: u64,
    follow: bool,
    soft_wrap: bool,
    next_line: u64,
}

impl Gallery {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Preferences are an in-memory value only; neither settings::load nor save is called.
        let preferences = Preferences::default();
        theme::apply(&preferences, cx);
        let base_font_size = cx.theme().font_size;
        let selection = TextSelectionHandle::new(String::new(), cx);
        let state = Rc::new(RefCell::new(TranscriptState::default()));
        let selection_state = state.clone();
        let selection_handle = selection.clone();
        let selection_window = window.window_handle();
        let selection_subscription = selection.subscribe(
            move |_, cx| {
                selection_state.borrow_mut().project(&selection_handle, cx);
                let released = selection_handle
                    .snapshot(cx)
                    .filter(|snapshot| !snapshot.is_selecting());
                let handle = selection_handle.clone();
                let _ = selection_window.update(cx, move |_, window, cx| {
                    if let Some(snapshot) = released {
                        window.defer(cx, move |window, cx| {
                            if handle.snapshot(cx) == Some(snapshot) {
                                TextSelection::clear(window, cx);
                            }
                        });
                    }
                    window.refresh();
                });
            },
            cx,
        );
        let focus = cx.focus_handle().tab_stop(true);
        let selection_focus = focus.clone();
        let owner = cx.weak_entity();
        selection.focus_with(
            move |window, cx| {
                selection_focus.focus(window, cx);
                let _ = owner.update(cx, |this, cx| {
                    this.terminal.follow = false;
                    cx.notify();
                });
            },
            cx,
        );
        let mut buffer = TerminalBuffer::new();
        for line in 1..=24 {
            buffer.push(
                format!(
                    "[{line:02}] Synthetic output: long lines wrap when the window narrows; sample text 中文 and emoji 🦀 remain selectable.\n"
                )
                .as_bytes(),
            );
        }
        state.borrow_mut().sync_buffer(&buffer, 1);
        let selection_epoch = buffer.selection_epoch();
        // Root's contextual Tab binding needs an initial focus path even before
        // the user has visited a control. This scope is not itself a Tab stop.
        let root_focus = cx.focus_handle().tab_stop(false);
        root_focus.focus(window, cx);
        Self {
            preferences,
            base_font_size,
            large_zoom: false,
            scene: Scene::Controls,
            long_text: false,
            detail: 0,
            selected: Sample::Alpha,
            root_focus,
            page_focus: cx.focus_handle().tab_stop(false),
            page_scroll: ScrollHandle::new(),
            terminal: GalleryTerminal {
                buffer,
                state,
                scroll: ScrollHandle::new(),
                selection,
                focus,
                _selection_subscription: selection_subscription,
                selection_epoch,
                follow: true,
                soft_wrap: true,
                next_line: 25,
            },
        }
    }

    fn apply_options(&self, cx: &mut App) {
        theme::apply(&self.preferences, cx);
        let zoom = if self.large_zoom { LARGE_ZOOM } else { 1. };
        Theme::update(cx, |theme| theme.font_size = self.base_font_size * zoom);
    }

    fn select_scene(&mut self, scene: Scene, cx: &mut Context<Self>) {
        self.scene = scene;
        self.page_scroll.set_offset(point(px(0.), px(0.)));
        cx.notify();
    }

    fn synchronize_terminal(&mut self, window: &mut Window, cx: &mut App) {
        let terminal = &mut self.terminal;
        let epoch = terminal.buffer.selection_epoch();
        if terminal.selection_epoch != epoch {
            if terminal.selection.snapshot(cx).is_some() {
                TextSelection::clear(window, cx);
            }
            terminal.state.borrow_mut().clear_selection();
            terminal.selection_epoch = epoch;
        }
        terminal.state.borrow_mut().sync_buffer(&terminal.buffer, 1);
        if terminal.follow {
            terminal.scroll.scroll_to_bottom();
        }
    }

    fn append_output(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let line = self.terminal.next_line;
        self.terminal.next_line = line.wrapping_add(1);
        self.terminal.buffer.push(
            format!("[{line:02}] Added synthetic output: long content for wrapping and scrolling. 中文 🦀\n")
                .as_bytes(),
        );
        self.synchronize_terminal(window, cx);
        cx.notify();
    }

    fn clear_output(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.terminal.buffer.clear();
        self.synchronize_terminal(window, cx);
        cx.notify();
    }

    fn toolbar(&self, cx: &mut Context<Self>) -> Div {
        let scene = self.scene;
        let language = self.preferences.language;
        let appearance = self.preferences.appearance;
        let large_zoom = self.large_zoom;
        div()
            .flex()
            .flex_wrap()
            .items_start()
            .gap_4()
            .px_4()
            .py_3()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(group(
                tr("gallery.scenes"),
                div()
                    .flex()
                    .gap_1()
                    .child(
                        choice(
                            "gallery-scene-controls",
                            tr("gallery.controls"),
                            scene == Scene::Controls,
                            cx,
                        )
                        .on_click(
                            cx.listener(|this, _, _, cx| this.select_scene(Scene::Controls, cx)),
                        ),
                    )
                    .child(
                        choice(
                            "gallery-scene-terminal",
                            tr("gallery.terminal"),
                            scene == Scene::Terminal,
                            cx,
                        )
                        .on_click(
                            cx.listener(|this, _, _, cx| this.select_scene(Scene::Terminal, cx)),
                        ),
                    ),
            ))
            .child(group(
                tr("app.settings.language.title"),
                div()
                    .flex()
                    .gap_1()
                    .child(
                        choice(
                            "gallery-language-en",
                            tr("app.settings.language.english"),
                            language == Language::English,
                            cx,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.preferences.language = Language::English;
                            this.apply_options(cx);
                            cx.notify();
                        })),
                    )
                    .child(
                        choice(
                            "gallery-language-zh",
                            tr("app.settings.language.simplified_chinese"),
                            language == Language::SimplifiedChinese,
                            cx,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.preferences.language = Language::SimplifiedChinese;
                            this.apply_options(cx);
                            cx.notify();
                        })),
                    ),
            ))
            .child(group(
                tr("app.settings.appearance.title"),
                div()
                    .flex()
                    .gap_1()
                    .child(
                        choice(
                            "gallery-theme-light",
                            tr("app.settings.appearance.light"),
                            appearance == Appearance::Light,
                            cx,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.preferences.appearance = Appearance::Light;
                            this.apply_options(cx);
                            cx.notify();
                        })),
                    )
                    .child(
                        choice(
                            "gallery-theme-dark",
                            tr("app.settings.appearance.dark"),
                            appearance == Appearance::Dark,
                            cx,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.preferences.appearance = Appearance::Dark;
                            this.apply_options(cx);
                            cx.notify();
                        })),
                    ),
            ))
            .child(group(
                tr("gallery.zoom"),
                div()
                    .flex()
                    .gap_1()
                    .child(
                        choice("gallery-zoom-normal", tr("gallery.normal"), !large_zoom, cx)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.large_zoom = false;
                                this.apply_options(cx);
                                cx.notify();
                            })),
                    )
                    .child(
                        choice("gallery-zoom-large", tr("gallery.larger"), large_zoom, cx)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.large_zoom = true;
                                this.apply_options(cx);
                                cx.notify();
                            })),
                    ),
            ))
    }

    fn controls(&self, cx: &mut Context<Self>) -> AnyElement {
        let description = tr(if self.long_text {
            "gallery.long_description"
        } else {
            "gallery.short_description"
        });
        let detail = tr(match self.detail {
            0 => "gallery.detail_first",
            1 => "gallery.detail_second",
            _ => "gallery.detail_third",
        });
        let selected =
            rust_i18n::t!("gallery.selected", sample = self.selected.label()).to_string();
        let cards = vec![
            self.sample(
                "gallery-card-alpha",
                "gallery-alpha-reveal",
                Sample::Alpha,
                description,
                cx,
            ),
            self.sample(
                "gallery-card-beta",
                "gallery-beta-reveal",
                Sample::Beta,
                detail,
                cx,
            ),
            sample_card(
                "gallery-card-offline",
                tr("gallery.offline_card"),
                tr("gallery.offline_description"),
                false,
                cx,
            )
            .into_any_element(),
        ];
        let content = div()
            .w_full()
            .flex_shrink_0()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_4()
            .p_4()
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(tr("gallery.offline")),
            )
            .child(reveal_on_focus(
                "gallery-content-actions-reveal",
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .child(div().text_sm().child(tr("gallery.content")))
                    .child(
                        choice(
                            "gallery-content-short",
                            tr("gallery.short"),
                            !self.long_text,
                            cx,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.long_text = false;
                            cx.notify();
                        })),
                    )
                    .child(
                        choice(
                            "gallery-content-long",
                            tr("gallery.long"),
                            self.long_text,
                            cx,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.long_text = true;
                            cx.notify();
                        })),
                    )
                    .child(
                        Button::new("gallery-change-detail")
                            .small()
                            .outline()
                            .label(tr("gallery.change_detail"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.detail = (this.detail + 1) % 3;
                                cx.notify();
                            })),
                    ),
                &self.page_scroll,
            ))
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(tr("gallery.cards_title")),
            )
            .child(div().text_sm().child(selected))
            .child(div().w_full().min_w_0().max_w(rems(30.)).child(
                TrackerCardGrid::new("gallery-sample-grid".into(), cards).horizontal_padding(0.),
            ));
        page_scroll("gallery-controls-page", &self.page_focus, content)
            .scroll_handle(&self.page_scroll)
            .into_any_element()
    }

    fn sample(
        &self,
        id: &'static str,
        reveal_id: &'static str,
        sample: Sample,
        description: String,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let card = sample_card(id, sample.label(), description, true, cx)
            .selected(self.selected == sample)
            .toggled(self.selected == sample)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.selected = sample;
                cx.notify();
            }));
        reveal_on_focus(reveal_id, card, &self.page_scroll).into_any_element()
    }

    fn terminal(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let terminal = &self.terminal;
        let view = cx.weak_entity();
        let state = Rc::downgrade(&terminal.state);
        let on_reflow = Rc::new(move |_: &mut Window, cx: &mut App| {
            let Some(state) = state.upgrade() else {
                return;
            };
            let _ = view.update(cx, |this, cx| {
                let terminal = &this.terminal;
                if state.borrow_mut().settle_frame(
                    &terminal.scroll,
                    this.scene == Scene::Terminal && Rc::ptr_eq(&state, &terminal.state),
                    terminal.soft_wrap,
                    terminal.follow,
                ) {
                    cx.notify();
                }
            });
        });
        let transcript = div()
            .id("gallery-transcript-scroll")
            .role(gpui_kit::Role::Document)
            .aria_label("Synthetic terminal transcript")
            .size_full()
            .when(terminal.soft_wrap, |view| {
                view.overflow_y_scroll().overflow_x_hidden()
            })
            .when(!terminal.soft_wrap, |view| view.overflow_scroll())
            .px_2()
            .text_xs()
            .line_height(rems(1.25))
            .font_family(cx.theme().mono_font_family.clone())
            .whitespace_nowrap()
            .track_scroll(&terminal.scroll)
            .test_support()
            .track_focus(&terminal.focus)
            .key_context("TerminalTranscript")
            .on_scroll(cx.listener(|this, _, _, cx| {
                if this.terminal.follow {
                    this.terminal.follow = false;
                    cx.notify();
                }
            }))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if this.terminal.focus.is_focused(window)
                    && crate::components::terminal_transcript::handle_key_down(
                        &this.terminal.state,
                        event,
                        &this.terminal.scroll,
                        window,
                        cx,
                    )
                {
                    this.terminal.follow = false;
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    this.terminal.state.borrow_mut().clear_selection();
                    this.terminal.follow = false;
                    this.terminal.focus.focus(window, cx);
                    cx.notify();
                }),
            )
            .capture_action(
                cx.listener(|this, _: &gpui_kit::component::input::Copy, _, cx| {
                    if let Some(text) = this.terminal.state.borrow().selected_text() {
                        cx.write_to_clipboard(ClipboardItem::new_string(text));
                    }
                    cx.stop_propagation();
                }),
            )
            .child(TerminalTranscript::new(
                "gallery-transcript",
                terminal.selection.clone(),
                terminal.state.clone(),
                gpui_kit::base::Theme::global(cx).tokens.colors.selection,
                cx.theme().background,
                TranscriptViewport {
                    soft_wrap: terminal.soft_wrap,
                    follow: terminal.follow,
                    scroll: terminal.scroll.clone(),
                    focus: terminal.focus.clone(),
                    on_reflow,
                },
            ));
        let panel = div()
            .relative()
            .w_full()
            .h(rems(18.))
            .min_w_0()
            .rounded_md()
            .border_1()
            .border_color(if terminal.focus.is_focused(window) {
                cx.theme().ring
            } else {
                cx.theme().border
            })
            .bg(cx.theme().background)
            .child(if terminal.state.borrow().is_empty() {
                div()
                    .p_3()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(tr("gallery.empty"))
                    .into_any_element()
            } else {
                transcript.into_any_element()
            })
            .child(
                div().absolute().inset_0().child(
                    Scrollbar::new(&terminal.scroll)
                        .axis(if terminal.soft_wrap {
                            ScrollbarAxis::Vertical
                        } else {
                            ScrollbarAxis::Both
                        })
                        .viewport_from_layout(),
                ),
            );
        let content = div()
            .w_full()
            .flex_shrink_0()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(tr("gallery.offline")),
            )
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(tr("gallery.terminal")),
            )
            .child(
                div()
                    .text_sm()
                    .whitespace_normal()
                    .child(tr("gallery.terminal_hint")),
            )
            .child(reveal_on_focus(
                "gallery-terminal-actions-reveal",
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .child(
                        Button::new("gallery-terminal-append")
                            .small()
                            .outline()
                            .label(tr("gallery.append"))
                            .on_click(
                                cx.listener(|this, _, window, cx| this.append_output(window, cx)),
                            ),
                    )
                    .child(
                        Button::new("gallery-terminal-clear")
                            .small()
                            .outline()
                            .label(tr("gallery.clear"))
                            .on_click(
                                cx.listener(|this, _, window, cx| this.clear_output(window, cx)),
                            ),
                    )
                    .child(
                        Checkbox::new("gallery-terminal-follow")
                            .label(tr("serial.terminal.action.follow"))
                            .checked(terminal.follow)
                            .on_click(cx.listener(|this, checked, _, cx| {
                                this.terminal.follow = *checked;
                                if *checked {
                                    this.terminal.scroll.scroll_to_bottom();
                                }
                                cx.notify();
                            })),
                    )
                    .child(
                        Checkbox::new("gallery-terminal-wrap")
                            .label(tr("serial.terminal.action.wrap_lines"))
                            .checked(terminal.soft_wrap)
                            .on_click(cx.listener(|this, checked, _, cx| {
                                this.terminal.soft_wrap = *checked;
                                let mut offset = this.terminal.scroll.offset();
                                offset.x = px(0.);
                                this.terminal.scroll.set_offset(offset);
                                cx.notify();
                            })),
                    ),
                &self.page_scroll,
            ))
            .child(reveal_on_focus(
                "gallery-terminal-reveal",
                panel,
                &self.page_scroll,
            ));
        page_scroll("gallery-terminal-page", &self.page_focus, content)
            .scroll_handle(&self.page_scroll)
            .into_any_element()
    }
}

impl Render for Gallery {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = match self.scene {
            Scene::Controls => self.controls(cx),
            Scene::Terminal => self.terminal(window, cx),
        };
        div()
            .track_focus(&self.root_focus)
            .size_full()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(TitleBar::new().text_sm().child(tr("gallery.title")))
            .child(self.toolbar(cx))
            .child(content)
    }
}

fn choice(id: &'static str, label: String, selected: bool, cx: &App) -> Button {
    context_button(Button::new(id).small().ghost().label(label), selected, cx)
}

fn group(label: String, choices: impl IntoElement) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(div().text_xs().child(label))
        .child(choices)
}

fn sample_card(
    id: &'static str,
    title: String,
    description: String,
    enabled: bool,
    cx: &App,
) -> Button {
    let title: SharedString = title.into();
    action_card(id.into(), enabled, cx)
        .accessibility_label(title.clone())
        .child(
            div()
                .flex()
                .items_start()
                .w_full()
                .min_w_0()
                .h_auto()
                .gap_2()
                .whitespace_normal()
                .child(
                    div()
                        .w_5()
                        .flex_shrink_0()
                        .text_color(cx.theme().muted_foreground)
                        .child(Icon::new(IconName::Activity).size_5()),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .h_auto()
                        .gap_1()
                        .text_left()
                        .whitespace_normal()
                        .child(
                            div()
                                .min_w_0()
                                .whitespace_normal()
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(title),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .whitespace_normal()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(description),
                        ),
                ),
        )
}

#[cfg(test)]
#[path = "gallery/tests.rs"]
mod tests;
