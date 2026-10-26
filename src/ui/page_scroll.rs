use gpui_kit::base::{ElementExt as _, InteractiveElementExt as _};
use gpui_kit::component::{
    ActiveTheme,
    input::{MovePageDown, MovePageUp},
    scroll::ScrollableElement,
};
use gpui_kit::{
    AnyElement, App, Bounds, ElementId, FocusHandle, IntoElement, KeyDownEvent, Pixels, RenderOnce,
    ScrollHandle, Subscription, Window, div, prelude::*,
};
use std::{cell::Cell, rc::Rc};

#[derive(Clone)]
pub(super) struct FocusReveal {
    scope: FocusHandle,
    pending: Rc<Cell<bool>>,
}

impl FocusReveal {
    pub(super) fn scope(&self) -> &FocusHandle {
        &self.scope
    }

    pub(super) fn reveal(
        &self,
        bounds: Bounds<Pixels>,
        scroll: &ScrollHandle,
        window: &mut Window,
        cx: &App,
    ) {
        if !self.pending.replace(false) || !self.scope.contains_focused(window, cx) {
            return;
        }
        let viewport = scroll.bounds();
        let gutter = (window.rem_size() * 0.25).min(viewport.size.height * 0.5);
        let top = viewport.top() + gutter;
        let bottom = viewport.bottom() - gutter;
        let delta = if bounds.size.height > bottom - top || bounds.top() < top {
            top - bounds.top()
        } else if bounds.bottom() > bottom {
            bottom - bounds.bottom()
        } else {
            Pixels::ZERO
        };
        let previous = scroll.offset();
        let mut offset = previous;
        offset.y = (offset.y + delta).clamp(-scroll.max_offset().y, Pixels::ZERO);
        if offset != previous {
            scroll.set_offset(offset);
            // Prepaint bounds already include the scroll translation.
            window.on_next_frame(|window, _| {
                window.refresh();
            });
        }
    }
}

pub(super) struct ControlFocus {
    reveal: FocusReveal,
    _subscription: Subscription,
}

impl ControlFocus {
    pub(super) fn new(window: &mut Window, cx: &mut App) -> Self {
        let scope = cx.focus_handle().tab_stop(false);
        let pending = Rc::new(Cell::new(false));
        let request = pending.clone();
        let subscription = window.on_focus_in(&scope, cx, move |window, _| {
            request.set(true);
            // Focus notifications run during drawing, when refresh is suppressed.
            window.on_next_frame(|window, _| {
                window.refresh();
            });
        });
        Self {
            reveal: FocusReveal { scope, pending },
            _subscription: subscription,
        }
    }

    pub(super) fn reveal(&self) -> FocusReveal {
        self.reveal.clone()
    }
}

#[derive(IntoElement)]
pub(super) struct RevealOnFocus {
    id: &'static str,
    content: AnyElement,
    scroll: ScrollHandle,
}

pub(super) fn reveal_on_focus(
    id: &'static str,
    content: impl IntoElement,
    scroll: &ScrollHandle,
) -> RevealOnFocus {
    RevealOnFocus {
        id,
        content: content.into_any_element(),
        scroll: scroll.clone(),
    }
}

impl RenderOnce for RevealOnFocus {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        // Resolve keyed identity here, under the rendered device row's scope.
        let focus = window
            .use_keyed_state(
                (ElementId::from(self.id), "focus-reveal"),
                cx,
                |window, cx| ControlFocus::new(window, cx),
            )
            .read(cx)
            .reveal();
        let scroll = self.scroll;
        div()
            .id(self.id)
            .relative()
            .min_w_0()
            .track_focus(focus.scope())
            .on_prepaint(move |bounds, window, cx| focus.reveal(bounds, &scroll, window, cx))
            .child(self.content)
    }
}

#[derive(IntoElement)]
pub(super) struct PageScroll {
    id: &'static str,
    focus: FocusHandle,
    content: AnyElement,
    scroll: Option<ScrollHandle>,
    focus_scope: bool,
}

pub(super) fn page_scroll(
    id: &'static str,
    focus: &FocusHandle,
    content: impl IntoElement,
) -> PageScroll {
    PageScroll {
        id,
        focus: focus.clone(),
        content: content.into_any_element(),
        scroll: None,
        focus_scope: true,
    }
}

impl PageScroll {
    pub(super) fn scroll_handle(mut self, scroll: &ScrollHandle) -> Self {
        self.scroll = Some(scroll.clone());
        self
    }

    pub(super) fn focus_scope(mut self, enabled: bool) -> Self {
        self.focus_scope = enabled;
        self
    }
}

#[derive(Clone, Copy)]
enum PageMove {
    Up,
    Down,
    Start,
    End,
}

impl PageMove {
    fn from_key(key: &str) -> Option<Self> {
        match key {
            "pageup" => Some(Self::Up),
            "pagedown" => Some(Self::Down),
            "home" => Some(Self::Start),
            "end" => Some(Self::End),
            _ => None,
        }
    }

    fn offset(self, current: Pixels, viewport: Pixels, extent: Pixels) -> Pixels {
        let next = match self {
            Self::Up => current + viewport * 0.9,
            Self::Down => current - viewport * 0.9,
            Self::Start => Pixels::ZERO,
            Self::End => -extent,
        };
        next.clamp(-extent, Pixels::ZERO)
    }
}

pub(super) fn handle_page_key(
    event: &KeyDownEvent,
    scroll: &ScrollHandle,
    window: &mut Window,
    cx: &mut App,
) {
    // Focused controls resolve their own navigation bindings before
    // unhandled key events bubble to this page region.
    if event.keystroke.modifiers.number_of_modifiers() != 0 {
        return;
    }
    let Some(movement) = PageMove::from_key(event.keystroke.key.as_str()) else {
        return;
    };
    let mut offset = scroll.offset();
    offset.y = movement.offset(offset.y, scroll.bounds().size.height, scroll.max_offset().y);
    scroll.set_offset(offset);
    window.refresh();
    cx.stop_propagation();
}

impl RenderOnce for PageScroll {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let scroll = self.scroll.unwrap_or_else(|| {
            window
                .use_keyed_state((ElementId::from(self.id), "scroll"), cx, |_, _| {
                    ScrollHandle::new()
                })
                .read(cx)
                .clone()
        });
        let keyboard_scroll = scroll.clone();
        div()
            .id(self.id)
            .flex_1()
            .h_full()
            .min_w_0()
            .min_h_0()
            .relative()
            .border_1()
            .border_color(cx.theme().border.opacity(0.))
            .flex()
            .flex_col()
            .overflow_y_scroll()
            .lock_scroll_axis()
            .track_scroll(&scroll)
            .when(self.focus_scope, |this| {
                this.track_focus(&self.focus)
                    .focus_visible(|style| style.border_color(cx.theme().ring))
                    // Single-line Input binds these actions but leaves them unhandled;
                    // consume them here instead of treating them as page navigation.
                    .on_action(|_: &MovePageUp, _, cx| cx.stop_propagation())
                    .on_action(|_: &MovePageDown, _, cx| cx.stop_propagation())
                    .on_key_down(move |event, window, cx| {
                        handle_page_key(event, &keyboard_scroll, window, cx);
                    })
            })
            .child(self.content)
            .vertical_scrollbar(&scroll)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::component::{
        button::Button,
        input::{Input, InputState, Textarea, TextareaState},
    };
    use gpui_kit::{
        Bounds, Context, Entity, Render, TestAppContext, WindowBounds, WindowOptions, size,
        test::{TestSupportExt as _, TestWindowExt as _},
    };
    use gpui_kit::{Focusable as _, px};

    #[test]
    fn page_moves_keep_context_and_clamp_at_both_edges() {
        assert_eq!(PageMove::Down.offset(px(0.), px(200.), px(500.)), px(-180.));
        assert_eq!(PageMove::Up.offset(px(-180.), px(200.), px(500.)), px(0.));
        assert_eq!(PageMove::Up.offset(px(-10.), px(200.), px(500.)), px(0.));
        assert_eq!(
            PageMove::Down.offset(px(-490.), px(200.), px(500.)),
            px(-500.)
        );
        assert_eq!(
            PageMove::Start.offset(px(-490.), px(200.), px(500.)),
            px(0.)
        );
        assert_eq!(
            PageMove::End.offset(px(-10.), px(200.), px(500.)),
            px(-500.)
        );
        assert_eq!(PageMove::End.offset(px(0.), px(200.), px(0.)), px(0.));
    }

    struct PageHarness {
        focus: FocusHandle,
        input: Entity<InputState>,
        textarea: Entity<TextareaState>,
    }

    impl Render for PageHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().size_full().flex().flex_col().child(page_scroll(
                "scroll-test-page",
                &self.focus,
                div()
                    .w_full()
                    .flex_shrink_0()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .p_4()
                    .child(Button::new("scroll-top").label("Top control"))
                    .child(Input::new(&self.input).id("scroll-input"))
                    .child(
                        div()
                            .id("scroll-textarea")
                            .test_support()
                            .child(Textarea::new(&self.textarea)),
                    )
                    .child(div().h(px(800.)).flex_shrink_0())
                    .child(Button::new("scroll-bottom").label("Bottom control")),
            ))
        }
    }

    #[gpui_kit::test]
    fn page_keys_reveal_lower_controls_without_taking_editor_navigation(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (handle, harness) = cx.update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        Default::default(),
                        size(px(480.), px(400.)),
                    ))),
                    ..Default::default()
                },
                cx,
                |window, cx| {
                    cx.new(|cx| PageHarness {
                        focus: cx.focus_handle(),
                        input: cx.new(|cx| {
                            let mut input = InputState::new(window, cx);
                            input.set_value("Alpha", window, cx);
                            input
                        }),
                        textarea: cx.new(|cx| {
                            let mut textarea = TextareaState::new(window, cx).auto_grow(2, 2);
                            textarea.set_value("First line\nSecond line", window, cx);
                            textarea
                        }),
                    })
                },
            )
            .expect("open page scrolling fixture")
        });
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            harness.read(cx).focus.clone().focus(window, cx);
            window.render_frame(cx);
            assert!(window.find("scroll-top").visible());
            assert!(!window.find("scroll-bottom").visible());

            window.press("end", cx);
            assert!(window.find("scroll-bottom").visible());
            assert!(!window.find("scroll-top").visible());
            window.press("home", cx);
            assert!(window.find("scroll-top").visible());
            window.press("pagedown", cx);
            assert!(!window.find("scroll-top").visible());
            window.press("pageup", cx);
            assert!(window.find("scroll-top").visible());

            window.press("shift-end", cx);
            assert!(window.find("scroll-top").visible());
            assert!(!window.find("scroll-bottom").visible());
            window.click("scroll-input", cx);
            window.press("end", cx);
            window.input("Z", cx);
            assert_eq!(window.find("scroll-input").value(), Some("AlphaZ"));
            window.press("home", cx);
            window.press("pagedown", cx);
            assert!(window.find("scroll-top").visible());
            assert!(!window.find("scroll-bottom").visible());

            window.click("scroll-textarea", cx);
            assert!(
                harness
                    .read(cx)
                    .textarea
                    .read(cx)
                    .focus_handle(cx)
                    .is_focused(window),
                "the textarea must own native focus before editor navigation"
            );
            window.press("end", cx);
            window.press("home", cx);
            window.press("pagedown", cx);
            window.press("pageup", cx);
            assert!(window.find("scroll-top").visible());
            assert!(!window.find("scroll-bottom").visible());

            window.click("scroll-top", cx);
            // Kit command buttons intentionally preserve the active editor's
            // focus on pointer activation. Exit it through the real Tab order.
            assert!(
                harness
                    .read(cx)
                    .textarea
                    .read(cx)
                    .focus_handle(cx)
                    .is_focused(window),
                "clicking a command button must preserve textarea focus"
            );
            window.press("end", cx);
            assert!(window.find("scroll-top").visible());
            assert!(!window.find("scroll-bottom").visible());
            window.press("shift-tab", cx);
            assert_eq!(
                window.find("scroll-input").focused(),
                Some(true),
                "reverse Tab must leave the textarea for the preceding input"
            );
            window.press("shift-tab", cx);
            assert_eq!(
                window.find("scroll-top").focused(),
                Some(true),
                "reverse Tab must reach the non-editor control"
            );
            window.press("end", cx);
            assert!(window.find("scroll-bottom").visible());
        })
        .unwrap();
    }
}
