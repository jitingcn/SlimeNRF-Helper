//! Worker-cached availability and allocation-free, original-frame diagnostics.
//! Heading differences are wrapped endpoint comparisons, never ground-truth errors.
use super::{ReplayFrame, ReplayResult};
use std::sync::atomic::{AtomicBool, Ordering};

const GRAVITY: f64 = 9.80665;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReferencePair {
    pub a: usize,
    pub b: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutoReferenceError {
    InvalidTiming,
    TooShort,
    IndependentSegments,
    NoInitialRest,
    NoFinalRest,
    SameRestRun,
    NotOrdered,
    Cancelled,
}
impl std::fmt::Display for AutoReferenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidTiming => "Valid sample timing is required.",
            Self::TooShort => "Recording must be at least 60 s long.",
            Self::IndependentSegments => "Auto A/B requires one continuous replay segment.",
            Self::NoInitialRest => "No rest run contains 5 s within the first minute.",
            Self::NoFinalRest => "No rest run lasts at least 10 s.",
            Self::SameRestRun => "A and B need two distinct rest runs.",
            Self::NotOrdered => "The last 10 s rest run does not place B after A.",
            Self::Cancelled => "Analysis cancelled.",
        })
    }
}
impl std::error::Error for AutoReferenceError {}

/// Exact web policy: maximal post-update VQF rest runs, exclusive ends,
/// first qualifying clipped A run and last qualifying full-recording B run.
/// Rounding ties go later. No fallback search or physical same-pose assumption.
/// O(N), O(1) storage; call only on the retained cancellable worker.
pub fn select_rest_references(
    result: &ReplayResult,
    cancel: &AtomicBool,
) -> Result<ReferencePair, AutoReferenceError> {
    if cancel.load(Ordering::Relaxed) {
        return Err(AutoReferenceError::Cancelled);
    }
    let n = result.frames.len();
    let dt = result.timestep_s;
    if n == 0 || !dt.is_finite() || dt <= 0. {
        return Err(AutoReferenceError::InvalidTiming);
    }
    if (n as f64) * dt < 60. {
        return Err(AutoReferenceError::TooShort);
    }
    if result.segments.len() != 1 || result.segments[0].start != 0 || result.segments[0].end != n {
        return Err(AutoReferenceError::IndependentSegments);
    }
    let mut a = None;
    let mut b = None;
    let mut start = None;
    // An extra sentinel closes a run ending at the recording boundary.
    for i in 0..=n {
        if i % 256 == 0 && cancel.load(Ordering::Relaxed) {
            return Err(AutoReferenceError::Cancelled);
        }
        if i < n && result.frames[i].segment != 0 {
            return Err(AutoReferenceError::IndependentSegments);
        }
        if i < n && result.frames[i].rest {
            start.get_or_insert(i);
        } else if let Some(s) = start.take() {
            let a_end = ((i as f64) * dt).min(60.);
            if a.is_none() && a_end - (s as f64) * dt >= 5. {
                // Retain the original run identity, not its clipped interval.
                a = Some(((((a_end - 5.) / dt).round() as usize).clamp(s, i - 1), s));
            }
            if ((i - s) as f64) * dt >= 10. {
                b = Some((((i as f64 - 10. / dt).round() as usize).clamp(s, i - 1), s));
            }
        }
    }
    if cancel.load(Ordering::Relaxed) {
        return Err(AutoReferenceError::Cancelled);
    }
    let (a, run_a) = a.ok_or(AutoReferenceError::NoInitialRest)?;
    let (b, run_b) = b.ok_or(AutoReferenceError::NoFinalRest)?;
    if run_a == run_b {
        return Err(AutoReferenceError::SameRestRun);
    }
    if b <= a {
        return Err(AutoReferenceError::NotOrdered);
    }
    Ok(ReferencePair { a, b })
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MagneticReference {
    /// Mean of the first 1000 qualified magnetic observations, not VQF state.
    pub norm_gauss: f64,
    pub inclination_deg: f64,
    pub observations: usize,
}

#[derive(Clone, Debug)]
pub struct AnalysisSummary {
    pub auto_references: Result<ReferencePair, AutoReferenceError>,
    pub has_gyro: bool,
    pub has_accel: bool,
    pub has_mag: bool,
    pub has_9d: bool,
    /// Matches web visibility: finite nonzero temperature within first 1000 rows.
    pub has_temperature: bool,
    pub has_thermal: bool,
    pub magnetic_reference: Option<MagneticReference>,
}
impl AnalysisSummary {
    /// Scan once per replay publication, never on paint or viewport changes.
    pub fn new(result: &ReplayResult, cancel: &AtomicBool) -> Result<Self, String> {
        let auto_references = select_rest_references(result, cancel);
        if auto_references == Err(AutoReferenceError::Cancelled) {
            return Err(AutoReferenceError::Cancelled.to_string());
        }
        let mut summary = Self {
            auto_references,
            has_gyro: false,
            has_accel: false,
            has_mag: false,
            has_9d: false,
            has_temperature: false,
            has_thermal: false,
            magnetic_reference: None,
        };
        let mut norm_sum = 0.;
        let mut inclination_sum = 0.;
        let mut observations = 0;
        for (i, frame) in result.frames.iter().enumerate() {
            if i % 256 == 0 && cancel.load(Ordering::Relaxed) {
                return Err(AutoReferenceError::Cancelled.to_string());
            }
            summary.has_gyro |= frame.gyro_known && finite_vector(frame.gyro_rad_s);
            summary.has_accel |= acceleration_body(frame).is_some();
            summary.has_9d |= frame.heading9_valid && valid_quaternion(frame.q9);
            summary.has_temperature |= i < 1000 && temperature(frame).is_some();
            summary.has_thermal |= thermal_correction(frame).is_some();
            if let Some(mag) = magnetic_body(frame) {
                summary.has_mag = true;
                if observations < 1000
                    && let Some((norm, inclination, _)) = magnetic_derived(frame.q6, mag)
                        .filter(|(_, inclination, _)| inclination.is_finite())
                {
                    norm_sum += norm;
                    inclination_sum += inclination;
                    observations += 1;
                }
            }
        }
        if cancel.load(Ordering::Relaxed) {
            return Err(AutoReferenceError::Cancelled.to_string());
        }
        if observations > 0 {
            summary.magnetic_reference = Some(MagneticReference {
                norm_gauss: norm_sum / observations as f64,
                inclination_deg: inclination_sum / observations as f64,
                observations,
            });
        }
        Ok(summary)
    }
}

fn finite_vector(v: [f32; 3]) -> bool {
    v.into_iter().all(f32::is_finite)
}
fn valid_quaternion(q: [f32; 4]) -> bool {
    q.into_iter().all(f32::is_finite) && q != [0.; 4]
}
fn magnitude(v: [f64; 3]) -> f64 {
    (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
}
fn acceleration_body(frame: &ReplayFrame) -> Option<[f32; 3]> {
    (frame.observed && frame.accel_observed && finite_vector(frame.accel_m_s2))
        .then_some(frame.accel_m_s2)
}
/// Fresh finite magnetic input in the device/body frame, in gauss.
pub fn magnetic_body(frame: &ReplayFrame) -> Option<[f32; 3]> {
    (frame.observed && frame.mag_observed && finite_vector(frame.magnetic_gauss))
        .then_some(frame.magnetic_gauss)
}
fn linear_body_g(frame: &ReplayFrame) -> Option<[f64; 3]> {
    let acc = acceleration_body(frame)?;
    if !valid_quaternion(frame.q6) {
        return None;
    }
    let [w, x, y, z] = frame.q6.map(f64::from);
    let gravity = [
        2. * (x * z - w * y),
        2. * (y * z + w * x),
        1. - 2. * (x * x + y * y),
    ];
    Some(std::array::from_fn(|i| {
        f64::from(acc[i]) / GRAVITY - gravity[i]
    }))
}
/// Fresh measured acceleration minus 6D gravity, body frame, in m/s².
/// Valid zero linear acceleration means stationary, not missing sensor input.
pub fn linear_acceleration_body(frame: &ReplayFrame) -> Option<[f32; 3]> {
    linear_body_g(frame).map(|v| v.map(|x| (x * GRAVITY) as f32))
}
fn temperature(frame: &ReplayFrame) -> Option<f64> {
    frame
        .temperature_c
        .filter(|t| t.is_finite() && *t != 0.)
        .map(|t| f64::from(t).abs())
}
fn thermal_correction(frame: &ReplayFrame) -> Option<[f64; 3]> {
    frame
        .thermal_correction_rad_s
        .filter(|v| frame.gyro_known && finite_vector(*v))
        .map(|v| v.map(|x| f64::from(x).to_degrees()))
}

/// Hamilton wxyz, roll/pitch/yaw in degrees, following the reference equations.
pub fn euler_deg(q: [f32; 4]) -> [f64; 3] {
    let [w, x, y, z] = q.map(f64::from);
    [
        (2. * (w * x + y * z))
            .atan2(1. - 2. * (x * x + y * y))
            .to_degrees(),
        (2. * (w * y - z * x)).clamp(-1., 1.).asin().to_degrees(),
        (2. * (w * z + x * y))
            .atan2(1. - 2. * (y * y + z * z))
            .to_degrees(),
    ]
}
/// Wrap while preserving both +180° and -180° boundary signs, as in the web UI.
pub fn wrap_degrees(angle: f64) -> f64 {
    let mut wrapped = angle % 360.;
    if wrapped > 180. {
        wrapped -= 360.;
    } else if wrapped < -180. {
        wrapped += 360.;
    }
    wrapped
}
fn magnetic_derived(q: [f32; 4], mag: [f32; 3]) -> Option<(f64, f64, [f64; 3])> {
    if !valid_quaternion(q) {
        return None;
    }
    let [w, x, y, z] = q.map(f64::from);
    let [mx, my, mz] = mag.map(f64::from);
    let norm = magnitude([mx, my, mz]);
    let t0 = -x * mx - y * my - z * mz;
    let t1 = w * mx + y * mz - z * my;
    let t2 = w * my + z * mx - x * mz;
    let t3 = w * mz + x * my - y * mx;
    let earth = [
        -t0 * x + t1 * w - t2 * z + t3 * y,
        -t0 * y + t2 * w - t3 * x + t1 * z,
        -t0 * z + t3 * w - t1 * y + t2 * x,
    ];
    let inclination = if norm > 0. {
        (earth[2] / norm).clamp(-1., 1.).asin().to_degrees()
    } else {
        f64::NAN
    };
    Some((norm, inclination, earth))
}

#[derive(Clone, Copy, Debug)]
pub struct FrameDiagnostics {
    pub euler6_deg: [f64; 3],
    pub euler9_deg: Option<[f64; 3]>,
    pub heading_difference_deg: Option<f64>,
    pub bias_deg_s: [f64; 3],
    pub linear_accel_magnitude_g: Option<f64>,
    pub accel_norm_deviation_g: Option<f64>,
    pub magnetic_norm_gauss: Option<f64>,
    pub magnetic_inclination_deg: Option<f64>,
    /// Absolute held temperature, not an assertion of a fresh measurement.
    pub temperature_c: Option<f64>,
    pub thermal_correction_deg_s: Option<[f64; 3]>,
}
/// Qualified observed field norm (G), inclination (degrees), and 6D earth vector (G).
/// An observed calibrated zero retains norm/earth zero; its inclination is NaN.
pub fn magnetic_diagnostics(frame: &ReplayFrame) -> Option<(f64, f64, [f64; 3])> {
    magnetic_body(frame).and_then(|mag| magnetic_derived(frame.q6, mag))
}

/// Absolute acceleration magnitude deviation from 1 g, fresh observations only.
pub fn accel_norm_deviation_g(frame: &ReplayFrame) -> Option<f64> {
    acceleration_body(frame).map(|v| (magnitude(v.map(|x| f64::from(x) / GRAVITY)) - 1.).abs())
}

/// O(1), no allocations or borrowed-recording scans.
pub fn frame_diagnostics(frame: &ReplayFrame) -> FrameDiagnostics {
    let euler6_deg = euler_deg(frame.q6);
    let euler9_deg =
        (frame.heading9_valid && valid_quaternion(frame.q9)).then(|| euler_deg(frame.q9));
    let linear_accel_body_g = linear_body_g(frame);
    let derived_mag = magnetic_diagnostics(frame);
    let thermal_correction_deg_s = thermal_correction(frame);
    let bias_deg_s = frame.bias_rad_s.map(|x| f64::from(x).to_degrees());
    FrameDiagnostics {
        euler6_deg,
        euler9_deg,
        heading_difference_deg: euler9_deg.map(|e| wrap_degrees(e[2] - euler6_deg[2])),
        bias_deg_s,
        linear_accel_magnitude_g: linear_accel_body_g.map(magnitude),
        accel_norm_deviation_g: accel_norm_deviation_g(frame),
        magnetic_norm_gauss: derived_mag.map(|v| v.0),
        magnetic_inclination_deg: derived_mag.map(|v| v.1).filter(|v| v.is_finite()),
        temperature_c: temperature(frame),
        thermal_correction_deg_s,
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ReferenceEndpoint {
    pub time_s: f64,
    pub segment: usize,
    /// Wrapped heading relative to ~1 s into this segment, not accuracy.
    pub heading6_deg: Option<f64>,
    pub heading9_deg: Option<f64>,
}
#[derive(Clone, Copy, Debug)]
pub struct ReferenceInterval {
    pub duration_s: f64,
    /// False across filter resets; duration then describes only plotted time.
    pub continuous: bool,
    pub drift6_deg: Option<f64>,
    pub drift9_deg: Option<f64>,
    pub rate6_deg_min: Option<f64>,
    pub rate9_deg_min: Option<f64>,
}
#[derive(Clone, Copy, Debug, Default)]
pub struct ReferenceMetrics {
    pub a: Option<ReferenceEndpoint>,
    pub b: Option<ReferenceEndpoint>,
    pub interval: Option<ReferenceInterval>,
}
fn endpoint(result: &ReplayResult, index: usize) -> Option<ReferenceEndpoint> {
    let index = index.min(result.frames.len().checked_sub(1)?);
    let frame = &result.frames[index];
    let segment = result.segments.get(frame.segment)?;
    if segment.start > index
        || segment.end <= index
        || segment.end > result.frames.len()
        || !result.timestep_s.is_finite()
        || result.timestep_s <= 0.
    {
        return None;
    }
    let baseline_index = segment
        .start
        .saturating_add((1. / result.timestep_s).round() as usize)
        .min(segment.end - 1);
    let base = &result.frames[baseline_index];
    let heading6_deg = (valid_quaternion(frame.q6) && valid_quaternion(base.q6))
        .then(|| wrap_degrees(euler_deg(frame.q6)[2] - euler_deg(base.q6)[2]));
    let heading9_deg = (frame.heading9_valid
        && base.heading9_valid
        && valid_quaternion(frame.q9)
        && valid_quaternion(base.q9))
    .then(|| wrap_degrees(euler_deg(frame.q9)[2] - euler_deg(base.q9)[2]));
    Some(ReferenceEndpoint {
        time_s: frame.time_s,
        segment: frame.segment,
        heading6_deg,
        heading9_deg,
    })
}
/// O(1) endpoint differences. Reversed references preserve B−A sign and use
/// positive elapsed time; equal endpoints have no rate. Resets have no drift.
pub fn reference_metrics(
    result: &ReplayResult,
    a: Option<usize>,
    b: Option<usize>,
) -> ReferenceMetrics {
    let a = a.and_then(|i| endpoint(result, i));
    let b = b.and_then(|i| endpoint(result, i));
    let interval = a.zip(b).map(|(a, b)| {
        let continuous = a.segment == b.segment;
        let duration_s = (b.time_s - a.time_s).abs();
        let drift6_deg = if continuous {
            a.heading6_deg
                .zip(b.heading6_deg)
                .map(|(a, b)| wrap_degrees(b - a))
        } else {
            None
        };
        let drift9_deg = if continuous {
            a.heading9_deg
                .zip(b.heading9_deg)
                .map(|(a, b)| wrap_degrees(b - a))
        } else {
            None
        };
        let rate = |drift: Option<f64>| {
            drift
                .filter(|_| duration_s.is_finite() && duration_s > 0.)
                .map(|d| d * 60. / duration_s)
        };
        ReferenceInterval {
            duration_s,
            continuous,
            drift6_deg,
            drift9_deg,
            rate6_deg_min: rate(drift6_deg),
            rate9_deg_min: rate(drift9_deg),
        }
    });
    ReferenceMetrics { a, b, interval }
}

#[cfg(test)]
mod tests {
    use super::{
        AnalysisSummary, AutoReferenceError, ReferencePair, acceleration_body, euler_deg,
        frame_diagnostics, linear_acceleration_body, magnetic_body, magnetic_diagnostics,
        reference_metrics, select_rest_references, wrap_degrees,
    };
    use crate::replay::{ReplayFrame, ReplayOptions, ReplayResult, ReplaySegment};
    use std::sync::atomic::AtomicBool;

    fn frame() -> ReplayFrame {
        ReplayFrame {
            time_s: 0.,
            segment: 0,
            observed: true,
            gyro_known: true,
            accel_observed: false,
            mag_observed: false,
            heading9_valid: false,
            gyro_rad_s: [0.; 3],
            accel_m_s2: [0.; 3],
            magnetic_gauss: [0.; 3],
            temperature_c: None,
            q6: [1., 0., 0., 0.],
            q9: [1., 0., 0., 0.],
            bias_rad_s: [0.; 3],
            rest: false,
            mag_disturbed: false,
            delta_rad: 0.,
            mag_norm: 0.,
            mag_ref_norm: 0.,
            mag_dip_rad: 0.,
            mag_ref_dip_rad: 0.,
            bias_covariance: [0.; 3],
            k_mag_init: 0.,
            thermal_correction_rad_s: None,
        }
    }
    fn recording(n: usize, dt: f64, runs: &[(usize, usize)]) -> ReplayResult {
        let mut frames = vec![frame(); n];
        for (i, frame) in frames.iter_mut().enumerate() {
            frame.time_s = i as f64 * dt;
        }
        for &(s, e) in runs {
            for frame in &mut frames[s..e] {
                frame.rest = true;
            }
        }
        ReplayResult {
            frames,
            segments: if n == 0 {
                vec![]
            } else {
                vec![ReplaySegment { start: 0, end: n }]
            },
            warnings: vec![],
            profile: String::new(),
            timestep_s: dt,
            options: ReplayOptions::default(),
            calibration_available: Default::default(),
            identity: Default::default(),
        }
    }
    fn auto(result: &ReplayResult) -> Result<ReferencePair, AutoReferenceError> {
        select_rest_references(result, &AtomicBool::new(false))
    }
    fn yaw(degrees: f64) -> [f32; 4] {
        let angle = degrees.to_radians() / 2.;
        [angle.cos() as f32, 0., 0., angle.sin() as f32]
    }
    fn near(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-4, "{a} != {b}");
    }

    #[test]
    fn recorded_web_sensor25_rest_run_regression() {
        // Actual expanded rest intervals from web tests/rest-references.test.ts.
        // This fixes policy only, not cross-profile fusion-output equivalence.
        let result = recording(
            548322,
            1. / 473.7599792480469,
            &[
                (1328, 45061),
                (80496, 85866),
                (87232, 90970),
                (511418, 548322),
            ],
        );
        assert_eq!(
            auto(&result),
            Ok(ReferencePair {
                a: 26057,
                b: 543584
            })
        );
    }
    #[test]
    fn inclusive_duration_thresholds_and_exclusive_recording_end() {
        let result = recording(60, 1., &[(0, 5), (50, 60)]);
        assert_eq!(auto(&result), Ok(ReferencePair { a: 0, b: 50 }));
        assert_eq!(
            auto(&recording(59, 1., &[(0, 5), (49, 59)])),
            Err(AutoReferenceError::TooShort)
        );
        assert_eq!(
            auto(&recording(60, 1., &[(0, 4), (55, 60)])),
            Err(AutoReferenceError::NoFinalRest)
        );
        assert_eq!(
            auto(&recording(60, 1., &[(0, 5), (51, 60)])),
            Err(AutoReferenceError::NoFinalRest)
        );
    }
    #[test]
    fn clipping_and_rounding_match_web_without_splitting_runs() {
        assert_eq!(
            auto(&recording(100, 1., &[(55, 70), (80, 100)])),
            Ok(ReferencePair { a: 55, b: 90 })
        );
        assert_eq!(
            auto(&recording(100, 1., &[(56, 70), (80, 100)])),
            Err(AutoReferenceError::NoInitialRest)
        );
        assert_eq!(
            auto(&recording(120, 1., &[(0, 120)])),
            Err(AutoReferenceError::SameRestRun)
        );
        // A end−2.5 samples rounds up, B end−5.0 does not need rounding.
        assert_eq!(
            auto(&recording(40, 2., &[(0, 4), (30, 40)])),
            Ok(ReferencePair { a: 2, b: 35 })
        );
        // B end−1.25 rounds up; A end−0.625 also rounds to the end,
        // which must be clamped to the last actual sample of its run.
        assert_eq!(
            auto(&recording(10, 8., &[(0, 1), (8, 10)])),
            Ok(ReferencePair { a: 0, b: 9 })
        );
        assert_eq!(
            auto(&recording(6, 10., &[(0, 1), (5, 6)])),
            Ok(ReferencePair { a: 0, b: 5 })
        );
        assert_eq!(
            auto(&recording(20, 4., &[(0, 2), (16, 20)])),
            Ok(ReferencePair { a: 1, b: 18 })
        );
    }
    #[test]
    fn selected_same_run_does_not_trigger_fallback_search() {
        assert_eq!(
            auto(&recording(120, 1., &[(0, 20), (40, 46)])),
            Err(AutoReferenceError::SameRestRun)
        );
        assert_eq!(
            auto(&recording(120, 1., &[])),
            Err(AutoReferenceError::NoInitialRest)
        );
    }
    #[test]
    fn invalid_timing_cancel_and_independent_segments() {
        for dt in [0., -1., f64::NAN, f64::INFINITY] {
            assert_eq!(
                auto(&recording(60, dt, &[])),
                Err(AutoReferenceError::InvalidTiming)
            );
        }
        assert_eq!(
            auto(&recording(0, 1., &[])),
            Err(AutoReferenceError::InvalidTiming)
        );
        let mut result = recording(60, 1., &[(0, 5), (50, 60)]);
        assert_eq!(
            select_rest_references(&result, &AtomicBool::new(true)),
            Err(AutoReferenceError::Cancelled)
        );
        assert!(AnalysisSummary::new(&result, &AtomicBool::new(true)).is_err());
        result.segments = vec![
            ReplaySegment { start: 0, end: 30 },
            ReplaySegment { start: 30, end: 60 },
        ];
        assert_eq!(auto(&result), Err(AutoReferenceError::IndependentSegments));
    }
    #[test]
    fn euler_and_wrapped_boundary_signs_match_reference() {
        near(euler_deg(yaw(90.))[2], 90.);
        assert_eq!(wrap_degrees(180.), 180.);
        assert_eq!(wrap_degrees(-180.), -180.);
        assert_eq!(wrap_degrees(540.), 180.);
        assert_eq!(wrap_degrees(-540.), -180.);
        assert_eq!(wrap_degrees(358.), -2.);
        assert!(wrap_degrees(f64::INFINITY).is_nan());
    }
    #[test]
    fn references_use_one_second_baseline_signed_wrapped_endpoints_and_rates() {
        let mut result = recording(6, 0.5, &[]);
        for (i, frame) in result.frames.iter_mut().enumerate() {
            frame.q6 = yaw([0., 50., 170., 179., -179., -170.][i]);
            frame.q9 = yaw([0., 0., -170., -179., 179., 160.][i]);
            frame.heading9_valid = true;
        }
        let metrics = reference_metrics(&result, Some(3), Some(4));
        let a = metrics.a.unwrap();
        near(a.heading6_deg.unwrap(), 9.);
        near(a.heading9_deg.unwrap(), -9.);
        let interval = metrics.interval.unwrap();
        near(interval.drift6_deg.unwrap(), 2.);
        near(interval.drift9_deg.unwrap(), -2.);
        near(interval.rate6_deg_min.unwrap(), 240.);
        let reversed = reference_metrics(&result, Some(4), Some(3))
            .interval
            .unwrap();
        near(reversed.duration_s, 0.5);
        near(reversed.drift6_deg.unwrap(), -2.);
        near(reversed.drift9_deg.unwrap(), 2.);
        let equal = reference_metrics(&result, Some(3), Some(3))
            .interval
            .unwrap();
        near(equal.drift6_deg.unwrap(), 0.);
        assert!(equal.rate6_deg_min.is_none());
        near(
            reference_metrics(&result, Some(usize::MAX), None)
                .a
                .unwrap()
                .time_s,
            2.5,
        );
        assert!(reference_metrics(&result, None, None).interval.is_none());
        result.frames[2].heading9_valid = false;
        assert!(
            reference_metrics(&result, Some(3), Some(4))
                .interval
                .unwrap()
                .drift9_deg
                .is_none()
        );
    }
    #[test]
    fn cross_reset_references_have_local_baselines_but_no_continuous_drift() {
        let mut result = recording(8, 1., &[]);
        result.segments = vec![
            ReplaySegment { start: 0, end: 4 },
            ReplaySegment { start: 4, end: 8 },
        ];
        for frame in &mut result.frames[4..] {
            frame.segment = 1;
            frame.q6 = yaw(90.);
        }
        result.frames[1].q6 = yaw(10.);
        result.frames[5].q6 = yaw(70.);
        let metrics = reference_metrics(&result, Some(2), Some(6));
        near(metrics.a.unwrap().heading6_deg.unwrap(), -10.);
        near(metrics.b.unwrap().heading6_deg.unwrap(), 20.);
        let interval = metrics.interval.unwrap();
        assert!(!interval.continuous);
        assert!(interval.drift6_deg.is_none() && interval.rate6_deg_min.is_none());
        assert!(interval.drift9_deg.is_none() && interval.rate9_deg_min.is_none());
        assert!(
            reference_metrics(&recording(0, 1., &[]), Some(0), Some(1))
                .a
                .is_none()
        );
        near(
            reference_metrics(&recording(1, 0.001, &[]), Some(0), None)
                .a
                .unwrap()
                .heading6_deg
                .unwrap(),
            0.,
        );
    }
    #[test]
    fn diagnostics_qualify_freshness_and_identity_heading() {
        let mut frame = frame();
        let absent = frame_diagnostics(&frame);
        assert!(acceleration_body(&frame).is_none() && absent.magnetic_norm_gauss.is_none());
        assert!(absent.euler9_deg.is_none());
        frame.accel_m_s2 = [0., 0., 9.80665];
        frame.magnetic_gauss = [1., 0., 0.];
        assert!(magnetic_body(&frame).is_none());
        assert!(linear_acceleration_body(&frame).is_none());
        frame.accel_observed = true;
        frame.mag_observed = true;
        frame.heading9_valid = true;
        let actual = frame_diagnostics(&frame);
        near(actual.linear_accel_magnitude_g.unwrap(), 0.);
        near(actual.magnetic_norm_gauss.unwrap(), 1.);
        assert_eq!(actual.euler9_deg, Some([0.; 3]));
        frame.observed = false;
        assert!(acceleration_body(&frame).is_none());
        assert!(magnetic_body(&frame).is_none());
    }
    #[test]
    fn vectors_use_six_dimensional_body_to_earth_rotation() {
        let mut frame = frame();
        frame.q6 = yaw(90.);
        frame.q9 = yaw(-90.);
        frame.mag_observed = true;
        frame.magnetic_gauss = [1., 0., 1.];
        frame.accel_observed = true;
        frame.accel_m_s2 = [9.80665, 0., 9.80665];
        let diagnostics = frame_diagnostics(&frame);
        let earth = magnetic_diagnostics(&frame).unwrap().2;
        near(earth[0], 0.);
        near(earth[1], 1.);
        near(earth[2], 1.);
        near(diagnostics.magnetic_inclination_deg.unwrap(), 45.);
        let linear = linear_acceleration_body(&frame).unwrap();
        near(f64::from(linear[0]), 9.80665);
        near(f64::from(linear[1]), 0.);
        near(f64::from(linear[2]), 0.);
        frame.magnetic_gauss[0] = f32::NAN;
        assert!(frame_diagnostics(&frame).magnetic_norm_gauss.is_none());
        frame.accel_m_s2 = [0.; 3];
        near(
            frame_diagnostics(&frame).linear_accel_magnitude_g.unwrap(),
            1.,
        );
        frame.accel_observed = false;
        assert!(frame_diagnostics(&frame).linear_accel_magnitude_g.is_none());
    }
    #[test]
    fn observed_calibrated_zero_vectors_remain_real_measurements() {
        let mut result = recording(1, 1., &[]);
        let frame = &mut result.frames[0];
        frame.mag_observed = true;
        frame.accel_observed = true;
        let diagnostics = frame_diagnostics(frame);
        assert_eq!(acceleration_body(frame), Some([0.; 3]));
        assert_eq!(magnetic_body(frame), Some([0.; 3]));
        assert_eq!(magnetic_diagnostics(frame).unwrap().2, [0.; 3]);
        assert_eq!(diagnostics.magnetic_norm_gauss, Some(0.));
        assert!(diagnostics.magnetic_inclination_deg.is_none());
        near(diagnostics.accel_norm_deviation_g.unwrap(), 1.);
        let summary = AnalysisSummary::new(&result, &AtomicBool::new(false)).unwrap();
        assert!(summary.has_accel && summary.has_mag);
        assert!(summary.magnetic_reference.is_none());
    }
    #[test]
    fn cached_reference_counts_first_thousand_observations_not_rows() {
        let mut result = recording(3000, 1., &[]);
        for (i, frame) in result.frames.iter_mut().enumerate() {
            if i % 2 == 0 {
                frame.mag_observed = true;
                frame.heading9_valid = true;
                frame.magnetic_gauss = if i < 2000 {
                    [1., 0., 0.]
                } else {
                    [100., 0., 0.]
                };
            }
        }
        // Held/nonfresh nonzero vectors must not count toward the mean.
        result.frames[1].magnetic_gauss = [999., 0., 0.];
        result.frames[1000].temperature_c = Some(25.);
        let summary = AnalysisSummary::new(&result, &AtomicBool::new(false)).unwrap();
        assert!(summary.has_mag && summary.has_9d);
        assert!(!summary.has_temperature);
        let reference = summary.magnetic_reference.unwrap();
        assert_eq!(reference.observations, 1000);
        near(reference.norm_gauss, 1.);
        near(reference.inclination_deg, 0.);
        result.frames[999].temperature_c = Some(-25.);
        assert!(
            AnalysisSummary::new(&result, &AtomicBool::new(false))
                .unwrap()
                .has_temperature
        );
    }
    #[test]
    fn thermal_diagnostics_use_actual_applied_correction_not_an_alternate_fit() {
        let mut frame = frame();
        frame.bias_rad_s = [0.2, 0.3, 0.4];
        frame.thermal_correction_rad_s = Some([0.1, 0.2, 0.3]);
        frame.temperature_c = Some(-27.);
        let diagnostics = frame_diagnostics(&frame);
        near(diagnostics.temperature_c.unwrap(), 27.);
        near(
            diagnostics.thermal_correction_deg_s.unwrap()[0],
            0.1_f64.to_degrees(),
        );
        frame.gyro_known = false;
        assert!(frame_diagnostics(&frame).thermal_correction_deg_s.is_none());
    }
    #[test]
    fn absent_and_short_magnetic_reference_use_only_actual_observations() {
        let mut result = recording(8, 1., &[]);
        let missing = AnalysisSummary::new(&result, &AtomicBool::new(false)).unwrap();
        assert!(!missing.has_mag && !missing.has_9d && !missing.has_thermal);
        assert!(missing.magnetic_reference.is_none());
        result.frames[1].magnetic_gauss = [1., 0., 0.];
        result.frames[1].mag_observed = true;
        result.frames[3].magnetic_gauss = [0., 0., 3.];
        result.frames[3].mag_observed = true;
        result.frames[4].magnetic_gauss = [f32::INFINITY, 0., 0.];
        result.frames[4].mag_observed = true;
        let reference = AnalysisSummary::new(&result, &AtomicBool::new(false))
            .unwrap()
            .magnetic_reference
            .unwrap();
        assert_eq!(reference.observations, 2);
        near(reference.norm_gauss, 2.);
        near(reference.inclination_deg, 45.);
    }

    #[test]
    fn tilted_gravity_is_removed_in_body_not_earth_frame() {
        let mut frame = frame();
        let half = std::f32::consts::FRAC_1_SQRT_2;
        frame.q6 = [half, half, 0., 0.];
        frame.accel_observed = true;
        frame.accel_m_s2 = [0., 9.80665, 0.];
        let linear = linear_acceleration_body(&frame).unwrap();
        for value in linear {
            near(f64::from(value), 0.);
        }
    }
}
