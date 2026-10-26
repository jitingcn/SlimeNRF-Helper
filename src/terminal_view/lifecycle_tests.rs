use std::{cell::RefCell, rc::Rc};

use gpui_kit::Focusable as _;
use gpui_kit::base::TextSelectionHandle;
use gpui_kit::{
    App, Bounds, Context, Entity, FocusHandle, IntoElement, Pixels, PlatformInput, Render,
    ScrollDelta, ScrollEvent, ScrollHandle, Size, TestAppContext, TouchPhase, Window, WindowHandle,
    canvas,
    component::{
        ActiveTheme,
        input::{Input, InputState},
    },
    div, point,
    prelude::*,
    px, rems, size,
};

use super::{TerminalTranscript, TranscriptState, TranscriptViewport, geometry::ViewportAnchor};
use crate::terminal::{MAX_LINES, TerminalBuffer};

struct TranscriptHarness {
    buffer: TerminalBuffer,
    state: Rc<RefCell<TranscriptState>>,
    scroll: ScrollHandle,
    selection: TextSelectionHandle,
    soft_wrap: bool,
    focus: FocusHandle,
    input: Option<Entity<InputState>>,
    follow: bool,
    callbacks: usize,
    paint_region: Rc<RefCell<Option<Bounds<Pixels>>>>,
}

impl Render for TranscriptHarness {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.state.borrow_mut().sync_buffer(&self.buffer, 1);
        let view = cx.weak_entity();
        let on_reflow = Rc::new(move |_: &mut Window, cx: &mut App| {
            let _ = view.update(cx, |this, cx| {
                this.callbacks += 1;
                if this.state.borrow_mut().settle_frame(
                    &this.scroll,
                    true,
                    this.soft_wrap,
                    this.follow,
                ) {
                    cx.notify();
                }
            });
        });
        let paint_region = self.paint_region.clone();
        let transcript = div()
            .id("transcript-scroll")
            .size_full()
            .when(self.soft_wrap, |view| {
                view.overflow_y_scroll().overflow_x_hidden()
            })
            .when(!self.soft_wrap, |view| view.overflow_scroll())
            .px_2()
            .text_xs()
            .line_height(rems(1.25))
            .font_family("monospace")
            .whitespace_nowrap()
            .track_scroll(&self.scroll)
            .track_focus(&self.focus)
            .key_context("TerminalTranscript")
            .on_key_down(cx.listener(|this, event, window, cx| {
                if super::handle_key_down(&this.state, event, &this.scroll, window, cx) {
                    this.follow = false;
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            .child(TerminalTranscript::new(
                "transcript",
                self.selection.clone(),
                self.state.clone(),
                gpui_kit::base::Theme::global(cx).tokens.colors.selection,
                cx.theme().background,
                TranscriptViewport {
                    soft_wrap: self.soft_wrap,
                    follow: self.follow,
                    scroll: self.scroll.clone(),
                    focus: self.focus.clone(),
                    on_reflow,
                },
            ))
            .child(
                canvas(
                    move |_, window, _| {
                        *paint_region.borrow_mut() = Some(window.content_mask().bounds);
                    },
                    |_, _, _, _| {},
                )
                .absolute(),
            );
        if let Some(input) = &self.input {
            div()
                .size_full()
                .bg(cx.theme().background)
                .flex()
                .flex_col()
                .child(transcript.flex_1().min_h_0())
                .child(Input::new(input))
                .into_any_element()
        } else {
            div()
                .size_full()
                .bg(cx.theme().background)
                .child(transcript)
                .into_any_element()
        }
    }
}

struct Fixture {
    window: WindowHandle<TranscriptHarness>,
}

impl Fixture {
    fn new(
        cx: &mut TestAppContext,
        text: &str,
        soft_wrap: bool,
        follow: bool,
        scale: f32,
        viewport: Size<Pixels>,
    ) -> Self {
        let mut buffer = TerminalBuffer::new();
        buffer.push(text.as_bytes());
        let window = cx.open_window(viewport, move |window, cx| {
            window.set_scale_factor(scale);
            TranscriptHarness {
                buffer,
                state: Rc::new(RefCell::new(TranscriptState::default())),
                scroll: ScrollHandle::default(),
                selection: TextSelectionHandle::new("", cx),
                soft_wrap,
                follow,
                focus: cx.focus_handle().tab_stop(true),
                input: None,
                callbacks: 0,
                paint_region: Rc::new(RefCell::new(None)),
            }
        });
        Self { window }
    }

    fn inspect<R>(&self, cx: &TestAppContext, f: impl FnOnce(&TranscriptHarness) -> R) -> R {
        self.window.read_with(cx, |view, _| f(view)).unwrap()
    }

    fn draw(&self, cx: &mut TestAppContext) {
        cx.update_window(self.window.into(), |_, window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        })
        .unwrap();
    }

    fn draw_natural(&self, cx: &mut TestAppContext) {
        cx.update_window(self.window.into(), |_, window, cx| {
            window.draw(cx).clear(cx);
        })
        .unwrap();
    }

    fn frame_count(&self, cx: &mut TestAppContext) -> u64 {
        cx.update_window(self.window.into(), |_, window, _| {
            window.layout_stats().frames
        })
        .unwrap()
    }

    fn wheel_frame(&self, cx: &mut TestAppContext, delta: ScrollDelta) {
        // Match Fast's frame_after driver: an update may already draw the dirty
        // window. An extra idle frame would break the scroll-layer promotion streak.
        let before = self.frame_count(cx);
        self.wheel_delta(cx, delta);
        if self.frame_count(cx) == before {
            self.draw_natural(cx);
        }
        assert_eq!(self.frame_count(cx), before + 1);
    }

    fn deliver(&self, cx: &mut TestAppContext) -> usize {
        cx.update_window(self.window.into(), |_, window, cx| {
            window.simulate_next_frame(cx)
        })
        .unwrap()
    }

    fn settle(&self, cx: &mut TestAppContext) {
        for _ in 0..MAX_LINES + 10 {
            self.draw(cx);
            let delivered = self.deliver(cx);
            assert!(delivered <= 1, "a transcript must coalesce frame callbacks");
            if delivered == 0 {
                self.inspect(cx, |view| {
                    let state = view.state.borrow();
                    assert!(state.pending.is_none());
                    assert!(!state.frame_queued);
                    assert_eq!(state.reflow_cursor, state.lines.len());
                    assert_eq!(state.frame_work.background_lines, 0);
                });
                return;
            }
        }
        panic!("rendered transcript did not become idle");
    }

    fn focus(&self, cx: &mut TestAppContext) {
        self.window
            .update(cx, |view, window, cx| view.focus.focus(window, cx))
            .unwrap();
    }

    fn press(&self, cx: &mut TestAppContext, key: &str) {
        use gpui_kit::test::TestWindowExt;

        cx.update_window(self.window.into(), |_, window, cx| window.press(key, cx))
            .unwrap();
    }

    fn wheel(&self, cx: &mut TestAppContext, dy: f32) {
        self.wheel_delta(cx, ScrollDelta::Pixels(point(px(0.), px(dy))));
    }

    fn wheel_delta(&self, cx: &mut TestAppContext, delta: ScrollDelta) {
        cx.update_window(self.window.into(), |_, window, cx| {
            window.dispatch_event(
                PlatformInput::Scroll(ScrollEvent {
                    position: point(px(20.), px(20.)),
                    delta,
                    touch_phase: TouchPhase::Moved,
                    ..Default::default()
                }),
                cx,
            );
        })
        .unwrap();
    }

    fn resize(&self, cx: &TestAppContext, viewport: Size<Pixels>) {
        cx.simulate_window_resize(self.window.into(), viewport);
    }

    fn assert_coordinates(&self, cx: &mut TestAppContext, top: Pixels) {
        self.window
            .update(cx, |view, window, _| {
                let state = view.state.borrow();
                let frame = state.presented.as_ref().unwrap();
                assert_eq!(state.presented_top(view.scroll.offset()), Some(top));
                assert_eq!(frame.offset, view.scroll.offset());
                assert_eq!(frame.origin.y, window.pixel_snap_point(frame.offset).y);
                assert_eq!(state.runs.len(), state.hit_lines.len());
                for (&index, run) in state.hit_lines.iter().zip(&state.runs) {
                    let origin = frame.origin + point(px(0.), state.line_top(index));
                    let line = &state.lines[index];
                    assert_eq!(run.bounds().origin, origin);
                    assert_eq!(run.layout().bounds().origin, origin);
                    assert_eq!(
                        line.styled
                            .borrow()
                            .styled
                            .as_ref()
                            .unwrap()
                            .layout()
                            .bounds()
                            .origin,
                        origin
                    );
                    assert_eq!(
                        state.byte_at_point(point(px(0.), state.line_top(index) + px(1.))),
                        Some(line.offset)
                    );
                }
            })
            .unwrap();
    }
}

fn anchor_key(anchor: ViewportAnchor) -> (u64, u64, usize, super::geometry::RowAffinity, Pixels) {
    (
        anchor.generation,
        anchor.line_id,
        anchor.byte,
        anchor.affinity,
        anchor.intra_row_y,
    )
}

fn short_lines(count: usize) -> String {
    std::iter::repeat_n("abcdefghijklmnopqrstuvwx", count)
        .collect::<Vec<_>>()
        .join("\n")
}

#[gpui_kit::test]
fn native_repeated_wheel_frames_preserve_the_real_viewport_with_scroll_layers(
    cx: &mut TestAppContext,
) {
    cx.update(gpui_kit::init);
    let text = (0..200)
        .map(|index| format!("{index:03} {}", "abcdefghijklmnopqrstuvwx".repeat(3)))
        .collect::<Vec<_>>()
        .join("\n");
    for layers in [true, false] {
        for soft_wrap in [false, true] {
            for scale in [1., 1.25, 1.5] {
                for direction in [1., -1.] {
                    for lines in [false, true] {
                        let fixture = Fixture::new(
                            cx,
                            &text,
                            soft_wrap,
                            false,
                            scale,
                            size(px(320.), px(200.)),
                        );
                        fixture.settle(cx);
                        fixture.wheel(cx, -800.);
                        fixture.settle(cx);
                        cx.update_window(fixture.window.into(), |_, window, _| {
                            window.set_scroll_layers(layers);
                            window.reset_layout_stats();
                        })
                        .unwrap();
                        // Finish set_scroll_layers' refresh before consecutive wheel frames.
                        fixture.draw_natural(cx);
                        let row_height =
                            fixture.inspect(cx, |view| view.state.borrow().layout_key.row_height);
                        let delta = if lines {
                            ScrollDelta::Lines(point(0., direction * 0.25))
                        } else {
                            ScrollDelta::Pixels(point(px(0.), px(direction * 5.25)))
                        };
                        let step = delta.pixel_delta(row_height).y;
                        let mut expected_offset = px(-800.);
                        let mut previous_physical_top = px(800.);
                        let mut saw_expanded_region = false;
                        for wheel in 0..6 {
                            fixture.wheel_frame(cx, delta);
                            expected_offset += step;
                            fixture
                                .window
                                .update(cx, |view, window, _| {
                                    let state = view.state.borrow();
                                    let owner = view.scroll.bounds();
                                    let paint_region = view.paint_region.borrow().unwrap();
                                    let frame = state.presented.as_ref().unwrap();
                                    saw_expanded_region |= paint_region.size.height > owner.size.height;
                                    assert_eq!(
                                        view.scroll.offset().y,
                                        expected_offset,
                                        "wheel {wheel}: layers={layers}, wrap={soft_wrap}, scale={scale}, lines={lines}, direction={direction}"
                                    );
                                    let painted = state
                                        .painted
                                        .iter()
                                        .find(|painted| {
                                            let line = &state.lines[painted.index];
                                            let bounds = line
                                                .styled
                                                .borrow()
                                                .styled
                                                .as_ref()
                                                .unwrap()
                                                .layout()
                                                .bounds();
                                            bounds.top() <= owner.top()
                                                && bounds.bottom() > owner.top()
                                        })
                                        .expect("the real owner's top row must be painted");
                                    let line = &state.lines[painted.index];
                                    let row_origin = line
                                        .styled
                                        .borrow()
                                        .styled
                                        .as_ref()
                                        .unwrap()
                                        .layout()
                                        .bounds()
                                        .origin;
                                    let physical_top =
                                        owner.top() - row_origin.y + state.line_top(painted.index);
                                    let expected_physical_top = -window
                                        .pixel_snap_point(point(px(0.), expected_offset))
                                        .y;
                                    assert_eq!(
                                        physical_top, expected_physical_top,
                                        "a wheel step must move actual numbered rows, not only the logical anchor"
                                    );
                                    assert!(
                                        (physical_top - previous_physical_top).abs()
                                            <= step.abs() + px(1. / scale),
                                        "a small wheel step made a viewport-sized correction"
                                    );
                                    assert!(
                                        (physical_top - previous_physical_top) * direction <= px(0.),
                                        "a wheel step moved numbered rows in the opposite direction"
                                    );
                                    previous_physical_top = physical_top;
                                    assert_eq!(
                                        frame.viewport, owner,
                                        "the semantic viewport must not become the layer paint mask"
                                    );
                                    for (index, line) in state.lines.iter().enumerate() {
                                        let top = frame.origin.y + state.line_top(index);
                                        let bottom =
                                            top + row_height * line.row_count as f32;
                                        assert_eq!(
                                            frame.visible.contains(&index),
                                            top < owner.bottom() && bottom > owner.top(),
                                            "visible numbered rows must use the real owner"
                                        );
                                        assert_eq!(
                                            state.painted.iter().any(|line| line.index == index),
                                            top < paint_region.bottom()
                                                && bottom > paint_region.top(),
                                            "cached rows must cover the paint region from the final document origin"
                                        );
                                    }
                                    if let Some(pending) = &state.pending {
                                        assert_eq!(
                                            pending.target_offset, pending.expected_offset,
                                            "layer promotion must not queue a scroll correction"
                                        );
                                    }
                                })
                                .unwrap();
                        }
                        let repaints = cx
                            .update_window(fixture.window.into(), |_, window, _| {
                                window.layout_stats().layer_frames_repainted
                            })
                            .unwrap();
                        assert_eq!(
                            saw_expanded_region, layers,
                            "the natural-frame sequence must exercise actual layer overscan"
                        );
                        assert_eq!(
                            repaints > 0,
                            layers,
                            "scroll-layer promotion was not exercised"
                        );
                        for _ in 0..4 {
                            assert_eq!(
                                fixture.deliver(cx),
                                0,
                                "static wheel frames must not queue deferred corrections"
                            );
                            fixture.draw_natural(cx);
                            fixture.inspect(cx, |view| {
                                let state = view.state.borrow();
                                assert_eq!(view.scroll.offset().y, expected_offset);
                                assert!(state.pending.is_none());
                                assert!(!state.frame_queued);
                            });
                        }
                    }
                }
            }
        }
    }
}

#[gpui_kit::test]
fn native_fractional_wheel_is_quiescent_with_wrap_on_off_and_fractional_dpi(
    cx: &mut TestAppContext,
) {
    cx.update(gpui_kit::init);
    let text = std::iter::repeat_n("x".repeat(100), 50)
        .collect::<Vec<_>>()
        .join("\n");
    for soft_wrap in [false, true] {
        for scale in [1., 1.25, 1.5] {
            let fixture =
                Fixture::new(cx, &text, soft_wrap, false, scale, size(px(320.), px(200.)));
            fixture.settle(cx);
            fixture.wheel(cx, -50.);
            fixture.settle(cx);
            fixture.assert_coordinates(cx, px(50.));
            fixture.wheel(cx, -0.25);
            fixture.inspect(cx, |view| assert_eq!(view.scroll.offset().y, px(-50.25)));
            let callbacks = fixture.inspect(cx, |view| view.callbacks);
            for _ in 0..8 {
                fixture.draw(cx);
                assert_eq!(
                    fixture.deliver(cx),
                    0,
                    "idle fractional scroll queued a frame"
                );
                fixture.assert_coordinates(cx, px(50.25));
                fixture.inspect(cx, |view| {
                    let state = view.state.borrow();
                    assert_eq!(view.scroll.offset().y, px(-50.25));
                    assert_eq!(view.callbacks, callbacks);
                    assert!(state.pending.is_none());
                    assert!(!state.frame_queued);
                    assert_eq!(state.frame_work.background_lines, 0);
                });
            }
        }
    }
}

#[gpui_kit::test]
fn native_resize_clamp_preserves_the_paused_line_and_byte_anchor(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let fixture = Fixture::new(
        cx,
        &short_lines(50),
        true,
        false,
        1.,
        size(px(640.), px(200.)),
    );
    fixture.settle(cx);
    fixture.inspect(cx, |view| {
        assert_eq!(view.state.borrow().height(), px(1_000.))
    });
    fixture.wheel(cx, -800.);
    fixture.settle(cx);
    let anchor = fixture.inspect(cx, |view| view.state.borrow().anchor.unwrap());
    fixture.resize(cx, size(px(96.), px(300.)));
    fixture.draw(cx);
    fixture.inspect(cx, |view| {
        let state = view.state.borrow();
        assert_eq!(view.scroll.offset().y, px(-700.));
        assert_eq!(state.pending.as_ref().unwrap().expected_offset.y, px(-700.));
        assert_eq!(anchor_key(state.anchor.unwrap()), anchor_key(anchor));
    });
    fixture.settle(cx);
    let top = fixture.inspect(cx, |view| {
        let state = view.state.borrow();
        assert_eq!(anchor_key(state.anchor.unwrap()), anchor_key(anchor));
        let top = state.anchor_top(anchor).unwrap();
        assert!(top > px(800.));
        assert_eq!(view.scroll.offset().y, -top);
        top
    });
    fixture.assert_coordinates(cx, top);
}

#[gpui_kit::test]
fn native_head_trim_clamp_rebases_the_surviving_anchor_once(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let text = format!("{}\n{}x", "x".repeat(60), "x\n".repeat(MAX_LINES - 2));
    let fixture = Fixture::new(cx, &text, true, false, 1., size(px(1_024.), px(200.)));
    fixture.settle(cx);
    let width = fixture.inspect(cx, |view| {
        let state = view.state.borrow();
        let layout = &state.lines[0].geometry.as_ref().unwrap().content_layout;
        (layout.unwrapped_layout.x_for_index(10) + layout.unwrapped_layout.x_for_index(11)) / 2.
            + px(16.)
    });
    fixture.resize(cx, size(width, px(200.)));
    fixture.settle(cx);
    fixture.inspect(cx, |view| {
        let state = view.state.borrow();
        assert_eq!(state.lines.len(), MAX_LINES);
        assert_eq!(state.lines[0].row_count, 6);
        assert_eq!(state.height(), px(20_100.));
    });
    fixture.wheel(cx, -19_900.);
    fixture.settle(cx);
    let anchor = fixture.inspect(cx, |view| view.state.borrow().anchor.unwrap());
    fixture
        .window
        .update(cx, |view, _, cx| {
            view.buffer.push(b"\nx");
            cx.notify();
        })
        .unwrap();
    fixture.draw(cx);
    fixture.inspect(cx, |view| {
        let state = view.state.borrow();
        assert_eq!(state.height(), px(20_000.));
        assert_eq!(view.scroll.offset().y, px(-19_800.));
        let pending = state.pending.as_ref().unwrap();
        assert_eq!(pending.expected_offset.y, px(-19_800.));
        assert_eq!(pending.target_offset.y, px(-19_780.));
        assert_eq!(anchor_key(state.anchor.unwrap()), anchor_key(anchor));
    });
    fixture.settle(cx);
    fixture.inspect(cx, |view| {
        assert_eq!(view.scroll.offset().y, px(-19_780.));
        assert_eq!(
            anchor_key(view.state.borrow().anchor.unwrap()),
            anchor_key(anchor)
        );
    });
    fixture.assert_coordinates(cx, px(19_780.));
}

#[gpui_kit::test]
fn native_wheel_before_deferred_resize_commit_keeps_the_user_delta(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let fixture = Fixture::new(
        cx,
        &short_lines(50),
        true,
        false,
        1.,
        size(px(640.), px(200.)),
    );
    fixture.settle(cx);
    fixture.wheel(cx, -800.);
    fixture.settle(cx);
    let width = fixture.inspect(cx, |view| {
        let state = view.state.borrow();
        let layout = &state.lines[0].geometry.as_ref().unwrap().content_layout;
        (layout.unwrapped_layout.x_for_index(8) + layout.unwrapped_layout.x_for_index(9)) / 2.
            + px(16.)
    });
    fixture.resize(cx, size(width, px(300.)));
    fixture.draw(cx);
    let expected = fixture.inspect(cx, |view| {
        let state = view.state.borrow();
        assert_eq!(view.scroll.offset().y, px(-700.));
        assert_eq!(state.lines[0].row_count, 3);
        ViewportAnchor {
            generation: 1,
            line_id: state.lines[39].id,
            byte: 16,
            affinity: super::geometry::RowAffinity::Downstream,
            intra_row_y: px(10.),
        }
    });
    fixture.wheel(cx, 10.);
    fixture.inspect(cx, |view| assert_eq!(view.scroll.offset().y, px(-690.)));
    // Updating a test window flushes a dirty render after wheel dispatch. That render may
    // replace the pending correction before the already-coalesced callback is delivered.
    assert_eq!(fixture.deliver(cx), 1);
    fixture.settle(cx);
    fixture.inspect(cx, |view| {
        let state = view.state.borrow();
        assert_eq!(anchor_key(state.anchor.unwrap()), anchor_key(expected));
        assert_eq!(state.anchor_top(expected), Some(px(2_390.)));
        assert_eq!(view.scroll.offset().y, px(-2_390.));
    });
    fixture.assert_coordinates(cx, px(2_390.));
    for _ in 0..8 {
        fixture.draw(cx);
        assert_eq!(fixture.deliver(cx), 0, "settled wheel queued an idle frame");
        fixture.assert_coordinates(cx, px(2_390.));
        fixture.inspect(cx, |view| {
            let state = view.state.borrow();
            assert_eq!(anchor_key(state.anchor.unwrap()), anchor_key(expected));
            assert_eq!(view.scroll.offset().y, px(-2_390.));
            assert!(state.pending.is_none());
            assert!(!state.frame_queued);
        });
    }
}

#[gpui_kit::test]
fn native_follow_reaches_the_reflowed_bottom_after_resize_without_output(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let fixture = Fixture::new(
        cx,
        &short_lines(50),
        true,
        true,
        1.,
        size(px(640.), px(200.)),
    );
    fixture.settle(cx);
    fixture.inspect(cx, |view| assert_eq!(view.scroll.offset().y, px(-800.)));
    fixture.resize(cx, size(px(96.), px(300.)));
    fixture.settle(cx);
    let top = fixture.inspect(cx, |view| {
        let state = view.state.borrow();
        let top = state.height() - px(300.);
        assert!(top > px(800.));
        assert_eq!(view.scroll.offset().y, -top);
        top
    });
    fixture.assert_coordinates(cx, top);
}

#[gpui_kit::test]
fn native_keyboard_selection_preserves_wrapped_utf8_bytes_during_append_and_resize(
    cx: &mut TestAppContext,
) {
    cx.update(gpui_kit::init);
    let fixture = Fixture::new(
        cx,
        &format!("{}last", "a界世界 🦀\n".repeat(20)),
        true,
        true,
        1.25,
        size(px(80.), px(100.)),
    );
    fixture.settle(cx);
    // Derive both widths from shaped glyphs: the headless font does not give
    // CJK characters the double-cell advance a native terminal font may use.
    let (wrapped_width, unwrapped_width) = fixture.inspect(cx, |view| {
        let state = view.state.borrow();
        let layout = &state.lines[0].geometry.as_ref().unwrap().content_layout;
        let inset = view.scroll.bounds().size.width - state.layout_key.wrap_width.unwrap();
        let wrapped = (layout.unwrapped_layout.x_for_index("a界".len())
            + layout.unwrapped_layout.x_for_index("a界世".len()))
            / 2.;
        (
            wrapped + inset,
            layout.unwrapped_layout.width + inset + px(1.),
        )
    });
    fixture.resize(cx, size(wrapped_width, px(100.)));
    fixture.settle(cx);
    fixture.focus(cx);
    fixture.press(cx, "secondary-home");
    fixture.settle(cx);
    fixture.inspect(cx, |view| {
        assert!(!view.follow);
        assert_eq!(view.scroll.offset().y, px(0.));
        let state = view.state.borrow();
        assert!(
            state.lines[0].row_count > 1,
            "Unicode selection must cross a soft wrap: rows={}, wrap_width={:?}, text_width={:?}",
            state.lines[0].row_count,
            state.layout_key.wrap_width,
            state.lines[0]
                .geometry
                .as_ref()
                .unwrap()
                .content_layout
                .unwrapped_layout
                .width
        );
    });
    for _ in 0..6 {
        fixture.press(cx, "shift-right");
    }
    fixture.settle(cx);
    fixture.inspect(cx, |view| {
        assert_eq!(
            view.state.borrow().selected_text().as_deref(),
            Some("a界世界 🦀")
        );
    });
    fixture.resize(cx, size(unwrapped_width, px(120.)));
    fixture.settle(cx);
    fixture.inspect(cx, |view| {
        let state = view.state.borrow();
        assert_eq!(
            state.lines[0].row_count, 1,
            "resize must remove the soft wrap"
        );
        assert_eq!(state.selected_text().as_deref(), Some("a界世界 🦀"));
        assert_eq!(state.endpoints[1], Some("a界世界 🦀".len()));
    });
    fixture
        .window
        .update(cx, |view, _, cx| {
            view.buffer.push(b" appended\noutput");
            cx.notify();
        })
        .unwrap();
    fixture.settle(cx);
    fixture.inspect(cx, |view| {
        let state = view.state.borrow();
        assert!(!view.follow);
        assert_eq!(state.selected_text().as_deref(), Some("a界世界 🦀"));
        assert!(state.keyboard_caret);
        assert_eq!(state.endpoints[1], Some("a界世界 🦀".len()));
    });
}

#[gpui_kit::test]
fn native_keyboard_pages_preserve_paused_history_and_leave_command_input_keys_alone(
    cx: &mut TestAppContext,
) {
    cx.update(gpui_kit::init);
    let fixture = Fixture::new(
        cx,
        &short_lines(50),
        false,
        true,
        1.,
        size(px(400.), px(200.)),
    );
    fixture.settle(cx);
    fixture.focus(cx);
    fixture.press(cx, "secondary-end");
    fixture.settle(cx);
    let endpoints = fixture.inspect(cx, |view| view.state.borrow().endpoints);
    fixture.press(cx, "pageup");
    fixture.settle(cx);
    let offset = fixture.inspect(cx, |view| {
        assert!(!view.follow);
        assert_eq!(view.state.borrow().endpoints, endpoints);
        assert_eq!(view.scroll.offset().y, px(-620.));
        view.scroll.offset()
    });
    fixture
        .window
        .update(cx, |view, _, cx| {
            view.buffer.push(b"\nnew output");
            cx.notify();
        })
        .unwrap();
    fixture.settle(cx);
    fixture.inspect(cx, |view| assert_eq!(view.scroll.offset(), offset));
    fixture
        .window
        .update(cx, |view, window, cx| {
            let input = cx.new(|cx| InputState::new(window, cx));
            input.update(cx, |input, cx| {
                input.set_value("command history", window, cx);
                input.focus_handle(cx).focus(window, cx);
            });
            view.input = Some(input);
            cx.notify();
        })
        .unwrap();
    fixture.settle(cx);
    for key in ["home", "shift-end", "left", "up", "down", "secondary-a"] {
        fixture.press(cx, key);
    }
    fixture.inspect(cx, |view| {
        assert_eq!(view.state.borrow().endpoints, endpoints);
        assert!(!view.state.borrow().has_selection());
        assert!(!view.follow);
    });
}

#[gpui_kit::test]
fn native_transcript_zoom_scales_rows_hit_testing_and_keyboard_viewport_together(
    cx: &mut TestAppContext,
) {
    cx.update(gpui_kit::init);
    let fixture = Fixture::new(
        cx,
        &short_lines(30),
        true,
        false,
        1.5,
        size(px(200.), px(120.)),
    );
    fixture.settle(cx);
    fixture
        .window
        .update(cx, |_, window, _| {
            window.set_rem_size(px(20.));
            window.refresh();
        })
        .unwrap();
    fixture.settle(cx);
    fixture.focus(cx);
    fixture.press(cx, "secondary-home");
    fixture.press(cx, "down");
    fixture.settle(cx);
    fixture.inspect(cx, |view| {
        let state = view.state.borrow();
        assert_eq!(state.layout_key.row_height, px(25.));
        let byte = state.endpoints[1].unwrap();
        assert_eq!(
            state.caret_position(byte, state.caret_affinity).unwrap().y,
            px(25.)
        );
        assert_eq!(state.byte_at_point(point(px(0.), px(26.))), Some(byte));
        assert_eq!(view.scroll.offset().y, px(0.));
    });
}

#[gpui_kit::test]
fn native_keyboard_rederives_an_established_column_after_font_zoom(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let text = std::iter::repeat_n("01234界56789", 10)
        .collect::<Vec<_>>()
        .join("\n");
    let column = "01234界5678".len();
    let fixture = Fixture::new(cx, &text, false, false, 1.25, size(px(640.), px(200.)));
    fixture.settle(cx);
    fixture.focus(cx);
    fixture.press(cx, "secondary-home");
    for _ in 0..10 {
        fixture.press(cx, "right");
    }
    fixture.press(cx, "down");
    fixture.settle(cx);
    fixture.inspect(cx, |view| {
        let state = view.state.borrow();
        assert_eq!(state.endpoints[1], Some(state.lines[1].offset + column));
        assert!(state.preferred_x.is_some());
    });
    fixture
        .window
        .update(cx, |_, window, _| {
            window.set_rem_size(px(20.));
            window.refresh();
        })
        .unwrap();
    fixture.settle(cx);
    fixture.press(cx, "down");
    fixture.settle(cx);
    fixture.inspect(cx, |view| {
        let state = view.state.borrow();
        assert_eq!(state.endpoints[1], Some(state.lines[2].offset + column));
        assert_eq!(state.layout_key.row_height, px(25.));
        assert!(!view.follow);
    });
}
