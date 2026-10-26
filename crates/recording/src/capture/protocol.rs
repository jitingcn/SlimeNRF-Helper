//! Raw HID is a separate 64-byte endpoint; it is not four main-HID records.
use super::{AffineCalibration, CalibrationSnapshot, Gyro, Sample, SensorMetadata, ThermalPoint};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ParsedReport {
    Sample { slot: u8, sample: Sample },
    Metadata { slot: u8, metadata: SensorMetadata },
    Calibration { slot: u8, update: CalibrationUpdate },
}

impl ParsedReport {
    pub fn slot(&self) -> u8 {
        match self {
            Self::Sample { slot, .. }
            | Self::Metadata { slot, .. }
            | Self::Calibration { slot, .. } => *slot,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum CalibrationUpdate {
    Accel(AffineCalibration),
    Mag(AffineCalibration),
    Gyro {
        bias_dps: [f32; 3],
        sensitivity: [f32; 3],
    },
    Thermal {
        flags: u8,
        declared_count: u16,
        min_c: Option<f32>,
        max_c: Option<f32>,
        mode: u8,
    },
    ThermalChunk {
        index: u8,
        declared_count: u16,
        points: Vec<ThermalPoint>,
    },
}

impl CalibrationUpdate {
    pub fn apply(&self, target: &mut CalibrationSnapshot) {
        match self {
            Self::Accel(value) => target.accel = Some(value.clone()),
            Self::Mag(value) => target.mag = Some(value.clone()),
            Self::Gyro {
                bias_dps,
                sensitivity,
            } => {
                target.gyro_bias_dps = Some(*bias_dps);
                target.gyro_sensitivity = Some(*sensitivity);
            }
            Self::Thermal {
                flags,
                declared_count,
                min_c,
                max_c,
                mode,
            } => {
                let thermal = &mut target.thermal;
                if thermal.legacy_curve || thermal.declared_count != *declared_count {
                    thermal.chunks.clear();
                }
                thermal.legacy_curve = false;
                thermal.flags = *flags;
                thermal.declared_count = *declared_count;
                thermal.min_c = *min_c;
                thermal.max_c = *max_c;
                thermal.mode = Some(*mode);
            }
            Self::ThermalChunk {
                index,
                declared_count,
                points,
            } => {
                let thermal = &mut target.thermal;
                if thermal.declared_count != *declared_count {
                    thermal.chunks.clear();
                }
                thermal.declared_count = *declared_count;
                thermal.chunks.insert(*index, points.clone());
            }
        }
    }
}

fn float(data: &[u8], at: usize) -> f32 {
    f32::from_le_bytes(data[at..at + 4].try_into().unwrap())
}
fn vector(data: &[u8], at: usize) -> [f32; 3] {
    [float(data, at), float(data, at + 4), float(data, at + 8)]
}
fn finite(value: f32) -> Option<f32> {
    value.is_finite().then_some(value)
}
fn positive(value: f32) -> Option<f32> {
    finite(value).filter(|v| *v > 0.0)
}
fn require_finite(values: &[f32]) -> Result<(), String> {
    if values.iter().all(|v| v.is_finite()) {
        Ok(())
    } else {
        Err("Non-finite sensor or calibration vector".into())
    }
}

pub fn parse_hid(data: &[u8]) -> Result<ParsedReport, String> {
    if data.len() != 64 {
        return Err(format!(
            "Raw HID report must be 64 bytes, got {}",
            data.len()
        ));
    }
    parse_payload(data, true)
}

fn parse_payload(data: &[u8], hid: bool) -> Result<ParsedReport, String> {
    let kind = *data.first().ok_or("Empty raw payload")?;
    let payload_size = match kind {
        0x10 => 48,
        0x12..=0x14 => 52,
        _ => return Err(format!("Unsupported raw report type {kind:#04x}")),
    };
    if data.len() < payload_size + 5 || (!hid && data.len() != payload_size + 5) {
        return Err("Truncated or oversized raw payload".into());
    }
    let slot = data[1];
    match kind {
        0x10 | 0x13 => {
            let quaternion = kind == 0x13;
            let acc_at = if quaternion { 20 } else { 16 };
            let mag_at = acc_at + 12;
            let flags_at = mag_at + 12;
            let gyro = if quaternion {
                let q = [
                    float(data, 4),
                    float(data, 8),
                    float(data, 12),
                    float(data, 16),
                ];
                require_finite(&q)?;
                if q.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() <= 1e-20 {
                    return Err("Zero integrated quaternion".into());
                }
                Gyro::IntegratedQuat(q)
            } else {
                let v = vector(data, 4);
                require_finite(&v)?;
                Gyro::RatesDps(v)
            };
            let acceleration_g = vector(data, acc_at);
            let magnetic_gauss = vector(data, mag_at);
            require_finite(&acceleration_g)?;
            require_finite(&magnetic_gauss)?;
            Ok(ParsedReport::Sample {
                slot,
                sample: Sample {
                    sequence: u16::from_be_bytes([data[2], data[3]]),
                    gyro,
                    acceleration_g,
                    magnetic_gauss,
                    acceleration_fresh: acceleration_g != [0.0; 3],
                    new_mag: data[flags_at] & 1 != 0,
                    temperature_c: finite(float(data, flags_at + 1)),
                    discontinuity: false,
                    rssi: Some(data[payload_size] as i8),
                    receiver_ticks: Some(u32::from_be_bytes(
                        data[payload_size + 1..payload_size + 5].try_into().unwrap(),
                    )),
                },
            })
        }
        0x12 => Ok(ParsedReport::Metadata {
            slot,
            metadata: SensorMetadata {
                gyro_range_dps: positive(float(data, 2)),
                accel_range_g: positive(float(data, 6)),
                raw_tx_hz: positive(float(data, 10)),
                accel_hz: positive(float(data, 14)),
                mag_hz: positive(float(data, 18)),
                imu_id: Some(data[22]),
                mag_id: Some(data[23]),
                gyro_chip_hz: positive(float(data, 24)),
                gyro_fusion_hz: positive(float(data, 28)),
                extra: Default::default(),
            },
        }),
        0x14 => {
            let update = match data[2] {
                1 | 2 => {
                    let bias = vector(data, 3);
                    let matrix = [vector(data, 15), vector(data, 27), vector(data, 39)];
                    require_finite(&bias)?;
                    for row in &matrix {
                        require_finite(row)?;
                    }
                    let value = AffineCalibration { bias, matrix };
                    if data[2] == 1 {
                        CalibrationUpdate::Accel(value)
                    } else {
                        CalibrationUpdate::Mag(value)
                    }
                }
                3 => {
                    let bias_dps = vector(data, 3);
                    let sensitivity = vector(data, 15);
                    require_finite(&bias_dps)?;
                    require_finite(&sensitivity)?;
                    CalibrationUpdate::Gyro {
                        bias_dps,
                        sensitivity,
                    }
                }
                4 => {
                    let declared_count = u16::from_le_bytes([data[4], data[5]]);
                    if declared_count > 512 || data[14] > 2 {
                        return Err("Invalid current thermal calibration header".into());
                    }
                    CalibrationUpdate::Thermal {
                        flags: data[3],
                        declared_count,
                        min_c: finite(float(data, 6)),
                        max_c: finite(float(data, 10)),
                        mode: data[14],
                    }
                }
                5 => {
                    let declared_count = u16::from_le_bytes([data[4], data[5]]);
                    let count = usize::from(data[6]);
                    let index = data[3];
                    let empty_table = declared_count == 0 && index == 0 && count == 0;
                    if !empty_table
                        && (declared_count > 512
                            || count == 0
                            || count > 2
                            || usize::from(index) * 2 + count > usize::from(declared_count))
                    {
                        return Err("Invalid thermal calibration chunk bounds".into());
                    }
                    let mut points = Vec::with_capacity(count);
                    for i in 0..count {
                        let at = 7 + i * 16;
                        let temperature_c = float(data, at);
                        let bias_dps = vector(data, at + 4);
                        require_finite(&[temperature_c])?;
                        require_finite(&bias_dps)?;
                        points.push(ThermalPoint {
                            temperature_c,
                            bias_dps,
                        });
                    }
                    CalibrationUpdate::ThermalChunk {
                        index,
                        declared_count,
                        points,
                    }
                }
                value => return Err(format!("Unsupported calibration subtype {value}")),
            };
            Ok(ParsedReport::Calibration { slot, update })
        }
        _ => unreachable!(),
    }
}

pub fn crc8(data: &[u8]) -> u8 {
    let mut crc = 0x07u8;
    for byte in data {
        crc ^= byte;
        for _ in 0..8 {
            crc = if crc & 0x80 != 0 {
                (crc << 1) ^ 0x07
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// Incremental AA55/length/payload/CRC decoder. Memory is bounded to one frame.
#[derive(Default)]
pub struct CdcParser {
    buffer: Vec<u8>,
}
impl CdcParser {
    pub fn feed(&mut self, bytes: &[u8], mut emit: impl FnMut(Result<ParsedReport, String>)) {
        for byte in bytes {
            self.buffer.push(*byte);
            loop {
                if self.buffer.first().is_some_and(|v| *v != 0xaa) {
                    self.buffer.remove(0);
                    continue;
                }
                if self.buffer.len() < 2 {
                    break;
                }
                if self.buffer[1] != 0x55 {
                    self.buffer.remove(0);
                    continue;
                }
                if self.buffer.len() < 3 {
                    break;
                }
                let size = usize::from(self.buffer[2]);
                if size != 53 && size != 57 {
                    emit(Err("Invalid CDC frame length".into()));
                    self.buffer.remove(0);
                    continue;
                }
                let total = size + 4;
                if self.buffer.len() < total {
                    break;
                }
                if crc8(&self.buffer[2..total - 1]) != self.buffer[total - 1] {
                    emit(Err("CDC CRC mismatch".into()));
                    self.buffer.remove(0);
                    continue;
                }
                emit(parse_payload(&self.buffer[3..total - 1], false));
                self.buffer.drain(..total);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CalibrationUpdate, CdcParser, ParsedReport, crc8, parse_hid};
    use crate::capture::CalibrationSnapshot;
    fn packet(kind: u8) -> [u8; 64] {
        let mut p = [0; 64];
        p[0] = kind;
        p[1] = 3;
        p
    }
    #[test]
    fn quaternion_temperature_missing_and_freshness() {
        let mut p = packet(0x13);
        p[4..8].copy_from_slice(&1f32.to_le_bytes());
        p[45..49].copy_from_slice(&f32::NAN.to_le_bytes());
        let ParsedReport::Sample { sample, .. } = parse_hid(&p).unwrap() else {
            panic!()
        };
        assert_eq!(sample.temperature_c, None);
        assert!(!sample.acceleration_fresh);
        assert!(!sample.new_mag);
        p[45..49].copy_from_slice(&0f32.to_le_bytes());
        let ParsedReport::Sample { sample, .. } = parse_hid(&p).unwrap() else {
            panic!()
        };
        assert_eq!(sample.temperature_c, Some(0.));
        assert!(parse_hid(&p[..63]).is_err());
    }
    #[test]
    fn current_thermal_mode_is_not_legacy_offset() {
        let mut p = packet(0x14);
        p[2] = 4;
        p[3] = 7;
        p[4] = 4;
        p[14] = 2;
        p[6..10].copy_from_slice(&(-15f32).to_le_bytes());
        let ParsedReport::Calibration { update, .. } = parse_hid(&p).unwrap() else {
            panic!()
        };
        let mut target = CalibrationSnapshot::default();
        target.thermal.legacy_curve = true;
        target.thermal.declared_count = 4;
        target.thermal.chunks.insert(
            0,
            vec![super::ThermalPoint {
                temperature_c: 10.,
                bias_dps: [1.; 3],
            }],
        );
        update.apply(&mut target);
        assert_eq!(target.thermal.mode, Some(2));
        assert_eq!(target.thermal.min_c, Some(-15.));
        assert_eq!(target.thermal.legacy_offset_dps, None);
        assert!(!target.thermal.legacy_curve);
        assert!(target.thermal.chunks.is_empty());
        assert!(matches!(
            update,
            CalibrationUpdate::Thermal { flags: 7, .. }
        ));
    }
    #[test]
    fn cdc_fragmentation_crc_and_resynchronization() {
        let mut p = packet(0x13);
        p[4..8].copy_from_slice(&1f32.to_le_bytes());
        let mut frame = vec![0xaa, 0x55, 57];
        frame.extend_from_slice(&p[..57]);
        frame.push(crc8(&frame[2..]));
        let mut bad = frame.clone();
        bad[10] ^= 1;
        let mut decoder = CdcParser::default();
        let mut results = Vec::new();
        decoder.feed(&bad, |r| results.push(r));
        for byte in frame {
            decoder.feed(&[byte], |r| results.push(r));
        }
        assert!(results.iter().any(Result::is_err));
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    }

    #[test]
    fn actual_raw_and_metadata_offsets_are_distinct() {
        let mut p = packet(0x10);
        p[2..4].copy_from_slice(&65535u16.to_be_bytes());
        for (at, value) in [(4, 12.5f32), (16, 1.), (28, 0.3), (41, -7.)] {
            p[at..at + 4].copy_from_slice(&value.to_le_bytes());
        }
        p[40] = 1;
        p[48] = (-69i8) as u8;
        p[49..53].copy_from_slice(&0xfffffff0u32.to_be_bytes());
        let ParsedReport::Sample { sample, slot } = parse_hid(&p).unwrap() else {
            panic!()
        };
        assert_eq!(slot, 3);
        assert_eq!(sample.sequence, 65535);
        assert_eq!(sample.gyro, crate::capture::Gyro::RatesDps([12.5, 0., 0.]));
        assert_eq!(sample.temperature_c, Some(-7.));
        assert!(sample.new_mag && sample.acceleration_fresh);
        assert_eq!(sample.rssi, Some(-69));
        assert_eq!(sample.receiver_ticks, Some(0xfffffff0));
        let mut p = packet(0x12);
        for (at, value) in [
            (2, 2000f32),
            (6, 16.),
            (10, 100.),
            (14, 50.),
            (18, 25.),
            (24, 800.),
            (28, 400.),
        ] {
            p[at..at + 4].copy_from_slice(&value.to_le_bytes());
        }
        p[22] = 9;
        p[23] = 2;
        let ParsedReport::Metadata { metadata, .. } = parse_hid(&p).unwrap() else {
            panic!()
        };
        assert_eq!(metadata.raw_tx_hz, Some(100.));
        assert_eq!(metadata.gyro_chip_hz, Some(800.));
        assert_eq!(metadata.gyro_fusion_hz, Some(400.));
        assert_eq!(metadata.imu_id, Some(9));
        assert_eq!(metadata.mag_id, Some(2));
    }

    #[test]
    fn affine_rows_and_short_thermal_chunks_preserve_wire_evidence() {
        let mut p = packet(0x14);
        p[2] = 1;
        for i in 0..12 {
            p[3 + i * 4..7 + i * 4].copy_from_slice(&(i as f32).to_le_bytes());
        }
        let ParsedReport::Calibration { update, .. } = parse_hid(&p).unwrap() else {
            panic!()
        };
        let mut calibration = CalibrationSnapshot::default();
        update.apply(&mut calibration);
        let affine = calibration.accel.unwrap();
        assert_eq!(affine.bias, [0., 1., 2.]);
        assert_eq!(affine.matrix, [[3., 4., 5.], [6., 7., 8.], [9., 10., 11.]]);
        let mut p = packet(0x14);
        p[2] = 5;
        p[3] = 0;
        p[4] = 4;
        p[6] = 1;
        p[7..11].copy_from_slice(&(-20f32).to_le_bytes());
        let ParsedReport::Calibration { update, .. } = parse_hid(&p).unwrap() else {
            panic!()
        };
        let mut calibration = CalibrationSnapshot::default();
        update.apply(&mut calibration);
        assert_eq!(calibration.thermal.chunks[&0][0].temperature_c, -20.);
        assert!(!calibration.thermal.complete());
        p[6] = 3;
        assert!(parse_hid(&p).is_err());
    }

    #[test]
    fn nonfinite_measurements_are_rejected_not_turned_into_observations() {
        let mut p = packet(0x13);
        p[4..8].copy_from_slice(&1f32.to_le_bytes());
        p[20..24].copy_from_slice(&f32::INFINITY.to_le_bytes());
        assert!(parse_hid(&p).is_err());
        p[20..24].fill(0);
        p[4..8].fill(0);
        assert!(parse_hid(&p).is_err());
    }

    #[test]
    fn current_empty_thermal_header_and_disabled_marker_are_complete() {
        let mut calibration = CalibrationSnapshot::default();
        assert!(!calibration.thermal.complete());
        let mut header = packet(0x14);
        header[2] = 4;
        let ParsedReport::Calibration { update, .. } = parse_hid(&header).unwrap() else {
            panic!()
        };
        update.apply(&mut calibration);
        assert!(calibration.thermal.complete());
        assert!(calibration.thermal.points().is_empty());
        let mut marker = packet(0x14);
        marker[2] = 5;
        let ParsedReport::Calibration { update, .. } = parse_hid(&marker).unwrap() else {
            panic!()
        };
        update.apply(&mut calibration);
        assert!(calibration.thermal.complete());
        assert!(calibration.thermal.chunks[&0].is_empty());
        marker[3] = 1;
        assert!(parse_hid(&marker).is_err());
        marker[3] = 0;
        marker[4] = 2;
        assert!(parse_hid(&marker).is_err());
    }
}
