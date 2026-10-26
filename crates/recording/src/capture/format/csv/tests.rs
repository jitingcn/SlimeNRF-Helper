use super::{load_csv, write_csv};
use crate::capture::format::test_support::{CSV, META, cancel, fixture, identity};
use crate::capture::format::{MAX_SAMPLES, export_csv_zip, load};
use crate::capture::{Gyro, Recording, Sample, Track};
use std::io::{BufRead, Cursor, Read};
use std::sync::atomic::AtomicBool;
#[test]
fn synthetic_reference_shape_missing_temperature_and_held_acceleration() {
    let track = fixture();
    assert_eq!(track.samples.len(), 4);
    assert_eq!(track.metadata.raw_tx_hz, Some(204.8));
    assert_eq!(track.metadata.accel_hz, Some(102.4));
    assert_eq!(track.metadata.mag_hz, None);
    assert!(
        track
            .samples
            .iter()
            .all(|s| s.temperature_c.is_none() && !s.new_mag)
    );
    assert!(!track.samples[0].acceleration_fresh);
    assert!(track.samples[1].acceleration_fresh);
    assert!(!track.samples[2].acceleration_fresh);
    assert_eq!(
        track.samples[1].acceleration_g,
        track.samples[2].acceleration_g
    );
}
#[test]
fn bounded_rows_nonfinite_inputs_and_cancellation() {
    let csv = "seq,gx,gy,gz,ax,ay,az,mx,my,mz\n0,NaN,2,3,4,5,6,7,8,9\n";
    assert_eq!(
        load_csv(
            Cursor::new(csv),
            "",
            "nonfinite",
            &cancel(),
            &mut MAX_SAMPLES.clone()
        )
        .unwrap_err(),
        "Non-finite numeric input"
    );
    let mut remaining = 1;
    assert_eq!(
        load_csv(Cursor::new(CSV), META, "limited", &cancel(), &mut remaining).unwrap_err(),
        "Recording exceeds 5 million observations"
    );
    let csv = format!(
        "seq,gx,gy,gz,ax,ay,az,mx,my,mz\n0,\"{}",
        "x".repeat(20 * 1024)
    );
    assert_eq!(
        load_csv(
            Cursor::new(csv),
            "",
            "oversized",
            &cancel(),
            &mut MAX_SAMPLES.clone()
        )
        .unwrap_err(),
        "CSV record exceeds 16 KiB"
    );
    assert_eq!(
        load_csv(
            Cursor::new(CSV),
            META,
            "cancel",
            &AtomicBool::new(true),
            &mut MAX_SAMPLES.clone()
        )
        .unwrap_err(),
        "Cancelled"
    );
}
#[test]
fn missing_temperature_exports_python_float_compatible_nan_not_zero() {
    let track = fixture();
    let mut bytes = Vec::new();
    write_csv(&mut bytes, &track, &cancel()).unwrap();
    let text = String::from_utf8(bytes).unwrap();
    assert!(
        text.lines()
            .skip(1)
            .all(|row| row.split(',').nth(10) == Some("NaN"))
    );
    let mut remaining = MAX_SAMPLES;
    let restored = load_csv(
        Cursor::new(text),
        META,
        "missing-temperature",
        &cancel(),
        &mut remaining,
    )
    .unwrap();
    assert!(
        restored
            .samples
            .iter()
            .all(|sample| sample.temperature_c.is_none())
    );
}

const RAW_HEADER: &str = "seq,gx,gy,gz,ax,ay,az,mx,my,mz,note\n";

struct Chunked<'a> {
    bytes: &'a [u8],
    chunk: usize,
}

impl Read for Chunked<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let count = buffer.len().min(self.bytes.len()).min(self.chunk);
        buffer[..count].copy_from_slice(&self.bytes[..count]);
        self.consume(count);
        Ok(count)
    }
}

impl BufRead for Chunked<'_> {
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        Ok(&self.bytes[..self.bytes.len().min(self.chunk)])
    }

    fn consume(&mut self, amount: usize) {
        self.bytes = &self.bytes[amount..];
    }
}

fn load_chunked(text: &str, chunk: usize) -> Result<Track, String> {
    let mut remaining = MAX_SAMPLES;
    load_csv(
        Chunked {
            bytes: text.as_bytes(),
            chunk,
        },
        "",
        "synthetic-csv",
        &cancel(),
        &mut remaining,
    )
}

#[test]
fn csv_default_quote_grammar_survives_every_read_split() {
    let text = concat!(
        "seq,gx,gy,gz,ax,ay,az,mx,my,mz,note\n",
        "0,1,2,3,4,5,6,7,8,9,a\"\r\n",
        "1,\" \r\n10\r\n \",11,12,13,14,15,16,17,18,\"first \"\"quoted\"\"\r\nline\"\r",
        "2,19,20,21,22,23,24,25,26,27,\"closed\"tail\"\n",
        "3,28,29,30,31,32,33,34,35,36, \"not an opening quote\n",
        "4,\"37\",38,39,40,41,42,43,44,45,\"\"\r\n",
    );
    for chunk in 1..=text.len() {
        let track = load_chunked(text, chunk).unwrap();
        assert_eq!(
            track
                .samples
                .iter()
                .map(|sample| (sample.sequence, sample.gyro))
                .collect::<Vec<_>>(),
            vec![
                (0, Gyro::RatesDps([1., 2., 3.])),
                (1, Gyro::RatesDps([10., 11., 12.])),
                (2, Gyro::RatesDps([19., 20., 21.])),
                (3, Gyro::RatesDps([28., 29., 30.])),
                (4, Gyro::RatesDps([37., 38., 39.])),
            ],
            "read chunk {chunk}"
        );
        assert_eq!(track.samples[1].acceleration_g, [13., 14., 15.]);
        assert_eq!(track.samples[1].magnetic_gauss, [16., 17., 18.]);
    }
}

#[test]
fn unquoted_quotes_cannot_hide_oversized_multiline_records() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("oversized.csv");
    for note in ["a\"", "\"closed\"tail\"", " \"not an opening quote"] {
        let text = format!(
            "{RAW_HEADER}0,1,2,3,4,5,6,7,8,9,{note}\n1,10,11,12,13,14,15,16,17,18,\"{}\"\n",
            "x\n".repeat(9 * 1024)
        );
        for chunk in [1, 2, 3, 7, 8192] {
            assert_eq!(
                load_chunked(&text, chunk).unwrap_err(),
                "CSV record exceeds 16 KiB",
                "note {note:?}, read chunk {chunk}"
            );
        }
        std::fs::write(&path, text).unwrap();
        assert_eq!(
            load(&path, &cancel()).unwrap_err(),
            "CSV record exceeds 16 KiB"
        );
    }
}

#[test]
fn csv_raw_record_budget_counts_quotes_and_complete_terminators() {
    for ending in ["", "\n", "\r", "\r\n"] {
        let prefix = "0,1,2,3,4,5,6,7,8,9,\"first\n\"\"";
        let suffix = format!("\"{ending}");
        let padding = 16 * 1024 - prefix.len() - suffix.len();
        let exact = format!("{RAW_HEADER}{prefix}{}{suffix}", "x".repeat(padding));
        let oversized = format!("{RAW_HEADER}{prefix}{}{suffix}", "x".repeat(padding + 1));
        for chunk in [1, 2, 3, 7, 8192] {
            let track = load_chunked(&exact, chunk).unwrap();
            assert_eq!(track.samples.len(), 1);
            assert_eq!(track.samples[0].sequence, 0);
            assert_eq!(track.samples[0].gyro, Gyro::RatesDps([1., 2., 3.]));
            assert_eq!(track.samples[0].acceleration_g, [4., 5., 6.]);
            assert_eq!(track.samples[0].magnetic_gauss, [7., 8., 9.]);
            assert_eq!(
                load_chunked(&oversized, chunk).unwrap_err(),
                "CSV record exceeds 16 KiB",
                "ending {ending:?}, read chunk {chunk}"
            );
        }
    }
}

#[test]
fn cr_only_records_reset_the_raw_record_budget() {
    let mut text = RAW_HEADER.replace('\n', "\r");
    for sequence in 0..1024 {
        text.push_str(&format!("{sequence},1,2,3,4,5,6,7,8,9,note\r"));
    }
    for chunk in [1, 2, 3, 7, 8192] {
        let track = load_chunked(&text, chunk).unwrap();
        assert_eq!(track.samples.len(), 1024);
        assert_eq!(track.samples[0].sequence, 0);
        assert_eq!(track.samples[1023].sequence, 1023);
        assert_eq!(track.samples[1023].gyro, Gyro::RatesDps([1., 2., 3.]));
    }
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cr-only.csv");
    std::fs::write(&path, text).unwrap();
    let recording = load(&path, &cancel()).unwrap();
    assert_eq!(recording.tracks.len(), 1);
    assert_eq!(recording.tracks[0].samples.len(), 1024);
    assert_eq!(recording.tracks[0].samples[1023].sequence, 1023);
}

#[test]
fn csv_writer_uses_shortest_lossless_finite_observations() {
    let track = Track {
        samples: vec![Sample {
            sequence: 7,
            gyro: Gyro::RatesDps([1.2345678, 0.0000001, -0.]),
            acceleration_g: [0.5, -0.25, 1.],
            magnetic_gauss: [0., 2.5, -3.25],
            temperature_c: Some(12.345679),
            acceleration_fresh: true,
            new_mag: true,
            ..Sample::default()
        }],
        ..Track::default()
    };
    let mut bytes = Vec::new();
    write_csv(&mut bytes, &track, &cancel()).unwrap();
    assert_eq!(
        String::from_utf8(bytes).unwrap(),
        concat!(
            "seq,gx,gy,gz,ax,ay,az,mx,my,mz,temp,acc_fresh,discontinuity,new_mag,receiver_ticks,rssi\n",
            "7,1.2345678,0.0000001,-0,0.5,-0.25,1,0,2.5,-3.25,12.345679,1,0,1,,\n",
        )
    );
}

#[test]
fn csv_zip_roundtrip_preserves_finite_rate_observation_bits() {
    let bits = [
        0x00000000, 0x80000000, 0x00000001, 0x80000001, 0x007fffff, 0x807fffff, 0x00800000,
        0x80800000, 0x3dcccccd, 0xbdcccccd, 0x3f9e0652, 0xbf9e0652, 0x3f800001, 0xbf800001,
        0x7f7fffff, 0xff7fffff,
    ];
    let track = Track {
        identity: identity("synthetic-finite-rate-bits"),
        samples: bits
            .iter()
            .enumerate()
            .map(|(sequence, &bits)| {
                let value = f32::from_bits(bits);
                Sample {
                    sequence: sequence as u16,
                    gyro: Gyro::RatesDps([value; 3]),
                    acceleration_g: [value; 3],
                    magnetic_gauss: [value; 3],
                    temperature_c: Some(value),
                    acceleration_fresh: true,
                    new_mag: true,
                    ..Sample::default()
                }
            })
            .collect(),
        ..Track::default()
    };
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("finite-rate-bits.zip");
    export_csv_zip(
        &Recording {
            tracks: vec![track],
            ..Recording::default()
        },
        &path,
        &cancel(),
    )
    .unwrap();
    let restored = load(&path, &cancel()).unwrap();
    assert_eq!(restored.tracks.len(), 1);
    assert_eq!(restored.tracks[0].samples.len(), 16);
    for (sample, expected) in restored.tracks[0].samples.iter().zip(bits) {
        let Gyro::RatesDps(rates) = sample.gyro else {
            panic!("Expected rate observations");
        };
        assert_eq!(rates.map(f32::to_bits), [expected; 3]);
        assert_eq!(sample.acceleration_g.map(f32::to_bits), [expected; 3]);
        assert_eq!(sample.magnetic_gauss.map(f32::to_bits), [expected; 3]);
        assert_eq!(sample.temperature_c.map(f32::to_bits), Some(expected));
        assert!(sample.acceleration_fresh);
        assert!(sample.new_mag);
    }
}

#[test]
fn csv_zip_roundtrip_preserves_small_quaternion_bits() {
    let track = Track {
        identity: identity("synthetic-small-quaternion"),
        samples: vec![
            Sample {
                gyro: Gyro::IntegratedQuat([2e-10, 0., 0., 0.]),
                ..Sample::default()
            },
            Sample {
                sequence: 1,
                gyro: Gyro::IntegratedQuat(
                    [0x3f7fffff, 0x80000000, 0x00000001, 0xb3800000].map(f32::from_bits),
                ),
                ..Sample::default()
            },
        ],
        ..Track::default()
    };
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("small-quaternion.zip");
    export_csv_zip(
        &Recording {
            tracks: vec![track],
            ..Recording::default()
        },
        &path,
        &cancel(),
    )
    .unwrap();
    let restored = load(&path, &cancel()).unwrap();
    assert_eq!(restored.tracks.len(), 1);
    assert_eq!(restored.tracks[0].samples.len(), 2);
    for (sample, expected) in restored.tracks[0].samples.iter().zip([
        [2e-10f32.to_bits(), 0, 0, 0],
        [0x3f7fffff, 0x80000000, 0x00000001, 0xb3800000],
    ]) {
        let Gyro::IntegratedQuat(quaternion) = sample.gyro else {
            panic!("Expected quaternion observations");
        };
        assert_eq!(quaternion.map(f32::to_bits), expected);
        assert_eq!(sample.temperature_c, None);
    }
}

#[test]
fn bom_prefixed_quoted_headers_survive_every_read_split() {
    let text = concat!(
        "\u{feff}\"seq\",gx,gy,gz,ax,ay,az,mx,my,mz,note\r\n",
        "0,1,2,3,4,5,6,7,8,9,\"first \"\"quoted\"\"\r\nline\"\r\n",
    );
    for chunk in 1..=text.len() {
        let track = load_chunked(text, chunk).unwrap();
        assert_eq!(track.samples.len(), 1);
        assert_eq!(track.samples[0].sequence, 0);
        assert_eq!(track.samples[0].gyro, Gyro::RatesDps([1., 2., 3.]));
        assert_eq!(track.samples[0].acceleration_g, [4., 5., 6.]);
        assert_eq!(track.samples[0].magnetic_gauss, [7., 8., 9.]);
    }
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("bom.csv");
    std::fs::write(&path, text).unwrap();
    let recording = load(&path, &cancel()).unwrap();
    assert_eq!(recording.tracks.len(), 1);
    assert_eq!(recording.tracks[0].samples.len(), 1);
    assert_eq!(
        recording.tracks[0].samples[0].gyro,
        Gyro::RatesDps([1., 2., 3.])
    );
}

#[test]
fn bom_prefixed_quoted_headers_keep_the_raw_record_budget() {
    let text = format!(
        "\u{feff}\"seq{}\",gx,gy,gz,ax,ay,az,mx,my,mz\n0,1,2,3,4,5,6,7,8,9\n",
        "\nx".repeat(9 * 1024)
    );
    for chunk in [1, 2, 3, 4, 7, 8192] {
        assert_eq!(
            load_chunked(&text, chunk).unwrap_err(),
            "CSV record exceeds 16 KiB",
            "read chunk {chunk}"
        );
    }
}
