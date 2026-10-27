use super::control::{opcode_name, status_name};
use super::{DecodedData, DecodedRecord, EventSource, MalformedReason, Ota, WomResult};
use crate::i18n::tr;
use slimenrf_recording::capture::{
    Gyro,
    protocol::{CalibrationUpdate, ParsedReport},
};
use std::fmt::{Display, Write};

pub fn event_kind_label(kind: u8) -> String {
    let key = match kind {
        1 => "hid.event_kind.imu_zero_rate",
        2 => "hid.event_kind.accelerometer_poses",
        3 => "hid.event_kind.manual_magnetometer",
        4 => "hid.event_kind.online_magnetometer",
        5 => "hid.event_kind.gyro_sensitivity",
        6 => "hid.event_kind.boot_thermal",
        7 => "hid.event_kind.runtime_thermal",
        0x20 => "hid.event_kind.tracker_rest",
        0x21 => "hid.event_kind.fusion_rest",
        0x30 => "hid.event_kind.power",
        0x31 => "hid.event_kind.button",
        _ => "hid.category.unknown",
    };
    format!("{} (0x{kind:02X})", tr(key))
}
fn named(name: Option<&str>, value: u8) -> String {
    match name {
        Some(name) => format!("{name} ({value})"),
        None => format!("{} ({value})", tr("hid.category.unknown")),
    }
}
fn op(value: u8) -> String {
    match opcode_name(value) {
        Some(name) => format!("{name} (0x{value:02X})"),
        None => format!("{} (0x{value:02X})", tr("hid.category.unknown")),
    }
}
fn field(rows: &mut Vec<(String, String)>, key: &str, value: impl Display) {
    rows.push((tr(key), value.to_string()));
}
fn array<const N: usize>(values: &[f32; N]) -> String {
    let mut s = String::new();
    for (i, v) in values.iter().enumerate() {
        if i > 0 {
            s.push_str(", ");
        }
        let _ = write!(s, "{v:.6}");
    }
    s
}
fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 {
            s.push(' ');
        }
        let _ = write!(s, "{b:02X}");
    }
    s
}
fn yes(value: bool) -> String {
    tr(if value {
        "common.value.yes"
    } else {
        "common.value.no"
    })
}
impl DecodedRecord {
    pub fn summary(&self) -> String {
        let body = match &self.data {
            DecodedData::Sensor(s) => format!(
                "{} {}{}",
                tr(match s.record_type {
                    0 => "hid.record.device_info",
                    1 => "hid.record.pose_acceleration",
                    2 => "hid.record.compact_pose_acceleration",
                    3 => "hid.record.device_status",
                    4 => "hid.record.pose_magnetometer",
                    5 => "hid.record.remaining_runtime",
                    6 => "hid.record.interaction",
                    7 => "hid.record.interaction_compact_pose",
                    _ => "hid.category.unknown",
                }),
                s.rssi_dbm.map(|v| format!("{v} dBm")).unwrap_or_default(),
                s.quaternion_xyzw
                    .map(|q| format!(" q=[{}]", array(&q)))
                    .unwrap_or_default()
            ),
            DecodedData::Event(e) => format!(
                "{} · {} · {} · {}{}",
                event_kind_label(e.kind),
                named(e.action_name(), e.action),
                named(e.phase_name(), e.phase),
                named(e.outcome_name(), e.outcome),
                if e.snapshot {
                    format!(" · {}", tr("hid.field.receiver_snapshot"))
                } else {
                    String::new()
                }
            ),
            DecodedData::Identity { address_wire } => {
                let mut address = *address_wire;
                address.reverse();
                format!("{} {}", tr("hid.field.rf_address"), hex(&address))
            }
            DecodedData::Ack(a) => format!(
                "ACK {} · {} · {} {}",
                op(a.opcode),
                named(status_name(a.status), a.status),
                tr("hid.field.sequence"),
                a.sequence
            ),
            DecodedData::Command(c) => {
                format!(
                    "{} · {} {}",
                    op(c.opcode),
                    tr("hid.field.sequence"),
                    c.sequence
                )
            }
            DecodedData::Ota(ota) => match ota {
                Ota::Status {
                    code,
                    next_sequence,
                    bytes,
                    ..
                } => format!(
                    "OTA_STATUS · {} {} · {} {} · {bytes} B",
                    tr("hid.field.status"),
                    code,
                    tr("hid.field.next_sequence"),
                    next_sequence
                ),
                Ota::InfoFragment { index, .. } => {
                    format!("FW_INFO · {} {index}", tr("hid.field.fragment"))
                }
                _ => op(ota.opcode()),
            },
            DecodedData::Raw(p) => match p {
                ParsedReport::Sample { sample, .. } => format!(
                    "{} · {} {}",
                    tr(match sample.gyro {
                        Gyro::RatesDps(_) => "hid.record.raw_angular_rate",
                        Gyro::IntegratedQuat(_) => "hid.record.integrated_gyro_quaternion",
                    }),
                    tr("hid.field.sequence"),
                    sample.sequence
                ),
                ParsedReport::Metadata { .. } => tr("hid.record.sensor_metadata"),
                ParsedReport::Calibration { .. } => tr("hid.record.calibration"),
            },
            DecodedData::Padding => tr("hid.record.padding"),
            DecodedData::Open => tr("hid.value.connected"),
            DecodedData::Close => tr("hid.value.disconnected"),
            DecodedData::Error(error) => error.clone(),
            DecodedData::Unknown { record_type } => record_type
                .map(|v| format!("{} 0x{v:02X}", tr("hid.category.unknown")))
                .unwrap_or_else(|| tr("hid.category.unknown")),
            DecodedData::Malformed { .. } => tr("hid.category.malformed"),
        };
        if let Some(outcome) = &self.write_outcome {
            if outcome.error.is_some() {
                return format!("{} · {body}", tr("hid.value.write_failed"));
            }
            if outcome.transferred != Some(outcome.attempted) {
                return format!("{} · {body}", tr("hid.value.short_write"));
            }
        }
        body
    }

    pub fn details(&self) -> Vec<(String, String)> {
        let mut rows = Vec::new();
        field(&mut rows, "hid.field.category", self.category.label());
        if let Some(id) = self.tracker_id {
            field(&mut rows, "hid.field.tracker_id", id);
        }
        field(&mut rows, "hid.field.byte_offset", self.byte_offset);
        field(&mut rows, "hid.field.byte_length", self.byte_len);
        match &self.data {
            DecodedData::Sensor(s) => {
                field(&mut rows, "hid.field.record_type", s.record_type);
                if let Some(q) = s.quaternion_xyzw {
                    field(&mut rows, "hid.field.quaternion_xyzw", array(&q));
                }
                if let Some(p) = s.compact_quaternion_raw {
                    field(
                        &mut rows,
                        "hid.field.packed_quaternion",
                        format!("0x{p:08X}"),
                    );
                }
                if let Some(a) = s.linear_acceleration_ms2 {
                    field(&mut rows, "hid.field.linear_acceleration", array(&a));
                }
                if let Some(m) = s.magnetic_gauss {
                    field(&mut rows, "hid.field.magnetometer", array(&m));
                }
                if let Some(t) = s.telemetry {
                    field(
                        &mut rows,
                        "hid.field.battery_bytes",
                        hex(&[t.battery_raw, t.voltage_raw]),
                    );
                    field(&mut rows, "hid.field.battery_available", yes(t.available()));
                    if let Some(percent) = t.percent() {
                        field(&mut rows, "hid.field.battery_percent", percent);
                    }
                    field(&mut rows, "hid.field.charged", yes(t.charged()));
                    if let Some(v) = t.voltage_v() {
                        field(&mut rows, "hid.field.voltage", format!("{v:.2}"));
                    }
                    field(&mut rows, "hid.field.temperature_byte", t.temperature_raw);
                    if let Some(v) = t.encoded_temperature_c() {
                        field(
                            &mut rows,
                            "hid.field.encoded_temperature",
                            format!("{v:.1}"),
                        );
                        field(
                            &mut rows,
                            "hid.field.temperature_meaning",
                            tr("hid.value.magnetic_disturbance"),
                        );
                    }
                }
                if let Some(info) = s.device_info {
                    field(&mut rows, "hid.field.board_id", info.board_id);
                    field(&mut rows, "hid.field.mcu_id", info.mcu_id);
                    field(&mut rows, "hid.field.imu_id", info.imu_id);
                    field(&mut rows, "hid.field.magnetometer_id", info.mag_id);
                    let d = info.firmware_date_raw;
                    field(
                        &mut rows,
                        "hid.field.firmware_date",
                        format!(
                            "{}-{:02}-{:02} (0x{d:04X})",
                            2020 + (d >> 9),
                            (d >> 5) & 15,
                            d & 31
                        ),
                    );
                    let [a, b, c] = info.firmware_version;
                    field(
                        &mut rows,
                        "hid.field.firmware_version",
                        format!("{a}.{b}.{c}"),
                    );
                }
                if let Some([server, status]) = s.status {
                    field(&mut rows, "hid.field.server_status", server);
                    field(&mut rows, "hid.field.tracker_status", status);
                }
                if let Some(us) = s.remaining_runtime_us {
                    field(&mut rows, "hid.field.remaining_runtime", us);
                }
                if let Some(button) = s.button_raw {
                    field(&mut rows, "hid.field.button_byte", button);
                }
                if let Some(bytes) = s.sleep_ms_wire {
                    field(&mut rows, "hid.field.sleep_bytes", hex(&bytes));
                    field(
                        &mut rows,
                        "hid.field.byte_order",
                        tr("hid.value.unconfirmed"),
                    );
                }
                if let Some(rssi) = s.rssi_dbm {
                    field(&mut rows, "hid.field.rssi", rssi);
                }
            }
            DecodedData::Event(e) => {
                field(
                    &mut rows,
                    "hid.field.event_source",
                    tr(match e.source {
                        EventSource::Tracker => "hid.value.tracker_event",
                        EventSource::Receiver => "hid.value.receiver_observation",
                    }),
                );
                field(&mut rows, "hid.field.receiver_snapshot", yes(e.snapshot));
                field(
                    &mut rows,
                    "hid.field.boot_id",
                    format!("0x{:08X}", e.boot_id),
                );
                field(&mut rows, "hid.field.sequence", e.sequence);
                field(&mut rows, "hid.field.operation_id", e.operation_id);
                field(&mut rows, "hid.field.event_kind", event_kind_label(e.kind));
                field(
                    &mut rows,
                    "hid.field.action",
                    named(e.action_name(), e.action),
                );
                field(
                    &mut rows,
                    "hid.field.outcome",
                    named(e.outcome_name(), e.outcome),
                );
                field(&mut rows, "hid.field.phase", named(e.phase_name(), e.phase));
                field(
                    &mut rows,
                    "hid.field.detail",
                    named(e.detail_name(), e.detail),
                );
                field(&mut rows, "hid.field.automatic_flag", yes(e.automatic_flag));
                if e.is_calibration() {
                    field(
                        &mut rows,
                        "hid.field.origin",
                        tr(if e.automatic_flag {
                            "hid.value.automatic"
                        } else {
                            "hid.value.user"
                        }),
                    );
                }
                if let Some((count, exact)) = e.button_count() {
                    field(&mut rows, "hid.field.click_count", count);
                    field(&mut rows, "hid.field.exact_count", yes(exact));
                }
                if e.kind == 0x21 {
                    field(
                        &mut rows,
                        "hid.field.fusion_backend",
                        named(e.detail_name(), e.detail),
                    );
                }
            }
            DecodedData::Identity { address_wire } => {
                field(&mut rows, "hid.field.rf_address_wire", hex(address_wire));
                let mut b = *address_wire;
                b.reverse();
                field(&mut rows, "hid.field.rf_address", hex(&b));
            }
            DecodedData::Command(c) => {
                field(&mut rows, "hid.field.sequence", c.sequence);
                field(&mut rows, "hid.field.opcode", op(c.opcode));
                field(&mut rows, "hid.field.flags", format!("0x{:02X}", c.flags));
                field(&mut rows, "hid.field.arguments", hex(&c.arguments));
                if c.opcode == 224 {
                    field(&mut rows, "hid.field.protocol_version", c.arguments[0]);
                    field(&mut rows, "hid.field.action", c.arguments[1]);
                    field(&mut rows, "hid.field.tracker_filter", c.arguments[2]);
                    field(
                        &mut rows,
                        "hid.field.kind_mask",
                        format!("0x{:02X}", c.arguments[3]),
                    );
                }
                if c.opcode == 0x28 {
                    field(
                        &mut rows,
                        "hid.field.wom_minutes_wire",
                        u16::from_le_bytes([c.arguments[1], c.arguments[2]]),
                    );
                }
            }
            DecodedData::Ack(a) => {
                field(&mut rows, "hid.field.sequence", a.sequence);
                field(&mut rows, "hid.field.opcode", op(a.opcode));
                field(
                    &mut rows,
                    "hid.field.status",
                    named(status_name(a.status), a.status),
                );
                field(&mut rows, "hid.field.result_bytes", hex(&a.result));
                field(
                    &mut rows,
                    "hid.field.ack_meaning",
                    tr("hid.value.ack_not_completion"),
                );
                if let Some(s) = a.event_subscription {
                    field(&mut rows, "hid.field.protocol_version", s.version);
                    field(
                        &mut rows,
                        "hid.field.capabilities",
                        format!("0x{:02X}", s.capabilities),
                    );
                    field(&mut rows, "hid.field.max_repeats", s.max_repeats);
                    field(&mut rows, "hid.field.spacing", s.spacing_ms);
                    field(
                        &mut rows,
                        "hid.field.calibration_heartbeat",
                        s.calibration_heartbeat_ms,
                    );
                    field(
                        &mut rows,
                        "hid.field.calibration_silence",
                        s.calibration_silence_ms,
                    );
                    field(&mut rows, "hid.field.lease", s.lease_ms);
                    field(&mut rows, "hid.field.tracker_filter", s.tracker_filter);
                    field(&mut rows, "hid.field.active", yes(s.active));
                    field(
                        &mut rows,
                        "hid.field.kind_mask",
                        format!("0x{:02X}", s.kind_mask),
                    );
                    field(
                        &mut rows,
                        "hid.field.flags",
                        format!("0x{:02X}", s.flags_raw),
                    );
                }
                if let Some(wom) = a.wom {
                    match wom {
                        WomResult::Tracker {
                            tracker_id,
                            minutes,
                            error,
                            token,
                        } => {
                            field(&mut rows, "hid.field.tracker_id", tracker_id);
                            field(&mut rows, "hid.field.wom_minutes_wire", minutes);
                            field(&mut rows, "hid.field.firmware_error", error);
                            field(&mut rows, "hid.field.token", token);
                        }
                        WomResult::Complete {
                            token,
                            success_mask,
                            failure_mask,
                        } => {
                            field(&mut rows, "hid.field.token", token);
                            field(
                                &mut rows,
                                "hid.field.success_mask",
                                format!("0x{success_mask:08X}"),
                            );
                            field(
                                &mut rows,
                                "hid.field.failure_mask",
                                format!("0x{failure_mask:08X}"),
                            );
                        }
                    }
                }
            }
            DecodedData::Ota(ota) => {
                field(&mut rows, "hid.field.opcode", op(ota.opcode()));
                field(&mut rows, "hid.field.target", ota.target());
                match ota {
                    Ota::InfoFragment { index, payload, .. } => {
                        field(&mut rows, "hid.field.fragment", index);
                        field(&mut rows, "hid.field.fragment_bytes", hex(payload));
                    }
                    Ota::Status {
                        code,
                        next_sequence,
                        bytes,
                        ring,
                        ..
                    } => {
                        field(&mut rows, "hid.field.status", code);
                        field(&mut rows, "hid.field.next_sequence", next_sequence);
                        field(&mut rows, "hid.field.written_bytes", bytes);
                        field(&mut rows, "hid.field.ring", ring);
                    }
                    Ota::Begin {
                        size,
                        crc32,
                        packet_count,
                        protocol,
                        board_wire,
                        flash_base,
                        ..
                    } => {
                        field(&mut rows, "hid.field.image_bytes", size);
                        field(&mut rows, "hid.field.crc32", format!("0x{crc32:08X}"));
                        field(&mut rows, "hid.field.packet_count", packet_count);
                        field(&mut rows, "hid.field.protocol_version", protocol);
                        let end = board_wire.iter().position(|b| *b == 0).unwrap_or(48);
                        field(
                            &mut rows,
                            "hid.field.board_target",
                            String::from_utf8_lossy(&board_wire[..end]),
                        );
                        field(&mut rows, "hid.field.board_bytes", hex(board_wire));
                        field(
                            &mut rows,
                            "hid.field.flash_base",
                            format!("0x{flash_base:08X}"),
                        );
                    }
                    Ota::Data { sequence, .. } => field(&mut rows, "hid.field.sequence", sequence),
                    Ota::Control { .. } => {}
                }
            }
            DecodedData::Raw(p) => raw_details(&mut rows, p),
            DecodedData::Malformed { reason } => field(
                &mut rows,
                "hid.field.reason",
                match reason {
                    MalformedReason::MainLength(n) => {
                        format!(
                            "{}: {n}; {}: 64",
                            tr("hid.field.byte_length"),
                            tr("hid.value.expected")
                        )
                    }
                    MalformedReason::OutLength(n) => {
                        format!(
                            "{}: {n}; {}: 65",
                            tr("hid.field.byte_length"),
                            tr("hid.value.expected")
                        )
                    }
                    MalformedReason::ReportId(id) => {
                        format!(
                            "{}: {id}; {}: 0",
                            tr("hid.field.report_id"),
                            tr("hid.value.expected")
                        )
                    }
                    MalformedReason::Raw(error) => error.clone(),
                    MalformedReason::ZeroQuaternion => tr("hid.value.zero_quaternion"),
                },
            ),
            DecodedData::Unknown { record_type } => {
                if let Some(t) = record_type {
                    field(&mut rows, "hid.field.record_type", format!("0x{t:02X}"));
                }
            }
            DecodedData::Error(error) => field(&mut rows, "hid.field.error", error),
            DecodedData::Padding | DecodedData::Open | DecodedData::Close => {}
        }
        if let Some(outcome) = &self.write_outcome {
            field(&mut rows, "hid.field.attempted_bytes", outcome.attempted);
            field(
                &mut rows,
                "hid.field.transferred_bytes",
                outcome
                    .transferred
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| tr("hid.value.unconfirmed")),
            );
            if let Some(error) = &outcome.error {
                field(&mut rows, "hid.field.error", error);
            }
        }
        rows
    }
}
fn raw_details(rows: &mut Vec<(String, String)>, p: &ParsedReport) {
    match p {
        ParsedReport::Sample { sample, .. } => {
            field(rows, "hid.field.sequence", sample.sequence);
            match sample.gyro {
                Gyro::RatesDps(v) => field(rows, "hid.field.angular_rate", array(&v)),
                Gyro::IntegratedQuat(q) => {
                    field(rows, "hid.field.integrated_gyro_quaternion", array(&q))
                }
            }
            field(
                rows,
                "hid.field.acceleration",
                array(&sample.acceleration_g),
            );
            field(
                rows,
                "hid.field.magnetometer",
                array(&sample.magnetic_gauss),
            );
            field(
                rows,
                "hid.field.fresh_acceleration",
                yes(sample.acceleration_fresh),
            );
            field(rows, "hid.field.new_magnetometer", yes(sample.new_mag));
            if let Some(v) = sample.temperature_c {
                field(rows, "hid.field.temperature", v);
            }
            if let Some(v) = sample.rssi {
                field(rows, "hid.field.rssi", v);
            }
            if let Some(v) = sample.receiver_ticks {
                field(rows, "hid.field.receiver_ticks", v);
            }
        }
        ParsedReport::Metadata { metadata: m, .. } => {
            for (key, value) in [
                ("hid.field.gyro_range", m.gyro_range_dps),
                ("hid.field.accel_range", m.accel_range_g),
                ("hid.field.raw_rate", m.raw_tx_hz),
                ("hid.field.accel_rate", m.accel_hz),
                ("hid.field.mag_rate", m.mag_hz),
                ("hid.field.gyro_chip_rate", m.gyro_chip_hz),
                ("hid.field.gyro_fusion_rate", m.gyro_fusion_hz),
            ] {
                if let Some(v) = value {
                    field(rows, key, v);
                }
            }
            if let Some(v) = m.imu_id {
                field(rows, "hid.field.imu_id", v);
            }
            if let Some(v) = m.mag_id {
                field(rows, "hid.field.magnetometer_id", v);
            }
        }
        ParsedReport::Calibration { update, .. } => match update {
            CalibrationUpdate::Accel(c) | CalibrationUpdate::Mag(c) => {
                field(
                    rows,
                    "hid.field.calibration_type",
                    tr(if matches!(update, CalibrationUpdate::Accel(_)) {
                        "hid.value.accelerometer"
                    } else {
                        "hid.value.magnetometer"
                    }),
                );
                field(rows, "hid.field.bias", array(&c.bias));
                for (i, v) in c.matrix.iter().enumerate() {
                    field(rows, "hid.field.matrix_row", format!("{i}: {}", array(v)));
                }
            }
            CalibrationUpdate::Gyro {
                bias_dps,
                sensitivity,
            } => {
                field(rows, "hid.field.gyro_bias", array(bias_dps));
                field(rows, "hid.field.sensitivity", array(sensitivity));
            }
            CalibrationUpdate::Thermal {
                flags,
                declared_count,
                min_c,
                max_c,
                mode,
            } => {
                field(rows, "hid.field.flags", format!("0x{flags:02X}"));
                field(rows, "hid.field.point_count", declared_count);
                field(rows, "hid.field.mode", mode);
                if let Some(v) = min_c {
                    field(rows, "hid.field.minimum_temperature", v);
                }
                if let Some(v) = max_c {
                    field(rows, "hid.field.maximum_temperature", v);
                }
            }
            CalibrationUpdate::ThermalChunk {
                index,
                declared_count,
                points,
            } => {
                field(rows, "hid.field.fragment", index);
                field(rows, "hid.field.point_count", declared_count);
                for p in points {
                    field(
                        rows,
                        "hid.field.thermal_point",
                        format!("{} °C; {} °/s", p.temperature_c, array(&p.bias_dps)),
                    );
                }
            }
        },
    }
}
