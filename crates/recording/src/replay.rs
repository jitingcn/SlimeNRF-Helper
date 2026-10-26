//! Cold-start replay of reconstructible raw observations, not a reconstruction of retained live state.
pub mod analysis;
mod calibration;
#[allow(unsafe_code)]
mod filter;
pub mod presets;
#[cfg(test)]
mod tests;

use crate::capture::{CalibrationSnapshot, Gyro, Recording, Sample, TrackIdentity};
use calibration::{Calibration, affine};
use filter::Filter;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

pub const PROFILE: &str = "Tracker 8aa61a6 / vqf-c 3c1a6ae · firmware profile at recorded event rate · cold start · scalar CMSIS mixed precision";
const MAX_FRAMES: usize = 4_000_000;

#[derive(Clone, Debug, PartialEq)]
pub struct ReplayParams {
    pub tau_acc: f32,
    pub tau_mag: f32,
    pub bias_clip: f32,
    pub bias_sigma_init: f32,
    pub bias_forgetting_time: f32,
    pub bias_sigma_motion: f32,
    pub bias_sigma_rest: f32,
    pub rest_min_t: f32,
    pub rest_filter_tau: f32,
    pub rest_th_gyr: f32,
    pub rest_th_acc: f32,
    pub motion_bias_est_enabled: bool,
    pub rest_bias_est_enabled: bool,
    pub mag_dist_rejection_enabled: bool,
    pub bias_vertical_forgetting_factor: f32,
    pub mag_current_tau: f32,
    pub mag_norm_th: f32,
    pub mag_dip_th: f32,
    pub mag_ref_tau: f32,
    pub mag_new_time: f32,
    pub mag_new_first_time: f32,
    pub mag_new_min_gyr: f32,
    pub mag_min_undisturbed_time: f32,
    pub mag_max_rejection_time: f32,
    pub mag_rejection_factor: f32,
}
impl Default for ReplayParams {
    fn default() -> Self {
        Self {
            tau_acc: 1.011,
            tau_mag: 9.,
            bias_clip: 5.,
            bias_sigma_init: 1.55,
            bias_forgetting_time: 325.,
            bias_sigma_motion: 0.1053,
            bias_sigma_rest: 0.0108,
            rest_min_t: 0.25,
            rest_filter_tau: 2.6,
            rest_th_gyr: 0.4,
            rest_th_acc: 0.1,
            motion_bias_est_enabled: true,
            rest_bias_est_enabled: true,
            mag_dist_rejection_enabled: true,
            bias_vertical_forgetting_factor: 0.0001,
            mag_current_tau: 0.20,
            mag_norm_th: 0.10,
            mag_dip_th: 5.,
            mag_ref_tau: 10.,
            mag_new_time: 15.,
            mag_new_first_time: 5.,
            mag_new_min_gyr: 10.,
            mag_min_undisturbed_time: 0.5,
            mag_max_rejection_time: 60.,
            mag_rejection_factor: 1000.,
        }
    }
}
impl ReplayParams {
    // Flat bridge ABI: keep this ordering paired with replay_bridge.c.
    fn values(&self) -> [f32; 25] {
        [
            self.tau_acc,
            self.tau_mag,
            self.bias_clip,
            self.bias_sigma_init,
            self.bias_forgetting_time,
            self.bias_sigma_motion,
            self.bias_sigma_rest,
            self.rest_min_t,
            self.rest_filter_tau,
            self.rest_th_gyr,
            self.rest_th_acc,
            self.bias_vertical_forgetting_factor,
            self.motion_bias_est_enabled as u8 as f32,
            self.rest_bias_est_enabled as u8 as f32,
            self.mag_dist_rejection_enabled as u8 as f32,
            self.mag_current_tau,
            self.mag_norm_th,
            self.mag_dip_th,
            self.mag_ref_tau,
            self.mag_new_time,
            self.mag_new_first_time,
            self.mag_new_min_gyr,
            self.mag_min_undisturbed_time,
            self.mag_max_rejection_time,
            self.mag_rejection_factor,
        ]
    }
    pub fn validate(&self) -> Result<(), String> {
        // Keep coefficient/covariance arithmetic inside a supported numerical domain.
        let bounds = [
            ("tauAcc", self.tau_acc, 0.01, 1000.),
            ("tauMag", self.tau_mag, 0.01, 100_000.),
            ("biasClip", self.bias_clip, 0.001, 100.),
            ("biasSigmaInit", self.bias_sigma_init, 0.001, 100.),
            (
                "biasForgettingTime",
                self.bias_forgetting_time,
                0.01,
                100_000.,
            ),
            ("biasSigmaMotion", self.bias_sigma_motion, 0.0001, 100.),
            ("biasSigmaRest", self.bias_sigma_rest, 0.0001, 100.),
            ("restMinT", self.rest_min_t, 0.001, 10_000.),
            ("restFilterTau", self.rest_filter_tau, 0.01, 1000.),
            ("restThGyr", self.rest_th_gyr, 0.0001, 100.),
            ("restThAcc", self.rest_th_acc, 0.0001, 100.),
            (
                "biasVerticalForgettingFactor",
                self.bias_vertical_forgetting_factor,
                1e-9,
                1.,
            ),
            ("magNormTh", self.mag_norm_th, 0., 100.),
            ("magDipTh", self.mag_dip_th, 0., 180.),
            ("magRefTau", self.mag_ref_tau, 0.001, 100_000.),
            ("magNewTime", self.mag_new_time, 0., 100_000.),
            ("magNewFirstTime", self.mag_new_first_time, 0., 100_000.),
            ("magNewMinGyr", self.mag_new_min_gyr, 0., 10_000.),
            (
                "magMinUndisturbedTime",
                self.mag_min_undisturbed_time,
                0.,
                100_000.,
            ),
            (
                "magMaxRejectionTime",
                self.mag_max_rejection_time,
                0.,
                100_000.,
            ),
            (
                "magRejectionFactor",
                self.mag_rejection_factor,
                1.,
                100_000.,
            ),
        ];
        for (name, value, min, max) in bounds {
            if !value.is_finite() || value < min || value > max {
                return Err(format!(
                    "VQF parameter {name} must be finite and between {min} and {max}"
                ));
            }
        }
        if self.mag_current_tau != -1.
            && (!self.mag_current_tau.is_finite()
                || !(0.001..=1000.).contains(&self.mag_current_tau))
        {
            return Err(
                "VQF parameter magCurrentTau must be -1 (disabled) or between 0.001 and 1000"
                    .into(),
            );
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GyroCalibration {
    None,
    Bias,
    #[default]
    Thermal,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CalibrationAvailability {
    pub accel: bool,
    pub mag: bool,
    pub gyro_bias: bool,
    pub gyro_sensitivity: bool,
    pub gyro_thermal: bool,
}

/// Snapshot eligibility only; ReplayResult additionally qualifies thermal availability
/// with usable temperature data from the selected recording.
pub fn calibration_availability(snapshot: &CalibrationSnapshot) -> CalibrationAvailability {
    CalibrationAvailability {
        accel: snapshot.accel.is_some(),
        mag: snapshot.mag.is_some(),
        gyro_bias: snapshot.gyro_bias_dps.is_some(),
        gyro_sensitivity: snapshot.gyro_sensitivity.is_some(),
        gyro_thermal: calibration::thermal_eligible(snapshot),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReplayOptions {
    pub calibrate_accel: bool,
    pub calibrate_mag: bool,
    pub gyro_calibration: GyroCalibration,
    pub use_magnetometer: bool,
    pub raw_rate_hz: Option<f32>,
    pub gyro_rate_hz: Option<f32>,
    pub accel_rate_hz: Option<f32>,
    pub mag_rate_hz: Option<f32>,
    pub params: ReplayParams,
}
impl Default for ReplayOptions {
    fn default() -> Self {
        Self {
            calibrate_accel: true,
            calibrate_mag: true,
            gyro_calibration: GyroCalibration::Thermal,
            use_magnetometer: true,
            raw_rate_hz: None,
            gyro_rate_hz: None,
            accel_rate_hz: None,
            mag_rate_hz: None,
            params: ReplayParams::default(),
        }
    }
}
#[derive(Clone, Debug)]
pub struct ReplayFrame {
    pub time_s: f64,
    pub segment: usize,
    pub observed: bool,
    /// Known or explicitly reconstructed gyro interval, not an invented segment-start zero.
    pub gyro_known: bool,
    pub accel_observed: bool,
    pub mag_observed: bool,
    /// A nonzero calibrated magnetic update has occurred in this segment.
    pub heading9_valid: bool,
    /// Actual thermal bias (including provenance-qualified legacy offset), after sensitivity.
    /// None when static fallback, calibration disabled, or gyro interval unknown.
    pub thermal_correction_rad_s: Option<[f32; 3]>,
    pub gyro_rad_s: [f32; 3],
    pub accel_m_s2: [f32; 3],
    pub magnetic_gauss: [f32; 3],
    /// Last finite temperature held within this segment; not necessarily a fresh observation.
    pub temperature_c: Option<f32>,
    pub q6: [f32; 4],
    pub q9: [f32; 4],
    pub bias_rad_s: [f32; 3],
    pub rest: bool,
    pub mag_disturbed: bool,
    pub delta_rad: f32,
    pub mag_norm: f32,
    pub mag_ref_norm: f32,
    pub mag_dip_rad: f32,
    pub mag_ref_dip_rad: f32,
    #[cfg(test)]
    pub bias_covariance: [f32; 3],
    pub k_mag_init: f32,
}
#[derive(Clone, Debug)]
pub struct ReplaySegment {
    pub start: usize,
    pub end: usize,
}
#[derive(Debug)]
pub struct ReplayResult {
    pub frames: Vec<ReplayFrame>,
    pub warnings: Vec<String>,
    pub segments: Vec<ReplaySegment>,
    pub profile: String,
    pub timestep_s: f64,
    /// Frozen effective calibration modes and resolved event rates used by this result.
    pub options: ReplayOptions,
    pub calibration_available: CalibrationAvailability,
    pub identity: TrackIdentity,
}
fn rate(value: Option<f32>, name: &str) -> Result<f32, String> {
    value
        .filter(|v| v.is_finite() && *v >= 0.1 && *v <= 32_000.)
        .ok_or_else(|| format!("Specify a finite {name} rate between 0.1 and 32000 Hz"))
}
fn valid_quat(q: [f32; 4]) -> Option<[f64; 4]> {
    let norm = q.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt();
    if !norm.is_finite() || norm < 1e-12 {
        return None;
    }
    Some(q.map(|x| x as f64 / norm))
}
fn recovered_rate(previous: [f32; 4], current: [f32; 4], dt: f64) -> Option<[f32; 3]> {
    let a = valid_quat(previous)?;
    let b = valid_quat(current)?;
    let mut d = [
        a[0] * b[0] + a[1] * b[1] + a[2] * b[2] + a[3] * b[3],
        a[0] * b[1] - a[1] * b[0] - a[2] * b[3] + a[3] * b[2],
        a[0] * b[2] + a[1] * b[3] - a[2] * b[0] - a[3] * b[1],
        a[0] * b[3] - a[1] * b[2] + a[2] * b[1] - a[3] * b[0],
    ];
    if d[0] < 0. {
        d = d.map(|v| -v);
    }
    let norm = (d[1] * d[1] + d[2] * d[2] + d[3] * d[3]).sqrt();
    if norm < 1e-12 {
        return Some([0.; 3]);
    }
    let scale = 2. * norm.atan2(d[0]) / norm / dt * 180. / std::f64::consts::PI;
    Some([
        (d[1] * scale) as f32,
        (d[2] * scale) as f32,
        (d[3] * scale) as f32,
    ])
}
fn nonzero(v: [f32; 3]) -> bool {
    v != [0.; 3] && v.iter().all(|x| x.is_finite())
}
fn bounded_vector(v: [f32; 3], limit: f32) -> bool {
    v.iter().all(|x| x.is_finite() && x.abs() <= limit)
}

/// Runs entirely off the UI thread. All mutable filter state is worker-owned;
/// callers publish the returned arrays behind Arc only after successful completion.
pub fn process(
    recording: Arc<Recording>,
    track_index: usize,
    mut options: ReplayOptions,
    cancel: &AtomicBool,
    progress: &AtomicUsize,
) -> Result<ReplayResult, String> {
    progress.store(0, Ordering::Relaxed);
    let track = recording
        .tracks
        .get(track_index)
        .ok_or("Recording track no longer exists")?;
    if track.samples.is_empty() {
        return Err("Selected track contains no observations".into());
    }
    options.params.validate()?;
    let raw_hz = rate(
        options.raw_rate_hz.or(track.metadata.raw_tx_hz),
        "raw transmission",
    )?;
    let gyro_hz = rate(options.gyro_rate_hz.or(Some(raw_hz)), "reconstructed gyro")?;
    let accel_hz = rate(
        options.accel_rate_hz.or(track.metadata.accel_hz),
        "accelerometer",
    )?;
    let has_mag = options.use_magnetometer
        && track
            .samples
            .iter()
            .any(|s| s.new_mag && nonzero(s.magnetic_gauss));
    let mag_hz = if has_mag {
        rate(
            options.mag_rate_hz.or(track.metadata.mag_hz),
            "magnetometer",
        )?
    } else {
        raw_hz
    };
    if gyro_hz < raw_hz || gyro_hz / raw_hz > 256. {
        return Err(
            "Gyro reconstruction must be between raw TX rate and 256 times raw TX rate".into(),
        );
    }
    let raw_dt = 1. / raw_hz as f64;
    let mut warnings = recording.warnings.clone();
    warnings.push("Cold start: retained orientation, residual bias/covariance, runtime calibration generations and D_offset are not recorded. No exact live-device equivalence is claimed.".into());
    warnings.push("Exact scalar CMSIS-DSP algorithms/table and mixed f32/f64 kernel are used. Host precise arithmetic differs from the target's ARM compiler/FPU and CMSIS -Ofast objects; bitwise live-target equivalence is not claimed.".into());
    warnings.push("Receiver timestamps are not sensor sample times. Gyro/acc periods are fixed; mag elapsed time is inferred from raw packet positions while its filter coefficients remain fixed.".into());
    warnings.push("Default gyro period is the raw TX period, not the chip/fusion period: each recovered interval is integrated once. A higher gyro override explicitly approximates constant-rate subevents; it cannot restore actual chip history.".into());
    if track
        .metadata
        .gyro_fusion_hz
        .is_some_and(|hz| hz > raw_hz * 1.001)
        || track
            .metadata
            .gyro_chip_hz
            .is_some_and(|hz| hz > raw_hz * 1.001)
    {
        warnings.push("Undersampled gyro: individual chip events, coning, INT_merge and compensation-before-integration cannot be recovered. Replay is not equivalent to the higher-rate live filter.".into());
    }
    if track
        .samples
        .iter()
        .any(|s| matches!(s.gyro, Gyro::IntegratedQuat(_)))
    {
        warnings.push("Cumulative raw quaternions are uncorrected gyro integrals, not poses. First/segment-start gyro is absent, not compensated or fed to VQF; endpoint recovery uses the shortest arc and cannot recover rotations above pi or hidden producer loss.".into());
    }
    if options.gyro_calibration == GyroCalibration::Thermal {
        if track.calibration.thermal.legacy_curve {
            warnings.push("Legacy CSV supplies a replay thermal curve, not the device's live mode or LUT settings. Replay uses the firmware's signed-temperature MLS on complete >=4-point curves; no live enabled mode, LUT range or offset is inferred.".into());
        } else if track.calibration.thermal.flags & 1 != 0 {
            warnings.push("Thermal compensation uses signed temperature and only a complete >=4-point current curve-mode snapshot. A completed 0.5°C LUT is assumed when its configured range is known; incremental LUT/cache history and runtime D_offset are unavailable.".into());
        }
    }
    if cancel.load(Ordering::Relaxed) {
        return Err("Replay cancelled".into());
    }
    if track.samples.len() > MAX_FRAMES {
        return Err("Replay exceeds four million observations".into());
    }
    let mut samples = Vec::new();
    samples
        .try_reserve(track.samples.len())
        .map_err(|_| "Unable to allocate normalized observations")?;
    let mut normalizer = crate::capture::normalize::Normalizer::default();
    for (index, sample) in track.samples.iter().enumerate() {
        if index % 128 == 0 {
            if cancel.load(Ordering::Relaxed) {
                return Err("Replay cancelled".into());
            }
            progress.store(
                index.saturating_mul(100) / track.samples.len(),
                Ordering::Relaxed,
            );
        }
        normalizer.push(*sample, |sample| samples.push(sample));
    }
    normalizer.finish(|sample| samples.push(sample));
    let mut calibration_available = calibration_availability(&track.calibration);
    calibration_available.gyro_thermal &= samples
        .iter()
        .any(|sample| sample.temperature_c.is_some_and(f32::is_finite));
    options.calibrate_accel &= calibration_available.accel;
    options.calibrate_mag &= calibration_available.mag;
    options.gyro_calibration = match options.gyro_calibration {
        GyroCalibration::Thermal if calibration_available.gyro_thermal => GyroCalibration::Thermal,
        GyroCalibration::Thermal | GyroCalibration::Bias
            if calibration_available.gyro_bias || calibration_available.gyro_sensitivity =>
        {
            GyroCalibration::Bias
        }
        _ => GyroCalibration::None,
    };
    options.raw_rate_hz = Some(raw_hz);
    options.gyro_rate_hz = Some(gyro_hz);
    options.accel_rate_hz = Some(accel_hz);
    options.mag_rate_hz = has_mag.then_some(mag_hz);
    options.use_magnetometer = has_mag;
    let capacity = samples
        .len()
        .saturating_mul(4)
        .saturating_add(1000)
        .min(MAX_FRAMES);
    let mut frames = Vec::new();
    frames
        .try_reserve(samples.len())
        .map_err(|_| "Unable to allocate replay arrays")?;
    let mut segments = Vec::new();
    let mut filter = Filter::new(&options.params, 1. / gyro_hz, 1. / accel_hz, 1. / mag_hz)?;
    let mut calibration = (options.gyro_calibration != GyroCalibration::None)
        .then(|| Calibration::new(&track.calibration, options.gyro_calibration));
    let mut previous: Option<Sample> = None;
    let mut last_rate = [0.; 3];
    let mut temperature = None;
    let mut time = 0f64;
    let mut last_mag_time = None;
    let mut gyro_phase = 0f64;
    let mut gap_count = 0usize;
    for (index, sample) in samples.iter().enumerate() {
        if index % 128 == 0 {
            if cancel.load(Ordering::Relaxed) {
                return Err("Replay cancelled".into());
            }
            progress.store(
                100 + index.saturating_mul(900) / samples.len(),
                Ordering::Relaxed,
            );
        }
        let invalid_q = matches!(sample.gyro,Gyro::IntegratedQuat(q) if valid_quat(q).is_none());
        let span = previous.map_or(1, |p| sample.sequence.wrapping_sub(p.sequence) as usize);
        let mode_change = previous.is_some_and(|p| {
            matches!(
                (p.gyro, sample.gyro),
                (Gyro::RatesDps(_), Gyro::IntegratedQuat(_))
                    | (Gyro::IntegratedQuat(_), Gyro::RatesDps(_))
            )
        });
        let boundary = previous.is_none()
            || sample.discontinuity
            || span == 0
            || span > 501
            || mode_change
            || invalid_q;
        let span = if boundary { 1 } else { span };
        if frames.len().saturating_add(span) > capacity {
            return Err("Loss expansion exceeds bounded replay capacity (received × 4 + 1000, maximum four million)".into());
        }
        if boundary {
            if let Some(segment) = segments.last_mut() {
                let segment: &mut ReplaySegment = segment;
                segment.end = frames.len();
            }
            segments.push(ReplaySegment {
                start: frames.len(),
                end: frames.len(),
            });
            if segments.len() > 1 {
                filter.reset();
                if let Some(calibration) = calibration.as_mut() {
                    calibration.reset_cache();
                }
            }
            gyro_phase = 0.;
            last_mag_time = None;
            temperature = None;
            last_rate = [0.; 3];
        }
        let current_rate = match sample.gyro {
            Gyro::RatesDps(g) => g,
            Gyro::IntegratedQuat(q) => {
                if !boundary {
                    if let Some(Sample {
                        gyro: Gyro::IntegratedQuat(p),
                        ..
                    }) = previous
                    {
                        recovered_rate(p, q, span as f64 * raw_dt).unwrap_or([0.; 3])
                    } else {
                        [0.; 3]
                    }
                } else {
                    [0.; 3]
                }
            }
        };
        // Finite values alone are insufficient: CMSIS sin/cos converts its argument
        // to an integer table index. Reject extreme inputs before C norm/angle math.
        if !bounded_vector(current_rate, 10_000_000.)
            || !bounded_vector(sample.acceleration_g, 10_000.)
            || !bounded_vector(sample.magnetic_gauss, 10_000.)
        {
            return Err(format!(
                "Observation {index} exceeds the supported sensor numerical range"
            ));
        }
        let gyro_known = !(boundary && matches!(sample.gyro, Gyro::IntegratedQuat(_)));
        for step in 0..span {
            if step % 32 == 0 && cancel.load(Ordering::Relaxed) {
                return Err("Replay cancelled".into());
            }
            let observed = step + 1 == span;
            if !observed {
                gap_count += 1;
            }
            if observed && let Some(t) = sample.temperature_c.filter(|t| t.is_finite()) {
                temperature = Some(t);
            }
            let gyro = if observed || matches!(sample.gyro, Gyro::IntegratedQuat(_)) {
                current_rate
            } else {
                last_rate
            };
            let (gyro, thermal_correction_rad_s) = if !gyro_known {
                ([0.; 3], None)
            } else if let Some(calibration) = calibration.as_mut() {
                calibration.gyro(gyro, temperature)
            } else {
                (gyro, None)
            };
            if !bounded_vector(gyro, 10_000_000.)
                || thermal_correction_rad_s.is_some_and(|v| v.iter().any(|x| !x.is_finite()))
            {
                return Err(format!(
                    "Calibrated gyro at observation {index} exceeds the supported numerical range"
                ));
            }
            let gyro = gyro.map(|v| v * (std::f32::consts::PI / 180.));
            if gyro_known {
                gyro_phase += gyro_hz as f64 / raw_hz as f64;
                let gyro_steps = gyro_phase.floor() as usize;
                gyro_phase -= gyro_steps as f64;
                for _ in 0..gyro_steps {
                    filter.gyro(gyro);
                }
            }
            // Missing/fresh=false observations remain absent even when calibration has a nonzero bias.
            let accel_observed =
                observed && sample.acceleration_fresh && nonzero(sample.acceleration_g);
            let accel = if accel_observed {
                let a = if options.calibrate_accel {
                    affine(sample.acceleration_g, track.calibration.accel.as_ref())
                } else {
                    sample.acceleration_g
                };
                a.map(|v| v * 9.80665)
            } else {
                [0.; 3]
            };
            if !bounded_vector(accel, 100_000.) {
                return Err(format!(
                    "Calibrated acceleration at observation {index} exceeds the supported numerical range"
                ));
            }
            if nonzero(accel) {
                filter.accel(accel);
            }
            let mag_observed =
                observed && has_mag && sample.new_mag && nonzero(sample.magnetic_gauss);
            let mag = if mag_observed {
                if options.calibrate_mag {
                    affine(sample.magnetic_gauss, track.calibration.mag.as_ref())
                } else {
                    sample.magnetic_gauss
                }
            } else {
                [0.; 3]
            };
            if !bounded_vector(mag, 10_000.) {
                return Err(format!(
                    "Calibrated magnetic field at observation {index} exceeds the supported numerical range"
                ));
            }
            if nonzero(mag) {
                let elapsed = last_mag_time.map_or(1. / mag_hz, |last: f64| (time - last) as f32);
                filter.mag(mag, elapsed);
                last_mag_time = Some(time);
            }
            let s = filter.state();
            if s.iter().any(|v| !v.is_finite()) {
                return Err(format!(
                    "VQF became nonfinite at observation {index}; check rates, parameters and calibration"
                ));
            }
            frames.push(ReplayFrame {
                time_s: time,
                segment: segments.len() - 1,
                observed,
                gyro_known,
                accel_observed,
                mag_observed,
                heading9_valid: last_mag_time.is_some(),
                thermal_correction_rad_s,
                gyro_rad_s: gyro,
                accel_m_s2: accel,
                magnetic_gauss: mag,
                temperature_c: temperature,
                q6: [s[0], s[1], s[2], s[3]],
                q9: [s[4], s[5], s[6], s[7]],
                bias_rad_s: [s[8], s[9], s[10]],
                delta_rad: s[11],
                rest: s[12] != 0.,
                mag_disturbed: s[13] != 0.,
                mag_norm: s[14],
                mag_ref_norm: s[15],
                mag_dip_rad: s[16],
                mag_ref_dip_rad: s[17],
                #[cfg(test)]
                bias_covariance: [s[18], s[19], s[20]],
                k_mag_init: s[21],
            });
            time += raw_dt;
        }
        last_rate = current_rate;
        previous = if invalid_q { None } else { Some(*sample) };
    }
    if let Some(segment) = segments.last_mut() {
        segment.end = frames.len();
    }
    if gap_count > 0 {
        warnings.push(format!("{gap_count} missing packet positions: shortest-arc endpoint interpolation (quaternion) or previous-rate hold (legacy); no synthetic accel/mag."));
    }
    if segments.len() > 1 {
        warnings.push("Unknown-duration boundaries reset the filter. The plotted clock concatenates segments; do not compare heading/drift across them.".into());
    }
    if cancel.load(Ordering::Relaxed) {
        return Err("Replay cancelled".into());
    }
    progress.store(1000, Ordering::Relaxed);
    Ok(ReplayResult {
        frames,
        warnings,
        segments,
        profile: if options.params == ReplayParams::default() {
            PROFILE.into()
        } else {
            "Tracker 8aa61a6 / vqf-c 3c1a6ae · custom VQF parameters at recorded event rate · cold start · scalar CMSIS mixed precision".into()
        },
        timestep_s: raw_dt,
        options,
        calibration_available,
        identity: track.identity.clone(),
    })
}
