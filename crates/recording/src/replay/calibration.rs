// SPDX-License-Identifier: MIT
// MLS/LUT translation: SlimeVR Contributors (2025), see crates/recording/vendor/vqf/provenance.json.
use super::GyroCalibration;
use crate::capture::{AffineCalibration, CalibrationSnapshot, ThermalPoint};

pub(super) fn thermal_eligible(snapshot: &CalibrationSnapshot) -> bool {
    let t = &snapshot.thermal;
    let enabled = if t.legacy_curve {
        t.flags == 0 && t.mode.is_none()
    } else {
        t.flags & 1 != 0 && t.mode == Some(2)
    };
    t.complete() && t.declared_count >= 4 && enabled
}

#[derive(Clone, Copy)]
struct Fit {
    temperature: f32,
    bias: [f32; 3],
    slope: [f32; 3],
}

pub(super) struct Calibration<'a> {
    snapshot: &'a CalibrationSnapshot,
    points: Vec<ThermalPoint>,
    cache: [Option<Fit>; 5],
    lut: Vec<[f32; 3]>,
    lut_min: f32,
    curve: bool,
}
impl<'a> Calibration<'a> {
    pub fn reset_cache(&mut self) {
        self.cache.fill(None);
    }
    pub fn new(snapshot: &'a CalibrationSnapshot, mode: GyroCalibration) -> Self {
        let t = &snapshot.thermal;
        let curve = mode == GyroCalibration::Thermal && thermal_eligible(snapshot);
        let points = if curve { t.points() } else { Vec::new() };
        let mut result = Self {
            snapshot,
            points,
            cache: [None; 5],
            lut: Vec::new(),
            lut_min: 0.,
            curve,
        };
        if curve
            && !t.legacy_curve
            && let (Some(min), Some(max)) = (t.min_c, t.max_c)
            && min.is_finite()
            && max.is_finite()
            && max > min
            && max - min <= 500.
        {
            result.lut_min = min;
            let count = ((max - min) * 2.).floor() as usize + 1;
            result.lut.reserve(count);
            // Model the completed 0.5°C LUT; runtime's incremental build order is not recorded.
            for i in 0..count {
                if let Some(value) = result.mls(min + i as f32 * 0.5) {
                    result.lut.push(value);
                } else {
                    result.lut.clear();
                    break;
                }
            }
        }
        result
    }
    fn mls(&mut self, temperature: f32) -> Option<[f32; 3]> {
        if !temperature.is_finite() {
            return None;
        }
        for fit in self.cache.iter().flatten() {
            if (fit.temperature - temperature).abs() < 0.5 {
                return Some(std::array::from_fn(|i| {
                    fit.bias[i] + fit.slope[i] * (temperature - fit.temperature)
                }));
            }
        }
        // Online top-10 selection preserves the firmware's point/evaluation order.
        let mut selected = [(0usize, 0f32); 10];
        let mut count = 0;
        let mut weakest = 0;
        let mut nearest = None;
        let mut nearest_distance = f32::INFINITY;
        for (index, point) in self.points.iter().enumerate() {
            if point.temperature_c == 0.
                || !point.temperature_c.is_finite()
                || !point.bias_dps.iter().all(|v| v.is_finite())
            {
                continue;
            }
            let d = point.temperature_c - temperature;
            if d.abs() < nearest_distance {
                nearest = Some(index);
                nearest_distance = d.abs();
            }
            let weight = 1. / (1. + d * d / (2.5 * 2.5));
            if weight < 0.03 {
                continue;
            }
            if count < 10 {
                selected[count] = (index, weight);
                count += 1;
            } else if weight > selected[weakest].1 {
                selected[weakest] = (index, weight);
            } else {
                continue;
            }
            if count == 10 {
                weakest = 0;
                for j in 1..10 {
                    if selected[j].1 < selected[weakest].1 {
                        weakest = j;
                    }
                }
            }
        }
        let mut fit = Fit {
            temperature,
            bias: [0.; 3],
            slope: [0.; 3],
        };
        if count == 0 {
            fit.bias = self.points[nearest?].bias_dps;
        } else if count == 1 {
            fit.bias = self.points[selected[0].0].bias_dps;
        } else {
            let (mut sw, mut swdt, mut swdt2) = (0f64, 0f64, 0f64);
            let (mut swb, mut swdtb) = ([0f64; 3], [0f64; 3]);
            for &(index, weight) in &selected[..count] {
                let point = &self.points[index];
                let w = weight as f64;
                let dt = (point.temperature_c - temperature) as f64;
                sw += w;
                swdt += w * dt;
                swdt2 += w * dt * dt;
                for axis in 0..3 {
                    let b = point.bias_dps[axis] as f64;
                    swb[axis] += w * b;
                    swdtb[axis] += w * dt * b;
                }
            }
            let det = sw * swdt2 - swdt * swdt;
            for axis in 0..3 {
                if det.abs() < 1e-10 {
                    fit.bias[axis] = (swb[axis] / sw) as f32;
                } else {
                    fit.bias[axis] = ((swb[axis] * swdt2 - swdtb[axis] * swdt) / det) as f32;
                    fit.slope[axis] = ((sw * swdtb[axis] - swdt * swb[axis]) / det) as f32;
                }
            }
        }
        let slot = self
            .cache
            .iter()
            .position(Option::is_none)
            .unwrap_or_else(|| {
                let mut best = 0;
                for i in 1..5 {
                    if (self.cache[i].unwrap().temperature - temperature).abs()
                        > (self.cache[best].unwrap().temperature - temperature).abs()
                    {
                        best = i;
                    }
                }
                best
            });
        self.cache[slot] = Some(fit);
        Some(fit.bias)
    }
    fn thermal(&mut self, temperature: f32) -> Option<[f32; 3]> {
        if !self.curve || !temperature.is_finite() {
            return None;
        }
        if self.lut.len() >= 2 {
            let index = ((temperature - self.lut_min) * 2.).clamp(0., (self.lut.len() - 1) as f32);
            let lo = (index as usize).min(self.lut.len() - 2);
            let frac = (index - lo as f32).clamp(0., 1.);
            Some(std::array::from_fn(|i| {
                self.lut[lo][i] + frac * (self.lut[lo + 1][i] - self.lut[lo][i])
            }))
        } else {
            self.mls(temperature)
        }
    }
    pub fn gyro(
        &mut self,
        gyro: [f32; 3],
        temperature: Option<f32>,
    ) -> ([f32; 3], Option<[f32; 3]>) {
        let curve = temperature.and_then(|t| self.thermal(t));
        let mut offset = curve.unwrap_or(self.snapshot.gyro_bias_dps.unwrap_or([0.; 3]));
        if curve.is_some()
            && self.snapshot.thermal.legacy_provenance.is_some()
            && let Some(delta) = self.snapshot.thermal.legacy_offset_dps
        {
            for i in 0..3 {
                offset[i] += delta[i];
            }
        }
        let sensitivity = self.snapshot.gyro_sensitivity.unwrap_or([1.; 3]);
        let corrected = std::array::from_fn(|i| (gyro[i] - offset[i]) * sensitivity[i]);
        let thermal_correction = curve.map(|_| {
            std::array::from_fn(|i| offset[i] * sensitivity[i] * (std::f32::consts::PI / 180.))
        });
        (corrected, thermal_correction)
    }
}

pub(super) fn affine(v: [f32; 3], calibration: Option<&AffineCalibration>) -> [f32; 3] {
    let Some(c) = calibration else {
        return v;
    };
    let v = std::array::from_fn::<_, 3, _>(|i| v[i] - c.bias[i]);
    std::array::from_fn(|i| c.matrix[i][0] * v[0] + c.matrix[i][1] * v[1] + c.matrix[i][2] * v[2])
}

#[cfg(test)]
mod tests {
    use super::Calibration;
    use crate::capture::{CalibrationSnapshot, ThermalPoint};
    use crate::replay::GyroCalibration;
    #[test]
    fn signed_temperature_curve_gate_offset_then_sensitivity() {
        let mut snapshot = CalibrationSnapshot {
            gyro_bias_dps: Some([7.; 3]),
            gyro_sensitivity: Some([2.; 3]),
            ..Default::default()
        };
        snapshot.thermal.flags = 3;
        snapshot.thermal.mode = Some(2);
        snapshot.thermal.declared_count = 4;
        snapshot.thermal.chunks.insert(
            0,
            vec![
                ThermalPoint {
                    temperature_c: -20.,
                    bias_dps: [-2.; 3],
                },
                ThermalPoint {
                    temperature_c: -10.,
                    bias_dps: [-1.; 3],
                },
            ],
        );
        snapshot.thermal.chunks.insert(
            1,
            vec![
                ThermalPoint {
                    temperature_c: 10.,
                    bias_dps: [1.; 3],
                },
                ThermalPoint {
                    temperature_c: 20.,
                    bias_dps: [2.; 3],
                },
            ],
        );
        let mut c = Calibration::new(&snapshot, GyroCalibration::Thermal);
        let (value, thermal) = c.gyro([1.; 3], Some(-10.));
        assert!((thermal.unwrap()[0] - (-2f32).to_radians()).abs() < 1e-6);
        assert!((value[0] - 4.).abs() < 1e-5);
        snapshot.thermal.mode = Some(1);
        assert_eq!(
            Calibration::new(&snapshot, GyroCalibration::Thermal)
                .gyro([1.; 3], Some(-10.))
                .0,
            [-12.; 3]
        );
        snapshot.thermal.mode = Some(2);
        snapshot.thermal.min_c = Some(-20.);
        snapshot.thermal.max_c = Some(20.);
        snapshot.thermal.legacy_offset_dps = Some([0.5; 3]);
        // Current metadata must never manufacture a D_offset from the mode byte.
        assert!(
            (Calibration::new(&snapshot, GyroCalibration::Thermal)
                .gyro([1.; 3], Some(-100.))
                .0[0]
                - 6.)
                .abs()
                < 1e-5
        );
        snapshot.thermal.legacy_provenance = Some("explicit legacy offset-bearing capture".into());
        assert!(
            (Calibration::new(&snapshot, GyroCalibration::Thermal)
                .gyro([1.; 3], Some(-100.))
                .0[0]
                - 5.)
                .abs()
                < 1e-5
        );
        snapshot.thermal.chunks.remove(&1);
        assert_eq!(
            Calibration::new(&snapshot, GyroCalibration::Thermal)
                .gyro([1.; 3], Some(-10.))
                .0,
            [-12.; 3]
        );
    }

    #[test]
    fn legacy_curve_keeps_signed_firmware_math_without_assuming_a_lut() {
        let mut snapshot = CalibrationSnapshot::default();
        snapshot.thermal.legacy_curve = true;
        snapshot.thermal.declared_count = 4;
        // Even an in-memory caller supplying bounds cannot invent a legacy LUT.
        snapshot.thermal.min_c = Some(-20.);
        snapshot.thermal.max_c = Some(20.);
        for (index, temperatures) in [[-20f32, -10.], [10., 20.]].into_iter().enumerate() {
            snapshot.thermal.chunks.insert(
                index as u8,
                temperatures
                    .map(|temperature_c| ThermalPoint {
                        temperature_c,
                        bias_dps: [temperature_c * 0.1; 3],
                    })
                    .to_vec(),
            );
        }
        let mut calibration = Calibration::new(&snapshot, GyroCalibration::Thermal);
        assert!(calibration.lut.is_empty());
        let (negative, applied) = calibration.gyro([1.; 3], Some(-10.));
        assert!((negative[0] - 2.).abs() < 1e-5);
        assert!((applied.unwrap()[0] - (-1f32).to_radians()).abs() < 1e-6);
        snapshot.thermal.mode = Some(0);
        assert!(!super::thermal_eligible(&snapshot));
        snapshot.thermal.mode = None;
        snapshot.thermal.chunks.remove(&1);
        assert!(!super::thermal_eligible(&snapshot));
    }
}
