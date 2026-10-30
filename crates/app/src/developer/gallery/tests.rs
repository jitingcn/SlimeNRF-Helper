use super::{Gallery, Sample, Scene};
use crate::{
    features::settings::store::{Appearance, Language},
    test_support::UiTestSession,
};
use gpui_kit::{
    AnyWindowHandle, AppContext as _, Bounds, ClipboardItem, Entity, KeyBinding, MouseButton,
    ScrollDelta, TestAppContext, WindowBounds, WindowOptions, point, px, size,
    test::{TestEventExt as _, TestWindowExt as _},
};
use slimenrf_terminal_core::MAX_LINES;

struct Fixture {
    handle: AnyWindowHandle,
    gallery: Entity<Gallery>,
}

fn fixture(cx: &mut TestAppContext) -> Fixture {
    let (handle, gallery) = cx.update(|cx| {
        gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    Default::default(),
                    size(px(760.), px(600.)),
                ))),
                ..Default::default()
            },
            cx,
            |window, cx| cx.new(|cx| Gallery::new(window, cx)),
        )
        .expect("open isolated gallery test window")
    });
    cx.update_window(handle, |_, window, _| window.activate_window())
        .expect("activate gallery test window");
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        assert!(
            window.is_window_active(),
            "gallery keyboard test requires an active window"
        );
        window.render_frame(cx);
    })
    .expect("render gallery test window");
    let fixture = Fixture { handle, gallery };
    settle_gallery(cx, &fixture);
    fixture
}

fn settle_gallery(cx: &mut TestAppContext, fixture: &Fixture) {
    // The production grid updates its retained column count after prepaint.
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update_window(fixture.handle, |_, window, cx| {
            window.simulate_next_frame(cx);
            window.render_frame(cx);
        })
        .expect("settle gallery layout");
    }
}

fn settle_terminal(cx: &mut TestAppContext, fixture: &Fixture, phase: &str) {
    // Match the transcript lifecycle's bounded reflow settlement: callbacks
    // may require more than one frame as wrapped lines are measured.
    for _ in 0..MAX_LINES + 10 {
        cx.run_until_parked();
        let delivered = cx
            .update_window(fixture.handle, |_, window, cx| {
                window.render_frame(cx);
                window.simulate_next_frame(cx)
            })
            .expect("settle gallery transcript");
        if delivered == 0 {
            return;
        }
    }
    panic!("gallery transcript did not settle during {phase}");
}

#[gpui_kit::test]
fn resuming_follow_reveals_the_tail_without_new_output(cx: &mut TestAppContext) {
    let _session = UiTestSession::new(cx);
    // The styled scrollbar uses wall-clock motion and can independently keep
    // requesting frames; test terminal settlement under the supported
    // reduced-motion policy rather than conflating its animation with reflow.
    cx.update(|cx| cx.set_reduce_motion(true));
    let fixture = fixture(cx);
    cx.update_window(fixture.handle, |_, window, cx| {
        window.click("gallery-scene-terminal", cx);
    })
    .expect("open gallery terminal");
    settle_terminal(cx, &fixture, "initial layout");

    let (tail, extent) = cx.update(|cx| {
        let scroll = &fixture.gallery.read(cx).terminal.scroll;
        (scroll.offset().y, scroll.max_offset().y)
    });
    assert!(
        extent > px(20.),
        "synthetic output must overflow the transcript"
    );
    assert!(
        f32::from(tail + extent).abs() < 0.01,
        "Follow starts at the tail"
    );
    cx.update_window(fixture.handle, |_, window, cx| {
        window.scroll(
            "gallery-transcript-scroll",
            ScrollDelta::Pixels(point(px(0.), px(120.))),
            cx,
        );
    })
    .expect("scroll gallery transcript upward");
    settle_terminal(cx, &fixture, "paused scroll");
    let (paused, revision) = cx.update(|cx| {
        let terminal = &fixture.gallery.read(cx).terminal;
        assert!(!terminal.follow, "wheel input must pause Follow");
        (terminal.scroll.offset().y, terminal.buffer.revision())
    });
    assert!(
        paused > tail + px(20.),
        "wheel input must move away from the tail"
    );

    cx.update_window(fixture.handle, |_, window, cx| {
        window.click("gallery-terminal-follow", cx);
        assert_eq!(window.find("gallery-terminal-follow").checked(), Some(true));
    })
    .expect("resume gallery Follow");
    settle_terminal(cx, &fixture, "resumed Follow");
    cx.update(|cx| {
        let terminal = &fixture.gallery.read(cx).terminal;
        assert_eq!(
            terminal.buffer.revision(),
            revision,
            "no output was appended"
        );
        assert!(terminal.follow);
        assert!(
            f32::from(terminal.scroll.offset().y + terminal.scroll.max_offset().y).abs() < 0.01,
            "resuming Follow must reach the current tail without waiting for output"
        );
    });
}

#[gpui_kit::test]
fn gallery_grid_keeps_shared_columns_through_zoom_and_dynamic_content(cx: &mut TestAppContext) {
    let _session = UiTestSession::new(cx);
    let fixture = fixture(cx);
    cx.simulate_window_scale_factor_change(fixture.handle, 1.25);
    settle_gallery(cx, &fixture);
    cx.update_window(fixture.handle, |_, window, cx| {
        window.click("gallery-zoom-large", cx);
        window.click("gallery-content-long", cx);
        window.click("gallery-change-detail", cx);
    })
    .expect("change gallery presentation");
    settle_gallery(cx, &fixture);
    cx.update_window(fixture.handle, |_, window, _| {
        let alpha = window.find("gallery-card-alpha").bounds();
        let beta = window.find("gallery-card-beta").bounds();
        let offline = window.find("gallery-card-offline").bounds();
        let width = |bounds: Bounds<_>| {
            window.pixel_snap(window.pixel_snap(bounds.right()) - window.pixel_snap(bounds.left()))
        };
        assert_eq!(
            window.pixel_snap(alpha.top()),
            window.pixel_snap(beta.top())
        );
        assert!(
            beta.left() > alpha.left(),
            "the first row must have two columns"
        );
        assert!(
            offline.top() >= alpha.bottom(),
            "the disabled card is on the partial last row"
        );
        assert_eq!(
            window.pixel_snap(alpha.left()),
            window.pixel_snap(offline.left())
        );
        assert_eq!(
            width(alpha),
            width(beta),
            "first-row cards must have equal rendered widths"
        );
        assert_eq!(
            width(alpha),
            width(offline),
            "the partial row must keep the shared track width"
        );
    })
    .expect("inspect shared gallery grid");

    cx.simulate_window_resize(fixture.handle, size(px(340.), px(900.)));
    settle_gallery(cx, &fixture);
    cx.update_window(fixture.handle, |_, window, _| {
        let alpha = window.find("gallery-card-alpha").bounds();
        let beta = window.find("gallery-card-beta").bounds();
        assert!(
            beta.top() >= alpha.bottom(),
            "a narrow gallery must use one column"
        );
        assert_eq!(
            window.pixel_snap(alpha.left()),
            window.pixel_snap(beta.left())
        );
    })
    .expect("inspect narrow gallery grid");
}

#[gpui_kit::test]
fn gallery_preserves_selection_across_local_content_locale_and_zoom_changes(
    cx: &mut TestAppContext,
) {
    let _session = UiTestSession::new(cx);
    let fixture = fixture(cx);
    cx.update_window(fixture.handle, |_, window, cx| {
        assert_eq!(window.find("gallery-scene-controls").checked(), Some(true));
        assert_eq!(window.find("gallery-card-offline").disabled(), Some(true));
        let short_height = window.find("gallery-card-alpha").bounds().size.height;
        window.click("gallery-card-beta", cx);
        assert_eq!(fixture.gallery.read(cx).selected, Sample::Beta);
        assert_eq!(window.find("gallery-card-beta").checked(), Some(true));

        window.click("gallery-content-long", cx);
        assert!(fixture.gallery.read(cx).long_text);
        assert!(
            window.find("gallery-card-alpha").bounds().size.height > short_height,
            "long sample text must change the real card's wrapped layout"
        );
        window.click("gallery-change-detail", cx);
        window.click("gallery-change-detail", cx);
        assert_eq!(fixture.gallery.read(cx).detail, 2);

        let normal_rem = window.rem_size();
        window.click("gallery-zoom-large", cx);
        assert_eq!(window.find("gallery-zoom-large").checked(), Some(true));
        assert!(window.rem_size() > normal_rem);
        window.click("gallery-theme-light", cx);
        assert_eq!(
            fixture.gallery.read(cx).preferences.appearance,
            Appearance::Light
        );
        assert_eq!(window.find("gallery-zoom-large").checked(), Some(true));
        assert!(
            window.rem_size() > normal_rem,
            "theme change must retain zoom"
        );
        window.click("gallery-language-zh", cx);
        assert_eq!(
            fixture.gallery.read(cx).preferences.language,
            Language::SimplifiedChinese
        );
        assert_eq!(window.find("gallery-scene-terminal").label(), Some("终端"));
        assert_eq!(fixture.gallery.read(cx).selected, Sample::Beta);
        assert_eq!(window.find("gallery-content-long").checked(), Some(true));
        assert_eq!(window.find("gallery-card-beta").checked(), Some(true));
    })
    .expect("exercise gallery controls");
}

#[gpui_kit::test]
fn keyboard_opens_terminal_and_controls_change_the_real_bounded_transcript(
    cx: &mut TestAppContext,
) {
    let _session = UiTestSession::new(cx);
    let fixture = fixture(cx);
    cx.update_window(fixture.handle, |_, window, cx| {
        assert!(fixture.gallery.read(cx).root_focus.is_focused(window));
        window.press("tab", cx);
        assert_eq!(window.find("gallery-scene-controls").focused(), Some(true));
        window.press("tab", cx);
        assert_eq!(window.find("gallery-scene-terminal").focused(), Some(true));
        window.press("enter", cx);
        assert_eq!(fixture.gallery.read(cx).scene, Scene::Terminal);
        assert!(window.find("gallery-transcript-scroll").visible());
        assert_eq!(window.find("gallery-terminal-follow").checked(), Some(true));
        assert_eq!(window.find("gallery-terminal-wrap").checked(), Some(true));

        let original = fixture.gallery.read(cx).terminal.buffer.text();
        window.click("gallery-terminal-follow", cx);
        window.click("gallery-terminal-wrap", cx);
        assert_eq!(
            window.find("gallery-terminal-follow").checked(),
            Some(false)
        );
        assert_eq!(window.find("gallery-terminal-wrap").checked(), Some(false));
        window.click("gallery-terminal-append", cx);
        let appended = fixture.gallery.read(cx).terminal.buffer.text();
        assert!(appended.starts_with(&original));
        assert!(appended.contains("[25] Added synthetic output"));
        window.click("gallery-terminal-follow", cx);
        assert_eq!(window.find("gallery-terminal-follow").checked(), Some(true));
        window.click("gallery-terminal-clear", cx);
        assert!(fixture.gallery.read(cx).terminal.buffer.text().is_empty());
        assert!(window.try_find("gallery-transcript-scroll").is_none());
        window.click("gallery-terminal-append", cx);
        assert!(
            fixture
                .gallery
                .read(cx)
                .terminal
                .buffer
                .text()
                .contains("[26] Added synthetic output")
        );
        assert!(window.find("gallery-transcript-scroll").visible());
    })
    .expect("exercise gallery terminal");
}

const SELECTION_OUTPUT: &str =
    "[25] Added synthetic output: long content for wrapping and scrolling. 中文 🦀\n";

fn selection_fixture(cx: &mut TestAppContext) -> Fixture {
    cx.update(|cx| {
        cx.set_reduce_motion(true);
        cx.bind_keys([KeyBinding::new(
            if cfg!(target_os = "macos") {
                "cmd-c"
            } else {
                "ctrl-c"
            },
            gpui_kit::component::input::Copy,
            Some("TerminalTranscript"),
        )]);
    });
    let fixture = fixture(cx);
    cx.update_window(fixture.handle, |_, window, cx| {
        window.click("gallery-scene-terminal", cx);
        window.click("gallery-terminal-clear", cx);
        window.click("gallery-terminal-append", cx);
        assert_eq!(
            fixture.gallery.read(cx).terminal.buffer.text(),
            SELECTION_OUTPUT
        );
    })
    .expect("prepare one synthetic transcript line");
    settle_terminal(cx, &fixture, "selection fixture");
    fixture
}

fn selected_text(cx: &mut TestAppContext, fixture: &Fixture) -> Option<String> {
    cx.update(|cx| {
        fixture
            .gallery
            .read(cx)
            .terminal
            .state
            .borrow()
            .selected_text()
    })
}

#[gpui_kit::test]
fn terminal_lost_release_titlebar_resize_hover_then_new_drag_copies(cx: &mut TestAppContext) {
    let _session = UiTestSession::new(cx);
    let fixture = selection_fixture(cx);
    cx.update_window(fixture.handle, |_, window, cx| {
        let titlebar = window.find("title-bar").bounds();
        let position = point(titlebar.left() + px(100.), titlebar.center().y);
        assert!(!fixture.gallery.read(cx).terminal.focus.is_focused(window));
        assert!(fixture.gallery.read(cx).terminal.follow);
        window.pointer_move(position, None, cx);
        window.pointer_down(position, MouseButton::Left, cx);
    })
    .expect("press the real gallery title bar");

    // Model an OS move/resize loop consuming the corresponding mouse-up.
    cx.simulate_window_resize(fixture.handle, size(px(840.), px(700.)));
    settle_terminal(cx, &fixture, "title bar resize");
    cx.update_window(fixture.handle, |_, window, cx| {
        let bounds = window.find("gallery-transcript-scroll").bounds();
        let row = point(
            bounds.left() + px(10.),
            bounds.top() + window.rem_size() * 0.625,
        );
        window.pointer_move(row, None, cx);
        window.pointer_move(point(row.x + px(140.), row.y), None, cx);
        assert!(
            !fixture.gallery.read(cx).terminal.focus.is_focused(window),
            "button-free hover must not focus the transcript"
        );
        assert!(
            fixture.gallery.read(cx).terminal.follow,
            "button-free hover must not pause Follow"
        );
    })
    .expect("hover the resized transcript without a button");
    cx.run_until_parked();
    assert_eq!(
        selected_text(cx, &fixture),
        None,
        "a missed title-bar release must not select transcript bytes on hover"
    );

    cx.update_window(fixture.handle, |_, window, cx| {
        let bounds = window.find("gallery-transcript-scroll").bounds();
        let start = point(
            bounds.left() + px(10.),
            bounds.top() + window.rem_size() * 0.625,
        );
        let end = point(start.x + px(40.), bounds.top() + window.rem_size() * 8.);
        window.pointer_move(start, None, cx);
        window.pointer_down(start, MouseButton::Left, cx);
        window.pointer_move(end, Some(MouseButton::Left), cx);
        window.pointer_up(end, MouseButton::Left, cx);
    })
    .expect("complete a new ordinary transcript drag");
    cx.run_until_parked();
    assert_eq!(
        selected_text(cx, &fixture).as_deref(),
        Some(SELECTION_OUTPUT),
        "the next ordinary drag must select the intended UTF-8 text"
    );
    cx.update_window(fixture.handle, |_, window, cx| {
        cx.write_to_clipboard(ClipboardItem::new_string("unchanged".into()));
        window.press("secondary-c", cx);
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some(SELECTION_OUTPUT.into()),
            "Copy must use the new drag's selected bytes"
        );
    })
    .expect("copy the new transcript selection");
}

#[gpui_kit::test]
fn terminal_lost_release_freezes_last_held_endpoint_through_hover_and_copy(
    cx: &mut TestAppContext,
) {
    let _session = UiTestSession::new(cx);
    let fixture = selection_fixture(cx);
    let (held, hover) = cx
        .update_window(fixture.handle, |_, window, cx| {
            let bounds = window.find("gallery-transcript-scroll").bounds();
            let start = point(
                bounds.left() + px(10.),
                bounds.top() + window.rem_size() * 0.625,
            );
            let held = point(start.x + px(60.), start.y);
            let hover = point(start.x + px(220.), start.y);
            window.pointer_move(start, None, cx);
            window.pointer_down(start, MouseButton::Left, cx);
            window.pointer_move(held, Some(MouseButton::Left), cx);
            (held, hover)
        })
        .expect("start a genuine transcript selection");
    cx.run_until_parked();
    let selected = selected_text(cx, &fixture).expect("held drag selects text");
    assert!(
        selected.len() >= 4 && SELECTION_OUTPUT.contains(&selected),
        "the held endpoint must select a nonempty portion of the fixture: {selected:?}"
    );

    // There is deliberately no MouseUp between the held drag and the hover.
    cx.update_window(fixture.handle, |_, window, cx| {
        window.pointer_move(hover, None, cx);
    })
    .expect("move after the lost release");
    cx.run_until_parked();
    assert_eq!(
        selected_text(cx, &fixture).as_deref(),
        Some(selected.as_str()),
        "the first button-free move must freeze the last held endpoint"
    );
    cx.update_window(fixture.handle, |_, window, cx| {
        window.pointer_move(point(held.x + px(300.), held.y), None, cx);
    })
    .expect("hover farther across the transcript");
    cx.run_until_parked();
    assert_eq!(
        selected_text(cx, &fixture).as_deref(),
        Some(selected.as_str()),
        "later hovers must retain the completed selection"
    );
    cx.update_window(fixture.handle, |_, window, cx| {
        cx.write_to_clipboard(ClipboardItem::new_string("unchanged".into()));
        window.press("secondary-c", cx);
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some(selected.clone()),
            "a drag completed by lost-release recovery remains copyable"
        );
    })
    .expect("copy the frozen selection");
}

#[gpui_kit::test]
fn terminal_lost_release_guard_keeps_held_drag_working_outside_transcript(cx: &mut TestAppContext) {
    let _session = UiTestSession::new(cx);
    let fixture = selection_fixture(cx);
    let outside = cx
        .update_window(fixture.handle, |_, window, cx| {
            let bounds = window.find("gallery-transcript-scroll").bounds();
            let start = point(
                bounds.left() + px(10.),
                bounds.top() + window.rem_size() * 0.625,
            );
            let inside = point(start.x + px(60.), start.y);
            let outside = point(bounds.right() + px(2.), start.y);
            window.pointer_move(start, None, cx);
            window.pointer_down(start, MouseButton::Left, cx);
            window.pointer_move(inside, Some(MouseButton::Left), cx);
            outside
        })
        .expect("start the held drag inside the transcript");
    cx.run_until_parked();
    let inside = selected_text(cx, &fixture).expect("inside drag selects text");

    cx.update_window(fixture.handle, |_, window, cx| {
        window.pointer_move(outside, Some(MouseButton::Left), cx);
    })
    .expect("continue holding beyond the transcript boundary");
    cx.run_until_parked();
    let extended = selected_text(cx, &fixture).expect("outside drag keeps a selection");
    assert!(
        extended.starts_with(&inside) && extended.len() > inside.len(),
        "a held outside drag must extend {inside:?}, got {extended:?}"
    );
    assert!(
        SELECTION_OUTPUT.contains(&extended),
        "outside drag must select the fixture's actual text: {extended:?}"
    );
    cx.update_window(fixture.handle, |_, window, cx| {
        window.pointer_up(outside, MouseButton::Left, cx);
        let bounds = window.find("gallery-transcript-scroll").bounds();
        window.pointer_move(bounds.center(), None, cx);
    })
    .expect("release the outside drag and hover");
    cx.run_until_parked();
    assert_eq!(
        selected_text(cx, &fixture).as_deref(),
        Some(extended.as_str()),
        "normal release and later hover must preserve the outside drag"
    );
    cx.update_window(fixture.handle, |_, window, cx| {
        cx.write_to_clipboard(ClipboardItem::new_string("unchanged".into()));
        window.press("secondary-c", cx);
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some(extended.clone())
        );
    })
    .expect("copy the outside drag selection");
}
