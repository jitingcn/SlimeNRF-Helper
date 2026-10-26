//! Raw sensor recordings preserve observations, not a device orientation estimate.
pub mod format;
pub mod normalize;
pub mod protocol;

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TrackIdentity {
    pub receiver: String,
    pub slot: u8,
    pub rf_address: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Recording {
    pub tracks: Vec<Track>,
    pub provenance: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Track {
    pub identity: TrackIdentity,
    pub metadata: SensorMetadata,
    pub calibration: CalibrationSnapshot,
    pub samples: Vec<Sample>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum Gyro {
    RatesDps([f32; 3]),
    IntegratedQuat([f32; 4]),
}

impl Default for Gyro {
    fn default() -> Self {
        Self::RatesDps([0.0; 3])
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Sample {
    pub sequence: u16,
    pub gyro: Gyro,
    pub acceleration_g: [f32; 3],
    pub magnetic_gauss: [f32; 3],
    pub new_mag: bool,
    pub acceleration_fresh: bool,
    pub temperature_c: Option<f32>,
    pub discontinuity: bool,
    pub receiver_ticks: Option<u32>,
    pub rssi: Option<i8>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SensorMetadata {
    pub gyro_range_dps: Option<f32>,
    pub accel_range_g: Option<f32>,
    pub raw_tx_hz: Option<f32>,
    pub gyro_chip_hz: Option<f32>,
    pub gyro_fusion_hz: Option<f32>,
    pub accel_hz: Option<f32>,
    pub mag_hz: Option<f32>,
    pub imu_id: Option<u8>,
    pub mag_id: Option<u8>,
    #[serde(default)]
    pub extra: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AffineCalibration {
    pub bias: [f32; 3],
    pub matrix: [[f32; 3]; 3],
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ThermalPoint {
    pub temperature_c: f32,
    pub bias_dps: [f32; 3],
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ThermalCalibration {
    pub flags: u8,
    pub mode: Option<u8>,
    pub declared_count: u16,
    pub min_c: Option<f32>,
    pub max_c: Option<f32>,
    pub chunks: BTreeMap<u8, Vec<ThermalPoint>>,
    pub legacy_offset_dps: Option<[f32; 3]>,
    pub legacy_provenance: Option<String>,
    /// A complete curve explicitly supplied by legacy CSV analysis metadata, not
    /// evidence of a device's live thermal flags/mode or LUT configuration.
    #[serde(default)]
    pub legacy_curve: bool,
}

impl ThermalCalibration {
    pub fn complete(&self) -> bool {
        let count = usize::from(self.declared_count);
        if count == 0 {
            // A current header proves an empty snapshot. No header means unknown;
            // disabled-TCAL firmware may additionally send its empty index-0 marker.
            return self.mode.is_some()
                && self
                    .chunks
                    .iter()
                    .all(|(index, points)| *index == 0 && points.is_empty());
        }
        self.chunks.len() == count.div_ceil(2)
            && (0..count.div_ceil(2)).all(|i| {
                self.chunks
                    .get(&(i as u8))
                    .is_some_and(|points| points.len() == (count - i * 2).min(2))
            })
    }

    /// Only complete indexed snapshots may be used for calibration.
    pub fn points(&self) -> Vec<ThermalPoint> {
        if self.complete() {
            self.chunks.values().flatten().copied().collect()
        } else {
            Vec::new()
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CalibrationSnapshot {
    pub accel: Option<AffineCalibration>,
    pub mag: Option<AffineCalibration>,
    pub gyro_bias_dps: Option<[f32; 3]>,
    pub gyro_sensitivity: Option<[f32; 3]>,
    pub thermal: ThermalCalibration,
}
