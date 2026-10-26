use super::{ThermalDump, ThermalDumpPoint};

const INTERIOR_SAMPLES: usize = 100;
const EXTRAPOLATION_SAMPLES: usize = 50;
const EXTRAPOLATION_C: f64 = 6.0;
const BOUNDARY_C: f64 = 4.0;
const MLS_SCALE_C: f64 = 2.5;
const MLS_MIN_WEIGHT: f64 = 0.03;
const MLS_POINTS: usize = 10;
const MLS_SINGULAR: f64 = 1e-10;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AxisStatistics {
    pub linear_slope: Option<f64>,
    /// Sum of squared residuals at the raw input temperatures, not RMSE.
    pub linear_residual_sum_squares: Option<f64>,
    pub mls_residual_sum_squares: Option<f64>,
    /// Minimum and maximum finite adjacent slopes after stable temperature sort.
    /// Equal-temperature pairs are excluded rather than divided by zero.
    pub adjacent_slope_range: Option<[f64; 2]>,
    pub bias_range: f64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AxisAnalysis {
    pub raw: Vec<[f64; 2]>,
    pub linear: Vec<[f64; 2]>,
    pub mls: Vec<[f64; 2]>,
    pub extrapolation_low: Vec<[f64; 2]>,
    pub extrapolation_high: Vec<[f64; 2]>,
    pub statistics: AxisStatistics,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ThermalDumpAnalysis {
    pub temperature_range_c: Option<[f64; 2]>,
    pub axes: [AxisAnalysis; 3],
}

/// Precompute the legacy console-plot analysis once, outside UI paint. This
/// intentionally does not use or alter the firmware-compatible replay model.
/// Empty input has no range/fits; one point has raw data only. Repeated
/// temperatures use a constant mean global fit and weighted-mean MLS fallback.
pub fn analyze(dump: &ThermalDump) -> ThermalDumpAnalysis {
    let Some(first) = dump.points.first() else {
        return ThermalDumpAnalysis::default();
    };
    let [minimum, maximum] = dump.points.iter().fold(
        [first.temperature_c, first.temperature_c],
        |[minimum, maximum], point| {
            [
                minimum.min(point.temperature_c),
                maximum.max(point.temperature_c),
            ]
        },
    );
    // Sorting references preserves the source document's slot/traversal order.
    let mut sorted: Vec<_> = dump.points.iter().collect();
    sorted.sort_by(|a, b| a.temperature_c.total_cmp(&b.temperature_c));
    ThermalDumpAnalysis {
        temperature_range_c: Some([minimum, maximum]),
        axes: std::array::from_fn(|axis| {
            analyze_axis(&dump.points, &sorted, axis, minimum, maximum)
        }),
    }
}

#[derive(Clone, Copy)]
struct LinearFit {
    x_scale: f64,
    y_scale: f64,
    x_mean: f64,
    y_mean: f64,
    normalized_slope: f64,
}

impl LinearFit {
    fn evaluate(self, temperature: f64) -> f64 {
        (self.normalized_slope * (temperature / self.x_scale - self.x_mean) + self.y_mean)
            * self.y_scale
    }

    fn slope(self) -> Option<f64> {
        finite((self.normalized_slope / self.x_scale) * self.y_scale)
    }
}

fn linear_fit<'a>(
    points: impl Iterator<Item = &'a ThermalDumpPoint> + Clone,
    axis: usize,
) -> LinearFit {
    let count = points.clone().count() as f64;
    let (x_scale, y_scale, min_temperature, max_temperature) = points.clone().fold(
        (1.0_f64, 1.0_f64, f64::INFINITY, f64::NEG_INFINITY),
        |(x, y, minimum, maximum), point| {
            (
                x.max(point.temperature_c.abs()),
                y.max(point.bias_dps[axis].abs()),
                minimum.min(point.temperature_c),
                maximum.max(point.temperature_c),
            )
        },
    );
    let (x_mean, y_mean) = points.clone().fold((0.0, 0.0), |(x, y), point| {
        (
            x + (point.temperature_c / x_scale) / count,
            y + (point.bias_dps[axis] / y_scale) / count,
        )
    });
    let (numerator, denominator) = points.fold((0.0, 0.0), |(numerator, denominator), point| {
        let x = point.temperature_c / x_scale - x_mean;
        let y = point.bias_dps[axis] / y_scale - y_mean;
        (numerator + x * y, denominator + x * x)
    });
    LinearFit {
        x_scale,
        y_scale,
        x_mean,
        y_mean,
        // Summed means can round away from an identical source temperature.
        // Detect that degenerate input explicitly, not from centered squares.
        normalized_slope: if min_temperature < max_temperature && denominator > 0.0 {
            numerator / denominator
        } else {
            0.0
        },
    }
}

fn boundary_fit(
    points: &[ThermalDumpPoint],
    axis: usize,
    boundary: f64,
    low: bool,
    global: LinearFit,
) -> LinearFit {
    let selected = points.iter().filter(|point| {
        let distance = if low {
            point.temperature_c - boundary
        } else {
            boundary - point.temperature_c
        };
        (0.0..=BOUNDARY_C).contains(&distance)
    });
    if selected.clone().count() < 2 {
        global
    } else {
        linear_fit(selected, axis)
    }
}

fn analyze_axis(
    points: &[ThermalDumpPoint],
    sorted: &[&ThermalDumpPoint],
    axis: usize,
    minimum: f64,
    maximum: f64,
) -> AxisAnalysis {
    let mut result = AxisAnalysis {
        raw: points
            .iter()
            .map(|point| [point.temperature_c, point.bias_dps[axis]])
            .collect(),
        ..AxisAnalysis::default()
    };
    let (min_bias, max_bias) = points
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(min, max), point| {
            (min.min(point.bias_dps[axis]), max.max(point.bias_dps[axis]))
        });
    result.statistics.bias_range = max_bias - min_bias;
    if points.len() < 2 {
        return result;
    }
    let global = linear_fit(points.iter(), axis);
    result.statistics.linear_slope = global.slope();
    result.statistics.linear_residual_sum_squares =
        residual_sum(points, axis, |temperature| global.evaluate(temperature));
    result.statistics.mls_residual_sum_squares =
        residual_sum(points, axis, |temperature| mls(points, axis, temperature));
    let mut slope_range: Option<[f64; 2]> = None;
    for pair in sorted.windows(2) {
        let dt = pair[1].temperature_c - pair[0].temperature_c;
        if dt == 0.0 {
            continue;
        }
        if let Some(slope) = finite((pair[1].bias_dps[axis] - pair[0].bias_dps[axis]) / dt) {
            slope_range = Some(match slope_range {
                Some([min, max]) => [min.min(slope), max.max(slope)],
                None => [slope, slope],
            });
        }
    }
    result.statistics.adjacent_slope_range = slope_range;
    result.linear = sample(minimum, maximum, INTERIOR_SAMPLES, |temperature| {
        global.evaluate(temperature)
    });
    result.mls = sample(minimum, maximum, INTERIOR_SAMPLES, |temperature| {
        mls(points, axis, temperature)
    });
    let left = boundary_fit(points, axis, minimum, true, global);
    let right = boundary_fit(points, axis, maximum, false, global);
    result.extrapolation_low = sample(
        minimum - EXTRAPOLATION_C,
        minimum,
        EXTRAPOLATION_SAMPLES,
        |temperature| left.evaluate(temperature),
    );
    result.extrapolation_high = sample(
        maximum,
        maximum + EXTRAPOLATION_C,
        EXTRAPOLATION_SAMPLES,
        |temperature| right.evaluate(temperature),
    );
    result
}

fn sample(
    start: f64,
    end: f64,
    count: usize,
    mut evaluate: impl FnMut(f64) -> f64,
) -> Vec<[f64; 2]> {
    (0..count)
        .filter_map(|index| {
            let fraction = index as f64 / (count - 1) as f64;
            let temperature = if index == 0 {
                start
            } else if index + 1 == count {
                end
            } else {
                start * (1.0 - fraction) + end * fraction
            };
            let bias = evaluate(temperature);
            (temperature.is_finite() && bias.is_finite()).then_some([temperature, bias])
        })
        .collect()
}

fn finite(value: f64) -> Option<f64> {
    value.is_finite().then_some(value)
}

fn residual_sum(
    points: &[ThermalDumpPoint],
    axis: usize,
    mut evaluate: impl FnMut(f64) -> f64,
) -> Option<f64> {
    finite(
        points
            .iter()
            .map(|point| {
                let residual = evaluate(point.temperature_c) - point.bias_dps[axis];
                residual * residual
            })
            .sum(),
    )
}

// Fixed-size top-10 selection: no allocation/sort for each query, with stable
// source-order tie breaking matching Python's stable descending weight sort.
fn mls(points: &[ThermalDumpPoint], axis: usize, query: f64) -> f64 {
    let mut selected = [(0.0, 0_usize); MLS_POINTS];
    let mut count = 0;
    let mut nearest = (f64::INFINITY, 0);
    for (index, point) in points.iter().enumerate() {
        let distance = (point.temperature_c - query).abs();
        if distance < nearest.0 {
            nearest = (distance, index);
        }
        let weight = 1.0 / (1.0 + (distance / MLS_SCALE_C).powi(2));
        if weight < MLS_MIN_WEIGHT {
            continue;
        }
        let insertion = selected[..count]
            .iter()
            .position(|&(old, _)| weight > old)
            .unwrap_or(count);
        if insertion >= MLS_POINTS {
            continue;
        }
        let end = count.min(MLS_POINTS - 1);
        selected.copy_within(insertion..end, insertion + 1);
        selected[insertion] = (weight, index);
        count = (count + 1).min(MLS_POINTS);
    }
    if count == 0 {
        return points[nearest.1].bias_dps[axis];
    }
    if count == 1 {
        return points[selected[0].1].bias_dps[axis];
    }
    let mut sum_w = 0.0;
    let mut sum_wdt = 0.0;
    let mut sum_wdt2 = 0.0;
    let mut sum_wb = 0.0;
    let mut sum_wdtb = 0.0;
    // Scaling only bias preserves the legacy determinant and avoids overflow in
    // sums when finite console values have unusually large magnitude.
    let bias_scale = selected[..count]
        .iter()
        .fold(1.0_f64, |scale, &(_, index)| {
            scale.max(points[index].bias_dps[axis].abs())
        });
    for &(weight, index) in &selected[..count] {
        let point = &points[index];
        let dt = point.temperature_c - query;
        let bias = point.bias_dps[axis] / bias_scale;
        sum_w += weight;
        sum_wdt += weight * dt;
        sum_wdt2 += weight * dt * dt;
        sum_wb += weight * bias;
        sum_wdtb += weight * dt * bias;
    }
    let determinant = sum_w * sum_wdt2 - sum_wdt * sum_wdt;
    if determinant.abs() < MLS_SINGULAR {
        (sum_wb / sum_w) * bias_scale
    } else {
        ((sum_wb * sum_wdt2 - sum_wdtb * sum_wdt) / determinant) * bias_scale
    }
}

#[cfg(test)]
pub(super) fn evaluate_mls_for_test(points: &[ThermalDumpPoint], axis: usize, query: f64) -> f64 {
    mls(points, axis, query)
}
