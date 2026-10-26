//! Bounded analyzer CSV observations and freshness-preserving export.
use super::metadata::{finite_number, parse_meta};
use super::{MAX_INPUT_BYTES, cancelled, err, validate_sample};
use crate::capture::{Gyro, Sample, Track};
use std::collections::BTreeMap;
use std::io::{BufRead, Cursor, Read, Write};
use std::sync::atomic::AtomicBool;
/// Bound raw records before csv's reusable record buffer can grow.
struct CsvInput<R> {
    inner: R,
    total: u64,
    row: usize,
    state: CsvState,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CsvState {
    FieldStart,
    Unquoted,
    Quoted,
    AfterQuote,
    AfterCr,
}
impl<R: Read> Read for CsvInput<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let count = self.inner.read(buffer)?;
        let first_read = self.total == 0;
        self.total += count as u64;
        if self.total > MAX_INPUT_BYTES {
            return Err(std::io::Error::other("CSV exceeds byte limit"));
        }
        // csv-core strips a complete BOM only from its first input buffer.
        let start = if first_read && buffer[..count].starts_with(b"\xef\xbb\xbf") {
            self.row += 3;
            3
        } else {
            0
        };
        for &byte in &buffer[start..count] {
            if self.state == CsvState::AfterCr && byte != b'\n' {
                self.row = 0;
                self.state = CsvState::FieldStart;
            }
            self.row += 1;
            if self.row > 16 * 1024 {
                return Err(std::io::Error::other("CSV record exceeds 16 KiB"));
            }
            self.state = match (self.state, byte) {
                (CsvState::Quoted, b'"') => CsvState::AfterQuote,
                (CsvState::Quoted, _) => CsvState::Quoted,
                (CsvState::FieldStart | CsvState::AfterQuote, b'"') => CsvState::Quoted,
                (_, b',') => CsvState::FieldStart,
                (_, b'\r') => CsvState::AfterCr,
                (_, b'\n') => {
                    self.row = 0;
                    CsvState::FieldStart
                }
                _ => CsvState::Unquoted,
            };
        }
        Ok(count)
    }
}

pub(super) fn load_csv(
    mut reader: impl BufRead,
    meta: &str,
    source: &str,
    cancel: &AtomicBool,
    remaining: &mut usize,
) -> Result<Track, String> {
    let mut track = parse_meta(meta, source)?;
    // csv-core needs the complete BOM and a following byte in its first buffer.
    let mut prefix = [0; 4];
    let mut prefix_len = 0;
    while prefix_len < prefix.len() {
        let count = reader.read(&mut prefix[prefix_len..]).map_err(err)?;
        if count == 0 {
            break;
        }
        prefix_len += count;
    }
    let input = CsvInput {
        inner: Cursor::new(prefix).take(prefix_len as u64).chain(reader),
        total: 0,
        row: 0,
        state: CsvState::FieldStart,
    };
    let mut reader = csv::ReaderBuilder::new().from_reader(input);
    let header = reader.headers().map_err(err)?.clone();
    if header.is_empty() {
        return Err("Empty CSV".into());
    }
    let mut columns = BTreeMap::new();
    for (i, value) in header.iter().enumerate() {
        let key = value.trim().trim_start_matches('\u{feff}').to_owned();
        if columns.insert(key, i).is_some() {
            return Err("Duplicate CSV column".into());
        }
    }
    let quaternion = columns.contains_key("qw");
    let required: &[&str] = if quaternion {
        &[
            "seq", "qw", "qx", "qy", "qz", "ax", "ay", "az", "mx", "my", "mz",
        ]
    } else {
        &["seq", "gx", "gy", "gz", "ax", "ay", "az", "mx", "my", "mz"]
    };
    if required.iter().any(|key| !columns.contains_key(*key)) {
        return Err("CSV lacks required gyro/quaternion or sensor columns".into());
    }
    if quaternion
        && ["gx", "gy", "gz"]
            .iter()
            .any(|key| columns.contains_key(*key))
    {
        return Err("CSV mixes raw-rate and quaternion columns".into());
    }
    if let Some(mode) = track.metadata.extra.get("data_mode")
        && ((mode == "gyr_quat" && !quaternion) || (mode == "raw" && quaternion))
    {
        return Err("CSV mode disagrees with metadata".into());
    }
    let sequence_column = columns["seq"];
    let gyro_columns = if quaternion {
        ["qw", "qx", "qy", "qz"].map(|key| columns[key])
    } else {
        ["gx", "gy", "gz", "gx"].map(|key| columns[key])
    };
    let acceleration_columns = ["ax", "ay", "az"].map(|key| columns[key]);
    let magnetic_columns = ["mx", "my", "mz"].map(|key| columns[key]);
    let [
        temperature_column,
        accel_fresh_column,
        new_mag_column,
        discontinuity_column,
        ticks_column,
        rssi_column,
    ] = [
        "temp",
        "acc_fresh",
        "new_mag",
        "discontinuity",
        "receiver_ticks",
        "rssi",
    ]
    .map(|key| columns.get(key).copied());
    let mut record = csv::StringRecord::new();
    while reader.read_record(&mut record).map_err(err)? {
        cancelled(cancel)?;
        if *remaining == 0 {
            return Err("Recording exceeds 5 million observations".into());
        }
        let optional = |index: Option<usize>| -> Option<&str> { index.and_then(|i| record.get(i)) };
        let number = |index: usize| -> Result<f32, String> { finite_number(&record[index]) };
        let flag = |index: Option<usize>, default: bool| -> Result<bool, String> {
            match optional(index) {
                None | Some("") => Ok(default),
                Some("0") => Ok(false),
                Some("1") => Ok(true),
                _ => Err("Invalid CSV freshness/discontinuity flag".into()),
            }
        };
        let acceleration_g = [
            number(acceleration_columns[0])?,
            number(acceleration_columns[1])?,
            number(acceleration_columns[2])?,
        ];
        let magnetic_gauss = [
            number(magnetic_columns[0])?,
            number(magnetic_columns[1])?,
            number(magnetic_columns[2])?,
        ];
        let temperature_c = match optional(temperature_column) {
            None | Some("") => None,
            Some(v) => {
                let v = v.parse::<f32>().map_err(err)?;
                v.is_finite().then_some(v)
            }
        };
        let sample = Sample {
            sequence: record[sequence_column].parse().map_err(err)?,
            gyro: if quaternion {
                Gyro::IntegratedQuat([
                    number(gyro_columns[0])?,
                    number(gyro_columns[1])?,
                    number(gyro_columns[2])?,
                    number(gyro_columns[3])?,
                ])
            } else {
                Gyro::RatesDps([
                    number(gyro_columns[0])?,
                    number(gyro_columns[1])?,
                    number(gyro_columns[2])?,
                ])
            },
            acceleration_g,
            magnetic_gauss,
            acceleration_fresh: flag(accel_fresh_column, acceleration_g != [0.; 3])?,
            new_mag: flag(new_mag_column, magnetic_gauss != [0.; 3])?,
            temperature_c,
            discontinuity: flag(discontinuity_column, false)?,
            receiver_ticks: optional(ticks_column)
                .filter(|v| !v.is_empty())
                .map(str::parse)
                .transpose()
                .map_err(err)?,
            rssi: optional(rssi_column)
                .filter(|v| !v.is_empty())
                .map(str::parse)
                .transpose()
                .map_err(err)?,
        };
        validate_sample(&sample)?;
        track.samples.push(sample);
        *remaining -= 1;
    }
    Ok(track)
}
pub(super) fn write_csv(
    writer: &mut impl Write,
    track: &Track,
    cancel: &AtomicBool,
) -> Result<(), String> {
    let quaternion = track
        .samples
        .first()
        .is_some_and(|s| matches!(s.gyro, Gyro::IntegratedQuat(_)));
    writer.write_all(if quaternion { b"seq,qw,qx,qy,qz,ax,ay,az,mx,my,mz,temp,acc_fresh,discontinuity,new_mag,receiver_ticks,rssi\n" } else { b"seq,gx,gy,gz,ax,ay,az,mx,my,mz,temp,acc_fresh,discontinuity,new_mag,receiver_ticks,rssi\n" }).map_err(err)?;
    for sample in &track.samples {
        cancelled(cancel)?;
        validate_sample(sample)?;
        write!(writer, "{}", sample.sequence).map_err(err)?;
        match sample.gyro {
            Gyro::IntegratedQuat(q) if quaternion => { for value in q { write!(writer, ",{value}").map_err(err)?; } }
            Gyro::RatesDps(v) if !quaternion => { for value in v { write!(writer, ",{value}").map_err(err)?; } }
            _ => return Err("CSV cannot encode mixed gyro modes in one track; preserve native .slimecap instead".into()),
        }
        // Analyzer readers without freshness extensions also see absence, never
        // a repeated held value masquerading as a newly measured observation.
        let acceleration = if sample.acceleration_fresh {
            sample.acceleration_g
        } else {
            [0.; 3]
        };
        let magnetic = if sample.new_mag {
            sample.magnetic_gauss
        } else {
            [0.; 3]
        };
        for value in acceleration.into_iter().chain(magnetic) {
            write!(writer, ",{value}").map_err(err)?;
        }
        writer.write_all(b",").map_err(err)?;
        if let Some(value) = sample.temperature_c {
            write!(writer, "{value}").map_err(err)?;
        } else {
            // Python's reference DictReader consumer calls float(temp): blank
            // fails, while NaN round-trips as missing rather than a fabricated 0°C.
            writer.write_all(b"NaN").map_err(err)?;
        }
        write!(
            writer,
            ",{},{},{},",
            u8::from(sample.acceleration_fresh),
            u8::from(sample.discontinuity),
            u8::from(sample.new_mag)
        )
        .map_err(err)?;
        if let Some(value) = sample.receiver_ticks {
            write!(writer, "{value}").map_err(err)?;
        }
        writer.write_all(b",").map_err(err)?;
        if let Some(value) = sample.rssi {
            write!(writer, "{value}").map_err(err)?;
        }
        writer.write_all(b"\n").map_err(err)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
