//! Main-interface wire layouts from receiver hid.c's explicit byte table.
//! Types 6/7 have no current encoder: retain button and sleep bytes without invented semantics.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BatteryTemperature {
    pub battery_raw: u8,
    pub voltage_raw: u8,
    pub temperature_raw: u8,
}
impl BatteryTemperature {
    pub(crate) fn parse(b: &[u8; 16]) -> Option<Self> {
        matches!(b[0], 0 | 2).then_some(Self {
            battery_raw: b[2],
            voltage_raw: b[3],
            temperature_raw: b[4],
        })
    }
    pub fn available(self) -> bool {
        self.battery_raw & 128 != 0
    }
    pub fn charged(self) -> bool {
        self.battery_raw == 255
    }
    pub fn percent(self) -> Option<u8> {
        self.available().then_some(if self.charged() {
            100
        } else {
            self.battery_raw & 127
        })
    }
    pub fn voltage_v(self) -> Option<f32> {
        self.available()
            .then_some(2.45 + f32::from(self.voltage_raw) / 100.0)
    }
    /// The firmware can negate temperature to indicate magnetic disturbance.
    pub fn encoded_temperature_c(self) -> Option<f32> {
        (self.temperature_raw != 0)
            .then_some((f32::from(self.temperature_raw) - 128.0) / 2.0 + 25.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DeviceInfo {
    pub telemetry: BatteryTemperature,
    pub board_id: u8,
    pub mcu_id: u8,
    pub imu_id: u8,
    pub mag_id: u8,
    pub firmware_date_raw: u16,
    pub firmware_version: [u8; 3],
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MainSensor {
    pub record_type: u8,
    pub quaternion_xyzw: Option<[f32; 4]>,
    pub compact_quaternion_raw: Option<u32>,
    pub linear_acceleration_ms2: Option<[f32; 3]>,
    pub magnetic_gauss: Option<[f32; 3]>,
    pub telemetry: Option<BatteryTemperature>,
    pub device_info: Option<DeviceInfo>,
    pub status: Option<[u8; 2]>,
    pub remaining_runtime_us: Option<u64>,
    pub button_raw: Option<u8>,
    /// Millisecond field, byte order unconfirmed; never interpret as LE or BE.
    pub sleep_ms_wire: Option<[u8; 2]>,
    pub rssi_dbm: Option<i8>,
}
impl MainSensor {
    pub fn parse(b: &[u8; 16]) -> Option<Self> {
        if b[0] > 7 {
            return None;
        }
        let mut out = Self {
            record_type: b[0],
            quaternion_xyzw: None,
            compact_quaternion_raw: None,
            linear_acceleration_ms2: None,
            magnetic_gauss: None,
            telemetry: None,
            device_info: None,
            status: None,
            remaining_runtime_us: None,
            button_raw: None,
            sleep_ms_wire: None,
            rssi_dbm: (!matches!(b[0], 1 | 4)).then_some(b[15] as i8),
        };
        match b[0] {
            0 => {
                let telemetry = BatteryTemperature::parse(b)?;
                out.telemetry = Some(telemetry);
                out.device_info = Some(DeviceInfo {
                    telemetry,
                    board_id: b[5],
                    mcu_id: b[6],
                    imu_id: b[8],
                    mag_id: b[9],
                    firmware_date_raw: u16::from_le_bytes([b[10], b[11]]),
                    firmware_version: [b[12], b[13], b[14]],
                });
            }
            1 | 4 => {
                out.quaternion_xyzw = Some([
                    fixed(b, 2, 32768.0),
                    fixed(b, 4, 32768.0),
                    fixed(b, 6, 32768.0),
                    fixed(b, 8, 32768.0),
                ]);
                if b[0] == 1 {
                    out.linear_acceleration_ms2 = Some(vector(b, 10, 128.0));
                } else {
                    out.magnetic_gauss = Some(vector(b, 10, 1024.0));
                }
            }
            2 | 7 => {
                let packed = u32::from_le_bytes(b[5..9].try_into().unwrap());
                out.compact_quaternion_raw = Some(packed);
                out.quaternion_xyzw = Some(compact_quaternion_xyzw(packed));
                out.linear_acceleration_ms2 = Some(vector(b, 9, 128.0));
                if b[0] == 2 {
                    out.telemetry = BatteryTemperature::parse(b);
                } else {
                    out.button_raw = Some(b[2]);
                    out.sleep_ms_wire = Some([b[3], b[4]]);
                }
            }
            3 => out.status = Some([b[2], b[3]]),
            5 => out.remaining_runtime_us = Some(u64::from_le_bytes(b[2..10].try_into().unwrap())),
            6 => {
                out.button_raw = Some(b[2]);
                out.sleep_ms_wire = Some([b[3], b[4]]);
            }
            _ => unreachable!(),
        }
        Some(out)
    }
}
fn fixed(b: &[u8; 16], at: usize, divisor: f32) -> f32 {
    f32::from(i16::from_le_bytes([b[at], b[at + 1]])) / divisor
}
fn vector(b: &[u8; 16], at: usize, divisor: f32) -> [f32; 3] {
    [
        fixed(b, at, divisor),
        fixed(b, at + 2, divisor),
        fixed(b, at + 4, divisor),
    ]
}
/// Inverse exponential-map packing (q_iem), not smallest-three quaternion packing.
/// The firmware quantizer scales by 1024/2048, not 1023/2047.
pub fn compact_quaternion_xyzw(packed: u32) -> [f32; 4] {
    let v = [
        2.0 * ((packed & 1023) as f32 / 1024.0) - 1.0,
        2.0 * (((packed >> 10) & 2047) as f32 / 2048.0) - 1.0,
        2.0 * ((packed >> 21) as f32 / 2048.0) - 1.0,
    ];
    let d = v[0] * v[0] + v[1] * v[1] + v[2] * v[2];
    let inv = 1.0 / (d + 1e-6).sqrt();
    let angle = std::f32::consts::FRAC_PI_2 * d * inv;
    let k = angle.sin() * inv;
    [k * v[0], k * v[1], k * v[2], angle.cos()]
}
