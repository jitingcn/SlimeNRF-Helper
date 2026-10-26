use crate::i18n::tr;
use gpui_kit::base::plot::{PathCaches, ShapeKey};
use gpui_kit::component::ActiveTheme;
use gpui_kit::{
    AnyElement, App, ContentMask, Hsla, InteractiveElement, IntoElement, ParentElement, Path,
    PathBuilder, Pixels, Styled, canvas, div, point, px, relative, rems, rgb,
};
use slimenrf_recording::thermal_dump::{AxisAnalysis, ThermalDumpAnalysis};
use std::{
    fmt::Write as _,
    hash::{DefaultHasher, Hash, Hasher},
    sync::Arc,
};

const BAND: f64 = 0.01;
const LAYERS: usize = 9;
type Sample = [f64; 2];

// Hash values rather than Arc addresses: allocator address reuse must not revive stale paths.
struct Geometry {
    x: [f64; 2],
    y: [f64; 2],
    measured: Option<[f64; 2]>,
    linear_low: Option<[Sample; 2]>,
    linear_high: Option<[Sample; 2]>,
    band: Vec<Sample>,
    fingerprint: u64,
}

pub(super) struct ChartData {
    analysis: Arc<ThermalDumpAnalysis>,
    axis: usize,
    geometry: Geometry,
    x_ticks: Ticks,
    y_ticks: Ticks,
}

impl ChartData {
    fn new(analysis: Arc<ThermalDumpAnalysis>, axis: usize, range: [f64; 2]) -> Self {
        let geometry = Geometry::new(&analysis, axis, range);
        let x_ticks = ticks(geometry.x, 4);
        let y_ticks = ticks(geometry.y, 6);
        Self {
            analysis,
            axis,
            geometry,
            x_ticks,
            y_ticks,
        }
    }
}

#[derive(Default)]
pub(super) struct ChartCache {
    axes: [Option<Arc<ChartData>>; 3],
}

impl ChartCache {
    pub(super) fn get(
        &mut self,
        analysis: &Arc<ThermalDumpAnalysis>,
        axis: usize,
        range: [f64; 2],
    ) -> Arc<ChartData> {
        let range = finite_range(range);
        let slot = &mut self.axes[axis];
        if let Some(chart) = slot
            && Arc::ptr_eq(&chart.analysis, analysis)
            && chart.geometry.x.map(f64::to_bits) == range.map(f64::to_bits)
        {
            return chart.clone();
        }
        let chart = Arc::new(ChartData::new(analysis.clone(), axis, range));
        *slot = Some(chart.clone());
        chart
    }
}

fn finite_range(range: [f64; 2]) -> [f64; 2] {
    if !range.iter().all(|value| value.is_finite()) {
        return [0., 1.];
    }
    let [a, b] = [range[0].min(range[1]), range[0].max(range[1])];
    if a < b {
        return [a, b];
    }
    let padding = (a.abs() * 0.01).max(1.);
    let low = a - padding;
    let high = a + padding;
    [
        if low.is_finite() { low } else { a },
        if high.is_finite() { high } else { a },
    ]
}

fn fraction(value: f64, range: [f64; 2]) -> f64 {
    let span = range[1] - range[0];
    let offset = value - range[0];
    if span.is_finite() && offset.is_finite() {
        offset / span
    } else {
        // Also handles valid finite endpoints whose subtraction overflows.
        let scale = range[0].abs().max(range[1].abs()).max(value.abs());
        (value / scale - range[0] / scale) / (range[1] / scale - range[0] / scale)
    }
}

#[derive(Clone, Copy)]
struct Ticks {
    values: [f64; 12],
    len: usize,
    step: f64,
}

impl Ticks {
    fn iter(&self) -> impl Iterator<Item = f64> + '_ {
        self.values[..self.len].iter().copied()
    }
}

fn ticks(range: [f64; 2], intervals: usize) -> Ticks {
    let mut result = Ticks {
        values: [0.; 12],
        len: 0,
        step: 1.,
    };
    let rough = range[1] / intervals as f64 - range[0] / intervals as f64;
    let magnitude = 10_f64.powf(rough.log10().floor());
    let unit = rough / magnitude;
    let nice = [1., 2., 2.5, 5., 10.]
        .into_iter()
        .find(|candidate| *candidate >= unit * 0.9)
        .unwrap_or(10.);
    let step = nice * magnitude;
    if step.is_finite() && step > 0. {
        result.step = step;
        let first = (range[0] / step).ceil();
        for index in 0..12 {
            let value = (first + index as f64) * step;
            if !value.is_finite() || value > range[1] {
                break;
            }
            if value >= range[0] {
                if result.len > 0 && value <= result.values[result.len - 1] {
                    break;
                }
                result.values[result.len] = if value.abs() < step * 1e-10 {
                    0.
                } else {
                    value
                };
                result.len += 1;
            }
        }
    }
    if result.len == 0 {
        result.values[..2].copy_from_slice(&range);
        result.len = 2;
    }
    result
}

fn label(value: f64, step: f64) -> String {
    let exponent = step.log10().floor();
    let scale = 10_f64.powf(exponent);
    let fractional_step = usize::from((step / scale - (step / scale).round()).abs() > 1e-8);
    if value != 0. && (step < 0.000001 || value.abs() >= 1_000_000.) {
        let decimals =
            ((value.abs().log10().floor() - exponent).max(0.) as usize + fractional_step).min(15);
        format!("{value:.decimals$e}")
    } else if value == 0. && step < 0.000001 {
        "0".into()
    } else {
        let decimals = ((-exponent).max(0.) as usize + fractional_step).min(10);
        format!("{value:.decimals$}")
    }
}

fn linear_extension(axis: &AxisAnalysis, points: &[Sample]) -> Option<[Sample; 2]> {
    let slope = axis.statistics.linear_slope?;
    let anchor = *axis.linear.first()?;
    let first = *points.first()?;
    let last = *points.last()?;
    let at = |x: f64| [x, slope.mul_add(x - anchor[0], anchor[1])];
    let result = [at(first[0]), at(last[0])];
    result
        .iter()
        .flatten()
        .all(|v| v.is_finite())
        .then_some(result)
}

impl Geometry {
    fn new(analysis: &ThermalDumpAnalysis, axis: usize, range: [f64; 2]) -> Self {
        let axis = &analysis.axes[axis];
        let mut geometry = Self {
            x: finite_range(range),
            y: [0., 1.],
            measured: analysis.temperature_range_c,
            linear_low: linear_extension(axis, &axis.extrapolation_low),
            linear_high: linear_extension(axis, &axis.extrapolation_high),
            fingerprint: 0,
            band: Vec::new(),
        };
        let mut low = f64::INFINITY;
        let mut high = f64::NEG_INFINITY;
        let mut hasher = DefaultHasher::new();
        for layer in 1..7 {
            let points = geometry.series(axis, layer);
            points.len().hash(&mut hasher);
            for &[x, y] in points {
                x.to_bits().hash(&mut hasher);
                y.to_bits().hash(&mut hasher);
                let margin = if layer >= 4 { BAND } else { 0. };
                low = low.min(y - margin);
                high = high.max(y + margin);
            }
        }
        for &[x, y] in &axis.raw {
            x.to_bits().hash(&mut hasher);
            y.to_bits().hash(&mut hasher);
            low = low.min(y);
            high = high.max(y);
        }
        if let Some(measured) = geometry.measured {
            measured.map(f64::to_bits).hash(&mut hasher);
        }
        let base = finite_range([low, high]);
        let padding = ((base[1] - base[0]) * 0.06).max(0.002);
        let padded = [base[0] - padding, base[1] + padding];
        geometry.y = if padded.iter().all(|v| v.is_finite()) {
            padded
        } else {
            base
        };
        geometry.fingerprint = hasher.finish();
        geometry.band = band_polygon(&geometry, axis);
        geometry
    }

    fn series<'a>(&'a self, axis: &'a AxisAnalysis, layer: usize) -> &'a [Sample] {
        match layer {
            1 => &axis.linear,
            2 => self
                .linear_low
                .as_ref()
                .map_or(&[][..], |points| &points[..]),
            3 => self
                .linear_high
                .as_ref()
                .map_or(&[][..], |points| &points[..]),
            4 => &axis.mls,
            5 => &axis.extrapolation_low,
            6 => &axis.extrapolation_high,
            _ => &[],
        }
    }

    fn project(&self, sample: Sample, width: f64, height: f64) -> Sample {
        [
            fraction(sample[0], self.x) * width,
            (1. - fraction(sample[1], self.y)) * height,
        ]
    }
}

fn temperature_segment(a: Sample, b: Sample, range: [f64; 2]) -> Option<[Sample; 2]> {
    if a[0].max(b[0]) < range[0] || a[0].min(b[0]) > range[1] {
        return None;
    }
    let clip = |point: Sample| {
        let x = point[0].clamp(range[0], range[1]);
        if x == point[0] {
            point
        } else {
            let t = fraction(x, [a[0], b[0]]).clamp(0., 1.);
            [x, a[1] * (1. - t) + b[1] * t]
        }
    };
    Some([clip(a), clip(b)])
}

// Liang–Barsky clipping preserves segments crossing the viewport even when neither
// endpoint is visible. Shared by native rendering and SVG, before any f32 conversion.
fn clipped_segment(a: Sample, b: Sample, width: f64, height: f64) -> Option<[Sample; 2]> {
    if !a.into_iter().chain(b).all(f64::is_finite) {
        return None;
    }
    let [dx, dy] = [b[0] - a[0], b[1] - a[1]];
    let (mut start, mut end) = (0.0_f64, 1.0_f64);
    for (p, q) in [
        (-dx, a[0]),
        (dx, width - a[0]),
        (-dy, a[1]),
        (dy, height - a[1]),
    ] {
        if !p.is_finite() || !q.is_finite() {
            return None;
        }
        if p == 0. {
            if q < 0. {
                return None;
            }
        } else if p < 0. {
            start = start.max(q / p);
        } else {
            end = end.min(q / p);
        }
        if start > end {
            return None;
        }
    }
    let at = |t: f64| {
        [
            dx.mul_add(t, a[0]).clamp(0., width),
            dy.mul_add(t, a[1]).clamp(0., height),
        ]
    };
    Some([at(start), at(end)])
}

// One polygon spans measured MLS and both extrapolated tails. Shared endpoints are
// removed before creating its two edges, so translucent fills cannot overlap or seam.
fn band_polygon(geometry: &Geometry, axis: &AxisAnalysis) -> Vec<Sample> {
    let count = axis.mls.len() + axis.extrapolation_low.len() + axis.extrapolation_high.len();
    let mut points = Vec::with_capacity(count.saturating_add(6).saturating_mul(2));
    for series in [&axis.extrapolation_low, &axis.mls, &axis.extrapolation_high] {
        for pair in series.windows(2) {
            if let Some(pair) = temperature_segment(pair[0], pair[1], geometry.x) {
                for point in pair {
                    if point.iter().all(|value| value.is_finite()) && points.last() != Some(&point)
                    {
                        points.push(point);
                    }
                }
            }
        }
    }
    if points.len() < 2 {
        points.clear();
        return points;
    }
    let length = points.len();
    for index in (0..length).rev() {
        let [x, y] = points[index];
        points.push([x, y - BAND]);
    }
    for point in &mut points[..length] {
        point[1] += BAND;
    }
    points
}

#[derive(Clone, Copy)]
enum Primitive {
    Line([Sample; 2]),
    Move(Sample),
    Vertex(Sample),
    Close,
    Circle(Sample),
}

fn primitives(
    geometry: &Geometry,
    axis: &AxisAnalysis,
    layer: usize,
    width: f64,
    height: f64,
    mut emit: impl FnMut(Primitive),
) {
    if layer == 0 {
        for (index, &sample) in geometry.band.iter().enumerate() {
            let mut point = geometry.project(sample, width, height);
            point[0] = point[0].clamp(0., width);
            point[1] = point[1].clamp(0., height);
            if index == 0 {
                emit(Primitive::Move(point));
            } else {
                emit(Primitive::Vertex(point));
            }
        }
        if !geometry.band.is_empty() {
            emit(Primitive::Close);
        }
    } else if layer == 7 {
        if let Some(measured) = geometry.measured {
            for temperature in measured {
                let x = fraction(temperature, geometry.x) * width;
                if (0. ..=width).contains(&x) {
                    emit(Primitive::Line([[x, 0.], [x, height]]));
                }
            }
        }
    } else if layer == 8 {
        for &sample in &axis.raw {
            let [x, y] = geometry.project(sample, width, height);
            if (0. ..=width).contains(&x) && (0. ..=height).contains(&y) {
                emit(Primitive::Circle([x, y]));
            }
        }
    } else {
        for pair in geometry.series(axis, layer).windows(2) {
            let Some(pair) = temperature_segment(pair[0], pair[1], geometry.x) else {
                continue;
            };
            if let Some(pair) = clipped_segment(
                geometry.project(pair[0], width, height),
                geometry.project(pair[1], width, height),
                width,
                height,
            ) {
                emit(Primitive::Line(pair));
            }
        }
    }
}

fn dashed(layer: usize) -> bool {
    matches!(layer, 2 | 3 | 5 | 6 | 7)
}

fn stroke_width(layer: usize) -> f32 {
    match layer {
        1 => 2.,
        4 => 2.5,
        7 => 0.8,
        _ => 1.6,
    }
}

fn color(layer: usize, dark: bool) -> u32 {
    match layer {
        1..=3 => {
            if dark {
                0x6ce39a
            } else {
                0x12813b
            }
        }
        7 => {
            if dark {
                0xa1a1aa
            } else {
                0x777777
            }
        }
        8 => {
            if dark {
                0xffffff
            } else {
                0x11111a
            }
        }
        0 => {
            if dark {
                0x9991ff
            } else {
                0x7771ef
            }
        }
        _ => {
            if dark {
                0x81b1ff
            } else {
                0x245ac8
            }
        }
    }
}

fn opacity(layer: usize) -> f32 {
    match layer {
        0 => 0.22,
        2 | 3 | 5 | 6 => 0.75,
        7 => 0.35,
        _ => 1.,
    }
}

fn native_path(
    geometry: &Geometry,
    axis: &AxisAnalysis,
    layer: usize,
    width: f32,
    height: f32,
) -> Option<Path<Pixels>> {
    let mut path = if layer == 0 || layer == 8 {
        PathBuilder::fill()
    } else {
        PathBuilder::stroke(px(stroke_width(layer)))
    };
    if dashed(layer) {
        path = path.dash_array(&[px(1.5), px(3.)]);
    }
    let at = |p: Sample| point(px(p[0] as f32), px(p[1] as f32));
    primitives(
        geometry,
        axis,
        layer,
        width as f64,
        height as f64,
        |primitive| match primitive {
            Primitive::Line([a, b]) => {
                path.move_to(at(a));
                path.line_to(at(b));
            }
            Primitive::Move(point) => path.move_to(at(point)),
            Primitive::Vertex(point) => path.line_to(at(point)),
            Primitive::Close => path.close(),
            Primitive::Circle([x, y]) => {
                // Four cubic arcs retain genuinely round dots at every DPI.
                let radius = 2.7;
                let k = radius * 0.552_284_749_830_793_6;
                path.move_to(at([x + radius, y]));
                path.cubic_bezier_to(
                    at([x, y + radius]),
                    at([x + radius, y + k]),
                    at([x + k, y + radius]),
                );
                path.cubic_bezier_to(
                    at([x - radius, y]),
                    at([x - k, y + radius]),
                    at([x - radius, y + k]),
                );
                path.cubic_bezier_to(
                    at([x, y - radius]),
                    at([x - radius, y - k]),
                    at([x - k, y - radius]),
                );
                path.cubic_bezier_to(
                    at([x + radius, y]),
                    at([x + k, y - radius]),
                    at([x + radius, y - k]),
                );
                path.close();
            }
        },
    );
    path.build().ok()
}

fn legend_item(layer: usize, text: String, dark: bool) -> AnyElement {
    let mut ink: Hsla = rgb(color(layer, dark)).into();
    ink.a = if layer == 0 { 0.35 } else { 1. };
    let marker = if layer == 8 {
        div().size_1p5().rounded_full().bg(ink)
    } else {
        div()
            .w_4()
            .h(rems(if layer == 0 { 0.5 } else { 0.125 }))
            .bg(ink)
    };
    div()
        .flex()
        .items_center()
        .gap_1()
        .child(marker.flex_shrink_0())
        .child(text)
        .into_any_element()
}

pub(super) fn chart(data: Arc<ChartData>, cx: &App) -> AnyElement {
    let axis = data.axis;
    let geometry = &data.geometry;
    let x = geometry.x;
    let y = geometry.y;
    let x_ticks = data.x_ticks;
    let y_ticks = data.y_ticks;
    let fingerprint = geometry.fingerprint;
    let stats = &data.analysis.axes[axis].statistics;
    let slopes = stats.adjacent_slope_range;
    let slope_labels = [
        format!(
            "{} {}",
            tr("temperature.chart.slope.average"),
            statistic(stats.linear_slope)
        ),
        format!(
            "{} {}",
            tr("temperature.chart.slope.maximum"),
            statistic(slopes.map(|range| range[1]))
        ),
        format!(
            "{} {} /°C",
            tr("temperature.chart.slope.minimum"),
            statistic(slopes.map(|range| range[0]))
        ),
    ];
    let range_text = format!(
        "{}: {} °/s",
        tr("temperature.chart.bias_range"),
        statistic(Some(stats.bias_range))
    );
    let linear_text = format!(
        "{} (SSR={})",
        tr("temperature.chart.legend.linear"),
        statistic(stats.linear_residual_sum_squares)
    );
    let mls_text = format!("MLS (SSR={})", statistic(stats.mls_residual_sum_squares));
    let border = cx.theme().border;
    let muted = cx.theme().muted_foreground;
    let dark = cx.theme().is_dark();
    let painted = data;
    let signal = canvas(
        |_, _, _| (),
        move |bounds, _, window, cx| {
            let clip = window.content_mask().bounds;
            if bounds.right() <= clip.left()
                || bounds.left() >= clip.right()
                || bounds.bottom() <= clip.top()
                || bounds.top() >= clip.bottom()
            {
                return;
            }
            let width = f32::from(bounds.size.width);
            let height = f32::from(bounds.size.height);
            if width <= 0. || height <= 0. {
                return;
            }
            let key = ShapeKey::new((
                fingerprint,
                width.to_bits(),
                height.to_bits(),
                x.map(f64::to_bits),
                y.map(f64::to_bits),
            ))
            .finish();
            let caches = PathCaches::for_paint("thermal-dump-paths", window, cx);
            window.with_content_mask(Some(ContentMask { bounds }), |window| {
                caches.update(cx, |caches, _| {
                    if let Some(path) = caches.slot(LAYERS).get(key, bounds.origin, || {
                        let mut grid = PathBuilder::stroke(px(0.7));
                        for value in x_ticks.iter() {
                            let x = (fraction(value, x) * f64::from(width)) as f32;
                            grid.move_to(point(px(x), px(0.)));
                            grid.line_to(point(px(x), px(height)));
                        }
                        for value in y_ticks.iter() {
                            let y = ((1. - fraction(value, y)) * f64::from(height)) as f32;
                            grid.move_to(point(px(0.), px(y)));
                            grid.line_to(point(px(width), px(y)));
                        }
                        grid.build().ok()
                    }) {
                        let mut grid_ink = border;
                        grid_ink.a *= 0.55;
                        window.paint_path(path, grid_ink);
                    }
                    for layer in 0..LAYERS {
                        if let Some(path) = caches.slot(layer).get(key, bounds.origin, || {
                            native_path(
                                &painted.geometry,
                                &painted.analysis.axes[axis],
                                layer,
                                width,
                                height,
                            )
                        }) {
                            let mut ink: Hsla = rgb(color(layer, dark)).into();
                            ink.a = opacity(layer);
                            window.paint_path(path, ink);
                        }
                    }
                });
            });
        },
    )
    .size_full();
    div()
        .id(("thermal-dump-chart", axis))
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .text_xs()
        .child(div().text_center().text_base().child(format!(
            "{} {}",
            tr("temperature.chart.gyroscope"),
            ["X", "Y", "Z"][axis]
        )))
        .child(
            div()
                .flex()
                .flex_wrap()
                .justify_center()
                .gap_x_2()
                .children(slope_labels.into_iter().map(|text| div().child(text))),
        )
        .child(div().text_center().mb_8().child(range_text))
        .child(
            div()
                .flex()
                .w_full()
                .pr_6()
                .child(
                    div()
                        .relative()
                        .w_20()
                        .self_stretch()
                        .flex_shrink_0()
                        .text_color(muted)
                        .child(
                            div()
                                .absolute()
                                .top(-rems(2.))
                                .right_2()
                                .child(tr("temperature.chart.axis.bias")),
                        )
                        .children(y_ticks.iter().map(|value| {
                            div()
                                .absolute()
                                .right_2()
                                .top(relative((1. - fraction(value, y)) as f32))
                                .mt(-rems(0.5))
                                .child(label(value, y_ticks.step))
                        })),
                )
                .child(
                    // Resolve width after the relative Y and trailing label gutters.
                    // A square border box keeps the inset signal square as well;
                    // absolute paint content cannot impose an intrinsic height.
                    div()
                        .relative()
                        .flex_1()
                        .min_w_0()
                        .aspect_square()
                        .border_1()
                        .border_color(border)
                        .child(div().absolute().inset_0().child(signal)),
                ),
        )
        .child(
            div()
                .relative()
                .ml_20()
                .mr_6()
                .h_6()
                .text_color(muted)
                .children(x_ticks.iter().map(|value| {
                    div()
                        .absolute()
                        .top_1()
                        .left(relative(fraction(value, x) as f32))
                        .ml(-rems(2.))
                        .w_16()
                        .text_center()
                        .child(label(value, x_ticks.step))
                })),
        )
        .child(
            div()
                .ml_20()
                .mr_6()
                .text_center()
                .child(tr("temperature.chart.axis.temperature")),
        )
        .child(
            div()
                .mt_1p5()
                .flex()
                .flex_wrap()
                .justify_center()
                .gap_x_3()
                .gap_y_0p5()
                .child(legend_item(1, linear_text, dark))
                .child(legend_item(4, mls_text, dark)),
        )
        .child(
            div()
                .flex()
                .flex_wrap()
                .justify_center()
                .gap_x_3()
                .gap_y_0p5()
                .child(legend_item(
                    8,
                    tr("temperature.chart.legend.raw_points"),
                    dark,
                ))
                .child(legend_item(
                    0,
                    tr("temperature.chart.legend.tolerance_band"),
                    dark,
                ))
                .child(
                    div()
                        .text_color(muted)
                        .child(tr("temperature.chart.legend.extrapolation")),
                ),
        )
        .into_any_element()
}

fn svg_path(
    geometry: &Geometry,
    axis: &AxisAnalysis,
    layer: usize,
    width: f64,
    height: f64,
) -> String {
    let mut result = String::new();
    let mut previous_end: Option<Sample> = None;
    primitives(geometry, axis, layer, width, height, |primitive| {
        if !matches!(primitive, Primitive::Line(_)) {
            previous_end = None;
        }
        match primitive {
            Primitive::Line([a, b]) => {
                // A new SVG subpath restarts the dash phase. Keep adjacent
                // short curve samples connected so extrapolation stays dotted.
                let connected = previous_end.is_some_and(|end| {
                    (end[0] - a[0]).abs() < 1e-9 && (end[1] - a[1]).abs() < 1e-9
                });
                if !connected {
                    let _ = write!(result, "M{:.3},{:.3}", a[0], a[1]);
                }
                let _ = write!(result, "L{:.3},{:.3}", b[0], b[1]);
                previous_end = Some(b);
            }
            Primitive::Move(point) => {
                let _ = write!(result, "M{:.3},{:.3}", point[0], point[1]);
            }
            Primitive::Vertex(point) => {
                let _ = write!(result, "L{:.3},{:.3}", point[0], point[1]);
            }
            Primitive::Close => result.push('Z'),
            Primitive::Circle([x, y]) => {
                let _ = write!(
                    result,
                    "M{:.3},{y:.3}a2.7,2.7 0 1,0 5.4,0a2.7,2.7 0 1,0 -5.4,0Z",
                    x - 2.7
                );
            }
        }
    });
    result
}

fn statistic(value: Option<f64>) -> String {
    value.filter(|value| value.is_finite()).map_or_else(
        || "—".into(),
        |value| {
            if value != 0. && value.abs() < 0.000001 {
                format!("{value:.3e}")
            } else {
                format!("{value:.6}")
            }
        },
    )
}

pub(super) fn svg(analysis: &ThermalDumpAnalysis, range: [f64; 2]) -> String {
    let range = finite_range(range);
    let x_ticks = ticks(range, 4);
    let mut out = String::with_capacity(160_000);
    out.push_str("<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"1500\" height=\"660\" viewBox=\"0 0 1500 660\"><rect width=\"1500\" height=\"660\" fill=\"white\"/><g font-family=\"sans-serif\" fill=\"#202027\">");
    for axis in 0..3 {
        let geometry = Geometry::new(analysis, axis, range);
        let y_ticks = ticks(geometry.y, 6);
        let left = 76 + axis * 500;
        let center = left + 200;
        let name = ["X", "Y", "Z"][axis];
        let stats = &analysis.axes[axis].statistics;
        let slopes = stats.adjacent_slope_range;
        let _ = write!(
            out,
            "<text x=\"{center}\" y=\"24\" text-anchor=\"middle\" font-size=\"17\">Gyro {name}</text><text x=\"{center}\" y=\"43\" text-anchor=\"middle\" font-size=\"12\">Slope avg {} · max {} · min {} /°C</text><text x=\"{center}\" y=\"61\" text-anchor=\"middle\" font-size=\"12\">Bias range: {} °/s</text><defs><clipPath id=\"axis-{axis}\"><rect width=\"400\" height=\"460\"/></clipPath></defs><g transform=\"translate({left},78)\">",
            statistic(stats.linear_slope),
            statistic(slopes.map(|range| range[1])),
            statistic(slopes.map(|range| range[0])),
            statistic(Some(stats.bias_range)),
        );
        for value in x_ticks.iter() {
            let x = fraction(value, geometry.x) * 400.;
            let _ = write!(
                out,
                "<path d=\"M{x:.3},0V460\" fill=\"none\" stroke=\"#dddddd\" stroke-width=\"0.7\"/><text x=\"{x:.3}\" y=\"482\" text-anchor=\"middle\" font-size=\"12\">{}</text>",
                label(value, x_ticks.step),
            );
        }
        for value in y_ticks.iter() {
            let y = (1. - fraction(value, geometry.y)) * 460.;
            let baseline = y + 4.;
            let _ = write!(
                out,
                "<path d=\"M0,{y:.3}H400\" fill=\"none\" stroke=\"#dddddd\" stroke-width=\"0.7\"/><text x=\"-9\" y=\"{baseline:.3}\" text-anchor=\"end\" font-size=\"12\">{}</text>",
                label(value, y_ticks.step),
            );
        }
        let _ = write!(out, "<g clip-path=\"url(#axis-{axis})\">");
        for layer in 0..LAYERS {
            let path = svg_path(&geometry, &analysis.axes[axis], layer, 400., 460.);
            if path.is_empty() {
                continue;
            }
            let ink = format!("#{:06x}", color(layer, false));
            let fill = layer == 0 || layer == 8;
            let _ = write!(
                out,
                "<path d=\"{path}\" fill=\"{}\" stroke=\"{}\" stroke-width=\"{}\" opacity=\"{}\"{} />",
                if fill { ink.as_str() } else { "none" },
                if fill { "none" } else { ink.as_str() },
                stroke_width(layer),
                opacity(layer),
                if dashed(layer) {
                    " stroke-dasharray=\"1.5 3\""
                } else {
                    ""
                },
            );
        }
        out.push_str("</g><rect width=\"400\" height=\"460\" fill=\"none\" stroke=\"#888888\"/><text x=\"200\" y=\"505\" text-anchor=\"middle\" font-size=\"13\">Temperature (°C)</text><text transform=\"translate(-61,230) rotate(-90)\" text-anchor=\"middle\" font-size=\"13\">Bias (°/s)</text>");
        let _ = write!(
            out,
            "<path d=\"M12,526h18\" stroke=\"#{:06x}\" stroke-width=\"2\"/><text x=\"35\" y=\"530\" font-size=\"11\">Linear (SSR={})</text><path d=\"M216,526h18\" stroke=\"#{:06x}\" stroke-width=\"2.5\"/><text x=\"239\" y=\"530\" font-size=\"11\">MLS (SSR={})</text><circle cx=\"18\" cy=\"547\" r=\"2.7\" fill=\"#{:06x}\"/><text x=\"29\" y=\"551\" font-size=\"11\">Raw points</text><rect x=\"112\" y=\"543\" width=\"16\" height=\"8\" fill=\"#{:06x}\" opacity=\"0.35\"/><text x=\"134\" y=\"551\" font-size=\"11\">±0.01 band</text><path d=\"M235,547h18\" stroke=\"#{:06x}\" stroke-width=\"1.6\" stroke-dasharray=\"1.5 3\"/><text x=\"259\" y=\"551\" font-size=\"11\">Dotted: extrapolation</text></g>",
            color(1, false),
            statistic(stats.linear_residual_sum_squares),
            color(4, false),
            statistic(stats.mls_residual_sum_squares),
            color(8, false),
            color(0, false),
            color(4, false),
        );
    }
    out.push_str("</g></svg>");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use slimenrf_recording::thermal_dump::AxisStatistics;

    fn fixture() -> ThermalDumpAnalysis {
        ThermalDumpAnalysis {
            temperature_range_c: Some([10., 20.]),
            axes: std::array::from_fn(|_| AxisAnalysis {
                raw: vec![[10., 0.1], [20., 0.2]],
                linear: vec![[10., 0.1], [20., 0.2]],
                mls: vec![[10., 0.12], [20., 0.18]],
                extrapolation_low: vec![[4., 0.05], [10., 0.12]],
                extrapolation_high: vec![[20., 0.18], [26., 0.25]],
                statistics: AxisStatistics {
                    linear_slope: Some(0.01),
                    linear_residual_sum_squares: Some(0.),
                    mls_residual_sum_squares: Some(0.0008),
                    adjacent_slope_range: Some([0.01, 0.01]),
                    bias_range: 0.1,
                },
            }),
        }
    }

    fn assert_samples(actual: &[Sample], expected: &[Sample]) {
        assert_eq!(actual.len(), expected.len());
        for (actual, expected) in actual.iter().flatten().zip(expected.iter().flatten()) {
            assert!((actual - expected).abs() < 1e-9, "{actual} != {expected}");
        }
    }

    #[test]
    fn prepared_charts_reuse_payloads_and_bands_for_unchanged_inputs() {
        let mut analysis = fixture();
        for (axis_index, axis) in analysis.axes.iter_mut().enumerate() {
            let offset = axis_index as f64 * 0.3;
            axis.raw = (0..512)
                .map(|index| {
                    let fraction = f64::from(index) / 511.;
                    [10. + 10. * fraction, 0.1 + offset + 0.1 * fraction]
                })
                .collect();
        }
        let analysis = Arc::new(analysis);
        let mut cache = ChartCache::default();
        let charts: [_; 3] = std::array::from_fn(|axis| cache.get(&analysis, axis, [4., 26.]));
        assert_samples(
            &charts.each_ref().map(|chart| chart.geometry.y),
            &[[0.0268, 0.2732], [0.0124, 0.5276], [-0.0056, 0.8456]],
        );
        for _ in 0..64 {
            for (axis, original) in charts.iter().enumerate() {
                let reused = cache.get(&analysis, axis, [4., 26.]);
                assert!(Arc::ptr_eq(original, &reused));
                assert_eq!(
                    original.geometry.band.as_ptr(),
                    reused.geometry.band.as_ptr()
                );
                assert_eq!(reused.analysis.axes[axis].raw.len(), 512);
            }
        }
        assert!(Arc::ptr_eq(&charts[0], &cache.get(&analysis, 0, [26., 4.])));
        let zoomed = cache.get(&analysis, 0, [12., 18.]);
        assert!(!Arc::ptr_eq(&charts[0], &zoomed));
        assert_ne!(
            charts[0].geometry.band.as_ptr(),
            zoomed.geometry.band.as_ptr()
        );
        assert_eq!(zoomed.geometry.x, [12., 18.]);
        assert_samples(
            &zoomed.geometry.band,
            &[[12., 0.142], [18., 0.178], [18., 0.158], [12., 0.122]],
        );
        assert!(Arc::ptr_eq(&charts[1], &cache.get(&analysis, 1, [4., 26.])));
    }

    #[test]
    fn chart_identity_changes_do_not_replace_content_based_path_fingerprints() {
        let mut cache = ChartCache::default();
        let first = cache.get(&Arc::new(fixture()), 0, [4., 26.]);
        let frozen_export = svg(&first.analysis, first.geometry.x);
        let identical = cache.get(&Arc::new(fixture()), 0, [4., 26.]);
        assert!(!Arc::ptr_eq(&first, &identical));
        assert_eq!(first.geometry.fingerprint, identical.geometry.fingerprint);
        let mut changed = fixture();
        changed.axes[0].raw[0][1] = 0.11;
        let changed = cache.get(&Arc::new(changed), 0, [12., 18.]);
        assert_ne!(first.geometry.fingerprint, changed.geometry.fingerprint);
        assert_eq!(first.geometry.x, [4., 26.]);
        assert_eq!(changed.geometry.x, [12., 18.]);
        assert_eq!(first.analysis.axes[0].raw[0], [10., 0.1]);
        assert_eq!(svg(&first.analysis, first.geometry.x), frozen_export);
        assert!(frozen_export.contains("Slope avg 0.010000"));
        assert!(frozen_export.contains("Linear (SSR=0.000000)"));
        assert!(frozen_export.contains("MLS (SSR=0.000800)"));
    }

    #[test]
    fn cached_geometry_and_svg_match_literal_negative_temperature_viewport() {
        let mut analysis = fixture();
        analysis.temperature_range_c = Some([-6., 4.]);
        for axis in &mut analysis.axes {
            for series in [
                &mut axis.raw,
                &mut axis.linear,
                &mut axis.mls,
                &mut axis.extrapolation_low,
                &mut axis.extrapolation_high,
            ] {
                for point in series {
                    point[0] -= 16.;
                }
            }
        }
        let analysis = Arc::new(analysis);
        let mut cache = ChartCache::default();
        let chart = cache.get(&analysis, 0, [-4., 2.]);
        assert_eq!(chart.geometry.x, [-4., 2.]);
        assert_samples(&[chart.geometry.y], &[[0.0268, 0.2732]]);
        assert_samples(
            &chart.geometry.band,
            &[[-4., 0.142], [2., 0.178], [2., 0.158], [-4., 0.122]],
        );
        assert_eq!(chart.x_ticks.iter().collect::<Vec<_>>(), [-4., -2., 0., 2.]);
        let mut native_line = Vec::new();
        primitives(
            &chart.geometry,
            &analysis.axes[0],
            1,
            400.,
            460.,
            |primitive| match primitive {
                Primitive::Line(points) => native_line.extend(points),
                _ => panic!("linear fit must emit clipped line segments"),
            },
        );
        assert_samples(
            &native_line,
            &[[0., 286.0064935064935], [400., 173.9935064935065]],
        );
        let export = svg(&analysis, [-4., 2.]);
        for expected in [
            "M0.000,244.935L400.000,177.727L400.000,215.065L0.000,282.273Z",
            "M0.000,286.006L400.000,173.994",
            "M0.000,263.604L400.000,196.396",
        ] {
            let attribute = format!("d=\"{expected}\"");
            assert_eq!(export.matches(attribute.as_str()).count(), 3);
        }
        for range in [[-12., 10.], [-4., 2.]] {
            let export = svg(&analysis, range);
            for axis in 0..3 {
                let chart = cache.get(&analysis, axis, range);
                for layer in 0..LAYERS {
                    let path = svg_path(&chart.geometry, &analysis.axes[axis], layer, 400., 460.);
                    if !path.is_empty() {
                        assert!(
                            export.contains(&format!("d=\"{path}\"")),
                            "axis {axis}, layer {layer}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn chart_cache_releases_superseded_payloads_and_analysis() {
        let mut cache = ChartCache::default();
        let analysis = Arc::new(fixture());
        let previous_analysis = Arc::downgrade(&analysis);
        let previous_charts: [_; 3] =
            std::array::from_fn(|axis| Arc::downgrade(&cache.get(&analysis, axis, [4., 26.])));
        drop(analysis);
        assert!(previous_analysis.upgrade().is_some());
        let replacement = Arc::new(fixture());
        for (axis, previous_chart) in previous_charts.iter().enumerate() {
            let chart = cache.get(&replacement, axis, [12., 18.]);
            assert_eq!(chart.geometry.x, [12., 18.]);
            assert_eq!(chart.geometry.band.len(), 4);
            assert!(previous_chart.upgrade().is_none());
        }
        assert!(previous_analysis.upgrade().is_none());
    }

    #[test]
    fn ranges_remain_finite_ordered_and_projectable() {
        for input in [
            [f64::NAN, 2.],
            [0., f64::INFINITY],
            [20., 10.],
            [8., 8.],
            [f64::MAX, f64::MAX],
            [-f64::MAX, f64::MAX],
        ] {
            let range = finite_range(input);
            assert!(range.iter().all(|v| v.is_finite()));
            assert!(range[0] < range[1]);
            assert_eq!(fraction(range[0], range), 0.);
            assert_eq!(fraction(range[1], range), 1.);
            let axis_ticks = ticks(range, 6);
            assert!(axis_ticks.iter().all(f64::is_finite));
            assert!(
                axis_ticks
                    .iter()
                    .all(|value| (range[0]..=range[1]).contains(&value))
            );
        }
    }

    #[test]
    fn clipping_retains_crossings_and_rejects_outside_and_nonfinite() {
        assert_eq!(
            clipped_segment([-10., 30.], [110., 30.], 100., 60.),
            Some([[0., 30.], [100., 30.]])
        );
        assert_eq!(clipped_segment([-1., -1.], [-1., 70.], 100., 60.), None);
        assert_eq!(clipped_segment([f64::NAN, 0.], [10., 10.], 100., 60.), None);
        let vertical = clipped_segment([50., -20.], [50., 80.], 100., 60.).unwrap();
        for (actual, expected) in vertical.into_iter().flatten().zip([50., 0., 50., 60.]) {
            assert!((actual - expected).abs() < 1e-10);
        }
    }

    #[test]
    fn temperature_clipping_precedes_projection_for_extreme_zoom() {
        let range = [0., f64::MIN_POSITIVE];
        let points = temperature_segment([-1., -1.], [1., 1.], range).unwrap();
        assert_eq!(points[0][0], range[0]);
        assert_eq!(points[1][0], range[1]);
        for point in points {
            assert!(fraction(point[0], range).is_finite());
        }
        assert_eq!(temperature_segment([2., 1.], [3., 2.], range), None);
    }

    #[test]
    fn full_view_contains_every_layer_and_geometry_changes_invalidate_cache() {
        let mut analysis = fixture();
        let geometry = Geometry::new(&analysis, 0, [4., 26.]);
        for layer in 0..LAYERS {
            assert!(!svg_path(&geometry, &analysis.axes[0], layer, 400., 540.).is_empty());
        }
        let original = geometry.fingerprint;
        analysis.axes[0].raw[0][1] += 0.001;
        assert_ne!(original, Geometry::new(&analysis, 0, [4., 26.]).fingerprint);
    }

    #[test]
    fn extensions_use_global_slope_not_boundary_slope() {
        let analysis = fixture();
        let geometry = Geometry::new(&analysis, 0, [4., 26.]);
        let low = geometry.linear_low.unwrap();
        assert!((low[0][1] - 0.04).abs() < 1e-12);
        assert!((low[1][1] - 0.1).abs() < 1e-12);
        assert!(geometry.y[0] < 0.04 && geometry.y[1] > 0.26);
    }

    #[test]
    fn native_and_svg_geometry_respects_current_viewport() {
        let analysis = fixture();
        let geometry = Geometry::new(&analysis, 0, [12., 18.]);
        for layer in [2, 3, 5, 6, 7, 8] {
            assert!(svg_path(&geometry, &analysis.axes[0], layer, 400., 540.).is_empty());
        }
        for layer in [0, 1, 4] {
            let mut count = 0;
            primitives(
                &geometry,
                &analysis.axes[0],
                layer,
                400.,
                540.,
                |primitive| {
                    let points: &[Sample] = match &primitive {
                        Primitive::Line(points) => points,
                        Primitive::Move(point)
                        | Primitive::Vertex(point)
                        | Primitive::Circle(point) => std::slice::from_ref(point),
                        Primitive::Close => &[],
                    };
                    for &[x, y] in points {
                        assert!((0. ..=400.).contains(&x));
                        assert!((0. ..=540.).contains(&y));
                    }
                    count += 1;
                },
            );
            assert!(count > 0);
        }
    }

    #[test]
    fn svg_contains_all_axes_styles_units_statistics_and_snapshot_range() {
        let analysis = fixture();
        let export = svg(&analysis, [4., 26.]);
        for text in [
            "Gyro X",
            "Gyro Y",
            "Gyro Z",
            "Temperature (°C)",
            "Bias (°/s)",
            "Dotted: extrapolation",
            "±0.01 band",
            "Slope avg 0.010000",
            "Linear (SSR=0.000000)",
            "MLS (SSR=0.000800)",
            "viewBox=\"0 0 1500 660\"",
            "stroke-dasharray=\"1.5 3\"",
            "clip-path=\"url(#axis-2)\"",
        ] {
            assert!(export.contains(text), "missing {text}");
        }
        assert!(!export.contains("NaN"));
        assert!(!export.contains("inf"));
        assert_ne!(export, svg(&analysis, [12., 18.]));
    }

    #[test]
    fn exported_dense_mls_tails_keep_dash_phase_and_stay_separate_from_measured_fit() {
        fn attribute<'a>(element: &'a str, name: &str) -> Option<&'a str> {
            let marker = format!(" {name}=\"");
            let value = element.split_once(marker.as_str())?.1;
            Some(value.split_once('"')?.0)
        }

        fn polyline(path: &str) -> Vec<Sample> {
            path.split(['M', 'L'])
                .filter(|coordinates| !coordinates.is_empty())
                .map(|coordinates| {
                    let (x, y) = coordinates.split_once(',').unwrap();
                    [x.parse().unwrap(), y.parse().unwrap()]
                })
                .collect()
        }

        let mut analysis = fixture();
        for axis in &mut analysis.axes {
            axis.mls = axis.linear.clone();
            axis.extrapolation_low = (0..50)
                .map(|index| {
                    let x = 4. + 6. * index as f64 / 49.;
                    [x, 0.1 + (x - 10.) * 0.0001]
                })
                .collect();
            axis.extrapolation_high = (0..50)
                .map(|index| {
                    let x = 20. + 6. * index as f64 / 49.;
                    [x, 0.2 + (x - 20.) * 0.0001]
                })
                .collect();
        }
        let export = svg(&analysis, [4., 46.]);
        let mut tails = 0;
        let mut measured = 0;
        for element in export.split("<path").skip(1) {
            let element = element.split_once('>').unwrap().0;
            if attribute(element, "stroke") != Some("#245ac8") {
                continue;
            }
            let path = attribute(element, "d").unwrap();
            if !path.contains('L') {
                continue; // Legend markers use relative h, not sampled fit geometry.
            }
            let points = polyline(path);
            assert_eq!(path.matches('M').count(), 1);
            if let Some(pattern) = attribute(element, "stroke-dasharray") {
                assert_eq!(pattern, "1.5 3");
                assert_eq!(points.len(), 50);
                // These source-derived short segments are individually shorter
                // than one dash, but the continuous path spans many dash periods.
                let lengths: Vec<_> = points
                    .windows(2)
                    .map(|pair| (pair[1][0] - pair[0][0]).hypot(pair[1][1] - pair[0][1]))
                    .collect();
                assert!(lengths.iter().all(|length| *length < 1.5));
                assert!(lengths.iter().sum::<f64>() > 4.5 * 10.);
                let measured_low = (10. - 4.) / 42. * 400.;
                let measured_high = (20. - 4.) / 42. * 400.;
                assert!(
                    points.iter().all(|point| point[0] <= measured_low + 0.001)
                        || points.iter().all(|point| point[0] >= measured_high - 0.001)
                );
                tails += 1;
            } else {
                assert_eq!(points.len(), 2);
                measured += 1;
            }
        }
        assert_eq!(tails, 6);
        assert_eq!(measured, 3);
    }

    #[test]
    fn singleton_has_a_visible_point_without_fabricated_curves() {
        let mut analysis = fixture();
        for axis in &mut analysis.axes {
            axis.raw = vec![[15., 0.15]];
            axis.linear.clear();
            axis.mls.clear();
            axis.extrapolation_low.clear();
            axis.extrapolation_high.clear();
        }
        analysis.temperature_range_c = Some([15., 15.]);
        let geometry = Geometry::new(&analysis, 0, [15., 15.]);
        assert!(!svg_path(&geometry, &analysis.axes[0], 8, 400., 540.).is_empty());
        for layer in 0..7 {
            assert!(svg_path(&geometry, &analysis.axes[0], layer, 400., 540.).is_empty());
        }
    }

    #[test]
    fn nice_ticks_use_human_intervals_and_step_precision() {
        let temperature = ticks([4., 46.], 4);
        assert_eq!(temperature.step, 10.);
        assert_eq!(temperature.iter().collect::<Vec<_>>(), [10., 20., 30., 40.]);
        let bias = ticks([-0.991, -0.977], 6);
        assert!((bias.step - 0.0025).abs() < 1e-12);
        assert_eq!(label(-0.9875, bias.step), "-0.9875");
        assert_eq!(label(15., 5.), "15");
        assert_eq!(label(0., 0.01), "0.00");
        let zoomed = ticks([1e30 - 1e20, 1e30 + 1e20], 4);
        assert!(zoomed.iter().zip(zoomed.iter().skip(1)).all(|(a, b)| a < b));
    }

    #[test]
    fn band_is_one_continuous_mls_following_polygon() {
        let analysis = fixture();
        let geometry = Geometry::new(&analysis, 0, [4., 26.]);
        let expected = [
            [4., 0.06],
            [10., 0.13],
            [20., 0.19],
            [26., 0.26],
            [26., 0.24],
            [20., 0.17],
            [10., 0.11],
            [4., 0.04],
        ];
        assert_eq!(geometry.band.len(), expected.len());
        for (actual, expected) in geometry
            .band
            .iter()
            .flatten()
            .zip(expected.into_iter().flatten())
        {
            assert!((actual - expected).abs() < 1e-12);
        }
        let mut moves = 0;
        let mut closes = 0;
        primitives(
            &geometry,
            &analysis.axes[0],
            0,
            400.,
            460.,
            |primitive| match primitive {
                Primitive::Move(_) => moves += 1,
                Primitive::Close => closes += 1,
                _ => {}
            },
        );
        assert_eq!((moves, closes), (1, 1));
        let zoomed = Geometry::new(&analysis, 0, [12., 18.]);
        assert_eq!(zoomed.band.len(), 4);
        assert_eq!(zoomed.band[0][0], 12.);
        assert_eq!(zoomed.band[1][0], 18.);
    }

    #[test]
    fn empty_analysis_has_finite_axes_without_fabricated_signal() {
        let mut analysis = fixture();
        analysis.temperature_range_c = None;
        for axis in &mut analysis.axes {
            axis.raw.clear();
            axis.linear.clear();
            axis.mls.clear();
            axis.extrapolation_low.clear();
            axis.extrapolation_high.clear();
        }
        let geometry = Geometry::new(&analysis, 0, [0., 1.]);
        assert!(geometry.y.iter().all(|value| value.is_finite()));
        for layer in 0..LAYERS {
            assert!(svg_path(&geometry, &analysis.axes[0], layer, 400., 460.).is_empty());
        }
    }
}
