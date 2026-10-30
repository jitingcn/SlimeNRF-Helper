use super::*;
use gpui_kit::base::plot::{PathCaches, ShapeKey};
use slimenrf_recording::replay::analysis::{
    AnalysisSummary, accel_norm_deviation_g, euler_deg, magnetic_diagnostics, wrap_degrees,
};
use std::sync::atomic::AtomicU64;

const DIRECTION_COLORS_DARK: [u32; 6] =
    [0xd65a63, 0x45a879, 0x558fe0, 0xe4a647, 0xb776df, 0x31b8c6];
const DIRECTION_COLORS_LIGHT: [u32; 6] =
    [0xab3544, 0x206d43, 0x3269b1, 0x805a12, 0x81509f, 0x146f7a];

pub(super) fn direction_color(axis: usize, dark: bool) -> Hsla {
    rgb(if dark {
        DIRECTION_COLORS_DARK[axis]
    } else {
        DIRECTION_COLORS_LIGHT[axis]
    })
    .into()
}

pub(super) fn reference_color(index: usize, dark: bool) -> Hsla {
    direction_color(1 - index, dark)
}

static NEXT_PLOT_CACHE_ID: AtomicU64 = AtomicU64::new(1);
const MAX_BINS: usize = 512;
const MAX_RUNS: usize = 4;
const MAX_QUANTILES: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ChartId {
    Gyro,
    Accel,
    Euler,
    Heading,
    Orientation,
    Bias,
    Rest,
    AccelDeviation,
    Temperature,
    Mag,
    MagNorm,
    MagDip,
    MagEarth,
    MagCorrection,
    MagDist,
    ThermalCompensation,
    ThermalResidual,
}
impl ChartId {
    pub(super) const ALL: [Self; 17] = [
        Self::Gyro,
        Self::Accel,
        Self::Euler,
        Self::Heading,
        Self::Orientation,
        Self::Bias,
        Self::Rest,
        Self::AccelDeviation,
        Self::Temperature,
        Self::Mag,
        Self::MagNorm,
        Self::MagDip,
        Self::MagEarth,
        Self::MagCorrection,
        Self::MagDist,
        Self::ThermalCompensation,
        Self::ThermalResidual,
    ];
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Gyro => "replay.chart.gyroscope",
            Self::Accel => "replay.chart.acceleration",
            Self::Euler => "replay.chart.euler",
            Self::Heading => "replay.chart.heading",
            Self::Orientation => "replay.chart.quaternion",
            Self::Bias => "replay.chart.gyro_bias",
            Self::Rest => "replay.chart.rest",
            Self::AccelDeviation => "replay.chart.acceleration_deviation",
            Self::Temperature => "replay.chart.temperature",
            Self::Mag => "replay.chart.magnetometer",
            Self::MagNorm => "replay.chart.magnetic_norm",
            Self::MagDip => "replay.chart.magnetic_inclination",
            Self::MagEarth => "replay.chart.magnetic_earth_vector",
            Self::MagCorrection => "replay.chart.magnetic_correction",
            Self::MagDist => "replay.chart.magnetic_disturbance",
            Self::ThermalCompensation => "replay.chart.thermal_compensation",
            Self::ThermalResidual => "replay.chart.thermal_residual",
        }
    }
    pub(super) fn unit(self) -> &'static str {
        match self {
            Self::Gyro | Self::Bias | Self::ThermalCompensation | Self::ThermalResidual => "°/s",
            Self::Accel | Self::AccelDeviation => "g",
            Self::Euler | Self::Heading | Self::MagDip | Self::MagCorrection => "°",
            Self::Mag | Self::MagNorm | Self::MagEarth => "G",
            Self::Orientation => "unitless",
            Self::Rest | Self::MagDist => "0 / 1",
            Self::Temperature => "°C",
        }
    }
    pub(super) fn legend(self) -> &'static [&'static str] {
        match self {
            Self::Euler => &["replay.axis.roll", "replay.axis.pitch", "replay.axis.yaw"],
            Self::Heading => &[
                "replay.legend.heading_6d",
                "replay.legend.heading_9d",
                "replay.legend.heading_difference",
            ],
            Self::Orientation => &[
                "replay.axis.w",
                "replay.axis.x",
                "replay.axis.y",
                "replay.axis.z",
            ],
            Self::MagNorm | Self::MagDip => &["replay.legend.observed", "replay.legend.first_mean"],
            Self::Rest => &["replay.legend.rest"],
            Self::MagDist => &["replay.legend.disturbed"],
            Self::AccelDeviation => &["replay.legend.acceleration_deviation"],
            Self::Temperature => &["replay.legend.temperature"],
            Self::MagCorrection => &["replay.legend.magnetic_correction"],
            Self::MagEarth => &["replay.axis.x", "replay.axis.y", "replay.axis.z"],
            _ => &["replay.axis.x", "replay.axis.y", "replay.axis.z"],
        }
    }
    pub(super) fn color(self, channel: usize, dark: bool) -> Hsla {
        if channel < 3 {
            direction_color(channel, dark)
        } else {
            rgb(if dark { 0xb283d4 } else { 0x765092 }).into()
        }
    }
    pub(super) fn robust(self) -> bool {
        !matches!(
            self,
            Self::Euler
                | Self::Heading
                | Self::Orientation
                | Self::Rest
                | Self::MagDip
                | Self::MagCorrection
                | Self::MagDist
        )
    }
    fn state(self) -> bool {
        matches!(self, Self::Rest | Self::MagDist)
    }
    fn wrapped(self) -> bool {
        matches!(self, Self::Euler | Self::Heading | Self::MagCorrection)
    }
    fn sparse(self) -> bool {
        matches!(
            self,
            Self::Accel
                | Self::AccelDeviation
                | Self::Mag
                | Self::MagNorm
                | Self::MagDip
                | Self::MagEarth
        )
    }
    fn count(self) -> usize {
        self.legend().len()
    }
    fn available(
        self,
        summary: &AnalysisSummary,
        options: &slimenrf_recording::replay::ReplayOptions,
    ) -> bool {
        match self {
            Self::Gyro => summary.has_gyro,
            Self::Accel | Self::AccelDeviation => summary.has_accel,
            Self::Heading => summary.has_9d,
            Self::Mag | Self::MagNorm | Self::MagDip | Self::MagEarth | Self::MagCorrection => {
                summary.has_mag
            }
            Self::MagDist => summary.has_mag && options.params.mag_dist_rejection_enabled,
            Self::Rest => options.params.rest_bias_est_enabled,
            Self::Temperature => summary.has_temperature,
            Self::ThermalCompensation | Self::ThermalResidual => summary.has_thermal,
            _ => true,
        }
    }
    fn values(
        self,
        f: &slimenrf_recording::replay::ReplayFrame,
        nine: bool,
        summary: &AnalysisSummary,
    ) -> [f32; 4] {
        let triple = |v: [f32; 3], scale: f32| [v[0] * scale, v[1] * scale, v[2] * scale, f32::NAN];
        let triple64 = |v: Option<[f64; 3]>| {
            v.map_or([f32::NAN; 4], |v| {
                [v[0] as f32, v[1] as f32, v[2] as f32, f32::NAN]
            })
        };
        let scalar = |v: f32| [v, f32::NAN, f32::NAN, f32::NAN];
        let degrees = 180. / std::f32::consts::PI;
        match self {
            Self::Gyro => {
                if f.gyro_known {
                    triple(f.gyro_rad_s, degrees)
                } else {
                    [f32::NAN; 4]
                }
            }
            Self::Accel => {
                if f.accel_observed {
                    triple(f.accel_m_s2, 1. / 9.80665)
                } else {
                    [f32::NAN; 4]
                }
            }
            Self::Mag => {
                if f.mag_observed {
                    triple(f.magnetic_gauss, 1.)
                } else {
                    [f32::NAN; 4]
                }
            }
            Self::Euler => triple64(Some(euler_deg(if nine && f.heading9_valid {
                f.q9
            } else {
                f.q6
            }))),
            Self::Heading => {
                let yaw6 = euler_deg(f.q6)[2];
                let yaw9 = if f.heading9_valid {
                    euler_deg(f.q9)[2]
                } else {
                    f64::NAN
                };
                [
                    yaw6 as f32,
                    yaw9 as f32,
                    wrap_degrees(yaw9 - yaw6) as f32,
                    f32::NAN,
                ]
            }
            Self::Orientation => {
                if nine && f.heading9_valid {
                    f.q9
                } else {
                    f.q6
                }
            }
            Self::Bias => triple(f.bias_rad_s, degrees),
            Self::Rest => scalar(f.rest as u8 as f32),
            Self::MagDist => scalar(if f.heading9_valid {
                f.mag_disturbed as u8 as f32
            } else {
                f32::NAN
            }),
            Self::Temperature => scalar(f.temperature_c.map(f32::abs).unwrap_or(f32::NAN)),
            Self::MagCorrection => scalar(if f.heading9_valid {
                wrap_degrees(f.delta_rad as f64 * 180. / std::f64::consts::PI) as f32
            } else {
                f32::NAN
            }),
            Self::ThermalCompensation => f
                .thermal_correction_rad_s
                .map_or([f32::NAN; 4], |v| triple(v, degrees)),
            Self::ThermalResidual => f.thermal_correction_rad_s.map_or([f32::NAN; 4], |v| {
                triple(
                    [
                        f.bias_rad_s[0] - v[0],
                        f.bias_rad_s[1] - v[1],
                        f.bias_rad_s[2] - v[2],
                    ],
                    degrees,
                )
            }),
            Self::AccelDeviation => scalar(accel_norm_deviation_g(f).unwrap_or(f64::NAN) as f32),
            Self::MagEarth => triple64(magnetic_diagnostics(f).map(|v| v.2)),
            Self::MagNorm => [
                magnetic_diagnostics(f).map_or(f32::NAN, |v| v.0 as f32),
                if f.mag_observed {
                    summary
                        .magnetic_reference
                        .as_ref()
                        .map_or(f32::NAN, |r| r.norm_gauss as f32)
                } else {
                    f32::NAN
                },
                f32::NAN,
                f32::NAN,
            ],
            Self::MagDip => [
                magnetic_diagnostics(f).map_or(f32::NAN, |v| v.1 as f32),
                if f.mag_observed {
                    summary
                        .magnetic_reference
                        .as_ref()
                        .map_or(f32::NAN, |r| r.inclination_deg as f32)
                } else {
                    f32::NAN
                },
                f32::NAN,
                f32::NAN,
            ],
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct PlotPoint {
    time: f64,
    value: f32,
    segment: usize,
    break_before: bool,
    observed: bool,
}
pub(super) struct PlotData {
    pub series: Vec<Vec<PlotPoint>>,
    pub min: f32,
    pub max: f32,
    pub dense_boundaries: bool,
    pub finite_values: usize,
    robust_domain: (f32, f32),
    clipped_values: usize,
    chart: ChartId,
    cache_id: u64,
}
impl PlotData {
    pub(super) fn domain(&self, full: bool) -> (f32, f32) {
        if self.chart.state() {
            (-0.05, 1.05)
        } else if full || !self.chart.robust() {
            padded_domain(self.min, self.max)
        } else {
            self.robust_domain
        }
    }
    pub(super) fn clipped(&self, full: bool) -> usize {
        if full { 0 } else { self.clipped_values }
    }
    pub(super) fn y_ticks(&self, full: bool) -> [f32; 5] {
        let (low, high) = self.domain(full);
        std::array::from_fn(|i| high - (high - low) * i as f32 / 4.)
    }
}
pub(super) struct ReplayViewData {
    pub result: Arc<slimenrf_recording::replay::ReplayResult>,
    pub summary: Arc<AnalysisSummary>,
    pub range: (f64, f64),
    pub plots: Vec<Arc<PlotData>>,
    pub orientation9: Arc<PlotData>,
    euler9: Arc<PlotData>,
}
impl ReplayViewData {
    pub fn prepare_range(
        result: slimenrf_recording::replay::ReplayResult,
        range: Option<(f64, f64)>,
        cancel: &AtomicBool,
    ) -> Result<Self, String> {
        let range = range.unwrap_or((0., result.frames.last().map_or(0., |f| f.time_s)));
        let summary = Arc::new(AnalysisSummary::new(&result, cancel)?);
        Self::for_range(Arc::new(result), summary, range, cancel)
    }
    pub fn for_range(
        result: Arc<slimenrf_recording::replay::ReplayResult>,
        summary: Arc<AnalysisSummary>,
        range: (f64, f64),
        cancel: &AtomicBool,
    ) -> Result<Self, String> {
        let duration = result.frames.last().map_or(0., |f| f.time_s);
        let range = clamp_range(range, duration, result.timestep_s);
        let start = result.frames.partition_point(|f| f.time_s < range.0);
        let end = result
            .frames
            .partition_point(|f| f.time_s <= range.1)
            .max(start);
        let frames = &result.frames[start..end];
        let mut plots = Vec::with_capacity(ChartId::ALL.len());
        for chart in ChartId::ALL {
            plots.push(Arc::new(decimate(
                if chart.available(&summary, &result.options) {
                    frames
                } else {
                    &[]
                },
                chart,
                false,
                &summary,
                cancel,
            )?));
        }
        let orientation9 = if summary.has_9d {
            Arc::new(decimate(
                frames,
                ChartId::Orientation,
                true,
                &summary,
                cancel,
            )?)
        } else {
            plots[ChartId::Orientation as usize].clone()
        };
        let euler9 = if summary.has_9d {
            Arc::new(decimate(frames, ChartId::Euler, true, &summary, cancel)?)
        } else {
            plots[ChartId::Euler as usize].clone()
        };
        Ok(Self {
            result,
            summary,
            range,
            plots,
            orientation9,
            euler9,
        })
    }
    pub fn available(&self, chart: ChartId) -> bool {
        chart.available(&self.summary, &self.result.options)
    }
    pub fn plot(&self, chart: ChartId, nine: bool) -> Arc<PlotData> {
        match (chart, nine) {
            (ChartId::Orientation, true) => self.orientation9.clone(),
            (ChartId::Euler, true) => self.euler9.clone(),
            _ => self.plots[chart as usize].clone(),
        }
    }
    pub fn values(&self, chart: ChartId, index: usize, nine: bool) -> [Option<f32>; 4] {
        self.result.frames.get(index).map_or([None; 4], |f| {
            chart
                .values(f, nine, &self.summary)
                .map(|v| v.is_finite().then_some(v))
        })
    }
    /// Original time samples only; midpoint ties choose the later observation.
    pub fn nearest_index(&self, time: f64) -> Option<usize> {
        let frames = &self.result.frames;
        if frames.is_empty() || !time.is_finite() {
            return None;
        }
        let after = frames
            .partition_point(|f| f.time_s < time)
            .min(frames.len() - 1);
        Some(
            if after > 0 && time - frames[after - 1].time_s < frames[after].time_s - time {
                after - 1
            } else {
                after
            },
        )
    }
}

fn clamp_range(range: (f64, f64), duration: f64, timestep: f64) -> (f64, f64) {
    if duration <= 0. || !duration.is_finite() {
        return (0., 0.);
    }
    if !range.0.is_finite() || !range.1.is_finite() || range.1 <= range.0 {
        return (0., duration);
    }
    let minimum = if timestep.is_finite() && timestep > 0. {
        timestep.min(duration)
    } else {
        duration.min(1e-6)
    };
    let width = (range.1 - range.0).clamp(minimum, duration);
    let start = range.0.clamp(0., duration - width);
    (start, start + width)
}

fn padded_domain(min: f32, max: f32) -> (f32, f32) {
    if !min.is_finite() || !max.is_finite() {
        return (-1., 1.);
    }
    let pad = ((max - min) * 0.1)
        .max(min.abs().max(max.abs()) * 0.001)
        .max(1e-6);
    (min - pad, max + pad)
}
#[derive(Clone, Copy)]
struct Run {
    first: PlotPoint,
    low: PlotPoint,
    high: PlotPoint,
    last: PlotPoint,
}
impl Run {
    fn new(point: PlotPoint) -> Self {
        Self {
            first: point,
            low: point,
            high: point,
            last: point,
        }
    }
    fn push(&mut self, point: PlotPoint) {
        if point.value < self.low.value {
            self.low = point;
        }
        if point.value > self.high.value {
            self.high = point;
        }
        self.last = point;
    }
    fn emit(self, points: &mut Vec<PlotPoint>) {
        let mut selected = [self.first, self.low, self.high, self.last];
        selected.sort_unstable_by(|a, b| a.time.total_cmp(&b.time));
        let mut last_time = None;
        for mut point in selected {
            if last_time == Some(point.time) {
                continue;
            }
            point.break_before = last_time.is_none() && self.first.break_before;
            last_time = Some(point.time);
            points.push(point);
        }
    }
}

// Worker-only full-window passes; output <=512*4*4 points/channel. Missing
// sparse observations connect only within an uninterrupted received segment;
// reconstructed intervals, reset boundaries and wrapped angles remain explicit.
fn decimate(
    frames: &[slimenrf_recording::replay::ReplayFrame],
    chart: ChartId,
    nine: bool,
    summary: &AnalysisSummary,
    cancel: &AtomicBool,
) -> Result<PlotData, String> {
    let mut series: Vec<Vec<PlotPoint>> = (0..chart.count())
        .map(|_| Vec::with_capacity(2048))
        .collect();
    let mut samples: [Vec<f32>; 4] = std::array::from_fn(|_| Vec::with_capacity(MAX_QUANTILES));
    let mut counts = [0usize; 4];
    let sample_stride = frames.len().div_ceil(MAX_QUANTILES).max(1);
    let stride = frames.len().div_ceil(MAX_BINS).max(1);
    let mut previous: [Option<PlotPoint>; 4] = [None; 4];
    let (mut min, mut max) = (f32::INFINITY, f32::NEG_INFINITY);
    let mut dense_boundaries = false;
    for chunk in frames.chunks(stride) {
        let mut runs: [[Option<Run>; MAX_RUNS]; 4] = [[None; MAX_RUNS]; 4];
        let mut run_count = [0usize; 4];
        let mut lows = [f32::INFINITY; 4];
        let mut highs = [f32::NEG_INFINITY; 4];
        let mut all_observed = [true; 4];
        for (offset, f) in chunk.iter().enumerate() {
            if offset % 128 == 0 && cancel.load(Ordering::Relaxed) {
                return Err(tr("replay.notice.cancelled"));
            }
            let values = chart.values(f, nine, summary);
            for channel in 0..chart.count() {
                let value = values[channel];
                if !value.is_finite() {
                    if !chart.sparse()
                        || !f.observed
                        || previous[channel].is_some_and(|p| p.segment != f.segment)
                    {
                        previous[channel] = None;
                    }
                    continue;
                }
                if counts[channel] % sample_stride == 0 && samples[channel].len() < MAX_QUANTILES {
                    samples[channel].push(value);
                }
                counts[channel] += 1;
                min = min.min(value);
                max = max.max(value);
                lows[channel] = lows[channel].min(value);
                highs[channel] = highs[channel].max(value);
                all_observed[channel] &= f.observed;
                let before = previous[channel];
                let broken = before.is_none_or(|p| {
                    p.segment != f.segment || (chart.wrapped() && (value - p.value).abs() > 180.)
                });
                let new_run = run_count[channel] == 0
                    || broken
                    || before.is_some_and(|p| {
                        p.observed != f.observed || (chart.state() && p.value != value)
                    });
                let point = PlotPoint {
                    time: f.time_s,
                    value,
                    segment: f.segment,
                    break_before: broken,
                    observed: f.observed,
                };
                if new_run {
                    run_count[channel] += 1;
                    if run_count[channel] <= MAX_RUNS {
                        runs[channel][run_count[channel] - 1] = Some(Run::new(point));
                    }
                } else if run_count[channel] <= MAX_RUNS
                    && let Some(run) = &mut runs[channel][run_count[channel] - 1]
                {
                    run.push(point);
                }
                previous[channel] = Some(point);
            }
        }
        for channel in 0..chart.count() {
            if run_count[channel] > MAX_RUNS {
                dense_boundaries = true;
                let time = (chunk[0].time_s + chunk[chunk.len() - 1].time_s) * 0.5;
                for (value, break_before) in [(lows[channel], true), (highs[channel], false)] {
                    series[channel].push(PlotPoint {
                        time,
                        value,
                        segment: usize::MAX,
                        break_before,
                        observed: all_observed[channel],
                    });
                }
                previous[channel] = None;
            } else {
                for run in runs[channel].iter().flatten() {
                    run.emit(&mut series[channel]);
                }
            }
        }
    }
    if cancel.load(Ordering::Relaxed) {
        return Err(tr("replay.notice.cancelled"));
    }
    let (mut robust_min, mut robust_max) = (f32::INFINITY, f32::NEG_INFINITY);
    for sample in &mut samples[..chart.count()] {
        if sample.is_empty() {
            continue;
        }
        sample.sort_unstable_by(f32::total_cmp);
        let last = sample.len() - 1;
        robust_min = robust_min.min(sample[last / 100]);
        robust_max = robust_max.max(sample[(last * 99).div_ceil(100)]);
    }
    let robust_domain = if chart.robust() {
        padded_domain(robust_min, robust_max)
    } else {
        padded_domain(min, max)
    };
    let mut clipped_values = 0;
    if chart.robust() && (min < robust_domain.0 || max > robust_domain.1) {
        for (index, frame) in frames.iter().enumerate() {
            if index % 128 == 0 && cancel.load(Ordering::Relaxed) {
                return Err(tr("replay.notice.cancelled"));
            }
            for value in chart
                .values(frame, nine, summary)
                .iter()
                .take(chart.count())
            {
                if value.is_finite() && (*value < robust_domain.0 || *value > robust_domain.1) {
                    clipped_values += 1;
                }
            }
        }
    }
    if !min.is_finite() {
        min = -1.;
        max = 1.;
    }
    Ok(PlotData {
        series,
        min,
        max,
        dense_boundaries,
        finite_values: counts.iter().sum(),
        robust_domain,
        clipped_values,
        chart,
        cache_id: NEXT_PLOT_CACHE_ID.fetch_add(1, Ordering::Relaxed),
    })
}

fn stroke(
    window: &mut Window,
    points: impl IntoIterator<Item = Point<Pixels>>,
    color: Hsla,
    width: f32,
) {
    let mut points = points.into_iter();
    let Some(first) = points.next() else {
        return;
    };
    let mut path = PathBuilder::stroke(px(width));
    path.move_to(first);
    for point in points {
        path.line_to(point);
    }
    if let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
}
fn time_at(x: Pixels, bounds: Bounds<Pixels>, range: (f64, f64)) -> f64 {
    range.0
        + (range.1 - range.0)
            * (f32::from(x - bounds.left()) / f32::from(bounds.size.width).max(1.)).clamp(0., 1.)
                as f64
}
fn register_plot_scrub(
    owner: WeakEntity<ReplayFeature>,
    chart: Option<ChartId>,
    hitbox: Hitbox,
    bounds: Bounds<Pixels>,
    range: (f64, f64),
    window: &mut Window,
) {
    let down_owner = owner.clone();
    let down_hitbox = hitbox.clone();
    window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
        if phase.bubble() && event.button == MouseButton::Left && down_hitbox.is_hovered(window) {
            if event.click_count == 2 {
                let _ = down_owner.update(cx, |this, cx| {
                    if let Some(focus) = &this.state.focus {
                        window.focus(focus, cx);
                    }
                    this.reset_replay_plot(cx);
                });
                cx.stop_propagation();
                return;
            }
            let started = down_owner
                .update(cx, |this, cx| {
                    if let Some(focus) = &this.state.focus {
                        window.focus(focus, cx);
                    }
                    this.begin_replay_scrub(
                        event.position.x,
                        bounds,
                        range,
                        down_hitbox.id,
                        event.modifiers.shift,
                        cx,
                    )
                })
                .unwrap_or(false);
            if started {
                window.capture_pointer(down_hitbox.id);
                cx.stop_propagation();
            }
        }
    });
    let move_owner = owner.clone();
    let move_hitbox = hitbox.clone();
    window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
        if phase.capture()
            && move_owner
                .update(cx, |this, cx| this.continue_replay_scrub(event, window, cx))
                .unwrap_or(false)
        {
            cx.stop_propagation();
        } else if phase.bubble()
            && let Some(chart) = chart
        {
            let time = move_hitbox
                .is_hovered(window)
                .then(|| time_at(event.position.x, bounds, range));
            let _ = move_owner.update(cx, |this, cx| this.replay_plot_hover_chart(chart, time, cx));
        }
    });
    let up_owner = owner.clone();
    window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
        if phase.capture()
            && up_owner
                .update(cx, |this, cx| this.finish_replay_scrub(event, window, cx))
                .unwrap_or(false)
        {
            cx.stop_propagation();
        }
    });
    let wheel_owner = owner.clone();
    window.on_mouse_event(move |event: &ScrollEvent, phase, window, cx| {
        // Ordinary wheel scrolls the chart stack; Ctrl+wheel zooms shared time.
        if phase.bubble() && event.modifiers.control && hitbox.is_hovered(window) {
            let delta = f32::from(event.delta.pixel_delta(px(24.)).y);
            if delta.is_finite() {
                let _ = wheel_owner.update(cx, |this, cx| {
                    this.replay_plot_wheel(time_at(event.position.x, bounds, range), delta, cx)
                });
                cx.stop_propagation();
            }
        }
    });
    window.on_key_event(move |event: &KeyDownEvent, phase, window, cx| {
        if phase.capture()
            && event.keystroke.key == "escape"
            && owner
                .update(cx, |this, cx| this.cancel_replay_scrub(window, cx))
                .unwrap_or(false)
        {
            cx.stop_propagation();
        }
    });
}

// Clip before tessellation, not just at paint time: robust ranges can otherwise
// turn finite outliers into enormous offscreen path coordinates.
fn clipped_segment(
    a: Point<Pixels>,
    b: Point<Pixels>,
    width: f32,
    height: f32,
) -> Option<(Point<Pixels>, Point<Pixels>)> {
    let (x, y) = (f32::from(a.x) as f64, f32::from(a.y) as f64);
    let (dx, dy) = (f32::from(b.x) as f64 - x, f32::from(b.y) as f64 - y);
    let (mut start, mut end) = (0.0f64, 1.0f64);
    for (p, q) in [
        (-dx, x),
        (dx, width as f64 - x),
        (-dy, y),
        (dy, height as f64 - y),
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
    let at = |t| point(px((x + dx * t) as f32), px((y + dy * t) as f32));
    Some((at(start), at(end)))
}

fn signal_path(
    points: &[PlotPoint],
    range: (f64, f64),
    domain: (f32, f32),
    width: f32,
    height: f32,
    reconstructed: bool,
    stepped: bool,
) -> Option<Path<Pixels>> {
    let map = |point: &PlotPoint| {
        gpui::point(
            px(((point.time - range.0) / (range.1 - range.0).max(1e-9)) as f32 * width),
            px(height - (point.value - domain.0) / (domain.1 - domain.0) * height),
        )
    };
    let mut path = PathBuilder::stroke(px(1.5));
    if reconstructed {
        path = path.dash_array(&[px(4.), px(3.)]);
    }
    let mut previous: Option<&PlotPoint> = None;
    for point in points {
        if let Some(before) =
            previous.filter(|before| before.segment == point.segment && !point.break_before)
        {
            if (before.observed && point.observed) != reconstructed {
                let start = map(before);
                let end = map(point);
                let corner = gpui::point(end.x, start.y);
                for (a, b) in [(start, if stepped { corner } else { end }), (corner, end)]
                    .into_iter()
                    .take(if stepped { 2 } else { 1 })
                {
                    if let Some((a, b)) = clipped_segment(a, b, width, height) {
                        path.move_to(a);
                        path.line_to(b);
                    }
                }
            }
        } else if point.observed != reconstructed {
            let at = map(point);
            if let Some((a, b)) = clipped_segment(
                gpui::point(at.x - px(1.), at.y),
                gpui::point(at.x + px(1.), at.y),
                width,
                height,
            ) {
                path.move_to(a);
                path.line_to(b);
            }
        }
        previous = Some(point);
    }
    path.build().ok()
}
fn clipping_path(
    points: &[PlotPoint],
    range: (f64, f64),
    domain: (f32, f32),
    width: f32,
    height: f32,
) -> Option<Path<Pixels>> {
    let mut path = PathBuilder::stroke(px(1.));
    for point in points {
        if (domain.0..=domain.1).contains(&point.value) {
            continue;
        }
        let x = px(((point.time - range.0) / (range.1 - range.0).max(1e-9)) as f32 * width);
        let (y, inside) = if point.value > domain.1 {
            (px(1.), px(4.))
        } else {
            (px(height - 1.), px(height - 4.))
        };
        path.move_to(gpui::point(x - px(2.), inside));
        path.line_to(gpui::point(x, y));
        path.line_to(gpui::point(x + px(2.), inside));
    }
    path.build().ok()
}

#[derive(Clone, Copy, Default)]
pub(super) struct PlotOverlay {
    pub cursor: f64,
    pub a: Option<f64>,
    pub b: Option<f64>,
    pub hover: Option<f64>,
    pub selection: Option<(f64, f64)>,
}
fn paint_overlay(
    overlay: PlotOverlay,
    bounds: Bounds<Pixels>,
    range: (f64, f64),
    window: &mut Window,
    dark: bool,
) {
    let x_at = |time: f64| {
        bounds.left()
            + px(((time - range.0) / (range.1 - range.0).max(1e-9)) as f32
                * f32::from(bounds.size.width))
    };
    if let Some((a, b)) = overlay.selection {
        let left = x_at(a.min(b).clamp(range.0, range.1));
        let right = x_at(a.max(b).clamp(range.0, range.1));
        let mut fill: Hsla = rgb(0x558fe0).into();
        fill.a = 0.13;
        window.paint_quad(gpui_kit::fill(
            Bounds::new(
                point(left, bounds.top()),
                size(right - left, bounds.size.height),
            ),
            fill,
        ));
    }
    for (time, color, width, dashed) in [
        (overlay.a, reference_color(0, dark), 1.5, true),
        (overlay.b, reference_color(1, dark), 1.5, true),
        (
            overlay.hover,
            rgb(if dark { 0x9a9a9a } else { 0x646464 }).into(),
            1.,
            true,
        ),
        (Some(overlay.cursor), direction_color(3, dark), 2., false),
    ] {
        if let Some(time) = time.filter(|t| (range.0..=range.1).contains(t)) {
            let x = x_at(time);
            let mut path = PathBuilder::stroke(px(width));
            if dashed {
                path = path.dash_array(&[px(4.), px(3.)]);
            }
            path.move_to(point(x, bounds.top()));
            path.line_to(point(x, bounds.bottom()));
            if let Ok(path) = path.build() {
                window.paint_path(path, color);
            }
        }
    }
}
fn visible(bounds: Bounds<Pixels>, window: &Window) -> bool {
    let clip = window.content_mask().bounds;
    bounds.right() > clip.left()
        && bounds.left() < clip.right()
        && bounds.bottom() > clip.top()
        && bounds.top() < clip.bottom()
}
fn tick_label(value: f64) -> String {
    if value != 0. && (value.abs() < 0.001 || value.abs() >= 100000.) {
        format!("{value:.1e}")
    } else if value.abs() >= 100. {
        format!("{value:.0}")
    } else if value.abs() >= 1. {
        format!("{value:.2}")
    } else {
        format!("{value:.3}")
    }
}
pub(super) fn plot_canvas(
    chart: ChartId,
    data: Arc<PlotData>,
    range: (f64, f64),
    overlay: PlotOverlay,
    full: bool,
    owner: WeakEntity<ReplayFeature>,
    cx: &App,
) -> AnyElement {
    let border = cx.theme().border;
    let dark = cx.theme().is_dark();
    let ticks = data.y_ticks(full);
    let height = if chart.state() { 82. } else { 142. };
    let signal = canvas(
        move |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
        move |bounds, hitbox, window, cx| {
            if !visible(bounds, window) {
                return;
            }
            register_plot_scrub(owner.clone(), Some(chart), hitbox, bounds, range, window);
            let width = f32::from(bounds.size.width);
            let height = f32::from(bounds.size.height);
            let domain = data.domain(full);
            let key = ShapeKey::new((
                data.cache_id,
                width.to_bits(),
                height.to_bits(),
                range.0.to_bits(),
                range.1.to_bits(),
                full,
            ))
            .finish();
            let caches = PathCaches::for_paint("replay-signal-paths", window, cx);
            for fraction in [0., 0.25, 0.5, 0.75, 1.] {
                let y = bounds.top() + bounds.size.height * fraction;
                stroke(
                    window,
                    [point(bounds.left(), y), point(bounds.right(), y)],
                    border,
                    1.,
                );
            }
            window.with_content_mask(Some(ContentMask { bounds }), |window| {
                caches.update(cx, |caches, _| {
                    for (channel, points) in data.series.iter().enumerate() {
                        for reconstructed in [false, true] {
                            let slot = channel * 3 + usize::from(reconstructed);
                            if let Some(path) = caches.slot(slot).get(key, bounds.origin, || {
                                signal_path(
                                    points,
                                    range,
                                    domain,
                                    width,
                                    height,
                                    reconstructed,
                                    chart.state(),
                                )
                            }) {
                                let mut color = chart.color(channel, dark);
                                if reconstructed {
                                    color.a *= 0.65;
                                }
                                window.paint_path(path, color);
                            }
                        }
                        if data.clipped(full) > 0
                            && let Some(path) =
                                caches.slot(channel * 3 + 2).get(key, bounds.origin, || {
                                    clipping_path(points, range, domain, width, height)
                                })
                        {
                            window.paint_path(path, chart.color(channel, dark));
                        }
                    }
                });
                paint_overlay(overlay, bounds, range, window, dark);
            });
        },
    )
    .w_full()
    .h(px(height));
    div()
        .id(("replay-chart-canvas", chart as usize))
        .w_full()
        .flex()
        .flex_col()
        .child(
            div()
                .flex()
                .w_full()
                .child(
                    div()
                        .w(px(54.))
                        .h(px(height))
                        .flex_shrink_0()
                        .flex()
                        .flex_col()
                        .justify_between()
                        .text_xs()
                        .children(
                            ticks
                                .into_iter()
                                .map(|tick| div().child(tick_label(tick as f64))),
                        ),
                )
                .child(
                    div()
                        .relative()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .child(signal)
                        .children(
                            [
                                (overlay.a, "A", reference_color(0, dark)),
                                (overlay.b, "B", reference_color(1, dark)),
                            ]
                            .into_iter()
                            .enumerate()
                            .filter_map(
                                |(index, (time, label, color))| {
                                    time.filter(|t| (range.0..=range.1).contains(t))
                                        .map(|time| {
                                            let fraction = ((time - range.0)
                                                / (range.1 - range.0).max(1e-9))
                                                as f32;
                                            div()
                                                .absolute()
                                                .top(px(index as f32 * 14.))
                                                .when(fraction <= 0.5, |label| {
                                                    label.left(relative(fraction))
                                                })
                                                .when(fraction > 0.5, |label| {
                                                    label.right(relative(1. - fraction))
                                                })
                                                .text_xs()
                                                .text_color(color)
                                                .child(label)
                                        })
                                },
                            ),
                        ),
                ),
        )
        .child(
            div()
                .ml(px(54.))
                .flex()
                .justify_between()
                .text_xs()
                .children((0..5).map(|i| {
                    div().child(format!(
                        "{} s",
                        tick_label(range.0 + (range.1 - range.0) * i as f64 / 4.)
                    ))
                })),
        )
        .into_any_element()
}
pub(super) fn timeline_canvas(
    range: (f64, f64),
    overlay: PlotOverlay,
    owner: WeakEntity<ReplayFeature>,
    border: Hsla,
    dark: bool,
) -> AnyElement {
    canvas(
        move |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
        move |bounds, hitbox, window, _cx| {
            if !visible(bounds, window) {
                return;
            }
            register_plot_scrub(owner.clone(), None, hitbox, bounds, range, window);
            let y = bounds.top() + bounds.size.height * 0.5;
            stroke(
                window,
                [point(bounds.left(), y), point(bounds.right(), y)],
                border,
                2.,
            );
            paint_overlay(overlay, bounds, range, window, dark);
        },
    )
    .w_full()
    .h(px(24.))
    .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::{ChartId, MAX_BINS, MAX_RUNS, ReplayViewData, decimate};
    use slimenrf_recording::capture::TrackIdentity;
    use slimenrf_recording::replay::analysis::AnalysisSummary;
    use slimenrf_recording::replay::{
        CalibrationAvailability, ReplayFrame, ReplayOptions, ReplayResult,
    };
    use std::sync::{Arc, atomic::AtomicBool};

    fn frame(index: usize) -> ReplayFrame {
        ReplayFrame {
            time_s: index as f64 / 100.,
            segment: index / 5000,
            observed: true,
            gyro_known: true,
            accel_observed: true,
            mag_observed: false,
            heading9_valid: true,
            gyro_rad_s: [0.; 3],
            accel_m_s2: [0., 0., 9.80665],
            magnetic_gauss: [0.; 3],
            temperature_c: None,
            q6: [1., 0., 0., 0.],
            q9: [0., 0., 0., 1.],
            bias_rad_s: [0.; 3],
            rest: false,
            mag_disturbed: false,
            delta_rad: 0.,
            mag_norm: 0.,
            mag_ref_norm: 0.,
            mag_dip_rad: 0.,
            mag_ref_dip_rad: 0.,
            k_mag_init: 0.,
            thermal_correction_rad_s: None,
        }
    }
    fn result(frames: Vec<ReplayFrame>) -> ReplayResult {
        ReplayResult {
            frames,
            warnings: Vec::new(),
            segments: Vec::new(),
            profile: String::new(),
            timestep_s: 0.01,
            options: ReplayOptions::default(),
            calibration_available: CalibrationAvailability::default(),
            identity: TrackIdentity::default(),
        }
    }
    fn plot(frames: Vec<ReplayFrame>, chart: ChartId) -> super::PlotData {
        let result = result(frames);
        let cancel = AtomicBool::new(false);
        let summary = AnalysisSummary::new(&result, &cancel).unwrap();
        decimate(&result.frames, chart, false, &summary, &cancel).unwrap()
    }
    #[test]
    fn extrema_and_geometry_remain_bounded() {
        let mut frames: Vec<_> = (0..20000).map(frame).collect();
        for (i, f) in frames.iter_mut().enumerate() {
            f.segment = i;
            f.gyro_rad_s[0] = (i % 100) as f32;
        }
        let data = plot(frames, ChartId::Gyro);
        assert!(data.dense_boundaries);
        assert!(
            data.series
                .iter()
                .all(|s| s.len() <= MAX_BINS * MAX_RUNS * 4)
        );
        assert_eq!(data.min, 0.);
        assert!((data.max - 99.0f32.to_degrees()).abs() < 0.001);
        assert!(
            data.series[0]
                .as_chunks::<2>()
                .0
                .iter()
                .all(|p| p[0].break_before && p[0].time == p[1].time)
        );
    }
    #[test]
    fn robust_range_counts_original_outliers_without_losing_extrema() {
        let mut frames: Vec<_> = (0..10000).map(frame).collect();
        frames[1234].gyro_rad_s[0] = 100.;
        frames[1235].gyro_rad_s[0] = -50.;
        let data = plot(frames, ChartId::Gyro);
        assert_eq!(data.clipped(false), 2);
        assert_eq!(data.clipped(true), 0);
        assert!(data.domain(false).1 < 1.);
        assert!(data.domain(true).1 > 100.);
        assert!(data.series[0].iter().any(|p| p.value == data.max));
        assert!(data.series[0].iter().any(|p| p.value == data.min));
    }
    #[test]
    fn sparse_values_connect_only_across_received_rows() {
        let mut frames: Vec<_> = (0..5).map(frame).collect();
        frames[1].accel_observed = false;
        frames[3].accel_observed = false;
        frames[3].observed = false;
        let data = plot(frames, ChartId::Accel);
        assert_eq!(data.series[0].len(), 3);
        assert!(!data.series[0][1].break_before);
        assert!(data.series[0][2].break_before);
    }
    #[test]
    fn wrap_edges_are_not_connected() {
        let mut frames: Vec<_> = (0..2).map(frame).collect();
        for (f, degrees) in frames.iter_mut().zip([179.0f32, -179.]) {
            let half = degrees.to_radians() / 2.;
            f.q6 = [half.cos(), 0., 0., half.sin()];
        }
        let data = plot(frames, ChartId::Euler);
        assert!(data.series[2][1].break_before);
    }
    #[test]
    fn viewport_reuses_summary_and_original_hover_values() {
        let initial = ReplayViewData::prepare_range(
            result((0..10000).map(frame).collect()),
            None,
            &AtomicBool::new(false),
        )
        .unwrap();
        let zoom = ReplayViewData::for_range(
            initial.result.clone(),
            initial.summary.clone(),
            (12., 12.2),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(Arc::ptr_eq(&initial.result, &zoom.result));
        assert!(Arc::ptr_eq(&initial.summary, &zoom.summary));
        assert_eq!(zoom.nearest_index(12.004), Some(1200));
        assert!(
            zoom.plot(ChartId::Gyro, false).series[0]
                .iter()
                .all(|p| (12.0..=12.2).contains(&p.time))
        );
        assert_eq!(zoom.values(ChartId::Temperature, 1200, false), [None; 4]);
        assert_eq!(zoom.values(ChartId::Accel, 1200, false)[2], Some(1.));
    }
    #[test]
    fn cancellation_is_checked_even_for_empty_plots() {
        let result = result(vec![frame(0)]);
        let summary = AnalysisSummary::new(&result, &AtomicBool::new(false)).unwrap();
        assert!(decimate(&[], ChartId::Gyro, false, &summary, &AtomicBool::new(true)).is_err());
    }
    #[test]
    fn chart_inventory_and_disabled_detector_availability_are_truthful() {
        assert_eq!(ChartId::ALL.len(), 17);
        let mut result = result(vec![frame(0)]);
        result.frames[0].mag_observed = true;
        result.frames[0].magnetic_gauss = [0., 0., 1.];
        result.options.params.mag_dist_rejection_enabled = false;
        result.options.params.rest_bias_est_enabled = false;
        let view = ReplayViewData::prepare_range(result, None, &AtomicBool::new(false)).unwrap();
        assert!(!view.available(ChartId::MagDist));
        assert!(!view.available(ChartId::Rest));
        assert!(view.available(ChartId::MagNorm));
        assert_eq!(view.values(ChartId::MagNorm, 0, false)[0], Some(1.));
        assert_eq!(ChartId::Gyro.unit(), "°/s");
        assert_eq!(ChartId::Accel.unit(), "g");
    }
    #[test]
    fn state_changes_and_unknown_sensor_values_are_not_fabricated() {
        let mut frames: Vec<_> = (0..10000).map(frame).collect();
        frames[5].rest = true;
        frames[6].gyro_known = false;
        let view =
            ReplayViewData::prepare_range(result(frames), None, &AtomicBool::new(false)).unwrap();
        let data = view.plot(ChartId::Rest, false);
        assert!(
            data.series[0]
                .iter()
                .any(|p| p.time == 0.05 && p.value == 1.)
        );
        assert!(
            data.series[0]
                .iter()
                .any(|p| p.time == 0.06 && p.value == 0.)
        );
        assert_eq!(view.values(ChartId::Gyro, 6, false), [None; 4]);
    }
    #[test]
    fn viewport_clamps_after_rate_changes_and_extreme_geometry_is_clipped() {
        assert_eq!(super::clamp_range((90., 100.), 50., 0.01), (40., 50.));
        assert_eq!(super::clamp_range((0., 100.), 0., 0.01), (0., 0.));
        let a = gpui_kit::point(gpui_kit::px(10.), gpui_kit::px(-1e9));
        let b = gpui_kit::point(gpui_kit::px(20.), gpui_kit::px(1e9));
        let (a, b) = super::clipped_segment(a, b, 100., 100.).unwrap();
        assert!(f32::from(a.y).abs() < 0.01);
        assert!((f32::from(b.y) - 100.).abs() < 0.01);
    }
}
