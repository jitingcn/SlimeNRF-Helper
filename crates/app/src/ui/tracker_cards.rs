use crate::settings::AccentColor;
use gpui_kit::{
    AnyElement, App, Div, Hsla, IntoElement, RenderOnce, SharedString, Window, div, hsla,
    prelude::*, px,
};

pub(super) fn tracker_card_frame(
    card: impl IntoElement,
    activity: Hsla,
    outline_width: f32,
) -> Div {
    div()
        .w_full()
        .min_w_0()
        // Reserve a constant inset so sampled rate changes never move content.
        .p(px(TRACKER_ACTIVITY_MAX_WIDTH - outline_width))
        .rounded_md()
        .border(px(outline_width))
        .border_color(activity)
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

#[derive(IntoElement)]
pub(super) struct TrackerCardGrid {
    pub(super) id: SharedString,
    pub(super) horizontal_padding: f32,
    pub(super) cards: Vec<AnyElement>,
}

impl RenderOnce for TrackerCardGrid {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        use gpui_kit::base::ElementExt as _;
        let columns = window.use_keyed_state(self.id.clone(), cx, |_, _| 1_u16);
        let column_count = *columns.read(cx);
        let horizontal_padding = self.horizontal_padding;
        // Measure the full available container, never a row or the capped grid.
        // Grid's shared tracks keep an incomplete last row the same width.
        div()
            .id(self.id)
            .relative()
            .w_full()
            .min_w_0()
            .on_prepaint(move |bounds, window, cx| {
                let content_width =
                    (f32::from(bounds.size.width) - horizontal_padding * 2.).max(0.);
                let next = tracker_grid_columns(content_width);
                if *columns.read(cx) != next {
                    columns.update(cx, |columns, _| *columns = next);
                    window.refresh();
                }
            })
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .px(px(horizontal_padding))
                    .py(px(TRACKER_GRID_PADDING))
                    .child(
                        div()
                            .grid()
                            .grid_cols(column_count)
                            .w_full()
                            .min_w_0()
                            .max_w(px(tracker_grid_max_width(column_count)))
                            .gap(px(TRACKER_CARD_GAP))
                            .children(self.cards),
                    ),
            )
    }
}

#[cfg(test)]
mod tracker_grid_tests {
    use super::{
        TRACKER_ACTIVITY_MAX_WIDTH, TRACKER_CARD_GAP, tracker_activity_outline,
        tracker_grid_columns, tracker_grid_max_width,
    };
    use crate::settings::AccentColor;

    #[test]
    fn activity_width_is_bounded_monotonic_and_keeps_constant_inset() {
        for accent in AccentColor::ALL {
            for is_dark in [false, true] {
                let mut previous = 0.;
                for tps in (0..=240).chain([u32::MAX]) {
                    let (_, width) = tracker_activity_outline(tps, is_dark, accent);
                    assert!((1. ..=TRACKER_ACTIVITY_MAX_WIDTH).contains(&width));
                    assert!(width >= previous);
                    let padding = TRACKER_ACTIVITY_MAX_WIDTH - width;
                    assert!((width + padding - TRACKER_ACTIVITY_MAX_WIDTH).abs() < f32::EPSILON);
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
}
