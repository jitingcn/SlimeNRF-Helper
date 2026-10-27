use crate::settings::AccentColor;
use gpui_kit::{
    AnyElement, App, Div, Hsla, IntoElement, Pixels, RenderOnce, SharedString, Window, div, hsla,
    prelude::*, px,
};

pub(super) fn tracker_card_frame(
    card: impl IntoElement,
    activity: Hsla,
    outline_width: f32,
) -> Div {
    div()
        .relative()
        .w_full()
        .min_w_0()
        // The activity outline is painted independently so its changing
        // stroke and padding cannot round to different device-pixel insets.
        .p(px(TRACKER_ACTIVITY_MAX_WIDTH))
        .child(
            div()
                .absolute()
                .inset_0()
                .rounded_md()
                .border(px(outline_width))
                .border_color(activity),
        )
        .child(card)
}

const TRACKER_ACTIVITY_MAX_WIDTH: f32 = 3.;

// Continuous color and width response at sampled TPS, with no animation timer.
pub(super) fn tracker_activity_outline(
    tps: u32,
    is_dark: bool,
    accent: AccentColor,
) -> (Hsla, f32) {
    let rate = tps.min(120) as f32;
    let (idle, low, medium, high) = if accent == AccentColor::Sakura {
        // Sage activity stays distinct from rose selection without a cyan flare.
        let lightness = if is_dark {
            [0.22, 0.43, 0.56, 0.68]
        } else {
            [0.67, 0.44, 0.36, 0.28]
        };
        (
            (135., 0.08, lightness[0]),
            (135., 0.14, lightness[1]),
            (135., 0.21, lightness[2]),
            (135., 0.25, lightness[3]),
        )
    } else {
        let lightness = if is_dark {
            [0.22, 0.43, 0.52, 0.66]
        } else {
            [0.62, 0.36, 0.32, 0.28]
        };
        (
            (210., 0.10, lightness[0]),
            (195., 0.65, lightness[1]),
            (150., 0.78, lightness[2]),
            (175., 0.90, lightness[3]),
        )
    };
    let (from, to, progress) = if rate <= 25. {
        (idle, low, rate / 25.)
    } else if rate <= 75. {
        (low, medium, (rate - 25.) / 50.)
    } else {
        (medium, high, (rate - 75.) / 45.)
    };
    let hue = (from.0 + (to.0 - from.0) * progress) / 360.;
    let saturation = from.1 + (to.1 - from.1) * progress;
    let lightness = from.2 + (to.2 - from.2) * progress;
    (
        hsla(hue, saturation, lightness, 1.),
        1. + (TRACKER_ACTIVITY_MAX_WIDTH - 1.) * rate / 120.,
    )
}

const TRACKER_CARD_MIN_WIDTH: f32 = 200.;
const TRACKER_CARD_MAX_WIDTH: f32 = 320.;
const TRACKER_CARD_GAP: f32 = 8.;
pub(super) const TRACKER_GRID_PADDING: f32 = 8.;

fn tracker_grid_columns(content_width: f32) -> u16 {
    ((content_width + TRACKER_CARD_GAP) / (TRACKER_CARD_MIN_WIDTH + TRACKER_CARD_GAP))
        .floor()
        .max(1.) as u16
}

fn tracker_grid_max_width(columns: u16) -> f32 {
    f32::from(columns) * TRACKER_CARD_MAX_WIDTH
        + f32::from(columns.saturating_sub(1)) * TRACKER_CARD_GAP
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct TrackerGridLayout {
    columns: u16,
    device_width: u32,
    scale: f32,
}

fn tracker_grid_device_width(available: u32, gap: u32, maximum_card: u32, columns: u16) -> u32 {
    let columns = u32::from(columns);
    let gaps = gap.saturating_mul(columns - 1);
    if gaps > available {
        return 0;
    }
    let card = ((available - gaps) / columns).min(maximum_card);
    card * columns + gaps
}

#[derive(IntoElement)]
pub(super) struct TrackerCardGrid {
    pub(super) id: SharedString,
    pub(super) horizontal_padding: f32,
    pub(super) cards: Vec<AnyElement>,
}

impl RenderOnce for TrackerCardGrid {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        use gpui_kit::base::ElementExt as _;
        let layout = window.use_keyed_state(self.id.clone(), cx, |_, _| None::<TrackerGridLayout>);
        let current = *layout.read(cx);
        let column_count = current.map_or(1, |layout| layout.columns);
        let horizontal_padding = self.horizontal_padding;
        let grid = div()
            .grid()
            .grid_cols(column_count)
            .min_w_0()
            .gap(px(TRACKER_CARD_GAP));
        let grid = match current {
            Some(layout) => grid.w(px(layout.device_width as f32 / layout.scale)),
            None => grid
                .w_full()
                .max_w(px(tracker_grid_max_width(column_count))),
        };
        // Measure the full available container, never a row or the capped grid.
        // Restrict the grid itself to an integer number of equal physical-pixel
        // tracks; any indivisible remainder stays at the trailing edge.
        div()
            .id(self.id)
            .relative()
            .w_full()
            .min_w_0()
            .on_prepaint(move |bounds, window, cx| {
                let content_width =
                    (f32::from(bounds.size.width) - horizontal_padding * 2.).max(0.);
                let columns = tracker_grid_columns(content_width);
                let scale = window.scale_factor();
                let device_pixels = |value: Pixels| (f32::from(value) * scale).round() as u32;
                let available = device_pixels(bounds.size.width).saturating_sub(
                    device_pixels(window.pixel_snap(px(horizontal_padding))).saturating_mul(2),
                );
                let gap = device_pixels(window.pixel_snap(px(TRACKER_CARD_GAP)));
                let maximum = device_pixels(window.pixel_snap(px(TRACKER_CARD_MAX_WIDTH)));
                let width = tracker_grid_device_width(available, gap, maximum, columns);
                let next = TrackerGridLayout {
                    columns,
                    device_width: width,
                    scale,
                };
                if *layout.read(cx) != Some(next) {
                    layout.update(cx, |layout, _| *layout = Some(next));
                    // `refresh` is ignored during prepaint; request it after
                    // the frame so the measured layout reaches the screen.
                    window.on_next_frame(|window, _| window.refresh());
                }
            })
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .px(px(horizontal_padding))
                    .py(px(TRACKER_GRID_PADDING))
                    .child(grid.children(self.cards)),
            )
    }
}

#[cfg(test)]
mod tracker_grid_tests {
    use super::{
        TRACKER_ACTIVITY_MAX_WIDTH, TRACKER_CARD_GAP, TrackerCardGrid, tracker_activity_outline,
        tracker_grid_columns, tracker_grid_device_width, tracker_grid_max_width,
    };
    use crate::settings::AccentColor;
    use gpui_kit::{
        Context, IntoElement, Render, TestAppContext, Window, div, prelude::*, px, size,
        test::TestWindowExt as _,
    };

    #[test]
    fn activity_width_is_bounded_and_monotonic() {
        for accent in AccentColor::ALL {
            for is_dark in [false, true] {
                let mut previous = 0.;
                for tps in (0..=240).chain([u32::MAX]) {
                    let (_, width) = tracker_activity_outline(tps, is_dark, accent);
                    assert!((1. ..=TRACKER_ACTIVITY_MAX_WIDTH).contains(&width));
                    assert!(width >= previous);
                    previous = width;
                }
            }
        }
    }

    #[test]
    fn columns_and_shared_track_width_obey_available_space() {
        for (width, expected_columns) in [
            (0., 1),
            (199., 1),
            (200., 1),
            (320., 1),
            (407., 1),
            (408., 2),
            (615., 2),
            (616., 3),
            (1000., 4),
            (1800., 8),
        ] {
            let columns = tracker_grid_columns(width);
            assert_eq!(columns, expected_columns);
            let grid_width = width.min(tracker_grid_max_width(columns));
            let card_width =
                (grid_width - f32::from(columns - 1) * TRACKER_CARD_GAP) / f32::from(columns);
            assert!(card_width <= 320.);
            if width >= 200. {
                assert!(card_width >= 200.);
            } else {
                assert_eq!(card_width, width);
            }
        }
        assert_eq!(tracker_grid_max_width(1), 320.);
        assert_eq!(tracker_grid_max_width(2), 648.);
    }

    #[test]
    fn device_pixel_tracks_leave_only_the_indivisible_remainder() {
        assert_eq!(tracker_grid_device_width(591, 10, 400, 2), 590);
        for columns in 1_u16..=8 {
            let count = u32::from(columns);
            let gaps = 10 * (count - 1);
            for available in gaps..=1200 {
                let grid = tracker_grid_device_width(available, 10, 400, columns);
                let card_space = grid - gaps;
                assert!(grid <= available);
                assert_eq!(card_space % count, 0);
                let card = card_space / count;
                assert!(card <= 400);
                if card < 400 {
                    assert!(
                        available - grid < count,
                        "only the indivisible remainder may be left unused",
                    );
                }
            }
        }
    }

    struct GridHarness;

    impl Render for GridHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().size_full().child(TrackerCardGrid {
                id: "idle-grid".into(),
                horizontal_padding: 8.,
                cards: (0..7)
                    .map(|_| div().h(px(40.)).into_any_element())
                    .collect(),
            })
        }
    }

    #[gpui_kit::test]
    fn measured_grid_refreshes_once_then_remains_idle(cx: &mut TestAppContext) {
        let _ui_session = crate::ui::test_support::UiTestSession::new(cx);
        let handle = cx.open_window(size(px(548.), px(600.)), |window, _| {
            window.set_scale_factor(1.25);
            GridHarness
        });
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(
                window.simulate_next_frame(cx),
                1,
                "the first measured layout must request one follow-up frame",
            );
            window.render_frame(cx);
            for _ in 0..3 {
                assert_eq!(
                    window.simulate_next_frame(cx),
                    0,
                    "an unchanged measured layout must not request idle frames",
                );
                window.render_frame(cx);
            }
        })
        .unwrap();

        cx.simulate_window_resize(handle.into(), size(px(744.), px(600.)));
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(
                window.simulate_next_frame(cx),
                1,
                "a resized grid must request its new measured layout",
            );
            window.render_frame(cx);
            assert_eq!(
                window.simulate_next_frame(cx),
                0,
                "the resized layout must settle without an idle refresh loop",
            );
        })
        .unwrap();
    }
}
