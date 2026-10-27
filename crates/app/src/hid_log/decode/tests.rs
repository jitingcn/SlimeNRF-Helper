use super::sensor::compact_quaternion_xyzw;
use super::{
    BatteryTemperature, Category, DecodedData, Direction, EventSource, Interface, MainSensor,
    MalformedReason, Ota, Record, RecordData, TrackerEvent, WomResult, decode,
};
use crate::hid_log::{ReportBytes, Source};
use slimenrf_recording::capture::{Gyro, protocol::ParsedReport};
use std::sync::Arc;

fn source(interface: Interface, key: &str) -> Arc<Source> {
    Arc::new(Source {
        receiver_key: key.into(),
        label: key.into(),
        serial_number: None,
        path: key.as_bytes().to_vec(),
        interface,
        vendor_id: 0x1209,
        product_id: 0x7690,
        usage_page: if interface == Interface::Raw {
            0xff00
        } else {
            1
        },
        connection_id: 1,
    })
}
fn report(interface: Interface, direction: Direction, b: &[u8]) -> Record {
    Record {
        sequence: 1,
        elapsed_us: 2,
        source: source(interface, "receiver-a"),
        data: RecordData::Report {
            direction,
            bytes: ReportBytes::new(b).unwrap(),
            transferred: Some(b.len()),
            error: None,
        },
    }
}
fn packet(b: [u8; 16]) -> [u8; 64] {
    let mut p = [0; 64];
    p[..16].copy_from_slice(&b);
    p
}
fn close(a: f32, b: f32) {
    assert!((a - b).abs() < 2e-6, "{a} != {b}");
}

#[test]
fn report_boundaries_never_shift_slots_or_treat_prefix_as_tracker() {
    for len in [0, 1, 15, 16, 31, 48, 63, 65] {
        let r = report(Interface::Main, Direction::In, &[0x01; 65][..len]);
        let rows: Vec<_> = decode(&r).collect();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].category, Category::Malformed);
        assert_eq!(rows[0].tracker_id, None);
        assert_eq!(rows[0].byte_len, len);
    }
    let mut p = [0; 65];
    p[1] = 254;
    p[2] = 23;
    p[3] = 0x11;
    p[5] = 7;
    let r = report(Interface::Main, Direction::Out, &p);
    let row = decode(&r).next().unwrap();
    assert_eq!(row.tracker_id, Some(7));
    assert_eq!(row.byte_offset, 0);
    assert_eq!(row.byte_len, 65);
    match row.data {
        DecodedData::Command(c) => {
            assert_eq!(c.sequence, 23);
            assert_eq!(c.opcode, 0x11);
        }
        _ => panic!("not a command"),
    }
    p[0] = 1;
    assert!(matches!(
        decode(&report(Interface::Main, Direction::Out, &p))
            .next()
            .unwrap()
            .data,
        DecodedData::Malformed {
            reason: MalformedReason::ReportId(1)
        }
    ));
}
#[test]
fn mixed_slots_preserve_offsets_and_ack_sequence_is_not_tracker_id() {
    let mut p = [0; 64];
    p[0] = 3;
    p[1] = 9;
    p[2] = 4;
    p[3] = 5;
    p[15] = (-61i8) as u8;
    p[16] = 251;
    p[17] = 8;
    p[18] = 17;
    p[19] = 6;
    p[32] = 255;
    p[33] = 2;
    p[34..40].copy_from_slice(&[1, 2, 3, 4, 5, 6]);
    p[48] = 0x80;
    p[49] = 14;
    let rows: Vec<_> = decode(&report(Interface::Main, Direction::In, &p)).collect();
    assert_eq!(
        rows.iter().map(|r| r.byte_offset).collect::<Vec<_>>(),
        [0, 16, 32, 48]
    );
    assert_eq!(
        rows.iter().map(|r| r.tracker_id).collect::<Vec<_>>(),
        [Some(9), None, Some(2), None]
    );
    assert_eq!(rows[3].category, Category::Unknown);
    assert!(matches!(rows[1].data,DecodedData::Ack(a) if a.sequence==8 && a.status==6));
}
#[test]
fn zero_padding_only_is_skipped_and_unknown_frames_remain_inspectable() {
    assert_eq!(
        decode(&report(Interface::Main, Direction::In, &[0; 64])).count(),
        0
    );
    let mut p = [0; 64];
    p[16] = 0xf8;
    p[32] = 0x90;
    p[33] = 7;
    p[63] = 3;
    let rows: Vec<_> = decode(&report(Interface::Main, Direction::In, &p)).collect();
    assert_eq!(rows.len(), 3);
    assert!(matches!(rows[0].data, DecodedData::Padding));
    assert_eq!(rows[1].tracker_id, None);
    // A nonzero type-zero record is device info, not anonymous padding.
    assert_eq!(rows[2].category, Category::Sensor);
}
#[test]
fn full_pose_units_signed_boundaries_and_no_rssi_alias() {
    let mut b = [0; 16];
    b[0] = 1;
    b[1] = 3;
    for (at, v) in [
        (2, i16::MIN),
        (4, 16384),
        (6, -8192),
        (8, 32767),
        (10, -128),
        (12, 256),
        (14, -512),
    ] {
        b[at..at + 2].copy_from_slice(&v.to_le_bytes());
    }
    let s = MainSensor::parse(&b).unwrap();
    assert_eq!(s.rssi_dbm, None);
    assert_eq!(
        s.quaternion_xyzw,
        Some([-1.0, 0.5, -0.25, 32767.0 / 32768.0])
    );
    assert_eq!(s.linear_acceleration_ms2, Some([-1.0, 2.0, -4.0]));
    b[0] = 4;
    let s = MainSensor::parse(&b).unwrap();
    assert_eq!(s.magnetic_gauss, Some([-0.125, 0.25, -0.5]));
    assert_eq!(s.linear_acceleration_ms2, None);
    assert_eq!(s.rssi_dbm, None);
}
#[test]
fn compact_pose_uses_inverse_exponential_map_and_quantizer_divisors() {
    // Exact quantized vector (0.5,-0.25,0.125), scalar-double reference values.
    let packed = 768 | (768 << 10) | (1152 << 21);
    let q = compact_quaternion_xyzw(packed);
    for (a, b) in q
        .into_iter()
        .zip([0.6836262, -0.3418131, 0.17090656, 0.6217782])
    {
        close(a, b);
    }
    let q = compact_quaternion_xyzw(512 | (1024 << 10) | (1024 << 21));
    assert_eq!(q, [0.0, 0.0, 0.0, 1.0]);
    let mut b = [0; 16];
    b[0] = 2;
    b[1] = 5;
    b[2] = 200;
    b[3] = 177;
    b[4] = 144;
    b[5..9].copy_from_slice(&packed.to_le_bytes());
    b[9..11].copy_from_slice(&(-384i16).to_le_bytes());
    b[15] = (-45i8) as u8;
    let s = MainSensor::parse(&b).unwrap();
    assert_eq!(s.linear_acceleration_ms2, Some([-3.0, 0.0, 0.0]));
    assert_eq!(s.rssi_dbm, Some(-45));
    assert_eq!(s.telemetry.unwrap().percent(), Some(72));
    close(s.telemetry.unwrap().voltage_v().unwrap(), 4.22);
}
#[test]
fn info_runtime_and_interaction_preserve_unconfirmed_fields() {
    let mut b = [0; 16];
    b[0] = 0;
    b[1] = 1;
    b[2] = 255;
    b[3] = 180;
    b[4] = 0;
    b[5] = 17;
    b[6] = 3;
    b[8] = 4;
    b[9] = 5;
    b[10..12].copy_from_slice(&((6u16 << 9) | (10 << 5) | 13).to_le_bytes());
    b[12..15].copy_from_slice(&[1, 2, 3]);
    let s = MainSensor::parse(&b).unwrap();
    assert_eq!(s.telemetry.unwrap().percent(), Some(100));
    assert_eq!(s.telemetry.unwrap().encoded_temperature_c(), None);
    assert_eq!(s.device_info.unwrap().firmware_version, [1, 2, 3]);
    b[0] = 5;
    b[2..10].copy_from_slice(&0x0102030405060708u64.to_le_bytes());
    assert_eq!(
        MainSensor::parse(&b).unwrap().remaining_runtime_us,
        Some(0x0102030405060708)
    );
    b[0] = 6;
    b[2] = 0xab;
    b[3] = 0x12;
    b[4] = 0x34;
    let s = MainSensor::parse(&b).unwrap();
    assert_eq!(s.button_raw, Some(0xab));
    assert_eq!(s.sleep_ms_wire, Some([0x12, 0x34]));
    assert_eq!(s.quaternion_xyzw, None);
    b[0] = 7;
    b[5..9].copy_from_slice(&(512u32 | (1024 << 10) | (1024 << 21)).to_le_bytes());
    let s = MainSensor::parse(&b).unwrap();
    assert_eq!(s.sleep_ms_wire, Some([0x12, 0x34]));
    assert_eq!(s.quaternion_xyzw, Some([0.0, 0.0, 0.0, 1.0]));
}
#[test]
fn lightweight_telemetry_preserves_unavailable_zero_and_fully_charged_semantics() {
    for kind in [0, 2] {
        for (battery_raw, voltage_raw, percent, voltage) in [
            (0, 155, None, None),
            (127, 155, None, None),
            (128, 0, Some(0), Some(2.45)),
            (200, 177, Some(72), Some(4.22)),
            (255, 180, Some(100), Some(4.25)),
        ] {
            let mut bytes = [0; 16];
            bytes[..5].copy_from_slice(&[kind, 3, battery_raw, voltage_raw, 144]);
            let telemetry = BatteryTemperature::parse(&bytes).unwrap();
            assert_eq!(telemetry.percent(), percent);
            assert_eq!(telemetry.charged(), battery_raw == 255);
            match (telemetry.voltage_v(), voltage) {
                (Some(actual), Some(expected)) => close(actual, expected),
                (None, None) => {}
                values => panic!("unexpected battery voltage {values:?}"),
            }
            assert_eq!(
                MainSensor::parse(&bytes).unwrap().telemetry,
                Some(telemetry)
            );
        }
    }
    for kind in [1, 3, 4, 5, 6, 7, 8, 255] {
        let mut bytes = [255; 16];
        bytes[0] = kind;
        assert_eq!(BatteryTemperature::parse(&bytes), None);
    }
}

fn event(op: u8, kind: u8, action: u8, outcome: u8, phase: u8, detail: u8) -> [u8; 16] {
    let mut b = [
        251,
        0,
        op,
        (outcome << 4) | action,
        7,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        kind,
        phase,
        detail,
    ];
    b[5..9].copy_from_slice(&0x12345678u32.to_le_bytes());
    b[9..11].copy_from_slice(&0x9abcu16.to_le_bytes());
    b[11..13].copy_from_slice(&0xdef0u16.to_le_bytes());
    b
}
#[test]
fn tracker_events_decode_identity_nibbles_calibration_and_observations() {
    let b = event(225, 0x81, 4, 2, 18, 19);
    let e = TrackerEvent::parse(&b).unwrap();
    assert_eq!(e.source, EventSource::Tracker);
    assert!(!e.snapshot);
    assert!(e.automatic_flag);
    assert_eq!(e.kind, 1);
    assert_eq!(e.boot_id, 0x12345678);
    assert_eq!(e.sequence, 0x9abc);
    assert_eq!(e.operation_id, 0xdef0);
    assert_eq!(e.action_name(), Some("END"));
    assert_eq!(e.outcome_name(), Some("FAILED"));
    assert_eq!(e.phase_name(), Some("STORAGE"));
    assert_eq!(e.detail_name(), Some("CANDIDATE_REJECTED"));
    let b = event(226, 0x20, 6, 0, 1, 0);
    let e = TrackerEvent::parse(&b).unwrap();
    assert_eq!(e.source, EventSource::Receiver);
    assert!(e.snapshot);
    assert_eq!(e.phase_name(), Some("REST"));
    assert_eq!(e.detail_name(), Some("OBSERVED"));
    let row = decode(&report(Interface::Main, Direction::In, &packet(b)))
        .next()
        .unwrap();
    assert_eq!(row.tracker_id, Some(7));
    assert_eq!(row.event_kind, Some(0x20));
    let mut b = b;
    b[1] = 9;
    let row = decode(&report(Interface::Main, Direction::In, &packet(b)))
        .next()
        .unwrap();
    assert_eq!(row.event_kind, None);
    assert_eq!(row.tracker_id, None);
    assert_eq!(row.category, Category::Command);
}
#[test]
fn event_rest_power_button_and_unknown_values_remain_exact() {
    let e = TrackerEvent::parse(&event(225, 0x21, 6, 0, 3, 2)).unwrap();
    assert_eq!(e.phase_name(), Some("UNAVAILABLE"));
    assert_eq!(e.detail_name(), Some("EQF"));
    let e = TrackerEvent::parse(&event(225, 0x30, 7, 0, 6, 2)).unwrap();
    assert_eq!(e.phase_name(), Some("WOM_CANCELLED"));
    assert_eq!(e.detail_name(), Some("WOM_FORCED"));
    let e = TrackerEvent::parse(&event(225, 0x31, 7, 0, 1, 255)).unwrap();
    assert_eq!(e.button_count(), Some((255, false)));
    assert_eq!(e.phase_name(), Some("CLICK_GROUP"));
    let e = TrackerEvent::parse(&event(226, 0xff, 15, 15, 255, 254)).unwrap();
    assert_eq!(e.kind, 127);
    assert_eq!(e.action, 15);
    assert_eq!(e.outcome, 15);
    assert_eq!(e.phase_name(), None);
    assert_eq!(e.detail, 254);
    assert_eq!(e.detail_name(), None);
    assert!(!e.snapshot);
}
#[test]
fn event_subscription_and_wom_ack_results_use_documented_endian() {
    let mut b = [0; 16];
    b[0] = 251;
    b[1] = 19;
    b[2] = 224;
    b[4..16].copy_from_slice(&[1, 1, 3, 7, 0xd0, 7, 0x10, 0x27, 0x98, 0x3a, 255, 63]);
    let row = decode(&report(Interface::Main, Direction::In, &packet(b)))
        .next()
        .unwrap();
    match row.data {
        DecodedData::Ack(a) => {
            let s = a.event_subscription.unwrap();
            assert_eq!(s.spacing_ms, 70);
            assert_eq!(s.calibration_heartbeat_ms, 2000);
            assert_eq!(s.calibration_silence_ms, 10000);
            assert_eq!(s.lease_ms, 15000);
            assert!(s.active);
            assert_eq!(s.kind_mask, 31);
        }
        _ => panic!("not ack"),
    }
    b[2] = 0x28;
    b[4..16].copy_from_slice(&[1, 5, 0xfe, 0xff, 0x34, 0x12, 0x78, 0x56, 0, 0, 0, 0]);
    let row = decode(&report(Interface::Main, Direction::In, &packet(b)))
        .next()
        .unwrap();
    assert_eq!(row.tracker_id, Some(5));
    assert!(
        matches!(row.data,DecodedData::Ack(a) if a.wom==Some(WomResult::Tracker{tracker_id:5,minutes:65534,error:0x1234,token:0x5678}))
    );
}
#[test]
fn ota_status_fragment_and_output_begin_have_distinct_layouts() {
    let b = [
        0xf4, 3, 5, 0x12, 0x34, 0x78, 0x56, 0x34, 0x12, 9, 0, 0, 0, 0, 0, 0,
    ];
    let row = decode(&report(Interface::Main, Direction::In, &packet(b)))
        .next()
        .unwrap();
    assert!(matches!(
        row.data,
        DecodedData::Ota(Ota::Status {
            target: 3,
            code: 5,
            next_sequence: 0x1234,
            bytes: 0x12345678,
            ring: 9
        })
    ));
    let mut b = [0; 16];
    b[0] = 0xf1;
    b[1] = 4;
    b[2] = 5;
    b[3..].fill(0xaa);
    let row = decode(&report(Interface::Main, Direction::In, &packet(b)))
        .next()
        .unwrap();
    assert!(
        matches!(row.data,DecodedData::Ota(Ota::InfoFragment{target:4,index:5,payload}) if payload==[0xaa;13])
    );
    let mut p = [0; 65];
    p[1] = 0xf2;
    p[2] = 0xfe;
    p[3..7].copy_from_slice(&12345u32.to_le_bytes());
    p[7..11].copy_from_slice(&0x98765432u32.to_le_bytes());
    p[11..13].copy_from_slice(&0x1234u16.to_be_bytes());
    p[13] = 1;
    p[14..19].copy_from_slice(b"board");
    p[62..64].copy_from_slice(&0x26u16.to_be_bytes());
    let row = decode(&report(Interface::Main, Direction::Out, &p))
        .next()
        .unwrap();
    assert_eq!(row.tracker_id, None);
    assert!(matches!(
        row.data,
        DecodedData::Ota(Ota::Begin {
            size: 12345,
            crc32: 0x98765432,
            packet_count: 0x1234,
            flash_base: 0x26000,
            ..
        })
    ));
}
#[test]
fn raw_interface_is_one_report_and_wxyz_is_not_fused_xyzw() {
    let mut b = [0; 64];
    b[0] = 0x13;
    b[1] = 9;
    b[2..4].copy_from_slice(&0x1234u16.to_be_bytes());
    for (at, v) in [
        (4, 0.5f32),
        (8, -0.5),
        (12, 0.5),
        (16, -0.5),
        (20, 1.25),
        (24, -2.5),
        (28, 3.75),
        (32, 0.1),
        (36, 0.2),
        (40, 0.3),
        (45, 31.5),
    ] {
        b[at..at + 4].copy_from_slice(&v.to_le_bytes());
    }
    b[44] = 1;
    b[52] = (-60i8) as u8;
    b[53..57].copy_from_slice(&0x12345678u32.to_be_bytes());
    let rows: Vec<_> = decode(&report(Interface::Raw, Direction::In, &b)).collect();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].byte_len, 64);
    assert_eq!(rows[0].tracker_id, Some(9));
    match &rows[0].data {
        DecodedData::Raw(ParsedReport::Sample { sample, .. }) => {
            assert_eq!(sample.gyro, Gyro::IntegratedQuat([0.5, -0.5, 0.5, -0.5]));
            assert_eq!(sample.acceleration_g, [1.25, -2.5, 3.75]);
            assert_eq!(sample.receiver_ticks, Some(0x12345678));
            assert_eq!(sample.sequence, 0x1234);
        }
        _ => panic!("not raw sample"),
    }
    b[4..8].copy_from_slice(&f32::NAN.to_le_bytes());
    assert_eq!(
        decode(&report(Interface::Raw, Direction::In, &b))
            .next()
            .unwrap()
            .category,
        Category::Malformed
    );
    b[0] = 0xee;
    let row = decode(&report(Interface::Raw, Direction::In, &b))
        .next()
        .unwrap();
    assert_eq!(row.category, Category::Unknown);
    assert_eq!(row.tracker_id, None);
}
#[test]
fn receiver_slot_ids_and_identity_fragments_never_leak_across_sources() {
    let mut b = [0; 16];
    b[0] = 255;
    b[1] = 2;
    b[2..8].copy_from_slice(&[1, 2, 3, 4, 5, 6]);
    let a = report(Interface::Main, Direction::In, &packet(b));
    let mut other = report(
        Interface::Main,
        Direction::In,
        &packet([3, 2, 1, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
    );
    other.source = source(Interface::Main, "receiver-b");
    assert!(matches!(
        decode(&a).next().unwrap().data,
        DecodedData::Identity {
            address_wire: [1, 2, 3, 4, 5, 6]
        }
    ));
    let row = decode(&other).next().unwrap();
    assert_eq!(row.tracker_id, Some(2));
    assert_eq!(row.category, Category::Sensor);
    assert_ne!(a.source.receiver_key, other.source.receiver_key);
}
#[test]
fn write_failure_short_write_and_lifecycle_observations_survive_decoding() {
    let mut p = [0; 65];
    p[1] = 254;
    p[2] = 1;
    p[3] = 17;
    p[5] = 2;
    let mut r = report(Interface::Main, Direction::Out, &p);
    r.data = RecordData::Report {
        direction: Direction::Out,
        bytes: ReportBytes::new(&p).unwrap(),
        transferred: None,
        error: Some("native write failed".into()),
    };
    let row = decode(&r).next().unwrap();
    let result = row.write_outcome.unwrap();
    assert_eq!(result.transferred, None);
    assert_eq!(result.error.as_deref(), Some("native write failed"));
    assert_eq!(result.attempted, 65);
    r.data = RecordData::Report {
        direction: Direction::Out,
        bytes: ReportBytes::new(&p).unwrap(),
        transferred: Some(20),
        error: None,
    };
    assert_eq!(
        decode(&r)
            .next()
            .unwrap()
            .write_outcome
            .unwrap()
            .transferred,
        Some(20)
    );
    r.data = RecordData::Open;
    assert!(matches!(decode(&r).next().unwrap().data, DecodedData::Open));
    r.data = RecordData::Close;
    assert!(matches!(
        decode(&r).next().unwrap().data,
        DecodedData::Close
    ));
    r.data = RecordData::Error {
        message: "unplugged".into(),
    };
    assert!(matches!(decode(&r).next().unwrap().data,DecodedData::Error(v) if v=="unplugged"));
}
