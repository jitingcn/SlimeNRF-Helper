use super::{Gallery, Sample, Scene};
use crate::{
    settings::{Appearance, Language},
    ui::test_support::UiTestSession,
};
use gpui_kit::{
    AnyWindowHandle, AppContext as _, Bounds, Entity, ScrollDelta, TestAppContext, WindowBounds,
    WindowOptions, point, px, size, test::TestWindowExt as _,
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
